// SPDX-License-Identifier: GPL-2.0-or-later
//! Timer 1 on the HBlank clock: how many counts a frame holds when nobody
//! reads it, and when the CPU reads it as fast as it can.
//!
//! The question behind it: does reading the counter in a tight loop change
//! what it counts, or what a reader sees? Both windows are 120 frames long,
//! bounded by the run's own VBlank counter, so the two totals are directly
//! comparable. Nothing here passes or fails.
//!
//! `tick_loss` is the same question for the other counters: Timer 2 and
//! Timer 0 on the system clock, Timer 2 at a eighth of it and Timer 0 on the
//! dot clock, each read in a tight loop for 30 frames while Timer 1 on the
//! HBlank clock runs free as the reference (read at the two ends only). The
//! ratio of ticks to lines is the rate the reader saw; a reader that loses
//! ticks while it polls shows as a ratio below the unpolled one.

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

/// rec timer_poll_rate: ticks_per_hblank_x16, hblanks_in_window, largest_step_between_reads (Timer 2 sysclk 0x6B0, Timer 2 sysclk/8 0x6B2, Timer 0 sysclk 0x6B4, Timer 0 dot clock 0x6B6)
const POLL_RATE_RECORD: u16 = 0x6B0;
/// rec timer_poll_reads: reads_low, reads_high, steps_larger_than_64 (0x6B1 0x6B3 0x6B5 0x6B7)
const POLL_READS_RECORD: u16 = 0x6B1;
const TICK_LOSS_FRAMES: u32 = 30;

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

/// Tight-poll each counter and compare its ticks with the free-running Timer
/// 1 HBlank count over the same 30 frames.
pub(crate) fn tick_loss(records: &mut Records, next: &mut usize) {
    // (timer, mode): the system clock, a eighth of it, and Timer 0's dot clock.
    let variants = [
        (Timer::Timer2, 0x0000u16),
        (Timer::Timer2, 0x0200),
        (Timer::Timer0, 0x0000),
        (Timer::Timer0, 0x0100),
    ];
    for (index, (timer, mode)) in variants.into_iter().enumerate() {
        timers::set_mode(timer, mode);
        timers::set_counter(timer, 0);
        timers::set_mode(Timer::Timer1, HBLANK_CLOCK);
        timers::set_counter(Timer::Timer1, 0);
        let _ = wait_edge();
        let v0 = interrupts::vblank_count();
        let h0 = timers::counter(Timer::Timer1);
        let mut last = timers::counter(timer);
        let (mut total, mut largest, mut big) = (0u32, 0u32, 0u32);
        let mut reads = 0u32;
        let mut spins = 0u32;
        while interrupts::vblank_count().wrapping_sub(v0) < TICK_LOSS_FRAMES && spins < SPIN_LIMIT {
            let now = timers::counter(timer);
            reads += 1;
            spins += 1;
            let step = now.wrapping_sub(last) as u32;
            if step != 0 {
                total += step;
                largest = largest.max(step);
                if step > 64 {
                    big += 1;
                }
                last = now;
            }
        }
        let hblanks = timers::counter(Timer::Timer1).wrapping_sub(h0) as u32;
        let per_line = (total * 16).checked_div(hblanks).unwrap_or(0xFFFF);
        let base = 2 * index as u16;
        push_timing_record(
            records,
            next,
            record(POLL_RATE_RECORD + base, per_line, hblanks, largest),
        );
        push_timing_record(
            records,
            next,
            record(POLL_READS_RECORD + base, reads & 0xFFFF, reads >> 16, big),
        );
        timers::set_mode(timer, 0);
    }
    timers::set_mode(Timer::Timer1, 0);
}
