// SPDX-License-Identifier: GPL-2.0-or-later
//! [`Controllers`]: the one way a game reads its pads.
//!
//! Every game runs this implementation and puts the same bytes on the wire:
//!
//! * `poll(bus, port)` reads one port. A game reads port 1 with one call per loop; a
//!   two-player game makes a second call for port 2. A port nobody asks for costs nothing.
//! * A poll selects its port when it starts, waits the pad's setup time ([`PAD_SETUP_CYCLES`], on the
//!   system clock) and releases `/CS` when it ends, so the pad has `/CS` high from one poll to the
//!   next, as the SDK did before the held select. On the console an SCPH-110 did not answer
//!   polls whose select followed the last deselect by a few hundred cycles; with `/CS` high for the
//!   rest of the frame it did, so there is no option to leave the select open. A retry, and the
//!   handshake after a poll, wait [`PAD_CS_HIGH_CYCLES`] with `/CS` high first.
//! * The first time a port answers, the same handshake runs on it: analog mode requested
//!   and locked, a video frame between packets, as Sony's libpad does. It blocks for a few
//!   frames, once per plug-in. Each packet goes out with CPU interrupts masked: a pad's
//!   `/ACK` is a pulse of about 90 cycles, a handler that runs over it hides it, and a
//!   packet abandoned part way is not retried and its answer not read. A pad that answered
//!   the handshake like a DualShock but still reports digital is asked again, [`RETRIES`]
//!   times at most, each a handshake of its own (a few video frames) after the last. A
//!   digital pad, which answers no configuration packet in full, is never asked again.
//! * A port that did not answer is polled every time for its first [`EAGER_PROBES`] such answers
//!   (a pad that is slow to come up is read from its first answer), then not polled again for 59
//!   polls and probed once: an empty port 2 costs one probe a second, not one every frame. A pad that was there has
//!   its absence confirmed by four tries before it is believed.
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
    pad_begin, pad_delay, pad_exchange, poll_state_first, AnalogRequirement, PadMode, PadState,
    Port, Rumble, MOTOR_MAP_PACKET, REQUIRE_ANALOG_GAP_SPINS,
};
use psx_io::controller_port::{Timing, Transport};

/// Polls an absent port is left alone between probes: one probe in 60.
const ABSENT_SKIP: u8 = 59;

/// Answers of "nobody there" a port gives, from boot or from the moment a pad left it, before it
/// is probed only once in 60 polls: until then it is polled every time, so a pad that is slow
/// to come up is read from its first answer. PCSX-ReARMed's DualShock acknowledges the address
/// byte and answers 0xFF to the command byte, which is what an empty socket does on that port,
/// until about 33 video frames have gone by (31 to 33 whether a game polls once, twice or four
/// times a frame: the silence is time, not polls); a probe every 60 polls made it ignore the pad
/// for 120 polls. The window covers a game that polls twice a frame with a margin of two. An
/// empty port pays this many failed polls once (about 20,000 cycles each in the emulator's model
/// of an empty socket).
const EAGER_PROBES: u8 = 128;

/// Packets sent again to a pad that answered the handshake like a DualShock and is not analog
/// after it, at most for a plug-in: the whole handshake to one that reports digital, the exit
/// alone to one parked in configuration mode. Each is a few video frames after the last,
/// because the handshake spaces its packets so.
pub const RETRIES: u8 = 3;

/// Polls between the asks that go on for a pad that answered the handshake like a DualShock and
/// still is not analog after its [`RETRIES`] are spent: the pad of an SCPH-110 left in
/// configuration mode, one that lost a packet to an interrupt late, or one a previous program
/// left digital. A program that wants analog (the disc launcher, which held its menu on a
/// notice and asked every 60 frames before this was shared) must not be left digital for the
/// rest of the session because three asks happened to fail. Each ask costs the handshake's few
/// frames, so it is slow, and it is made only for a pad that proved it takes configuration
/// packets: a digital-only pad is never asked.
pub const RECHECK_POLLS: u8 = 60;

/// Consecutive polls that came back unusable (`PadMode::Unknown`) after which the port stops
/// presenting its last clean state as the current one: the state becomes `Unknown` and the
/// next clean poll runs the handshake again. Until then a garbled poll keeps the last clean
/// state, which stops one bad frame from reading as every button released; past this many it
/// is a dead link, and a menu that kept reading the last state would show a pad that answers
/// nothing as analog and idle.
pub const STALE_POLLS: u8 = 8;

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
    /// "Nobody there" answers since boot or since a pad was last present, up to
    /// [`EAGER_PROBES`].
    unanswered: u8,
    /// A pad answered the last probe; the handshake has run on it.
    present: bool,
    /// The motors were mapped on the pad now in the port.
    mapped: bool,
    /// What the motors are asked for on every poll.
    rumble: Rumble,
    /// Retries left for the pad in the port, [`RETRIES`] if it answered the handshake like a
    /// DualShock.
    again: u8,
    /// The pad answered the handshake's packets in full, so it takes configuration commands.
    capable: bool,
    /// Polls until a pad that is still not analog is asked again, after [`RETRIES`] are spent.
    recheck: u8,
    /// Consecutive unusable polls, up to [`STALE_POLLS`].
    failed: u8,
    #[cfg(feature = "counters")]
    counts: PollCounters,
}

impl Slot {
    const NEW: Self = Self {
        skip: 0,
        unanswered: 0,
        present: false,
        mapped: false,
        rumble: Rumble::OFF,
        again: 0,
        capable: false,
        recheck: 0,
        failed: 0,
        #[cfg(feature = "counters")]
        counts: PollCounters {
            polls: 0,
            failed: 0,
            first_try_failed: 0,
            handshakes: 0,
        },
    };

    /// One poll of `socket`; `last` is its latest clean state and is updated. Inlined into [`Controllers::step`].
    #[inline(always)]
    fn step<T: Transport, const ANALOG: bool>(
        &mut self,
        bus: &mut T,
        socket: Port,
        last: &mut PadState,
    ) {
        let mut confirm = self.present;
        // One call site of the poll: the poll itself, the poll after the handshake,
        // and the poll after an exit sent to a pad parked in configuration mode (the
        // SCPH-110 failure, retried a bounded number of times).
        let mut first_clean = true;
        let mut tried = false;
        let state = loop {
            let (state, clean) = poll_state_first(
                bus,
                socket,
                confirm,
                if ANALOG { self.rumble } else { Rumble::OFF },
            );
            if !tried {
                tried = true;
                first_clean = clean;
            }
            if !ANALOG || (state.mode == PadMode::Analog && self.present) {
                break state;
            }
            if !self.settle(bus, socket, state.mode) {
                break state;
            }
            confirm = true;
        };
        #[cfg(feature = "counters")]
        {
            let c = &mut self.counts;
            c.polls = c.polls.saturating_add(1);
            if !first_clean {
                c.first_try_failed = c.first_try_failed.saturating_add(1);
            }
            if matches!(state.mode, PadMode::Unknown | PadMode::Disconnected) {
                c.failed = c.failed.saturating_add(1);
            }
        }
        let _ = first_clean;
        match state.mode {
            // A garbled packet: the last clean state stands, for a few polls. A link that
            // stays dead is reported as such and the handshake runs again when it recovers.
            PadMode::Unknown => {
                self.failed = self.failed.saturating_add(1);
                if self.failed >= STALE_POLLS {
                    self.failed = STALE_POLLS;
                    self.present = false;
                    self.mapped = false;
                    *last = state;
                }
            }
            PadMode::Disconnected => {
                if self.present {
                    self.unanswered = 0;
                }
                self.present = false;
                self.mapped = false;
                self.capable = false;
                self.recheck = 0;
                self.failed = 0;
                self.skip = if self.unanswered < EAGER_PROBES {
                    self.unanswered += 1;
                    0
                } else {
                    ABSENT_SKIP
                };
                *last = state;
            }
            _ => {
                self.present = true;
                self.failed = 0;
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
        } else if self.capable
            && self.recheck == 0
            && matches!(mode, PadMode::Config | PadMode::Digital)
        {
            // The retries are spent and the pad still is not analog: ask once more, then
            // again every RECHECK_POLLS polls for as long as it stays so.
            self.recheck = RECHECK_POLLS;
            if mode == PadMode::Config {
                2
            } else {
                0
            }
        } else {
            self.recheck = self.recheck.saturating_sub(1);
            return false;
        };
        #[cfg(feature = "counters")]
        {
            self.counts.handshakes = self.counts.handshakes.saturating_add(1);
        }
        let capable = config(bus, socket, Sequence::Analog, first);
        if fresh {
            self.capable = capable;
            self.recheck = RECHECK_POLLS;
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
    // The first packet follows a poll's deselect at once.
    bus.cs_high_gap();
    while n < end {
        let index = match n {
            0 => 0,
            1 if sequence == Sequence::Analog => 1,
            1 => 2,
            _ => 3,
        };
        let packet = packet(index);
        ok = pad_begin(bus, socket);
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

/// What one port's driver counted since it was made (feature `counters`, for test instruments:
/// nothing here touches the wire, and a game build leaves the feature off).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct PollCounters {
    /// Calls of `poll` that went to the wire.
    pub polls: u16,
    /// Polls whose result was a pad that did not answer or a garbled packet, every retry spent.
    pub failed: u16,
    /// Polls whose first try was not clean (the retry may have been).
    pub first_try_failed: u16,
    /// Configuration sequences started: the first handshake, its retries and the rechecks.
    pub handshakes: u16,
}

/// The controller ports, polled the one way every game polls them. See the module
/// documentation.
#[derive(Copy, Clone)]
pub struct Controllers<const ANALOG: bool = true> {
    slots: [Slot; 2],
    /// The latest clean state of each port.
    last: [PadState; 2],
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
            // An empty port between probes: nothing goes on the wire.
            slot.skip -= 1;
            return;
        }
        slot.step::<T, ANALOG>(bus, port, &mut self.last[i]);
    }

    /// What `port`'s driver counted (feature `counters`).
    #[cfg(feature = "counters")]
    #[must_use]
    pub fn counters(&self, port: Port) -> PollCounters {
        self.slots[usize::from(port.is_two())].counts
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
    fn an_empty_port_is_polled_every_time_at_first_and_then_probed_once_in_sixty_polls() {
        let mut bus = TwoPorts::new(dualshock(0), empty());
        let mut pads = Controllers::new();
        for n in 1..=usize::from(EAGER_PROBES) + 1 {
            let [_, two] = both(&mut pads, &mut bus);
            assert_eq!(two.mode, PadMode::Disconnected);
            assert_eq!(bus.selects[1], n, "every early poll probes");
        }
        let probed = bus.selects[1];
        for _ in 0..59 {
            both(&mut pads, &mut bus);
        }
        assert_eq!(bus.selects[1], probed, "then 59 polls leave the port alone");
        both(&mut pads, &mut bus);
        assert_eq!(bus.selects[1], probed + 1, "and the 60th probes again");
        assert_eq!(pads.analog(Port::Two), AnalogRequirement::Absent);
    }

    #[test]
    fn a_pad_plugged_in_later_gets_the_handshake_at_the_next_poll() {
        let mut bus = TwoPorts::new(dualshock(0), empty());
        let mut pads = Controllers::new();
        both(&mut pads, &mut bus);
        bus.model(1, |m| *m = dualshock(button::START));
        let [_, two] = both(&mut pads, &mut bus);
        assert!(two.is_analog() && two.buttons.bits() == button::START);
        assert_eq!(command_count(&mut bus, 1, 0x44), 1);
    }

    #[test]
    fn a_pad_plugged_in_after_the_early_probes_waits_for_the_next_probe() {
        let mut bus = TwoPorts::new(dualshock(0), empty());
        let mut pads = Controllers::new();
        for _ in 0..=EAGER_PROBES {
            both(&mut pads, &mut bus);
        }
        bus.model(1, |m| *m = dualshock(button::START));
        for _ in 0..59 {
            assert_eq!(both(&mut pads, &mut bus)[1].mode, PadMode::Disconnected);
        }
        let [_, two] = both(&mut pads, &mut bus);
        assert!(two.is_analog() && two.buttons.bits() == button::START);
        assert_eq!(command_count(&mut bus, 1, 0x44), 1);
    }

    /// PCSX-ReARMed's DualShock, as the wire log of the first polls records it: the address byte
    /// is acknowledged, the command byte is answered 0xFF and not acknowledged (an empty socket
    /// on that port looks the same) until 33 video frames have gone by, then the pad answers as a
    /// digital pad. The test sets the ID by the poll the game asks for, whether or not the
    /// driver puts it on the wire, as the pad's readiness depends on time and not on polls.
    fn slow_to_answer(buttons: u16) -> Model {
        Model {
            id: 0xff,
            dualshock: true,
            buttons,
            latch_only: true,
            latch_free_bytes: 2,
            empty_latches: true,
            ..Model::default()
        }
    }

    #[test]
    fn a_pad_slow_to_answer_is_read_from_its_first_answer_at_one_and_two_polls_a_frame() {
        for per_frame in [1, 2] {
            for digital in [true, false] {
                let ready = 33 * per_frame;
                let mut bus = TwoPorts::new(slow_to_answer(button::CROSS), empty());
                let mut digital_pads = Controllers::digital();
                let mut pads = Controllers::new();
                let mut first = None;
                for n in 0..400 {
                    bus.model(0, |m| m.id = if n < ready { 0xff } else { 0x41 });
                    let state = if digital {
                        *digital_pads.poll(&mut bus, Port::One)
                    } else {
                        *pads.poll(&mut bus, Port::One)
                    };
                    if state.buttons.bits() == button::CROSS {
                        first = Some(n);
                        break;
                    }
                }
                assert_eq!(
                    first,
                    Some(ready),
                    "{per_frame} polls a frame, digital {digital}: read at the first answer"
                );
            }
        }
    }

    #[test]
    fn a_pad_that_goes_away_is_confirmed_absent_and_then_polled_early_and_skipped_late() {
        let mut bus = TwoPorts::new(dualshock(button::CROSS), empty());
        let mut pads = Controllers::new();
        pads.poll(&mut bus, Port::One);
        bus.model(0, |m| m.id = 0xff);
        let before = bus.selects[0];
        let one = *pads.poll(&mut bus, Port::One);
        assert_eq!(one.mode, PadMode::Disconnected);
        // Four tries, each its own select.
        assert_eq!(bus.selects[0] - before, 4, "four tries confirm the absence");
        for _ in 0..EAGER_PROBES {
            let before = bus.selects[0];
            pads.poll(&mut bus, Port::One);
            assert_eq!(bus.selects[0], before + 1, "then every poll probes once");
        }
        let before = bus.selects[0];
        pads.poll(&mut bus, Port::One);
        assert_eq!(bus.selects[0], before, "then the port is skipped");
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
        let polls = 300;
        for _ in 0..polls {
            let one = *pads.poll(&mut bus, Port::One);
            assert!(!one.is_analog() && one.buttons.bits() == button::LEFT);
        }
        // The handshake and its retries, then one more ask every RECHECK_POLLS polls: slow
        // enough that a pad which will never do it costs a few frames a second at most.
        let asks = command_count(&mut bus, 0, 0x44);
        let rechecks = usize::from(polls / u16::from(RECHECK_POLLS));
        assert!(
            asks > 1 + usize::from(RETRIES) && asks <= 1 + usize::from(RETRIES) + rechecks,
            "{asks} asks"
        );
        assert_eq!(pads.analog(Port::One), AnalogRequirement::DigitalOnly);
    }

    #[test]
    fn a_dualshock_that_comes_round_late_is_found_analog_by_the_recheck() {
        let mut bus = TwoPorts::new(
            Model {
                refuse_analog: true,
                ..dualshock(button::CROSS)
            },
            empty(),
        );
        let mut pads = Controllers::new();
        for _ in 0..100 {
            assert!(!pads.poll(&mut bus, Port::One).is_analog());
        }
        bus.model(0, |m| m.refuse_analog = false);
        let mut polls = 0;
        while !pads.poll(&mut bus, Port::One).is_analog() {
            polls += 1;
            assert!(polls <= 2 * u16::from(RECHECK_POLLS), "never found analog");
        }
        assert_eq!(pads.analog(Port::One), AnalogRequirement::Analog);
        bus.model(0, |m| assert!(m.locked && m.id == 0x73));
    }

    /// A pad that ignores the first `exits` exit-config packets stays in configuration mode
    /// (ID 0xF3: buttons and sticks on the wire, not analog), the way an SCPH-110 did when its
    /// commands came too close. Returns the state after `polls` polls.
    fn parked(exits: u32, polls: usize) -> (PadState, usize) {
        let mut bus = TwoPorts::new(
            Model {
                ignore_exits: exits,
                axes: [0x80, 0x80, 0xff, 0x80],
                ..dualshock(button::CROSS)
            },
            empty(),
        );
        let mut pads = Controllers::new();
        let mut state = PadState::NONE;
        for _ in 0..polls {
            state = *pads.poll(&mut bus, Port::One);
        }
        (state, command_count(&mut bus, 0, 0x43))
    }

    #[test]
    fn a_pad_that_ignores_the_exit_more_often_than_the_retries_is_found_analog_by_the_recheck() {
        // The handshake's exit and its RETRIES repeats are all ignored: the pad is parked.
        let (state, _) = parked(1 + u32::from(RETRIES), 20);
        assert_eq!(
            state.mode,
            PadMode::Config,
            "parked after the retries are spent"
        );
        assert_eq!(state.buttons.bits(), button::CROSS, "buttons still read");
        assert!(
            !state.is_analog(),
            "a game that tests is_analog ignores the sticks"
        );
        // Left alone long enough, the recheck sends the exit again and the pad leaves it.
        let (state, _) = parked(1 + u32::from(RETRIES), 200);
        assert!(state.is_analog() && state.buttons.bits() == button::CROSS);
    }

    #[test]
    fn a_digital_only_pad_is_never_rechecked() {
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
            pads.poll(&mut bus, Port::One);
        }
        assert_eq!(command_count(&mut bus, 0, 0x43), 2, "enter and exit, once");
    }

    #[test]
    fn a_link_that_stays_dead_is_not_presented_as_the_last_clean_state() {
        let mut bus = TwoPorts::new(dualshock(button::CROSS), empty());
        let mut pads = Controllers::new();
        assert!(pads.poll(&mut bus, Port::One).is_analog());
        // A pad whose /ACK after the fourth byte comes far later than any wait: every poll
        // times out part way, as a dead link does.
        for _ in 1..STALE_POLLS {
            bus.model(0, |m| m.slow_ack = Some((3, 1_000_000)));
            let one = *pads.poll(&mut bus, Port::One);
            assert!(
                one.is_analog() && one.buttons.bits() == button::CROSS,
                "kept for a while"
            );
        }
        bus.model(0, |m| m.slow_ack = Some((3, 1_000_000)));
        let one = *pads.poll(&mut bus, Port::One);
        assert_eq!(one.mode, PadMode::Unknown, "then reported as what it is");
        assert_eq!(pads.analog(Port::One), AnalogRequirement::Absent);
        assert_eq!(one.buttons.bits(), 0, "and no button is held over");
        // The link comes back: the handshake runs again and the pad reads analog.
        bus.model(0, |m| m.slow_ack = None);
        let handshakes = command_count(&mut bus, 0, 0x44);
        assert!(pads.poll(&mut bus, Port::One).is_analog());
        assert_eq!(command_count(&mut bus, 0, 0x44), handshakes + 1);
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

    /// Selects made on each port by the polls in `f`.
    fn selects(bus: &mut TwoPorts, f: impl FnOnce(&mut TwoPorts)) -> [usize; 2] {
        let before = bus.selects;
        f(bus);
        [bus.selects[0] - before[0], bus.selects[1] - before[1]]
    }

    #[cfg(feature = "counters")]
    #[test]
    fn the_counters_count_polls_handshakes_and_failures_without_touching_the_wire() {
        let mut bus = TwoPorts::new(dualshock(button::CROSS), empty());
        let mut pads = Controllers::new();
        for _ in 0..5 {
            pads.poll(&mut bus, Port::One);
        }
        let c = pads.counters(Port::One);
        assert_eq!((c.polls, c.failed, c.handshakes), (5, 0, 1));
        assert_eq!(c.first_try_failed, 0);
        // A pad whose /ACK after the fourth byte never comes in time: every poll fails.
        for _ in 0..3 {
            bus.model(0, |m| m.slow_ack = Some((3, 1_000_000)));
            pads.poll(&mut bus, Port::One);
        }
        let c = pads.counters(Port::One);
        assert_eq!((c.polls, c.failed, c.first_try_failed), (8, 3, 3));
    }

    #[test]
    fn a_poll_selects_its_own_port_at_its_start_and_ends_deselected() {
        let mut bus = TwoPorts::new(dualshock(button::CROSS), dualshock(button::CIRCLE));
        let mut pads = Controllers::new();
        both(&mut pads, &mut bus);
        for _ in 0..4 {
            let n = selects(&mut bus, |bus| {
                assert_eq!(pads.poll(bus, Port::One).buttons.bits(), button::CROSS);
            });
            assert_eq!(n, [1, 0], "one select, on port 1");
            bus.model(0, |m| assert!(!m.is_selected(), "released at the end"));
            let n = selects(&mut bus, |bus| {
                assert_eq!(pads.poll(bus, Port::Two).buttons.bits(), button::CIRCLE);
            });
            assert_eq!(n, [0, 1], "one select, on port 2");
        }
    }

    /// A transport that leaves out the wait `/CS` must stay high, the bare timing of the code.
    struct NoGaps(TwoPorts);

    impl Transport for NoGaps {
        fn status(&mut self) -> u32 {
            self.0.status()
        }
        fn receive(&mut self) -> u8 {
            self.0.receive()
        }
        fn transmit(&mut self, byte: u8) {
            self.0.transmit(byte);
        }
        fn set_mode(&mut self, value: u16) {
            self.0.set_mode(value);
        }
        fn set_baud(&mut self, value: u16) {
            self.0.set_baud(value);
        }
        fn set_control(&mut self, value: u16) {
            self.0.set_control(value);
        }
        fn control(&mut self) -> u16 {
            self.0.control()
        }
        fn clock(&mut self) -> u16 {
            self.0.clock()
        }
        fn select_with(&mut self, port: Port, arm_ack_irq: bool) {
            self.0.select_with(port, arm_ack_irq);
        }
        fn cs_high_gap(&mut self) {}
    }

    /// A frame of game code between two polls, in model ticks.
    const FRAME: u64 = 560_000;

    #[test]
    fn the_pad_gets_cs_high_between_any_two_transactions_and_its_setup_time() {
        let mut bus = TwoPorts::new(dualshock(button::CROSS), empty());
        let mut pads = Controllers::new();
        pads.poll(&mut bus, Port::One);
        for _ in 0..4 {
            bus.model(0, |m| m.now += FRAME);
            pads.poll(&mut bus, Port::One);
        }
        bus.model(0, |m| {
            let setup = u64::from(psx_io::controller_port::PAD_SETUP_CYCLES);
            assert!(m.min_setup_gap >= setup, "{}", m.min_setup_gap);
            // The handshake's first packet and every retry follow a deselect at once, too.
            let high = u64::from(psx_io::controller_port::PAD_CS_HIGH_CYCLES);
            let shortest = m.cs_high.iter().copied().min().unwrap();
            assert!(shortest >= high, "{shortest}");
        });
    }

    #[test]
    fn without_the_wait_the_handshake_follows_the_poll_within_a_few_ticks() {
        let mut bus = NoGaps(TwoPorts::new(dualshock(button::CROSS), empty()));
        let mut pads = Controllers::new();
        pads.poll(&mut bus, Port::One);
        bus.0.model(0, |m| {
            let high = u64::from(psx_io::controller_port::PAD_CS_HIGH_CYCLES);
            let shortest = m.cs_high.iter().copied().min().unwrap();
            assert!(shortest < high, "{shortest}");
        });
    }
}
