// SPDX-License-Identifier: GPL-2.0-or-later
//! M11 of the timing audit: polled-counter tick loss.
//!
//! A counter read in a tight loop should still count every tick it counts when
//! nobody reads it. The emulator once lost ticks while a loop polled (about
//! 2.3 clocks elapsed per tick for a 7-clock loop); a console has never been
//! measured. Each case polls one counter in an assembly loop that adds up the
//! 16-bit steps it sees, until the sum reaches a target worth about a second,
//! while Timer 1 on the HBlank clock runs free as the reference: it is read
//! before and after and never inside the loop. Lost fraction is 1 minus
//! ticks seen over (reference lines times clocks a line in that counter's
//! unit); the record carries the three numbers to compute it with.
//!
//! The loops are 6 and 14 instructions: `lw`, the add of the previous step,
//! `subu`, `andi`, and the branch on the running sum with the delay slot
//! moving the last value; the long one pads eight nops. Interrupts are masked
//! for the whole case (a second or so), so the VBlank counter stands still
//! meanwhile. The target is about one second rather than a second and a half
//! so that Timer 1 (65,536 lines is 4.2 s) cannot wrap even when a counter
//! loses most of its ticks. A counter that does not advance at all is skipped
//! up front rather than waited for.
//!
//! The reference is Timer 1 for every case. Timer 0 would have to serve for
//! Timer 2 at an eighth and for the dot clock in the audit's wording, but a
//! 16-bit system-clock Timer 0 wraps every 1.9 ms, so it cannot count a
//! second read at its two ends; Timer 1 on HBlank can.

use crate::console_tests::record;
use crate::{push_timing_record, IrqGuard, TimingRecord, TIMING_RECORD_COUNT};
use psx_io::timers::{self, Timer};

type Records = [TimingRecord; TIMING_RECORD_COUNT];

/// rec tick_loss: reference_hblanks, ticks_seen_kilo, ticks_per_hblank_x16 (Timer 2 sysclk 6 and 14 instructions 0x6B0 0x6B1, Timer 0 sysclk 0x6B2 0x6B3, Timer 2 sysclk/8 0x6B4 0x6B5, Timer 0 dot clock 0x6B6 0x6B7)
const TICK_LOSS_RECORD: u16 = 0x6B0;

const NONE: u32 = 0xFFFF;
const HBLANK_CLOCK: u16 = 0x0100;

macro_rules! poll_loop {
    ($name:ident, $id:literal, $padding:literal) => {
        /// Poll the counter at `address` until the 16-bit steps seen add up to
        /// `target`; returns the sum.
        #[inline(never)]
        fn $name(address: u32, target: u32) -> u32 {
            let seen: u32;
            // SAFETY: reads of a timer register and register arithmetic.
            unsafe {
                core::arch::asm!(
                    concat!(
                        ".set noreorder\n",
                        ".balign 16\n",
                        ".word 0x34000000 | (", stringify!($id), " << 1)\n",
                        "lw $9, 0($8)\n",
                        "nop\n",
                        "move $12, $zero\n",
                        "subu $11, $zero, $13\n",
                        "2:\n",
                        "lw $10, 0($8)\n",
                        "addu $11, $11, $12\n",
                        "subu $12, $10, $9\n",
                        "andi $12, $12, 0xFFFF\n",
                        $padding,
                        "bltz $11, 2b\n",
                        "move $9, $10\n",
                        "addu $2, $11, $13\n",
                        ".word 0x34000001 | (", stringify!($id), " << 1)\n",
                        ".set reorder"
                    ),
                    in("$8") address,
                    in("$13") target,
                    lateout("$2") seen,
                    lateout("$9") _,
                    lateout("$10") _,
                    lateout("$11") _,
                    lateout("$12") _,
                    options(nostack)
                );
            }
            seen
        }
    };
}

poll_loop!(poll_short_timer2, 240, "");
poll_loop!(poll_long_timer2, 241, ".rept 8\nnop\n.endr\n");
poll_loop!(poll_short_timer0, 242, "");
poll_loop!(poll_long_timer0, 243, ".rept 8\nnop\n.endr\n");
poll_loop!(poll_short_div8, 244, "");
poll_loop!(poll_long_div8, 245, ".rept 8\nnop\n.endr\n");
poll_loop!(poll_short_dot, 246, "");
poll_loop!(poll_long_dot, 247, ".rept 8\nnop\n.endr\n");

type Poll = fn(u32, u32) -> u32;

/// Whether the counter moves at all.
fn ticking(timer: Timer) -> bool {
    let first = timers::counter(timer);
    for _ in 0..4_000 {
        if timers::counter(timer) != first {
            return true;
        }
    }
    false
}

fn case(
    id: u16,
    timer: Timer,
    mode: u16,
    target: u32,
    poll: Poll,
    records: &mut Records,
    next: &mut usize,
) {
    timers::set_mode(timer, mode);
    timers::set_counter(timer, 0);
    timers::set_mode(Timer::Timer1, HBLANK_CLOCK);
    timers::set_counter(Timer::Timer1, 0);
    let address = match timer {
        Timer::Timer0 => 0x1F80_1100,
        Timer::Timer1 => 0x1F80_1110,
        Timer::Timer2 => 0x1F80_1120,
    };
    if !ticking(timer) || !ticking(Timer::Timer1) {
        push_timing_record(records, next, record(id, NONE, NONE, NONE));
        timers::set_mode(timer, 0);
        return;
    }
    let guard = IrqGuard::mask();
    let before = timers::counter(Timer::Timer1);
    let seen = poll(address, target);
    let after = timers::counter(Timer::Timer1);
    drop(guard);
    let lines = after.wrapping_sub(before) as u32;
    let per_line = seen.wrapping_mul(16) / lines.max(1);
    push_timing_record(records, next, record(id, lines, seen >> 10, per_line));
    timers::set_mode(timer, 0);
}

pub(crate) fn run(records: &mut Records, next: &mut usize) {
    let sys = 33_868_800;
    let eighth = sys / 8;
    let dot = 6_700_000;
    let cases: [(Timer, u16, u32, Poll, Poll); 4] = [
        (
            Timer::Timer2,
            0x0000,
            sys,
            poll_short_timer2,
            poll_long_timer2,
        ),
        (
            Timer::Timer0,
            0x0000,
            sys,
            poll_short_timer0,
            poll_long_timer0,
        ),
        (
            Timer::Timer2,
            0x0200,
            eighth,
            poll_short_div8,
            poll_long_div8,
        ),
        (Timer::Timer0, 0x0100, dot, poll_short_dot, poll_long_dot),
    ];
    for (index, (timer, mode, target, short, long)) in cases.into_iter().enumerate() {
        let id = TICK_LOSS_RECORD + 2 * index as u16;
        case(id, timer, mode, target, short, records, next);
        case(id + 1, timer, mode, target, long, records, next);
    }
}
