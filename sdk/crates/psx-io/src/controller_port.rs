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
use crate::{read_u16, read_u32, read_u8, write_u16, write_u8};
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

/// The shortest time a pad is given between `/CS` asserting and the first
/// byte, in system-clock cycles (33.8688 MHz): what the pad driver's 1,024
/// setup spins took on a console (6.93 cycles a spin, run of 11-03, so about
/// 7,100), rounded up. The original SCPH-1200 needs between 384 and 768 spins;
/// the 11-03 console answered at every delay from 64 to 24,576 cycles.
///
/// The pad driver measures it on root counter 0 ([`Transport::clock`]), so the
/// wait can start before the transaction does: [`Transport::open`] asserts
/// `/CS` and [`Transport::begin_pad`] waits only for what is left.
pub const PAD_SETUP_CYCLES: u16 = 7_168;

/// The two forms a device's `/ACK` takes in `STAT`: the live level, and the
/// latched interrupt bit.
const ACK_SIGNALS: u32 = sio0::stat::DSR_LEVEL | sio0::stat::IRQ;

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
    /// Read the control register back.
    fn control(&mut self) -> u16;
    /// System-clock cycles, modulo 2^16: root counter 0, which `psx-rt` sets
    /// to count the system clock at entry and nothing in the SDK reconfigures.
    /// A difference of two readings never exceeds the time that really passed
    /// (a wrap only makes it smaller), so a wait on it is never too short.
    fn clock(&mut self) -> u16;

    /// Assert `/CS` on `port` for a pad transaction that [`begin_pad`]
    /// starts later, and return the [`clock`] it was asserted at. The pad's
    /// setup time then passes while the program does something else.
    ///
    /// [`begin_pad`]: Self::begin_pad
    /// [`clock`]: Self::clock
    #[inline(never)]
    fn open(&mut self, port: Port) -> u16 {
        self.select(port);
        self.clock()
    }

    /// Start a pad transaction on `port`: [`begin`](Self::begin) with the
    /// pad's timing, or, on a select [`open`](Self::open) left, the rest of
    /// [`PAD_SETUP_CYCLES`] and then the same drain and `/ACK` release.
    ///
    /// `opened` is what `open` returned for this port. It is used only if
    /// `CTRL` still reads back as that select, so a memory-card transfer or
    /// anything else that released or moved the select in between costs a
    /// fresh select and the whole setup delay, as does `None`. `false` means
    /// the line never released; call [`finish`](Self::finish) either way.
    fn begin_pad(&mut self, port: Port, opened: Option<u16>) -> bool {
        match opened {
            Some(at) if self.control() == sio0::selected_ctrl(port.is_two(), false) => {
                // Bounded as well, so a counter someone stopped cannot hang
                // the poll: twice the pad's setup spins of clock readings is
                // still at least the setup time without the clock.
                let mut spins = 2 * Timing::PAD.setup_spins;
                while self.clock().wrapping_sub(at) < PAD_SETUP_CYCLES && spins != 0 {
                    spins -= 1;
                    core::hint::spin_loop();
                }
            }
            _ => {
                self.select(port);
                self.delay(Timing::PAD.setup_spins);
            }
        }
        self.drain_receive();
        self.wait_status_clear(sio0::stat::DSR_LEVEL, Timing::PAD.ack_spins)
    }

    /// Configure the port for 250 kHz, 8N1 and assert `/CS` on `port`.
    ///
    /// The `/ACK` interrupt is left disarmed: arming it disturbed real
    /// transfers (the SCPH-1200 stopped answering, a clone's bytes corrupted),
    /// so the transport watches the live `/ACK` level in `STAT` instead.
    fn select(&mut self, port: Port) {
        self.select_with(port, false);
    }

    /// [`select`](Self::select) with the `/ACK` interrupt armed when
    /// `arm_ack_irq` is set, so `STAT` latches each pulse. Memory cards use
    /// this because a CPU interrupt can hide a complete short ACK pulse.
    fn select_with(&mut self, port: Port, arm_ack_irq: bool) {
        self.set_mode(sio0::MODE_8N1);
        self.set_baud(sio0::BAUD_250KHZ);
        // Clear any stale IRQ latch from the previous transaction, then
        // assert the select line.
        self.set_control(sio0::ctrl::ACK);
        self.set_control(sio0::selected_ctrl(port.is_two(), arm_ack_irq));
    }

    /// Whether the `/ACK` interrupt is armed in `CTRL`. A pad poll arms it only
    /// when it has seen a port acknowledge a byte with the latch alone, so this
    /// tells a caller, after the fact, that the port is of that kind.
    fn ack_irq_armed(&mut self) -> bool {
        self.control() & sio0::ctrl::ACK_IRQ_EN != 0
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

    /// [`wait_status_set`](Self::wait_status_set), returning
    /// the bits of `mask` that were set, so the caller can tell which one; 0 on
    /// timeout.
    fn wait_status_any(&mut self, mask: u32, spins: u32) -> u32 {
        let mut remaining = spins;
        loop {
            let seen = self.status() & mask;
            if seen != 0 {
                return seen;
            }
            if remaining == 0 {
                return 0;
            }
            remaining -= 1;
            core::hint::spin_loop();
        }
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
        self.finish_begin(timing)
    }

    /// Begin a memory-card transaction with the SIO ACK interrupt latch
    /// armed. The card's short pulse can fall entirely inside an unrelated
    /// CPU interrupt, after which the live DSR level no longer records it.
    fn begin_card(&mut self, port: Port, timing: Timing) -> bool {
        self.select_with(port, true);
        let ready = self.finish_begin(timing);
        if ready {
            // A late ACK from the preceding transaction can arrive during
            // setup. Its live level is gone now, but its IRQ latch must not
            // count as the first byte's acknowledgement.
            self.set_control(sio0::selected_ctrl(port.is_two(), true) | sio0::ctrl::ACK);
        }
        ready
    }

    /// Shared setup, stale RX drain and live ACK release check after select.
    fn finish_begin(&mut self, timing: Timing) -> bool {
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

    /// Clock one byte and wait for the device's `/ACK`, in whichever form the
    /// port reports it.
    ///
    /// Waits for TX ready, writes `byte`, and watches `STAT` while the reply
    /// shifts in, so a pulse that ends before the reply arrives is not missed.
    /// An acknowledgement is either the live `/ACK` level ([`DSR_LEVEL`]) or
    /// the latched interrupt bit ([`IRQ`]): silicon and the PSoXide emulator
    /// show the level (the latch needs the interrupt armed), while PCSX-ReARMed
    /// latches the bit and never shows a level. Unless `is_last` (the final
    /// byte of a packet is never acknowledged), it then waits for either to
    /// appear if neither has yet, waits for the level to release, and, when the
    /// latch is what answered, clears it so it cannot count for the next byte.
    /// RX-ready alone only says the byte arrived, not that the device is ready
    /// for another.
    ///
    /// A port that shows the level takes the same register accesses as before
    /// and no control-register write happens mid-transaction. Only when the
    /// latch is seen does the exchange write `CTRL` once, with the interrupt
    /// armed and the latch cleared, `/CS` still asserted on `port`: ReARMed
    /// latches the first two bytes of a pad packet unarmed and the rest only
    /// when armed.
    ///
    /// [`DSR_LEVEL`]: sio0::stat::DSR_LEVEL
    /// [`IRQ`]: sio0::stat::IRQ
    fn exchange(
        &mut self,
        port: Port,
        byte: u8,
        is_last: bool,
        timing: Timing,
    ) -> Result<u8, ExchangeError> {
        if !self.wait_status_set(sio0::stat::TX_READY, timing.byte_spins) {
            return Err(ExchangeError::TxTimeout);
        }
        self.transmit(byte);
        let mut ack_seen = 0;
        let mut remaining = timing.byte_spins;
        loop {
            let status = self.status();
            ack_seen |= status & ACK_SIGNALS;
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
            if ack_seen == 0 {
                ack_seen = self.wait_status_any(ACK_SIGNALS, timing.ack_spins);
                if ack_seen == 0 {
                    return Err(ExchangeError::AckTimeout { reply });
                }
            }
            if !self.wait_status_clear(sio0::stat::DSR_LEVEL, timing.ack_spins) {
                return Err(ExchangeError::AckStuck { reply });
            }
            if ack_seen & sio0::stat::IRQ != 0 {
                self.set_control(sio0::selected_ctrl(port.is_two(), true) | sio0::ctrl::ACK);
            }
        }
        Ok(reply)
    }

    /// Card byte exchange. STAT.IRQ latches an ACK edge across a CPU
    /// interrupt; the live DSR level still has to release before the next
    /// byte, and CTRL.ACK clears only this byte's latch while /CS stays low.
    fn exchange_card(
        &mut self,
        port: Port,
        byte: u8,
        is_last: bool,
        timing: Timing,
    ) -> Result<u8, ExchangeError> {
        if !self.wait_status_set(sio0::stat::TX_READY, timing.byte_spins) {
            return Err(ExchangeError::TxTimeout);
        }
        self.transmit(byte);
        if !self.wait_status_set(sio0::stat::RX_NOT_EMPTY, timing.byte_spins) {
            return Err(ExchangeError::RxTimeout);
        }
        let reply = self.receive();
        if !is_last {
            if !self.wait_status_set(sio0::stat::IRQ, timing.ack_spins) {
                return Err(ExchangeError::AckTimeout { reply });
            }
            if !self.wait_status_clear(sio0::stat::DSR_LEVEL, timing.ack_spins) {
                return Err(ExchangeError::AckStuck { reply });
            }
            self.set_control(sio0::selected_ctrl(port.is_two(), true) | sio0::ctrl::ACK);
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

    #[inline(always)]
    fn control(&mut self) -> u16 {
        // SAFETY: CTRL is the port's aligned 16-bit control register; reading it
        // has no side effects.
        unsafe { read_u16(sio0::CTRL) }
    }

    #[inline(always)]
    fn clock(&mut self) -> u16 {
        crate::timers::counter(crate::timers::Timer::Timer0)
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
        /// Root counter 0, and how far it moves on each read.
        time: u16,
        tick: u16,
        clocks: usize,
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
            self.received.pop().unwrap_or(0xff)
        }
        fn transmit(&mut self, byte: u8) {
            self.sent.push(byte);
        }
        fn set_mode(&mut self, _value: u16) {}
        fn set_baud(&mut self, _value: u16) {}
        fn set_control(&mut self, value: u16) {
            self.control.push(value);
        }
        fn control(&mut self) -> u16 {
            self.control.last().copied().unwrap_or(0)
        }
        fn clock(&mut self) -> u16 {
            self.clocks += 1;
            let now = self.time;
            self.time = self.time.wrapping_add(self.tick.max(1));
            now
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
    fn card_begin_clears_an_ack_that_arrived_during_setup() {
        // The earlier transaction's ACK arrives during the setup delay.
        // Its live level has released by the time begin checks the line.
        let mut bus = Script::with_status(&[sio0::stat::IRQ | DSR, 0, 0, 0]);
        assert!(bus.begin_card(Port::One, TIMING));
        assert_eq!(
            bus.control,
            [
                sio0::ctrl::ACK,
                sio0::selected_ctrl(false, true),
                sio0::selected_ctrl(false, true) | sio0::ctrl::ACK,
            ]
        );
    }

    #[test]
    fn an_acknowledged_byte_waits_for_ack_to_assert_and_release() {
        // TX ready; RX arrives at once without an ack; ack asserts, releases.
        let mut bus = Script::with_status(&[TX, RX, DSR, 0]);
        bus.received.push(0x41);
        assert_eq!(bus.exchange(Port::One, 0x42, false, TIMING), Ok(0x41));
        assert_eq!(bus.sent, [0x42]);
        // No control write happened inside the exchange.
        assert!(bus.control.is_empty());
    }

    #[test]
    fn an_ack_seen_while_the_reply_shifts_in_is_not_waited_for_again() {
        // The pulse lands during the RX wait (DSR set, RX not yet), ends, and
        // RX arrives after: only the release wait remains.
        let mut bus = Script::with_status(&[TX, DSR, RX, 0]);
        bus.received.push(0x5a);
        assert_eq!(bus.exchange(Port::One, 0x00, false, TIMING), Ok(0x5a));
    }

    #[test]
    fn the_last_byte_of_a_packet_is_never_waited_on() {
        let mut bus = Script::with_status(&[TX, RX]);
        bus.received.push(0x80);
        assert_eq!(bus.exchange(Port::One, 0x00, true, TIMING), Ok(0x80));
        // Two status reads: TX ready and RX ready, nothing after.
        assert_eq!(bus.reads, 2);
    }

    #[test]
    fn a_latched_ack_with_no_live_level_acknowledges_the_byte_and_is_cleared() {
        // The reply arrives, the latch shows, the level never does. One CTRL
        // write arms the interrupt and clears the latch, /CS held on port 2.
        const IRQ: u32 = sio0::stat::IRQ;
        let mut bus = Script::with_status(&[TX, RX, IRQ, IRQ, 0]);
        bus.received.push(0x41);
        assert_eq!(bus.exchange(Port::Two, 0x42, false, TIMING), Ok(0x41));
        assert_eq!(
            bus.control,
            [sio0::selected_ctrl(true, true) | sio0::ctrl::ACK]
        );
    }

    #[test]
    fn a_latch_seen_while_the_reply_shifts_in_is_not_waited_for_again() {
        const IRQ: u32 = sio0::stat::IRQ;
        let mut bus = Script::with_status(&[TX, IRQ, RX | IRQ, 0]);
        bus.received.push(0x5a);
        assert_eq!(bus.exchange(Port::One, 0x00, false, TIMING), Ok(0x5a));
        assert_eq!(bus.control.len(), 1);
    }

    #[test]
    fn a_level_and_a_latch_together_wait_for_the_level_to_release_before_clearing() {
        const IRQ: u32 = sio0::stat::IRQ;
        let mut bus = Script::with_status(&[TX, RX, DSR | IRQ, DSR | IRQ, IRQ, IRQ]);
        bus.received.push(0x73);
        assert_eq!(bus.exchange(Port::One, 0x42, false, TIMING), Ok(0x73));
        // TX, RX, then STAT read until DSR dropped: the write came after.
        assert_eq!(bus.reads, 5);
        assert_eq!(
            bus.control,
            [sio0::selected_ctrl(false, true) | sio0::ctrl::ACK]
        );
    }

    #[test]
    fn a_level_only_ack_writes_nothing_mid_packet() {
        // The wire pattern silicon was tuned with: no CTRL write.
        let mut bus = Script::with_status(&[TX, RX, DSR, DSR, 0]);
        bus.received.push(0x41);
        assert_eq!(bus.exchange(Port::One, 0x42, false, TIMING), Ok(0x41));
        assert!(bus.control.is_empty());
    }

    #[test]
    fn the_ack_interrupt_reads_as_armed_only_after_the_latch_exchange_arms_it() {
        const IRQ: u32 = sio0::stat::IRQ;
        let mut level = Script::with_status(&[TX, RX, DSR, 0]);
        level.received.push(0x41);
        assert_eq!(level.exchange(Port::One, 0x42, false, TIMING), Ok(0x41));
        assert!(!level.ack_irq_armed());

        let mut latch = Script::with_status(&[TX, RX, IRQ, IRQ, 0]);
        latch.received.push(0x41);
        assert_eq!(latch.exchange(Port::One, 0x42, false, TIMING), Ok(0x41));
        assert!(latch.ack_irq_armed());
    }

    #[test]
    fn no_ack_signal_at_all_times_out_with_the_reply_kept() {
        let mut bus = Script::with_status(&[TX, RX, 0]);
        bus.received.push(0xff);
        assert_eq!(
            bus.exchange(Port::One, 0x01, false, TIMING),
            Err(ExchangeError::AckTimeout { reply: 0xff })
        );
        assert!(bus.control.is_empty());
    }

    #[test]
    fn card_ack_latch_survives_a_pulse_hidden_by_an_interrupt() {
        // The reply arrived before the card's ACK. The next observation is
        // after the pulse: DSR is low, but STAT.IRQ still records the edge.
        let mut bus = Script::with_status(&[TX, RX, sio0::stat::IRQ, 0]);
        bus.received.push(0x5a);
        assert_eq!(bus.exchange_card(Port::One, 0x00, false, TIMING), Ok(0x5a));
        assert_eq!(
            bus.control,
            [sio0::selected_ctrl(false, true) | sio0::ctrl::ACK]
        );

        let mut live = Script::with_status(&[TX, RX, 0]);
        live.received.push(0x5a);
        assert_eq!(
            live.exchange(Port::One, 0x00, false, TIMING),
            Err(ExchangeError::AckTimeout { reply: 0x5a })
        );
    }

    #[test]
    fn card_terminator_needs_no_ack_and_missing_card_times_out() {
        let mut last = Script::with_status(&[TX, RX]);
        last.received.push(0x47);
        assert_eq!(last.exchange_card(Port::One, 0, true, TIMING), Ok(0x47));
        assert_eq!(last.reads, 2);
        assert!(last.control.is_empty());

        let mut absent = Script::with_status(&[TX, RX, 0]);
        absent.received.push(0xff);
        assert_eq!(
            absent.exchange_card(Port::One, 0x81, false, TIMING),
            Err(ExchangeError::AckTimeout { reply: 0xff })
        );
    }

    #[test]
    fn each_failure_is_reported_with_its_stage() {
        let mut bus = Script::with_status(&[0]);
        assert_eq!(
            bus.exchange(Port::One, 0x01, false, TIMING),
            Err(ExchangeError::TxTimeout)
        );
        assert!(bus.sent.is_empty());

        let mut bus = Script::with_status(&[TX, 0]);
        assert_eq!(
            bus.exchange(Port::One, 0x01, false, TIMING),
            Err(ExchangeError::RxTimeout)
        );

        let mut bus = Script::with_status(&[TX, RX, 0]);
        bus.received.push(0xff);
        assert_eq!(
            bus.exchange(Port::One, 0x01, false, TIMING),
            Err(ExchangeError::AckTimeout { reply: 0xff })
        );

        let mut bus = Script::with_status(&[TX, RX | DSR, DSR]);
        bus.received.push(0x41);
        assert_eq!(
            bus.exchange(Port::One, 0x01, false, TIMING),
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

    #[test]
    fn a_select_opened_earlier_is_kept_and_only_the_rest_of_the_setup_is_waited() {
        let mut bus = Script {
            tick: 1_000,
            ..Script::default()
        };
        let opened = bus.open(Port::Two);
        let writes = bus.control.len();
        // The program worked for 5,000 cycles in between.
        bus.time = bus.time.wrapping_add(4_000);
        let reads = bus.clocks;
        assert!(bus.begin_pad(Port::Two, Some(opened)));
        assert_eq!(bus.control.len(), writes, "no second select");
        // 5,000 had passed; 7,168 needs three more readings of 1,000.
        assert_eq!(bus.clocks - reads, 4);
        assert!(bus.time.wrapping_sub(opened) >= PAD_SETUP_CYCLES);
    }

    #[test]
    fn a_long_gap_costs_no_wait_and_a_wrap_never_shortens_it() {
        // Opened just before the counter wraps, polled after it: the modular
        // difference is the real one.
        let mut bus = Script {
            time: 0xFF00,
            tick: 16,
            ..Script::default()
        };
        let opened = bus.open(Port::One);
        bus.time = opened.wrapping_add(30_000);
        let reads = bus.clocks;
        assert!(bus.begin_pad(Port::One, Some(opened)));
        assert_eq!(bus.clocks - reads, 1, "one reading, no wait");

        // A gap of a whole wrap plus a little reads as the little: the wait is
        // longer than needed, never shorter.
        let mut bus = Script {
            tick: 64,
            ..Script::default()
        };
        let opened = bus.open(Port::One);
        bus.time = opened.wrapping_add(100);
        assert!(bus.begin_pad(Port::One, Some(opened)));
        assert!(bus.time.wrapping_sub(opened) >= PAD_SETUP_CYCLES);
    }

    #[test]
    fn a_select_released_or_moved_in_between_is_selected_again_with_the_whole_wait() {
        for (moved_to, port) in [(0u16, Port::One), (sio0::selected_ctrl(true, false), Port::One)] {
            let mut bus = Script {
                tick: 64,
                ..Script::default()
            };
            let opened = bus.open(port);
            // A card transfer ended with a deselect, or another port was taken.
            bus.set_control(moved_to);
            bus.time = opened.wrapping_add(50_000);
            assert!(bus.begin_pad(port, Some(opened)));
            let fresh = *bus.control.last().unwrap();
            assert_eq!(fresh, sio0::selected_ctrl(false, false));
            assert!(bus.reads >= 1_024, "the whole setup delay again");
        }
        // No earlier select at all: the same.
        let mut bus = Script {
            tick: 64,
            ..Script::default()
        };
        assert!(bus.begin_pad(Port::One, None));
        assert_eq!(bus.control, [sio0::ctrl::ACK, sio0::selected_ctrl(false, false)]);
        // The sequence before select-early: the spin delay, no clock.
        assert!(bus.reads >= 1_024);
        assert_eq!(bus.clocks, 0);
    }
}
