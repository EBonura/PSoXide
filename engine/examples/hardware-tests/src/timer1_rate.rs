// SPDX-License-Identifier: GPL-2.0-or-later
//! Timer 1 on the HBlank clock: how many counts a frame holds when nobody
//! reads it, and when the CPU reads it as fast as it can.
//!
//! The question behind it: does reading the counter in a tight loop change
//! what it counts, or what a reader sees? Both windows are 120 frames long,
//! bounded by the run's own VBlank counter, so the two totals are directly
//! comparable. Nothing here passes or fails.

use crate::console_tests::record;
use crate::{push_timing_record, TimingRecord, TIMING_RECORD_COUNT};
use psx_io::timers::{self, Timer};
use psx_rt::interrupts;

type Records = [TimingRecord; TIMING_RECORD_COUNT];

/// rec timer1_free: hblanks_in_window, frames_in_window, spins_without_vblank (nobody reads the counter, 0x650)
const FREE_RECORD: u16 = 0x650;
/// rec timer1_polled: hblanks_in_window, distinct_values_seen, largest_step_between_reads (tight read loop, 0x651)
const POLLED_RECORD: u16 = 0x651;
/// rec timer1_polled_reads: reads_low, reads_high, steps_larger_than_one (tight read loop, 0x652)
const POLLED_READS_RECORD: u16 = 0x652;

/// Timer 1 mode: bit 8 selects the HBlank clock.
const HBLANK_CLOCK: u16 = 0x0100;
const FRAMES: u32 = 120;
/// Give a window up after this many spins if no VBlank arrives.
const SPIN_LIMIT: u32 = 400_000_000;

fn wait_edge() -> bool {
    let start = interrupts::vblank_count();
    let mut spins = 0u32;
    while interrupts::vblank_count() == start {
        spins += 1;
        if spins > SPIN_LIMIT {
            return false;
        }
    }
    true
}

pub(crate) fn run(records: &mut Records, next: &mut usize) {
    crate::bounds::record_start(FREE_RECORD);
    timers::set_mode(Timer::Timer1, HBLANK_CLOCK);
    timers::set_counter(Timer::Timer1, 0);

    // Free-running: read at the two ends only.
    let edge = wait_edge();
    let v0 = interrupts::vblank_count();
    let t0 = timers::counter(Timer::Timer1);
    let mut spins = 0u32;
    while interrupts::vblank_count().wrapping_sub(v0) < FRAMES && spins < SPIN_LIMIT {
        spins += 1;
    }
    let frames = interrupts::vblank_count().wrapping_sub(v0);
    let t1 = timers::counter(Timer::Timer1);
    let free = t1.wrapping_sub(t0) as u32;
    push_timing_record(
        records,
        next,
        record(FREE_RECORD, free, frames, if edge { 0 } else { 1 }),
    );

    // Polled: the CPU reads the counter every few cycles for the whole window.
    let _ = wait_edge();
    let v0 = interrupts::vblank_count();
    let mut last = timers::counter(Timer::Timer1);
    let (mut total, mut distinct, mut largest, mut jumps) = (0u32, 0u32, 0u32, 0u32);
    let mut reads = 0u32;
    let mut spins = 0u32;
    while interrupts::vblank_count().wrapping_sub(v0) < FRAMES && spins < SPIN_LIMIT {
        let now = timers::counter(Timer::Timer1);
        reads += 1;
        spins += 1;
        if now != last {
            let step = now.wrapping_sub(last) as u32;
            total += step;
            distinct += 1;
            largest = largest.max(step);
            if step > 1 {
                jumps += 1;
            }
            last = now;
        }
    }
    push_timing_record(
        records,
        next,
        record(POLLED_RECORD, total, distinct, largest),
    );
    push_timing_record(
        records,
        next,
        record(POLLED_READS_RECORD, reads & 0xFFFF, reads >> 16, jumps),
    );
    timers::set_mode(Timer::Timer1, 0);
}
