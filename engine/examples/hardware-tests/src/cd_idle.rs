// SPDX-License-Identifier: GPL-2.0-or-later
//! CD READ AFTER IDLE: how long the SDK's polled reader (`SectorReader`) waits
//! for the first sector of a read issued after the drive sat paused for 5, 10,
//! 15 and 30 seconds, and after a Stop, against the bound the reader gives up
//! at; and whether such a read ever fails the first time and works on a retry.
//!
//! Why: a game on the 2026-11-19 console session (QD Up) stopped in a load that
//! the emulator finishes. `CD MOTOR` (0x312) once measured 4.4 s to the first
//! sector of a read after a Pause and 15 s; the reader's data wait is
//! `DATA_POLL` = 4,000,000 spins, whose length in time is the cost of one spin
//! on the console, which nothing measured. Here the reader's idle hook (called
//! once per spin of every wait it makes) counts the spins and widens Timer 1's
//! HBlank count, so each read gives its time and its spins, and the bound in
//! milliseconds follows (the hook's own few instructions are inside the spin,
//! so the bound comes out slightly long).
//!
//! Records from `0x9C0`, two per case (case 0 to 4: Pause and 5, 10, 15, 30 s,
//! then Stop and 5 s):
//!
//! * `0x9C0 + 2 * case`: HBlanks from the read call to its return `lo`, `hi`
//!   (times 2,153 for system clocks), outcome (bit 0 the first read failed,
//!   bit 1 a retry was made and worked, bits 8 to 15 the reader's failure cause
//!   byte).
//! * `0x9C1 + 2 * case`: spins of the reader's waits in that read `lo`, `hi`,
//!   HBlanks of the retry (0 when none, 0xFFFF clamped).
//! * `0x9CA`: tenths of a clock per spin (from the longest read), the reader's
//!   `DATA_POLL` bound in milliseconds at that cost, the Stop command's result
//!   (1 acknowledged, 0 refused).

use crate::console_tests::record;
use crate::ui::{self, Line};
use crate::{push_timing_record, TimingRecord, TIMING_RECORD_COUNT};
use psx_font::FontAtlas;
use psx_hw::cd::CMD_STOP;
use psx_io::cd::reader::{set_idle_hook, SectorReader, SECTOR_WORDS};
use psx_io::timers::{self, Timer};

type Records = [TimingRecord; TIMING_RECORD_COUNT];

const FIRST_RECORD: u16 = 0x9C0;
const BOUND_RECORD: u16 = 0x9CA;
/// The reader's data wait, `psx_io::cd::reader`'s `DATA_POLL`.
const DATA_POLL: u32 = 4_000_000;
const HZ: u32 = 15_734;
const CLOCKS_PER_LINE: u32 = 2_153;
/// Timer 1 on the HBlank clock.
const HBLANK_CLOCK: u16 = 0x0100;
/// The test region `CD MOTOR` reads.
const PRIME_LBA: u32 = 300;
const READ_LBA: u32 = 300 + 1 + 128;
/// Idle seconds of the four Pause cases; the Stop case waits 5.
const PAUSES: [u32; 4] = [5, 10, 15, 30];

static mut SPINS: u32 = 0;
static mut LINES: u32 = 0;
static mut LAST: u16 = 0;

/// The reader's idle hook: one spin, and Timer 1 widened to 32 bits.
fn count_spin() {
    let now = timers::counter(Timer::Timer1);
    // SAFETY: single thread; only this hook and `begin`/`lines` touch these.
    unsafe {
        SPINS += 1;
        LINES += u32::from(now.wrapping_sub(LAST));
        LAST = now;
    }
}

fn begin() {
    timers::set_mode(Timer::Timer1, HBLANK_CLOCK);
    timers::set_counter(Timer::Timer1, 0);
    // SAFETY: as `count_spin`.
    unsafe {
        SPINS = 0;
        LINES = 0;
        LAST = 0;
    }
}

/// HBlanks and spins since [`begin`], the time up to now included.
fn taken() -> (u32, u32) {
    count_spin();
    // SAFETY: as `count_spin`.
    unsafe { (LINES, SPINS - 1) }
}

/// Wait `seconds` on Timer 1, servicing nothing: the drive is left alone.
fn idle(seconds: u32) {
    begin();
    let mut guard = 0u32;
    while taken().0 < seconds * HZ && guard < 400_000_000 {
        guard += 1;
    }
}

/// One timed read: its HBlanks, spins, outcome and the retry's HBlanks.
fn timed_read(reader: &mut SectorReader, buffer: &mut [u32; SECTOR_WORDS]) -> (u32, u32, u32, u32) {
    begin();
    let ok = reader.read_at(READ_LBA, buffer);
    let (lines, spins) = taken();
    if ok {
        return (lines, spins, 0, 0);
    }
    let cause = reader.diagnostics() >> 24;
    begin();
    let retry = reader.read_at(READ_LBA, buffer);
    let (retry_lines, _) = taken();
    (lines, spins, 1 | (u32::from(retry) << 1) | (cause << 8), retry_lines.min(0xFFFF))
}

fn show(font: &FontAtlas, row: u32, name: &str, lines: u32, outcome: u32, bound_ms: u32) {
    let mut line = Line::new();
    line.s(name)
        .s(": ")
        .u(lines * 1000 / HZ)
        .s(" MS (BOUND ")
        .u(bound_ms)
        .s(" MS)")
        .s(match outcome & 3 {
            0 => " OK",
            3 => " FAILED, RETRY OK",
            _ => " FAILED TWICE",
        });
    let colour = if outcome == 0 { (120, 230, 120) } else { (255, 110, 90) };
    ui::live_line(font, row, line.as_str(), colour);
}

pub(crate) fn run(font: &FontAtlas, records: &mut Records, next: &mut usize) {
    crate::bounds::record_start(FIRST_RECORD);
    // The polled reader owns the drive here: no transport is installed between the
    // Drive area's CD steps (each opens and closes its own).
    let mut reader = SectorReader::with_cd(crate::probe_tokens::cd());
    let mut buffer = [0u32; SECTOR_WORDS];
    set_idle_hook(Some(count_spin));
    let prepared = reader.prepare();

    let mut results = [(0u32, 0u32, 0u32, 0u32); 5];
    for (case, &seconds) in PAUSES.iter().enumerate() {
        crate::bounds::record_start(FIRST_RECORD + 2 * case as u16);
        let mut line = Line::new();
        line.s("PAUSE THEN IDLE ").u(seconds).s(" S");
        ui::live_line(font, case as u32, line.as_str(), (220, 224, 230));
        // read_at ends with a Pause.
        let _ = reader.read_at(PRIME_LBA, &mut buffer);
        idle(seconds);
        results[case] = timed_read(&mut reader, &mut buffer);
    }
    crate::bounds::record_start(FIRST_RECORD + 8);
    ui::live_line(font, 4, "STOP THEN IDLE 5 S", (220, 224, 230));
    let _ = reader.read_at(PRIME_LBA, &mut buffer);
    let stopped = reader.cd_mut().command(CMD_STOP, &[]).is_ok();
    idle(5);
    results[4] = timed_read(&mut reader, &mut buffer);

    set_idle_hook(None);
    reader.stop();
    let _ = reader.release();

    // The cost of a spin from the read that spun most.
    let (lines, spins) = results
        .iter()
        .max_by_key(|r| r.1)
        .map_or((0, 0), |r| (r.0, r.1));
    let tenths = if spins == 0 {
        0
    } else {
        // Clocks x 10 / spins, without overflowing 32 bits: lines stay under
        // about 200,000 for a 10 s read.
        (lines * CLOCKS_PER_LINE / spins.max(1)) * 10
            + (lines * CLOCKS_PER_LINE % spins.max(1)) * 10 / spins.max(1)
    };
    // DATA_POLL spins at that cost, in ms: (tenths / 10) * 4,000,000 clocks over
    // 33,868.8 clocks a millisecond = tenths * 40,000 / 3,386.88.
    let bound_ms = tenths * (DATA_POLL / 100) / 3_387;
    for (case, r) in results.iter().enumerate() {
        let id = FIRST_RECORD + 2 * case as u16;
        push_timing_record(records, next, record(id, r.0 & 0xFFFF, r.0 >> 16, r.2));
        push_timing_record(records, next, record(id + 1, r.1 & 0xFFFF, r.1 >> 16, r.3));
        let name = match case {
            0 => "PAUSE+5S",
            1 => "PAUSE+10S",
            2 => "PAUSE+15S",
            3 => "PAUSE+30S",
            _ => "STOP+5S",
        };
        show(font, case as u32, name, r.0, r.2, bound_ms);
    }
    push_timing_record(
        records,
        next,
        record(
            BOUND_RECORD,
            tenths,
            bound_ms,
            u32::from(stopped) | (u32::from(!prepared) << 1),
        ),
    );
    let mut line = Line::new();
    line.s("ONE READER SPIN = ")
        .u(tenths / 10)
        .s(".")
        .u(tenths % 10)
        .s(" CLK; DATA_POLL = ")
        .u(bound_ms)
        .s(" MS");
    ui::live_line(font, 5, line.as_str(), (220, 224, 230));
}
