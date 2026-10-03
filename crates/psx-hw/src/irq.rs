//! Interrupt controller registers and source bits.
//!
//! Reference: nocash PSX-SPX "Interrupts".

/// Pending interrupt set. Write-to-ack: writing 0 to a bit clears it
/// (writing 1 preserves).
pub const I_STAT: u32 = 0x1F80_1070;

/// Interrupt enable set. Straight write.
pub const I_MASK: u32 = 0x1F80_1074;

/// Source bit positions inside [`I_STAT`] and [`I_MASK`].
pub mod source {
    /// VBlank.
    pub const VBLANK: u32 = 0;
    /// GPU (GP0 1Fh).
    pub const GPU: u32 = 1;
    /// CD-ROM controller.
    pub const CDROM: u32 = 2;
    /// DMA completion.
    pub const DMA: u32 = 3;
    /// Root counter 0.
    pub const TIMER0: u32 = 4;
    /// Root counter 1.
    pub const TIMER1: u32 = 5;
    /// Root counter 2.
    pub const TIMER2: u32 = 6;
    /// Controller / memory card (SIO0 byte received).
    pub const CONTROLLER: u32 = 7;
    /// SIO1 (debug serial).
    pub const SIO1: u32 = 8;
    /// SPU.
    pub const SPU: u32 = 9;
    /// Lightpen / controller IRQ10 line.
    pub const LIGHTPEN: u32 = 10;
}
