// SPDX-License-Identifier: GPL-2.0-or-later
//! On-device SIO0 transport for a physical memory card (feature `hw`).
//!
//! Talks the raw card protocol byte by byte, the same style as
//! [`psx_pad`](../psx_pad): assert the port, clock `0x81` to address the card,
//! then a Read (`0x52`) or Write (`0x57`) command frame. Register usage matches
//! the proven pad path (MODE `8N1`, BAUD `0x88`, a setup delay after select for
//! the strict SCPH-1200), with per-byte `/ACK` pacing because a card's write
//! commit stalls between bytes while flash settles.
//!
//! Timing is exposed as a runtime knob ([`HardwareCard::with_timing`]) because
//! real silicon varies: the emulator ACKs instantly, an official card needs the
//! setup delay, and a write commit may need a longer ACK wait than a pad byte.
//!
//! [`HardwareCard::last_trace`] exposes bounded transport diagnostics. In
//! particular, an `/ACK` timeout is never silently treated as a successful
//! exchange: the rest of that transaction is aborted and the first failing byte
//! remains visible to an on-console diagnostic.

use crate::{Block, Error, Result, FRAME_COUNT, FRAME_SIZE};
use psx_hw::sio::sio0;
use psx_io::sio;

// SIO0 register layout: `psx_hw::sio::sio0` is the single source of truth,
// shared with psx-pad. Only protocol bytes and timing stay local.
const CTRL_ACK: u16 = sio0::ctrl::ACK;

const STAT_TX_READY: u32 = sio0::stat::TX_READY;
const STAT_RX_NOT_EMPTY: u32 = sio0::stat::RX_NOT_EMPTY;
const STAT_DSR_LEVEL: u32 = sio0::stat::DSR_LEVEL;
const STAT_IRQ: u32 = sio0::stat::IRQ;

const MODE_8N1: u16 = sio0::MODE_8N1;
const BAUD: u16 = sio0::BAUD_250KHZ;

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
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Slot {
    /// Port 1.
    One,
    /// Port 2.
    Two,
}

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
        Timing {
            setup_spins: 1_024,
            byte_spins: 32_768,
            // Generous: a card's write-commit ACK can lag a plain pad byte.
            ack_spins: 200_000,
        }
    }
}

/// A physical memory card reached over SIO0.
pub struct HardwareCard {
    port2: bool,
    timing: Timing,
    trace: TransportTrace,
}

impl HardwareCard {
    /// A card on the given `slot` with default timing.
    pub fn new(slot: Slot) -> Self {
        HardwareCard {
            port2: slot == Slot::Two,
            timing: Timing::default(),
            trace: TransportTrace::new(),
        }
    }

    /// A card with custom [`Timing`] (for tuning against real silicon).
    pub fn with_timing(slot: Slot, timing: Timing) -> Self {
        HardwareCard {
            port2: slot == Slot::Two,
            timing,
            trace: TransportTrace::new(),
        }
    }

    /// Diagnostic evidence from the most recent frame read or write.
    pub fn last_trace(&self) -> TransportTrace {
        self.trace
    }

    fn active_ctrl(&self) -> u16 {
        sio0::selected_ctrl(self.port2, true)
    }

    fn begin(&mut self) {
        self.trace = TransportTrace::new();
    }

    fn select(&self) {
        unsafe {
            psx_io::write16(sio::MODE, MODE_8N1);
            psx_io::write16(sio::BAUD, BAUD);
            // Clear the stale IRQ latch while /CS is high, then select the card.
            psx_io::write16(sio::CTRL, CTRL_ACK);
            psx_io::write16(sio::CTRL, self.active_ctrl());
        }
        self.spin(self.timing.setup_spins);
        self.drain_rx();
    }

    fn deselect(&self) {
        unsafe { psx_io::write16(sio::CTRL, 0) };
    }

    fn drain_rx(&self) {
        let mut n = 0;
        while unsafe { psx_io::read32(sio::STAT) } & STAT_RX_NOT_EMPTY != 0 && n < 16 {
            let _ = unsafe { psx_io::read8(sio::DATA) };
            n += 1;
        }
    }

    fn spin(&self, mut n: u32) {
        while n > 0 {
            let _ = unsafe { psx_io::read32(sio::STAT) };
            n -= 1;
            core::hint::spin_loop();
        }
    }

    fn wait_high(&self, mask: u32, mut spins: u32) -> bool {
        while unsafe { psx_io::read32(sio::STAT) } & mask == 0 {
            if spins == 0 {
                return false;
            }
            spins -= 1;
            core::hint::spin_loop();
        }
        true
    }

    fn wait_low(&self, mask: u32, mut spins: u32) -> bool {
        while unsafe { psx_io::read32(sio::STAT) } & mask != 0 {
            if spins == 0 {
                return false;
            }
            spins -= 1;
            core::hint::spin_loop();
        }
        true
    }

    /// Clock one byte and, when `wait_ack`, block for the card's `/ACK` pulse so
    /// the next byte is not clocked early. Returns the received byte.
    fn record_fault(&mut self, fault: TransportFault, exchange: u16) {
        if self.trace.fault == TransportFault::None {
            self.trace.fault = fault;
            self.trace.fault_exchange = exchange;
        }
    }

    fn xfer(&mut self, tx: u8, wait_ack: bool) -> u8 {
        if self.trace.fault != TransportFault::None {
            return 0xff;
        }
        let exchange = self.trace.exchanges;
        if !self.wait_high(STAT_TX_READY, self.timing.byte_spins) {
            self.record_fault(TransportFault::TxTimeout, exchange);
            return 0xff;
        }
        unsafe { psx_io::write8(sio::DATA, tx) };
        if !self.wait_high(STAT_RX_NOT_EMPTY, self.timing.byte_spins) {
            self.record_fault(TransportFault::RxTimeout, exchange);
            return 0xff;
        }
        let rx = unsafe { psx_io::read8(sio::DATA) };
        if (exchange as usize) < self.trace.response_prefix.len() {
            self.trace.response_prefix[exchange as usize] = rx;
        }
        self.trace.exchanges = exchange + 1;
        if wait_ack {
            if !self.wait_high(STAT_IRQ, self.timing.ack_spins) {
                self.record_fault(TransportFault::AckTimeout, exchange);
                return rx;
            }
            self.trace.acknowledgements += 1;
            // STAT.9 clears only after the live /ACK releases; then pulse CTRL.ACK
            // while keeping the port selected for the next byte.
            if !self.wait_low(STAT_DSR_LEVEL, self.timing.ack_spins) {
                self.record_fault(TransportFault::AckReleaseTimeout, exchange);
                return rx;
            }
            unsafe { psx_io::write16(sio::CTRL, self.active_ctrl() | CTRL_ACK) };
        }
        rx
    }

    fn check_range(frame: u16) -> Result<()> {
        if (frame as usize) < FRAME_COUNT {
            Ok(())
        } else {
            Err(Error::OutOfRange)
        }
    }
}

impl Block for HardwareCard {
    fn read_frame(&mut self, frame: u16, out: &mut [u8; FRAME_SIZE]) -> Result<()> {
        Self::check_range(frame)?;
        self.begin();
        let msb = (frame >> 8) as u8;
        let lsb = frame as u8;

        self.select();
        self.xfer(CARD_SELECT, true); // open bus
        self.spin(FIRST_COMMAND_SETTLE_SPINS);
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
        self.deselect();

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
        Self::check_range(frame)?;
        self.begin();
        let msb = (frame >> 8) as u8;
        let lsb = frame as u8;
        let mut chk = msb ^ lsb;
        for &b in data.iter() {
            chk ^= b;
        }

        self.select();
        self.xfer(CARD_SELECT, true); // flag
        self.spin(FIRST_COMMAND_SETTLE_SPINS);
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
        self.deselect();

        // Leave the card idle while its non-volatile write finishes. Starting
        // another transaction too early can produce a protocol-successful
        // readback while still losing directory updates across a power cycle.
        self.spin(POST_WRITE_SETTLE_SPINS);

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
