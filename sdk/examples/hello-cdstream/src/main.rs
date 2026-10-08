//! `hello-cdstream` -- stream a known file off the disc through
//! `psx-cdstream` and check every byte.
//!
//! The disc carries `CDTEST.BIN`, the deterministic benchmark file
//! `mkisopsx --cdtest-sectors` writes (`make hello-cdstream-disc`). The
//! program finds it by name with the polled reader, hands the CD controller
//! to the interrupt-driven transport, and then runs these checks, each of
//! which verifies the bytes that landed against the pattern:
//!
//! 1. one request;
//! 2. two contiguous requests (the second must chain, with no seek);
//! 3. priority: an urgent request queued behind a background one runs first;
//! 4. abort mid-transfer, then resume from `lba + received`;
//! 5. a sustained stream: 20 contiguous requests through four buffers, the
//!    queue kept topped up from the foreground, with throughput;
//! 6. the CPU the foreground loses while a long read runs;
//! 7. an audio lease taken in the middle of a read, the controller token
//!    used for a polled command, and the read resumed after the release;
//! 8. a read the disc cannot satisfy, then a good read to prove recovery.
//!
//! Results are collected silently (TTY output goes through the BIOS, which
//! the emulator's HLE leaves with `I_MASK` rewritten, so nothing prints
//! while the transport runs) and printed at the end, to the TTY and to the
//! screen. A copy of the text is left at `CDSTREAM_REPORT` for a RAM dump.

#![no_std]
#![no_main]

extern crate psx_rt;

use core::hint::black_box;
use core::ptr::addr_of_mut;
use psx_cdstream::{
    Completion, Config, FailureKind, LeaseState, Outcome, Priority, Request, RequestState, Ticket,
};
use psx_fmv::iso;
use psx_font::{fonts::BASIC, FontAtlas};
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
/// NTSC VBlanks per second.
const VBLANK_HZ: u32 = 60;

const SECTOR_WORDS: usize = 512;
/// One long buffer for the single-request checks.
const BIG_SECTORS: usize = 256;
static mut BIG: [u32; BIG_SECTORS * SECTOR_WORDS] = [0; BIG_SECTORS * SECTOR_WORDS];
/// Four ring buffers for the sustained stream.
const RING_SECTORS: usize = 32;
static mut RING: [[u32; RING_SECTORS * SECTOR_WORDS]; 4] = [[0; RING_SECTORS * SECTOR_WORDS]; 4];

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
        if !ok {
            let stats = psx_cdstream::stats();
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

fn submit(lba: u32, sectors: u32, destination: *mut u32, priority: Priority) -> Option<Ticket> {
    // SAFETY: every destination is a static buffer of at least `sectors`
    // sectors that nothing touches until the request has finished (each check
    // waits for it before it reads the buffer).
    let request = unsafe { Request::new_raw(lba, sectors, destination) };
    psx_cdstream::submit(request.with_priority(priority)).ok()
}

fn submit_file(file_sector: u32, sectors: u32, destination: *mut u32) -> Option<Ticket> {
    submit(
        file_lba() + file_sector,
        sectors,
        destination,
        Priority::NORMAL,
    )
}

fn big() -> *mut u32 {
    addr_of_mut!(BIG).cast::<u32>()
}

fn ring(slot: usize) -> *mut u32 {
    // SAFETY: `slot` < 4; the address is formed without a reference.
    unsafe {
        addr_of_mut!(RING)
            .cast::<[u32; RING_SECTORS * SECTOR_WORDS]>()
            .add(slot)
            .cast()
    }
}

/// Wait for a request to finish, spinning on `state`.
fn wait(ticket: Ticket) -> Option<Completion> {
    let start = vblank_count();
    loop {
        if let RequestState::Finished(done) = psx_cdstream::state(ticket) {
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

/// Wait until the drive is stopped and nothing is queued.
fn wait_idle() {
    let start = vblank_count();
    while !(psx_cdstream::is_idle() && psx_cdstream::queued_count() == 0) {
        if vblank_count().wrapping_sub(start) > PATIENCE_VBLANKS {
            return;
        }
    }
}

// ------------------------------------------------------------------ checks

fn check_single(report: &mut Report) {
    report.begin("1 single request");
    let ticket = submit_file(0, 8, big());
    let ok = wait_done(ticket, 8) && verify(big(), 0, 8);
    report.push("8 sectors");
    report.finish(ok);
}

fn check_chain(report: &mut Report) {
    report.begin("2 chained requests");
    let before = psx_cdstream::stats();
    let a = submit_file(8, 8, big());
    // SAFETY: the second half of BIG, 8 sectors in.
    let second = unsafe { big().add(8 * SECTOR_WORDS) };
    let b = submit_file(16, 8, second);
    let done = wait_done(a, 8) & wait_done(b, 8);
    wait_idle();
    let after = psx_cdstream::stats();
    let chained = after.chained.wrapping_sub(before.chained);
    let ok = done && chained == 1 && verify(big(), 8, 16);
    report.push("chained=");
    report.num(chained);
    report.finish(ok);
}

fn check_priority(report: &mut Report) {
    report.begin("3 priority order");
    let busy = submit_file(100, 48, big());
    let low = submit(file_lba() + 600, 2, ring(0), Priority::BACKGROUND);
    let urgent = submit(file_lba() + 400, 2, ring(1), Priority::URGENT);
    let (Some(busy), Some(low), Some(urgent)) = (busy, low, urgent) else {
        report.push("submit refused");
        report.finish(false);
        return;
    };
    // Poll all three; note the order in which the two queued ones finish.
    let mut order = [0u8; 2];
    let mut seen = 0;
    let start = vblank_count();
    while seen < 2 && vblank_count().wrapping_sub(start) <= PATIENCE_VBLANKS {
        for (id, ticket) in [(1u8, urgent), (2u8, low)] {
            if order[..seen].contains(&id) {
                continue;
            }
            if matches!(psx_cdstream::state(ticket), RequestState::Finished(_)) && seen < 2 {
                order[seen] = id;
                seen += 1;
            }
        }
    }
    let busy_ok = wait_done(Some(busy), 48) && verify(big(), 100, 48);
    let ok = order == [1, 2] && busy_ok && verify(ring(1), 400, 2) && verify(ring(0), 600, 2);
    report.push("finish order urgent,background=");
    report.push(if order == [1, 2] { "yes" } else { "no" });
    report.finish(ok);
}

fn check_abort_resume(report: &mut Report) {
    report.begin("4 abort and resume");
    let before = psx_cdstream::stats();
    // SAFETY: BIG holds 256 sectors; this request is 128.
    let original = unsafe { Request::new_raw(file_lba() + 200, 128, big()) };
    let Ok(ticket) = psx_cdstream::submit(original) else {
        report.push("submit refused");
        report.finish(false);
        return;
    };
    let start = vblank_count();
    loop {
        if let RequestState::Active { received } = psx_cdstream::state(ticket) {
            if received >= 10 {
                break;
            }
        }
        if vblank_count().wrapping_sub(start) > PATIENCE_VBLANKS {
            report.push("never reached 10 sectors");
            report.finish(false);
            return;
        }
    }
    psx_cdstream::cancel(ticket);
    let Some(done) = wait(ticket) else {
        report.push("cancel never finished");
        report.finish(false);
        return;
    };
    let after_cancel = psx_cdstream::stats();
    report.push("stopped at ");
    report.num(done.received);
    report.push("/128, discarded ");
    report.num(
        after_cancel
            .discarded_sectors
            .wrapping_sub(before.discarded_sectors),
    );
    report.push(", ");
    let cancelled =
        done.outcome == Outcome::Cancelled && done.received >= 10 && done.received < 128;
    let prefix = verify(big(), 200, done.received.min(128));
    // SAFETY: `received` is what the transport reported for this request.
    let rest = unsafe { original.remaining_after(done.received) };
    let resumed = rest
        .and_then(|rest| psx_cdstream::submit(rest).ok())
        .map(|t| wait_done(Some(t), 128 - done.received))
        .unwrap_or(false);
    let whole = verify(big(), 200, 128);
    report.push("resumed whole=");
    report.push(if whole { "intact" } else { "CORRUPT" });
    report.finish(cancelled && prefix && resumed && whole);
}

/// Streaming throughput and the queue kept topped up from the foreground.
fn check_stream(report: &mut Report) {
    report.begin("5 sustained stream");
    const TOTAL: u32 = 640;
    const PER: u32 = RING_SECTORS as u32;
    let before = psx_cdstream::stats();
    let mut tickets: [Option<Ticket>; 4] = [None; 4];
    let mut next = 0u32;
    let start = vblank_count();
    for slot in 0..4 {
        tickets[slot] = submit_file(next, PER, ring(slot));
        next += PER;
    }
    let mut verified = 0u32;
    let mut all_ok = true;
    let mut slot = 0;
    while verified < TOTAL {
        let Some(done) = tickets[slot].and_then(wait) else {
            all_ok = false;
            break;
        };
        let first = verified;
        all_ok &= done.outcome == Outcome::Done && verify(ring(slot), first, PER);
        verified += PER;
        if next < TOTAL {
            tickets[slot] = submit_file(next, PER, ring(slot));
            next += PER;
        } else {
            tickets[slot] = None;
        }
        slot = (slot + 1) % 4;
    }
    let vblanks = vblank_count().wrapping_sub(start);
    wait_idle();
    let after = psx_cdstream::stats();
    let chained = after.chained.wrapping_sub(before.chained);
    report.num(TOTAL);
    report.push(" sectors in ");
    report.num(vblanks);
    report.push(" vblanks = ");
    let tenths = TOTAL * VBLANK_HZ * 10 / vblanks.max(1);
    report.num(tenths / 10);
    report.push(".");
    report.num(tenths % 10);
    report.push(" sectors/s, chained=");
    report.num(chained);
    report.push(", lost=");
    report.num(
        after
            .discarded_sectors
            .wrapping_sub(before.discarded_sectors),
    );
    report.finish(all_ok && chained >= 15);
}

/// Spin for `vblanks` VBlanks; the iterations done, in units of 64.
fn spin_idle(vblanks: u32) -> u32 {
    wait_vblank();
    let end = vblank_count() + vblanks;
    let mut iterations = 0u32;
    let mut acc = 0u32;
    while vblank_count() < end {
        for k in 0..64u32 {
            acc = black_box(acc.wrapping_add(k));
        }
        iterations += 1;
        if iterations % 32 == 0 {
            psx_cdstream::service();
        }
    }
    black_box(acc);
    iterations
}

fn check_cpu(report: &mut Report) {
    report.begin("6 foreground cost");
    wait_idle();
    let idle_vblanks = 40;
    let idle = spin_idle(idle_vblanks);
    wait_vblank();
    let sectors = 256;
    let start = vblank_count();
    let ticket = submit_file(0, sectors, big());
    let mut iterations = 0u32;
    let mut acc = 0u32;
    let mut finished = None;
    if let Some(t) = ticket {
        loop {
            for k in 0..64u32 {
                acc = black_box(acc.wrapping_add(k));
            }
            iterations += 1;
            if iterations % 32 == 0 {
                if let RequestState::Finished(done) = psx_cdstream::state(t) {
                    finished = Some(done);
                    break;
                }
                if vblank_count().wrapping_sub(start) > PATIENCE_VBLANKS {
                    break;
                }
            }
        }
    }
    black_box(acc);
    let vblanks = vblank_count().wrapping_sub(start).max(1);
    let stats = psx_cdstream::stats();
    let idle_rate = idle / idle_vblanks;
    let busy_rate = iterations / vblanks;
    let lost_permille = 1000u32.saturating_sub(busy_rate * 1000 / idle_rate.max(1));
    report.num(sectors);
    report.push(" sectors read in ");
    report.num(vblanks);
    report.push(" vblanks; spin loop ran at ");
    report.num(busy_rate * 1000 / idle_rate.max(1) / 10);
    report.push("% of idle (");
    report.num(lost_permille / 10);
    report.push(".");
    report.num(lost_permille % 10);
    report.push("% lost); longest handler ");
    report.num(stats.max_irq_ticks * 8);
    report.push(" cycles, ");
    report.num(psx_cdstream::handler_stack_unused_bytes() as u32);
    report.push(" stack bytes unused");
    let ok = matches!(finished, Some(done) if done.outcome == Outcome::Done)
        && verify(big(), 0, sectors);
    report.finish(ok);
}

fn check_lease(report: &mut Report) {
    report.begin("7 audio lease");
    wait_idle();
    // SAFETY: BIG holds 256 sectors; this request is 200.
    let original = unsafe { Request::new_raw(file_lba() + 300, 200, big()) };
    let Ok(ticket) = psx_cdstream::submit(original) else {
        report.push("submit refused");
        report.finish(false);
        return;
    };
    let start = vblank_count();
    loop {
        if let RequestState::Active { received } = psx_cdstream::state(ticket) {
            if received >= 20 {
                break;
            }
        }
        if vblank_count().wrapping_sub(start) > PATIENCE_VBLANKS {
            report.push("never reached 20 sectors");
            report.finish(false);
            return;
        }
    }
    let first = psx_cdstream::request_audio_lease();
    let mut spins = 0;
    while psx_cdstream::lease_state() != LeaseState::Granted && spins < 4_000_000 {
        spins += 1;
    }
    let granted = psx_cdstream::lease_state() == LeaseState::Granted;
    let done = wait(ticket);
    let received = done.map_or(0, |d| d.received);
    let cancelled =
        matches!(done, Some(d) if d.outcome == Outcome::Cancelled) && (20..200).contains(&received);
    report.push(if first == LeaseState::Pending {
        "pending then "
    } else {
        "granted at once, "
    });
    report.push(if granted {
        "granted, "
    } else {
        "NOT granted, "
    });
    report.push("read stopped at ");
    report.num(received);
    report.push("/200, ");
    // The audio code owns the controller now: prove it by a polled command.
    let mut status_ok = false;
    let mut returned = false;
    let mut resumed = false;
    if let Some(mut cd) = psx_cdstream::take_audio_lease() {
        status_ok = use_controller(&mut cd);
        returned = psx_cdstream::release_audio_lease(cd).is_ok();
        // SAFETY: `received` is what the transport reported for this request.
        resumed = unsafe { original.remaining_after(received) }
            .and_then(|rest| psx_cdstream::submit(rest).ok())
            .map(|t| wait_done(Some(t), 200 - received))
            .unwrap_or(false);
    }
    report.push(if status_ok {
        "polled Getstat ok, "
    } else {
        "polled Getstat FAILED, "
    });
    let whole = verify(big(), 300, 200);
    report.push("after release the rest ");
    report.push(if resumed && whole { "intact" } else { "WRONG" });
    report.finish(granted && cancelled && status_ok && returned && resumed && whole);
}

/// A polled command on the leased controller.
fn use_controller(cd: &mut Cd) -> bool {
    cd.status().is_ok()
}

fn check_error(report: &mut Report) {
    report.begin("8 unreadable sector");
    wait_idle();
    // A sector far past the end of the disc image.
    let ticket = submit(file_lba() + 3_000_000, 2, big(), Priority::NORMAL);
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
    let again = submit_file(0, 4, big());
    let recovered = wait_done(again, 4) && verify(big(), 0, 4);
    report.push("next read ");
    report.push(if recovered { "ok" } else { "BAD" });
    report.finish(recovered);
}

// --------------------------------------------------------------------- main

/// Find the test file by name with the polled reader, then hand the
/// controller to the transport.
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
    let config = Config {
        timeout_vblanks: 240,
        time_handler: true,
        ..Config::DEFAULT
    };
    wait_vblank();
    psx_cdstream::install(cd, config).map_err(|_| "already installed")
}

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

    let mut report = Report::new();
    match start(peripherals.cd) {
        Err(why) => {
            report.begin("0 start");
            report.push(why);
            report.finish(false);
        }
        Ok(()) => {
            report.note("psx-cdstream on the emulator");
            report.begin("0 file");
            report.push("CDTEST.BIN at lba ");
            report.num(file_lba());
            report.push(", ");
            report.num(file_sectors());
            report.push(" sectors");
            report.finish(file_sectors() >= 640);
            check_single(&mut report);
            check_chain(&mut report);
            check_priority(&mut report);
            check_abort_resume(&mut report);
            check_stream(&mut report);
            check_cpu(&mut report);
            check_lease(&mut report);
            check_error(&mut report);
            let stats = psx_cdstream::stats();
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
            report.push(" chained=");
            report.num(stats.chained);
            report.push(" stray=");
            report.num(psx_rt::interrupts::stray_interrupt_count());
            report.push("\n");
        }
    }
    // Quiet the transport before the BIOS writes the TTY.
    psx_cdstream::cancel_all();
    wait_idle();
    psx_rt::tty::print(report.text());
    psx_rt::tty::println(if report.ok {
        "hello-cdstream: ALL PASS"
    } else {
        "hello-cdstream: FAIL"
    });

    loop {
        if report.ok {
            fb.clear(&mut gpu, (16, 96, 32));
        } else {
            fb.clear(&mut gpu, (110, 20, 20));
        }
        font.draw_text(8, 6, "PSX-CDSTREAM TEST", (230, 230, 240));
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
