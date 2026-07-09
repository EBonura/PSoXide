//! Serial I/O: controller / memory-card port (`SIO0`) and debug serial (`SIO1`).
//!
//! `SIO0` is the interface to the two controller ports and two memory-card
//! slots. `SIO1` is a more conventional async serial port used mostly by
//! debugging dev-kits; absent on retail cables.
//!
//! Still to be populated: controller protocol framing, memory-card protocol
//! framing.
//!
//! Reference: nocash PSX-SPX "Controllers / Memory Cards" section.

/// `SIO0` register base: controllers and memory cards.
pub const SIO0_BASE: u32 = 0x1F80_1040;

/// `SIO1` register base: debug / standard serial.
pub const SIO1_BASE: u32 = 0x1F80_1050;

/// `SIO0` register bit layouts and standard mode/baud values, shared by every
/// SIO0 device driver (pads in `psx-pad`, memory cards in `psx-mc`) and usable
/// by the emulator's SIO model. Register *addresses* live with the MMIO
/// accessors in `psx-io`; this module owns only the layout facts.
pub mod sio0 {
    /// `JOY_CTRL` (u16) bits.
    pub mod ctrl {
        /// TX enable.
        pub const TXEN: u16 = 1 << 0;
        /// `/JOYn` select (DTR) output: hold low to address the selected port.
        pub const DTR: u16 = 1 << 1;
        /// RX enable (receive while selected).
        pub const RXEN: u16 = 1 << 2;
        /// Write-1 to acknowledge / clear the latched SIO IRQ ([`super::stat::IRQ`]).
        pub const ACK: u16 = 1 << 4;
        /// Enable IRQ7 generation from the device `/ACK` (DSR) pulse. Even with
        /// the CPU interrupt masked in `I_MASK`, enabling this is what latches
        /// [`super::stat::IRQ`] on the `/ACK` edge, so a brief pulse cannot be
        /// missed by a polling driver.
        pub const ACK_IRQ_EN: u16 = 1 << 12;
        /// Address port 2 instead of port 1.
        pub const SLOT_PORT2: u16 = 1 << 13;
    }

    /// `JOY_STAT` (u32) bits.
    pub mod stat {
        /// TX FIFO ready for a new byte.
        pub const TX_READY: u32 = 1 << 0;
        /// RX FIFO holds at least one byte.
        pub const RX_NOT_EMPTY: u32 = 1 << 1;
        /// Live `/ACK` (DSR) input level: 1 while the device holds it asserted.
        pub const DSR_LEVEL: u32 = 1 << 7;
        /// Latched DSR/ACK interrupt: set on the `/ACK` edge, held until
        /// [`super::ctrl::ACK`] clears it.
        pub const IRQ: u32 = 1 << 9;
    }

    /// `JOY_MODE` value used by pads and memory cards: 8 data bits, no parity,
    /// 1x baud multiplier.
    pub const MODE_8N1: u16 = 0x000D;
    /// `JOY_BAUD` reload for the standard ~250 kHz controller/card clock.
    pub const BAUD_250KHZ: u16 = 0x0088;
}
