// SPDX-License-Identifier: GPL-2.0-or-later
//! [`Controllers`]: the one way a game reads its pads.
//!
//! Every game runs this implementation and puts the same bytes on the wire:
//!
//! * `poll(bus, port)` reads one port. A game reads port 1 with one call per loop; a
//!   two-player game makes a second call for port 2. A port nobody asks for costs nothing.
//! * A poll leaves the next one's select open: it asserts `/CS` on the port it expects to be
//!   asked for next (the port polled before this one, so a two-player loop alternates, and
//!   a one-player loop reopens its own), and the next poll starts clocking bytes as soon as
//!   the pad's setup time ([`PAD_SETUP_CYCLES`]) has passed since then. The game's frame runs
//!   in that time instead of a busy wait. The wait is measured on the system clock, so a poll
//!   that follows the last one at once still waits the whole of it, and a select that a
//!   memory-card transfer or a configuration packet released is made again.
//! * The first time a port answers, the same handshake runs on it: analog mode requested
//!   and locked, a video frame between packets, as Sony's libpad does. It blocks for a few
//!   frames, once per plug-in. Each packet goes out with CPU interrupts masked: a pad's
//!   `/ACK` is a pulse of about 90 cycles, a handler that runs over it hides it, and a
//!   packet abandoned part way is not retried and its answer not read. A pad that answered
//!   the handshake like a DualShock but still reports digital is asked again, [`RETRIES`]
//!   times at most, each a handshake of its own (a few video frames) after the last. A
//!   digital pad, which answers no configuration packet in full, is never asked again.
//! * A port that did not answer is not polled again for 59 polls, then probed once: an
//!   empty port 2 costs one probe a second, not one every frame. A pad that was there has
//!   its absence confirmed by four tries before it is believed. No select is left open on
//!   an empty port, so a pad plugged in always sees a fresh one.
//! * A packet that fails part way returns the last clean state, so a held button is never
//!   read as released for a frame.
//! * [`Controllers::set_rumble`] is the only call a game that rumbles makes beyond `poll`:
//!   the first time for a port (and after a re-plug) it maps the motors, a few frames, and
//!   the request then rides every poll. A game that never calls it never links the mapping.
//!
//! * A game that never uses sticks makes its pads with [`Controllers::digital`]: the same
//!   `poll`, the handshake, its retries and the motor mapping not compiled in, every
//!   poll the five bytes a digital pad answers (nine from a pad that is already analog).
//!
//! ```text
//! let mut pads = psx_pad::Controllers::new();           // a static, or a field
//! let one = *pads.poll(&mut peripherals.controller_port, Port::One);
//! ```
//!
//! [`PAD_SETUP_CYCLES`]: psx_io::controller_port::PAD_SETUP_CYCLES

use crate::{
    pad_begin, pad_delay, pad_exchange, poll_state, AnalogRequirement, PadMode, PadState, Port,
    Rumble, MOTOR_MAP_PACKET, REQUIRE_ANALOG_GAP_SPINS,
};
use psx_io::controller_port::{Timing, Transport};

/// Polls an absent port is left alone between probes: one probe in 60.
const ABSENT_SKIP: u8 = 59;

/// Packets sent again to a pad that answered the handshake like a DualShock and is not analog
/// after it, at most for a plug-in: the whole handshake to one that reports digital, the exit
/// alone to one parked in configuration mode. Each is a few video frames after the last,
/// because the handshake spaces its packets so.
pub const RETRIES: u8 = 3;

/// Whether a poll leaves the next one's select open. Off until a console has run
/// hardware-tests v2.13 (`SIO0 SELECT EARLY`, records `0x6F0`-`0x6F4`) and passed: until then
/// every poll selects and waits as before, and the open select is exercised only by that step.
/// After the verdict this constant goes, with the losing sequence.
const SELECT_EARLY: bool = false;

/// The configuration packets: enter configuration mode, request analog mode and lock it,
/// map the motors, leave. The analog handshake sends 0, 1, 3; the rumble one 0, 2, 3.
/// Built in registers, not read from a table in memory.
#[inline(always)]
fn packet(index: usize) -> [u8; 8] {
    match index {
        0 => [0x43, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
        1 => [0x44, 0x00, 0x01, 0x03, 0x00, 0x00, 0x00, 0x00],
        2 => MOTOR_MAP_PACKET,
        _ => [0x43, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    }
}

/// Which packets a sequence sends.
#[derive(Copy, Clone, PartialEq)]
enum Sequence {
    Analog,
    Rumble,
}

/// What one port remembers between polls besides its state.
#[derive(Copy, Clone)]
struct Slot {
    /// Polls left before an absent port is probed again.
    skip: u8,
    /// A pad answered the last probe; the handshake has run on it.
    present: bool,
    /// The motors were mapped on the pad now in the port.
    mapped: bool,
    /// What the motors are asked for on every poll.
    rumble: Rumble,
    /// Retries left for the pad in the port, [`RETRIES`] if it answered the handshake like a
    /// DualShock.
    again: u8,
}

impl Slot {
    const NEW: Self = Self {
        skip: 0,
        present: false,
        mapped: false,
        rumble: Rumble::OFF,
        again: 0,
    };

    /// One poll of `socket`, starting from the select `opened` left open if there is one;
    /// `last` is its latest clean state and is updated. Inlined into [`Controllers::step`].
    #[inline(always)]
    fn step<T: Transport, const ANALOG: bool>(
        &mut self,
        bus: &mut T,
        socket: Port,
        mut opened: Option<u16>,
        last: &mut PadState,
    ) {
        let mut confirm = self.present;
        // One call site of the poll: the poll itself, the poll after the handshake,
        // and the poll after an exit sent to a pad parked in configuration mode (the
        // SCPH-110 failure, retried a bounded number of times).
        let state = loop {
            let state = poll_state(
                bus,
                socket,
                confirm,
                if ANALOG { self.rumble } else { Rumble::OFF },
                opened.take(),
            );
            if !ANALOG || (state.mode == PadMode::Analog && self.present) {
                break state;
            }
            if !self.settle(bus, socket, state.mode) {
                break state;
            }
            confirm = true;
        };
        match state.mode {
            // A garbled packet: the last clean state stands.
            PadMode::Unknown => {}
            PadMode::Disconnected => {
                self.present = false;
                self.mapped = false;
                self.skip = ABSENT_SKIP;
                *last = state;
            }
            _ => {
                self.present = true;
                *last = state;
            }
        }
    }
}

impl Slot {
    /// What a poll that did not find the pad analog and settled does next: send the handshake
    /// (the first answer of a pad), the exit alone (to a pad parked in configuration mode, the
    /// SCPH-110 failure) or the handshake again (a pad that answered it like a DualShock and
    /// still reports digital: something broke it). The two retries share one budget, [`RETRIES`]
    /// a plug-in. `false` when there is nothing to send and the poll stands. Out of line: a
    /// game's frames run the poll, not this.
    #[cold]
    #[inline(never)]
    fn settle<T: Transport>(&mut self, bus: &mut T, socket: Port, mode: PadMode) -> bool {
        let fresh = mode.is_connected() && !self.present;
        let first = if fresh {
            self.present = true;
            0
        } else if self.again != 0
            && matches!(mode, PadMode::Config | PadMode::Digital | PadMode::Unknown)
        {
            self.again -= 1;
            if mode == PadMode::Config {
                2
            } else {
                0
            }
        } else {
            return false;
        };
        let capable = config(bus, socket, Sequence::Analog, first);
        if fresh {
            self.again = if capable { RETRIES } else { 0 };
        }
        true
    }
}

/// The packets of `sequence` from the `first`-th on, each a video frame after the last: the
/// address byte, then the eight command bytes, each waiting for the pad's `/ACK` like a poll
/// does, with CPU interrupts masked from the first byte to the last (the select and its setup
/// time are outside: an interrupt there only makes the gap longer). The answers are not read:
/// the poll that follows says what the pad settled on. From the last packet only, it is the
/// exit sent again to a pad parked in configuration mode. Returns whether the last packet, the
/// exit, was answered in full: a DualShock in configuration mode answers all nine bytes, a
/// digital pad stops acknowledging after the fourth and never entered it.
#[inline(never)]
fn config<T: Transport>(bus: &mut T, socket: Port, sequence: Sequence, first: usize) -> bool {
    // `black_box` keeps LLVM from unrolling the packets into copies.
    let end = core::hint::black_box(3);
    let mut n = first;
    let mut ok = false;
    while n < end {
        let index = match n {
            0 => 0,
            1 if sequence == Sequence::Analog => 1,
            1 => 2,
            _ => 3,
        };
        let packet = packet(index);
        ok = pad_begin(bus, socket, None);
        let enabled = bus.mask_interrupts();
        let mut i = 0;
        while ok && i <= packet.len() {
            let tx = if i == 0 { 0x01 } else { packet[i - 1] };
            ok = pad_exchange(bus, socket, tx, i == packet.len(), Timing::PAD).is_ok();
            i += 1;
        }
        bus.restore_interrupts(enabled);
        bus.finish(ok);
        pad_delay(bus, REQUIRE_ANALOG_GAP_SPINS);
        n += 1;
    }
    ok
}

/// The controller ports, polled the one way every game polls them. See the module
/// documentation.
#[derive(Copy, Clone)]
pub struct Controllers<const ANALOG: bool = true> {
    slots: [Slot; 2],
    /// The latest clean state of each port.
    last: [PadState; 2],
    /// The select the last poll left open: its port and when it was asserted.
    open: Option<(Port, u16)>,
    /// The port polled before the last one.
    previous: Port,
}

impl Controllers<true> {
    /// Two ports that have seen nothing yet.
    #[must_use]
    pub const fn new() -> Self {
        Self::NEW
    }

    /// What the motors of `port`'s pad are asked for on every poll from now on. The first
    /// call for a port, and the first after a pad was plugged in again, maps the motors
    /// first (a few frames, like the analog handshake); a game that never rumbles never
    /// calls this and never links the mapping.
    pub fn set_rumble<T: Transport>(&mut self, bus: &mut T, port: Port, rumble: Rumble) {
        let slot = &mut self.slots[usize::from(port.is_two())];
        slot.rumble = rumble;
        if slot.present && !slot.mapped {
            slot.mapped = true;
            config(bus, port, Sequence::Rumble, 0);
        }
    }
}

impl Controllers<false> {
    /// Two ports that have seen nothing yet, for a game that never uses sticks: polls read
    /// whatever mode the pad is in, and the handshake, its retries and the motor mapping are
    /// not compiled in.
    #[must_use]
    pub const fn digital() -> Self {
        Self::NEW
    }
}

impl<const ANALOG: bool> Controllers<ANALOG> {
    const NEW: Self = Self {
        slots: [Slot::NEW; 2],
        last: [PadState::NONE; 2],
        open: None,
        previous: Port::One,
    };

    /// Poll `port` once and return its latest clean state. `bus` is the `ControllerPort`
    /// token (or any [`Transport`]).
    pub fn poll<T: Transport>(&mut self, bus: &mut T, port: Port) -> &PadState {
        self.step(bus, port);
        self.last(port)
    }

    /// [`poll`](Self::poll), out of line: one copy for every call site.
    #[inline(never)]
    fn step<T: Transport>(&mut self, bus: &mut T, port: Port) {
        let i = usize::from(port.is_two());
        let slot = &mut self.slots[i];
        if slot.skip != 0 {
            // An empty port between probes: nothing goes on the wire and an open select
            // stays open.
            slot.skip -= 1;
            return;
        }
        // The open select and the port polled before this one are the early select's
        // bookkeeping: a poll with it off does none.
        let opened = if SELECT_EARLY {
            match self.open.take() {
                Some((open, at)) if open == port => Some(at),
                _ => None,
            }
        } else {
            None
        };
        slot.step::<T, ANALOG>(bus, port, opened, &mut self.last[i]);
        if SELECT_EARLY {
            // Open the port polled before this one if a pad is in it, else this one.
            let mut next = core::mem::replace(&mut self.previous, port);
            if !self.slots[usize::from(next.is_two())].present {
                next = port;
            }
            if self.slots[usize::from(next.is_two())].present {
                self.open = Some((next, bus.open(next)));
            }
        }
    }

    /// The latest clean state of `port`, without polling.
    #[must_use]
    pub fn last(&self, port: Port) -> &PadState {
        &self.last[usize::from(port.is_two())]
    }

    /// What `port` held at the last poll, for a game that shows a notice: analog and
    /// locked, a digital pad, or nothing.
    #[must_use]
    pub fn analog(&self, port: Port) -> AnalogRequirement {
        AnalogRequirement::from_mode(self.last(port).mode)
    }
}

impl Default for Controllers<true> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::button;
    use crate::mock_sio::{self as mock, MockBus, Model};
    use psx_hw::sio::sio0;

    /// Port 1 and port 2 each with a model of their own: the active one is
    /// the thread's model, the other waits here.
    struct TwoPorts {
        parked: [Model; 2],
        active: usize,
        selects: [usize; 2],
    }

    impl TwoPorts {
        fn new(one: Model, two: Model) -> Self {
            mock::reset(one);
            Self {
                parked: [Model::default(), two],
                active: 0,
                selects: [0, 0],
            }
        }
        fn model(&mut self, port: usize, f: impl FnOnce(&mut Model)) {
            if port == self.active {
                mock::with(f);
            } else {
                f(&mut self.parked[port]);
            }
        }
    }

    impl Transport for TwoPorts {
        fn status(&mut self) -> u32 {
            MockBus.status()
        }
        fn receive(&mut self) -> u8 {
            MockBus.receive()
        }
        fn transmit(&mut self, byte: u8) {
            MockBus.transmit(byte);
        }
        fn set_mode(&mut self, value: u16) {
            MockBus.set_mode(value);
        }
        fn set_baud(&mut self, value: u16) {
            MockBus.set_baud(value);
        }
        fn set_control(&mut self, value: u16) {
            MockBus.set_control(value);
        }
        fn control(&mut self) -> u16 {
            MockBus.control()
        }
        fn clock(&mut self) -> u16 {
            MockBus.clock()
        }
        fn select_with(&mut self, port: Port, arm_ack_irq: bool) {
            let index = usize::from(port.is_two());
            if index != self.active {
                let mine = mock::with(core::mem::take);
                self.parked[self.active] = mine;
                mock::reset(core::mem::take(&mut self.parked[index]));
                self.active = index;
            }
            self.selects[index] += 1;
            self.set_mode(sio0::MODE_8N1);
            self.set_baud(sio0::BAUD_250KHZ);
            self.set_control(sio0::ctrl::ACK);
            self.set_control(sio0::selected_ctrl(port.is_two(), arm_ack_irq));
        }
    }

    fn dualshock(buttons: u16) -> Model {
        Model {
            id: 0x41,
            dualshock: true,
            buttons,
            axes: [0x80, 0x80, 0x80, 0x80],
            ..Model::default()
        }
    }

    fn empty() -> Model {
        Model {
            id: 0xff,
            ..Model::default()
        }
    }

    fn command_count(bus: &mut TwoPorts, port: usize, command: u8) -> usize {
        let mut n = 0;
        bus.model(port, |m| {
            n = m.cmd_log.iter().filter(|&&c| c == command).count()
        });
        n
    }

    /// One loop of a two-player game: both ports, one call each.
    fn both(pads: &mut Controllers, bus: &mut TwoPorts) -> [PadState; 2] {
        let one = *pads.poll(bus, Port::One);
        let two = *pads.poll(bus, Port::Two);
        [one, two]
    }

    #[test]
    fn the_first_contact_runs_the_analog_handshake_once_and_later_polls_only_poll() {
        let mut bus = TwoPorts::new(dualshock(button::CROSS), empty());
        let mut pads = Controllers::new();
        let one = *pads.poll(&mut bus, Port::One);
        assert!(one.is_analog(), "analog after the handshake");
        assert_eq!(one.buttons.bits(), button::CROSS);
        bus.model(0, |m| assert!(m.locked && m.id == 0x73));
        assert_eq!(command_count(&mut bus, 0, 0x44), 1);
        assert_eq!(
            command_count(&mut bus, 0, 0x4d),
            0,
            "no motor mapping for a game that does not rumble"
        );
        let polls = command_count(&mut bus, 0, 0x42);
        for _ in 0..5 {
            let one = *pads.poll(&mut bus, Port::One);
            assert_eq!(one.buttons.bits(), button::CROSS);
        }
        assert_eq!(command_count(&mut bus, 0, 0x44), 1, "no second handshake");
        assert_eq!(command_count(&mut bus, 0, 0x42), polls + 5);
        assert_eq!(pads.analog(Port::One), AnalogRequirement::Analog);
        assert_eq!(bus.selects[1], 0, "port 2 was never asked for");
    }

    #[test]
    fn an_empty_port_is_probed_once_in_sixty_polls() {
        let mut bus = TwoPorts::new(dualshock(0), empty());
        let mut pads = Controllers::new();
        for _ in 0..60 {
            let [_, two] = both(&mut pads, &mut bus);
            assert_eq!(two.mode, PadMode::Disconnected);
        }
        assert_eq!(bus.selects[1], 1, "one probe in the first 60 polls");
        both(&mut pads, &mut bus);
        assert_eq!(bus.selects[1], 2, "the 61st poll probes again");
        assert_eq!(pads.analog(Port::Two), AnalogRequirement::Absent);
    }

    #[test]
    fn a_pad_plugged_in_later_gets_the_handshake_at_the_next_probe() {
        let mut bus = TwoPorts::new(dualshock(0), empty());
        let mut pads = Controllers::new();
        both(&mut pads, &mut bus);
        bus.model(1, |m| *m = dualshock(button::START));
        for _ in 0..59 {
            assert_eq!(both(&mut pads, &mut bus)[1].mode, PadMode::Disconnected);
        }
        let [_, two] = both(&mut pads, &mut bus);
        assert!(two.is_analog() && two.buttons.bits() == button::START);
        assert_eq!(command_count(&mut bus, 1, 0x44), 1);
    }

    #[test]
    fn a_digital_pad_is_handshaken_once_and_reported_as_digital_only() {
        let mut bus = TwoPorts::new(
            Model {
                id: 0x41,
                buttons: button::LEFT,
                dualshock: false,
                ..Model::default()
            },
            empty(),
        );
        let mut pads = Controllers::new();
        for _ in 0..4 {
            assert_eq!(pads.poll(&mut bus, Port::One).buttons.bits(), button::LEFT);
        }
        assert_eq!(command_count(&mut bus, 0, 0x43), 2, "enter and exit, once");
        assert_eq!(pads.analog(Port::One), AnalogRequirement::DigitalOnly);
    }

    #[test]
    fn rumble_maps_the_motors_once_then_rides_the_next_poll() {
        let mut bus = TwoPorts::new(dualshock(0), empty());
        let mut pads = Controllers::new();
        pads.poll(&mut bus, Port::One);
        let big = Rumble {
            small: true,
            large: 0x80,
        };
        pads.set_rumble(&mut bus, Port::One, big);
        bus.model(0, |m| assert!(m.rumble_mapped));
        pads.set_rumble(&mut bus, Port::One, big);
        assert_eq!(command_count(&mut bus, 0, 0x4d), 1, "mapped once");
        pads.poll(&mut bus, Port::One);
        bus.model(0, |m| {
            let last = *m.motor_tx.last().unwrap();
            assert_eq!((last.0, last.1, last.2), (0x73, 1, 0x80));
        });
    }

    #[test]
    fn a_pad_that_goes_away_is_confirmed_absent_and_then_skipped() {
        let mut bus = TwoPorts::new(dualshock(button::CROSS), empty());
        let mut pads = Controllers::new();
        pads.poll(&mut bus, Port::One);
        bus.model(0, |m| m.id = 0xff);
        let before = bus.selects[0];
        let one = *pads.poll(&mut bus, Port::One);
        assert_eq!(one.mode, PadMode::Disconnected);
        // Four tries; with the select left open, the first is on it.
        let fresh = if SELECT_EARLY { 3 } else { 4 };
        assert_eq!(
            bus.selects[0] - before,
            fresh,
            "four tries confirm the absence"
        );
        let before = bus.selects[0];
        pads.poll(&mut bus, Port::One);
        assert_eq!(bus.selects[0], before, "then the port is skipped");
    }

    #[test]
    fn a_garbled_packet_returns_the_last_clean_state() {
        let mut bus = TwoPorts::new(dualshock(button::CIRCLE), empty());
        let mut pads = Controllers::new();
        pads.poll(&mut bus, Port::One);
        bus.model(0, |m| m.id = 0x10); // an ID no pad has: the packet fails
        let one = *pads.poll(&mut bus, Port::One);
        assert_eq!(one.buttons.bits(), button::CIRCLE);
        assert!(one.is_analog());
    }

    /// A transport that counts the bytes it sends with CPU interrupts masked and not.
    struct Watched {
        bus: TwoPorts,
        depth: usize,
        masked_bytes: usize,
        open_bytes: usize,
    }

    impl Transport for Watched {
        fn status(&mut self) -> u32 {
            self.bus.status()
        }
        fn receive(&mut self) -> u8 {
            self.bus.receive()
        }
        fn transmit(&mut self, byte: u8) {
            if self.depth == 0 {
                self.open_bytes += 1;
            } else {
                self.masked_bytes += 1;
            }
            self.bus.transmit(byte);
        }
        fn set_mode(&mut self, value: u16) {
            self.bus.set_mode(value);
        }
        fn set_baud(&mut self, value: u16) {
            self.bus.set_baud(value);
        }
        fn set_control(&mut self, value: u16) {
            self.bus.set_control(value);
        }
        fn control(&mut self) -> u16 {
            self.bus.control()
        }
        fn clock(&mut self) -> u16 {
            self.bus.clock()
        }
        fn select_with(&mut self, port: Port, arm_ack_irq: bool) {
            self.bus.select_with(port, arm_ack_irq);
        }
        fn mask_interrupts(&mut self) -> bool {
            self.depth += 1;
            true
        }
        fn restore_interrupts(&mut self, were_enabled: bool) {
            assert!(were_enabled);
            self.depth -= 1;
        }
    }

    #[test]
    fn every_configuration_byte_goes_out_with_interrupts_masked_and_no_poll_byte_does() {
        let mut bus = Watched {
            bus: TwoPorts::new(dualshock(button::CROSS), empty()),
            depth: 0,
            masked_bytes: 0,
            open_bytes: 0,
        };
        let mut pads = Controllers::new();
        assert!(pads.poll(&mut bus, Port::One).is_analog());
        // The first poll of a digital-mode DualShock is five bytes, the one after the handshake
        // nine; the rest is the three packets (the first stops acknowledging at the fifth byte).
        assert_eq!(bus.open_bytes, 5 + 9);
        let mut sends = 0;
        bus.bus.model(0, |m| sends = m.sends);
        assert_eq!(bus.masked_bytes, sends - bus.open_bytes);
        assert!(bus.masked_bytes >= 5 + 9 + 9);
        assert_eq!(bus.depth, 0, "every section is closed");
    }

    /// A DualShock whose set-analog packet (the third transaction of the session: the first
    /// poll, enter configuration, set analog) lost its `/ACK` at the third byte, as an
    /// interrupt handler running over the pulse does. The packet is abandoned before the pad
    /// saw the request, and the exit that follows leaves it digital.
    fn interrupted_in_set_analog() -> TwoPorts {
        TwoPorts::new(
            Model {
                fault: mock::Fault::Ack,
                fault_byte: 2,
                fault_attempt: 3,
                ..dualshock(button::CROSS)
            },
            empty(),
        )
    }

    #[test]
    fn a_dualshock_left_digital_by_a_lost_ack_is_asked_again_and_ends_analog() {
        let mut bus = interrupted_in_set_analog();
        let mut pads = Controllers::new();
        // The first poll runs the handshake, finds the pad still digital and runs it again.
        let first = *pads.poll(&mut bus, Port::One);
        assert!(first.is_analog() && first.buttons.bits() == button::CROSS);
        assert_eq!(command_count(&mut bus, 0, 0x44), 2);
        bus.model(0, |m| assert!(m.locked && m.id == 0x73));
        for _ in 0..200 {
            assert!(pads.poll(&mut bus, Port::One).is_analog());
        }
        assert_eq!(
            command_count(&mut bus, 0, 0x44),
            2,
            "and then it is left alone"
        );
    }

    #[test]
    fn a_dualshock_that_refuses_analog_is_asked_a_bounded_number_of_times() {
        let mut bus = TwoPorts::new(
            Model {
                refuse_analog: true,
                ..dualshock(button::LEFT)
            },
            empty(),
        );
        let mut pads = Controllers::new();
        for _ in 0..300 {
            let one = *pads.poll(&mut bus, Port::One);
            assert!(!one.is_analog() && one.buttons.bits() == button::LEFT);
        }
        assert_eq!(
            command_count(&mut bus, 0, 0x44),
            1 + usize::from(RETRIES),
            "the handshake and its retries, no more"
        );
        assert_eq!(pads.analog(Port::One), AnalogRequirement::DigitalOnly);
    }

    #[test]
    fn a_digital_pad_pays_for_the_handshake_once_and_for_nothing_after() {
        let mut bus = TwoPorts::new(
            Model {
                id: 0x41,
                buttons: button::UP,
                dualshock: false,
                ..Model::default()
            },
            empty(),
        );
        let mut pads = Controllers::new();
        for _ in 0..300 {
            assert_eq!(pads.poll(&mut bus, Port::One).buttons.bits(), button::UP);
        }
        assert_eq!(command_count(&mut bus, 0, 0x43), 2, "enter and exit, once");
        assert_eq!(command_count(&mut bus, 0, 0x44), 1, "no retry");
    }

    #[test]
    fn a_digital_only_game_never_sends_a_configuration_packet_and_polls_as_the_legacy_poll_does() {
        let mut bus = TwoPorts::new(dualshock(button::SQUARE), empty());
        let mut pads = Controllers::digital();
        for _ in 0..5 {
            let one = *pads.poll(&mut bus, Port::One);
            assert!(!one.is_analog() && one.buttons.bits() == button::SQUARE);
        }
        assert_eq!(command_count(&mut bus, 0, 0x43), 0);
        assert_eq!(command_count(&mut bus, 0, 0x44), 0);
        let (mut sends, mut log) = (0, std::vec::Vec::new());
        bus.model(0, |m| (sends, log) = (m.sends, m.cmd_log.clone()));
        assert_eq!(sends, 5 * 5, "five bytes a poll");

        // The deprecated poll of the same pad: the same bytes in the same order.
        let mut legacy = TwoPorts::new(dualshock(button::SQUARE), empty());
        for _ in 0..5 {
            assert_eq!(
                crate::poll_on(&mut legacy, Port::One).buttons.bits(),
                button::SQUARE
            );
        }
        legacy.model(0, |m| {
            assert_eq!(m.sends, sends);
            assert_eq!(m.cmd_log, log);
        });
    }

    /// Selects made on each port by the polls in `f`, and how many of them were
    /// left open for the next poll.
    fn selects(bus: &mut TwoPorts, f: impl FnOnce(&mut TwoPorts)) -> [usize; 2] {
        let before = bus.selects;
        f(bus);
        [bus.selects[0] - before[0], bus.selects[1] - before[1]]
    }

    #[test]
    fn a_poll_leaves_its_own_select_open_and_the_next_one_starts_from_it() {
        if !SELECT_EARLY {
            return;
        }
        let mut bus = TwoPorts::new(dualshock(button::CROSS), empty());
        let mut pads = Controllers::new();
        pads.poll(&mut bus, Port::One);
        assert_eq!(pads.open.map(|(port, _)| port), Some(Port::One));
        for _ in 0..5 {
            // One select a poll: the one it leaves open. The bytes go out on the
            // select the poll before left.
            let n = selects(&mut bus, |bus| {
                assert_eq!(pads.poll(bus, Port::One).buttons.bits(), button::CROSS);
            });
            assert_eq!(n, [1, 0]);
            bus.model(0, |m| assert_eq!(m.early, 0));
        }
        assert_eq!(bus.selects[1], 0, "port 2 is never selected");
    }

    #[test]
    fn the_setup_time_is_waited_from_the_open_select_however_soon_the_next_poll_comes() {
        if !SELECT_EARLY {
            return;
        }
        let mut bus = TwoPorts::new(dualshock(0), empty());
        let mut pads = Controllers::new();
        pads.poll(&mut bus, Port::One);
        // The next poll at once: it takes the whole setup time on the clock, where
        // its bytes alone take a few hundred model ticks.
        let mut before = 0;
        bus.model(0, |m| before = m.now);
        pads.poll(&mut bus, Port::One);
        let mut after = 0;
        bus.model(0, |m| after = m.now);
        assert!(after - before >= u64::from(psx_io::controller_port::PAD_SETUP_CYCLES));
    }

    #[test]
    fn a_select_released_in_between_is_made_again() {
        if !SELECT_EARLY {
            return;
        }
        let mut bus = TwoPorts::new(dualshock(button::SQUARE), empty());
        let mut pads = Controllers::new();
        pads.poll(&mut bus, Port::One);
        // A memory-card transfer on the same port ends with a deselect.
        bus.set_control(0);
        let n = selects(&mut bus, |bus| {
            assert_eq!(pads.poll(bus, Port::One).buttons.bits(), button::SQUARE);
        });
        assert_eq!(n, [2, 0], "a fresh select for the bytes, then the open one");
    }

    #[test]
    fn a_two_player_loop_opens_the_other_port() {
        if !SELECT_EARLY {
            return;
        }
        let mut bus = TwoPorts::new(dualshock(button::CROSS), dualshock(button::CIRCLE));
        let mut pads = Controllers::new();
        both(&mut pads, &mut bus);
        both(&mut pads, &mut bus);
        for _ in 0..4 {
            let n = selects(&mut bus, |bus| {
                assert_eq!(pads.poll(bus, Port::One).buttons.bits(), button::CROSS);
            });
            assert_eq!(n, [0, 1], "port 1 found its select open and opened port 2");
            let n = selects(&mut bus, |bus| {
                assert_eq!(pads.poll(bus, Port::Two).buttons.bits(), button::CIRCLE);
            });
            assert_eq!(n, [1, 0], "port 2 found its select open and opened port 1");
        }
    }

    #[test]
    fn no_select_is_left_open_on_an_empty_port_and_its_skipped_polls_keep_the_open_one() {
        if !SELECT_EARLY {
            return;
        }
        let mut bus = TwoPorts::new(empty(), empty());
        let mut pads = Controllers::new();
        pads.poll(&mut bus, Port::One);
        assert!(pads.open.is_none());

        let mut bus = TwoPorts::new(dualshock(0), empty());
        let mut pads = Controllers::new();
        both(&mut pads, &mut bus);
        both(&mut pads, &mut bus);
        for _ in 0..10 {
            // Port 2 is skipped: port 1 keeps its select from one loop to the next.
            let n = selects(&mut bus, |bus| {
                both(&mut pads, bus);
            });
            assert_eq!(n, [1, 0]);
        }
    }
}
