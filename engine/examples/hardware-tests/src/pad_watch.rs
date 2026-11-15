// SPDX-License-Identifier: GPL-2.0-or-later
//! The watching transport under `psx_pad::Controllers`, shared by the survey and the menu.
//!
//! It is a test instrument, not part of the SDK path: every call goes straight to the real
//! `ControllerPort`, so the bytes on the wire are the ones the SDK sends. What it adds is a
//! record of them. It reads the system clock only when `STAT` shows an `/ACK` or its release
//! (never inside the SDK's wait loops), and once per transaction a root counter 1 read to time
//! how long `/CS` was held low before the first byte.
//!
//! Two switches change what the HOST does, nothing the pad is sent: `release_between` makes
//! `Transport::open` leave `/CS` high, so the next poll makes a fresh select (the menu lab's
//! variant 2).

use crate::pad_record::{self as rec, Counters, PollNote};
use psx_hw::sio::sio0;
use psx_io::controller_port::{Port, Transport};
use psx_io::periph::ControllerPort;
use psx_io::timers::{self, Timer};
use psx_pad::PadMode;

/// Cycles after the deselect the held-select phase opens the next select: tuned in the emulator
/// so the `/CS`-high time is the 562 the v2.22 menu measured through this transport.
const HOLD_WAIT: u16 = 431;

/// Reply bytes a pad of this ID sends in all, address byte included: three, and two for every
/// halfword the low nibble of the ID counts.
pub(crate) fn reply_len(id: u8) -> usize {
    3 + 2 * usize::from(id & 0x0F).max(1)
}

pub(crate) fn mode_code(mode: PadMode) -> u8 {
    match mode {
        PadMode::Disconnected => 0,
        PadMode::Digital => 1,
        PadMode::Analog => 2,
        PadMode::Negcon => 3,
        PadMode::Config => 4,
        PadMode::Unknown => 5,
    }
}

pub(crate) fn mode_name(mode: u8) -> &'static str {
    match mode {
        0 => "NONE",
        1 => "DIGITAL",
        2 => "ANALOG",
        3 => "NEGCON",
        4 => "CONFIG",
        _ => "UNKNOWN",
    }
}

/// How one poll transaction ended: class (see `rec::NOTE_CLASS_NAMES`) and the byte it died at.
pub(crate) fn classify(sent: u8, n: u8, rx: &[u8; 9], acked: u8, ok: bool) -> (u8, u8) {
    if sent == 0 {
        (4, 0xFF)
    } else if sent == 1 {
        (5, 0)
    } else if n >= 2 && rx[1] == 0xFF {
        (3, 1)
    } else if n >= 3 && rx[2] != 0x5A {
        (2, 2)
    } else if !ok || usize::from(n) < reply_len(rx[1]).min(9) {
        (1, acked)
    } else {
        (0, 0xFF)
    }
}

/// What the watching transport kept of the poll call in progress.
#[derive(Clone, Copy)]
struct Call {
    tries: u8,
    first_class: u8,
    first_id: u8,
    first_died: u8,
    first_fresh: bool,
    class: u8,
    id: u8,
    n: u8,
    acked: u8,
    died: u8,
    rx: [u8; 9],
    selects: u8,
    cfg: u8,
    exits: u8,
    hold: u16,
}

impl Call {
    const NEW: Call = Call {
        tries: 0,
        first_class: 4,
        first_id: 0xFF,
        first_died: 0xFF,
        first_fresh: false,
        class: 4,
        id: 0xFF,
        n: 0,
        acked: 0,
        died: 0xFF,
        rx: [0; 9],
        selects: 0,
        cfg: 0,
        exits: 0,
        hold: 0,
    };
}

/// The real port under `Controllers`, watching it.
pub(crate) struct Watch {
    port: ControllerPort,
    position: usize,
    command: u8,
    data0: u8,
    rx: [u8; 9],
    n: u8,
    rise: [u16; 9],
    width: [u8; 9],
    up_at: u16,
    wrote_at: u16,
    clear_pending: bool,
    irq_after_clear: bool,
    /// Selects the SDK made since the last transaction ended.
    txn_selects: u8,
    open_t1: u16,
    open_valid: bool,
    txn_hold: u16,
    exit_pending: bool,
    call: Call,
    /// Latest poll transaction (command 0x42), as the survey reads it.
    pub snap: [u8; 9],
    pub snap_n: u8,
    pub snap_ok: bool,
    pub snap_wire: rec::Wire,
    pub snap_irq: bool,
    /// Poll transactions (command 0x42) made.
    pub polls: u32,
    /// Other transactions that sent at least one byte.
    pub configs: u32,
    /// Counts since this watch was made.
    pub counters: Counters,
    /// Host-side switch: `/CS` is released between polls.
    pub release_between: bool,
    /// A select left open after the last poll (see `hold_polls`), taken by the next poll.
    held: Option<u16>,
    /// Leave the next poll's select open after a successful poll (the A/B phase that watches the
    /// held select; the SDK no longer does it): done in `finish`, `HOLD_WAIT` cycles after the deselect.
    pub(crate) hold_polls: bool,
    socket: Port,
}

impl Watch {
    pub(crate) fn new() -> Self {
        Self {
            // SAFETY: a token is a logic guard; nothing else drives SIO0 while the caller runs.
            port: unsafe { ControllerPort::steal() },
            position: 0,
            command: 0,
            data0: 0,
            rx: [0; 9],
            n: 0,
            rise: [0xFFFF; 9],
            width: [0; 9],
            up_at: 0,
            wrote_at: 0,
            clear_pending: false,
            irq_after_clear: false,
            txn_selects: 0,
            open_t1: 0,
            open_valid: false,
            txn_hold: 0,
            exit_pending: false,
            call: Call::NEW,
            snap: [0; 9],
            snap_n: 0,
            snap_ok: false,
            snap_wire: rec::Wire::NONE,
            snap_irq: false,
            polls: 0,
            configs: 0,
            counters: Counters {
                ok: 0,
                bad: 0,
                nak: 0,
                retry: 0,
                handshakes: 0,
                cfg: 0,
                exits: 0,
                exits_f3: 0,
            },
            release_between: false,
            held: None,
            hold_polls: false,
            socket: Port::One,
        }
    }

    /// Index of the byte last written.
    fn current(&self) -> Option<usize> {
        self.position.checked_sub(1).filter(|&i| i < 9)
    }

    /// Forget the poll call in progress; call before `Controllers::poll`.
    pub(crate) fn begin_call(&mut self) {
        self.call = Call::NEW;
    }

    /// The poll call that just ended, with the fields the transport knows; the caller fills in
    /// what the SDK reported and what its fold made of it. Counts the call in the totals.
    pub(crate) fn end_call(&mut self, lab: u8) -> (PollNote, u16) {
        let c = self.call;
        if c.tries > 1 {
            self.counters.retry += u32::from(c.tries - 1);
        }
        if c.tries != 0 {
            if c.class == 0 {
                self.counters.ok += 1;
            } else {
                self.counters.bad += 1;
            }
        }
        let note = PollNote {
            lab,
            first_class: c.first_class,
            first_id: c.first_id,
            first_died: c.first_died,
            class: c.class,
            id: c.id,
            n: c.n,
            acked: c.acked,
            died: c.died,
            tries: c.tries,
            fresh: u8::from(c.first_fresh) | (c.selects << 1),
            cfg: c.cfg,
            exits: c.exits,
            rx: c.rx,
            ..PollNote::default()
        };
        (note, c.hold)
    }
}

impl Transport for Watch {
    fn status(&mut self) -> u32 {
        let status = self.port.status();
        if self.clear_pending {
            self.clear_pending = false;
            if status & sio0::stat::IRQ != 0 {
                self.irq_after_clear = true;
            }
        }
        if let Some(i) = self.current() {
            if status & (sio0::stat::DSR_LEVEL | sio0::stat::IRQ) != 0 && self.rise[i] == 0xFFFF {
                let now = self.port.clock();
                self.rise[i] = now.wrapping_sub(self.wrote_at);
                if status & sio0::stat::DSR_LEVEL != 0 {
                    self.up_at = now;
                    self.width[i] = 0;
                } else {
                    self.width[i] = 0xFF;
                }
            } else if status & sio0::stat::DSR_LEVEL == 0
                && self.rise[i] != 0xFFFF
                && self.width[i] == 0
            {
                self.width[i] = self.port.clock().wrapping_sub(self.up_at).clamp(1, 254) as u8;
            }
        }
        status
    }
    fn receive(&mut self) -> u8 {
        let byte = self.port.receive();
        if let Some(at) = self.position.checked_sub(1) {
            if at < 9 {
                self.rx[at] = byte;
                self.n = (at + 1) as u8;
            }
        }
        byte
    }
    fn transmit(&mut self, byte: u8) {
        if self.position == 0 {
            self.txn_hold = if self.txn_selects == 0 && self.open_valid {
                timers::counter(Timer::Timer1).wrapping_sub(self.open_t1)
            } else {
                0
            };
            self.open_valid = false;
        }
        if self.position == 1 {
            self.command = byte;
        }
        if self.position == 3 {
            self.data0 = byte;
        }
        self.position += 1;
        self.wrote_at = self.port.clock();
        self.port.transmit(byte);
    }
    fn set_mode(&mut self, value: u16) {
        self.port.set_mode(value);
    }
    fn set_baud(&mut self, value: u16) {
        self.port.set_baud(value);
    }
    fn set_control(&mut self, value: u16) {
        if value & sio0::ctrl::ACK != 0 {
            self.clear_pending = true;
        }
        self.port.set_control(value);
    }
    fn control(&mut self) -> u16 {
        self.port.control()
    }
    fn clock(&mut self) -> u16 {
        self.port.clock()
    }
    fn select_with(&mut self, port: Port, arm_ack_irq: bool) {
        self.txn_selects = self.txn_selects.saturating_add(1);
        self.set_mode(sio0::MODE_8N1);
        self.set_baud(sio0::BAUD_250KHZ);
        self.set_control(sio0::ctrl::ACK);
        self.set_control(sio0::selected_ctrl(port.is_two(), arm_ack_irq));
    }
    fn open(&mut self, port: Port) -> u16 {
        self.open_t1 = timers::counter(Timer::Timer1);
        self.open_valid = true;
        if !self.release_between {
            self.port.select(port);
        }
        self.port.clock()
    }
    fn begin_pad(&mut self, port: Port, _opened: Option<u16>) -> bool {
        self.socket = port;
        match self.held.take() {
            Some(at) if self.control() == sio0::selected_ctrl(port.is_two(), false) => {
                while self.clock().wrapping_sub(at) < psx_io::controller_port::PAD_SETUP_CYCLES {}
            }
            _ => {
                self.select(port);
                let at = self.clock();
                while self.clock().wrapping_sub(at) < psx_io::controller_port::PAD_SETUP_CYCLES {}
            }
        }
        self.drain_receive();
        self.wait_status_clear(sio0::stat::DSR_LEVEL, psx_io::controller_port::Timing::PAD.ack_spins)
    }
    fn finish(&mut self, succeeded: bool) {
        let was_poll = self.command == 0x42;
        let sent = self.position as u8;
        let acked = self.rise.iter().filter(|&&r| r != 0xFFFF).count() as u8;
        let poll_like = self.command == 0x42 || (self.command == 0 && self.position == 1);
        if self.command == 0x42 {
            self.snap = self.rx;
            self.snap_n = self.n;
            self.snap_ok = succeeded;
            self.snap_wire = rec::Wire { rise: self.rise, width: self.width };
            self.snap_irq = self.irq_after_clear;
            self.polls += 1;
        } else if self.position > 0 {
            self.configs += 1;
        }
        if poll_like {
            let (class, died) = classify(sent, self.n, &self.rx, acked, succeeded);
            let c = &mut self.call;
            if c.tries == 0 {
                c.first_class = class;
                c.first_id = if self.n >= 2 { self.rx[1] } else { 0xFF };
                c.first_died = died;
                c.first_fresh = self.txn_selects != 0;
                c.hold = self.txn_hold;
            }
            c.tries = c.tries.saturating_add(1);
            c.selects = c.selects.saturating_add(self.txn_selects);
            c.class = class;
            c.id = if self.n >= 2 { self.rx[1] } else { 0xFF };
            c.n = self.n;
            c.acked = acked;
            c.died = died;
            c.rx = self.rx;
            if class == 5 {
                self.counters.nak += 1;
            }
            if self.exit_pending && self.n >= 2 {
                self.exit_pending = false;
                if self.rx[1] == 0xF3 {
                    self.counters.exits_f3 += 1;
                }
            }
        } else if self.position > 1 {
            self.counters.cfg += 1;
            self.call.cfg = self.call.cfg.saturating_add(1);
            if self.command == 0x43 {
                if self.data0 == 1 {
                    self.counters.handshakes += 1;
                } else {
                    self.counters.exits += 1;
                    self.call.exits = self.call.exits.saturating_add(1);
                    self.exit_pending = true;
                }
            }
        }
        self.position = 0;
        self.command = 0;
        self.data0 = 0;
        self.n = 0;
        self.rx = [0; 9];
        self.rise = [0xFFFF; 9];
        self.width = [0; 9];
        self.irq_after_clear = false;
        self.txn_selects = 0;
        self.port.finish(succeeded);
        if self.hold_polls && succeeded && was_poll {
            let at = self.port.clock();
            while self.port.clock().wrapping_sub(at) < HOLD_WAIT {}
            self.held = Some(self.open(self.socket));
        }
    }
    fn mask_interrupts(&mut self) -> bool {
        self.port.mask_interrupts()
    }
    fn restore_interrupts(&mut self, were_enabled: bool) {
        self.port.restore_interrupts(were_enabled);
    }
}
