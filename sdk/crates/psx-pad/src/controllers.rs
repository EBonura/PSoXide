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
//!   frames, once per plug-in.
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
//! ```text
//! let mut pads = psx_pad::Controllers::new();           // a static, or a field
//! let one = *pads.poll(&mut peripherals.controller_port, Port::One);
//! ```
//!
//! [`PAD_SETUP_CYCLES`]: psx_io::controller_port::PAD_SETUP_CYCLES

use crate::{
    pad_begin, pad_delay, pad_exchange, poll_state, AnalogRequirement, PadMode, PadState, Port,
    Rumble, CONFIG_EXIT_RETRIES, MOTOR_MAP_PACKET, REQUIRE_ANALOG_GAP_SPINS,
};
use psx_io::controller_port::{Timing, Transport};

/// Polls an absent port is left alone between probes: one probe in 60.
const ABSENT_SKIP: u8 = 59;

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
}

impl Slot {
    const NEW: Self = Self {
        skip: 0,
        present: false,
        mapped: false,
        rumble: Rumble::OFF,
    };

    /// One poll of `socket`, starting from the select `opened` left open if there is one;
    /// `last` is its latest clean state and is updated. Inlined into [`Controllers::step`].
    #[inline(always)]
    fn step<T: Transport>(
        &mut self,
        bus: &mut T,
        socket: Port,
        mut opened: Option<u16>,
        last: &mut PadState,
    ) {
        let mut confirm = self.present;
        let mut retries = 0;
        // One call site of the poll: the poll itself, the poll after the handshake,
        // and the poll after an exit sent to a pad parked in configuration mode (the
        // SCPH-110 failure, retried a bounded number of times).
        let state = loop {
            let state = poll_state(bus, socket, confirm, self.rumble, opened.take());
            if state.mode.is_connected() && !self.present {
                self.present = true;
                config(bus, socket, Sequence::Analog, 0);
            } else if state.mode == PadMode::Config && retries < CONFIG_EXIT_RETRIES {
                retries += 1;
                config(bus, socket, Sequence::Analog, 2);
            } else {
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

/// The packets of `sequence` from the `first`-th on, each a video frame after the last: the
/// address byte, then the eight command bytes, each waiting for the pad's `/ACK` like a poll
/// does. The answers are not read: the poll that follows says what the pad settled on. From
/// the last one only, it is the exit sent again to a pad parked in configuration mode.
#[inline(never)]
fn config<T: Transport>(bus: &mut T, socket: Port, sequence: Sequence, first: usize) {
    // `black_box` keeps LLVM from unrolling the packets into copies.
    let end = core::hint::black_box(3);
    let mut n = first;
    while n < end {
        let index = match n {
            0 => 0,
            1 if sequence == Sequence::Analog => 1,
            1 => 2,
            _ => 3,
        };
        let packet = packet(index);
        let mut ok = pad_begin(bus, socket, None);
        let mut i = 0;
        while ok && i <= packet.len() {
            let tx = if i == 0 { 0x01 } else { packet[i - 1] };
            ok = pad_exchange(bus, socket, tx, i == packet.len(), Timing::PAD).is_ok();
            i += 1;
        }
        bus.finish(ok);
        pad_delay(bus, REQUIRE_ANALOG_GAP_SPINS);
        n += 1;
    }
}

/// The controller ports, polled the one way every game polls them. See the module
/// documentation.
#[derive(Copy, Clone)]
pub struct Controllers {
    slots: [Slot; 2],
    /// The latest clean state of each port.
    last: [PadState; 2],
    /// The select the last poll left open: its port and when it was asserted.
    open: Option<(Port, u16)>,
    /// The port polled before the last one.
    previous: Port,
}

impl Controllers {
    /// Two ports that have seen nothing yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: [Slot::NEW; 2],
            last: [PadState::NONE; 2],
            open: None,
            previous: Port::One,
        }
    }

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
        let opened = match self.open.take() {
            Some((open, at)) if open == port => Some(at),
            _ => None,
        };
        slot.step(bus, port, opened, &mut self.last[i]);
        // Open the port polled before this one if a pad is in it, else this one.
        let mut next = core::mem::replace(&mut self.previous, port);
        if !self.slots[usize::from(next.is_two())].present {
            next = port;
        }
        if SELECT_EARLY && self.slots[usize::from(next.is_two())].present {
            self.open = Some((next, bus.open(next)));
        }
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

impl Default for Controllers {
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
