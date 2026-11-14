// SPDX-License-Identifier: GPL-2.0-or-later
//! CD STOP AT ACKNOWLEDGE: `SectorReader::stop` with and without its wait for Pause's second response.
//!
//! `stop` sends Pause and today waits 16,384 spins (about 296,000 clocks) for the second response, which
//! on the console comes 1.2 to 3.9 million clocks after the acknowledge (`psx_io::cd::pause`), so the wait
//! always runs out and a load of many short reads pays about 8.9 ms for each. `psx_io::cd::reader::STOP_AT_ACKNOWLEDGE`
//! (off) returns at the acknowledge instead, and the next command of the next read, Setloc, then reaches a
//! drive that is still completing the Pause. A command between the two responses is known to cancel the
//! second one only for GetStat (`CD PAUSE SECOND RESPONSE`, 4 of 4). This step is the console's answer for
//! the commands a read sequence sends.
//!
//! Four cases, each a run of reads from `CDTEST.BIN` at scattered sectors, every sector checked against the
//! file's pattern, each read followed by `stop`:
//!
//! * `0x6F8` reads of 1 to 8 sectors, `stop` as shipped (waits for the second response).
//! * `0x6F9` the same reads, `stop` returning at the acknowledge.
//! * `0x6FA` reads of one sector, as shipped.
//! * `0x6FB` reads of one sector, returning at the acknowledge.
//!
//! A read that fails (the drive refuses a command, a sector never comes, a sector holds the wrong bytes) is
//! tried once more after a proved stop; the second failure is an error. Fields of each record:
//!
//! * `sectors`: sectors read and checked correct.
//! * `errors_and_retries`: errors (reads that failed twice) in the high byte, retries in the low byte.
//! * `time`: the whole run in units of 16 HBlanks (about a millisecond). The run is [`READS`] reads.
//!
//! Pass: `0x6F9` and `0x6FB` read as many sectors correctly as `0x6F8` and `0x6FA` with no more errors and
//! retries (both zero is the expectation), and take less time, by about 8.9 ms a read. Fail: any extra error or
//! retry, or a wrong sector, in `0x6F9`/`0x6FB`; then the early return is wrong for that drive and is deleted.

use crate::cdstream_cases::verify;
use crate::console_tests::record;
use crate::{push_timing_record, TimingRecord, TIMING_RECORD_COUNT};
use psx_io::cd::reader::{SectorReader, SECTOR_WORDS};
use psx_io::timers::{self, Timer};
use psx_rt::tty;

type Records = [TimingRecord; TIMING_RECORD_COUNT];

/// First record of the step: four records, `0x6F8` to `0x6FB`.
const RECORD: u16 = 0x6F8;
/// Reads in a run.
const READS: u32 = 40;
const FILE_LBA: u32 = crate::CDTEST_LBA;
const FILE_SECTORS: u32 = crate::CDTEST_SECTORS;

pub(crate) static mut READER: SectorReader = SectorReader::with_cd(unsafe { psx_io::periph::Cd::steal() });
static mut SECTOR: [u32; SECTOR_WORDS] = [0; SECTOR_WORDS];

/// One read of `count` sectors from file sector `first`, then `stop`. `true` when every sector arrived and
/// holds the pattern.
fn read_once(reader: &mut SectorReader, first: u32, count: u32, wait: bool, ok: &mut u32) -> bool {
    let mut good = 0;
    let mut all = reader.prepare() && reader.start_read(FILE_LBA + first);
    if all {
        for sector in 0..count {
            // SAFETY: single thread; one borrow at a time.
            let buffer = unsafe { &mut *core::ptr::addr_of_mut!(SECTOR) };
            if !reader.read_sector(buffer) || !verify(buffer.as_ptr(), first + sector, 1, 1) {
                all = false;
                break;
            }
            good += 1;
        }
    }
    reader.stop_waiting(wait);
    *ok += good;
    all
}

/// One run: [`READS`] reads, `one` sector each or 1 to 8, with `stop` waiting or not.
fn run_case(wait: bool, one: bool) -> (u32, u32, u32, u32) {
    // SAFETY: single thread; the suite owns the drive for the step.
    let reader = unsafe { &mut *core::ptr::addr_of_mut!(READER) };
    timers::set_mode(Timer::Timer1, 0x0100);
    let (mut sectors, mut errors, mut retries, mut hblanks) = (0u32, 0u32, 0u32, 0u32);
    let mut last = timers::counter(Timer::Timer1);
    for read in 0..READS {
        let count = if one { 1 } else { 1 + read % 8 };
        // Scattered, so most reads seek: 37 is coprime with the file's length.
        let first = (read * 37) % (FILE_SECTORS - 8);
        if !read_once(reader, first, count, wait, &mut sectors) {
            retries += 1;
            // A proved stop first, so the retry starts from a quiet drive.
            let _ = reader.stop_proved();
            if !read_once(reader, first, count, wait, &mut sectors) {
                errors += 1;
            }
        }
        let now = timers::counter(Timer::Timer1);
        hblanks += u32::from(now.wrapping_sub(last));
        last = now;
    }
    let _ = reader.stop_proved();
    (sectors, errors, retries, hblanks)
}

/// The four records.
pub(crate) fn run(records: &mut Records, next: &mut usize) {
    for (case, (wait, one)) in [(true, false), (false, false), (true, true), (false, true)]
        .into_iter()
        .enumerate()
    {
        let id = RECORD + case as u16;
        crate::bounds::record_start(id);
        let (sectors, errors, retries, hblanks) = run_case(wait, one);
        tty::print("hardware-tests: cd-stop case ");
        tty::print(if wait { "waits" } else { "early" });
        tty::print(if one { " single" } else { " mixed" });
        for (label, value) in [
            (" sectors=", sectors),
            (" errors=", errors),
            (" retries=", retries),
            (" hblanks=", hblanks),
        ] {
            tty::print(label);
            let mut buffer = [0u8; 10];
            tty::print(crate::console_tests::number(&mut buffer, value));
        }
        tty::println("");
        push_timing_record(
            records,
            next,
            record(id, sectors, (errors.min(255) << 8) | retries.min(255), hblanks / 16),
        );
    }
}
