//! Root counter (timer) MMIO. Three 16-bit counters at `0x1F80_1100`,
//! `0x1F80_1110`, `0x1F80_1120`, each with counter / mode / target
//! registers. Mode bits select the clock source and IRQ behaviour.

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
        0x1F80_1100 + 0x10 * (self as u32)
    }
}

/// Current counter value (0..=65535).
#[inline(always)]
pub fn counter(t: Timer) -> u16 {
    // SAFETY: `t.base()` is 0x1F80_1100 + 0x10 * n for n in 0..=2 (the `Timer` discriminants), so
    // it and the +0x4 / +0x8 offsets used below are root counter n's counter, mode and target
    // registers: aligned MMIO on every PS1. Reading the counter has no side effects.
    unsafe { crate::read_u32(t.base()) as u16 }
}

/// Set the counter directly.
#[inline(always)]
pub fn set_counter(t: Timer, value: u16) {
    // SAFETY: root counter `t`'s counter register (see `counter`); a write only reloads the count.
    unsafe { crate::write_u32(t.base(), value as u32) }
}

/// Mode / control register. See PSX-SPX for bit details; key bits:
///   0    sync enable
///   1..2 sync mode (source-dependent)
///   3    reset counter when target reached (else wrap at 0xFFFF)
///   4    IRQ on target
///   5    IRQ on wrap
///   6    repeat IRQ
///   7    toggle IRQ flag (else one-shot active-low)
///   8..9 clock source
///   10   IRQ (active-low; reading latches, writes clear)
#[inline(always)]
pub fn set_mode(t: Timer, mode: u16) {
    // SAFETY: `t.base() + 0x4` is root counter `t`'s mode register (see `counter`); a write
    // reconfigures and resets that counter only.
    unsafe { crate::write_u32(t.base() + 0x4, mode as u32) }
}

/// Read the mode register (includes "reached target" / "reached wrap"
/// sticky bits 11 / 12).
#[inline(always)]
pub fn mode(t: Timer) -> u16 {
    // SAFETY: `t.base() + 0x4` is root counter `t`'s mode register (see `counter`); the read clears
    // the sticky reached bits and touches no memory.
    unsafe { crate::read_u32(t.base() + 0x4) as u16 }
}

/// Target value for `reset-on-target` mode.
#[inline(always)]
pub fn set_target(t: Timer, value: u16) {
    // SAFETY: `t.base() + 0x8` is root counter `t`'s target register (see `counter`).
    unsafe { crate::write_u32(t.base() + 0x8, value as u32) }
}
