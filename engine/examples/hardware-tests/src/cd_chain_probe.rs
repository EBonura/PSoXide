//! CL1: chain-load CD read matrix.
//!
//! Four demo-disc burns failed with the loader's header read returning
//! zeros (no drive error) while the identical `SectorReader` works in
//! every standalone program and in the emulator. This probe reproduces
//! the chain-load environment stepwise on a standalone boot and reads
//! the deterministic CDTEST region under each variant, so one QR photo
//! says which ingredient breaks the read on silicon:
//!
//! - V0  control: read exactly as booted (BIOS DMA/IRQ state).
//! - V1  DPCR zeroed first (the original quiesce state).
//! - V2  DPCR set to the BIOS ladder 0x07654321 (the current quiesce).
//! - V3  DMA destination high in RAM at the loader's own address range.
//! - V4  CD-DA played then stopped-and-settled first (the drive history
//!       a real chain-load inherits from the menu).
//! - V5  full replica: CD-DA prestress + quiesce IRQ masking + ladder
//!       DPCR + high destination buffer.
//! - V6  control again, proving the drive still behaves after the run.
//!
//! Each variant records: OK bits for prepare/start/read, the DPCR
//! readback, the first three words read (expected: "PSOX" "STRM" then
//! the sector count), FNV-1a over the whole sector vs the expected
//! value computed from the generator formula, channel-3 MADR after the
//! DMA, drive/controller status, and the reader's diag snapshot.

use psx_engine::{button, Ctx};
use psx_font::FontAtlas;
use psx_io::cdrom;
use psx_io::timers;
use psx_pack::cd::{SectorReader, SECTOR_WORDS};
use psx_rt::tty;
use qrcodegen_no_heap::{QrCode, QrCodeEcc, Version};

use crate::{hex2, hex8};

/// First LBA of the deterministic CDTEST.BIN region (matches main.rs).
const CDTEST_LBA: u32 = 424;
/// Sector count baked into the CDTEST header by `make hardware-tests-disc`.
const CDTEST_SECTORS: u32 = 600;
/// Spin budget for reader handshakes, mirroring the loader's generous side.
const CD_SPINS: u32 = 0x10_0000;

const VARIANT_COUNT: usize = 7;
const FIELD_COUNT: usize = 10;

/// The loader's own address range: chain-load DMA lands up here, the
/// suite's ordinary buffers live in low .bss. 2 KiB at the chain
/// loader's link base; the suite touches nothing at this address.
const HIGH_BUFFER_ADDR: u32 = 0x801F_0000;

/// Timer 1 on the HBlank clock (~63.9 us per tick), like the CD checks.
const TIMER1_HBLANK_MODE: u16 = 0x0100;
/// CD-DA prestress duration, ~2 s of HBlank ticks.
const PRESTRESS_HBLANKS: u16 = 31_250;

const QR_VERSION: Version = Version::new(17);
const QR_SIZE: usize = 85;
const QR_BUFFER_LEN: usize = QR_VERSION.buffer_len();
const QR_SCALE: i16 = 2;
const QR_QUIET: i16 = 4;
// b"CL1B" + version + counts + run (8) + lba + sectors (8) = 16 header
// bytes, 7 x 10 x u32 records, u32 CRC.
const BINARY_LEN: usize = 16 + VARIANT_COUNT * FIELD_COUNT * 4 + 4;
const BASE64_LEN: usize = BINARY_LEN.div_ceil(3) * 4;
const QR_TEXT_MAX: usize = 4 + BASE64_LEN + 3 + 8;

static mut LOW_BUFFER: [u32; SECTOR_WORDS] = [0; SECTOR_WORDS];

#[derive(Copy, Clone, PartialEq, Eq)]
enum Variant {
    Control,
    DpcrZero,
    DpcrLadder,
    HighBuffer,
    CddaStress,
    FullReplica,
    ControlAgain,
}

impl Variant {
    const ALL: [Self; VARIANT_COUNT] = [
        Self::Control,
        Self::DpcrZero,
        Self::DpcrLadder,
        Self::HighBuffer,
        Self::CddaStress,
        Self::FullReplica,
        Self::ControlAgain,
    ];

    const fn short(self) -> &'static str {
        match self {
            Self::Control => "CTRL",
            Self::DpcrZero => "DPCR0",
            Self::DpcrLadder => "LADDR",
            Self::HighBuffer => "HIBUF",
            Self::CddaStress => "CDDA",
            Self::FullReplica => "REPL",
            Self::ControlAgain => "CTRL2",
        }
    }

    const fn prestress(self) -> bool {
        matches!(self, Self::CddaStress | Self::FullReplica)
    }

    const fn high_buffer(self) -> bool {
        matches!(self, Self::HighBuffer | Self::FullReplica)
    }

    const fn dpcr(self) -> Option<u32> {
        match self {
            Self::DpcrZero => Some(0),
            Self::DpcrLadder | Self::FullReplica => Some(0x0765_4321),
            _ => None,
        }
    }

    const fn mask_irqs(self) -> bool {
        matches!(self, Self::FullReplica)
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
        self.next_variant = 0;
        self.complete = false;
        self.run = self.run.wrapping_add(1);
        self.records = [VariantRecord::empty(); VARIANT_COUNT];
        self.qr_size = 0;
        tty::println("hardware-tests: cl1 begin");
    }

    pub(crate) fn restart(&mut self) {
        self.start();
    }

    /// Returns `(timing_realign, consume_navigation_input)`. One variant
    /// runs per call; each is blocking (CD mechanics take real time), so
    /// the scheduler realigns after every one.
    pub(crate) fn update(&mut self, ctx: &mut Ctx) -> (bool, bool) {
        if self.complete {
            if ctx.just_pressed(button::CROSS) {
                self.start();
                return (true, true);
            }
            return (false, false);
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
        (true, true)
    }

    fn encode_qr(&mut self) {
        let mut binary = [0u8; BINARY_LEN];
        let mut out = BinaryBuffer::new(&mut binary);
        self.write_binary(&mut out, 0);
        let crc = crc32(&out.bytes()[..BINARY_LEN - 4]);
        binary[BINARY_LEN - 4..].copy_from_slice(&crc.to_le_bytes());
        self.binary_crc = crc;

        let mut payload = [0u8; BASE64_LEN];
        assert_eq!(base64_encode(&binary, &mut payload), BASE64_LEN, "CL1 Base64");
        let mut text = [0u8; QR_TEXT_MAX];
        let mut text_len = 0usize;
        append(&mut text, &mut text_len, b"CL1/");
        append(&mut text, &mut text_len, &payload);
        append(&mut text, &mut text_len, b"/C:");
        append(&mut text, &mut text_len, hex8(crc).digits().as_bytes());
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
        out.push_u8(1);
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
        assert_eq!(out.len(), BINARY_LEN, "CL1 binary layout drift");
    }

    fn print_payload(&self) {
        let mut binary = [0u8; BINARY_LEN];
        let mut out = BinaryBuffer::new(&mut binary);
        self.write_binary(&mut out, self.binary_crc);
        let mut payload = [0u8; BASE64_LEN];
        base64_encode(&binary, &mut payload);
        tty::print("hardware-tests: cl1 CL1/");
        tty::print(unsafe { core::str::from_utf8_unchecked(&payload) });
        tty::print("/C:");
        tty::println(hex8(self.binary_crc).digits());
    }

    fn qr_module(&self, x: usize, y: usize) -> bool {
        let bit = y * QR_SIZE + x;
        self.qr_modules[bit / 8] & (1 << (bit & 7)) != 0
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

    pub(crate) fn draw(&self, font: &FontAtlas) {
        font.draw_text(8, 8, "CD CHAIN-LOAD MATRIX CL1", (255, 232, 128));
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
                font.draw_text(88, y, hex8(record.fields[2]).digits(), (232, 236, 244));
                font.draw_text(164, y, hex8(record.fields[5]).digits(), (200, 204, 220));
            }
            let y = 24 + self.next_variant as i16 * 12;
            font.draw_text(8, y, Variant::ALL[self.next_variant].short(), (255, 216, 96));
            font.draw_text(56, y, "RUNNING - SCREEN MAY PAUSE", (255, 216, 96));
            return;
        }
        // Complete: the table lives in the QR now; the screen becomes the
        // transport. Same geometry PA4 proved on camera.
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
        psx_gpu::draw_rect_flat(left, top, total as u16, total as u16, 255, 255, 255);
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
                    psx_gpu::draw_rect_flat(
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

/// Execute one variant, fully blocking, and return its record.
fn run_variant(variant: Variant, run: u8) -> VariantRecord {
    let mut record = VariantRecord::empty();

    if variant.prestress() {
        // The menu's drive history: mode, demute, play the tone track,
        // two seconds of playback, then the SDK's stop-and-settle.
        let _ = cdrom::try_set_mode(cdrom::MODE_CDDA, CD_SPINS);
        let _ = cdrom::try_demute(CD_SPINS);
        let _ = cdrom::try_play_track(2, CD_SPINS);
        hblank_wait(PRESTRESS_HBLANKS);
        let settled = cdrom::stop_and_settle(CD_SPINS, 240);
        record.fields[0] |= (settled as u32) << 3;
    }

    if variant.mask_irqs() {
        unsafe {
            psx_io::write32(0x1F80_1074, 0); // I_MASK
            psx_io::write32(0x1F80_1070, 0); // I_STAT
        }
    }
    if let Some(dpcr) = variant.dpcr() {
        unsafe { psx_io::write32(psx_io::dma::DPCR, dpcr) };
    }

    let buffer: *mut [u32; SECTOR_WORDS] = if variant.high_buffer() {
        HIGH_BUFFER_ADDR as *mut [u32; SECTOR_WORDS]
    } else {
        &raw mut LOW_BUFFER
    };
    unsafe { (*buffer).fill(0xDEAD_BEEF) };

    let mut reader = SectorReader::new();
    let ok_prepare = unsafe { reader.prepare() };
    let dpcr_after = unsafe { psx_io::read32(psx_io::dma::DPCR) };
    let ok_start = ok_prepare && unsafe { reader.start_read(CDTEST_LBA) };
    let ok_read = ok_start && unsafe { reader.read_sector(&mut *buffer) };
    let madr_after = unsafe {
        psx_io::read32(psx_io::dma::Channel::Cdrom.base())
    };
    unsafe { reader.stop() };

    let words = unsafe { &*buffer };
    let observed_fnv = fnv1a_words(words);
    let expected_fnv = expected_sector_fnv();

    record.fields[0] |= ((variant as u32) << 24)
        | (ok_prepare as u32)
        | ((ok_start as u32) << 1)
        | ((ok_read as u32) << 2)
        | ((run as u32) << 8);
    record.fields[1] = dpcr_after;
    record.fields[2] = words[0];
    record.fields[3] = words[1];
    record.fields[4] = words[2];
    record.fields[5] = observed_fnv;
    record.fields[6] = expected_fnv;
    record.fields[7] = madr_after;
    record.fields[8] = drive_state();
    record.fields[9] = reader.diag();
    record
}

/// Raw controller + drive status snapshot: HW status register, latched
/// IRQ flags, and a GetStat status byte.
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

/// Busy-wait on Timer 1's HBlank clock; no interrupt dependency.
fn hblank_wait(ticks: u16) {
    timers::set_mode(timers::Timer::Timer1, TIMER1_HBLANK_MODE);
    timers::set_counter(timers::Timer::Timer1, 0);
    while timers::counter(timers::Timer::Timer1) < ticks {
        core::hint::spin_loop();
    }
}

/// Expected byte `index` of CDTEST sector 0, mirroring
/// `psx_iso::cd_stream_bench_expected_byte` for the burned sector count.
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
    tty::print("hardware-tests: cl1 ");
    tty::print(variant.short());
    tty::print(" ok=");
    tty::print(hex2((record.fields[0] & 0xFF) as u8).as_str());
    tty::print(" dpcr=");
    tty::print(hex8(record.fields[1]).digits());
    tty::print(" w0=");
    tty::print(hex8(record.fields[2]).digits());
    tty::print(" fnv=");
    tty::print(hex8(record.fields[5]).digits());
    tty::print(" exp=");
    tty::print(hex8(record.fields[6]).digits());
    tty::print(" diag=");
    tty::println(hex8(record.fields[9]).digits());
}

// --- module-local transport helpers, per suite convention ---------------

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
