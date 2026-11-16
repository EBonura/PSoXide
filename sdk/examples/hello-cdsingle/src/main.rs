//! `hello-cdsingle` -- stream a known file off the disc through the single-transfer transport
//! (`psx_cdstream::single`) and check every byte.
//!
//! The disc carries `CDTEST.BIN`, the deterministic benchmark file `mkisopsx --cdtest-sectors`
//! writes (`make hello-cdsingle-disc`). The program finds it by name with the polled reader,
//! hands the controller to the transport, and runs the reads a game that loads one chunk at a
//! time makes, each verified against the pattern:
//!
//! 1. one request, and a second refused while it runs;
//! 2. progress read with the cheap counter (`sectors_landed`) under a main loop that looks once
//!    every six VBlanks (a split-screen frame), with no sector lost;
//! 3. a cancel mid-read, then a good read;
//! 4. a park, the head-moved counter (`submissions`), and the read after it;
//! 5. an exact byte bound on the last sector;
//! 6. the CD source closed between requests (`IrqMaskPolicy::OnlyWhileBusy`) and a polled read
//!    in between, then the transport again;
//! 7. the controller handed to polled code (`uninstall`, as before an XA command) and taken back;
//! 8. a read the disc cannot satisfy, then a good read to prove recovery.
//!
//! Results are collected silently (TTY output goes through the BIOS, which the emulator's HLE
//! leaves with `I_MASK` rewritten) and printed at the end, to the TTY and to the screen.

#![no_std]
#![no_main]

extern crate psx_rt;

use core::ptr::addr_of_mut;
use psx_cdstream::{
    single, Completion, Config, FailureKind, IrqMaskPolicy, Outcome, Request, RequestState,
    SubmitError, Ticket,
};
use psx_font::fonts::BASIC;
use psx_font::FontAtlas;
use psx_gpu::display::{DisplayConfig, DoubleBuffer, Resolution, VideoMode};
use psx_gpu::Gpu;
use psx_io::cd::reader::SectorReader;
use psx_io::periph::Cd;
use psx_rt::interrupts::{vblank_count, wait_vblank};
use psx_vram::{Clut, TextureDepth, TexturePage};

const FONT_TPAGE: TexturePage = TexturePage::new(320, 0, TextureDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);
const GREEN: (u8, u8, u8) = (80, 220, 100);
const RED: (u8, u8, u8) = (230, 80, 80);

/// The file the disc is built with (`--cdtest-sectors`), found by name.
const FILE_NAME: &str = "CDTEST.BIN";
/// VBlanks to wait for any one request before giving the check up.
const PATIENCE_VBLANKS: u32 = 600;
/// The CD source's bit in `I_MASK`.
const CD_BIT: u32 = 1 << psx_hw::irq::source::CDROM;

const SECTOR_WORDS: usize = 512;
/// One long buffer for the checks.
const BIG_SECTORS: usize = 256;
static mut BIG: [u32; BIG_SECTORS * SECTOR_WORDS] = [0; BIG_SECTORS * SECTOR_WORDS];

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
            .wrapping_add(0x5d) as u8
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

const REPORT_BYTES: usize = 3072;

/// The text, in RAM where a dump can find it.
#[no_mangle]
static mut CDSINGLE_REPORT: [u8; REPORT_BYTES] = [0; REPORT_BYTES];

/// `from..to` of the report buffer, as text.
fn report_text(from: usize, to: usize) -> &'static str {
    // SAFETY: only `Report` writes the buffer, from `main`'s one thread, and
    // never inside a range it has handed out (it only appends).
    let bytes = unsafe {
        core::slice::from_raw_parts(
            addr_of_mut!(CDSINGLE_REPORT).cast::<u8>().add(from),
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
                unsafe { (*addr_of_mut!(CDSINGLE_REPORT))[self.len] = b };
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
        if !ok {
            let stats = single::stats();
            self.push(" [phase ");
            self.num(stats.phase);
            self.push(" error ");
            self.hex(stats.error);
            self.push(" stored ");
            self.num(stats.sectors);
            self.push(" dropped ");
            self.num(stats.discarded_sectors);
            self.push("]");
        }
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

fn submit(lba: u32, sectors: u32, destination: *mut u32) -> Result<Ticket, SubmitError> {
    // SAFETY: every destination is a static buffer of at least `sectors` sectors that nothing
    // touches until the request has finished (each check waits for it before it reads the
    // buffer).
    let request = unsafe { Request::new_raw(lba, sectors, destination) };
    single::submit(request)
}

fn submit_file(file_sector: u32, sectors: u32, destination: *mut u32) -> Option<Ticket> {
    submit(file_lba() + file_sector, sectors, destination).ok()
}

fn big() -> *mut u32 {
    addr_of_mut!(BIG).cast::<u32>()
}

/// Wait for a request to finish, spinning on `state`.
fn wait(ticket: Ticket) -> Option<Completion> {
    let start = vblank_count();
    loop {
        if let RequestState::Finished(done) = single::state(ticket) {
            return Some(done);
        }
        if vblank_count().wrapping_sub(start) > PATIENCE_VBLANKS {
            return None;
        }
    }
}

fn wait_done(ticket: Option<Ticket>, sectors: u32) -> bool {
    matches!(
        ticket.and_then(wait),
        Some(Completion {
            outcome: Outcome::Done,
            received,
            ..
        }) if received == sectors
    )
}

/// Wait until the drive is stopped (the request is finished before the drive has paused).
fn wait_idle() {
    let start = vblank_count();
    while !single::is_idle() {
        if vblank_count().wrapping_sub(start) > PATIENCE_VBLANKS {
            return;
        }
    }
}

/// Whether the CD source is open in `I_MASK` right now.
fn source_open() -> bool {
    psx_io::irq::mask() & CD_BIT != 0
}

// ------------------------------------------------------------------ checks

/// One request; a second is refused while it runs.
fn check_single(report: &mut Report) {
    report.begin("1 one at a time");
    let ticket = submit_file(0, 8, big());
    let refused = matches!(
        submit(file_lba() + 100, 1, big()),
        Err(SubmitError::QueueFull)
    );
    let ok = wait_done(ticket, 8) && verify(big(), 0, 8) && refused;
    report.push("8 sectors, second refused: ");
    report.push(if refused { "yes" } else { "no" });
    report.finish(ok);
}

/// A read under a main loop that looks once every six VBlanks (a split-screen frame): the handler
/// loses no sector, and the cheap counter agrees with `state` at every look.
fn check_progress(report: &mut Report) {
    report.begin("2 slow main loop");
    const SECTORS: u32 = 64;
    wait_idle();
    let dropped_before = single::stats().discarded_sectors;
    let mark = psx_cdstream::sectors_landed();
    let ticket = submit_file(16, SECTORS, big());
    let (mut looks, mut last, mut agrees, mut finished) = (0u32, 0u32, true, None);
    if let Some(ticket) = ticket {
        let start = vblank_count();
        while vblank_count().wrapping_sub(start) <= PATIENCE_VBLANKS {
            for _ in 0..6 {
                wait_vblank();
            }
            looks += 1;
            let state = single::state(ticket);
            let landed = psx_cdstream::sectors_landed().wrapping_sub(mark);
            agrees &= landed >= last;
            last = landed;
            match state {
                RequestState::Active { received } => agrees &= landed >= received,
                RequestState::Finished(done) => {
                    finished = Some(done);
                    break;
                }
                _ => agrees = false,
            }
        }
    }
    let landed_all = matches!(
        finished,
        Some(Completion { outcome: Outcome::Done, received, .. }) if received == SECTORS
    );
    let ok = landed_all
        && agrees
        && last == SECTORS
        && verify(big(), 16, SECTORS)
        && single::stats().discarded_sectors == dropped_before;
    report.push("64 sectors, looked ");
    report.num(looks);
    report.push(" times, counter ");
    report.num(last);
    report.finish(ok);
}

/// A cancel mid-read ends the request `Cancelled` with what landed, and the next read is whole.
fn check_cancel(report: &mut Report) {
    report.begin("3 cancel");
    wait_idle();
    let mark = psx_cdstream::sectors_landed();
    let ticket = submit_file(0, 200, big());
    let start = vblank_count();
    while psx_cdstream::sectors_landed().wrapping_sub(mark) < 20
        && vblank_count().wrapping_sub(start) <= PATIENCE_VBLANKS
    {}
    let stopped = ticket.is_some_and(single::cancel);
    let done = ticket.and_then(wait);
    let cancelled = matches!(
        done,
        Some(Completion { outcome: Outcome::Cancelled, received, .. })
            if (20..200).contains(&received) && verify(big(), 0, received)
    );
    wait_idle();
    let again = submit_file(8, 8, big());
    let recovered = wait_done(again, 8) && verify(big(), 8, 8);
    report.push("cancelled after 20+ sectors, next read ");
    report.push(if recovered { "ok" } else { "BAD" });
    report.finish(stopped && cancelled && recovered);
}

/// A park is one short read, `submissions` counts it, and a read after it lands whole.
fn check_park(report: &mut Report) {
    report.begin("4 park then read");
    wait_idle();
    let before = single::submissions();
    let parked = single::submit(Request::park(file_lba() + 100)).ok();
    let parked_ok = matches!(
        parked.and_then(wait),
        Some(Completion {
            outcome: Outcome::Done,
            received: 1,
            ..
        })
    );
    wait_idle();
    let after_park = single::submissions();
    let still_parked = single::submissions() == after_park;
    let ticket = submit_file(100, 8, big());
    let read_ok = wait_done(ticket, 8) && verify(big(), 100, 8);
    let moved = single::submissions() == after_park.wrapping_add(1);
    report.push("park, then 8 sectors");
    report.finish(
        parked_ok && after_park == before.wrapping_add(1) && still_parked && read_ok && moved,
    );
}

/// A byte bound stores nothing past the chunk's last byte.
fn check_byte_bound(report: &mut Report) {
    report.begin("5 byte bound");
    wait_idle();
    const SECTORS: u32 = 3;
    const BYTES: u32 = 2 * 2048 + 100;
    // SAFETY: `big()` is a static buffer far larger than this; nothing else touches it here.
    unsafe {
        for word in 0..(SECTORS as usize * SECTOR_WORDS) {
            big().add(word).write_volatile(0xdead_beef);
        }
    }
    // SAFETY: as in `submit`.
    let request = unsafe { Request::new_raw(file_lba() + 40, SECTORS, big()) }.with_byte_len(BYTES);
    let ticket = single::submit(request).ok();
    let done = wait_done(ticket, SECTORS);
    // The first 2 sectors and 25 words of the third land; the rest stays the canary.
    let first = verify(big(), 40, 2);
    let words_stored = (BYTES as usize).div_ceil(4);
    // SAFETY: inside the buffer written above.
    let (last_stored, first_untouched) = unsafe {
        (
            big().add(words_stored - 1).read_volatile(),
            big().add(words_stored).read_volatile(),
        )
    };
    let ok = done
        && first
        && last_stored
            == expected_word(
                40 * SECTOR_WORDS as u32 + words_stored as u32 - 1,
                file_sectors(),
            )
        && first_untouched == 0xdead_beef;
    report.push("2048*2+100 bytes");
    report.finish(ok);
}

/// With `OnlyWhileBusy` the CD source is open only while a request runs, so a polled read between
/// two requests never meets the handler.
fn check_source_policy(report: &mut Report) {
    report.begin("6 source only while busy");
    wait_idle();
    let closed_idle = !source_open();
    let ticket = submit_file(0, 8, big());
    let open_busy = source_open();
    let read = wait_done(ticket, 8) && verify(big(), 0, 8);
    wait_idle();
    let closed_again = !source_open();
    // A polled read of the directory, as a game's loader does between streams.
    // SAFETY: nothing is queued or active and the source is closed (checked above), so the
    // transport is not programming the controller.
    let polled = unsafe { Cd::steal() };
    let mut reader = SectorReader::with_cd(polled);
    let mut sector = [0u32; SECTOR_WORDS];
    let found = reader.prepare() && lookup(&mut reader, &mut sector).is_some();
    let _ = reader.release();
    let after = submit_file(8, 8, big());
    let again = wait_done(after, 8) && verify(big(), 8, 8);
    report.push("closed idle, open busy, polled read between");
    report.finish(closed_idle && open_busy && read && closed_again && found && again);
}

/// The controller handed to polled code and taken back, as around an XA command.
fn check_handoff(report: &mut Report) {
    report.begin("7 hand-off");
    let stopped = single::stop_and_wait(120);
    let token = single::uninstall();
    let (gave, polled) = match token {
        Some(cd) => {
            let mut reader = SectorReader::with_cd(cd);
            let mut sector = [0u32; SECTOR_WORDS];
            let found = reader.prepare() && lookup(&mut reader, &mut sector).is_some();
            let cd = reader.release();
            let back = single::install(cd, config()).is_ok();
            (back, found)
        }
        None => (false, false),
    };
    let ticket = submit_file(24, 8, big());
    let read = wait_done(ticket, 8) && verify(big(), 24, 8);
    report.push("uninstall, polled read, install, read");
    report.finish(stopped && gave && polled && read);
}

/// A read the disc cannot satisfy ends `Failed`, and the next good read works.
fn check_error(report: &mut Report) {
    report.begin("8 unreadable sector");
    wait_idle();
    // A sector far past the end of the disc image.
    let ticket = submit(file_lba() + 3_000_000, 2, big()).ok();
    let done = ticket.and_then(wait);
    match done {
        Some(Completion {
            outcome: Outcome::Failed(failure),
            received,
            ..
        }) => {
            report.push("failed (");
            report.hex(failure.code());
            report.push(match failure.kind() {
                FailureKind::Drive { .. } => " drive error",
                FailureKind::Watchdog => " watchdog",
                FailureKind::DataNotReady => " data not ready",
                FailureKind::UnexpectedInterrupt { .. } => " unexpected interrupt",
                FailureKind::CommandRefused { .. } => " command refused",
                FailureKind::Unknown => " ?",
            });
            report.push(") after ");
            report.num(received);
            report.push(" sectors, ");
        }
        Some(done) => {
            report.push("ended ");
            report.push(match done.outcome {
                Outcome::Done => "Done",
                Outcome::Cancelled => "Cancelled",
                Outcome::Failed(_) => "Failed",
            });
            report.push(" with ");
            report.num(done.received);
            report.push(" sectors, ");
        }
        None => report.push("never finished, "),
    }
    // Whatever happened, the next good read must work.
    wait_idle();
    let again = submit_file(0, 4, big());
    let recovered = wait_done(again, 4) && verify(big(), 0, 4);
    report.push("next read ");
    report.push(if recovered { "ok" } else { "BAD" });
    report.finish(recovered);
}

/// The configuration the transport runs under here.
fn config() -> Config {
    Config {
        timeout_vblanks: 240,
        irq_mask_policy: IrqMaskPolicy::OnlyWhileBusy,
        time_handler: true,
        ..Config::DEFAULT
    }
}

// --------------------------------------------------------------------- main

/// Find the test file by name with the polled reader, then hand the controller to the transport.
fn start(cd: Cd) -> Result<(), &'static str> {
    let mut reader = SectorReader::with_cd(cd);
    let mut sector = [0u32; SECTOR_WORDS];
    if !reader.prepare() {
        return Err("reader prepare failed");
    }
    let found = lookup(&mut reader, &mut sector);
    let cd = reader.release();
    let Some((lba, bytes)) = found else {
        return Err("CDTEST.BIN not on the disc");
    };
    // SAFETY: written once, before any check runs or the handler is installed.
    unsafe {
        FILE_LBA = lba;
        FILE_SECTORS = bytes / 2048;
    }
    wait_vblank();
    single::install(cd, config()).map_err(|_| "already installed")
}

fn lookup(reader: &mut SectorReader, sector: &mut [u32; SECTOR_WORDS]) -> Option<(u32, u32)> {
    reader.find(sector, None, &[FILE_NAME])
}

#[no_mangle]
fn main() {
    // So the run can report how much of the dispatcher's stack the handler used.
    psx_rt::dispatch::paint_stack();
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

    let mut report = Report::new();
    match start(peripherals.cd) {
        Err(why) => {
            report.begin("0 start");
            report.push(why);
            report.finish(false);
        }
        Ok(()) => {
            report.note("psx-cdstream single transfer on the emulator");
            report.begin("0 file");
            report.push("CDTEST.BIN at lba ");
            report.num(file_lba());
            report.push(", ");
            report.num(file_sectors());
            report.push(" sectors");
            report.finish(file_sectors() >= 640);
            check_single(&mut report);
            check_progress(&mut report);
            check_cancel(&mut report);
            check_park(&mut report);
            check_byte_bound(&mut report);
            check_source_policy(&mut report);
            check_handoff(&mut report);
            check_error(&mut report);
            let stats = single::stats();
            report.note("counters:");
            report.push("irq=");
            report.num(stats.irq_count);
            report.push(" stored=");
            report.num(stats.sectors);
            report.push(" dropped=");
            report.num(stats.discarded_sectors);
            report.push(" done=");
            report.num(stats.requests_done);
            report.push(" cancelled=");
            report.num(stats.requests_cancelled);
            report.push(" failed=");
            report.num(stats.requests_failed);
            report.push(" rejected=");
            report.num(stats.rejected);
            report.push(" longest handler ");
            report.num(stats.max_irq_ticks * 8);
            report.push(" cycles, ");
            report.num(psx_cdstream::handler_stack_unused_bytes() as u32);
            report.push(" stack bytes unused, stray=");
            report.num(psx_rt::interrupts::stray_interrupt_count());
            report.push("\n");
        }
    }
    // Quiet the transport before the BIOS writes the TTY.
    single::cancel_all();
    wait_idle();
    psx_rt::tty::print(report.text());
    psx_rt::tty::println(if report.ok {
        "hello-cdsingle: ALL PASS"
    } else {
        "hello-cdsingle: FAIL"
    });

    loop {
        if report.ok {
            fb.clear(&mut gpu, (16, 96, 32));
        } else {
            fb.clear(&mut gpu, (110, 20, 20));
        }
        font.draw_text(8, 6, "PSX-CDSTREAM SINGLE", (230, 230, 240));
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
