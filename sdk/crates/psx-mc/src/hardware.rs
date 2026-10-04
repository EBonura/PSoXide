// SPDX-License-Identifier: GPL-2.0-or-later
//! On-device SIO0 transport for a physical memory card (feature `hw`).
//!
//! Talks the raw card protocol byte by byte: assert the port, clock `0x81` to
//! address the card, then a Read (`0x52`) or Write (`0x57`) command frame. The
//! serial link is `psx-io`'s shared [`Transport`], the one `psx-pad` polls
//! controllers with: the same select, the same ACK-paced byte exchange that
//! watches the live `/ACK` level, the same release. A card's own timing is a
//! longer `/ACK` budget, because its write commit stalls between bytes while
//! flash settles.
//!
//! A [`HardwareCard`] holds the controller port while it exists: either the
//! [`ControllerPort`] token itself ([`HardwareCard::on_port`] with the token,
//! and [`release`](HardwareCard::release) gives it back) or a `&mut` borrow of
//! one the program keeps for polling pads between saves.
//!
//! Timing is exposed as a runtime knob ([`HardwareCard::on_port_with_timing`])
//! because real silicon varies: the emulator ACKs instantly, an official card
//! needs the setup delay, and a write commit may need a longer ACK wait than a
//! pad byte.
//!
//! [`HardwareCard::last_trace`] exposes bounded transport diagnostics. In
//! particular, an `/ACK` timeout is never silently treated as a successful
//! exchange: the rest of that transaction is aborted and the first failing byte
//! remains visible to an on-console diagnostic.

use crate::{Block, Error, Result, FRAME_COUNT, FRAME_SIZE};
use core::borrow::BorrowMut;
use psx_io::controller_port::{ExchangeError, Port, Transport};
use psx_io::periph::ControllerPort;

// Protocol bytes.
const CARD_SELECT: u8 = 0x81;
const CMD_READ: u8 = 0x52;
const CMD_WRITE: u8 = 0x57;
const ID1: u8 = 0x5A;
const ACK1: u8 = 0x5C;
const END_GOOD: u8 = 0x47;
/// A card needs a slightly longer recovery interval after acknowledging the
/// initial `0x81` device-select byte than it does between ordinary frame bytes.
const FIRST_COMMAND_SETTLE_SPINS: u32 = 1_024;
/// Conservative volatile-MMIO delay after a 128-byte sector write. Reference
/// drivers leave at least two video periods before accessing the card again.
/// This intentionally covers the slower 50 Hz case as well as 60 Hz consoles.
const POST_WRITE_SETTLE_SPINS: u32 = 400_000;

/// Which controller/card port to use.
pub type Slot = Port;

/// The first low-level failure observed in the latest frame transaction.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TransportFault {
    /// No low-level timeout was observed.
    None,
    /// SIO0 never became ready to accept the next byte.
    TxTimeout,
    /// SIO0 never returned a byte after transmission.
    RxTimeout,
    /// The card did not pulse `/ACK` after a non-final byte.
    AckTimeout,
    /// `/ACK` was observed, but the live line did not return high.
    AckReleaseTimeout,
}

/// Compact evidence from the latest raw read/write transaction.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TransportTrace {
    /// Total byte exchanges attempted before the transaction was aborted.
    pub exchanges: u16,
    /// Number of `/ACK` pulses observed.
    pub acknowledgements: u16,
    /// First low-level fault.
    pub fault: TransportFault,
    /// Zero-based exchange at which `fault` occurred, or `0xffff` for none.
    pub fault_exchange: u16,
    /// First ten response bytes (flags, IDs, echoes and acknowledgements).
    pub response_prefix: [u8; 10],
}

impl TransportTrace {
    const fn new() -> Self {
        Self {
            exchanges: 0,
            acknowledgements: 0,
            fault: TransportFault::None,
            fault_exchange: u16::MAX,
            response_prefix: [0xff; 10],
        }
    }
}

/// Tunable transfer timing (bounded MMIO spin counts). Defaults mirror the pad.
#[derive(Copy, Clone, Debug)]
pub struct Timing {
    /// Delay after asserting select, before the first byte (strict-pad setup).
    pub setup_spins: u32,
    /// Bound on waiting for TX-ready / RX-filled per byte.
    pub byte_spins: u32,
    /// Bound on waiting for the `/ACK` pulse (raise for slow write commits).
    pub ack_spins: u32,
}

impl Default for Timing {
    fn default() -> Self {
        let card = psx_io::controller_port::Timing::CARD;
        Timing {
            setup_spins: card.setup_spins,
            byte_spins: card.byte_spins,
            ack_spins: card.ack_spins,
        }
    }
}

impl Timing {
    fn link(self) -> psx_io::controller_port::Timing {
        psx_io::controller_port::Timing {
            setup_spins: self.setup_spins,
            byte_spins: self.byte_spins,
            ack_spins: self.ack_spins,
        }
    }
}

/// A physical memory card reached over SIO0.
///
/// `P` is how the card holds the port: the [`ControllerPort`] token (the
/// default) or a `&mut ControllerPort` borrowed from the program for the
/// length of the card's use.
pub struct HardwareCard<P = ControllerPort> {
    port: P,
    slot: Slot,
    timing: Timing,
    trace: TransportTrace,
}

impl HardwareCard<ControllerPort> {
    /// A card on the given `slot` with default timing.
    #[deprecated(note = "use `HardwareCard::on_port` with the `ControllerPort` token")]
    pub fn new(slot: Slot) -> Self {
        Self::on_port(steal_port(), slot)
    }

    /// A card with custom [`Timing`] (for tuning against real silicon).
    #[deprecated(note = "use `HardwareCard::on_port_with_timing` with the `ControllerPort` token")]
    pub fn with_timing(slot: Slot, timing: Timing) -> Self {
        Self::on_port_with_timing(steal_port(), slot, timing)
    }
}

/// The token for a deprecated constructor that never took one.
fn steal_port() -> ControllerPort {
    // SAFETY: a token is a logic guard, not a memory-safety one (see
    // `psx_io::periph`), and the old constructors never took one.
    unsafe { ControllerPort::steal() }
}

impl<P: BorrowMut<ControllerPort>> HardwareCard<P> {
    /// A card on `slot`, reached through `port` (the token, or a borrow of
    /// it), with the memory card's default [`Timing`].
    pub fn on_port(port: P, slot: Slot) -> Self {
        Self::on_port_with_timing(port, slot, Timing::default())
    }

    /// [`on_port`](Self::on_port) with custom [`Timing`], for tuning against
    /// real silicon.
    pub fn on_port_with_timing(port: P, slot: Slot, timing: Timing) -> Self {
        HardwareCard {
            port,
            slot,
            timing,
            trace: TransportTrace::new(),
        }
    }

    /// Give the port back: the token when the card owned it, the borrow
    /// otherwise.
    pub fn release(self) -> P {
        self.port
    }

    /// Diagnostic evidence from the most recent frame read or write.
    pub fn last_trace(&self) -> TransportTrace {
        self.trace
    }
}

/// One frame transaction's view of the port: the shared transport plus the
/// card's trace of what happened on it.
struct Link<'a, T> {
    bus: &'a mut T,
    slot: Slot,
    timing: Timing,
    trace: TransportTrace,
}

impl<T: Transport> Link<'_, T> {
    fn select(&mut self) {
        if !self.bus.begin(self.slot, self.timing.link()) {
            // The previous transaction left `/ACK` asserted and it never
            // released. Every exchange fails from here.
            self.record_fault(TransportFault::AckReleaseTimeout, 0);
        }
    }

    fn record_fault(&mut self, fault: TransportFault, exchange: u16) {
        if self.trace.fault == TransportFault::None {
            self.trace.fault = fault;
            self.trace.fault_exchange = exchange;
        }
    }

    fn note_reply(&mut self, exchange: u16, reply: u8) {
        if (exchange as usize) < self.trace.response_prefix.len() {
            self.trace.response_prefix[exchange as usize] = reply;
        }
        self.trace.exchanges = exchange + 1;
    }

    /// Clock one byte and, when `wait_ack`, wait for the card's `/ACK` pulse so
    /// the next byte is not clocked early. Returns the received byte, or
    /// `0xff` once the transaction has failed.
    fn xfer(&mut self, tx: u8, wait_ack: bool) -> u8 {
        if self.trace.fault != TransportFault::None {
            return 0xff;
        }
        let exchange = self.trace.exchanges;
        match self.bus.exchange(tx, !wait_ack, self.timing.link()) {
            Ok(reply) => {
                self.note_reply(exchange, reply);
                if wait_ack {
                    self.trace.acknowledgements += 1;
                }
                reply
            }
            Err(ExchangeError::TxTimeout) => {
                self.record_fault(TransportFault::TxTimeout, exchange);
                0xff
            }
            Err(ExchangeError::RxTimeout) => {
                self.record_fault(TransportFault::RxTimeout, exchange);
                0xff
            }
            Err(ExchangeError::AckTimeout { reply }) => {
                self.note_reply(exchange, reply);
                self.record_fault(TransportFault::AckTimeout, exchange);
                reply
            }
            Err(ExchangeError::AckStuck { reply }) => {
                self.note_reply(exchange, reply);
                self.trace.acknowledgements += 1;
                self.record_fault(TransportFault::AckReleaseTimeout, exchange);
                reply
            }
        }
    }

    fn end(&mut self) {
        let clean = self.trace.fault == TransportFault::None;
        self.bus.finish(clean);
    }

    fn read_frame(&mut self, frame: u16, out: &mut [u8; FRAME_SIZE]) -> Result<()> {
        let msb = (frame >> 8) as u8;
        let lsb = frame as u8;

        self.select();
        self.xfer(CARD_SELECT, true); // open bus
        self.bus.delay(FIRST_COMMAND_SETTLE_SPINS);
        let flags = self.xfer(CMD_READ, true);
        let id1 = self.xfer(0x00, true); // 0x5A
        let _id2 = self.xfer(0x00, true); // 0x5D
        let _ = self.xfer(msb, true); // 0x00
        let _ = self.xfer(lsb, true); // MSB echo
        let ack1 = self.xfer(0x00, true); // 0x5C
        let _ack2 = self.xfer(0x00, true); // 0x5D
        let emsb = self.xfer(0x00, true); // MSB
        let elsb = self.xfer(0x00, true); // LSB
        for b in out.iter_mut() {
            *b = self.xfer(0x00, true);
        }
        let chk = self.xfer(0x00, true);
        let end = self.xfer(0x00, false); // terminator, no ACK
        self.end();

        if id1 == 0xFF {
            return Err(Error::NoCard);
        }
        // FLAG bit 2 reports an error from the previous write transaction.
        if flags & 0x04 != 0
            || id1 != ID1
            || ack1 != ACK1
            || emsb != msb
            || elsb != lsb
            || end != END_GOOD
        {
            return Err(Error::Protocol);
        }
        let mut want = msb ^ lsb;
        for &b in out.iter() {
            want ^= b;
        }
        if want != chk {
            return Err(Error::BadChecksum);
        }
        Ok(())
    }

    fn write_frame(&mut self, frame: u16, data: &[u8; FRAME_SIZE]) -> Result<()> {
        let msb = (frame >> 8) as u8;
        let lsb = frame as u8;
        let mut chk = msb ^ lsb;
        for &b in data.iter() {
            chk ^= b;
        }

        self.select();
        self.xfer(CARD_SELECT, true); // flag
        self.bus.delay(FIRST_COMMAND_SETTLE_SPINS);
        self.xfer(CMD_WRITE, true);
        let id1 = self.xfer(0x00, true); // 0x5A
        let _id2 = self.xfer(0x00, true); // 0x5D
        let _ = self.xfer(msb, true); // 0x00
        let _ = self.xfer(lsb, true); // MSB echo
        for &b in data.iter() {
            self.xfer(b, true);
        }
        self.xfer(chk, true); // commit happens here (flash write)
        let ack1 = self.xfer(0x00, true); // 0x5C
        let _ack2 = self.xfer(0x00, true); // 0x5D
        let end = self.xfer(0x00, false); // terminator
        self.end();

        // Leave the card idle while its non-volatile write finishes. Starting
        // another transaction too early can produce a protocol-successful
        // readback while still losing directory updates across a power cycle.
        self.bus.delay(POST_WRITE_SETTLE_SPINS);

        if id1 == 0xFF {
            return Err(Error::NoCard);
        }
        if id1 != ID1 || ack1 != ACK1 {
            return Err(Error::Protocol);
        }
        if end != END_GOOD {
            // 0x4E = bad checksum / not written.
            return Err(Error::Protocol);
        }
        Ok(())
    }
}

/// Run one frame transaction on `bus`, leaving its trace in `trace`.
fn transact<T: Transport, R>(
    bus: &mut T,
    slot: Slot,
    timing: Timing,
    trace: &mut TransportTrace,
    run: impl FnOnce(&mut Link<'_, T>) -> R,
) -> R {
    let mut link = Link {
        bus,
        slot,
        timing,
        trace: TransportTrace::new(),
    };
    let result = run(&mut link);
    *trace = link.trace;
    result
}

fn check_range(frame: u16) -> Result<()> {
    if (frame as usize) < FRAME_COUNT {
        Ok(())
    } else {
        Err(Error::OutOfRange)
    }
}

impl<P: BorrowMut<ControllerPort>> Block for HardwareCard<P> {
    fn read_frame(&mut self, frame: u16, out: &mut [u8; FRAME_SIZE]) -> Result<()> {
        check_range(frame)?;
        let (slot, timing) = (self.slot, self.timing);
        transact(
            self.port.borrow_mut(),
            slot,
            timing,
            &mut self.trace,
            |link| link.read_frame(frame, out),
        )
    }

    fn write_frame(&mut self, frame: u16, data: &[u8; FRAME_SIZE]) -> Result<()> {
        check_range(frame)?;
        let (slot, timing) = (self.slot, self.timing);
        transact(
            self.port.borrow_mut(),
            slot,
            timing,
            &mut self.trace,
            |link| link.write_frame(frame, data),
        )
    }
}

#[cfg(test)]
mod tests;
