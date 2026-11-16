//! `hello-cdcount` -- `psx_cdstream::counted`: the lean transport plus one counter, the sectors the
//! interrupt handler has stored. The probe for a program that decodes a request chunk by chunk
//! while its sectors are still landing, and still wants one `status()` per frame, no tickets and
//! none of the statistics block.
//!
//! It finds `CDTEST.BIN` with the polled reader (`make hello-cdcount-disc`), installs
//! `psx_cdstream::counted` with the CD source open only while a request runs, then:
//!
//! 1. streams a long chunk and, while the read is in flight, spins on `sectors_landed()`: the
//!    count never goes down, takes more than two distinct values before the end, and every
//!    sector it announces is already in the destination, byte for byte (a loader that decodes
//!    as it goes depends on exactly that); `status` is `Busy` until the count is complete and
//!    the drive has stopped, and the count ends at the request's sector count;
//! 2. a chunk that ends inside a sector (`with_byte_len`): the partly stored last sector counts
//!    once;
//! 3. a park (`Request::park`): one sector counted, `submissions()` up by one, `status` `Done`;
//! 4. a request cancelled while it runs: `status` is `Failed`, `last_error()` is 0 (a cancel,
//!    not a failure), the count stopped short of the request, and the sectors the handler
//!    dropped after the cancel are not counted;
//! 5. a read the disc cannot satisfy: `status` is `Failed`, `last_error()` is a failure code (not
//!    0; which one depends on how the drive model reports it), nothing counted, and the next good
//!    read resets `last_error()` to 0;
//! 6. the polled reader between requests, as the loader does.
//!
//! Every check runs against the transport's own CD interrupt on whatever hardware model the
//! emulator is given. The verdict goes to the TTY only.

#![no_std]
#![no_main]

extern crate psx_rt;

use core::ptr::addr_of_mut;
use psx_cdstream::{counted, Config, IrqMaskPolicy, Request, Status};
use psx_io::cd::reader::SectorReader;
use psx_rt::interrupts::{vblank_count, wait_vblank};

/// The file the disc is built with (`--cdtest-sectors`), found by name.
const FILE_NAME: &str = "CDTEST.BIN";
/// VBlanks to wait for a request before giving up.
const PATIENCE_VBLANKS: u32 = 600;
const SECTOR_WORDS: usize = 512;
/// The long chunk, in sectors: about 0.2 s at double speed, so the count is seen in flight.
const LONG_SECTORS: u32 = 32;
const CANARY: u32 = 0xdead_beef;
/// The byte bound of check 2: three sectors and a bit.
const BOUND_BYTES: u32 = 3 * 2048 + 100;

static mut CHUNK: [u32; LONG_SECTORS as usize * SECTOR_WORDS] =
    [0; LONG_SECTORS as usize * SECTOR_WORDS];

/// Word `word` of the file, as `psx_iso::cd_stream_bench_expected_byte` defines its bytes (a
/// mirror, so the guest needs no host crate).
fn expected_word(word: u32, sectors: u32) -> u32 {
    let byte = |index: u32| -> u32 {
        const MAGIC: &[u8; 8] = b"PSOXSTRM";
        u32::from(if index < 8 {
            MAGIC[index as usize]
        } else if index < 12 {
            sectors.to_le_bytes()[(index - 8) as usize]
        } else {
            index
                .wrapping_mul(37)
                .wrapping_add(index >> 3)
                .wrapping_add(0x5d) as u8
        })
    };
    let at = word * 4;
    byte(at) | byte(at + 1) << 8 | byte(at + 2) << 16 | byte(at + 3) << 24
}

fn chunk() -> *mut u32 {
    addr_of_mut!(CHUNK).cast()
}

fn fill_canary() {
    for word in 0..LONG_SECTORS as usize * SECTOR_WORDS {
        // SAFETY: inside `CHUNK`; nothing is running.
        unsafe { chunk().add(word).write_volatile(CANARY) };
    }
}

/// Whether chunk words `at..at + words` are the file's words from `file_first`.
fn words_are(at: u32, file_first: u32, words: u32, file_sectors: u32) -> bool {
    (0..words).all(|word| {
        // SAFETY: inside `CHUNK`; the sectors counted so far are stored.
        let got = unsafe { chunk().add((at + word) as usize).read_volatile() };
        got == expected_word(file_first + word, file_sectors)
    })
}

/// Whether every word of the chunk from `from_word` on is still the canary.
fn canary_from(from_word: u32) -> bool {
    (from_word..LONG_SECTORS * SECTOR_WORDS as u32).all(|word| {
        // SAFETY: inside `CHUNK`.
        unsafe { chunk().add(word as usize).read_volatile() == CANARY }
    })
}

/// The drive has stopped and the transport says how the latest request ended, giving up after
/// the patience limit.
fn finish() -> Status {
    let start = vblank_count();
    loop {
        match counted::status() {
            Status::Busy if vblank_count().wrapping_sub(start) > PATIENCE_VBLANKS => {
                return Status::Busy
            }
            Status::Busy => wait_vblank(),
            done => return done,
        }
    }
}

struct Report {
    ok: bool,
}

impl Report {
    fn check(&mut self, name: &str, passed: bool) {
        self.ok &= passed;
        psx_rt::tty::print("hello-cdcount: ");
        psx_rt::tty::print(name);
        psx_rt::tty::println(if passed { " ok" } else { " BAD" });
    }
}

/// 1. A long chunk, the count read while it runs.
fn check_in_flight(report: &mut Report, lba: u32, file_sectors: u32) {
    fill_canary();
    let mark = counted::sectors_landed();
    // SAFETY: `CHUNK` holds `LONG_SECTORS` sectors and nothing touches it until the request ends,
    // except reads of the sectors the count announces.
    let request = unsafe { Request::new_raw(lba + 16, LONG_SECTORS, chunk()) };
    let started = counted::submit(request).is_ok();
    let begin = vblank_count();
    let mut last = 0u32;
    let mut distinct = 0u32;
    let mut monotonic = true;
    let mut announced_stored = true;
    let mut busy_while_counting = true;
    let mut checked = 0u32;
    // Spin on the count, as a loader that decodes sector by sector does, and look at `status`
    // once per VBlank, as it does once per frame.
    let mut frame = vblank_count();
    while last < LONG_SECTORS && vblank_count().wrapping_sub(begin) <= PATIENCE_VBLANKS {
        let landed = counted::sectors_landed().wrapping_sub(mark);
        if landed < last || landed > LONG_SECTORS {
            monotonic = false;
            break;
        }
        if landed != last {
            distinct += 1;
            // Every announced sector is complete in the destination.
            announced_stored &= words_are(
                checked * SECTOR_WORDS as u32,
                (16 + checked) * SECTOR_WORDS as u32,
                (landed - checked) * SECTOR_WORDS as u32,
                file_sectors,
            );
            checked = landed;
            last = landed;
        }
        if vblank_count() != frame {
            frame = vblank_count();
            // Busy until the drive has stopped, which is after the last sector.
            busy_while_counting &= counted::status() == Status::Busy || last == LONG_SECTORS;
        }
    }
    let status = finish();
    let end = counted::sectors_landed().wrapping_sub(mark);
    let whole_ok = words_are(
        0,
        16 * SECTOR_WORDS as u32,
        LONG_SECTORS * SECTOR_WORDS as u32,
        file_sectors,
    );
    report.check("1 submit", started);
    report.check("1 count never goes down", monotonic);
    report.check("1 count seen in flight (more than two values)", distinct > 2);
    report.check("1 announced sectors are stored", announced_stored);
    report.check("1 busy while counting", busy_while_counting);
    report.check("1 count ends at the sector count", end == LONG_SECTORS);
    report.check("1 done, bytes whole", status == Status::Done && whole_ok);
    report.check("1 no error", counted::last_error() == 0 && counted::is_idle());
}

/// 2. A byte bound inside a sector: the partly stored last sector counts once.
fn check_bound(report: &mut Report, lba: u32, file_sectors: u32) {
    fill_canary();
    let mark = counted::sectors_landed();
    // SAFETY: as above; the first request has finished and the drive has stopped.
    let request =
        unsafe { Request::new_raw(lba + 16, 4, chunk()) }.with_byte_len(BOUND_BYTES);
    let started = counted::submit(request).is_ok();
    let status = finish();
    let landed = counted::sectors_landed().wrapping_sub(mark);
    let stored_words = BOUND_BYTES.div_ceil(4);
    report.check(
        "2 bound: done, four counted",
        started && status == Status::Done && landed == 4,
    );
    report.check(
        "2 bound: bytes, nothing past the bound",
        words_are(0, 16 * SECTOR_WORDS as u32, stored_words, file_sectors)
            && canary_from(stored_words),
    );
}

/// 3. A park: one sector, one submission.
fn check_park(report: &mut Report, lba: u32) {
    let mark = counted::sectors_landed();
    let before = counted::submissions();
    let started = counted::submit(Request::park(lba + 200)).is_ok();
    let status = finish();
    report.check(
        "3 park: done, one sector",
        started && status == Status::Done && counted::sectors_landed().wrapping_sub(mark) == 1,
    );
    report.check(
        "3 park: one submission",
        counted::submissions().wrapping_sub(before) == 1,
    );
}

/// 4. A cancel in flight.
fn check_cancel(report: &mut Report, lba: u32) {
    fill_canary();
    let mark = counted::sectors_landed();
    let before = counted::submissions();
    // SAFETY: as above; nothing is running.
    let request = unsafe { Request::new_raw(lba + 300, LONG_SECTORS, chunk()) };
    let started = counted::submit(request).is_ok();
    // A second submit while this one runs is refused and counts for nothing.
    // SAFETY: refused, so the pointer is never used.
    let refused = counted::submit(unsafe { Request::new_raw(lba + 100, 1, chunk()) }).is_err();
    let begin = vblank_count();
    while counted::sectors_landed().wrapping_sub(mark) < 2
        && vblank_count().wrapping_sub(begin) <= PATIENCE_VBLANKS
    {}
    counted::cancel();
    let stopped = counted::stop_and_wait(120);
    let landed = counted::sectors_landed().wrapping_sub(mark);
    report.check("4 cancel: stopped", started && refused && stopped);
    report.check(
        "4 cancel: failed, error 0, short of the request",
        counted::status() == Status::Failed
            && counted::last_error() == 0
            && (2..LONG_SECTORS).contains(&landed),
    );
    report.check(
        "4 cancel: counted sectors are all that was stored",
        canary_from(landed * SECTOR_WORDS as u32)
            && counted::submissions().wrapping_sub(before) == 1,
    );
}

/// 5. A read the disc cannot satisfy, then a good one.
fn check_error(report: &mut Report, lba: u32, file_sectors: u32) {
    fill_canary();
    let mark = counted::sectors_landed();
    // SAFETY: as above; nothing is running.
    let request = unsafe { Request::new_raw(lba + 3_000_000, 2, chunk()) };
    let started = counted::submit(request).is_ok();
    let status = finish();
    let error = counted::last_error();
    report.check(
        "5 unreadable: failed, nothing counted",
        started && status == Status::Failed && counted::sectors_landed() == mark,
    );
    psx_rt::tty::print("hello-cdcount: 5 unreadable: last_error ");
    psx_rt::tty::print_hex_u32(error);
    psx_rt::tty::println("");
    report.check("5 unreadable: last_error is a failure code", error != 0);
    // SAFETY: as above; the failed request has ended and the drive has stopped.
    let again = unsafe { Request::new_raw(lba + 16, 4, chunk()) };
    let restarted = counted::submit(again).is_ok();
    let cleared = counted::last_error() == 0;
    let status = finish();
    report.check(
        "5 next read ok, error cleared",
        restarted
            && cleared
            && status == Status::Done
            && counted::sectors_landed().wrapping_sub(mark) == 4
            && words_are(0, 16 * SECTOR_WORDS as u32, 4 * SECTOR_WORDS as u32, file_sectors),
    );
}

fn main_checks(report: &mut Report, lba: u32, file_sectors: u32) {
    check_in_flight(report, lba, file_sectors);
    check_bound(report, lba, file_sectors);
    check_park(report, lba);
    check_cancel(report, lba);
    check_error(report, lba, file_sectors);

    // 6. the polled reader, between requests.
    let _ = counted::stop_and_wait(120);
    // SAFETY: nothing is queued or active and the source is closed (`OnlyWhileBusy`), so the
    // transport is not programming the controller.
    let polled = unsafe { psx_io::periph::Cd::steal() };
    let mut reader = SectorReader::with_cd(polled);
    let mut sector = [0u32; SECTOR_WORDS];
    let found = reader.prepare() && reader.find(&mut sector, None, &[FILE_NAME]).is_some();
    let _ = reader.release();
    report.check("6 polled read between requests", found);
}

#[no_mangle]
fn main() {
    let Some(peripherals) = psx_rt::Peripherals::take() else {
        return;
    };
    let mut reader = SectorReader::with_cd(peripherals.cd);
    let mut sector = [0u32; SECTOR_WORDS];
    let found = if reader.prepare() {
        reader.find(&mut sector, None, &[FILE_NAME])
    } else {
        None
    };
    let cd = reader.release();
    let mut report = Report { ok: true };
    match found {
        Some((lba, bytes)) => {
            wait_vblank();
            let config = Config {
                timeout_vblanks: 300,
                irq_mask_policy: IrqMaskPolicy::OnlyWhileBusy,
                ..Config::DEFAULT
            };
            let installed = counted::install(cd, config).is_ok();
            report.check("0 install", installed);
            if installed {
                main_checks(&mut report, lba, bytes / 2048);
            }
        }
        None => report.check("0 find CDTEST.BIN", false),
    }
    let _ = counted::stop_and_wait(120);
    psx_rt::tty::println(if report.ok {
        "hello-cdcount: ALL PASS"
    } else {
        "hello-cdcount: FAIL"
    });
    loop {
        wait_vblank();
    }
}
