// SPDX-License-Identifier: GPL-2.0-or-later
//! CD DRAIN AND SEEK (v2.25, records `0xAA0`-`0xAB3`), for the goldsrc stream
//! moving onto one SDK path (lane cs-cd: the polled reader starts the read, the
//! CD interrupt pops the sectors through `psx_cdstream::drain`, a 600-VBlank
//! stall bound, two read attempts).
//!
//! **Drain cost** (`0xAA0`-`0xAA1`). Up to [`DRAIN_SECTORS`] sectors of
//! `CDTEST.BIN` (as many as the heap holds) read at double speed, started with
//! `SectorReader::prepare` and `start_read` (implicit seek, as goldsrc does)
//! and popped by the drain's interrupt handler, while the foreground spins a
//! counting loop that reads Timer 2 (system clock) every pass. Against the same
//! loop with nothing running, the passes lost give the CPU the drain costs per
//! sector, and the longest gap between two passes bounds the longest handler
//! call. Every byte is checked against the `CDTEST.BIN` pattern afterwards.
//!
//! * `0xAA0`: sectors stored, sectors a second times 10 (first sector to the
//!   last), sectors with a wrong byte (bits 0-11) and the end state (bits
//!   12-15, as `0xAA2`).
//! * `0xAA1`: foreground clocks lost a sector, the longest gap between two
//!   passes of the loop in clocks during the drain (`0xFFFF` past 65,535), the
//!   same gap with nothing running (the VBlank handler's).
//!
//! **Drain after idle** (`0xAA2`-`0xAA5`): the drive paused, then idle 0, 5, 15
//! and 30 s, then a 16-sector drain with the 600-VBlank bound: VBlanks to the
//! first sector, VBlanks to the last, end state (0 done, 1 the 600-VBlank
//! watchdog, 2 drive error, 3 FIFO never ready, 4 other, 5 not started).
//!
//! **Implicit seek against seek-first** (`0xAA8`-`0xAB3`). The SDK reader's
//! own documentation says a bare Setloc+ReadN (the implicit seek every goldsrc
//! load and the weapon-switch stream use) starts data flowing while the
//! mechanism is still settling and corrupted sustained streams on the owner's
//! console, while the BIOS bracket (Setloc, SeekL, SetMode, ReadN) did not.
//! This settles it: at three places (inner `CDTEST.BIN`, 300 sectors; the
//! middle of `RADIUS.DAT` and a minute before its end, [`LONG_RUN`] sectors
//! each, radius disc only), after parking the head at the other end of the
//! disc, one long polled read each way, every word of every sector checked as
//! it arrives. Case `k = 2 * place + method` (method 0 implicit, 1 seek-first):
//!
//! * `0xAA8 + 2 * k`: sectors delivered, sectors with wrong data, the first
//!   wrong sector's index in the run (`0xFFFF` none).
//! * `0xAA9 + 2 * k`: the first wrong sector's offset from where it should have
//!   come from, plus 128 (it matched the sector that many away, `128` the
//!   right one with damage, `0xFF` none of -8..+8), read failures (each restarts
//!   at the sector reached, at most three), HBlanks of the run divided by 16.
//!
//! Decoder labels: `cd_drain_cost`, `cd_drain_cost_cpu`, `cd_drain_after_idle_Ns`,
//! `cd_seek_PLACE_METHOD`, `cd_seek_PLACE_METHOD_detail`.

use crate::console_tests::record;
use crate::filler;
use crate::ui::{self, Line};
use crate::xa_loop::{self, Clock};
use crate::{push_timing_record, TimingRecord, TIMING_RECORD_COUNT};
use core::ptr::addr_of_mut;
use psx_cdstream::drain::{self, DrainFailure, DrainState};
use psx_font::FontAtlas;
use psx_io::cd::reader::{SectorReader, SECTOR_WORDS};
use psx_io::cd::wait::FIRST_SECTOR_WAIT;
use psx_io::timers::{self, Timer};
use psx_rt::interrupts;

type Records = [TimingRecord; TIMING_RECORD_COUNT];

const COST_RECORD: u16 = 0xAA0;
const IDLE_RECORD: u16 = 0xAA2;
const SEEK_RECORD: u16 = 0xAA8;
/// The drain's sectors at most (256 KiB of heap).
const DRAIN_SECTORS: u32 = 128;
/// The drain bound goldsrc uses.
const WATCHDOG_VBLANKS: u32 = 600;
/// Sectors of the long runs in `RADIUS.DAT`.
const LONG_RUN: u32 = 600;
/// Sectors of the inner run (inside `CDTEST.BIN`).
const INNER_RUN: u32 = 300;
const HZ: u32 = 15_734;
const CLOCKS_PER_LINE: u32 = 2_153;
/// Where the polled reader parks before an idle.
const PRIME_LBA: u32 = 300;

static mut SECTOR: [u32; SECTOR_WORDS] = [0; SECTOR_WORDS];

fn state_code(state: DrainState) -> u32 {
    match state {
        DrainState::Done => 0,
        DrainState::Failed(DrainFailure::Watchdog) => 1,
        DrainState::Failed(DrainFailure::Drive { .. }) => 2,
        DrainState::Failed(DrainFailure::DataNotReady) => 3,
        _ => 4,
    }
}

/// The counting loop for `lines` HBlanks with nothing running: (passes, the
/// longest gap in clocks). Its body is the drain loop's, pass for pass (the
/// state look and the clock every 64 passes), so the two pass rates compare.
#[optimize(size)]
#[inline(never)]
fn idle_loop(lines: u32) -> (u32, u32) {
    timers::set_mode(Timer::Timer2, 0);
    let mut clock = Clock::start();
    let mut passes = 0u32;
    let mut longest = 0u32;
    let mut last = timers::counter(Timer::Timer2);
    loop {
        let now = timers::counter(Timer::Timer2);
        longest = longest.max(u32::from(now.wrapping_sub(last)));
        last = now;
        passes += 1;
        if passes % 64 == 0 {
            let _ = drain::poll();
            if clock.ticks() >= lines {
                break;
            }
            last = timers::counter(Timer::Timer2);
        }
    }
    (passes, longest)
}

/// A drain of `sectors` at `lba` into `buffer`, the loop counting meanwhile:
/// (end state, passes, HBlanks in all, HBlank of the first sector, longest gap).
#[optimize(size)]
#[inline(never)]
fn drain_run(reader: &mut SectorReader, lba: u32, buffer: *mut u32, sectors: u32) -> [u32; 5] {
    if !(reader.prepare() && reader.start_read(lba)) {
        reader.stop();
        return [5, 0, 0, 0, 0];
    }
    // SAFETY: `buffer` holds `sectors` sectors of the heap that nothing else
    // touches until the drain is cancelled below; the read was started above.
    if !unsafe { drain::start(buffer, sectors, sectors * 2_048, WATCHDOG_VBLANKS) } {
        reader.stop();
        return [5, 0, 0, 0, 0];
    }
    timers::set_mode(Timer::Timer2, 0);
    let mut clock = Clock::start();
    let mut passes = 0u32;
    let mut longest = 0u32;
    let mut first = u32::MAX;
    let mut last = timers::counter(Timer::Timer2);
    let state = loop {
        let now = timers::counter(Timer::Timer2);
        longest = longest.max(u32::from(now.wrapping_sub(last)));
        last = now;
        passes += 1;
        if passes % 64 == 0 {
            match drain::poll() {
                DrainState::Running { received } => {
                    if received > 0 && first == u32::MAX {
                        first = clock.ticks();
                    }
                }
                other => break other,
            }
            // Keep the loop's clock wide (Timer 1 wraps every 4 s).
            let _ = clock.ticks();
            last = timers::counter(Timer::Timer2);
        }
    };
    let lines = clock.ticks();
    drain::cancel();
    reader.stop();
    [state_code(state), passes, lines, if first == u32::MAX { lines } else { first }, longest]
}

/// One polled read of `count` sectors from `lba`, each checked by `right`:
/// the two records' six fields.
#[optimize(size)]
#[inline(never)]
fn long_run(
    reader: &mut SectorReader,
    lba: u32,
    count: u32,
    seek_first: bool,
    right: &dyn Fn(u32, &[u32; SECTOR_WORDS]) -> bool,
    which: &dyn Fn(u32, &[u32; SECTOR_WORDS]) -> Option<i32>,
) -> [u32; 6] {
    // SAFETY: single thread; the buffer is this module's.
    let sector = unsafe { &mut *addr_of_mut!(SECTOR) };
    let mut clock = Clock::start();
    let (mut delivered, mut wrong, mut first_wrong, mut shift, mut failures) =
        (0u32, 0u32, 0xFFFFu32, 0xFFu32, 0u32);
    while delivered < count && failures <= 3 {
        let at = lba + delivered;
        let started = reader.prepare()
            && if seek_first {
                reader.start_read_seek_first(at, FIRST_SECTOR_WAIT)
            } else {
                reader.start_read(at)
            };
        if !started {
            failures += 1;
            reader.stop();
            continue;
        }
        while delivered < count {
            if !reader.read_sector(sector) {
                failures += 1;
                break;
            }
            if !right(delivered, sector) {
                wrong += 1;
                if first_wrong == 0xFFFF {
                    first_wrong = delivered;
                    shift = which(delivered, sector).map_or(0xFF, |d| (d + 128) as u32);
                }
            }
            delivered += 1;
        }
        reader.stop();
        crate::bounds::heartbeat();
    }
    [delivered, wrong, first_wrong, shift, failures, clock.ticks() / 16]
}

#[optimize(size)]
#[inline(never)]
fn idle(seconds: u32) {
    let mut clock = Clock::start();
    while clock.ticks() < seconds * HZ {}
}

#[optimize(size)]
#[inline(never)]
pub(crate) fn run(font: &FontAtlas, records: &mut Records, next: &mut usize) {
    let mut reader = SectorReader::with_cd(crate::probe_tokens::cd());
    // ---- drain cost
    crate::bounds::record_start(COST_RECORD);
    let room = psx_rt::heap::remaining().saturating_sub(32 * 1024) / 2_048;
    let sectors = (room as u32).min(DRAIN_SECTORS).min(crate::CDTEST_SECTORS);
    let bytes = sectors as usize * 2_048;
    let buffer = if sectors >= 16 { psx_rt::heap::alloc_raw(bytes, 4).cast::<u32>() } else { core::ptr::null_mut() };
    if buffer.is_null() {
        for id in COST_RECORD..IDLE_RECORD + 4 {
            push_timing_record(records, next, record(id, 0xFFFF, 0xFFFF, 0xFFFF));
        }
        ui::live_line(font, 0, "DRAIN: NO HEAP FOR THE BUFFER", (255, 110, 90));
    } else {
        let (idle_passes, idle_gap) = idle_loop(2_000);
        let r = drain_run(&mut reader, crate::CDTEST_LBA, buffer, sectors);
        let mut wrong = 0u32;
        for s in 0..sectors {
            // SAFETY: inside the buffer; the drain was cancelled.
            let at = unsafe { buffer.add((s * 512) as usize) };
            if !crate::cdstream_cases::verify(at, s, 1, 1) {
                wrong += 1;
            }
        }
        let span = r[2].saturating_sub(r[3]).max(1);
        let rate10 = (sectors - 1) * HZ * 10 / span;
        // Passes the loop would have made in the drain's HBlanks, from the idle rate.
        let per16 = (idle_passes * 16 / 2_000).max(1);
        let equivalent = r[1] * 16 / per16;
        let lost = r[2].saturating_sub(equivalent) * CLOCKS_PER_LINE / sectors;
        push_timing_record(records, next, record(COST_RECORD, sectors, rate10, wrong | r[0] << 12));
        push_timing_record(records, next, record(COST_RECORD + 1, lost, r[4], idle_gap));
        let mut line = Line::new();
        line.s("DRAIN: ")
            .u(rate10 / 10)
            .s(" SECTORS/S, ")
            .u(lost)
            .s(" CLK LOST/SECTOR, LONGEST ")
            .u(r[4])
            .s(" CLK, BAD ")
            .u(wrong);
        let good = wrong == 0 && r[0] == 0;
        ui::live_line(font, 0, line.as_str(), if good { (120, 230, 120) } else { (255, 110, 90) });

        // ---- drain after idle
        let mut line = Line::new();
        line.s("DRAIN AFTER IDLE (VBL):");
        for (k, seconds) in [0u32, 5, 15, 30].into_iter().enumerate() {
            let id = IDLE_RECORD + k as u16;
            crate::bounds::record_start(id);
            let _ = reader.read_at(PRIME_LBA, unsafe { &mut *addr_of_mut!(SECTOR) });
            idle(seconds);
            let start = interrupts::vblank_count();
            let r = drain_run(&mut reader, crate::CDTEST_LBA + 128, buffer, 16);
            let total = interrupts::vblank_count().wrapping_sub(start);
            let first = r[3] / 263;
            push_timing_record(records, next, record(id, first, total, r[0]));
            line.s(" ").u(first);
        }
        ui::live_line(font, 1, line.as_str(), (220, 224, 230));
        // SAFETY: the buffer is the heap's tip, allocated above and done with.
        let _ = unsafe { psx_rt::heap::resize_tip(buffer.cast::<u8>(), bytes, 0) };
    }

    // ---- implicit seek against seek-first
    let radius = xa_loop::locate(crate::radius::FILLER_FILE);
    let cdtest_end = crate::CDTEST_LBA + crate::CDTEST_SECTORS - 1;
    let mut places = [Some((crate::CDTEST_LBA, INNER_RUN)); 3];
    let (far, first_lba) = match radius {
        Some((first, bytes)) => {
            let end = first + bytes / 2_048;
            places[1] = Some((first + (end - first) / 2, LONG_RUN));
            places[2] = Some((end - 4_500, LONG_RUN));
            (end - 1, first)
        }
        None => {
            places[1] = None;
            places[2] = None;
            (cdtest_end, 0)
        }
    };
    for (place, &spot) in places.iter().enumerate() {
        for method in 0..2u16 {
            let k = 2 * place as u16 + method;
            let id = SEEK_RECORD + 2 * k;
            crate::bounds::record_start(id);
            let Some((lba, count)) = spot else {
                push_timing_record(records, next, record(id, 0xFFFF, 0xFFFF, 0xFFFF));
                push_timing_record(records, next, record(id + 1, 0xFFFF, 0xFFFF, 0xFFFF));
                continue;
            };
            // Park at the other end of the disc, so the start is a long seek.
            let park = if place == 0 { far } else { PRIME_LBA };
            let _ = reader.read_at(park, unsafe { &mut *addr_of_mut!(SECTOR) });
            let r = if place == 0 {
                let right = |i: u32, s: &[u32; SECTOR_WORDS]| crate::cdstream_cases::verify(s.as_ptr(), i, 1, 1);
                let which = |i: u32, s: &[u32; SECTOR_WORDS]| {
                    (-8i32..=8).find(|d| {
                        let at = i as i32 + d;
                        at >= 0 && (at as u32) < crate::CDTEST_SECTORS && crate::cdstream_cases::verify(s.as_ptr(), at as u32, 1, 1)
                    })
                };
                long_run(&mut reader, lba, count, method == 1, &right, &which)
            } else {
                let base = lba - first_lba;
                let right = move |i: u32, s: &[u32; SECTOR_WORDS]| {
                    s.iter().enumerate().all(|(w, &v)| v == filler::word(base + i, w as u32))
                };
                let which = move |i: u32, s: &[u32; SECTOR_WORDS]| {
                    (-8i32..=8).find(|d| {
                        let at = (base + i) as i32 + d;
                        at >= 0 && s[0] == filler::word(at as u32, 0) && s[511] == filler::word(at as u32, 511)
                    })
                };
                long_run(&mut reader, lba, count, method == 1, &right, &which)
            };
            push_timing_record(records, next, record(id, r[0], r[1], r[2]));
            push_timing_record(records, next, record(id + 1, r[3], r[4], r[5]));
            let mut line = Line::new();
            line.s(["INNER", "MIDDLE", "OUTER"][place])
                .s(if method == 0 { " IMPLICIT: " } else { " SEEK FIRST: " })
                .u(r[1])
                .s(" BAD OF ")
                .u(r[0]);
            let good = r[1] == 0 && r[0] == count;
            ui::live_line(font, 2 + k as u32, line.as_str(), if good { (120, 230, 120) } else { (255, 110, 90) });
        }
    }
    let _ = reader.release();
}
