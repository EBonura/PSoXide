//! The engine against a controller model with real timing.
//!
//! `Sim` is a serial port, a deadline timer and up to two devices, on a clock
//! counted in CPU cycles. Nothing is abstract here: a byte shifts for 1,088
//! cycles, a pad pulses `/ACK` some cycles after it, and an interrupt reaches
//! the engine after a fixed entry cost. The driver loop delivers events in
//! time order, so what the engine does between events is what a console would
//! see between interrupts, and the cycles it spends are counted.

extern crate std;

use crate::engine::{BytePacing, Config, Engine, Fault, Health, Hw, Owner, Published, Snapshot};
use crate::{button, AnalogRequirement, PadMode, Port};
use psx_hw::sio::sio0::stat;
use std::boxed::Box;
use std::vec::Vec;

/// CPU cycles in one 60 Hz frame.
const FRAME: u32 = 564_480;
/// CPU cycles a byte takes to shift, at the 250 kHz controller clock.
const BYTE: u32 = 1_088;
/// CPU cycles to enter and leave an interrupt handler.
const ISR_ENTRY: u32 = 120;
/// CPU cycles per register access.
const ACCESS: u32 = 7;

/// One controller.
#[derive(Clone, Debug)]
struct Pad {
    id: u8,
    buttons: u16,
    sticks: [u8; 4],
    /// Honors the DualShock configuration commands.
    dualshock: bool,
    analog_requested: bool,
    /// Exit-config commands to ignore: the SCPH-110 failure that parks the pad
    /// answering ID 0xF3.
    ignore_exits: u32,
    /// Cycles from the end of a byte to the start of its `/ACK`.
    ack_delay: u32,
    /// Cycles `/ACK` stays asserted.
    ack_width: u32,
    /// A host that clocks a byte before this device acknowledged the last one
    /// gets that reply again: the packet slips.
    slips_if_early: bool,
    /// Exchange index that is never acknowledged.
    drop_ack_at: Option<u8>,
    /// Reply to the ID's second byte.
    magic: u8,
    /// The motors are mapped to the poll bytes (command 0x4D in config mode).
    mapped: bool,
    /// What the motors are doing: small on, large level.
    motors: (bool, u8),
    /// `(ID answered, byte 3, byte 4)` of every 0x42 poll.
    motor_tx: Vec<(u8, u8, u8)>,
    /// Command byte of every packet.
    cmds: Vec<u8>,
    pending_small: u8,
    map_small: bool,
    // Per-select state.
    step: u8,
    cmd: u8,
    last_reply: u8,
    ack_start: u32,
}

impl Pad {
    fn dualshock_analog() -> Pad {
        Pad {
            id: 0x73,
            buttons: 0,
            sticks: [0x80, 0x80, 0x80, 0x80],
            dualshock: true,
            analog_requested: true,
            ignore_exits: 0,
            ack_delay: 300,
            ack_width: 100,
            slips_if_early: false,
            drop_ack_at: None,
            magic: 0x5A,
            mapped: false,
            motors: (false, 0),
            motor_tx: Vec::new(),
            cmds: Vec::new(),
            pending_small: 0,
            map_small: false,
            step: 0,
            cmd: 0,
            last_reply: 0xFF,
            ack_start: 0,
        }
    }

    fn digital() -> Pad {
        Pad {
            id: 0x41,
            dualshock: false,
            analog_requested: false,
            ..Pad::dualshock_analog()
        }
    }

    /// Exchanges in the packet for the ID it is answering with now.
    fn final_step(&self) -> u8 {
        if self.id == 0x41 {
            4
        } else {
            8
        }
    }

    fn reply(&self, idx: u8) -> u8 {
        match idx {
            0 => 0xFF,
            1 => self.id,
            2 => self.magic,
            3 if self.cmd == 0x42 => !(self.buttons as u8),
            4 if self.cmd == 0x42 => !((self.buttons >> 8) as u8),
            5..=8 if self.cmd == 0x42 => self.sticks[idx as usize - 5],
            _ => 0x00,
        }
    }

    fn apply(&mut self, idx: u8, value: u8) {
        if idx == 1 {
            self.cmds.push(value);
        }
        match (self.cmd, idx) {
            (0x42, 3) => self.pending_small = value,
            (0x42, 4) => {
                self.motor_tx.push((self.id, self.pending_small, value));
                if self.mapped && self.id == 0x73 {
                    self.motors = (self.pending_small & 1 != 0, value);
                }
            }
            _ => {}
        }
        if !self.dualshock {
            return;
        }
        match (self.cmd, idx) {
            (0x43, 3) if value == 1 => self.id = 0xF3,
            (0x43, 3) if value == 0 && self.id == 0xF3 => {
                if self.ignore_exits > 0 {
                    self.ignore_exits -= 1;
                } else {
                    self.id = if self.analog_requested { 0x73 } else { 0x41 };
                }
            }
            (0x44, 3) => self.analog_requested = value == 1,
            // The mapping: small motor on the first motor byte, large on the
            // second. Only configuration mode takes it.
            (0x4D, 3) if self.id == 0xF3 => self.map_small = value == 0x00,
            (0x4D, 4) if self.id == 0xF3 => self.mapped = self.map_small && value == 0x01,
            _ => {}
        }
    }
}

/// The serial port, timer and devices.
struct Sim {
    now: u32,
    selected: Option<(Port, bool)>,
    pads: [Option<Pad>; 2],
    /// Replies in flight or waiting: `(byte, ready at)`.
    rx: Vec<(u8, u32)>,
    /// `/ACK` asserted from..until.
    dsr: Option<(u32, u32)>,
    latch: bool,
    irq7: bool,
    deadline: Option<u32>,
    /// CPU cycles the engine has used in handlers.
    busy: u32,
    // Observations.
    accesses: u32,
    latch_clears: u32,
    resets: u32,
    selects: Vec<(Port, bool)>,
    sent: Vec<(Port, u8)>,
    /// Interrupts the model withholds from the engine (a lost interrupt).
    mute: bool,
}

impl Sim {
    fn new() -> Sim {
        Sim {
            now: 0,
            selected: None,
            pads: [None, None],
            rx: Vec::new(),
            dsr: None,
            latch: false,
            irq7: false,
            deadline: None,
            busy: 0,
            accesses: 0,
            latch_clears: 0,
            resets: 0,
            selects: Vec::new(),
            sent: Vec::new(),
            mute: false,
        }
    }

    fn touch(&mut self) {
        self.accesses += 1;
        self.now += ACCESS;
    }

    fn dsr_level(&self) -> bool {
        self.dsr
            .is_some_and(|(from, until)| self.now >= from && self.now < until)
    }

    /// The next time something happens on its own.
    fn next_event(&self) -> Option<u32> {
        let mut next: Option<u32> = None;
        let mut consider = |t: u32| next = Some(next.map_or(t, |n| n.min(t)));
        if let Some((from, until)) = self.dsr {
            if self.now < from {
                consider(from);
            } else if self.now < until {
                consider(until);
            }
        }
        if let Some(d) = self.deadline.filter(|_| !self.mute) {
            consider(d);
        }
        next
    }

    /// Advance to `t`, raising IRQ7 at the start of an `/ACK` pulse.
    fn advance_to(&mut self, t: u32) {
        if let Some((from, _)) = self.dsr {
            if self.now < from && t >= from {
                let armed = self.selected.is_some_and(|(_, irq)| irq);
                if armed && !self.latch {
                    self.latch = true;
                    self.irq7 = true;
                }
            }
        }
        self.now = self.now.max(t);
    }
}

impl Hw for Sim {
    fn status(&mut self) -> u32 {
        self.touch();
        let mut s = stat::TX_READY;
        if self.rx.iter().any(|&(_, ready)| self.now >= ready) {
            s |= stat::RX_NOT_EMPTY;
        }
        if self.dsr_level() {
            s |= stat::DSR_LEVEL;
        }
        if self.latch {
            s |= stat::IRQ;
        }
        s
    }

    fn select(&mut self, port: Port, ack_irq: bool) {
        self.touch();
        self.latch = false;
        self.irq7 = false;
        self.selected = Some((port, ack_irq));
        self.selects.push((port, ack_irq));
        if let Some(pad) = self.pads[port as usize].as_mut() {
            pad.step = 0;
        }
    }

    fn deselect(&mut self) {
        self.touch();
        self.selected = None;
        self.dsr = None;
    }

    fn reset_uart(&mut self) {
        self.touch();
        self.rx.clear();
        self.resets += 1;
    }

    fn drain_receive(&mut self) {
        self.touch();
        let now = self.now;
        self.rx.retain(|&(_, ready)| now < ready);
    }

    fn transmit(&mut self, byte: u8) {
        self.touch();
        let (port, _) = self.selected.expect("a byte sent with nothing selected");
        self.sent.push((port, byte));
        let end = self.now + BYTE;
        let now = self.now;
        let Some(pad) = self.pads[port as usize].as_mut() else {
            self.rx.push((0xFF, end));
            self.dsr = None;
            return;
        };
        let idx = pad.step;
        let early = idx != 0 && pad.slips_if_early && now < pad.ack_start;
        if idx == 1 {
            pad.cmd = byte;
        }
        let fresh = pad.reply(idx);
        pad.apply(idx, byte);
        let reply = if early { pad.last_reply } else { fresh };
        pad.last_reply = reply;
        self.rx.push((reply, end));
        let acked = idx < pad.final_step() && pad.drop_ack_at != Some(idx);
        if acked {
            let from = end + pad.ack_delay;
            pad.ack_start = from;
            self.dsr = Some((from, from + pad.ack_width));
        } else {
            self.dsr = None;
        }
        pad.step += 1;
    }

    fn receive(&mut self) -> u8 {
        self.touch();
        let now = self.now;
        let at = self
            .rx
            .iter()
            .position(|&(_, ready)| now >= ready)
            .expect("read of an empty receive FIFO");
        self.rx.remove(at).0
    }

    fn wait_ack_release(&mut self) -> bool {
        // Bounded: 256 status reads, about 7 cycles each.
        for _ in 0..256 {
            if !self.dsr_level() {
                return true;
            }
            self.touch();
        }
        !self.dsr_level()
    }

    fn clear_ack_latch(&mut self, _port: Option<Port>, _ack_irq: bool) {
        self.touch();
        self.latch = false;
        self.latch_clears += 1;
    }

    fn arm_deadline(&mut self, cycles: u32) {
        self.touch();
        self.deadline = Some(self.now + cycles);
    }

    fn cancel_deadline(&mut self) {
        self.touch();
        self.deadline = None;
    }

    fn deadline_expired(&mut self) -> bool {
        self.touch();
        self.deadline.is_some_and(|d| self.now >= d)
    }
}

/// An engine on the model, with the clock and the driver loop.
struct Rig {
    engine: Engine<'static, Sim>,
    published: &'static Published,
}

impl Rig {
    fn new(config: Config) -> Rig {
        let published: &'static Published = Box::leak(Box::new(Published::new()));
        Rig {
            engine: Engine::new(Sim::new(), published, config),
            published,
        }
    }

    fn sim(&mut self) -> &mut Sim {
        self.engine.hw_mut()
    }

    fn plug(&mut self, port: Port, pad: Pad) {
        self.sim().pads[port as usize] = Some(pad);
    }

    fn unplug(&mut self, port: Port) {
        self.sim().pads[port as usize] = None;
    }

    fn pad(&mut self, port: Port) -> &mut Pad {
        self.sim().pads[port as usize]
            .as_mut()
            .expect("a pad is plugged in")
    }

    /// Deliver interrupts and let time pass until `until`.
    fn run_until(&mut self, until: u32) {
        loop {
            let (irq7, deadline_due) = {
                let sim = self.sim();
                (
                    sim.irq7 && !sim.mute,
                    !sim.mute && sim.deadline.is_some_and(|d| sim.now >= d),
                )
            };
            if irq7 || deadline_due {
                self.deliver(irq7);
                continue;
            }
            let next = self.sim().next_event();
            match next {
                Some(t) if t < until => {
                    self.sim().advance_to(t);
                }
                _ => {
                    self.sim().advance_to(until);
                    return;
                }
            }
        }
    }

    /// One interrupt: entry cost, then the engine's handlers.
    fn deliver(&mut self, irq7: bool) {
        let start_accesses = self.sim().accesses;
        self.sim().now += ISR_ENTRY / 2;
        if irq7 {
            self.sim().irq7 = false;
            self.engine.on_ack();
        }
        self.engine.on_deadline();
        let sim = self.sim();
        sim.now += ISR_ENTRY / 2;
        sim.busy += ISR_ENTRY + (sim.accesses - start_accesses) * ACCESS;
        // The model deadline is one-shot.
        if sim.deadline.is_some_and(|d| sim.now >= d) {
            sim.deadline = None;
        }
    }

    /// One frame: the VBlank, then everything it sets off.
    fn frame(&mut self) {
        let start = self.sim().now;
        let busy = self.sim().busy;
        self.sim().now += ISR_ENTRY / 2;
        self.engine.on_vblank();
        let sim = self.sim();
        sim.now += ISR_ENTRY / 2;
        sim.busy += ISR_ENTRY + ACCESS * 4;
        let _ = busy;
        self.run_until(start + FRAME);
    }

    fn frames(&mut self, n: u32) {
        for _ in 0..n {
            self.frame();
        }
    }

    fn snap(&self) -> Snapshot {
        self.published.read()
    }
}

fn analog_with(buttons: u16) -> Pad {
    Pad {
        buttons,
        ..Pad::dualshock_analog()
    }
}

// ---------------------------------------------------------------- frames

#[test]
fn digital_analog_and_config_frames_decode_the_same_buttons() {
    let held = button::CROSS | button::START | button::LEFT;
    for (id, exchanges) in [(0x41u8, 5usize), (0x73, 9), (0xF3, 9)] {
        let mut rig = Rig::new(Config::DEFAULT);
        rig.plug(
            Port::One,
            Pad {
                id,
                buttons: held,
                sticks: [0x10, 0x20, 0x30, 0x40],
                ..Pad::dualshock_analog()
            },
        );
        rig.frame();
        let snap = rig.snap();
        let one = snap.port(Port::One);
        assert_eq!(one.health, Health::Present, "id {id:02x}");
        assert_eq!(one.pad.buttons.bits(), held, "id {id:02x}");
        assert_eq!(one.pad.id_low, id);
        if id == 0x41 {
            assert_eq!(one.pad.mode, PadMode::Digital);
        } else {
            assert_eq!(one.pad.sticks.right_x, 0x10);
            assert_eq!(one.pad.sticks.left_y, 0x40);
        }
        let on_one = rig
            .sim()
            .sent
            .iter()
            .filter(|(p, _)| *p == Port::One)
            .count();
        assert_eq!(on_one, exchanges, "id {id:02x}");
        assert_eq!(snap.port(Port::Two).health, Health::Absent);
    }
}

#[test]
fn the_packet_carries_the_poll_command_and_fills() {
    let mut rig = Rig::new(Config::PORT1_ONLY);
    rig.plug(Port::One, Pad::dualshock_analog());
    rig.frame();
    let sent: Vec<u8> = rig.sim().sent.iter().map(|&(_, b)| b).collect();
    assert_eq!(sent, [0x01, 0x42, 0, 0, 0, 0, 0, 0, 0]);
}

#[test]
fn an_empty_socket_costs_a_handful_of_cycles_not_a_frame() {
    // The synchronous driver spends about 119,000 cycles a frame finding an
    // empty port 2 (four attempts of a 1,024-read setup and a 2,048-read
    // `/ACK` wait). The engine's whole cost for two empty sockets is a few
    // interrupts.
    let mut rig = Rig::new(Config::DEFAULT);
    rig.frames(60);
    let snap = rig.snap();
    assert_eq!(snap.port(Port::One).health, Health::Absent);
    assert_eq!(snap.port(Port::Two).health, Health::Absent);
    assert_eq!(snap.port(Port::One).pad.mode, PadMode::Disconnected);
    assert_eq!(snap.seq, 120, "one publication per port per frame");
    let per_frame = rig.sim().busy / 60;
    assert!(per_frame < 1_200, "{per_frame} cycles a frame");
    // Each empty socket is one byte on the wire.
    assert_eq!(rig.sim().sent.len(), 120);
}

#[test]
fn a_connected_pad_and_an_empty_socket_cost_the_connected_pad_only() {
    let mut rig = Rig::new(Config::DEFAULT);
    rig.plug(Port::One, analog_with(button::CIRCLE));
    rig.frames(60);
    let per_frame = rig.sim().busy / 60;
    // Nine bytes of interrupts, each a few register accesses: well under
    // the 17,000 cycles the synchronous poll of the same pad spends spinning.
    assert!(per_frame < 4_500, "{per_frame} cycles a frame");
    let snap = rig.snap();
    assert_eq!(snap.port(Port::One).pad.buttons.bits(), button::CIRCLE);
    assert_eq!(snap.port(Port::One).updates, 60);
}

// ----------------------------------------------------- pad speed and wire

#[test]
fn pads_slow_and_fast_to_acknowledge_are_read_correctly() {
    for ack_delay in [1, 40, 400, 3_000, 9_000, 13_000] {
        for ack_width in [20, 400, 1_500] {
            let mut rig = Rig::new(Config::DEFAULT);
            rig.plug(
                Port::One,
                Pad {
                    ack_delay,
                    ack_width,
                    buttons: button::SQUARE | button::UP,
                    ..Pad::dualshock_analog()
                },
            );
            rig.frames(3);
            let one = *rig.snap().port(Port::One);
            assert_eq!(
                one.health,
                Health::Present,
                "delay {ack_delay} width {ack_width}"
            );
            assert_eq!(one.pad.buttons.bits(), button::SQUARE | button::UP);
            assert_eq!(one.faults, 0);
        }
    }
}

#[test]
fn a_pad_that_slips_if_clocked_early_never_slips_under_ack_pacing() {
    let mut rig = Rig::new(Config::DEFAULT);
    rig.plug(
        Port::One,
        Pad {
            ack_delay: 2_500,
            slips_if_early: true,
            buttons: button::TRIANGLE,
            ..Pad::dualshock_analog()
        },
    );
    rig.frames(5);
    let one = *rig.snap().port(Port::One);
    assert_eq!((one.health, one.faults), (Health::Present, 0));
    assert_eq!(one.pad.buttons.bits(), button::TRIANGLE);
}

#[test]
fn fixed_pacing_reads_a_pad_inside_its_gap_and_slips_one_outside_it() {
    let timed = Config {
        pacing: BytePacing::Timed,
        ..Config::DEFAULT
    };
    let slow = Pad {
        ack_delay: 2_500,
        slips_if_early: true,
        buttons: button::TRIANGLE,
        ..Pad::dualshock_analog()
    };
    // The default gap is longer than the pad's ACK: clean.
    let mut rig = Rig::new(timed);
    rig.plug(Port::One, slow.clone());
    rig.frames(2);
    let one = *rig.snap().port(Port::One);
    assert_eq!(one.pad.buttons.bits(), button::TRIANGLE);
    assert_eq!(one.faults, 0);
    // A gap shorter than the pad's ACK clocks the next byte early.
    let mut rig = Rig::new(Config {
        timed_gap_cycles: 1_500,
        ..timed
    });
    rig.plug(Port::One, slow);
    rig.frames(2);
    let one = *rig.snap().port(Port::One);
    assert!(one.faults > 0 || one.pad.buttons.bits() != button::TRIANGLE);
}

#[test]
fn fixed_pacing_never_arms_the_ack_interrupt_or_touches_control_mid_packet() {
    let mut rig = Rig::new(Config {
        pacing: BytePacing::Timed,
        ..Config::DEFAULT
    });
    rig.plug(Port::One, analog_with(button::START));
    rig.frames(4);
    assert!(rig.sim().selects.iter().all(|&(_, irq)| !irq));
    assert_eq!(rig.sim().latch_clears, 0);
    assert_eq!(rig.snap().port(Port::One).pad.buttons.bits(), button::START);
    // And an empty socket reads as absent without an `/ACK`.
    assert_eq!(rig.snap().port(Port::Two).health, Health::Absent);
}

#[test]
fn a_change_of_pacing_waits_for_the_next_packet() {
    // The packet in flight was selected for ACK pacing; switching to fixed
    // pacing under it must not make its next byte look unacknowledged.
    let mut rig = Rig::new(Config::PORT1_ONLY);
    rig.plug(Port::One, analog_with(button::SELECT));
    rig.engine.on_vblank();
    rig.run_until(20_000);
    assert!(!rig.engine.is_idle(), "the packet is part way");
    rig.engine.configure(Config {
        pacing: BytePacing::Timed,
        ..Config::PORT1_ONLY
    });
    rig.run_until(FRAME);
    let one = *rig.snap().port(Port::One);
    assert_eq!((one.health, one.faults), (Health::Present, 0));
    rig.frame();
    // The next packet is fixed-paced: its select did not arm the interrupt.
    assert_eq!(rig.sim().selects.last(), Some(&(Port::One, false)));
    assert_eq!(rig.snap().port(Port::One).faults, 0);
}

#[test]
fn ack_pacing_writes_control_once_per_acknowledged_byte() {
    let mut rig = Rig::new(Config::PORT1_ONLY);
    rig.plug(Port::One, Pad::dualshock_analog());
    rig.frame();
    // Eight acknowledged bytes of nine; the last is not acknowledged.
    assert_eq!(rig.sim().latch_clears, 8);
}

// ------------------------------------------------------------- failures

#[test]
fn a_packet_that_fails_after_a_good_one_keeps_the_last_clean_state() {
    let mut rig = Rig::new(Config::PORT1_ONLY);
    rig.plug(Port::One, analog_with(button::CROSS));
    rig.frame();
    assert_eq!(rig.snap().port(Port::One).pad.buttons.bits(), button::CROSS);
    rig.pad(Port::One).drop_ack_at = Some(4);
    rig.frame();
    let one = *rig.snap().port(Port::One);
    assert_eq!(one.health, Health::Faulted);
    assert_eq!(one.last_fault, Some(Fault::AckTimeout));
    assert_eq!(
        one.pad.buttons.bits(),
        button::CROSS,
        "held button stays held"
    );
    assert_eq!(rig.sim().resets, 1, "the abandoned reply is reset away");
    rig.pad(Port::One).drop_ack_at = None;
    rig.frame();
    let one = *rig.snap().port(Port::One);
    assert_eq!((one.health, one.faults), (Health::Present, 1));
}

#[test]
fn a_packet_that_loses_an_acknowledgement_part_way_is_rejected_whole() {
    for drop in 2..8u8 {
        let mut rig = Rig::new(Config::PORT1_ONLY);
        rig.plug(
            Port::One,
            Pad {
                drop_ack_at: Some(drop),
                buttons: button::CROSS,
                ..Pad::dualshock_analog()
            },
        );
        rig.frame();
        let one = *rig.snap().port(Port::One);
        assert_eq!(one.health, Health::Faulted, "dropped at {drop}");
        assert_eq!(
            one.pad.buttons.bits(),
            0,
            "nothing decoded from a partial packet"
        );
        assert_eq!(one.pad.mode, PadMode::Disconnected, "no state to keep yet");
    }
}

#[test]
fn a_wrong_id_or_magic_is_rejected() {
    let mut rig = Rig::new(Config::PORT1_ONLY);
    rig.plug(
        Port::One,
        Pad {
            magic: 0x12,
            ..Pad::dualshock_analog()
        },
    );
    rig.frame();
    assert_eq!(rig.snap().port(Port::One).last_fault, Some(Fault::BadMagic));
    rig.plug(
        Port::One,
        Pad {
            id: 0x55,
            ..Pad::dualshock_analog()
        },
    );
    rig.frame();
    assert_eq!(rig.snap().port(Port::One).last_fault, Some(Fault::BadId));
}

#[test]
fn a_lost_interrupt_is_recovered_at_the_next_vblank() {
    let mut rig = Rig::new(Config::PORT1_ONLY);
    rig.plug(Port::One, analog_with(button::START));
    rig.sim().mute = true;
    rig.frame();
    rig.frame();
    assert_eq!(rig.engine.stats().stalls, 1);
    assert!(rig.engine.is_idle());
    assert_eq!(rig.sim().resets, 1);
    rig.sim().mute = false;
    rig.sim().deadline = None;
    rig.frame();
    assert_eq!(rig.snap().port(Port::One).pad.buttons.bits(), button::START);
}

#[test]
fn an_interrupt_left_over_from_a_replaced_deadline_is_ignored() {
    let mut rig = Rig::new(Config::PORT1_ONLY);
    rig.plug(Port::One, Pad::dualshock_analog());
    rig.engine.on_vblank();
    // The setup deadline is armed and not yet reached: a stray timer
    // interrupt must not start the packet.
    rig.engine.on_deadline();
    assert!(rig.sim().sent.is_empty());
    rig.run_until(FRAME);
    assert_eq!(rig.snap().port(Port::One).health, Health::Present);
}

// -------------------------------------------------------------- plugging

#[test]
fn a_pad_plugged_in_later_is_found_by_the_next_frame_and_removal_by_the_one_after() {
    let mut rig = Rig::new(Config::DEFAULT);
    rig.plug(Port::One, Pad::dualshock_analog());
    rig.frames(3);
    assert_eq!(rig.snap().port(Port::Two).health, Health::Absent);
    rig.plug(Port::Two, analog_with(button::R1));
    rig.frame();
    let two = *rig.snap().port(Port::Two);
    assert_eq!(
        (two.health, two.pad.buttons.bits()),
        (Health::Present, button::R1)
    );
    rig.unplug(Port::Two);
    rig.frame();
    let two = *rig.snap().port(Port::Two);
    assert_eq!(
        (two.health, two.pad.mode),
        (Health::Absent, PadMode::Disconnected)
    );
    assert_eq!(rig.snap().port(Port::One).health, Health::Present);
}

#[test]
fn the_kick_rate_divides_the_vblank_rate() {
    let mut rig = Rig::new(Config {
        kick_every: 2,
        ..Config::PORT1_ONLY
    });
    rig.plug(Port::One, Pad::dualshock_analog());
    rig.frames(10);
    assert_eq!(rig.engine.stats().kicks, 5);
    assert_eq!(rig.snap().port(Port::One).updates, 5);
}

#[test]
fn a_port_left_out_of_the_config_is_never_selected() {
    let mut rig = Rig::new(Config::PORT1_ONLY);
    rig.plug(Port::Two, Pad::dualshock_analog());
    rig.frames(3);
    assert!(rig.sim().selects.iter().all(|&(p, _)| p == Port::One));
    assert_eq!(rig.snap().port(Port::Two).health, Health::Unseen);
}

// -------------------------------------------------------------- analog

#[test]
fn an_analog_request_sends_one_command_per_frame_and_reports_the_result() {
    let mut rig = Rig::new(Config::PORT1_ONLY);
    rig.plug(
        Port::One,
        Pad {
            id: 0x41,
            analog_requested: false,
            ..Pad::dualshock_analog()
        },
    );
    rig.frame();
    assert_eq!(rig.snap().port(Port::One).pad.mode, PadMode::Digital);
    rig.engine.request_analog(Port::One);
    assert!(rig.engine.analog_pending(Port::One));
    rig.frame();
    assert_eq!(rig.sim().sent.last().map(|&(_, b)| b), Some(0x00));
    let commands: Vec<u8> = rig
        .sim()
        .sent
        .iter()
        .filter(|(_, b)| matches!(b, 0x43 | 0x44))
        .map(|&(_, b)| b)
        .collect();
    assert_eq!(commands, [0x43], "one command in the first frame");
    rig.frames(2);
    let commands: Vec<u8> = rig
        .sim()
        .sent
        .iter()
        .filter(|(_, b)| matches!(b, 0x43 | 0x44))
        .map(|&(_, b)| b)
        .collect();
    assert_eq!(commands, [0x43, 0x44, 0x43], "three, one per frame");
    rig.frame();
    assert_eq!(
        rig.engine.analog_outcome(Port::One),
        Some(AnalogRequirement::Analog)
    );
    assert!(!rig.engine.analog_pending(Port::One));
    assert_eq!(rig.snap().port(Port::One).pad.mode, PadMode::Analog);
}

#[test]
fn a_second_request_while_one_runs_changes_nothing() {
    let mut rig = Rig::new(Config::PORT1_ONLY);
    rig.plug(
        Port::One,
        Pad {
            id: 0x41,
            analog_requested: false,
            ..Pad::dualshock_analog()
        },
    );
    rig.engine.request_analog(Port::One);
    rig.frame();
    rig.engine.request_analog(Port::One);
    rig.frames(5);
    assert_eq!(
        rig.engine.analog_outcome(Port::One),
        Some(AnalogRequirement::Analog)
    );
    let commands: Vec<u8> = rig
        .sim()
        .sent
        .iter()
        .filter(|(_, b)| matches!(b, 0x43 | 0x44))
        .map(|&(_, b)| b)
        .collect();
    assert_eq!(
        commands,
        [0x43, 0x44, 0x43],
        "the second request did not restart it"
    );
}

#[test]
fn a_pad_parked_in_config_mode_is_sent_the_exit_again() {
    let mut rig = Rig::new(Config::PORT1_ONLY);
    rig.plug(
        Port::One,
        Pad {
            id: 0x41,
            analog_requested: false,
            ignore_exits: 2,
            ..Pad::dualshock_analog()
        },
    );
    rig.engine.request_analog(Port::One);
    rig.frames(12);
    assert_eq!(
        rig.engine.analog_outcome(Port::One),
        Some(AnalogRequirement::Analog)
    );
    // A pad that never lets go is reported, not retried forever.
    let mut rig = Rig::new(Config::PORT1_ONLY);
    rig.plug(
        Port::One,
        Pad {
            id: 0x41,
            ignore_exits: 100,
            ..Pad::dualshock_analog()
        },
    );
    rig.engine.request_analog(Port::One);
    rig.frames(20);
    assert_eq!(
        rig.engine.analog_outcome(Port::One),
        Some(AnalogRequirement::DigitalOnly)
    );
}

#[test]
fn analog_requests_for_a_digital_pad_and_an_empty_port_say_so() {
    let mut rig = Rig::new(Config::DEFAULT);
    rig.plug(Port::One, Pad::digital());
    rig.engine.request_analog(Port::One);
    rig.engine.request_analog(Port::Two);
    rig.frames(8);
    assert_eq!(
        rig.engine.analog_outcome(Port::One),
        Some(AnalogRequirement::DigitalOnly)
    );
    assert_eq!(
        rig.engine.analog_outcome(Port::Two),
        Some(AnalogRequirement::Absent)
    );
}

// ----------------------------------------------------------- the lease

#[test]
fn a_lease_is_refused_mid_packet_and_holds_the_engine_off_the_port() {
    let mut rig = Rig::new(Config::PORT1_ONLY);
    rig.plug(Port::One, analog_with(button::L1));
    rig.engine.on_vblank();
    assert!(!rig.engine.try_lease(), "a packet is on the wire");
    rig.run_until(FRAME);
    assert!(rig.engine.try_lease());
    assert_eq!(rig.engine.owner(), Owner::Lease);
    let seq = rig.snap().seq;
    let accesses = rig.sim().accesses;
    rig.frames(3);
    assert_eq!(
        rig.sim().accesses,
        accesses,
        "the engine did not touch the port"
    );
    assert_eq!(rig.snap().seq, seq);
    assert_eq!(rig.engine.stats().leased_skips, 3);
    rig.engine.release_lease();
    rig.frame();
    assert_eq!(rig.snap().seq, seq + 1);
    assert_eq!(rig.snap().port(Port::One).pad.buttons.bits(), button::L1);
}

// ------------------------------------------------------------ snapshots

#[test]
fn the_snapshot_sequence_counts_publications_and_always_reads_the_latest() {
    let mut rig = Rig::new(Config::PORT1_ONLY);
    rig.plug(Port::One, analog_with(0));
    let mut last = rig.snap().seq;
    assert_eq!(last, 0);
    for held in [button::UP, button::DOWN, button::LEFT] {
        rig.pad(Port::One).buttons = held;
        rig.frame();
        let snap = rig.snap();
        assert_eq!(snap.seq, last.wrapping_add(1));
        assert_eq!(snap.port(Port::One).pad.buttons.bits(), held);
        last = snap.seq;
    }
}

#[test]
fn a_new_engine_on_the_same_snapshot_starts_its_counters_again() {
    // Uninstall and install again builds a new engine over the same published
    // snapshot; the old engine's counts must not outlive it.
    let mut rig = Rig::new(Config::PORT1_ONLY);
    rig.plug(Port::One, analog_with(button::START));
    rig.frames(5);
    assert_eq!(rig.snap().port(Port::One).updates, 5);
    let published = rig.published;
    let sim = Sim::new();
    let again = Engine::new(sim, published, Config::PORT1_ONLY);
    again.publish_readings();
    let snap = published.read();
    assert_eq!(snap.port(Port::One).updates, 0);
    assert_eq!(snap.port(Port::One).health, Health::Unseen);
}

// -------------------------------------------------------------- rumble

use crate::Rumble;

fn commands(rig: &mut Rig, port: Port) -> Vec<u8> {
    rig.pad(port)
        .cmds
        .iter()
        .copied()
        .filter(|&c| c != 0x42)
        .collect()
}

#[test]
fn enabling_rumble_maps_the_motors_in_one_visit_and_then_every_poll_carries_them() {
    let mut rig = Rig::new(Config::PORT1_ONLY);
    rig.plug(Port::One, Pad::dualshock_analog());
    rig.frame();
    rig.engine.enable_rumble(Port::One);
    rig.frames(5);
    assert_eq!(
        commands(&mut rig, Port::One),
        [0x43, 0x4D, 0x43],
        "enter, map, exit"
    );
    assert!(rig.pad(Port::One).mapped);
    assert!(rig.engine.rumble_mapped(Port::One));
    assert_eq!(rig.pad(Port::One).id, 0x73, "analog mode left as it was");
    rig.engine.set_rumble(Port::One, Rumble::new(true, 200));
    rig.frames(2);
    assert_eq!(rig.pad(Port::One).motors, (true, 200));
    let last = *rig.pad(Port::One).motor_tx.last().unwrap();
    assert_eq!(last, (0x73, 0x01, 200));
}

#[test]
fn an_analog_request_with_the_motors_wanted_maps_them_in_the_same_visit() {
    let mut rig = Rig::new(Config::PORT1_ONLY);
    rig.plug(
        Port::One,
        Pad {
            id: 0x41,
            analog_requested: false,
            ..Pad::dualshock_analog()
        },
    );
    rig.engine.enable_rumble(Port::One);
    rig.frames(1);
    // The rumble job started first; let it finish, then ask for analog mode.
    rig.frames(6);
    rig.engine.request_analog(Port::One);
    rig.frames(8);
    assert_eq!(
        rig.engine.analog_outcome(Port::One),
        Some(AnalogRequirement::Analog)
    );
    assert!(rig.engine.rumble_mapped(Port::One));
    // Wanted with nothing plugged in, then a digital-mode DualShock appears and
    // analog mode is asked for: one visit to configuration mode does both.
    let mut rig = Rig::new(Config::PORT1_ONLY);
    rig.engine.enable_rumble(Port::One);
    rig.frames(3);
    assert!(!rig.engine.analog_pending(Port::One));
    rig.plug(
        Port::One,
        Pad {
            id: 0x41,
            analog_requested: false,
            ..Pad::dualshock_analog()
        },
    );
    rig.frame();
    rig.engine.request_analog(Port::One);
    rig.frames(8);
    assert_eq!(commands(&mut rig, Port::One), [0x43, 0x44, 0x4D, 0x43]);
    assert_eq!(
        rig.engine.analog_outcome(Port::One),
        Some(AnalogRequirement::Analog)
    );
    assert!(rig.engine.rumble_mapped(Port::One));
}

#[test]
fn stopping_the_motors_sends_zeros_on_the_next_poll() {
    let mut rig = Rig::new(Config::PORT1_ONLY);
    rig.plug(Port::One, Pad::dualshock_analog());
    rig.engine.enable_rumble(Port::One);
    rig.frames(5);
    rig.engine.set_rumble(Port::One, Rumble::new(true, 255));
    rig.frames(2);
    assert_eq!(rig.pad(Port::One).motors, (true, 255));
    assert!(rig.engine.motors_requested());
    rig.engine.stop_motors();
    assert!(!rig.engine.motors_requested());
    rig.frame();
    assert_eq!(rig.pad(Port::One).motors, (false, 0));
    // And disabling stops asking for them at all.
    rig.engine.set_rumble(Port::One, Rumble::new(true, 9));
    rig.engine.disable_rumble(Port::One);
    assert!(rig.engine.rumble(Port::One).is_off());
}

#[test]
fn a_digital_pad_is_asked_once_and_hears_only_zeros() {
    let mut rig = Rig::new(Config::PORT1_ONLY);
    rig.plug(Port::One, Pad::digital());
    rig.engine.enable_rumble(Port::One);
    rig.engine.set_rumble(Port::One, Rumble::new(true, 255));
    rig.frames(12);
    assert!(!rig.engine.rumble_mapped(Port::One));
    assert!(rig.engine.rumble_refused(Port::One));
    let cmds = commands(&mut rig, Port::One);
    assert_eq!(cmds, [0x43, 0x4D, 0x43], "asked once, not every frame");
    assert!(rig
        .pad(Port::One)
        .motor_tx
        .iter()
        .all(|&(_, s, l)| (s, l) == (0, 0)));
}

#[test]
fn a_pad_that_refuses_configuration_mode_is_asked_once_and_not_driven() {
    let mut rig = Rig::new(Config::PORT1_ONLY);
    rig.plug(
        Port::One,
        Pad {
            dualshock: false,
            ..Pad::dualshock_analog()
        },
    );
    rig.engine.enable_rumble(Port::One);
    rig.engine.set_rumble(Port::One, Rumble::new(true, 255));
    rig.frames(12);
    assert!(rig.engine.rumble_refused(Port::One));
    assert_eq!(commands(&mut rig, Port::One), [0x43, 0x4D, 0x43]);
    assert!(rig
        .pad(Port::One)
        .motor_tx
        .iter()
        .all(|&(_, s, l)| (s, l) == (0, 0)));
    assert_eq!(rig.pad(Port::One).motors, (false, 0));
    // Wanting it again asks again.
    rig.engine.enable_rumble(Port::One);
    rig.frames(6);
    assert_eq!(commands(&mut rig, Port::One).len(), 6);
}

#[test]
fn a_replugged_pad_is_mapped_again_and_a_pad_that_vanishes_mid_sequence_is_not() {
    let mut rig = Rig::new(Config::PORT1_ONLY);
    rig.plug(Port::One, Pad::dualshock_analog());
    rig.engine.enable_rumble(Port::One);
    rig.engine.set_rumble(Port::One, Rumble::new(false, 120));
    rig.frames(6);
    assert_eq!(rig.pad(Port::One).motors, (false, 120));
    // Unplugged and plugged in again: a fresh pad has no mapping.
    rig.unplug(Port::One);
    rig.frame();
    assert!(!rig.engine.rumble_mapped(Port::One));
    rig.plug(Port::One, Pad::dualshock_analog());
    rig.frames(6);
    assert!(
        rig.pad(Port::One).mapped,
        "mapped again without being asked"
    );
    assert_eq!(rig.pad(Port::One).motors, (false, 120));
    // Gone between the enter and the map: the request ends Absent, nothing is
    // mapped, and the next pad is mapped when it appears.
    let mut rig = Rig::new(Config::PORT1_ONLY);
    rig.plug(Port::One, Pad::dualshock_analog());
    rig.engine.enable_rumble(Port::One);
    rig.frame();
    rig.unplug(Port::One);
    rig.frames(2);
    assert!(!rig.engine.rumble_mapped(Port::One));
    assert_eq!(
        rig.engine.analog_outcome(Port::One),
        Some(AnalogRequirement::Absent)
    );
    rig.plug(Port::One, Pad::dualshock_analog());
    rig.frames(6);
    assert!(rig.engine.rumble_mapped(Port::One));
}

#[test]
fn motors_on_the_second_port_are_independent_of_the_first() {
    let mut rig = Rig::new(Config::DEFAULT);
    rig.plug(Port::One, Pad::dualshock_analog());
    rig.plug(Port::Two, Pad::dualshock_analog());
    rig.engine.enable_rumble(Port::One);
    rig.engine.enable_rumble(Port::Two);
    rig.frames(6);
    rig.engine.set_rumble(Port::Two, Rumble::new(true, 77));
    rig.frames(2);
    assert_eq!(rig.pad(Port::One).motors, (false, 0));
    assert_eq!(rig.pad(Port::Two).motors, (true, 77));
}

#[test]
fn rumble_enabled_during_an_analog_request_does_not_erase_its_answer() {
    let mut rig = Rig::new(Config::PORT1_ONLY);
    rig.plug(
        Port::One,
        Pad {
            id: 0x41,
            analog_requested: false,
            ..Pad::dualshock_analog()
        },
    );
    rig.frame();
    rig.engine.request_analog(Port::One);
    rig.frame();
    // The request is under way as a plain analog job; the motors are wanted
    // after it started, so the engine maps them once the pad reads analog.
    rig.engine.enable_rumble(Port::One);
    // The verifying poll is the first to read analog mode; the answer must be
    // there when it is published, even though the motor mapping starts at once.
    for _ in 0..12 {
        rig.frame();
        if rig.snap().port(Port::One).pad.mode == PadMode::Analog {
            break;
        }
    }
    assert_eq!(
        rig.engine.analog_outcome(Port::One),
        Some(AnalogRequirement::Analog),
        "the rumble job started by the same poll must not erase the answer"
    );
    rig.frames(8);
    assert!(rig.engine.rumble_mapped(Port::One));
}
