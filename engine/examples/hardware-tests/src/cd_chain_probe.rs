//! CL2: the CD read-mechanism matrix.
//!
//! Ported here from the standalone probe disc so the suite is the one
//! place hardware questions get asked. Four demo-disc burns died on the
//! chain loader's header read returning zeros, and CL1 convicted the
//! transfer itself: commands and DataReady succeed, the sector sits in
//! the controller FIFO, and the DMA moves nothing while MADR stays put.
//!
//! Each variant reads the deterministic CDTEST region a different way,
//! so one QR says which mechanism silicon actually honours:
//!
//!   SDKRD  SectorReader exactly as the SDK ships it
//!   RAWNP  raw driver, no purge, BFRD then immediate DMA
//!   RAWPU  raw driver WITH the Request=0 purge, then BFRD+DMA
//!   RAWWT  raw, no purge, wait for data-FIFO-not-empty before the kick
//!   PIONP  raw, no purge, PIO drain: no DMA at all
//!   PIOPU  raw, with purge, PIO drain
//!   CHCRQ  raw DMA with CHCR sampled at the kick and after a spin
//!   SDKR2  SectorReader again: is the drive still sane afterwards?
//!
//! Per variant: OK bits, the CHCR pair, the first words read, the
//! sector FNV against the expected value, channel-3 MADR after the
//! transfer, drive/controller status, and the reader's diag snapshot.

use psx_engine::{button, Ctx};
use psx_font::FontAtlas;
use psx_gpu as gpu;
use psx_io::cdrom;
use psx_pack::cd::{SectorReader, SECTOR_WORDS};
use psx_rt::tty;
use qrcodegen_no_heap::{QrCode, QrCodeEcc, Version};

/// First LBA of the CDTEST region on THIS disc (verified against the
/// built image by scanning for the sector-aligned "PSOXSTRM" header; a
/// drift reads BAD rather than lying, since the magic words are checked).
const CDTEST_LBA: u32 = 424;
/// Sector count the disc build bakes into the CDTEST header.
const CDTEST_SECTORS: u32 = 600;
const CD_SPINS: u32 = 0x10_0000;

const VARIANT_COUNT: usize = 8;
const FIELD_COUNT: usize = 10;

const QR_VERSION: Version = Version::new(17);
const QR_SIZE: usize = 85;
const QR_BUFFER_LEN: usize = QR_VERSION.buffer_len();
const QR_SCALE: i16 = 2;
const QR_QUIET: i16 = 4;
const BINARY_LEN: usize = 16 + VARIANT_COUNT * FIELD_COUNT * 4 + 4;
const BASE64_LEN: usize = BINARY_LEN.div_ceil(3) * 4;
const QR_TEXT_MAX: usize = 4 + BASE64_LEN + 3 + 8;

static mut LOW_BUFFER: [u32; SECTOR_WORDS] = [0; SECTOR_WORDS];


#[derive(Copy, Clone, PartialEq, Eq)]
enum Variant {
    /// SectorReader exactly as the SDK ships it (with the BFRD purge).
    SdkRead,
    /// Raw driver, no purge, BFRD then immediate DMA (the hl-psx classic).
    RawNoPurge,
    /// Raw driver WITH the Request=0 purge before Setmode, then BFRD+DMA.
    /// The discriminator: if RawNoPurge works and this fails, the purge
    /// kills DREQ on silicon.
    RawPurge,
    /// Raw, no purge, BFRD then wait for data-FIFO-not-empty before DMA.
    RawWaitFifo,
    /// Raw, no purge, PIO drain of the FIFO: no DMA involved at all.
    PioNoPurge,
    /// Raw, WITH purge, PIO drain: does the purge poison PIO too?
    PioPurge,
    /// Raw, no purge, DMA with CHCR sampled right after the kick and
    /// again after a spin, plus a busy-ever-seen flag in `extra`.
    ChcrProbe,
    /// SectorReader again: is the drive still sane after the matrix?
    SdkReadAgain,
}

impl Variant {
    const ALL: [Self; VARIANT_COUNT] = [
        Self::SdkRead,
        Self::RawNoPurge,
        Self::RawPurge,
        Self::RawWaitFifo,
        Self::PioNoPurge,
        Self::PioPurge,
        Self::ChcrProbe,
        Self::SdkReadAgain,
    ];

    const fn short(self) -> &'static str {
        match self {
            Self::SdkRead => "SDKRD",
            Self::RawNoPurge => "RAWNP",
            Self::RawPurge => "RAWPU",
            Self::RawWaitFifo => "RAWWT",
            Self::PioNoPurge => "PIONP",
            Self::PioPurge => "PIOPU",
            Self::ChcrProbe => "CHCRQ",
            Self::SdkReadAgain => "SDKR2",
        }
    }
}

#[derive(Copy, Clone)]
struct VariantRecord {
    fields: [u32; FIELD_COUNT],
}

impl VariantRecord {
    const fn empty() -> Self {
        Self {
            fields: [0; FIELD_COUNT],
        }
    }
}


pub(crate) struct CdChainProbe {
    next_variant: usize,
    complete: bool,
    run: u8,
    records: [VariantRecord; VARIANT_COUNT],
    qr_modules: [u8; (QR_SIZE * QR_SIZE + 7) / 8],
    qr_size: u8,
    binary_crc: u32,
}

impl CdChainProbe {
    pub(crate) const fn new() -> Self {
        Self {
            next_variant: 0,
            complete: false,
            run: 0,
            records: [VariantRecord::empty(); VARIANT_COUNT],
            qr_modules: [0; (QR_SIZE * QR_SIZE + 7) / 8],
            qr_size: 0,
            binary_crc: 0,
        }
    }

    pub(crate) fn start(&mut self) {
        self.restart();
    }

    /// Returns `(timing_realign, consume_navigation_input)`. One variant
    /// runs per call: each blocks for real drive time, so the scheduler
    /// realigns after every one.
    pub(crate) fn update(&mut self, ctx: &mut Ctx) -> (bool, bool) {
        if self.complete {
            if ctx.just_pressed(button::CROSS) {
                self.restart();
                return (true, true);
            }
            return (false, false);
        }
        self.step();
        (true, true)
    }

    pub(crate) fn restart(&mut self) {
        self.next_variant = 0;
        self.complete = false;
        self.run = self.run.wrapping_add(1);
        self.records = [VariantRecord::empty(); VARIANT_COUNT];
        self.qr_size = 0;
        tty::println("cd-chain-probe: cl2 begin");
    }

    fn step(&mut self) {
        if self.complete {
            return;
        }
        let index = self.next_variant;
        self.records[index] = run_variant(Variant::ALL[index], self.run);
        print_record(Variant::ALL[index], &self.records[index]);
        self.next_variant += 1;
        if self.next_variant == VARIANT_COUNT {
            self.complete = true;
            self.encode_qr();
            self.print_payload();
        }
    }

    fn bad_variants(&self) -> u32 {
        let mut bad = 0;
        for record in &self.records {
            let ok = record.fields[0] & 0x7 == 0x7 && record.fields[5] == record.fields[6];
            if !ok {
                bad += 1;
            }
        }
        bad
    }

    fn qr_module(&self, x: usize, y: usize) -> bool {
        let bit = y * QR_SIZE + x;
        self.qr_modules[bit / 8] & (1 << (bit & 7)) != 0
    }


    fn encode_qr(&mut self) {
        let mut binary = [0u8; BINARY_LEN];
        let mut out = BinaryBuffer::new(&mut binary);
        self.write_binary(&mut out, 0);
        let crc = crc32(&out.bytes()[..BINARY_LEN - 4]);
        binary[BINARY_LEN - 4..].copy_from_slice(&crc.to_le_bytes());
        self.binary_crc = crc;

        let mut payload = [0u8; BASE64_LEN];
        assert_eq!(base64_encode(&binary, &mut payload), BASE64_LEN, "CL2 Base64");
        let mut text = [0u8; QR_TEXT_MAX];
        let mut text_len = 0usize;
        append(&mut text, &mut text_len, b"CL1/");
        append(&mut text, &mut text_len, &payload);
        append(&mut text, &mut text_len, b"/C:");
        append(&mut text, &mut text_len, hex8(crc).as_str().as_bytes());
        let encoded = unsafe { core::str::from_utf8_unchecked(&text[..text_len]) };
        let mut temp = [0u8; QR_BUFFER_LEN];
        let mut output = [0u8; QR_BUFFER_LEN];
        let Ok(qr) = QrCode::encode_text(
            encoded,
            &mut temp,
            &mut output,
            QrCodeEcc::Medium,
            QR_VERSION,
            QR_VERSION,
            None,
            false,
        ) else {
            self.qr_size = 0;
            return;
        };
        self.qr_modules.fill(0);
        self.qr_size = qr.size() as u8;
        for y in 0..qr.size() {
            for x in 0..qr.size() {
                if qr.get_module(x, y) {
                    let bit = y as usize * QR_SIZE + x as usize;
                    self.qr_modules[bit / 8] |= 1 << (bit & 7);
                }
            }
        }
    }

    fn write_binary(&self, out: &mut BinaryBuffer<'_>, crc: u32) {
        out.push_bytes(b"CL1B");
        out.push_u8(3); // v3: CL2 mechanism matrix (raw driver + PIO)
        out.push_u8(VARIANT_COUNT as u8);
        out.push_u8(FIELD_COUNT as u8);
        out.push_u8(self.run);
        out.push_u32(CDTEST_LBA);
        out.push_u32(CDTEST_SECTORS);
        for record in self.records {
            for value in record.fields {
                out.push_u32(value);
            }
        }
        out.push_u32(crc);
        assert_eq!(out.len(), BINARY_LEN, "CL2 binary layout drift");
    }

    fn print_payload(&self) {
        let mut binary = [0u8; BINARY_LEN];
        let mut out = BinaryBuffer::new(&mut binary);
        self.write_binary(&mut out, self.binary_crc);
        let mut payload = [0u8; BASE64_LEN];
        base64_encode(&binary, &mut payload);
        tty::print("cd-chain-probe: CL1/");
        tty::print(unsafe { core::str::from_utf8_unchecked(&payload) });
        tty::print("/C:");
        tty::println(hex8(self.binary_crc).as_str());
    }

    pub(crate) fn draw(&self, font: &FontAtlas) {
        font.draw_text(8, 8, "CD MECHANISM MATRIX CL2 SA", (255, 232, 128));
        if !self.complete {
            for (i, variant) in Variant::ALL.iter().enumerate().take(self.next_variant) {
                let record = &self.records[i];
                let ok = record.fields[0] & 0x7 == 0x7 && record.fields[5] == record.fields[6];
                let y = 24 + i as i16 * 12;
                font.draw_text(8, y, variant.short(), (150, 170, 200));
                font.draw_text(
                    56,
                    y,
                    if ok { "OK" } else { "BAD" },
                    if ok { (96, 240, 128) } else { (255, 96, 64) },
                );
                font.draw_text(88, y, hex8(record.fields[3]).as_str(), (232, 236, 244));
                font.draw_text(164, y, hex8(record.fields[5]).as_str(), (200, 204, 220));
            }
            if self.next_variant < VARIANT_COUNT {
                let y = 24 + self.next_variant as i16 * 12;
                font.draw_text(8, y, Variant::ALL[self.next_variant].short(), (255, 216, 96));
                font.draw_text(56, y, "RUNNING", (255, 216, 96));
            }
            return;
        }
        if self.bad_variants() == 0 {
            font.draw_text(8, 24, "ALL VARIANTS OK - HOLD CAMERA ON QR", (96, 240, 128));
        } else {
            font.draw_text(8, 24, "BAD VARIANTS - HOLD CAMERA ON QR", (255, 96, 64));
        }
        font.draw_text(8, 230, "X RERUN MATRIX", (150, 170, 200));
        if self.qr_size as usize != QR_SIZE {
            font.draw_text(88, 112, "QR ENCODE FAILED", (255, 96, 96));
            return;
        }
        let total = (QR_SIZE as i16 + QR_QUIET * 2) * QR_SCALE;
        let left = (320 - total) / 2;
        let top = 42;
        gpu::draw_rect_flat(left, top, total as u16, total as u16, 255, 255, 255);
        let data_left = left + QR_QUIET * QR_SCALE;
        let data_top = top + QR_QUIET * QR_SCALE;
        for y in 0..QR_SIZE {
            let mut x = 0usize;
            while x < QR_SIZE {
                while x < QR_SIZE && !self.qr_module(x, y) {
                    x += 1;
                }
                let first = x;
                while x < QR_SIZE && self.qr_module(x, y) {
                    x += 1;
                }
                if first < x {
                    gpu::draw_rect_flat(
                        data_left + first as i16 * QR_SCALE,
                        data_top + y as i16 * QR_SCALE,
                        ((x - first) as i16 * QR_SCALE) as u16,
                        QR_SCALE as u16,
                        0,
                        0,
                        0,
                    );
                }
            }
        }
    }
}

// --- raw CD driver -------------------------------------------------------
//
// Open-coded mirror of SectorReader's polled sequence with every knob
// exposed, so the variants can vary the purge, the BFRD timing, and the
// transfer mechanism independently of the SDK build.

const CD_STATUS_REG: u32 = 0x1F80_1800;
const CD_RESPONSE_REG: u32 = 0x1F80_1801;
const CD_DATA_REG: u32 = 0x1F80_1802; // index-0 reads pop the data FIFO
const CD_IRQ_REG: u32 = 0x1F80_1803;
const STATUS_PARAM_NOT_FULL: u8 = 1 << 4;
const STATUS_RESPONSE_NOT_EMPTY: u8 = 1 << 5;
const STATUS_DATA_NOT_EMPTY: u8 = 1 << 6;
const IRQ_DATA_READY: u8 = 1;
const IRQ_ACK: u8 = 3;
const IRQ_ERROR: u8 = 5;
const CMD_SETLOC: u8 = 0x02;
const CMD_READN: u8 = 0x06;
const CMD_PAUSE: u8 = 0x09;
const CMD_SETMODE: u8 = 0x0E;
const MODE_DOUBLE_2048: u8 = 0x80;

fn wr_index(index: u8) {
    unsafe { psx_io::write8(CD_STATUS_REG, index & 3) };
}

fn cd_status() -> u8 {
    wr_index(0);
    unsafe { psx_io::read8(CD_STATUS_REG) }
}

fn irq_flag() -> u8 {
    wr_index(1);
    let f = unsafe { psx_io::read8(CD_IRQ_REG) } & 0x1F;
    wr_index(0);
    f
}

fn ack_all() {
    wr_index(1);
    unsafe { psx_io::write8(CD_IRQ_REG, 0x5F) };
    psx_io::irq::ack(1 << psx_io::irq::source::CDROM);
    wr_index(0);
}

fn drain_responses() {
    wr_index(0);
    let mut guard = 0;
    while unsafe { psx_io::read8(CD_STATUS_REG) } & STATUS_RESPONSE_NOT_EMPTY != 0 && guard < 256 {
        let _ = unsafe { psx_io::read8(CD_RESPONSE_REG) };
        guard += 1;
    }
}

/// Dispatch one command and wait for `expected`; `false` on INT5/timeout.
fn send_cmd(command: u8, params: &[u8], expected: u8) -> bool {
    ack_all();
    drain_responses();
    // Reset the parameter FIFO before queueing parameters.
    wr_index(1);
    unsafe { psx_io::write8(CD_IRQ_REG, 0x40) };
    wr_index(0);
    for &p in params {
        let mut spins = 0u32;
        while unsafe { psx_io::read8(CD_STATUS_REG) } & STATUS_PARAM_NOT_FULL == 0 {
            spins += 1;
            if spins > CD_SPINS {
                return false;
            }
        }
        unsafe { psx_io::write8(CD_DATA_REG, p) }; // param FIFO shares 1F801802 writes
    }
    unsafe { psx_io::write8(CD_RESPONSE_REG, command) };
    let mut spins = 0u32;
    loop {
        let flag = irq_flag();
        if flag == expected {
            drain_responses();
            wr_index(1);
            unsafe { psx_io::write8(CD_IRQ_REG, expected) };
            psx_io::irq::ack(1 << psx_io::irq::source::CDROM);
            wr_index(0);
            return true;
        }
        if flag == IRQ_ERROR {
            drain_responses();
            ack_all();
            return false;
        }
        if flag != 0 {
            drain_responses();
            ack_all();
        }
        spins += 1;
        if spins > CD_SPINS {
            return false;
        }
    }
}

const fn bin_to_bcd(v: u8) -> u8 {
    ((v / 10) << 4) | (v % 10)
}

fn lba_to_msf(lba: u32) -> [u8; 3] {
    let abs = lba + 150;
    [
        bin_to_bcd((abs / (60 * 75)) as u8),
        bin_to_bcd(((abs / 75) % 60) as u8),
        bin_to_bcd((abs % 75) as u8),
    ]
}

/// Prepare the controller: VBlank-only I_MASK, channel 3 enabled, IRQs
/// unmasked at the controller, pending flags drained, optional purge.
fn raw_prepare(purge: bool) -> bool {
    psx_io::irq::set_mask(1 << psx_io::irq::source::VBLANK);
    psx_io::irq::ack(1 << psx_io::irq::source::CDROM);
    psx_io::dma::enable_channel(psx_io::dma::Channel::Cdrom);
    wr_index(1);
    unsafe { psx_io::write8(CD_DATA_REG, 0x1F) }; // IRQ-enable register at index 1
    wr_index(0);
    let mut guard = 0;
    while irq_flag() != 0 && guard < 16 {
        drain_responses();
        ack_all();
        guard += 1;
    }
    ack_all();
    if purge {
        wr_index(0);
        unsafe { psx_io::write8(CD_IRQ_REG, 0x00) }; // Request: BFRD off
    }
    send_cmd(CMD_SETMODE, &[MODE_DOUBLE_2048], IRQ_ACK)
}

/// Wait for the DataReady flag of the running ReadN stream.
fn wait_data_ready() -> bool {
    let mut spins = 0u32;
    loop {
        let flag = irq_flag();
        if flag == IRQ_DATA_READY {
            return true;
        }
        if flag == IRQ_ERROR {
            return false;
        }
        spins += 1;
        if spins > 4_000_000 {
            return false;
        }
    }
}

fn ack_data_ready() {
    drain_responses();
    wr_index(1);
    unsafe { psx_io::write8(CD_IRQ_REG, IRQ_DATA_READY) };
    psx_io::irq::ack(1 << psx_io::irq::source::CDROM);
    wr_index(0);
}

enum Transfer {
    Dma { wait_fifo: bool, probe_chcr: bool },
    Pio,
}

/// One full raw read of `CDTEST_LBA` into `buffer`. Returns
/// (ok_bits, chcr_kick, chcr_late, extra).
fn raw_read(purge: bool, transfer: Transfer, buffer: *mut [u32; SECTOR_WORDS]) -> (u32, u32, u32, u32) {
    let mut ok = 0u32;
    if raw_prepare(purge) {
        ok |= 1;
    } else {
        return (ok, 0, 0, 0);
    }
    let msf = lba_to_msf(CDTEST_LBA);
    if send_cmd(CMD_SETLOC, &msf, IRQ_ACK) && send_cmd(CMD_READN, &[], IRQ_ACK) {
        ok |= 2;
    } else {
        return (ok, 0, 0, 0);
    }
    if !wait_data_ready() {
        let _ = send_cmd(CMD_PAUSE, &[], IRQ_ACK);
        return (ok, 0, 0, 0);
    }

    let mut chcr_kick = 0u32;
    let mut chcr_late = 0u32;
    let mut extra = 0u32;
    match transfer {
        Transfer::Dma { wait_fifo, probe_chcr } => {
            // Arm BFRD, optionally wait until the FIFO reports data.
            wr_index(0);
            unsafe { psx_io::write8(CD_IRQ_REG, 0x80) };
            wr_index(0);
            if wait_fifo {
                let mut spins = 0u32;
                while cd_status() & STATUS_DATA_NOT_EMPTY == 0 && spins < 2_000_000 {
                    spins += 1;
                }
                extra = spins;
            }
            psx_io::dma::set_madr(psx_io::dma::Channel::Cdrom, buffer as u32);
            psx_io::dma::set_bcr_manual(psx_io::dma::Channel::Cdrom, SECTOR_WORDS as u16);
            psx_io::dma::set_chcr(psx_io::dma::Channel::Cdrom, 0x1140_0100);
            if probe_chcr {
                chcr_kick = unsafe {
                    psx_io::read32(psx_io::dma::Channel::Cdrom.base() + 0x8)
                };
            }
            let mut busy_seen = false;
            let mut spins = 0u32;
            while psx_io::dma::is_busy(psx_io::dma::Channel::Cdrom) && spins < 65_536 {
                busy_seen = true;
                spins += 1;
            }
            if probe_chcr {
                chcr_late = unsafe {
                    psx_io::read32(psx_io::dma::Channel::Cdrom.base() + 0x8)
                };
                extra |= (busy_seen as u32) << 31;
            }
            psx_io::irq::ack(1 << psx_io::irq::source::DMA);
            ok |= 4;
        }
        Transfer::Pio => {
            wr_index(0);
            unsafe { psx_io::write8(CD_IRQ_REG, 0x80) };
            wr_index(0);
            let mut spins = 0u32;
            while cd_status() & STATUS_DATA_NOT_EMPTY == 0 && spins < 2_000_000 {
                spins += 1;
            }
            extra = spins;
            if cd_status() & STATUS_DATA_NOT_EMPTY != 0 {
                // Drain 2048 bytes as single-byte pops: 16-bit reads of
                // the data port duplicate bytes in the emulator (silicon
                // pops two), so byte reads are the one width both worlds
                // agree on. Slow is fine; unambiguous is the point.
                for word_index in 0..SECTOR_WORDS {
                    let b0 = unsafe { psx_io::read8(CD_DATA_REG) } as u32;
                    let b1 = unsafe { psx_io::read8(CD_DATA_REG) } as u32;
                    let b2 = unsafe { psx_io::read8(CD_DATA_REG) } as u32;
                    let b3 = unsafe { psx_io::read8(CD_DATA_REG) } as u32;
                    unsafe { (*buffer)[word_index] = (b3 << 24) | (b2 << 16) | (b1 << 8) | b0 };
                }
                ok |= 4;
            }
        }
    }
    ack_data_ready();
    let _ = send_cmd(CMD_PAUSE, &[], IRQ_ACK);
    (ok, chcr_kick, chcr_late, extra)
}

/// Execute one variant, fully blocking, and return its record.
fn run_variant(variant: Variant, run: u8) -> VariantRecord {
    let mut record = VariantRecord::empty();
    let buffer: *mut [u32; SECTOR_WORDS] = &raw mut LOW_BUFFER;
    unsafe { (*buffer).fill(0xDEAD_BEEF) };

    let (ok, chcr_kick, chcr_late, extra) = match variant {
        Variant::SdkRead | Variant::SdkReadAgain => {
            let mut reader = SectorReader::new();
            let ok_prepare = unsafe { reader.prepare() };
            let ok_start = ok_prepare && unsafe { reader.start_read(CDTEST_LBA) };
            let ok_read = ok_start && unsafe { reader.read_sector(&mut *buffer) };
            let diag = reader.diag();
            unsafe { reader.stop() };
            record.fields[9] = diag;
            (
                (ok_prepare as u32) | ((ok_start as u32) << 1) | ((ok_read as u32) << 2),
                0,
                0,
                0,
            )
        }
        Variant::RawNoPurge => raw_read(false, Transfer::Dma { wait_fifo: false, probe_chcr: false }, buffer),
        Variant::RawPurge => raw_read(true, Transfer::Dma { wait_fifo: false, probe_chcr: false }, buffer),
        Variant::RawWaitFifo => raw_read(false, Transfer::Dma { wait_fifo: true, probe_chcr: false }, buffer),
        Variant::PioNoPurge => raw_read(false, Transfer::Pio, buffer),
        Variant::PioPurge => raw_read(true, Transfer::Pio, buffer),
        Variant::ChcrProbe => raw_read(false, Transfer::Dma { wait_fifo: false, probe_chcr: true }, buffer),
    };

    let words = unsafe { &*buffer };
    record.fields[0] |= ((variant as u32) << 24) | (ok & 0x7) | ((run as u32) << 8);
    record.fields[1] = chcr_kick;
    record.fields[2] = chcr_late;
    record.fields[3] = words[0];
    record.fields[4] = words[1];
    record.fields[5] = fnv1a_words(words);
    record.fields[6] = expected_sector_fnv();
    record.fields[7] = unsafe { psx_io::read32(psx_io::dma::Channel::Cdrom.base()) };
    record.fields[8] = drive_state();
    if record.fields[9] == 0 {
        record.fields[9] = extra;
    }
    record
}

fn drive_state() -> u32 {
    let hw_status = unsafe { psx_io::read8(0x1F80_1800) };
    let irq_flag = unsafe {
        psx_io::write8(0x1F80_1800, 1);
        let f = psx_io::read8(0x1F80_1803) & 0x1F;
        psx_io::write8(0x1F80_1800, 0);
        f
    };
    let stat = cdrom::try_get_stat(CD_SPINS)
        .and_then(|r| r.bytes().first().copied())
        .unwrap_or(0xEE);
    ((hw_status as u32) << 24) | ((irq_flag as u32) << 16) | ((stat as u32) << 8)
}


const fn expected_byte(index: usize) -> u8 {
    const MAGIC: [u8; 8] = *b"PSOXSTRM";
    if index < 8 {
        MAGIC[index]
    } else if index < 12 {
        (CDTEST_SECTORS.to_le_bytes())[index - 8]
    } else {
        let mixed = (index as u32)
            .wrapping_mul(37)
            .wrapping_add((index as u32) >> 3)
            .wrapping_add(0x5D);
        mixed as u8
    }
}

fn expected_sector_fnv() -> u32 {
    let mut hash: u32 = 0x811C_9DC5;
    let mut index = 0usize;
    while index < SECTOR_WORDS * 4 {
        hash ^= expected_byte(index) as u32;
        hash = hash.wrapping_mul(0x0100_0193);
        index += 1;
    }
    hash
}

fn fnv1a_words(words: &[u32; SECTOR_WORDS]) -> u32 {
    let mut hash: u32 = 0x811C_9DC5;
    for word in words {
        for byte in word.to_le_bytes() {
            hash ^= byte as u32;
            hash = hash.wrapping_mul(0x0100_0193);
        }
    }
    hash
}

fn print_record(variant: Variant, record: &VariantRecord) {
    tty::print("cd-chain-probe: ");
    tty::print(variant.short());
    tty::print(" ok=");
    tty::print(hex2((record.fields[0] & 0xFF) as u8).as_str());
    tty::print(" dpcr=");
    tty::print(hex8(record.fields[1]).as_str());
    tty::print(" w0=");
    tty::print(hex8(record.fields[3]).as_str());
    tty::print(" fnv=");
    tty::print(hex8(record.fields[5]).as_str());
    tty::print(" exp=");
    tty::print(hex8(record.fields[6]).as_str());
    tty::print(" diag=");
    tty::println(hex8(record.fields[9]).as_str());
}

// --- tiny hex formatters -------------------------------------------------

struct Hex<const N: usize> {
    buf: [u8; N],
}

impl<const N: usize> Hex<N> {
    fn as_str(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(&self.buf) }
    }
}

fn hex8(v: u32) -> Hex<8> {
    const H: &[u8; 16] = b"0123456789ABCDEF";
    let mut buf = [0u8; 8];
    for (i, slot) in buf.iter_mut().enumerate() {
        *slot = H[((v >> ((7 - i) * 4)) & 0xF) as usize];
    }
    Hex { buf }
}

fn hex2(v: u8) -> Hex<2> {
    const H: &[u8; 16] = b"0123456789ABCDEF";
    Hex {
        buf: [H[(v >> 4) as usize], H[(v & 0xF) as usize]],
    }
}

// --- transport helpers ---------------------------------------------------

fn append(target: &mut [u8], len: &mut usize, bytes: &[u8]) {
    target[*len..*len + bytes.len()].copy_from_slice(bytes);
    *len += bytes.len();
}

struct BinaryBuffer<'a> {
    bytes: &'a mut [u8],
    len: usize,
}

impl<'a> BinaryBuffer<'a> {
    fn new(bytes: &'a mut [u8]) -> Self {
        Self { bytes, len: 0 }
    }

    fn push_bytes(&mut self, value: &[u8]) {
        self.bytes[self.len..self.len + value.len()].copy_from_slice(value);
        self.len += value.len();
    }

    fn push_u8(&mut self, value: u8) {
        self.bytes[self.len] = value;
        self.len += 1;
    }

    fn push_u32(&mut self, value: u32) {
        self.push_bytes(&value.to_le_bytes());
    }

    fn len(&self) -> usize {
        self.len
    }

    fn bytes(&self) -> &[u8] {
        self.bytes
    }
}

fn base64_encode(input: &[u8], output: &mut [u8]) -> usize {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = 0usize;
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        output[out] = TABLE[(triple >> 18) as usize & 63];
        output[out + 1] = TABLE[(triple >> 12) as usize & 63];
        output[out + 2] = if chunk.len() > 1 {
            TABLE[(triple >> 6) as usize & 63]
        } else {
            b'='
        };
        output[out + 3] = if chunk.len() > 2 {
            TABLE[triple as usize & 63]
        } else {
            b'='
        };
        out += 4;
    }
    out
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}
