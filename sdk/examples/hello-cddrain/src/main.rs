//! `hello-cddrain` -- read a known file with the polled reader while the CD
//! interrupt pops the sectors (`psx_cdstream::drain`), and check every byte.
//!
//! The disc carries `CDTEST.BIN`, the deterministic benchmark file
//! `mkisopsx --cdtest-sectors` writes (`make hello-cddrain-disc`). Each check
//! starts a read the way a polled-read program does (`prepare`, `start_read`),
//! hands the sectors to the interrupt with `drain::start`, spins on other work
//! as a frame loop would, and ends the read with the reader's own `stop`:
//!
//! 1. one read of 12 sectors, with the foreground busy;
//! 2. a read whose last sector is only partly stored, and the bytes past it intact;
//! 3. thirty reads back to back at spread offsets (a dropped sector shows as a wrong byte);
//! 4. a read cancelled part-way, then a good read to prove recovery;
//! 5. a drain with no read behind it, which must end with the watchdog, then a good read.
//!
//! Results are collected silently and printed at the end, to the TTY and the screen.

#![no_std]
#![no_main]

extern crate psx_rt;

use core::ptr::addr_of_mut;
use psx_cdstream::drain::{self, DrainFailure, DrainState};
use psx_fmv::iso;
use psx_font::fonts::BASIC;
use psx_font::FontAtlas;
use psx_gpu::display::{DisplayConfig, DoubleBuffer, Resolution, VideoMode};
use psx_gpu::Gpu;
use psx_io::cd::reader::SectorReader;
use psx_rt::interrupts::{vblank_count, wait_vblank};
use psx_vram::{Clut, TextureDepth, TexturePage};

const FONT_TPAGE: TexturePage = TexturePage::new(320, 0, TextureDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);
const GREEN: (u8, u8, u8) = (80, 220, 100);
const RED: (u8, u8, u8) = (230, 80, 80);

/// The file the disc is built with (`--cdtest-sectors`), found by name.
const FILE_NAME: &str = "CDTEST.BIN";
/// VBlanks to wait for any one read before giving the check up.
const PATIENCE_VBLANKS: u32 = 600;
/// VBlanks without a sector before the drain gives up.
const TIMEOUT_VBLANKS: u32 = 120;

const SECTOR_WORDS: usize = 512;
/// One long buffer, with a guard sector after it.
const BIG_SECTORS: usize = 64;
static mut BIG: [u32; (BIG_SECTORS + 1) * SECTOR_WORDS] = [0; (BIG_SECTORS + 1) * SECTOR_WORDS];
const GUARD: u32 = 0xa5a5_5a5a;

/// File position, set once the file has been found.
static mut FILE_LBA: u32 = 0;
static mut FILE_SECTORS: u32 = 0;

fn file_lba() -> u32 {
    // SAFETY: written once in `main` before any check runs.
    unsafe { FILE_LBA }
}

fn file_sectors() -> u32 {
    // SAFETY: as `file_lba`.
    unsafe { FILE_SECTORS }
}

// ------------------------------------------------------------------ pattern

/// Byte `index` of the file, as `psx_iso::cd_stream_bench_expected_byte`
/// defines it (a mirror, so the guest needs no host crate).
fn expected_byte(index: u32, sectors: u32) -> u8 {
    const MAGIC: &[u8; 8] = b"PSOXSTRM";
    if index < 8 {
        MAGIC[index as usize]
    } else if index < 12 {
        sectors.to_le_bytes()[(index - 8) as usize]
    } else {
        index
            .wrapping_mul(37)
            .wrapping_add(index >> 3)
            .wrapping_add(0x5D) as u8
    }
}

fn expected_word(word: u32, sectors: u32) -> u32 {
    let at = word * 4;
    u32::from(expected_byte(at, sectors))
        | u32::from(expected_byte(at + 1, sectors)) << 8
        | u32::from(expected_byte(at + 2, sectors)) << 16
        | u32::from(expected_byte(at + 3, sectors)) << 24
}

/// Do the `sectors` sectors at `buffer`, which hold file sectors
/// `file_sector..`, carry the pattern?
fn verify(buffer: *const u32, file_sector: u32, sectors: u32) -> bool {
    let total = file_sectors();
    let first_word = file_sector * SECTOR_WORDS as u32;
    for word in 0..sectors * SECTOR_WORDS as u32 {
        // SAFETY: `word` is inside the `sectors` sectors the caller owns; the
        // request that filled them has finished.
        let got = unsafe { buffer.add(word as usize).read_volatile() };
        if got != expected_word(first_word + word, total) {
            return false;
        }
    }
    true
}

// ------------------------------------------------------------------- report

const REPORT_BYTES: usize = 3072;

/// The text, in RAM where a dump can find it.
#[no_mangle]
static mut CDSTREAM_REPORT: [u8; REPORT_BYTES] = [0; REPORT_BYTES];

/// `from..to` of the report buffer, as text.
fn report_text(from: usize, to: usize) -> &'static str {
    // SAFETY: only `Report` writes the buffer, from `main`'s one thread, and
    // never inside a range it has handed out (it only appends).
    let bytes = unsafe {
        core::slice::from_raw_parts(
            addr_of_mut!(CDSTREAM_REPORT).cast::<u8>().add(from),
            to - from,
        )
    };
    core::str::from_utf8(bytes).unwrap_or("?")
}

struct Report {
    len: usize,
    lines: [(u16, u16, bool); 16],
    line_count: usize,
    line_start: usize,
    ok: bool,
}

impl Report {
    const fn new() -> Self {
        Report {
            len: 0,
            lines: [(0, 0, true); 16],
            line_count: 0,
            line_start: 0,
            ok: true,
        }
    }

    fn text(&self) -> &'static str {
        report_text(0, self.len)
    }

    fn push(&mut self, s: &str) {
        for &b in s.as_bytes() {
            if self.len < REPORT_BYTES {
                // SAFETY: as `text`; `len` is in range.
                unsafe { (*addr_of_mut!(CDSTREAM_REPORT))[self.len] = b };
                self.len += 1;
            }
        }
    }

    fn num(&mut self, mut value: u32) {
        let mut digits = [0u8; 10];
        let mut at = digits.len();
        loop {
            at -= 1;
            digits[at] = b'0' + (value % 10) as u8;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        self.push(core::str::from_utf8(&digits[at..]).unwrap_or("?"));
    }

    fn hex(&mut self, value: u32) {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut text = [0u8; 8];
        for (i, slot) in text.iter_mut().enumerate() {
            *slot = DIGITS[((value >> ((7 - i) * 4)) & 15) as usize];
        }
        self.push(core::str::from_utf8(&text).unwrap_or("?"));
    }

    /// Begin a result line: "<tag>: ".
    fn begin(&mut self, tag: &str) {
        self.line_start = self.len;
        self.push(tag);
        self.push(": ");
    }

    /// End the line with PASS or FAIL (a failure also shows where the
    /// transport stood).
    fn finish(&mut self, ok: bool) {
        self.push(if ok { " PASS\n" } else { " FAIL\n" });
        if self.line_count < self.lines.len() {
            self.lines[self.line_count] = (self.line_start as u16, self.len as u16 - 1, ok);
            self.line_count += 1;
        }
        self.ok &= ok;
    }

    fn note(&mut self, text: &str) {
        self.push(text);
        self.push("\n");
    }
}

// ----------------------------------------------------------------- helpers

fn big() -> *mut u32 {
    addr_of_mut!(BIG).cast::<u32>()
}

/// Fill the buffer with the guard pattern.
fn clear_big() {
    for word in 0..(BIG_SECTORS + 1) * SECTOR_WORDS {
        // SAFETY: inside BIG.
        unsafe { big().add(word).write_volatile(GUARD) };
    }
}

/// Foreground work the drain must not disturb: a counter that stands in for a frame.
fn busy_until_settled(rounds: &mut u32) -> DrainState {
    let start = vblank_count();
    loop {
        let state = drain::poll();
        if !matches!(state, DrainState::Running { .. }) {
            return state;
        }
        if vblank_count().wrapping_sub(start) > PATIENCE_VBLANKS {
            return state;
        }
        *rounds += 1;
        for _ in 0..200 {
            core::hint::black_box(*rounds);
        }
    }
}

/// One read the way a polled-read program does it. Returns the final state and
/// leaves the drive stopped. `byte_len` 0 stores whole sectors.
fn read(
    reader: &mut SectorReader,
    file_sector: u32,
    sectors: u32,
    byte_len: u32,
    rounds: &mut u32,
) -> Option<DrainState> {
    clear_big();
    if !reader.prepare() || !reader.start_read(file_lba() + file_sector) {
        reader.stop();
        return None;
    }
    // SAFETY: BIG holds BIG_SECTORS sectors and the guard; nothing else touches it
    // or the controller until the state settles and `stop` has run.
    let started = unsafe { drain::start(big(), sectors, byte_len, TIMEOUT_VBLANKS) };
    if !started {
        reader.stop();
        return None;
    }
    let state = busy_until_settled(rounds);
    drain::cancel();
    reader.stop();
    Some(state)
}

// ------------------------------------------------------------------ checks

fn check_single(report: &mut Report, reader: &mut SectorReader) {
    report.begin("1 one read");
    let mut rounds = 0;
    let state = read(reader, 0, 12, 0, &mut rounds);
    let ok = state == Some(DrainState::Done) && verify(big(), 0, 12) && rounds > 0;
    report.push("12 sectors, foreground ran ");
    report.num(rounds);
    report.push(" rounds");
    report.finish(ok);
}

fn check_partial(report: &mut Report, reader: &mut SectorReader) {
    report.begin("2 partial last sector");
    let mut rounds = 0;
    let bytes = 37 * 2048 + 100;
    let state = read(reader, 40, 38, bytes, &mut rounds);
    let mut guard_ok = true;
    // The last sector stores 25 words; everything after them is untouched.
    for word in (37 * SECTOR_WORDS + 25)..(BIG_SECTORS + 1) * SECTOR_WORDS {
        // SAFETY: inside BIG.
        if unsafe { big().add(word).read_volatile() } != GUARD {
            guard_ok = false;
            break;
        }
    }
    let first_ok = verify(big(), 40, 37);
    let tail_ok = (0..25).all(|word| {
        // SAFETY: inside BIG.
        let got = unsafe { big().add(37 * SECTOR_WORDS + word).read_volatile() };
        got == expected_word((40 + 37) * SECTOR_WORDS as u32 + word as u32, file_sectors())
    });
    report.push(if guard_ok { "guard intact" } else { "GUARD OVERWRITTEN" });
    report.finish(state == Some(DrainState::Done) && first_ok && tail_ok && guard_ok);
}

fn check_repeat(report: &mut Report, reader: &mut SectorReader) {
    report.begin("3 thirty reads back to back");
    let mut rounds = 0;
    let mut good = 0;
    for i in 0..30u32 {
        let at = (i * 29) % 400;
        let state = read(reader, at, 8, 0, &mut rounds);
        if state == Some(DrainState::Done) && verify(big(), at, 8) {
            good += 1;
        }
    }
    report.num(good);
    report.push("/30 intact");
    report.finish(good == 30);
}

fn check_cancel(report: &mut Report, reader: &mut SectorReader) {
    report.begin("4 cancel part-way");
    clear_big();
    let mut stopped_at = 0;
    if reader.prepare() && reader.start_read(file_lba() + 100) {
        // SAFETY: as `read`.
        let started = unsafe { drain::start(big(), 64, 0, TIMEOUT_VBLANKS) };
        let start = vblank_count();
        while started && vblank_count().wrapping_sub(start) < PATIENCE_VBLANKS {
            if let DrainState::Running { received } = drain::poll() {
                if received >= 8 {
                    stopped_at = received;
                    break;
                }
            }
        }
        drain::cancel();
    }
    reader.stop();
    report.push("cancelled at ");
    report.num(stopped_at);
    report.push("/64, ");
    let mut rounds = 0;
    let again = read(reader, 200, 8, 0, &mut rounds);
    let recovered = again == Some(DrainState::Done) && verify(big(), 200, 8);
    report.push("next read ");
    report.push(if recovered { "ok" } else { "BAD" });
    report.finish(stopped_at >= 8 && recovered);
}

fn check_watchdog(report: &mut Report, reader: &mut SectorReader) {
    report.begin("5 watchdog");
    clear_big();
    // No read behind it: nothing ever arrives.
    // SAFETY: as `read`.
    let started = unsafe { drain::start(big(), 4, 0, 10) };
    let mut rounds = 0;
    let state = busy_until_settled(&mut rounds);
    drain::cancel();
    let watchdog = matches!(state, DrainState::Failed(DrainFailure::Watchdog));
    report.push(if watchdog { "ended by the watchdog, " } else { "NOT ended, " });
    let mut again_rounds = 0;
    let again = read(reader, 300, 8, 0, &mut again_rounds);
    let recovered = again == Some(DrainState::Done) && verify(big(), 300, 8);
    report.push("next read ");
    report.push(if recovered { "ok" } else { "BAD" });
    report.finish(started && watchdog && recovered);
}

// --------------------------------------------------------------------- main

fn read_sector<'s>(
    reader: &mut SectorReader,
    sector: &'s mut [u32; SECTOR_WORDS],
    lba: u32,
) -> Option<&'s [u8]> {
    if !reader.start_read(lba) {
        return None;
    }
    let ok = reader.read_sector(sector);
    reader.stop();
    // SAFETY: a `[u32; N]` viewed as its own bytes; alignment only shrinks.
    ok.then(|| unsafe { core::slice::from_raw_parts(sector.as_ptr().cast::<u8>(), 2048) })
}

fn lookup(reader: &mut SectorReader, sector: &mut [u32; SECTOR_WORDS]) -> Option<(u32, u32)> {
    let (root, _) = iso::root_directory(read_sector(reader, sector, iso::PVD_LBA)?)?;
    iso::find_in_directory(read_sector(reader, sector, root)?, FILE_NAME)
}

#[no_mangle]
fn main() {
    let Some(peripherals) = psx_rt::Peripherals::take() else {
        return;
    };
    let mut gpu = Gpu::new(
        peripherals.gpu_dma,
        DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240),
    );
    let mut fb = DoubleBuffer::new(Resolution::R320X240);
    gpu.set_draw_area((0, 0), (319, 239));
    gpu.set_draw_offset((0, 0));
    let font = FontAtlas::upload(&BASIC, FONT_TPAGE, FONT_CLUT);
    wait_vblank();

    let mut report = Report::new();
    let mut reader = SectorReader::with_cd(peripherals.cd);
    let mut sector = [0u32; SECTOR_WORDS];
    let found = if reader.prepare() {
        lookup(&mut reader, &mut sector)
    } else {
        None
    };
    match found {
        None => {
            report.begin("0 start");
            report.push("CDTEST.BIN not found");
            report.finish(false);
        }
        Some((lba, bytes)) => {
            // SAFETY: written once, before any check runs or the handler is installed.
            unsafe {
                FILE_LBA = lba;
                FILE_SECTORS = bytes / 2048;
            }
            report.note("psx-cdstream drain on the emulator");
            report.begin("0 file");
            report.push("CDTEST.BIN at lba ");
            report.num(file_lba());
            report.push(", ");
            report.num(file_sectors());
            report.push(" sectors");
            report.finish(file_sectors() >= 640);
            check_single(&mut report, &mut reader);
            check_partial(&mut report, &mut reader);
            check_repeat(&mut report, &mut reader);
            check_cancel(&mut report, &mut reader);
            check_watchdog(&mut report, &mut reader);
            report.begin("6 handler stack");
            let unused = drain::stack_unused_bytes();
            report.num(unused as u32);
            report.push(" of ");
            report.num(drain::DRAIN_STACK_BYTES as u32);
            report.push(" bytes unused");
            report.finish(unused >= 128);
        }
    }
    psx_rt::tty::print(report.text());
    psx_rt::tty::println(if report.ok {
        "hello-cddrain: ALL PASS"
    } else {
        "hello-cddrain: FAIL"
    });

    loop {
        if report.ok {
            fb.clear(&mut gpu, (16, 96, 32));
        } else {
            fb.clear(&mut gpu, (110, 20, 20));
        }
        font.draw_text(8, 6, "PSX-CDSTREAM DRAIN TEST", (230, 230, 240));
        let mut y: i16 = 22;
        for i in 0..report.line_count {
            let (from, to, ok) = report.lines[i];
            let text = report_text(from as usize, to as usize);
            let short = text.split(':').next().unwrap_or("?");
            font.draw_text(
                8,
                y,
                if ok { "OK" } else { "XX" },
                if ok { GREEN } else { RED },
            );
            font.draw_text(32, y, short, if ok { GREEN } else { RED });
            y += 12;
        }
        font.draw_text(
            8,
            220,
            if report.ok { "ALL PASS" } else { "FAIL" },
            (235, 255, 235),
        );
        gpu.wait_idle();
        wait_vblank();
        fb.swap(&mut gpu);
    }
}
