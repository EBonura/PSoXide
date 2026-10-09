//! `pollbench` -- what one controller poll costs, in CPU cycles.
//!
//! Polls port 1, then port 2, then both back to back, `POLLS` times each, and
//! prints the cycles per poll for every phase on the TTY. Root counter 2 runs
//! at the system clock divided by eight and is read around each poll, so the
//! numbers are the guest's own and need no profiler. A game that polls both
//! ports every frame pays the third figure per frame.
//!
//! Run headless with `frontend launch --path pollbench.exe --steps 400000000`
//! and read the `pollbench:` lines. An empty port 2 is the headless default;
//! the cost of an absent socket is the figure this exists to watch.
#![no_std]
#![no_main]
#![allow(deprecated)]

extern crate psx_rt;

use psx_io::timers::{self, Timer};
use psx_rt::tty;

/// Number of polls per phase.
const POLLS: u32 = 200;

/// Mode bit 9 selects system clock / 8 for counter 2.
const T2_SYSCLK_DIV8: u16 = 1 << 9;

fn now() -> u32 {
    u32::from(timers::counter(Timer::Timer2))
}

/// Counter ticks (8 CPU cycles each) spent in `body`, over `POLLS` calls.
fn ticks(mut body: impl FnMut()) -> u32 {
    let mut total = 0u32;
    let mut n = 0;
    while n < POLLS {
        let start = now();
        body();
        total += now().wrapping_sub(start) & 0xFFFF;
        n += 1;
    }
    total
}

fn report(label: &str, total_ticks: u32) {
    tty::print("pollbench: ");
    tty::print(label);
    tty::print(" cycles_x_polls=0x");
    tty::print_hex_u32(total_ticks.wrapping_mul(8));
    tty::print(" polls=0x");
    tty::print_hex_u32(POLLS);
    tty::print("\n");
}

#[no_mangle]
fn main() {
    timers::set_mode(Timer::Timer2, T2_SYSCLK_DIV8);
    report(
        "port1",
        ticks(|| {
            let _ = psx_pad::poll_port1();
        }),
    );
    report(
        "port2",
        ticks(|| {
            let _ = psx_pad::poll_port2();
        }),
    );
    report(
        "both",
        ticks(|| {
            let _ = psx_pad::poll_port1();
            let _ = psx_pad::poll_port2();
        }),
    );
    tty::println("pollbench: done");
    loop {}
}
