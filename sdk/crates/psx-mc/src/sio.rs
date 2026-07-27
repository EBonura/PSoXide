// SPDX-License-Identifier: GPL-2.0-or-later
//! On-device SIO0 transport for a physical memory card (feature `hw`).
//!
//! Talks the raw card protocol byte by byte, the same style as
//! [`psx_pad`](../psx_pad): assert the port, clock `0x81` to address the card,
//! then a Read (`0x52`) or Write (`0x57`) command frame. Register usage matches
//! the proven pad path (MODE `8N1`, BAUD `0x88`, a setup delay after select for
//! the strict SCPH-1200).
//!
//! ## Two pacing mechanisms ([`AckMode`])
//!
//! Timing is exposed as a runtime knob ([`HardwareCard::with_timing`],
//! [`HardwareCard::with_noack`]) because real silicon varies. Console
//! captures (2026-07-26) swept [`Timing`]'s bounds across a 1000x range under
//! [`AckMode::Irq`] with zero full-frame successes -- which reads less like
//! "needs a bigger constant" and more like a wrong mechanism. `psx_pad`'s own
//! `active_ctrl` doc comment already records that arming `CTRL_ACK_IRQ_EN` --
//! which `AckMode::Irq` does on every byte -- "turned out to disturb
//! real-hardware transfers (the official SCPH-1200 stopped answering, the
//! clone's bytes corrupted)" on this exact console family, which is why the
//! pad driver's default poll avoids it entirely. [`AckMode::NoAck`] does the
//! same for cards: no IRQ, no per-byte handshake, a fixed delay after every
//! byte instead. Console validation the same day: 24/24 probes, every field,
//! 100% correct at `setup_spins = 1_024, byte_spins = 32_768,
//! interbyte_spins = 8_000` -- the first configuration in the whole
//! investigation to read a real frame perfectly and repeatably.
//!
//! Verified against the PSoXide emulator's card model (which only exercises
//! [`AckMode::Irq`], the original mechanism); [`AckMode::NoAck`]'s constants
//! are pending further console validation, particularly for writes (a
//! card's flash-write commit stalls longer than a plain read byte, and
//! `interbyte_spins` applies uniformly rather than adaptively waiting for
//! `/ACK` the way `AckMode::Irq` did).

use crate::{Block, Error, Result, FRAME_COUNT, FRAME_SIZE};
use psx_hw::sio::sio0;
use psx_io::sio;

// SIO0 register layout: `psx_hw::sio::sio0` is the single source of truth,
// shared with psx-pad. Only protocol bytes and timing stay local.
const CTRL_TXEN: u16 = sio0::ctrl::TXEN;
const CTRL_DTR: u16 = sio0::ctrl::DTR;
const CTRL_RXEN: u16 = sio0::ctrl::RXEN;
const CTRL_ACK: u16 = sio0::ctrl::ACK;
const CTRL_ACK_IRQ_EN: u16 = sio0::ctrl::ACK_IRQ_EN;
const CTRL_SLOT_PORT2: u16 = sio0::ctrl::SLOT_PORT2;

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

/// Which controller/card port to use.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Slot {
    /// Port 1.
    One,
    /// Port 2.
    Two,
}

/// Tunable transfer timing (bounded MMIO spin counts) for [`AckMode::Irq`].
/// Defaults mirror the pad.
#[derive(Copy, Clone, Debug)]
pub struct Timing {
    /// Delay after asserting select, before the first byte (strict-pad setup).
    pub setup_spins: u32,
    /// Bound on waiting for TX-ready / RX-filled per byte.
    pub byte_spins: u32,
    /// Bound on waiting for the `/ACK` pulse (raise for slow write commits).
    pub ack_spins: u32,
    /// Extra unconditional delay inserted right before clocking the frame's
    /// terminator byte, after the previous byte's `/ACK` handshake. The
    /// terminator is the one byte in the sequence clocked with `wait_ack =
    /// false` (real cards do not pulse `/ACK` after it), so none of the
    /// other three knobs add any wait in that specific gap.
    pub end_delay_spins: u32,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            setup_spins: 1_024,
            byte_spins: 32_768,
            // Generous: a card's write-commit ACK can lag a plain pad byte.
            ack_spins: 200_000,
            end_delay_spins: 0,
        }
    }
}

/// Which per-byte pacing mechanism [`HardwareCard`] drives SIO0 with. See
/// the module doc for the console evidence behind having two.
#[derive(Copy, Clone, Debug)]
pub enum AckMode {
    /// Per-byte `/ACK` IRQ-latch + DSR-wait + CTRL.ACK pulse. The original
    /// mechanism; matches the emulator's card model.
    Irq(Timing),
    /// No `CTRL_ACK_IRQ_EN`, no per-byte handshake -- a fixed
    /// `interbyte_spins` delay after every byte instead, matching
    /// `psx_pad`'s real-hardware-proven default pacing.
    NoAck {
        /// Delay after asserting select, before the first byte.
        setup_spins: u32,
        /// Bound on waiting for TX-ready / RX-filled per byte.
        byte_spins: u32,
        /// Fixed delay applied after every acknowledged byte (i.e. every
        /// byte except the frame terminator).
        interbyte_spins: u32,
        /// Delay applied right after deselecting, before the card can be
        /// selected again. A single probe button-press has a large natural
        /// gap before the next one (human reaction time, frame boundaries),
        /// which every `probe_read` test so far has unknowingly relied on;
        /// `Card::write`/`format` instead issue several `read_frame`/
        /// `write_frame` calls back-to-back with no such gap (directory
        /// scan, header, icon, one write per allocated block). Console
        /// captures (2026-07-26): `probe_read` alone hit 100% pass at
        /// `interbyte_spins = 8_000` with `deselect_spins = 0`, but a real
        /// `Card::write` at the same timing failed immediately with
        /// `NoCard` -- consistent with the card not having reset its
        /// receiver before the very next transaction re-selects it.
        deselect_spins: u32,
        /// Extra delay applied only after a completed [`Block::write_frame`]
        /// (not after reads). PSn00bSDK's reference card driver
        /// (`indev/psxpad/card.s`) documents this exact requirement in a
        /// comment on its write routine: "you must wait at least two vsyncs
        /// between each sector write" -- roughly 33ms, far longer than
        /// `deselect_spins` was ever sized for. `deselect_spins` resets the
        /// card's receiver between any two transactions; this is a separate,
        /// much longer requirement specific to letting one sector's flash
        /// write actually finish committing before the next one starts.
        /// Console captures (2026-07-26): individual `write_frame` calls
        /// each reported `Ok` and an immediate same-session read-back
        /// verified byte-perfect, yet a real multi-frame `Card::write`
        /// (directory entry, title, icon, each data block -- several
        /// sector writes back-to-back) never appeared in the BIOS card
        /// manager. That combination is consistent with each write
        /// succeeding at the protocol level while the prior sector's flash
        /// commit was still being interrupted by the next command -- this
        /// field exists to test that theory.
        write_gap_spins: u32,
    },
}

/// Raw protocol bytes from one [`HardwareCard::probe_read`] call, for
/// diagnosing a `Protocol`/`NoCard` result against real silicon without
/// guessing from the collapsed [`Error`]. Expected-good values: `id1 = 0x5A`,
/// `ack1 = 0x5C`, `emsb`/`elsb` echo the requested frame address, `end =
/// 0x47`, `chk == want_chk`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadDiag {
    /// First ID byte (want `0x5A`).
    pub id1: u8,
    /// Second ID byte (want `0x5D`).
    pub id2: u8,
    /// Address MSB echoed back by the card (want the requested frame's MSB).
    pub emsb: u8,
    /// Address LSB echoed back by the card (want the requested frame's LSB).
    pub elsb: u8,
    /// First ack byte (want `0x5C`).
    pub ack1: u8,
    /// Second ack byte (want `0x5D`).
    pub ack2: u8,
    /// Terminator byte (want `0x47`).
    pub end: u8,
    /// Checksum byte the card sent.
    pub chk: u8,
    /// Checksum this side computed from the address + payload.
    pub want_chk: u8,
}

/// A physical memory card reached over SIO0.
pub struct HardwareCard {
    port2: bool,
    mode: AckMode,
}

impl HardwareCard {
    /// A card on the given `slot`, using [`AckMode::Irq`] with default
    /// [`Timing`].
    pub fn new(slot: Slot) -> Self {
        HardwareCard {
            port2: slot == Slot::Two,
            mode: AckMode::Irq(Timing::default()),
        }
    }

    /// A card using [`AckMode::Irq`] with custom [`Timing`] (for tuning
    /// against real silicon).
    pub fn with_timing(slot: Slot, timing: Timing) -> Self {
        HardwareCard {
            port2: slot == Slot::Two,
            mode: AckMode::Irq(timing),
        }
    }

    /// A card using [`AckMode::NoAck`] -- see the module doc for why this
    /// exists alongside [`Timing`]-tuned [`AckMode::Irq`].
    #[allow(clippy::too_many_arguments)]
    pub fn with_noack(
        slot: Slot,
        setup_spins: u32,
        byte_spins: u32,
        interbyte_spins: u32,
        deselect_spins: u32,
        write_gap_spins: u32,
    ) -> Self {
        HardwareCard {
            port2: slot == Slot::Two,
            mode: AckMode::NoAck {
                setup_spins,
                byte_spins,
                interbyte_spins,
                deselect_spins,
                write_gap_spins,
            },
        }
    }

    fn active_ctrl_irq(&self) -> u16 {
        let slot = if self.port2 { CTRL_SLOT_PORT2 } else { 0 };
        slot | CTRL_TXEN | CTRL_RXEN | CTRL_DTR | CTRL_ACK_IRQ_EN
    }

    fn active_ctrl_noack(&self) -> u16 {
        let slot = if self.port2 { CTRL_SLOT_PORT2 } else { 0 };
        slot | CTRL_TXEN | CTRL_RXEN | CTRL_DTR
    }

    fn select(&self) {
        let (setup_spins, ctrl) = match self.mode {
            AckMode::Irq(t) => (t.setup_spins, self.active_ctrl_irq()),
            AckMode::NoAck { setup_spins, .. } => (setup_spins, self.active_ctrl_noack()),
        };
        unsafe {
            psx_io::write16(sio::MODE, MODE_8N1);
            psx_io::write16(sio::BAUD, BAUD);
            psx_io::write16(sio::CTRL, CTRL_ACK); // clear stale IRQ latch
            psx_io::write16(sio::CTRL, ctrl);
        }
        self.spin(setup_spins);
        self.drain_rx();
    }

    fn deselect(&self) {
        unsafe { psx_io::write16(sio::CTRL, 0) };
        if let AckMode::NoAck { deselect_spins, .. } = self.mode {
            self.spin(deselect_spins);
        }
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

    /// Clock one byte. `wait_ack` means "this is not the frame terminator";
    /// under [`AckMode::Irq`] that blocks for the card's `/ACK` pulse before
    /// returning, and under [`AckMode::NoAck`] it applies the fixed
    /// `interbyte_spins` delay instead. Returns the received byte.
    fn xfer(&self, tx: u8, wait_ack: bool) -> u8 {
        let byte_spins = match self.mode {
            AckMode::Irq(t) => t.byte_spins,
            AckMode::NoAck { byte_spins, .. } => byte_spins,
        };
        if !self.wait_high(STAT_TX_READY, byte_spins) {
            return 0xFF;
        }
        unsafe { psx_io::write8(sio::DATA, tx) };
        if !self.wait_high(STAT_RX_NOT_EMPTY, byte_spins) {
            return 0xFF;
        }
        let rx = unsafe { psx_io::read8(sio::DATA) };
        match self.mode {
            AckMode::Irq(t) => {
                if wait_ack && self.wait_high(STAT_IRQ, t.ack_spins) {
                    // STAT.9 clears only after the live /ACK releases; then pulse
                    // CTRL.ACK while keeping the port selected for the next byte.
                    let _ = self.wait_low(STAT_DSR_LEVEL, t.ack_spins);
                    unsafe { psx_io::write16(sio::CTRL, self.active_ctrl_irq() | CTRL_ACK) };
                }
            }
            AckMode::NoAck { interbyte_spins, .. } => {
                if wait_ack {
                    self.spin(interbyte_spins);
                }
            }
        }
        rx
    }

    /// The `end_delay_spins` pause before the terminator byte only applies
    /// under [`AckMode::Irq`] -- `AckMode::NoAck` never benefited from it
    /// (console validation, 2026-07-26: it made `END` uniformly worse).
    fn end_delay(&self) {
        if let AckMode::Irq(t) = self.mode {
            self.spin(t.end_delay_spins);
        }
    }

    /// See [`AckMode::NoAck`]'s `write_gap_spins` doc -- called only after a
    /// completed [`Block::write_frame`], never after a read.
    fn write_gap(&self) {
        if let AckMode::NoAck { write_gap_spins, .. } = self.mode {
            self.spin(write_gap_spins);
        }
    }

    fn check_range(frame: u16) -> Result<()> {
        if (frame as usize) < FRAME_COUNT {
            Ok(())
        } else {
            Err(Error::OutOfRange)
        }
    }

    /// Read one frame with the exact same wire sequence as
    /// [`Block::read_frame`] (using whichever [`AckMode`] this card was
    /// constructed with), but return every raw protocol byte instead of
    /// collapsing them into an [`Error`]. Read-only and side-effect-free on
    /// the card, so it is safe to call repeatedly -- e.g. against frame 0
    /// (the directory) -- while sweeping timing against real hardware.
    pub fn probe_read(&mut self, frame: u16) -> (Result<()>, ReadDiag) {
        let mut diag = ReadDiag::default();
        if let Err(e) = Self::check_range(frame) {
            return (Err(e), diag);
        }
        let msb = (frame >> 8) as u8;
        let lsb = frame as u8;
        let mut out = [0u8; FRAME_SIZE];

        self.select();
        self.xfer(CARD_SELECT, true); // flag
        self.xfer(CMD_READ, true);
        diag.id1 = self.xfer(0x00, true); // 0x5A
        diag.id2 = self.xfer(0x00, true); // 0x5D
        let _ = self.xfer(msb, true);
        let _ = self.xfer(lsb, true);
        diag.ack1 = self.xfer(0x00, true); // 0x5C
        diag.ack2 = self.xfer(0x00, true); // 0x5D
        diag.emsb = self.xfer(0x00, true); // echoes msb
        diag.elsb = self.xfer(0x00, true); // echoes lsb
        for b in out.iter_mut() {
            *b = self.xfer(0x00, true);
        }
        diag.chk = self.xfer(0x00, true);
        self.end_delay();
        diag.end = self.xfer(0x00, false); // terminator, no ACK
        self.deselect();

        diag.want_chk = msb ^ lsb;
        for &b in out.iter() {
            diag.want_chk ^= b;
        }

        let result = if diag.id1 == 0xFF {
            Err(Error::NoCard)
        } else if diag.id1 != ID1
            || diag.ack1 != ACK1
            || diag.emsb != msb
            || diag.elsb != lsb
            || diag.end != END_GOOD
        {
            Err(Error::Protocol)
        } else if diag.chk != diag.want_chk {
            Err(Error::BadChecksum)
        } else {
            Ok(())
        };
        (result, diag)
    }
}

impl Block for HardwareCard {
    fn read_frame(&mut self, frame: u16, out: &mut [u8; FRAME_SIZE]) -> Result<()> {
        Self::check_range(frame)?;
        let msb = (frame >> 8) as u8;
        let lsb = frame as u8;

        self.select();
        self.xfer(CARD_SELECT, true); // flag
        self.xfer(CMD_READ, true);
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
        self.end_delay();
        let end = self.xfer(0x00, false); // terminator, no ACK
        self.deselect();

        if id1 == 0xFF {
            return Err(Error::NoCard);
        }
        if id1 != ID1 || ack1 != ACK1 || emsb != msb || elsb != lsb || end != END_GOOD {
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
        let msb = (frame >> 8) as u8;
        let lsb = frame as u8;
        let mut chk = msb ^ lsb;
        for &b in data.iter() {
            chk ^= b;
        }

        self.select();
        self.xfer(CARD_SELECT, true); // flag
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
        self.end_delay();
        let end = self.xfer(0x00, false); // terminator
        self.deselect();
        // Applied regardless of the outcome below: if the card started a
        // flash commit at all, the next transaction (whatever it is)
        // shouldn't interrupt it.
        self.write_gap();

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
