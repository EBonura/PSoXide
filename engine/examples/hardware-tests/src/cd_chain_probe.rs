//! CL2: the CD read-mechanism matrix.
//!
//! Ported here from the standalone probe disc so the suite is the one
//! place hardware questions get asked. Four demo-disc burns died on the
//! chain loader's header read returning zeros, and CL1 convicted the
//! transfer itself: commands and DataReady succeed, the sector sits in
//! the controller FIFO, and the DMA moves nothing while MADR stays put.
//!
//! Each variant reads the deterministic CDTEST region a different way,
//! so one record each says which mechanism silicon actually honours:
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
//! Per variant, record `0x500 + n`: bits 0-2 the OK bits (prepare, start,
//! read), bit 3 the sector's FNV equal to the expected one, bit 4 the DMA
//! channel was seen busy (the CHCR probe only); then the reader's diag
//! snapshot (or the FIFO wait count) low half; then the drive and controller
//! status bytes. A variant is clean when its first field reads 0x0F.

use crate::console_tests::record;
use crate::TimingRecord;
use psx_pack::cd::{SectorReader, SECTOR_WORDS};
use psx_rt::tty;

/// rec cl2_variant: ok_bits_and_data_match, diag_or_fifo_wait_low, drive_state_high (eight records, 0x500-0x507)
pub(crate) const CL2_RECORD: u16 = 0x500;

/// First LBA of the CDTEST region on THIS disc (verified against the
/// built image by scanning for the sector-aligned "PSOXSTRM" header; a
/// drift reads BAD rather than lying, since the magic words are checked).
const CDTEST_LBA: u32 = 564;
/// Sector count the disc build bakes into the CDTEST header.
const CDTEST_SECTORS: u32 = 460;
const CD_SPINS: u32 = 0x10_0000;

const VARIANT_COUNT: usize = 8;
const FIELD_COUNT: usize = 10;

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
    unsafe { psx_io::write_u8(CD_STATUS_REG, index & 3) };
}

fn cd_status() -> u8 {
    wr_index(0);
    unsafe { psx_io::read_u8(CD_STATUS_REG) }
}

fn irq_flag() -> u8 {
    wr_index(1);
    let f = unsafe { psx_io::read_u8(CD_IRQ_REG) } & 0x1F;
    wr_index(0);
    f
}

fn ack_all() {
    wr_index(1);
    unsafe { psx_io::write_u8(CD_IRQ_REG, 0x5F) };
    psx_io::irq::acknowledge(1 << psx_hw::irq::source::CDROM);
    wr_index(0);
}

fn drain_responses() {
    wr_index(0);
    let mut guard = 0;
    while unsafe { psx_io::read_u8(CD_STATUS_REG) } & STATUS_RESPONSE_NOT_EMPTY != 0 && guard < 256
    {
        let _ = unsafe { psx_io::read_u8(CD_RESPONSE_REG) };
        guard += 1;
    }
}

/// Dispatch one command and wait for `expected`; `false` on INT5/timeout.
fn send_cmd(command: u8, params: &[u8], expected: u8) -> bool {
    ack_all();
    drain_responses();
    // Reset the parameter FIFO before queueing parameters.
    wr_index(1);
    unsafe { psx_io::write_u8(CD_IRQ_REG, 0x40) };
    wr_index(0);
    for &p in params {
        let mut spins = 0u32;
        while unsafe { psx_io::read_u8(CD_STATUS_REG) } & STATUS_PARAM_NOT_FULL == 0 {
            spins += 1;
            if spins > CD_SPINS {
                return false;
            }
        }
        unsafe { psx_io::write_u8(CD_DATA_REG, p) }; // param FIFO shares 1F801802 writes
    }
    unsafe { psx_io::write_u8(CD_RESPONSE_REG, command) };
    let mut spins = 0u32;
    loop {
        let flag = irq_flag();
        if flag == expected {
            drain_responses();
            wr_index(1);
            unsafe { psx_io::write_u8(CD_IRQ_REG, expected) };
            psx_io::irq::acknowledge(1 << psx_hw::irq::source::CDROM);
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
    psx_io::irq::set_mask(1 << psx_hw::irq::source::VBLANK);
    psx_io::irq::acknowledge(1 << psx_hw::irq::source::CDROM);
    psx_io::dma::enable_channel(psx_io::dma::Channel::Cd);
    wr_index(1);
    unsafe { psx_io::write_u8(CD_DATA_REG, 0x1F) }; // IRQ-enable register at index 1
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
        unsafe { psx_io::write_u8(CD_IRQ_REG, 0x00) }; // Request: BFRD off
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
    unsafe { psx_io::write_u8(CD_IRQ_REG, IRQ_DATA_READY) };
    psx_io::irq::acknowledge(1 << psx_hw::irq::source::CDROM);
    wr_index(0);
}

enum Transfer {
    Dma { wait_fifo: bool, probe_chcr: bool },
    Pio,
}

/// One full raw read of `CDTEST_LBA` into `buffer`. Returns
/// (ok_bits, chcr_kick, chcr_late, extra).
fn raw_read(
    purge: bool,
    transfer: Transfer,
    buffer: *mut [u32; SECTOR_WORDS],
) -> (u32, u32, u32, u32) {
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
        Transfer::Dma {
            wait_fifo,
            probe_chcr,
        } => {
            // Arm BFRD, optionally wait until the FIFO reports data.
            wr_index(0);
            unsafe { psx_io::write_u8(CD_IRQ_REG, 0x80) };
            wr_index(0);
            if wait_fifo {
                let mut spins = 0u32;
                while cd_status() & STATUS_DATA_NOT_EMPTY == 0 && spins < 2_000_000 {
                    spins += 1;
                }
                extra = spins;
            }
            // SAFETY: silicon probe: the transfer touches only memory this probe
            // owns, which stays live and untouched until the probe waits the
            // channel idle or aborts it.
            unsafe {
                psx_io::dma::raw::set_address(psx_io::dma::Channel::Cd, buffer as u32);
                psx_io::dma::raw::set_size(
                    psx_io::dma::Channel::Cd,
                    psx_io::dma::size_words(SECTOR_WORDS as u16),
                );
                psx_io::dma::raw::set_control(psx_io::dma::Channel::Cd, 0x1140_0100);
            }
            if probe_chcr {
                chcr_kick =
                    unsafe { psx_io::read_u32(psx_io::dma::Channel::Cd.register_base() + 0x8) };
            }
            let mut busy_seen = false;
            let mut spins = 0u32;
            while psx_io::dma::is_busy(psx_io::dma::Channel::Cd) && spins < 65_536 {
                busy_seen = true;
                spins += 1;
            }
            if probe_chcr {
                chcr_late =
                    unsafe { psx_io::read_u32(psx_io::dma::Channel::Cd.register_base() + 0x8) };
                extra |= (busy_seen as u32) << 31;
            }
            psx_io::irq::acknowledge(1 << psx_hw::irq::source::DMA);
            ok |= 4;
        }
        Transfer::Pio => {
            wr_index(0);
            unsafe { psx_io::write_u8(CD_IRQ_REG, 0x80) };
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
                    let b0 = unsafe { psx_io::read_u8(CD_DATA_REG) } as u32;
                    let b1 = unsafe { psx_io::read_u8(CD_DATA_REG) } as u32;
                    let b2 = unsafe { psx_io::read_u8(CD_DATA_REG) } as u32;
                    let b3 = unsafe { psx_io::read_u8(CD_DATA_REG) } as u32;
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
            let ok_prepare = reader.prepare();
            let ok_start = ok_prepare && reader.start_read(CDTEST_LBA);
            let ok_read = ok_start && unsafe { reader.read_sector(&mut *buffer) };
            let diag = reader.diagnostics();
            reader.stop();
            record.fields[9] = diag;
            (
                (ok_prepare as u32) | ((ok_start as u32) << 1) | ((ok_read as u32) << 2),
                0,
                0,
                0,
            )
        }
        Variant::RawNoPurge => raw_read(
            false,
            Transfer::Dma {
                wait_fifo: false,
                probe_chcr: false,
            },
            buffer,
        ),
        Variant::RawPurge => raw_read(
            true,
            Transfer::Dma {
                wait_fifo: false,
                probe_chcr: false,
            },
            buffer,
        ),
        Variant::RawWaitFifo => raw_read(
            false,
            Transfer::Dma {
                wait_fifo: true,
                probe_chcr: false,
            },
            buffer,
        ),
        Variant::PioNoPurge => raw_read(false, Transfer::Pio, buffer),
        Variant::PioPurge => raw_read(true, Transfer::Pio, buffer),
        Variant::ChcrProbe => raw_read(
            false,
            Transfer::Dma {
                wait_fifo: false,
                probe_chcr: true,
            },
            buffer,
        ),
    };

    let words = unsafe { &*buffer };
    record.fields[0] |= ((variant as u32) << 24) | (ok & 0x7) | ((run as u32) << 8);
    record.fields[1] = chcr_kick;
    record.fields[2] = chcr_late;
    record.fields[3] = words[0];
    record.fields[4] = words[1];
    record.fields[5] = fnv1a_words(words);
    record.fields[6] = expected_sector_fnv();
    record.fields[7] = unsafe { psx_io::read_u32(psx_io::dma::Channel::Cd.register_base()) };
    record.fields[8] = drive_state();
    if record.fields[9] == 0 {
        record.fields[9] = extra;
    }
    record
}

fn drive_state() -> u32 {
    let hw_status = unsafe { psx_io::read_u8(0x1F80_1800) };
    let irq_flag = unsafe {
        psx_io::write_u8(0x1F80_1800, 1);
        let f = psx_io::read_u8(0x1F80_1803) & 0x1F;
        psx_io::write_u8(0x1F80_1800, 0);
        f
    };
    let stat = psx_io::cd::try_status(CD_SPINS)
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

/// Run all eight variants and return one record each.
pub(crate) fn run_matrix() -> [TimingRecord; VARIANT_COUNT] {
    core::array::from_fn(|n| {
        let variant = Variant::ALL[n];
        let rec = run_variant(variant, 1);
        print_record(variant, &rec);
        let ok = rec.fields[0] & 0x7;
        let matched = (rec.fields[5] == rec.fields[6]) as u32;
        let busy_seen = (rec.fields[9] >> 31) & 1;
        record(
            CL2_RECORD + n as u16,
            ok | (matched << 3) | (busy_seen << 4),
            rec.fields[9] & 0xFFFF,
            rec.fields[8] >> 8,
        )
    })
}
