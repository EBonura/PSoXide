//! Root counters (timers): addresses and mode-register bits.
//!
//! Three 16-bit counters, each with a counter, a mode and a target register
//! in a 16-byte block. Which clock a counter counts and what its sync modes
//! mean differ per counter; [`mode::clock_source`] and [`mode::sync_mode`]
//! only place the two-bit fields.
//!
//! Reference: nocash PSX-SPX "Timers".

/// Number of root counters.
pub const COUNT: u32 = 3;

/// Register block of counter 0; counter `n` is at `BASE + n * STRIDE`.
pub const BASE: u32 = 0x1F80_1100;
/// Distance between two counters' register blocks.
pub const STRIDE: u32 = 0x10;

/// Offset of the current-value register in a counter block (16-bit value in
/// a 32-bit register).
#[doc(alias = "T_CURRENT")]
pub const COUNTER: u32 = 0x0;
/// Offset of the mode register in a counter block.
#[doc(alias = "T_MODE")]
pub const MODE: u32 = 0x4;
/// Offset of the target-value register in a counter block.
#[doc(alias = "T_TARGET")]
pub const TARGET: u32 = 0x8;

/// Address of counter `n`'s register block (`n` in `0..COUNT`).
#[inline(always)]
pub const fn block(n: u32) -> u32 {
    BASE + STRIDE * n
}

/// Mode-register bits and fields (16-bit view of the register).
pub mod mode {
    /// Bit 0: synchronise the counter with its gate signal (HBlank for
    /// counter 0, VBlank for counter 1; counter 2 stops or free-runs).
    pub const SYNC_ENABLE: u16 = 1 << 0;
    /// Bits 1..=2: what the gate signal does, see [`sync_mode`].
    pub const SYNC_MODE_MASK: u16 = 0b11 << 1;
    /// Bit 3: reset the counter to 0 when it reaches the target (else it
    /// wraps at `0xFFFF`).
    pub const RESET_AT_TARGET: u16 = 1 << 3;
    /// Bit 4: raise an IRQ when the counter reaches the target.
    pub const IRQ_AT_TARGET: u16 = 1 << 4;
    /// Bit 5: raise an IRQ when the counter wraps past `0xFFFF`.
    pub const IRQ_AT_WRAP: u16 = 1 << 5;
    /// Bit 6: raise the IRQ every time (else once after the mode write).
    pub const IRQ_REPEAT: u16 = 1 << 6;
    /// Bit 7: toggle the IRQ line (else pulse it).
    pub const IRQ_TOGGLE: u16 = 1 << 7;
    /// Bits 8..=9: clock source, see [`clock_source`].
    pub const CLOCK_SOURCE_MASK: u16 = 0b11 << 8;
    /// Bit 10, read only: clear while an IRQ is requested (active low).
    pub const IRQ_NOT_REQUESTED: u16 = 1 << 10;
    /// Bit 11, read only: the counter has reached the target since the last
    /// read of this register.
    pub const REACHED_TARGET: u16 = 1 << 11;
    /// Bit 12, read only: the counter has reached `0xFFFF` since the last
    /// read of this register.
    pub const REACHED_WRAP: u16 = 1 << 12;

    /// Place a sync-mode number (0..=3) in bits 1..=2.
    #[inline(always)]
    pub const fn sync_mode(n: u16) -> u16 {
        (n & 0b11) << 1
    }

    /// Place a clock-source number (0..=3) in bits 8..=9.
    ///
    /// Counter 0: 0 or 2 system clock, 1 or 3 dot clock. Counter 1: 0 or 2
    /// system clock, 1 or 3 HBlank. Counter 2: 0 or 1 system clock, 2 or 3
    /// system clock / 8.
    #[inline(always)]
    pub const fn clock_source(n: u16) -> u16 {
        (n & 0b11) << 8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_blocks_follow_the_documented_addresses() {
        assert_eq!(
            [block(0), block(1), block(2)],
            [0x1F80_1100, 0x1F80_1110, 0x1F80_1120]
        );
        assert_eq!(block(2) + MODE, 0x1F80_1124);
        assert_eq!(block(1) + TARGET, 0x1F80_1118);
    }

    #[test]
    fn mode_fields_land_on_their_bits() {
        // Counter 1 counting HBlanks, reset at VBlank: the word the SDK's
        // frame-line counter has always written.
        let hblank_reset_at_vblank = mode::SYNC_ENABLE | mode::sync_mode(1) | mode::clock_source(1);
        assert_eq!(hblank_reset_at_vblank, 0x0103);
        // Counter 2 on system clock / 8, free-running.
        assert_eq!(mode::clock_source(2), 0x0200);
        assert_eq!(mode::sync_mode(7), mode::SYNC_MODE_MASK);
        assert_eq!(mode::clock_source(5), 0x0100);
    }
}
