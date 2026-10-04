//! Root counter (timer) MMIO. Three 16-bit counters, each with counter /
//! mode / target registers. Mode bits select the clock source and IRQ
//! behaviour. Register addresses and the mode bits live in
//! [`psx_hw::timers`].

use psx_hw::timers as reg;

/// One of the three root counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Timer {
    /// Dot-clock or system-clock, HSync-gatable.
    Timer0 = 0,
    /// System-clock or HBlank, VSync-gatable.
    Timer1 = 1,
    /// System-clock or system-clock/8.
    Timer2 = 2,
}

impl Timer {
    #[inline(always)]
    const fn base(self) -> u32 {
        reg::block(self as u32)
    }
}

/// Current counter value (0..=65535).
#[inline(always)]
pub fn counter(t: Timer) -> u16 {
    // SAFETY: `t.base() + reg::COUNTER` is root counter n's current-value register for n in 0..=2
    // (the `Timer` discriminants): aligned MMIO on every PS1. Reading the counter has no side
    // effects.
    unsafe { crate::read_u32(t.base() + reg::COUNTER) as u16 }
}

/// Set the counter directly.
#[inline(always)]
pub fn set_counter(t: Timer, value: u16) {
    // SAFETY: root counter `t`'s current-value register (see `counter`); a write only reloads the
    // count.
    unsafe { crate::write_u32(t.base() + reg::COUNTER, value as u32) }
}

/// Mode / control register; the bits are [`psx_hw::timers::mode`]. Writing
/// it reconfigures the counter and resets it to 0.
#[inline(always)]
pub fn set_mode(t: Timer, mode: u16) {
    // SAFETY: `t.base() + reg::MODE` is root counter `t`'s mode register (see `counter`); a write
    // reconfigures and resets that counter only.
    unsafe { crate::write_u32(t.base() + reg::MODE, mode as u32) }
}

/// Read the mode register (includes the sticky "reached target" and
/// "reached wrap" bits, [`psx_hw::timers::mode::REACHED_TARGET`] and
/// [`psx_hw::timers::mode::REACHED_WRAP`]).
#[inline(always)]
pub fn mode(t: Timer) -> u16 {
    // SAFETY: `t.base() + reg::MODE` is root counter `t`'s mode register (see `counter`); the read
    // clears the sticky reached bits and touches no memory.
    unsafe { crate::read_u32(t.base() + reg::MODE) as u16 }
}

/// Target value for `reset-on-target` mode.
#[inline(always)]
pub fn set_target(t: Timer, value: u16) {
    // SAFETY: `t.base() + reg::TARGET` is root counter `t`'s target register (see `counter`).
    unsafe { crate::write_u32(t.base() + reg::TARGET, value as u32) }
}
