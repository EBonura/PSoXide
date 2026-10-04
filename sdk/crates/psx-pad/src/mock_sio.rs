//! Host-side stand-in for `psx_io`, compiled only under `cfg(test)`.
//!
//! The driver reaches SIO0 through `psx_io::{read_u8, read_u32, write_u8,
//! write_u16}`. Declared as `mod psx_io` at the crate root in test builds,
//! this module shadows the real crate and replaces the registers with a
//! deterministic controller model, so the real driver source runs unchanged
//! under `cargo test` and Miri.
//!
//! Time is abstract: every register access is one tick. The ACK and RX delays
//! below are not calibrated to any physical controller; they only put the
//! events the driver must wait for in a fixed order. The model holds one
//! controller per test thread.

extern crate std;

use core::cell::RefCell;
use psx_hw::sio::sio0::{BAUD, CTRL, DATA, MODE, STAT};
use std::vec::Vec;

/// A transport fault injected at one byte index of a packet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fault {
    None,
    /// STAT never reports TX ready at the byte.
    Tx,
    /// The byte's reply never arrives.
    Rx,
    /// The byte is never acknowledged.
    Ack,
    /// ACK stays asserted after the byte.
    AckHeld,
    /// ACK is already asserted when the port is selected.
    Acquire,
    /// The reply arrives long after the transaction is abandoned.
    LateRx,
}

#[derive(Debug)]
pub struct Model {
    // psx-numeric-allow-next-line: host-only controller model, compiled under cfg(test)
    pub now: u64,
    pub selected: bool,
    pub step: usize,
    pub final_step: usize,
    pub cmd: u8,
    pub last_rx: u8,
    // psx-numeric-allow-next-line: host-only controller model, compiled under cfg(test)
    pub ack_at: u64,
    // psx-numeric-allow-next-line: host-only controller model, compiled under cfg(test)
    pub ack_end: u64,
    // psx-numeric-allow-next-line: host-only controller model, compiled under cfg(test)
    pub pending: Vec<(u8, u64)>,
    /// ID the pad reports now: 0x41, 0x73, 0xF3, or 0xFF for an empty port.
    pub id: u8,
    /// Active-high buttons the pad reports.
    pub buttons: u16,
    /// Honors the DualShock configuration commands (0x43, 0x44).
    pub dualshock: bool,
    /// Analog requested by a 0x44 command, applied when config mode exits.
    pub analog_requested: bool,
    pub locked: bool,
    /// Exit-config commands to ignore, the SCPH-110 failure that parks the
    /// pad answering ID 0xF3.
    pub ignore_exits: u32,
    /// `(byte, delay)`: the ACK after packet byte `byte` arrives `delay`
    /// ticks late, and a host that clocks the next byte before that ACK
    /// ended gets `byte`'s reply again instead of a fresh one: the packet
    /// slips.
    // psx-numeric-allow-next-line: host-only controller model, compiled under cfg(test)
    pub slow_ack: Option<(usize, u64)>,
    // psx-numeric-allow-next-line: host-only controller model, compiled under cfg(test)
    pub rx_delay: u64,
    // psx-numeric-allow-next-line: host-only controller model, compiled under cfg(test)
    pub ack_delay: u64,
    // psx-numeric-allow-next-line: host-only controller model, compiled under cfg(test)
    pub ack_width: u64,
    pub fault: Fault,
    pub fault_byte: usize,
    /// Apply the fault to the first select only.
    pub fault_once: bool,
    /// Per-select `(id, fault, byte)` overrides, indexed by attempt.
    pub sequence: Vec<(u8, Fault, usize)>,
    // Observations.
    pub early: usize,
    pub sends: usize,
    pub attempts: usize,
    pub mid_ctrl: usize,
    pub resets: usize,
    pub tx_errors: usize,
}

impl Model {
    pub fn pending_replies(&self) -> usize {
        self.pending.len()
    }

    pub fn is_selected(&self) -> bool {
        self.selected
    }

    pub fn in_config(&self) -> bool {
        self.id == 0xF3
    }
}

impl Default for Model {
    fn default() -> Self {
        Self {
            now: 0,
            selected: false,
            step: 0,
            final_step: 8,
            cmd: 0,
            last_rx: 0xFF,
            ack_at: 0,
            ack_end: 0,
            pending: Vec::new(),
            id: 0x73,
            buttons: 0,
            dualshock: true,
            analog_requested: false,
            locked: false,
            ignore_exits: 0,
            slow_ack: None,
            rx_delay: 3,
            ack_delay: 12,
            ack_width: 5,
            fault: Fault::None,
            fault_byte: 3,
            fault_once: false,
            sequence: Vec::new(),
            early: 0,
            sends: 0,
            attempts: 0,
            mid_ctrl: 0,
            resets: 0,
            tx_errors: 0,
        }
    }
}

std::thread_local! {
    static MODEL: RefCell<Model> = RefCell::new(Model::default());
}

/// Run `f` on this thread's controller model.
pub fn with<R>(f: impl FnOnce(&mut Model) -> R) -> R {
    MODEL.with(|m| f(&mut m.borrow_mut()))
}

/// Replace this thread's controller model.
pub fn reset(model: Model) {
    with(|m| *m = model);
}

fn faulted(m: &Model, kind: Fault, index: usize) -> bool {
    m.fault == kind && m.fault_byte == index && (!m.fault_once || m.attempts == 1)
}

/// 16-bit register write. MODE and BAUD are accepted and ignored; on CTRL,
/// DTR selects the pad and RESET cancels a late reply.
pub unsafe fn write_u16(addr: u32, value: u16) {
    if addr != CTRL {
        assert!(addr == MODE || addr == BAUD, "unmodelled 16-bit write");
        return;
    }
    with(|m| {
        m.now += 1;
        let selected = value & psx_hw::sio::sio0::ctrl::DTR != 0;
        if value & psx_hw::sio::sio0::ctrl::RESET != 0 {
            // The UART keeps a late reply across deselection; only RESET
            // cancels it.
            assert!(!selected);
            m.pending.clear();
            m.resets += 1;
        }
        if selected && m.selected {
            m.mid_ctrl += 1;
        }
        if selected && !m.selected {
            select(m);
        }
        if !selected {
            m.ack_at = 0;
            m.ack_end = 0;
        }
        m.selected = selected;
    });
}

fn select(m: &mut Model) {
    m.attempts += 1;
    m.step = 0;
    m.ack_at = 0;
    m.ack_end = 0;
    if let Some(&(id, kind, byte)) = m.sequence.get(m.attempts - 1) {
        m.id = id;
        m.fault = kind;
        m.fault_byte = byte;
    }
    if faulted(m, Fault::Acquire, 0) {
        m.ack_at = m.now;
        m.ack_end = m.now + 100_000;
    }
}

/// STAT read: TX ready, RX not empty, live ACK level.
pub unsafe fn read_u32(addr: u32) -> u32 {
    assert_eq!(addr, STAT, "only STAT is modelled for 32-bit reads");
    with(|m| {
        m.now += 1;
        let mut stat = if faulted(m, Fault::Tx, m.step) { 0 } else { 1 };
        if m.pending.iter().any(|&(_, ready)| m.now >= ready) {
            stat |= 2;
        }
        if m.ack_at != 0 && m.now >= m.ack_at && m.now < m.ack_end {
            stat |= 1 << 7;
        }
        stat
    })
}

/// DATA read: pops the oldest reply that has arrived.
pub unsafe fn read_u8(addr: u32) -> u8 {
    assert_eq!(addr, DATA, "only DATA is modelled for 8-bit reads");
    with(|m| {
        m.now += 1;
        let now = m.now;
        let (i, _) = m
            .pending
            .iter()
            .enumerate()
            .filter(|(_, (_, ready))| now >= *ready)
            .min_by_key(|(_, (_, ready))| *ready)
            .expect("read of an empty RX FIFO");
        m.pending.remove(i).0
    })
}

/// DATA write: clocks one byte to the controller and queues its reply.
pub unsafe fn write_u8(addr: u32, value: u8) {
    assert_eq!(addr, DATA, "only DATA is modelled for 8-bit writes");
    with(|m| {
        m.now += 1;
        assert!(m.selected, "DATA sent while deselected");
        let index = m.step;
        let early = index != 0 && m.now < m.ack_end;
        if early {
            m.early += 1;
        }
        if index == 1 {
            m.cmd = value;
            m.final_step = if m.id == 0x41 { 4 } else { 8 };
        }
        let expected = match index {
            0 => Some(0x01),
            1 => None,
            _ if m.cmd == 0x42 => Some(0x00),
            _ => None,
        };
        if expected.is_some_and(|e| e != value) {
            m.tx_errors += 1;
        }
        let fresh = reply(m, index);
        apply_command(m, index, value);
        let slipped = early && m.slow_ack.is_some_and(|(byte, _)| byte + 1 == index);
        let rx = if slipped { m.last_rx } else { fresh };
        m.last_rx = rx;
        if !faulted(m, Fault::Rx, index) {
            let late = faulted(m, Fault::LateRx, index);
            let ready = m.now + if late { 33_820 } else { m.rx_delay };
            m.pending.push((rx, ready));
        }
        let acked = m.id != 0xFF && index < m.final_step && !faulted(m, Fault::Ack, index);
        (m.ack_at, m.ack_end) = if acked {
            {
                let delay = match m.slow_ack {
                    Some((byte, slow)) if byte == index => slow,
                    _ => m.ack_delay,
                };
                (m.now + delay, m.now + delay + m.ack_width)
            }
        } else {
            (0, 0)
        };
        if faulted(m, Fault::AckHeld, index) && index < m.final_step {
            m.ack_end = m.now + 100_000;
        }
        m.step += 1;
        m.sends += 1;
    });
}

/// The reply byte for packet byte `index`.
fn reply(m: &Model, index: usize) -> u8 {
    match index {
        _ if m.id == 0xFF => 0xFF,
        0 => 0xFF,
        1 => m.id,
        2 => 0x5A,
        3 if m.cmd == 0x42 => !(m.buttons as u8),
        4 if m.cmd == 0x42 => !((m.buttons >> 8) as u8),
        _ if m.cmd == 0x42 => 0x80,
        _ => 0x00,
    }
}

/// A DualShock's response to the configuration commands the driver sends.
fn apply_command(m: &mut Model, index: usize, value: u8) {
    if !m.dualshock || m.id == 0xFF {
        return;
    }
    match (m.cmd, index) {
        (0x43, 3) if value == 1 => m.id = 0xF3,
        (0x43, 3) if value == 0 && m.id == 0xF3 => {
            if m.ignore_exits > 0 {
                m.ignore_exits -= 1;
            } else {
                m.id = if m.analog_requested { 0x73 } else { 0x41 };
            }
        }
        (0x44, 3) => m.analog_requested = value == 1,
        (0x44, 4) => m.locked = value == 3,
        _ => {}
    }
}

/// The modelled registers as the port the driver borrows.
pub struct MockBus;

impl psx_io::controller_port::Transport for MockBus {
    fn status(&mut self) -> u32 {
        // SAFETY: the mock reads its own model; no hardware is involved.
        unsafe { read_u32(STAT) }
    }
    fn receive(&mut self) -> u8 {
        // SAFETY: as above.
        unsafe { read_u8(DATA) }
    }
    fn transmit(&mut self, byte: u8) {
        // SAFETY: as above.
        unsafe { write_u8(DATA, byte) }
    }
    fn set_mode(&mut self, value: u16) {
        // SAFETY: as above.
        unsafe { write_u16(MODE, value) }
    }
    fn set_baud(&mut self, value: u16) {
        // SAFETY: as above.
        unsafe { write_u16(BAUD, value) }
    }
    fn set_control(&mut self, value: u16) {
        // SAFETY: as above.
        unsafe { write_u16(CTRL, value) }
    }
}
