//! Interrupt controller: pending and enabled sources.
//!
//! The register addresses and source bit positions live in
//! [`psx_hw::irq`].

use psx_hw::irq as reg;

/// Pending interrupt sources, one bit per [`psx_hw::irq::source`] position.
#[doc(alias = "I_STAT")]
#[inline(always)]
pub fn pending() -> u32 {
    // SAFETY: I_STAT (0x1F80_1070) is the interrupt controller's aligned 32-bit pending register on
    // every PS1; reading it has no side effects.
    unsafe { crate::read_u32(reg::I_STAT) }
}

/// Enabled interrupt sources (the mask register).
#[doc(alias = "I_MASK")]
#[inline(always)]
pub fn mask() -> u32 {
    // SAFETY: I_MASK (0x1F80_1074) is the interrupt controller's aligned 32-bit mask register on
    // every PS1; reading it has no side effects.
    unsafe { crate::read_u32(reg::I_MASK) }
}

/// Acknowledge pending bits by writing `!(bits)` -- the hardware
/// AND-accumulates, so any bit left 1 in the written value is
/// preserved, any bit that was 0 is cleared.
#[inline(always)]
pub fn acknowledge(bits: u32) {
    // SAFETY: an aligned 32-bit write to I_STAT (0x1F80_1070). The hardware ANDs the value in, so
    // it can only clear pending bits.
    unsafe { crate::write_u32(reg::I_STAT, !bits) }
}

/// Set the mask register (who can interrupt the CPU).
#[inline(always)]
pub fn set_mask(bits: u32) {
    // SAFETY: an aligned 32-bit write to I_MASK (0x1F80_1074). It only selects which sources raise
    // the CPU interrupt line and touches no memory.
    unsafe { crate::write_u32(reg::I_MASK, bits) }
}

/// Renamed to [`pending`].
#[deprecated(note = "renamed to `pending`")]
#[inline(always)]
pub fn stat() -> u32 {
    pending()
}

/// Renamed to [`acknowledge`].
#[deprecated(note = "renamed to `acknowledge`")]
#[inline(always)]
pub fn ack(bits: u32) {
    acknowledge(bits)
}

/// Moved to [`psx_hw::irq::I_STAT`].
#[deprecated(note = "moved to `psx_hw::irq::I_STAT`")]
pub const I_STAT: u32 = reg::I_STAT;

/// Moved to [`psx_hw::irq::I_MASK`].
#[deprecated(note = "moved to `psx_hw::irq::I_MASK`")]
pub const I_MASK: u32 = reg::I_MASK;

/// Moved to [`psx_hw::irq::source`].
pub mod source {
    use psx_hw::irq::source as bit;

    /// Moved to [`psx_hw::irq::source::VBLANK`].
    #[deprecated(note = "moved to `psx_hw::irq::source::VBLANK`")]
    pub const VBLANK: u32 = bit::VBLANK;
    /// Moved to [`psx_hw::irq::source::GPU`].
    #[deprecated(note = "moved to `psx_hw::irq::source::GPU`")]
    pub const GPU: u32 = bit::GPU;
    /// Moved to [`psx_hw::irq::source::CDROM`].
    #[deprecated(note = "moved to `psx_hw::irq::source::CDROM`")]
    pub const CDROM: u32 = bit::CDROM;
    /// Moved to [`psx_hw::irq::source::DMA`].
    #[deprecated(note = "moved to `psx_hw::irq::source::DMA`")]
    pub const DMA: u32 = bit::DMA;
    /// Moved to [`psx_hw::irq::source::TIMER0`].
    #[deprecated(note = "moved to `psx_hw::irq::source::TIMER0`")]
    pub const TIMER0: u32 = bit::TIMER0;
    /// Moved to [`psx_hw::irq::source::TIMER1`].
    #[deprecated(note = "moved to `psx_hw::irq::source::TIMER1`")]
    pub const TIMER1: u32 = bit::TIMER1;
    /// Moved to [`psx_hw::irq::source::TIMER2`].
    #[deprecated(note = "moved to `psx_hw::irq::source::TIMER2`")]
    pub const TIMER2: u32 = bit::TIMER2;
    /// Moved to [`psx_hw::irq::source::CONTROLLER`].
    #[deprecated(note = "moved to `psx_hw::irq::source::CONTROLLER`")]
    pub const CONTROLLER: u32 = bit::CONTROLLER;
    /// Moved to [`psx_hw::irq::source::SIO1`].
    #[deprecated(note = "moved to `psx_hw::irq::source::SIO1`")]
    pub const SIO1: u32 = bit::SIO1;
    /// Moved to [`psx_hw::irq::source::SPU`].
    #[deprecated(note = "moved to `psx_hw::irq::source::SPU`")]
    pub const SPU: u32 = bit::SPU;
    /// Moved to [`psx_hw::irq::source::LIGHTPEN`].
    #[deprecated(note = "moved to `psx_hw::irq::source::LIGHTPEN`")]
    pub const LIGHTPEN: u32 = bit::LIGHTPEN;
}
