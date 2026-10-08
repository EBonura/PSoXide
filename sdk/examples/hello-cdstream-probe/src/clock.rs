//! Wall time for the measurements: Timer 1 counting HBlanks.
//!
//! The probe sets the display to NTSC 320x240, 263 lines a frame at
//! 59.826 Hz, so one HBlank is 63.56 microseconds and 15 734 of them make a
//! second. The counter is 16 bits (it wraps after 4.1 s); [`now`] widens it
//! by noticing each wrap, so it must be called at least once every four
//! seconds while a measurement runs, which every wait loop does.

use psx_io::timers::{self, Timer};

/// HBlanks per second in the probe's video mode.
pub const HZ: u32 = 15_734;

static mut LAST: u16 = 0;
static mut HIGH: u16 = 0;

/// Start Timer 1 counting HBlanks.
pub fn init() {
    timers::set_mode(Timer::Timer1, psx_hw::timers::mode::clock_source(1));
    // SAFETY: single thread of control; the statics are only touched here
    // and in `now`.
    unsafe {
        LAST = 0;
        HIGH = 0;
    }
}

/// HBlanks since [`init`], as a 32-bit count.
pub fn now() -> u32 {
    let c = timers::counter(Timer::Timer1);
    // SAFETY: as `init`.
    unsafe {
        if c < LAST {
            HIGH = HIGH.wrapping_add(1);
        }
        LAST = c;
        (u32::from(HIGH) << 16) | u32::from(c)
    }
}

/// HBlanks since `start`.
pub fn since(start: u32) -> u32 {
    now().wrapping_sub(start)
}

/// Milliseconds in tenths, rounded: `hblanks * 63.556 us`.
pub fn ms10(hblanks: u32) -> u32 {
    (hblanks.saturating_mul(6356) + 5000) / 10_000
}

/// Whole milliseconds, rounded.
pub fn ms(hblanks: u32) -> u32 {
    (ms10(hblanks) + 5) / 10
}

/// Microseconds of a Timer 2 count (system clock / 8, 4 233 600 ticks a
/// second).
pub fn timer2_us(ticks: u32) -> u32 {
    ticks.saturating_mul(10_000) / 42_336
}
