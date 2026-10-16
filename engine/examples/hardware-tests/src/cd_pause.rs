// SPDX-License-Identifier: GPL-2.0-or-later
//! CD PAUSE: how long the controller takes to deliver Pause's second response.
//!
//! Pause answers twice: an acknowledge (INT3) at once, and a completion (INT2)
//! when the drive has actually stopped. PSX-SPX gives about 1,096,083 system
//! clocks for a double-speed drive that was reading; the emulator has used an
//! estimate of 120,000 clocks, and the v1.28 CD-DA case measured 123 ms
//! (4.17 million clocks) after audio. Nothing in the suite had timed the
//! second response for a *data read* or an idle drive, at both speeds and with
//! the head in different places, so a game that waits for INT2 (every loader
//! that pauses between chunks) was working from a guess.
//!
//! One clock for everything: Timer 2 counting system clock / 8 (one tick is
//! eight system clocks), widened to 32 bits by noticing wraps. Every wait
//! reads it at least once per loop pass, far more often than its 15.5 ms wrap.
//! Times are reported in system clocks (ticks x 8), measured from the moment
//! the Pause command byte has been written to the moment the loop saw the
//! interrupt flag change; the loop's own pass time (record 0x340) is the
//! resolution, and a VBlank handler landing in a window adds its few hundred
//! clocks.
//!
//! Every wait is bounded twice: by a clock deadline and by a pass count that
//! the SELECT skip key shrinks to one (`bounds::scale`). The second response
//! has [`INT2_LIMIT`] system clocks (8,000,000, 7.3 times PSX-SPX's figure and
//! about twice the longest the console has shown); a Pause that does not
//! complete in that time is recorded as a timeout, not waited for. The data
//! the drive reads is never fetched: each sector's INT1 is acknowledged and
//! counted, which is all the controller needs to keep streaming and to be
//! free to post INT2.
//!
//! The matrix is two speeds (SetMode bit 7 clear and set) by five head states,
//! three trials each:
//!
//! * IDLE: the drive paused and spinning, Pause again.
//! * SETTLED: a read near the start of the test region, Pause after 12 sectors.
//! * FIRST: the same read, Pause right after the first sector arrives.
//! * SEEK: a read aimed at a far sector, Pause the moment ReadN is
//!   acknowledged, while the head is still travelling (no sector yet).
//! * FAR: the same far read, Pause after its first sector. Each far trial
//!   first brings the head back to the test region, so the seek is long.
//!
//! Then the WATCH trials, two per speed, on the settled read: a Pause followed
//! by GetStat commands ([`WATCH_COMMANDS`] of them), logging every flag the
//! controller shows with its time and the status byte. EARLY sends the first
//! GetStat the moment the Pause is acknowledged and then one every
//! [`EARLY_STEP`] clocks; SPACED waits [`SPACED_STEP`] clocks from the Pause
//! for the first and sends one every [`SPACED_STEP`]. Whether a command sent
//! between Pause's two responses cancels the second one, and what the status
//! byte says while the drive spins down, is read off those events; the two
//! schedules are there so that a cancelling GetStat in EARLY does not hide the
//! later status bytes from SPACED.
//!
//! Records from `0x340` (system clocks are split over two fields where they
//! can pass 16 bits: `lo` then `hi`, `value = hi * 65536 + lo`):
//! 0x340: clocks per pass of the wait loop, min, median, max;
//! 0x341: INT2 limit and INT3 limit in units of 1024 clocks, trials run;
//! 0x342: setup steps that failed (a mode, Setloc or ReadN the drive refused,
//!   or a pause that never settled between trials), cleanup failures, 0;
//! 0x350 + 4 * cell + trial (trial 0 to 2): `lo`, `hi` of the clocks from the
//!   Pause to INT2 (0xFFFF 0xFFFF when it never came), then outcome in bits
//!   0 and 1 (0 completed, 1 no INT2 within the limit, 2 the drive answered
//!   with an error, 3 no acknowledge or the command was refused), sectors that
//!   arrived after the Pause command in bits 2 to 5 (saturating at 15), the
//!   status byte of the INT2 in bits 8 to 15;
//! 0x350 + 4 * cell + 3: the Pause acknowledge (INT3) time, min, median, max,
//!   in units of 32 clocks;
//!   where cell = speed * 5 + state, speed 0 single and 1 double, state 0 IDLE,
//!   1 SETTLED, 2 FIRST, 3 SEEK, 4 FAR (cells 0 to 9, ids 0x350 to 0x377);
//! 0x380 + 16 * watch + event (event 0 to 7): the offset of a flag from the
//!   Pause in units of 128 clocks, the flag in bits 8 to 10 (1 sector, 2
//!   complete, 3 acknowledge, 5 error, 7 none by the limit) and the status
//!   byte in bits 0 to 7 of the second field, GetStat commands sent so far;
//! 0x380 + 16 * watch + 8: flags seen in all (may exceed 8), GetStats sent,
//!   whether the Pause's own INT2 was among them (1 yes);
//!   where watch = speed * 2 + schedule (0 SPACED, 1 EARLY), ids 0x380 to 0x3B8.

use crate::bounds;
use crate::console_tests::{record, spread};
use crate::{ui, TimingRecord};
use psx_font::FontAtlas;
use psx_hw::cd::{CMD_GETSTAT, CMD_PAUSE, MODE_DOUBLE_SPEED};
use psx_io::cd as cdio;
use psx_io::periph::Cd;
use psx_io::timers::{self, Timer};

/// First record id.
pub(crate) const CD_PAUSE_RECORD: u16 = 0x340;
const META_POLL: u16 = CD_PAUSE_RECORD;
const META_LIMITS: u16 = 0x341;
const META_FAILS: u16 = 0x342;
const TRIAL_BASE: u16 = 0x350;
const WATCH_BASE: u16 = 0x380;

const SPEEDS: usize = 2;
const STATES: usize = 5;
const TRIALS: usize = 3;
const WATCHES: usize = 4;
const WATCH_EVENTS: usize = 8;
/// Records: three meta, four per cell, nine per watch.
pub(crate) const CD_PAUSE_COUNT: usize = 3 + SPEEDS * STATES * (TRIALS + 1) + WATCHES * (WATCH_EVENTS + 1);

/// Clocks per tick of the counter (system clock / 8).
const CLOCKS_PER_TICK: u32 = 8;
const TICKS_PER_SECOND: u32 = 4_233_600;
/// How long Pause's second response is waited for, in system clocks: 8 million,
/// a quarter of a second.
const INT2_LIMIT: u32 = 8_000_000;
/// How long the acknowledge is waited for.
const INT3_LIMIT: u32 = 2_000_000;
const INT2_LIMIT_TICKS: u32 = INT2_LIMIT / CLOCKS_PER_TICK;
const INT3_LIMIT_TICKS: u32 = INT3_LIMIT / CLOCKS_PER_TICK;
/// Longest wait for a drive to deliver sectors after ReadN: 6 seconds, which a
/// full-stroke seek on a worn disc does not reach.
const SECTOR_LIMIT_TICKS: u32 = 6 * TICKS_PER_SECOND;
/// Passes of any wait loop, far beyond what its clock deadline needs.
const PASS_LIMIT: u32 = 40_000_000;
/// Per-command spin budget of the setup helpers, as the rest of the suite.
const SPINS: u32 = 200_000;
/// GetStat spacing of the EARLY watch (150,000 clocks, 4.4 ms) and of the SPACED
/// watch, whose first GetStat is also one step in (400,000 clocks, 11.8 ms).
const EARLY_STEP: u32 = 150_000;
const SPACED_STEP: u32 = 400_000;
const WATCH_COMMANDS: u32 = 5;

/// The test region the disc build puts at LBA 564 (460 sectors), and a sector
/// far from it inside the data track (the long XA file's area).
const NEAR_LBA: u32 = 564;
const FAR_LBA: u32 = 15_000;
const SETTLED_SECTORS: u32 = 12;

const STATE_NAMES: [&str; STATES] = ["IDLE", "SETTLED", "FIRST", "SEEK", "FAR"];

// -------------------------------------------------------------------- clock

/// Timer 2 on system clock / 8, widened to 32 bits.
struct Clock {
    last: u16,
    high: u32,
}

impl Clock {
    fn start() -> Self {
        timers::set_mode(Timer::Timer2, 0x0200);
        timers::set_counter(Timer::Timer2, 0);
        Self { last: 0, high: 0 }
    }

    #[inline(never)]
    fn ticks(&mut self) -> u32 {
        let c = timers::counter(Timer::Timer2);
        if c < self.last {
            self.high = self.high.wrapping_add(1 << 16);
        }
        self.last = c;
        self.high | u32::from(c)
    }
}

// ------------------------------------------------------------ controller io

/// The first byte of the response (the drive status) and the response drained.
#[inline(never)]
fn take_response() -> u8 {
    // SAFETY: the polled helpers own the controller for the whole step; this
    // only pops the response FIFO.
    let mut cd = unsafe { Cd::steal() };
    let mut first = 0;
    for n in 0..16 {
        if cd.status_register() & psx_hw::cd::index_status::RESPONSE_NOT_EMPTY == 0 {
            break;
        }
        let byte = cd.read_response_byte();
        if n == 0 {
            first = byte;
        }
    }
    first
}

/// One flag the controller showed.
#[derive(Copy, Clone)]
struct Event {
    /// Clocks from the command to the flag being seen.
    clocks: u32,
    /// 1 sector, 2 complete, 3 acknowledge, 5 error, 7 gave up.
    flag: u8,
    status: u8,
    /// GetStat commands sent when it was seen.
    sent: u8,
}

impl Event {
    const NONE: Self = Self {
        clocks: 0,
        flag: 0,
        status: 0,
        sent: 0,
    };
}

/// What one Pause did.
#[derive(Copy, Clone)]
struct Trial {
    ack: Option<u32>,
    int2: Option<u32>,
    /// 0 completed, 1 no INT2 in time, 2 drive error, 3 no acknowledge.
    outcome: u8,
    sectors: u32,
    status: u8,
}

/// Acknowledge, count and discard whatever the controller shows until
/// `done(flag)` says stop, the deadline passes, or the pass limit is reached.
/// Returns how many sector flags went by and whether `done` ended it.
#[inline(never)]
fn drain_until(clock: &mut Clock, limit_ticks: u32, done: &mut dyn FnMut(u8, u8) -> bool) -> (u32, bool) {
    let start = clock.ticks();
    let mut sectors = 0u32;
    let mut passes = 0u32;
    let cap = bounds::scale(PASS_LIMIT);
    loop {
        passes += 1;
        if passes > cap || clock.ticks().wrapping_sub(start) > limit_ticks {
            return (sectors, false);
        }
        let flag = cdio::irq_flag_value();
        if flag == 0 {
            continue;
        }
        let status = take_response();
        cdio::acknowledge_irq(flag);
        if flag == 1 {
            sectors += 1;
        }
        if done(flag, status) {
            return (sectors, true);
        }
    }
}

/// Send Pause and time its two responses.
#[inline(never)]
fn timed_pause(clock: &mut Clock) -> Trial {
    let mut trial = Trial {
        ack: None,
        int2: None,
        outcome: 3,
        sectors: 0,
        status: 0,
    };
    let Some(saved) = cdio::dispatch_command(CMD_PAUSE, &[], SPINS) else {
        return trial;
    };
    let t0 = clock.ticks();
    let cap = bounds::scale(PASS_LIMIT);
    let mut passes = 0u32;
    trial.outcome = 3;
    loop {
        passes += 1;
        let elapsed = clock.ticks().wrapping_sub(t0);
        let limit = if trial.ack.is_some() {
            INT2_LIMIT_TICKS
        } else {
            INT3_LIMIT_TICKS
        };
        if passes > cap || elapsed > limit {
            if trial.ack.is_some() {
                trial.outcome = 1;
            }
            break;
        }
        let flag = cdio::irq_flag_value();
        if flag == 0 {
            continue;
        }
        let at = clock.ticks().wrapping_sub(t0);
        let status = take_response();
        cdio::acknowledge_irq(flag);
        match flag {
            1 => trial.sectors += 1,
            3 if trial.ack.is_none() => {
                trial.ack = Some(at * CLOCKS_PER_TICK);
                trial.outcome = 1;
            }
            2 if trial.ack.is_some() => {
                trial.int2 = Some(at * CLOCKS_PER_TICK);
                trial.status = status;
                trial.outcome = 0;
                break;
            }
            5 => {
                trial.status = status;
                trial.outcome = 2;
                break;
            }
            _ => {}
        }
    }
    cdio::restore_irq_output(saved);
    trial
}

/// Send Pause, then GetStat at intervals, and log every flag.
#[inline(never)]
fn watched_pause(
    clock: &mut Clock,
    events: &mut [Event; WATCH_EVENTS],
    early: bool,
) -> (u32, u32, bool) {
    let Some(saved) = cdio::dispatch_command(CMD_PAUSE, &[], SPINS) else {
        return (0, 0, false);
    };
    let t0 = clock.ticks();
    let cap = bounds::scale(PASS_LIMIT);
    let mut passes = 0u32;
    let mut seen = 0u32;
    let mut sent = 0u32;
    let step_ticks = (if early { EARLY_STEP } else { SPACED_STEP }) / CLOCKS_PER_TICK;
    // EARLY sends its first command when the acknowledge arrives; SPACED
    // counts from the Pause.
    let mut next_get = if early { u32::MAX } else { step_ticks };
    let mut acked = false;
    let mut int2 = false;
    loop {
        passes += 1;
        let elapsed = clock.ticks().wrapping_sub(t0);
        let limit = if acked {
            INT2_LIMIT_TICKS
        } else {
            INT3_LIMIT_TICKS
        };
        if passes > cap || elapsed > limit {
            break;
        }
        if acked && sent < WATCH_COMMANDS && elapsed >= next_get {
            // SAFETY: as `take_response`.
            let mut cd = unsafe { Cd::steal() };
            if cd.wait_parameter_room(SPINS) {
                cd.send_command_byte(CMD_GETSTAT);
                sent += 1;
                next_get = elapsed + step_ticks;
            } else {
                sent = WATCH_COMMANDS;
            }
        }
        let flag = cdio::irq_flag_value();
        if flag == 0 {
            continue;
        }
        let at = clock.ticks().wrapping_sub(t0);
        let status = take_response();
        cdio::acknowledge_irq(flag);
        if (seen as usize) < WATCH_EVENTS {
            events[seen as usize] = Event {
                clocks: at * CLOCKS_PER_TICK,
                flag,
                status,
                sent: sent as u8,
            };
        }
        seen += 1;
        if flag == 3 && !acked {
            acked = true;
            if early {
                next_get = at;
            }
        } else if flag == 2 {
            int2 = true;
            break;
        } else if flag == 5 {
            break;
        }
    }
    if (seen as usize) < WATCH_EVENTS {
        // The limit that ended the watch, as a flag of its own.
        let at = clock.ticks().wrapping_sub(t0);
        if !int2 {
            events[seen as usize] = Event {
                clocks: at * CLOCKS_PER_TICK,
                flag: 7,
                status: 0,
                sent: sent as u8,
            };
        }
    }
    cdio::restore_irq_output(saved);
    (seen, sent, int2)
}

// -------------------------------------------------------------- the states

fn mode_for(fast: bool) -> u8 {
    if fast {
        MODE_DOUBLE_SPEED
    } else {
        0
    }
}

/// The drive paused, at the speed under test.
#[inline(never)]
fn settle(fast: bool, fails: &mut u32) {
    if !cdio::try_pause_until_complete(SPINS) {
        *fails += 1;
    }
    if cdio::try_set_mode(mode_for(fast), SPINS).is_none() {
        *fails += 1;
    }
}

/// ReadN from `lba`, acknowledged (sectors start arriving afterwards).
#[inline(never)]
fn start_read(fast: bool, lba: u32, fails: &mut u32) -> bool {
    let ok = cdio::try_set_mode(mode_for(fast), SPINS).is_some()
        && cdio::try_set_target_lba(lba, SPINS).is_some()
        && cdio::try_start_reading(SPINS).is_some();
    if !ok {
        *fails += 1;
    }
    ok
}

/// Wait for `count` sectors; whether they came.
#[inline(never)]
fn wait_sectors(clock: &mut Clock, count: u32) -> bool {
    let mut got = 0u32;
    let (_, done) = drain_until(clock, SECTOR_LIMIT_TICKS, &mut |flag, _| {
        if flag == 1 {
            got += 1;
        }
        got >= count
    });
    done
}

/// Put the head back at the start of the test region and stop there, so the
/// next far read is a long seek.
#[inline(never)]
fn approach(clock: &mut Clock, fast: bool, fails: &mut u32) {
    if start_read(fast, NEAR_LBA, fails) && !wait_sectors(clock, 1) {
        *fails += 1;
    }
    settle(fast, fails);
}

/// Bring the drive to `state`, ready for Pause.
#[inline(never)]
fn arrange(clock: &mut Clock, state: usize, fast: bool, fails: &mut u32) -> bool {
    match state {
        0 => {
            settle(fast, fails);
            true
        }
        1 => {
            settle(fast, fails);
            let ok = start_read(fast, NEAR_LBA, fails);
            if ok && !wait_sectors(clock, SETTLED_SECTORS) {
                *fails += 1;
            }
            ok
        }
        2 => {
            settle(fast, fails);
            let ok = start_read(fast, NEAR_LBA, fails);
            if ok && !wait_sectors(clock, 1) {
                *fails += 1;
            }
            ok
        }
        3 => {
            approach(clock, fast, fails);
            // Pause goes out right after the ReadN acknowledge, mid-seek.
            start_read(fast, FAR_LBA, fails)
        }
        _ => {
            approach(clock, fast, fails);
            let ok = start_read(fast, FAR_LBA, fails);
            if ok && !wait_sectors(clock, 1) {
                *fails += 1;
            }
            ok
        }
    }
}

fn split(clocks: Option<u32>) -> (u32, u32) {
    match clocks {
        Some(value) => (value & 0xFFFF, value >> 16),
        None => (0xFFFF, 0xFFFF),
    }
}

// ----------------------------------------------------------------- the run

struct Progress<'a> {
    font: &'a FontAtlas,
    done: usize,
}

impl Progress<'_> {
    const TOTAL: usize = SPEEDS * STATES * TRIALS + WATCHES;

    fn step(&mut self, speed: usize, name: &str, number: usize) {
        let mut line = ui::Line::new();
        line.s(if speed == 0 { "PAUSE 1X " } else { "PAUSE 2X " })
            .s(name)
            .s(" ")
            .u(number as u32);
        ui::detail(self.font, "CD", line.as_str());
        ui::sub(self.done, Self::TOTAL);
        self.done += 1;
    }
}

/// How many clocks one pass of the wait loops takes.
#[inline(never)]
fn pass_cost() -> (u32, u32, u32) {
    let mut samples = [0u32; 3];
    for sample in samples.iter_mut() {
        let mut clock = Clock::start();
        let t0 = clock.ticks();
        for _ in 0..256 {
            let _ = cdio::irq_flag_value();
            let _ = clock.ticks();
        }
        *sample = clock.ticks().wrapping_sub(t0) * CLOCKS_PER_TICK / 256;
    }
    spread(&mut samples)
}

/// The whole case.
#[inline(never)]
pub(crate) fn run(font: &FontAtlas) -> [TimingRecord; CD_PAUSE_COUNT] {
    let mut out = [TimingRecord::pending(); CD_PAUSE_COUNT];
    let mut next = 0usize;
    let mut push = |rec: TimingRecord| {
        if next < CD_PAUSE_COUNT {
            out[next] = rec;
            next += 1;
        }
    };
    let mut progress = Progress { font, done: 0 };
    let mut fails = 0u32;
    let mut cleanup_fails = 0u32;
    let mut trials_run = 0u32;

    bounds::record_start(META_POLL);
    let (lo, med, hi) = pass_cost();
    push(record(META_POLL, lo, med, hi));

    let mut cells = [[TimingRecord::pending(); TRIALS + 1]; SPEEDS * STATES];
    for speed in 0..SPEEDS {
        let fast = speed == 1;
        for state in 0..STATES {
            let cell = speed * STATES + state;
            bounds::record_start(TRIAL_BASE + 4 * cell as u16);
            let mut acks = [0u32; TRIALS];
            let mut acked = 0usize;
            for trial in 0..TRIALS {
                progress.step(speed, STATE_NAMES[state], trial + 1);
                let mut clock = Clock::start();
                let ready = arrange(&mut clock, state, fast, &mut fails);
                let result = if ready {
                    timed_pause(&mut clock)
                } else {
                    Trial {
                        ack: None,
                        int2: None,
                        outcome: 3,
                        sectors: 0,
                        status: 0,
                    }
                };
                trials_run += 1;
                if let Some(ack) = result.ack {
                    acks[acked] = ack / 32;
                    acked += 1;
                }
                let (lo, hi) = split(result.int2);
                cells[cell][trial] = record(
                    TRIAL_BASE + (4 * cell + trial) as u16,
                    lo,
                    hi,
                    u32::from(result.outcome)
                        | (result.sectors.min(15) << 2)
                        | (u32::from(result.status) << 8),
                );
                // Whatever the trial left, the next one starts from a paused drive.
                if !cdio::try_pause_until_complete(SPINS) {
                    cleanup_fails += 1;
                }
            }
            let summary = if acked == 0 {
                (0xFFFF, 0xFFFF, 0xFFFF)
            } else {
                spread(&mut acks[..acked])
            };
            cells[cell][TRIALS] = record(
                TRIAL_BASE + (4 * cell + TRIALS) as u16,
                summary.0,
                summary.1,
                summary.2,
            );
        }
    }
    for cell in &cells {
        for rec in cell {
            push(*rec);
        }
    }

    for watch in 0..WATCHES {
        let fast = watch >= 2;
        let early = watch % 2 == 1;
        let id = WATCH_BASE + 16 * watch as u16;
        bounds::record_start(id);
        progress.step(
            usize::from(fast),
            if early { "WATCH EARLY" } else { "WATCH SPACED" },
            1,
        );
        let mut clock = Clock::start();
        let ready = arrange(&mut clock, 1, fast, &mut fails);
        let mut events = [Event::NONE; WATCH_EVENTS];
        let (seen, sent, int2) = if ready {
            watched_pause(&mut clock, &mut events, early)
        } else {
            (0, 0, false)
        };
        trials_run += 1;
        for (index, event) in events.iter().enumerate() {
            push(record(
                id + index as u16,
                event.clocks / 128,
                (u32::from(event.flag) << 8) | u32::from(event.status),
                u32::from(event.sent),
            ));
        }
        push(record(id + WATCH_EVENTS as u16, seen, sent, u32::from(int2)));
        if !cdio::try_pause_until_complete(SPINS) {
            cleanup_fails += 1;
        }
    }

    // Leave the drive paused at single speed, as the area expects.
    let _ = cdio::try_pause_until_complete(SPINS);
    let _ = cdio::try_set_mode(0, SPINS);

    push(record(
        META_LIMITS,
        INT2_LIMIT / 1024,
        INT3_LIMIT / 1024,
        trials_run,
    ));
    push(record(META_FAILS, fails, cleanup_fails, 0));
    out
}
