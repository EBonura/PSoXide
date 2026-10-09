// SPDX-License-Identifier: GPL-2.0-or-later
//! The interrupt-driven pad engine: its state machine, with no hardware in it.
//!
//! The synchronous driver in the crate root spends the CPU's time waiting. A
//! poll holds a setup delay of 1,024 status reads after raising `/CS`, then
//! spins on `/ACK` after every byte, and a socket with nothing in it spends
//! the whole delay and the whole `/ACK` budget to learn that. This engine
//! never waits. A frame's VBlank starts one transaction per port; every step
//! after that is an interrupt, and between interrupts the CPU runs the game.
//!
//! ```text
//!   VBlank ──► select port ──► arm timer(setup)
//!   timer  ──► transmit byte 0 ──► arm timer(ack budget)
//!   IRQ7   ──► read reply, clear latch, transmit next byte ──► arm timer
//!   ...        (the last byte has no ACK: the timer ends it)
//!   done   ──► deselect, publish a snapshot, start the next port
//! ```
//!
//! What stops a byte that never comes is the timer, not a loop: an absent
//! pad answers `0xFF` and never pulses `/ACK`, so the first byte's budget
//! runs out and the port reads as absent. That costs wall time on the wire
//! (a fraction of a millisecond) and almost nothing on the CPU, and a pad
//! plugged in later is found by the next VBlank's transaction.
//!
//! The game reads a [`Snapshot`] (a sequence number and, per port, the last
//! clean [`PadState`] and whether the port is present) without disturbing the
//! engine: [`Published::read`] is a copy.
//!
//! This module is the policy, generic over [`Hw`] so the host tests drive it
//! against a controller model with real timing; the console side (the
//! exception wrapper, the timer, the registers) is in `console.rs`.
//! `sdk/docs/PAD-IRQ-ENGINE.md` is the design.

use crate::{decode_buttons, mode_from_id_low, AnalogRequirement, AnalogSticks, PadMode, PadState};
use core::cell::UnsafeCell;
use core::ptr::{read_volatile, write_volatile};
use psx_hw::sio::sio0::stat;
use psx_io::controller_port::Port;

/// How the engine decides a byte is done and the next may go.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BytePacing {
    /// The device's `/ACK` pulse, delivered as IRQ7, advances the packet; the
    /// timer is only the limit for a byte that is never acknowledged. This
    /// is how the BIOS paces a pad and adapts to a slow unit. Each byte
    /// costs one write of the control register to clear the latched IRQ.
    Ack,
    /// A fixed gap after each byte. The `/ACK` interrupt is never armed and
    /// no control register is written between bytes, the wire pattern the
    /// pre-ACK driver was proven with on three pad models. A pad slower to
    /// acknowledge than [`Config::timed_gap_cycles`] slips the packet.
    Timed,
}

/// The engine's settings.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// How bytes are paced.
    pub pacing: BytePacing,
    /// VBlanks between transactions: 1 polls every frame, 2 every other.
    pub kick_every: u8,
    /// Which ports are polled, `[port 1, port 2]`.
    pub ports: [bool; 2],
    /// CPU cycles from raising `/CS` to the first byte. The original SCPH-1200
    /// answers nothing without it (silicon, 2026-06-22); the synchronous
    /// driver's 1,024 status reads are about 6,900 cycles.
    pub setup_cycles: u32,
    /// CPU cycles after a byte starts within which its `/ACK` must arrive,
    /// the byte's own shift included. The synchronous driver allows 2,048
    /// status reads after the reply, about 13,700 cycles.
    pub ack_cycles: u32,
    /// CPU cycles after the final byte starts at which its reply is read; the
    /// device does not acknowledge it. The byte itself shifts in 1,088.
    pub last_byte_cycles: u32,
    /// [`BytePacing::Timed`] only: CPU cycles after a byte starts before
    /// the next one goes.
    pub timed_gap_cycles: u32,
}

impl Config {
    /// Both ports every frame, paced on `/ACK`.
    pub const DEFAULT: Config = Config {
        pacing: BytePacing::Ack,
        kick_every: 1,
        ports: [true, true],
        setup_cycles: 7_000,
        ack_cycles: 14_800,
        last_byte_cycles: 3_000,
        timed_gap_cycles: 4_500,
    };

    /// [`DEFAULT`](Self::DEFAULT) for a game with one controller.
    pub const PORT1_ONLY: Config = Config {
        ports: [true, false],
        ..Config::DEFAULT
    };
}

impl Default for Config {
    fn default() -> Self {
        Config::DEFAULT
    }
}

/// What the engine needs from the console: the serial port, and one timer
/// that raises an interrupt after a number of CPU cycles.
pub trait Hw {
    /// The status register.
    fn status(&mut self) -> u32;
    /// Configure 250 kHz 8N1, clear a stale latched IRQ, and assert `/CS` on
    /// `port` with the `/ACK` interrupt armed when `ack_irq`.
    fn select(&mut self, port: Port, ack_irq: bool);
    /// Release `/CS`.
    fn deselect(&mut self);
    /// Reset the UART, so a reply still in flight from an abandoned packet
    /// cannot be taken for the next one's.
    fn reset_uart(&mut self);
    /// Pop stale bytes from the receive FIFO.
    fn drain_receive(&mut self);
    /// Start shifting one byte out.
    fn transmit(&mut self, byte: u8);
    /// Pop one received byte.
    fn receive(&mut self) -> u8;
    /// Wait, a few microseconds at most, for a pulse of `/ACK` already seen to
    /// end. `false` when the line stays asserted.
    fn wait_ack_release(&mut self) -> bool;
    /// Clear the latched `/ACK` IRQ so the next pulse raises IRQ7 again,
    /// leaving `/CS` asserted on `port`, or released when `None`.
    fn clear_ack_latch(&mut self, port: Option<Port>, ack_irq: bool);
    /// Raise the timer interrupt in `cycles` CPU cycles, once, replacing any
    /// deadline already armed.
    fn arm_deadline(&mut self, cycles: u32);
    /// Disarm the deadline.
    fn cancel_deadline(&mut self);
    /// Whether the armed deadline has been reached. An interrupt left over from
    /// a deadline that was cancelled or replaced answers `false`.
    fn deadline_expired(&mut self) -> bool;
}

/// What the engine last learned about a port.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Health {
    /// No transaction has finished on this port yet.
    Unseen,
    /// A device answered; [`PortReading::pad`] is its latest clean state.
    Present,
    /// Nothing answered: an empty socket.
    Absent,
    /// The latest transaction failed part way. [`PortReading::pad`] still holds
    /// the last clean state, so a held button is not released and pressed
    /// again.
    Faulted,
}

/// One port, as the game sees it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PortReading {
    /// The latest clean state: the pad's, or [`PadState::NONE`] for an empty
    /// socket.
    pub pad: PadState,
    /// What the latest transaction found.
    pub health: Health,
    /// Transactions that read a pad (or an empty socket) cleanly.
    pub updates: u32,
    /// Transactions that failed part way.
    pub faults: u32,
    /// Why the latest failed transaction failed; kept after a clean one.
    pub last_fault: Option<Fault>,
}

impl PortReading {
    /// A port nothing has been read from.
    pub const UNSEEN: PortReading = PortReading {
        pad: PadState::NONE,
        health: Health::Unseen,
        updates: 0,
        faults: 0,
        last_fault: None,
    };
}

/// Both ports at one instant, with the sequence number of the publication.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// Publications so far; changes whenever a port was updated.
    pub seq: u32,
    /// `[port 1, port 2]`.
    pub ports: [PortReading; 2],
}

impl Snapshot {
    /// The reading for `port`.
    pub const fn port(&self, port: Port) -> &PortReading {
        match port {
            Port::One => &self.ports[0],
            Port::Two => &self.ports[1],
        }
    }

    /// The latest clean pad state for `port`.
    pub const fn pad(&self, port: Port) -> PadState {
        self.port(port).pad
    }
}

/// Where the engine publishes: two buffers and a sequence number, so a reader
/// in the foreground gets a consistent copy while the interrupt handler
/// publishes, with no lock and no interrupt masking.
///
/// One writer (the engine) and any number of foreground readers, on one CPU.
pub struct Published {
    seq: UnsafeCell<u32>,
    slots: UnsafeCell<[[PortReading; 2]; 2]>,
}

// SAFETY: the single writer is the engine, in interrupt context or with
// interrupts off; readers copy under the sequence check in `read`, which
// retries if the writer reused the slot they were copying.
unsafe impl Sync for Published {}

impl Published {
    /// Nothing published.
    pub const fn new() -> Self {
        Published {
            seq: UnsafeCell::new(0),
            slots: UnsafeCell::new([[PortReading::UNSEEN; 2]; 2]),
        }
    }

    /// Publish `ports`. Only the engine calls this.
    fn publish(&self, ports: &[PortReading; 2]) {
        // SAFETY: the writer is the engine alone (see the `Sync` impl); the
        // slot written is the one readers do not take until `seq` moves.
        unsafe {
            let seq = read_volatile(self.seq.get());
            let next = seq.wrapping_add(1);
            write_volatile(&mut (*self.slots.get())[(next & 1) as usize], *ports);
            psx_io::dma::compiler_barrier();
            write_volatile(self.seq.get(), next);
        }
    }

    /// A consistent copy of the latest publication.
    pub fn read(&self) -> Snapshot {
        loop {
            // SAFETY: volatile reads of the sequence number and of the slot it
            // names; the slot is re-validated against the sequence number
            // after the copy, so a torn read is discarded.
            let (seq, ports, after) = unsafe {
                let seq = read_volatile(self.seq.get());
                psx_io::dma::compiler_barrier();
                let ports = read_volatile(&(*self.slots.get())[(seq & 1) as usize]);
                psx_io::dma::compiler_barrier();
                (seq, ports, read_volatile(self.seq.get()))
            };
            // The writer fills the other slot first; it only touches this one
            // again on the second publication after `seq`.
            if after.wrapping_sub(seq) < 2 {
                return Snapshot { seq, ports };
            }
        }
    }
}

impl Default for Published {
    fn default() -> Self {
        Self::new()
    }
}

/// Counters, for tools and for tests.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Interrupts the engine handled (ACK and deadline).
    pub events: u32,
    /// VBlanks that started a round of transactions.
    pub kicks: u32,
    /// VBlanks skipped because the port was leased out.
    pub leased_skips: u32,
    /// Rounds abandoned because the previous one was still running at the
    /// next VBlank: a round is under a millisecond and a half on the wire, so
    /// an interrupt was lost.
    pub stalls: u32,
    /// `/ACK` interrupts that arrived with no byte waiting for one.
    pub spurious: u32,
}

/// Who may use the serial port.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Owner {
    /// The engine, which polls on every VBlank.
    Engine,
    /// A foreground driver (a memory card) holds the port; the engine does
    /// not touch it, and VBlanks that come meanwhile are not polled.
    Lease,
}

/// Why a transaction failed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The port would not take the byte.
    TxBusy,
    /// `/ACK` was still asserted when it should have ended.
    AckStuck,
    /// A byte was never acknowledged.
    AckTimeout,
    /// The reply did not arrive.
    RxMissing,
    /// An ID this driver does not know.
    BadId,
    /// The ID's second byte was not `0x5A`.
    BadMagic,
}

#[derive(Copy, Clone, Debug)]
enum Kind {
    Poll,
    Config([u8; 8]),
}

/// The packet on the wire.
#[derive(Copy, Clone, Debug)]
struct Txn {
    port: Port,
    kind: Kind,
    /// The pacing in force when the transaction started: a change of
    /// [`Config`] takes effect from the next one, never half way through a
    /// packet whose select was made for the old one.
    pacing: BytePacing,
    /// The byte most recently transmitted.
    idx: u8,
    /// Exchanges in the packet, the address byte included. Nine until the ID
    /// says otherwise.
    len: u8,
    rx: [u8; 9],
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    /// `/CS` is up and the setup timer is running.
    Setup,
    /// Byte `txn.idx` is on the wire.
    Byte,
}

/// What a transaction found.
#[derive(Copy, Clone, Debug)]
enum Outcome {
    Pad(PadState),
    Absent,
    Configured,
    Failed(Fault),
}

/// The three configuration packets that put a DualShock in analog mode and
/// lock it there, one per VBlank, the spacing Sony's libpad uses.
const ANALOG_PACKETS: [[u8; 8]; 3] = [
    [0x43, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x44, 0x00, 0x01, 0x03, 0x00, 0x00, 0x00, 0x00],
    [0x43, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
];
/// Exit-config repeats for a pad still answering ID 0xF3 afterwards.
const EXIT_RETRIES: u8 = 3;
/// Failed configuration attempts before the request gives up.
const ANALOG_FAULT_LIMIT: u8 = 8;

/// A request to put a port's pad in locked analog mode, in progress.
#[derive(Copy, Clone, Debug)]
struct AnalogJob {
    /// 0 none; 1..=3 the packet to send; 4 waiting for a poll to verify.
    step: u8,
    retries: u8,
    faults: u8,
    outcome: Option<AnalogRequirement>,
}

impl AnalogJob {
    const NONE: AnalogJob = AnalogJob {
        step: 0,
        retries: 0,
        faults: 0,
        outcome: None,
    };
}

#[derive(Copy, Clone, Debug)]
struct PortCtl {
    reading: PortReading,
    job: AnalogJob,
}

/// Index of `port` in the engine's per-port arrays.
const fn index(port: Port) -> usize {
    match port {
        Port::One => 0,
        Port::Two => 1,
    }
}

/// The byte the host sends as exchange `idx` of a packet.
const fn tx_byte(kind: &Kind, idx: u8) -> u8 {
    match (kind, idx) {
        (_, 0) => 0x01,
        (Kind::Poll, 1) => 0x42,
        (Kind::Poll, _) => 0x00,
        (Kind::Config(packet), _) => packet[idx as usize - 1],
    }
}

/// The state machine. One per console, driven by three events: a VBlank
/// ([`on_vblank`](Self::on_vblank)), a device's `/ACK`
/// ([`on_ack`](Self::on_ack)) and the deadline timer
/// ([`on_deadline`](Self::on_deadline)). The three run with CPU interrupts off.
/// Everything else is for the foreground, which must not overlap them.
pub struct Engine<'p, H: Hw> {
    hw: H,
    published: &'p Published,
    config: Config,
    phase: Phase,
    txn: Txn,
    owner: Owner,
    vblanks: u8,
    ports: [PortCtl; 2],
    stats: Stats,
}

impl<'p, H: Hw> Engine<'p, H> {
    /// An idle engine that publishes to `published`.
    pub const fn new(hw: H, published: &'p Published, config: Config) -> Self {
        Engine {
            hw,
            published,
            config,
            phase: Phase::Idle,
            txn: Txn {
                port: Port::One,
                kind: Kind::Poll,
                pacing: BytePacing::Ack,
                idx: 0,
                len: 9,
                rx: [0; 9],
            },
            owner: Owner::Engine,
            vblanks: 0,
            ports: [PortCtl {
                reading: PortReading::UNSEEN,
                job: AnalogJob::NONE,
            }; 2],
            stats: Stats {
                events: 0,
                kicks: 0,
                leased_skips: 0,
                stalls: 0,
                spurious: 0,
            },
        }
    }

    /// The hardware.
    pub fn hw(&self) -> &H {
        &self.hw
    }

    /// The hardware, mutably.
    pub fn hw_mut(&mut self) -> &mut H {
        &mut self.hw
    }

    /// The settings in force.
    pub const fn config(&self) -> Config {
        self.config
    }

    /// Change the settings; they apply from the next transaction.
    pub fn configure(&mut self, config: Config) {
        self.config = config;
    }

    /// The counters.
    pub const fn stats(&self) -> Stats {
        self.stats
    }

    /// Who may use the serial port now.
    pub const fn owner(&self) -> Owner {
        self.owner
    }

    /// Whether no transaction is on the wire.
    pub fn is_idle(&self) -> bool {
        self.phase == Phase::Idle
    }

    /// Publish the current readings. A freshly built engine calls this so the
    /// snapshot a previous engine left behind does not outlive it: its
    /// counters start again from zero.
    pub fn publish_readings(&self) {
        self.published
            .publish(&[self.ports[0].reading, self.ports[1].reading]);
    }

    /// The latest publication.
    pub fn snapshot(&self) -> Snapshot {
        self.published.read()
    }

    // ------------------------------------------------------------ events

    /// A VBlank: start a round of transactions, one per enabled port, unless
    /// the port is leased. A round still running is abandoned.
    pub fn on_vblank(&mut self) {
        if self.owner == Owner::Lease {
            self.stats.leased_skips = self.stats.leased_skips.wrapping_add(1);
            return;
        }
        if self.phase != Phase::Idle {
            // A round takes well under a frame, so the last one lost an
            // interrupt. Reset the port; the next VBlank starts afresh.
            self.stats.stalls = self.stats.stalls.wrapping_add(1);
            self.abandon();
            return;
        }
        self.vblanks = self.vblanks.saturating_add(1);
        if self.vblanks < self.config.kick_every {
            return;
        }
        self.vblanks = 0;
        self.stats.kicks = self.stats.kicks.wrapping_add(1);
        self.start_port(0);
    }

    /// The device's `/ACK` pulse raised IRQ7.
    pub fn on_ack(&mut self) {
        self.note_event();
        let acking = self.txn.pacing == BytePacing::Ack;
        if !acking || self.phase != Phase::Byte {
            // Nothing is waiting for an acknowledgement: clear the latch so
            // the next one raises the interrupt, and move nothing.
            self.stats.spurious = self.stats.spurious.wrapping_add(1);
            let port = (self.phase != Phase::Idle).then_some(self.txn.port);
            self.hw.clear_ack_latch(port, acking);
            return;
        }
        if self.hw.status() & stat::RX_NOT_EMPTY == 0 {
            return self.fail(Fault::RxMissing);
        }
        let reply = self.hw.receive();
        if !self.hw.wait_ack_release() {
            return self.fail(Fault::AckStuck);
        }
        self.hw.clear_ack_latch(Some(self.txn.port), true);
        self.accept(reply);
    }

    /// The deadline timer's interrupt.
    pub fn on_deadline(&mut self) {
        if !self.hw.deadline_expired() {
            return;
        }
        self.note_event();
        match self.phase {
            Phase::Idle => {}
            Phase::Setup => self.begin(),
            Phase::Byte => self.byte_deadline(),
        }
    }

    fn note_event(&mut self) {
        self.stats.events = self.stats.events.wrapping_add(1);
    }

    // --------------------------------------------------------- the packet

    /// Start the transaction on the first enabled port from index `from`, or
    /// go idle when there is none.
    fn start_port(&mut self, from: usize) {
        let mut i = from;
        while i < 2 && !self.config.ports[i] {
            i += 1;
        }
        if i >= 2 {
            self.phase = Phase::Idle;
            return;
        }
        let port = if i == 0 { Port::One } else { Port::Two };
        let kind = match self.ports[i].job.step {
            step @ 1..=3 => Kind::Config(ANALOG_PACKETS[step as usize - 1]),
            _ => Kind::Poll,
        };
        self.txn = Txn {
            port,
            kind,
            pacing: self.config.pacing,
            idx: 0,
            len: 9,
            rx: [0; 9],
        };
        self.hw.select(port, self.txn.pacing == BytePacing::Ack);
        self.hw.arm_deadline(self.config.setup_cycles);
        self.phase = Phase::Setup;
    }

    /// The setup time has passed: send the address byte.
    fn begin(&mut self) {
        self.hw.drain_receive();
        // An `/ACK` left asserted by an abandoned packet would be taken for
        // this packet's first.
        if self.hw.status() & stat::DSR_LEVEL != 0 {
            return self.fail(Fault::AckStuck);
        }
        self.transmit(0);
    }

    fn transmit(&mut self, idx: u8) {
        if self.hw.status() & stat::TX_READY == 0 {
            return self.fail(Fault::TxBusy);
        }
        let last = idx + 1 == self.txn.len;
        self.txn.idx = idx;
        self.hw.transmit(tx_byte(&self.txn.kind, idx));
        let budget = if last {
            self.config.last_byte_cycles
        } else {
            match self.txn.pacing {
                BytePacing::Ack => self.config.ack_cycles,
                BytePacing::Timed => self.config.timed_gap_cycles,
            }
        };
        self.hw.arm_deadline(budget);
        self.phase = Phase::Byte;
    }

    /// The timer ended a byte: the reply is read either way, but only the
    /// last byte, or any byte under fixed pacing, may go unacknowledged. An
    /// address byte answered `0xFF` with no `/ACK` is an empty socket.
    fn byte_deadline(&mut self) {
        let last = self.txn.idx + 1 == self.txn.len;
        if self.hw.status() & stat::RX_NOT_EMPTY == 0 {
            return self.fail(Fault::RxMissing);
        }
        let reply = self.hw.receive();
        if last || self.txn.pacing == BytePacing::Timed {
            self.accept(reply);
        } else if self.txn.idx == 0 && reply == 0xFF {
            self.finish(Outcome::Absent);
        } else {
            self.fail(Fault::AckTimeout);
        }
    }

    /// Take the reply to byte `txn.idx` and send the next, or finish.
    fn accept(&mut self, reply: u8) {
        let idx = self.txn.idx;
        self.txn.rx[idx as usize] = reply;
        // The ID in the reply to the command byte sets the length of every
        // packet, a configuration command included: a digital pad answers a
        // nine-byte command with five bytes and stops acknowledging.
        match (idx, reply) {
            (1, id) => match mode_from_id_low(id) {
                PadMode::Digital => self.txn.len = 5,
                PadMode::Analog | PadMode::Config => self.txn.len = 9,
                PadMode::Disconnected => return self.finish(Outcome::Absent),
                PadMode::Unknown => return self.fail(Fault::BadId),
            },
            (2, magic) if magic != 0x5A => return self.fail(Fault::BadMagic),
            _ => {}
        }
        if idx + 1 >= self.txn.len {
            let outcome = match self.txn.kind {
                Kind::Poll => Outcome::Pad(self.decode()),
                Kind::Config(_) => Outcome::Configured,
            };
            self.finish(outcome);
        } else {
            self.transmit(idx + 1);
        }
    }

    /// The pad state in a complete poll reply.
    fn decode(&self) -> PadState {
        let rx = &self.txn.rx;
        let mode = mode_from_id_low(rx[1]);
        let sticks = if self.txn.len == 9 {
            AnalogSticks {
                right_x: rx[5],
                right_y: rx[6],
                left_x: rx[7],
                left_y: rx[8],
            }
        } else {
            AnalogSticks::CENTERED
        };
        PadState {
            buttons: decode_buttons(rx[3], rx[4]),
            mode,
            sticks,
            id_low: rx[1],
        }
    }

    fn fail(&mut self, why: Fault) {
        self.finish(Outcome::Failed(why));
    }

    /// End the transaction in flight and start the next port's.
    fn finish(&mut self, outcome: Outcome) {
        let i = self.end(outcome);
        self.start_port(i + 1);
    }

    /// End the transaction in flight, publish what it found, and go idle.
    /// Returns the port's index.
    fn end(&mut self, outcome: Outcome) -> usize {
        let i = index(self.txn.port);
        self.hw.cancel_deadline();
        self.hw.deselect();
        if matches!(outcome, Outcome::Failed(_)) {
            self.hw.reset_uart();
        }
        self.phase = Phase::Idle;
        self.record(i, outcome);
        if !matches!(outcome, Outcome::Configured) {
            self.published
                .publish(&[self.ports[0].reading, self.ports[1].reading]);
        }
        i
    }

    /// Give up on a round that no interrupt is advancing.
    fn abandon(&mut self) {
        self.end(Outcome::Failed(Fault::RxMissing));
    }

    /// Fold an outcome into the port's reading and its analog request.
    fn record(&mut self, i: usize, outcome: Outcome) {
        let ctl = &mut self.ports[i];
        let reading = &mut ctl.reading;
        let job = &mut ctl.job;
        match outcome {
            Outcome::Pad(pad) => {
                reading.pad = pad;
                reading.health = Health::Present;
                reading.updates = reading.updates.wrapping_add(1);
                if job.step == 4 {
                    if pad.mode == PadMode::Config && job.retries < EXIT_RETRIES {
                        // The exit did not take: send it again.
                        job.retries += 1;
                        job.step = 3;
                    } else {
                        job.outcome = Some(AnalogRequirement::from_mode(pad.mode));
                        job.step = 0;
                    }
                }
            }
            Outcome::Absent => {
                reading.pad = PadState::NONE;
                reading.health = Health::Absent;
                reading.updates = reading.updates.wrapping_add(1);
                if job.step != 0 {
                    job.outcome = Some(AnalogRequirement::Absent);
                    job.step = 0;
                }
            }
            Outcome::Configured => {
                if (1..=3).contains(&job.step) {
                    job.step += 1;
                }
            }
            Outcome::Failed(why) => {
                reading.health = Health::Faulted;
                reading.last_fault = Some(why);
                reading.faults = reading.faults.wrapping_add(1);
                if job.step != 0 {
                    job.faults += 1;
                    if job.faults >= ANALOG_FAULT_LIMIT {
                        job.outcome = Some(AnalogRequirement::Absent);
                        job.step = 0;
                    }
                }
            }
        }
    }

    // --------------------------------------------------------- foreground

    /// The latest reading of `port`, ahead of the next publication.
    pub fn reading(&self, port: Port) -> PortReading {
        self.ports[index(port)].reading
    }

    /// Put the pad on `port` in analog mode and lock it there, one command
    /// per VBlank, then check what it settled on. A request made while one is
    /// running changes nothing. [`analog_outcome`]
    /// (Self::analog_outcome) answers once that is done.
    pub fn request_analog(&mut self, port: Port) {
        let job = &mut self.ports[index(port)].job;
        if job.step == 0 {
            *job = AnalogJob {
                step: 1,
                ..AnalogJob::NONE
            };
        }
    }

    /// What the pad on `port` settled on after [`request_analog`]
    /// (Self::request_analog); `None` while the request is still running or
    /// none was made.
    pub fn analog_outcome(&self, port: Port) -> Option<AnalogRequirement> {
        self.ports[index(port)].job.outcome
    }

    /// Whether an analog request on `port` is still running.
    pub fn analog_pending(&self, port: Port) -> bool {
        self.ports[index(port)].job.step != 0
    }

    /// Take the serial port for a foreground driver, if no transaction is on
    /// the wire. While held, VBlanks poll nothing and the snapshot stands.
    pub fn try_lease(&mut self) -> bool {
        if self.owner == Owner::Engine && self.phase == Phase::Idle {
            self.owner = Owner::Lease;
            true
        } else {
            false
        }
    }

    /// Take the serial port now, abandoning a transaction in flight: for a
    /// caller that has waited as long as it can. The abandoned port reads
    /// [`Health::Faulted`] until its next transaction.
    pub fn force_lease(&mut self) {
        if self.phase != Phase::Idle {
            self.abandon();
        }
        self.owner = Owner::Lease;
    }

    /// Give the serial port back to the engine.
    pub fn release_lease(&mut self) {
        self.owner = Owner::Engine;
    }
}
