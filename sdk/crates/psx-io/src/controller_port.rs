//! The controller and memory-card port: one serial transport shared by every
//! driver on it.
//!
//! A pad and a memory card sit on the same wires, so a program has one owner
//! for them, the [`ControllerPort`] token, and the drivers (`psx-pad`,
//! `psx-mc`) borrow it for the length of a transaction. What they borrow is
//! the [`Transport`]: select a socket, clock bytes with the device's `/ACK`
//! pulse pacing each one, release the socket. The pad's timing is the
//! reference, since it was tuned on an original SCPH-1200, a clone and an
//! SCPH-110; a memory card uses the same exchange with a longer budget for the
//! `/ACK` after a flash write.
//!
//! Register addresses and bit layouts live in [`psx_hw::sio::sio0`].
//!
//! ```text
//! let mut port = peripherals.controller_port;
//! let mut pads = psx_pad::PadReader::port1();
//! let state = pads.poll(&mut port);
//! let mut card = psx_mc::Card::new(psx_mc::HardwareCard::on_port(&mut port, Slot::One));
//! ```

use crate::periph::ControllerPort;
use crate::{read_u32, read_u8, write_u16, write_u8};
use psx_hw::sio::sio0;

/// Which socket a transaction addresses.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Port {
    /// Controller port 1 and memory-card slot 1.
    One,
    /// Controller port 2 and memory-card slot 2.
    Two,
}

impl Port {
    /// Whether this is the second socket.
    #[inline(always)]
    pub const fn is_two(self) -> bool {
        matches!(self, Port::Two)
    }
}

/// Spin budgets for one transaction. Each spin is one bounded `STAT` read, so
/// the budgets bound the time a missing or wedged device can cost.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Timing {
    /// Delay after selecting the socket, before the first byte. The original
    /// SCPH-1200 gives no answer without it; fast clones do not care.
    pub setup_spins: u32,
    /// Bound on the byte shift itself (TX ready, then RX filled).
    pub byte_spins: u32,
    /// Bound on each wait for the device's `/ACK` to assert, and again to
    /// release.
    pub ack_spins: u32,
}

impl Timing {
    /// The pad's timing, measured on silicon: 1,024 setup spins (the
    /// SCPH-1200 floor sits between 384 and 768), and a 2,048-spin `/ACK`
    /// wait, which exceeds the BIOS's own 100 microsecond DSR timeout.
    pub const PAD: Timing = Timing {
        setup_spins: 1_024,
        byte_spins: 32_768,
        ack_spins: 2_048,
    };

    /// The memory card's timing: the pad's setup and byte budgets, and a long
    /// `/ACK` budget because a card can hold `/ACK` back while a frame's flash
    /// write commits.
    pub const CARD: Timing = Timing {
        setup_spins: 1_024,
        byte_spins: 32_768,
        ack_spins: 200_000,
    };
}

/// Why one byte exchange failed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ExchangeError {
    /// The port never became ready to take the byte.
    TxTimeout,
    /// The byte shifted out but no reply came back.
    RxTimeout,
    /// The reply came back and the device never pulsed `/ACK`; the reply is
    /// kept because an absent device answers `0xFF` here.
    AckTimeout {
        /// The byte received before the wait.
        reply: u8,
    },
    /// The device pulsed `/ACK` but held it asserted.
    AckStuck {
        /// The byte received before the wait.
        reply: u8,
    },
}

/// The six accesses the transport makes to the port, and the transaction
/// steps built on them.
///
/// [`ControllerPort`] implements it on the registers. Host tests implement it
/// on a model of a pad or a card, so the production timing code runs under
/// test unchanged.
pub trait Transport {
    /// Read the status register.
    fn status(&mut self) -> u32;
    /// Pop one received byte.
    fn receive(&mut self) -> u8;
    /// Start shifting one byte out.
    fn transmit(&mut self, byte: u8);
    /// Write the mode register.
    fn set_mode(&mut self, value: u16);
    /// Write the baud-rate reload.
    fn set_baud(&mut self, value: u16);
    /// Write the control register.
    fn set_control(&mut self, value: u16);

    /// Configure the port for 250 kHz, 8N1 and assert `/CS` on `port`.
    ///
    /// The `/ACK` interrupt is left disarmed: arming it disturbed real
    /// transfers (the SCPH-1200 stopped answering, a clone's bytes corrupted),
    /// so the transport watches the live `/ACK` level in `STAT` instead.
    fn select(&mut self, port: Port) {
        self.select_with(port, false);
    }

    /// [`select`](Self::select) with the `/ACK` interrupt armed when
    /// `arm_ack_irq` is set, so `STAT` latches each pulse. Only a diagnostic
    /// pacing wants that.
    fn select_with(&mut self, port: Port, arm_ack_irq: bool) {
        self.set_mode(sio0::MODE_8N1);
        self.set_baud(sio0::BAUD_250KHZ);
        // Clear any stale IRQ latch from the previous transaction, then
        // assert the select line.
        self.set_control(sio0::ctrl::ACK);
        self.set_control(sio0::selected_ctrl(port.is_two(), arm_ack_irq));
    }

    /// Release `/CS`, which resets the attached device's state machine.
    fn deselect(&mut self) {
        self.set_control(0);
    }

    /// Reset the port's UART. A deselect alone need not cancel a late reply
    /// still in the shifter, so a failed transaction ends with this.
    fn reset(&mut self) {
        self.set_control(sio0::ctrl::RESET);
    }

    /// Pop up to a few stale bytes so the next reply lines up with the
    /// first byte of the transaction. Bounded, so a stuck "RX not empty" flag
    /// cannot spin forever.
    fn drain_receive(&mut self) {
        let mut popped = 0;
        while self.status() & sio0::stat::RX_NOT_EMPTY != 0 && popped < 16 {
            let _ = self.receive();
            popped += 1;
        }
    }

    /// Burn time with `spins` status reads, which the compiler cannot remove.
    fn delay(&mut self, spins: u32) {
        let mut remaining = spins;
        while remaining > 0 {
            let _ = self.status();
            remaining -= 1;
            core::hint::spin_loop();
        }
    }

    /// Spin until any bit of `mask` is set in the status register, at most
    /// `spins` reads after the first. `false` on timeout.
    fn wait_status_set(&mut self, mask: u32, spins: u32) -> bool {
        let mut remaining = spins;
        while self.status() & mask == 0 {
            if remaining == 0 {
                return false;
            }
            remaining -= 1;
            core::hint::spin_loop();
        }
        true
    }

    /// Spin until every bit of `mask` is clear in the status register, at most
    /// `spins` reads after the first. `false` on timeout.
    fn wait_status_clear(&mut self, mask: u32, spins: u32) -> bool {
        let mut remaining = spins;
        while self.status() & mask != 0 {
            if remaining == 0 {
                return false;
            }
            remaining -= 1;
            core::hint::spin_loop();
        }
        true
    }

    /// Start a transaction: select `port`, wait out the setup time, drop
    /// stale bytes, and wait for an `/ACK` left asserted by an aborted
    /// transaction to release, so it is not counted as the first byte's.
    ///
    /// `false` means the line never released. Call [`finish`](Self::finish)
    /// either way.
    fn begin(&mut self, port: Port, timing: Timing) -> bool {
        self.select(port);
        self.delay(timing.setup_spins);
        self.drain_receive();
        self.wait_status_clear(sio0::stat::DSR_LEVEL, timing.ack_spins)
    }

    /// End a transaction: release `/CS`, and after a failed one reset the
    /// UART as well. The next [`select`](Self::select) restores mode and baud.
    fn finish(&mut self, succeeded: bool) {
        self.deselect();
        if !succeeded {
            self.reset();
        }
    }

    /// Clock one byte and wait for the device's `/ACK`.
    ///
    /// Waits for TX ready, writes `byte`, and watches the live `/ACK` level
    /// while the reply shifts in, so a pulse that ends before the reply
    /// arrives is not missed. Unless `is_last` (the final byte of a packet is
    /// never acknowledged), it then waits for `/ACK` to assert if it has not
    /// yet, and to release, before the next byte may go. RX-ready alone only
    /// says the byte arrived, not that the device is ready for another.
    ///
    /// No control-register write happens mid-transaction.
    fn exchange(&mut self, byte: u8, is_last: bool, timing: Timing) -> Result<u8, ExchangeError> {
        if !self.wait_status_set(sio0::stat::TX_READY, timing.byte_spins) {
            return Err(ExchangeError::TxTimeout);
        }
        self.transmit(byte);
        let mut ack_seen = false;
        let mut remaining = timing.byte_spins;
        loop {
            let status = self.status();
            ack_seen |= status & sio0::stat::DSR_LEVEL != 0;
            if status & sio0::stat::RX_NOT_EMPTY != 0 {
                break;
            }
            if remaining == 0 {
                return Err(ExchangeError::RxTimeout);
            }
            remaining -= 1;
            core::hint::spin_loop();
        }
        let reply = self.receive();
        if !is_last {
            if !ack_seen && !self.wait_status_set(sio0::stat::DSR_LEVEL, timing.ack_spins) {
                return Err(ExchangeError::AckTimeout { reply });
            }
            if !self.wait_status_clear(sio0::stat::DSR_LEVEL, timing.ack_spins) {
                return Err(ExchangeError::AckStuck { reply });
            }
        }
        Ok(reply)
    }
}

impl Transport for ControllerPort {
    #[inline(always)]
    fn status(&mut self) -> u32 {
        // SAFETY: STAT is the port's aligned 32-bit status register; reading it
        // has no side effects. The `&mut` borrow of the token is the proof no
        // other driver is mid-transaction.
        unsafe { read_u32(sio0::STAT) }
    }

    #[inline(always)]
    fn receive(&mut self) -> u8 {
        // SAFETY: a byte read of DATA pops one RX FIFO byte.
        unsafe { read_u8(sio0::DATA) }
    }

    #[inline(always)]
    fn transmit(&mut self, byte: u8) {
        // SAFETY: a byte write to DATA clocks it out of the port.
        unsafe { write_u8(sio0::DATA, byte) }
    }

    #[inline(always)]
    fn set_mode(&mut self, value: u16) {
        // SAFETY: MODE is the port's 16-bit mode register.
        unsafe { write_u16(sio0::MODE, value) }
    }

    #[inline(always)]
    fn set_baud(&mut self, value: u16) {
        // SAFETY: BAUD is the port's 16-bit baud-rate reload.
        unsafe { write_u16(sio0::BAUD, value) }
    }

    #[inline(always)]
    fn set_control(&mut self, value: u16) {
        // SAFETY: CTRL is the port's 16-bit control register; it selects the
        // socket and resets the port, nothing else.
        unsafe { write_u16(sio0::CTRL, value) }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    /// A scripted port: each status read pops the next scripted word, and
    /// every write is logged.
    #[derive(Default)]
    struct Script {
        status: Vec<u32>,
        reads: usize,
        received: Vec<u8>,
        sent: Vec<u8>,
        control: Vec<u16>,
    }

    impl Script {
        fn with_status(words: &[u32]) -> Self {
            Script {
                status: words.to_vec(),
                ..Script::default()
            }
        }
    }

    impl Transport for Script {
        fn status(&mut self) -> u32 {
            // Past the script, hold the last word.
            let word = self
                .status
                .get(self.reads)
                .or(self.status.last())
                .copied()
                .unwrap_or(0);
            self.reads += 1;
            word
        }
        fn receive(&mut self) -> u8 {
            self.received.pop().unwrap_or(0xFF)
        }
        fn transmit(&mut self, byte: u8) {
            self.sent.push(byte);
        }
        fn set_mode(&mut self, _value: u16) {}
        fn set_baud(&mut self, _value: u16) {}
        fn set_control(&mut self, value: u16) {
            self.control.push(value);
        }
    }

    const TX: u32 = sio0::stat::TX_READY;
    const RX: u32 = sio0::stat::RX_NOT_EMPTY;
    const DSR: u32 = sio0::stat::DSR_LEVEL;
    const TIMING: Timing = Timing {
        setup_spins: 2,
        byte_spins: 4,
        ack_spins: 4,
    };

    #[test]
    fn the_token_is_the_only_way_to_a_port_that_implements_the_transport() {
        fn takes_transport<T: Transport>(_: &mut T) {}
        // SAFETY: a test-local token on the host, where no register is touched.
        let mut port = unsafe { ControllerPort::steal() };
        takes_transport(&mut port);
        assert_eq!(core::mem::size_of::<ControllerPort>(), 0);
    }

    #[test]
    fn select_clears_the_latch_then_asserts_the_port_without_arming_the_ack_interrupt() {
        for (port, selected) in [(Port::One, 0x0003), (Port::Two, 0x2003)] {
            let mut bus = Script::default();
            bus.select(port);
            assert_eq!(bus.control, [sio0::ctrl::ACK, selected]);
            assert_eq!(selected & sio0::ctrl::ACK_IRQ_EN, 0);
        }
    }

    #[test]
    fn a_failed_transaction_resets_the_uart_and_a_clean_one_only_releases_the_port() {
        let mut bus = Script::default();
        bus.finish(true);
        assert_eq!(bus.control, [0]);
        let mut bus = Script::default();
        bus.finish(false);
        assert_eq!(bus.control, [0, sio0::ctrl::RESET]);
    }

    #[test]
    fn begin_drops_stale_bytes_and_waits_for_a_held_ack_to_release() {
        // One stale byte (RX set), then drained, then DSR still high twice
        // before it releases.
        let mut bus = Script::with_status(&[0, 0, RX, 0, DSR, DSR, 0]);
        bus.received.push(0x55);
        assert!(bus.begin(Port::One, TIMING));
        assert!(bus.received.is_empty());

        let mut stuck = Script::with_status(&[DSR]);
        assert!(!stuck.begin(Port::One, TIMING));
    }

    #[test]
    fn an_acknowledged_byte_waits_for_ack_to_assert_and_release() {
        // TX ready; RX arrives at once without an ack; ack asserts, releases.
        let mut bus = Script::with_status(&[TX, RX, DSR, 0]);
        bus.received.push(0x41);
        assert_eq!(bus.exchange(0x42, false, TIMING), Ok(0x41));
        assert_eq!(bus.sent, [0x42]);
        // No control write happened inside the exchange.
        assert!(bus.control.is_empty());
    }

    #[test]
    fn an_ack_seen_while_the_reply_shifts_in_is_not_waited_for_again() {
        // The pulse lands during the RX wait (DSR set, RX not yet), ends, and
        // RX arrives after: only the release wait remains.
        let mut bus = Script::with_status(&[TX, DSR, RX, 0]);
        bus.received.push(0x5A);
        assert_eq!(bus.exchange(0x00, false, TIMING), Ok(0x5A));
    }

    #[test]
    fn the_last_byte_of_a_packet_is_never_waited_on() {
        let mut bus = Script::with_status(&[TX, RX]);
        bus.received.push(0x80);
        assert_eq!(bus.exchange(0x00, true, TIMING), Ok(0x80));
        // Two status reads: TX ready and RX ready, nothing after.
        assert_eq!(bus.reads, 2);
    }

    #[test]
    fn each_failure_is_reported_with_its_stage() {
        let mut bus = Script::with_status(&[0]);
        assert_eq!(
            bus.exchange(0x01, false, TIMING),
            Err(ExchangeError::TxTimeout)
        );
        assert!(bus.sent.is_empty());

        let mut bus = Script::with_status(&[TX, 0]);
        assert_eq!(
            bus.exchange(0x01, false, TIMING),
            Err(ExchangeError::RxTimeout)
        );

        let mut bus = Script::with_status(&[TX, RX, 0]);
        bus.received.push(0xFF);
        assert_eq!(
            bus.exchange(0x01, false, TIMING),
            Err(ExchangeError::AckTimeout { reply: 0xFF })
        );

        let mut bus = Script::with_status(&[TX, RX | DSR, DSR]);
        bus.received.push(0x41);
        assert_eq!(
            bus.exchange(0x01, false, TIMING),
            Err(ExchangeError::AckStuck { reply: 0x41 })
        );
    }

    #[test]
    fn the_pad_and_card_budgets_differ_only_in_the_ack_wait() {
        assert_eq!(
            Timing {
                ack_spins: Timing::PAD.ack_spins,
                ..Timing::CARD
            },
            Timing::PAD
        );
        const { assert!(Timing::CARD.ack_spins > Timing::PAD.ack_spins) };
    }
}
