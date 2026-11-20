// SPDX-License-Identifier: GPL-2.0-or-later
//! CD FIFO AND OUTER CHUNKS (v2.25, records `0xA40`-`0xA4B`).
//!
//! **FIFO fill.** The SDK reader's `pop_sector` requests a sector's data after
//! the drive's data-ready interrupt (INT1) and waits up to 100 ms for the data
//! FIFO; until `lane/console-fixes` c2ad07691 an expired wait popped 2,048
//! bytes anyway and the read counted as good, now it fails the read. How long
//! the FIFO really takes, and whether the wait ever runs out, was never
//! measured. Raw commands (SetMode double speed, Setloc, ReadN)
//! read [`FIFO_SECTORS`] sectors at three places: `CDTEST.BIN` (inner), the
//! middle of `RADIUS.DAT` and its last minute (outer; the radius disc only).
//! For each sector Timer 2 (system clock / 8) runs from the data request to the
//! FIFO reporting data.
//!
//! * `0xA40 + 2 * place`: the wait in units of 8 system clocks, min, median,
//!   max (`0xFFFF` for a place not on this disc, or a wait past 15.5 ms).
//! * `0xA41 + 2 * place`: sectors read, waits over 15 ms (bits 0-7) and over
//!   100 ms (bits 8-15: past the SDK's bound, a failed read), drive errors
//!   (INT5, bits 0-7) and timeouts (bits 8-15).
//!
//! **Outer chunks.** Pack loads (`psx_pack`, goldsrc) read a chunk as prepare,
//! start a read, `read_sector` per sector, stop, with no checksum and no retry.
//! The 2026-11-03 console run saw INT5 at the outer radius. At three places in
//! the last minute of `RADIUS.DAT` (60, 20 and 3 seconds before its end) the
//! step reads [`CHUNKS`] chunks of [`CHUNK`] sectors exactly that way, checks
//! every word against the filler formula, and retries a failed chunk up to
//! twice:
//!
//! * `0xA46 + 2 * place`: chunks right the first time, chunks right after a
//!   retry, chunks never right.
//! * `0xA47 + 2 * place`: sectors with wrong data (all attempts), reads that
//!   ended on a drive error (INT5, high byte) or a timeout (low byte), the
//!   first INT5 error code (0 none).
//!
//! Decoder labels: `cd_fifo_PLACE`, `cd_fifo_PLACE_counts`,
//! `cd_outer_chunks_Ns`, `cd_outer_chunks_Ns_errors`.

use crate::console_tests::{record, spread};
use crate::filler;
use crate::ui::{self, Line};
use crate::xa_loop::{self, Clock};
use crate::{push_timing_record, TimingRecord, TIMING_RECORD_COUNT};
use core::ptr::addr_of_mut;
use psx_font::FontAtlas;
use psx_io::cd::reader::{SectorReader, SECTOR_WORDS};
use psx_io::periph::Cd;
use psx_io::timers::{self, Timer};
use psx_rt::interrupts;

type Records = [TimingRecord; TIMING_RECORD_COUNT];

const FIFO_RECORD: u16 = 0xA40;
const CHUNK_RECORD: u16 = 0xA46;
const FIFO_SECTORS: usize = 32;
const CHUNKS: usize = 16;
const CHUNK: u32 = 8;
const ATTEMPTS: u32 = 3;
const SPINS: u32 = 200_000;
/// Silence after which a raw read gives up: HBlanks, about 2.5 s.
const IDLE_TICKS: u32 = 39_000;
/// Timer 2: system clock / 8.
const SYSCLK_DIV8: u16 = 0x0200;

static mut SECTOR: [u32; SECTOR_WORDS] = [0; SECTOR_WORDS];

/// Leave the drive paused with no flag waiting.
#[optimize(size)]
#[inline(never)]
fn quiet(cd: &mut Cd) {
    let _ = cd.try_pause_until_complete(SPINS);
    for _ in 0..8 {
        let flag = cd.irq_flag_value();
        if flag == 0 {
            break;
        }
        cd.discard_response();
        cd.acknowledge_irq(flag);
    }
}

/// Request the sector and time the FIFO: (units of 8 clocks, VBlanks), then
/// drain it (byte reads, as the SDK's PIO pop).
#[optimize(size)]
#[inline(never)]
fn pop_timed(cd: &mut Cd) -> (u32, u32, bool) {
    timers::set_mode(Timer::Timer2, SYSCLK_DIV8);
    let vblank = interrupts::vblank_count();
    timers::set_counter(Timer::Timer2, 0);
    cd.request_data();
    let mut guard = 0u32;
    let mut ready = true;
    while !cd.is_data_fifo_ready() {
        guard += 1;
        if guard > 20_000_000 {
            ready = false;
            break;
        }
    }
    let units = u32::from(timers::counter(Timer::Timer2));
    let vblanks = interrupts::vblank_count().wrapping_sub(vblank);
    for _ in 0..2_048 {
        let _ = cd.read_data_byte();
    }
    (units, vblanks, ready)
}

/// Raw double-speed reads of [`FIFO_SECTORS`] at `lba`: the two records.
#[optimize(size)]
#[inline(never)]
fn fifo_place(lba: u32) -> [u32; 6] {
    let mut cd = crate::probe_tokens::cd();
    let mut units = [0u32; FIFO_SECTORS];
    let (mut read, mut over15, mut over100, mut int5, mut timeouts) = (0usize, 0u32, 0u32, 0u32, 0u32);
    quiet(&mut cd);
    let started = cd.try_set_mode(0x80, SPINS).is_some()
        && cd.try_set_target_lba(lba, SPINS).is_some()
        && cd.try_start_reading(SPINS).is_some();
    if started {
        let mut clock = Clock::start();
        let mut idle_since = clock.ticks();
        while read < FIFO_SECTORS {
            let now = clock.ticks();
            if now.wrapping_sub(idle_since) > IDLE_TICKS {
                timeouts += 1;
                break;
            }
            match cd.irq_flag_value() {
                0 => {}
                1 => {
                    let (taken, vblanks, ready) = pop_timed(&mut cd);
                    cd.discard_response();
                    cd.acknowledge_irq(1);
                    // Timer 2 wraps after 15.5 ms; two VBlank edges mean
                    // more than 16.7 ms went by, seven more than 100 ms.
                    units[read] = if vblanks >= 2 { 0xFFFF } else { taken };
                    if vblanks >= 2 {
                        over15 += 1;
                    }
                    if vblanks >= 7 || !ready {
                        over100 += 1;
                    }
                    read += 1;
                    idle_since = clock.ticks();
                }
                5 => {
                    cd.discard_response();
                    cd.acknowledge_irq(5);
                    int5 += 1;
                    break;
                }
                other => {
                    cd.discard_response();
                    cd.acknowledge_irq(other);
                }
            }
        }
    } else {
        timeouts += 1;
    }
    quiet(&mut cd);
    let (min, median, max) = spread(&mut units[..read.max(1)]);
    [
        min,
        median,
        max,
        read as u32,
        over15.min(0xFF) | over100.min(0xFF) << 8,
        int5.min(0xFF) | timeouts.min(0xFF) << 8,
    ]
}

/// One chunk the way `psx_pack` loads it: (sectors right, drive error code or
/// 0, timed out).
#[optimize(size)]
#[inline(never)]
fn chunk(reader: &mut SectorReader, lba: u32, first: u32) -> (u32, u32, bool) {
    // SAFETY: single thread; the buffer is this module's.
    let sector = unsafe { &mut *addr_of_mut!(SECTOR) };
    let mut right = 0u32;
    let mut failed = false;
    if reader.prepare() && reader.start_read(lba) {
        for k in 0..CHUNK {
            if !reader.read_sector(sector) {
                failed = true;
                break;
            }
            let index = lba + k - first;
            if sector.iter().enumerate().all(|(i, &w)| w == filler::word(index, i as u32)) {
                right += 1;
            }
        }
    } else {
        failed = true;
    }
    let diag = reader.diagnostics();
    reader.stop();
    let code = if failed && (diag >> 24) as u8 == 0x05 { 0x100 | (diag & 0xFF) } else { 0 };
    (right, code, failed && code == 0)
}

#[optimize(size)]
#[inline(never)]
fn chunk_place(lba: u32, first: u32) -> [u32; 6] {
    let mut reader = SectorReader::with_cd(crate::probe_tokens::cd());
    let (mut clean, mut retried, mut never) = (0u32, 0u32, 0u32);
    let (mut wrong, mut int5, mut timeouts, mut first_code) = (0u32, 0u32, 0u32, 0u32);
    for c in 0..CHUNKS {
        let at = lba + c as u32 * CHUNK;
        let mut ok = false;
        for attempt in 0..ATTEMPTS {
            let (right, code, timed_out) = chunk(&mut reader, at, first);
            wrong += CHUNK - right;
            if code != 0 {
                int5 += 1;
                if first_code == 0 {
                    first_code = code & 0xFF;
                }
            }
            if timed_out {
                timeouts += 1;
            }
            if right == CHUNK {
                ok = code == 0 && !timed_out;
                if ok {
                    if attempt == 0 {
                        clean += 1;
                    } else {
                        retried += 1;
                    }
                    break;
                }
            }
        }
        if !ok {
            never += 1;
        }
        crate::bounds::heartbeat();
    }
    let _ = reader.release();
    [clean, retried, never, wrong, int5.min(0xFF) << 8 | timeouts.min(0xFF), first_code]
}

#[optimize(size)]
#[inline(never)]
pub(crate) fn run(font: &FontAtlas, records: &mut Records, next: &mut usize) {
    let radius = xa_loop::locate(crate::radius::FILLER_FILE);
    let mut fifo_places = [Some(crate::CDTEST_LBA), None, None];
    if let Some((first, bytes)) = radius {
        let sectors = bytes / 2_048;
        fifo_places[1] = Some(first + sectors / 2);
        fifo_places[2] = Some(first + sectors - 4_500);
    }
    for (place, lba) in fifo_places.iter().enumerate() {
        let id = FIFO_RECORD + 2 * place as u16;
        crate::bounds::record_start(id);
        let values = match lba {
            Some(lba) => fifo_place(*lba),
            None => [0xFFFF; 6],
        };
        push_timing_record(records, next, record(id, values[0], values[1], values[2]));
        push_timing_record(records, next, record(id + 1, values[3], values[4], values[5]));
        let mut line = Line::new();
        line.s(["FIFO INNER", "FIFO MIDDLE", "FIFO OUTER"][place]);
        if lba.is_some() {
            line.s(": MEDIAN ")
                .u(values[1] * 8)
                .s(" CLK, MAX ")
                .u(values[2] * 8)
                .s(", OVER 100 MS ")
                .u(values[4] >> 8);
        } else {
            line.s(": NO RADIUS.DAT");
        }
        let good = lba.is_none() || values[4] == 0 && values[5] == 0;
        ui::live_line(font, place as u32, line.as_str(), if good { (120, 230, 120) } else { (255, 110, 90) });
    }
    for place in 0..3usize {
        let id = CHUNK_RECORD + 2 * place as u16;
        crate::bounds::record_start(id);
        let values = match radius {
            Some((first, bytes)) => {
                let end = first + bytes / 2_048;
                let back = [4_500u32, 1_500, 225][place];
                chunk_place(end - back, first)
            }
            None => [0xFFFF; 6],
        };
        push_timing_record(records, next, record(id, values[0], values[1], values[2]));
        push_timing_record(
            records,
            next,
            record(id + 1, values[3], values[4], values[5]),
        );
        let mut line = Line::new();
        line.s(["CHUNKS -60 S", "CHUNKS -20 S", "CHUNKS -3 S"][place]);
        if radius.is_some() {
            line.s(": ").u(values[0]).s(" OK, ").u(values[1]).s(" RETRIED, ").u(values[2]).s(" NEVER");
        } else {
            line.s(": NO RADIUS.DAT");
        }
        let good = radius.is_none() || values[0] == CHUNKS as u32;
        ui::live_line(font, 3 + place as u32, line.as_str(), if good { (120, 230, 120) } else { (255, 110, 90) });
    }
}
