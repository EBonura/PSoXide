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
        /// Force one received byte even while `/CS` is high.
        ///
        /// This is not the normal SIO0 receive-enable bit: selected devices are
        /// received with this bit clear, and hardware clears it after one byte.
        /// The usual controller/card CTRL values are therefore `0x1003` and
        /// `0x3003`, not `0x1007` and `0x3007`.
        pub const FORCE_RX_ONCE: u16 = 1 << 2;
        /// Backwards-compatible name for [`FORCE_RX_ONCE`].
        pub const RXEN: u16 = FORCE_RX_ONCE;
        /// Write-1 to acknowledge / clear the latched SIO IRQ ([`super::stat::IRQ`]).
        pub const ACK: u16 = 1 << 4;
        /// Write-1 to reset the SIO port (registers back to defaults).
        pub const RESET: u16 = 1 << 6;
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
        /// TX idle: the last byte has finished shifting onto the wire
        /// (nocash "TX Ready Flag 2").
        pub const TX_IDLE: u32 = 1 << 2;
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

    /// Normal CTRL value while a controller or memory card is selected.
    ///
    /// Receiving is implicit while `/CS` is low. `CTRL.2` is deliberately not
    /// set because it force-arms a one-shot receive and is not part of the
    /// standard selected-port setup.
    pub const fn selected_ctrl(port2: bool, ack_irq: bool) -> u16 {
        let mut value = ctrl::TXEN | ctrl::DTR;
        if ack_irq {
            value |= ctrl::ACK_IRQ_EN;
        }
        if port2 {
            value |= ctrl::SLOT_PORT2;
        }
        value
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn selected_port_values_match_retail_sio_setup() {
            assert_eq!(selected_ctrl(false, true), 0x1003);
            assert_eq!(selected_ctrl(true, true), 0x3003);
            assert_eq!(selected_ctrl(false, false), 0x0003);
            assert_eq!(selected_ctrl(true, false), 0x2003);
            assert_eq!(selected_ctrl(false, true) & ctrl::FORCE_RX_ONCE, 0);
        }
    }
}
