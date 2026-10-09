// SPDX-License-Identifier: GPL-2.0-or-later
//! Digital / DualShock pad polling via SIO0.
//!
//! Talks directly to the SIO0 hardware (no BIOS syscalls) so the
//! same code works whether we side-load a homebrew (HLE BIOS) or
//! boot through the real BIOS. The controller protocol is simple
//! enough that a hand-rolled select + four-byte exchange beats
//! opening events and waiting on them.
//!
//! Typical use from a game loop. The controller port has one owner, the
//! `ControllerPort` token from `psx_rt::Peripherals::take()`, which every pad
//! poll and memory-card transfer borrows for the length of its transaction:
//!
//! ```text
//! let mut port = peripherals.controller_port;
//! let mut reader = psx_pad::PadReader::port1();
//! let pad = reader.poll_on(&mut port);
//! if pad.buttons.is_held(psx_pad::button::START) {
//!     // …
//! }
//! ```
//!
//! The protocol spec, reproduced from nocash PSX-SPX:
//!
//! | `TX` | `RX` | Meaning                            |
//! |------|------|------------------------------------|
//! | `01` | `FF` | Address byte / select controller   |
//! | `42` | `41` | Poll command / digital pad ID low  |
//! | `00` | `5A` | Fill byte / ID high                |
//! | `00` | `b0` | Buttons group 1 (active-low)       |
//! | `00` | `b1` | Buttons group 2 (active-low)       |
//!
//! DualShock analog mode uses the same first four bytes but reports
//! ID low `0x73` and appends four stick bytes:
//! right X/Y, then left X/Y. Fresh DualShocks boot digital, so games
//! that require sticks should either call [`enable_analog_on`] or
//! show an "enable analog mode" prompt when [`PadState::is_analog`]
//! is false. Programs that require a DualShock call
//! [`require_analog_on`] at boot: it locks the pad in analog mode, so
//! the Analog button cannot switch it back, and says whether an
//! analog-capable pad is there at all (`sdk/docs/PAD-ANALOG.md`).
//!
//! Every poll is a complete, ACK-paced packet whose length follows the ID it
//! reports, so a 0x41 digital frame and a 0x73 analog frame decode the same
//! buttons. A packet that fails part way reports [`PadMode::Unknown`]; read
//! through a [`PadReader`] to get the last clean state instead.
//!
//! Our [`ButtonState`] stores active-high so `buttons.is_held` feels
//! natural in game code.

#![no_std]
#![cfg_attr(
    all(feature = "irq-engine", target_arch = "mips"),
    feature(asm_experimental_arch)
)]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

use psx_hw::sio::sio0;
use psx_io::controller_port::{ExchangeError, Timing, Transport};
use psx_io::periph::ControllerPort;

pub use psx_io::controller_port::Port;

#[cfg(all(feature = "irq-engine", target_arch = "mips"))]
pub mod console;
pub mod engine;
pub mod tracker;
pub use tracker::PadTracker;

// Under test the driver talks to a controller model, not the registers.
#[cfg(test)]
mod engine_tests;
#[cfg(test)]
mod mock_sio;
#[cfg(test)]
mod transport_tests;

/// Named button bitmasks (active-high in this representation).
/// Hardware's active-low wire format is hidden inside [`poll_port1`].
pub mod button {
    /// SELECT.
    pub const SELECT: u16 = 1 << 0;
    /// Left stick click (DualShock L3).
    pub const L3: u16 = 1 << 1;
    /// Right stick click (DualShock R3).
    pub const R3: u16 = 1 << 2;
    /// START.
    pub const START: u16 = 1 << 3;
    /// D-pad up.
    pub const UP: u16 = 1 << 4;
    /// D-pad right.
    pub const RIGHT: u16 = 1 << 5;
    /// D-pad down.
    pub const DOWN: u16 = 1 << 6;
    /// D-pad left.
    pub const LEFT: u16 = 1 << 7;
    /// L2 shoulder.
    pub const L2: u16 = 1 << 8;
    /// R2 shoulder.
    pub const R2: u16 = 1 << 9;
    /// L1 shoulder.
    pub const L1: u16 = 1 << 10;
    /// R1 shoulder.
    pub const R1: u16 = 1 << 11;
    /// Triangle face button.
    pub const TRIANGLE: u16 = 1 << 12;
    /// Circle face button.
    pub const CIRCLE: u16 = 1 << 13;
    /// Cross (X) face button.
    pub const CROSS: u16 = 1 << 14;
    /// Square face button.
    pub const SQUARE: u16 = 1 << 15;
}

/// Result of one pad poll. `bits()` gives the raw active-high mask;
/// [`ButtonState::is_held`] is the ergonomic per-button check.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ButtonState(u16);

impl ButtonState {
    /// Empty -- nothing held.
    pub const NONE: Self = Self(0);

    /// Construct from an active-high mask.
    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    /// Raw bitmask.
    #[inline]
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// True when any masked button is down now and none was down previously.
    /// For multi-button masks this is a group edge, not a per-button edge.
    #[inline]
    pub const fn pressed_since(self, previous: Self, mask: u16) -> bool {
        self.is_held(mask) && !previous.is_held(mask)
    }

    /// `true` when `mask` (any single-bit [`button`] constant or an
    /// OR of several) is currently pressed.
    #[inline]
    pub const fn is_held(self, mask: u16) -> bool {
        self.0 & mask != 0
    }
}

/// Default analog-stick reading. `0x80` = centred, `0x00` = full
/// negative, `0xFF` = full positive.
pub const STICK_CENTER: u8 = 0x80;

/// Controller operating mode inferred from the poll ID byte.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PadMode {
    /// No controller answered the poll.
    Disconnected,
    /// SCPH-1080 digital-pad shape: ID `0x41`, buttons only.
    Digital,
    /// DualShock analog shape: ID `0x73`, buttons plus stick bytes.
    Analog,
    /// DualShock config/escape shape: ID `0xF3`.
    Config,
    /// A controller answered with an ID this SDK does not classify yet.
    Unknown,
}

impl PadMode {
    /// `true` when a controller answered the poll.
    #[inline]
    pub const fn is_connected(self) -> bool {
        !matches!(self, Self::Disconnected)
    }

    /// `true` when the controller is currently reporting stick bytes.
    #[inline]
    pub const fn has_sticks(self) -> bool {
        matches!(self, Self::Analog | Self::Config)
    }
}

/// Raw DualShock stick bytes from one poll.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct AnalogSticks {
    /// Right stick horizontal axis.
    pub right_x: u8,
    /// Right stick vertical axis.
    pub right_y: u8,
    /// Left stick horizontal axis.
    pub left_x: u8,
    /// Left stick vertical axis.
    pub left_y: u8,
}

impl AnalogSticks {
    /// Centred sticks.
    pub const CENTERED: Self = Self {
        right_x: STICK_CENTER,
        right_y: STICK_CENTER,
        left_x: STICK_CENTER,
        left_y: STICK_CENTER,
    };

    /// Left stick as signed deltas from centre.
    #[inline]
    pub const fn left_centered(self) -> (i16, i16) {
        (
            self.left_x as i16 - STICK_CENTER as i16,
            self.left_y as i16 - STICK_CENTER as i16,
        )
    }

    /// Right stick as signed deltas from centre.
    #[inline]
    pub const fn right_centered(self) -> (i16, i16) {
        (
            self.right_x as i16 - STICK_CENTER as i16,
            self.right_y as i16 - STICK_CENTER as i16,
        )
    }
}

/// A radial dead region around stick centre, with the scaled response the
/// literature settles on.
///
/// Three games on the PSoXide demo disc had grown one of these and they did not
/// agree. hl-psx tests `x*x + y*y` against a squared radius. VoXide gates each
/// axis separately, which makes its dead region a square: a stick pushed gently
/// along a diagonal clears the threshold on one axis and not the other, so the
/// input snaps to a cardinal instead of going where it was pushed. Sutphin's
/// write-up calls that the axial dead zone and its snapping is the classic
/// complaint about it.
///
/// A stick's centre drift is radial, so the region that ignores it is a circle,
/// and [`Deadzone::scaled`] is the scaled-radial form: remapped so the first
/// real reading starts from zero rather than jumping to the boundary magnitude.
/// That is the recommended default. [`Deadzone::gate`] is the plain radial one,
/// kept because the games being replaced were tuned against it.
///
/// This is for two-axis input. A single-axis control is a different question
/// and a scalar threshold is right for it: NitroXide steers on the left stick's
/// X alone, and folding Y into the test there would let noise on an axis it
/// does not read enable steering. See [`Deadzone::is_outside_axis`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Deadzone {
    inner_squared: i32,
    inner: i16,
    outer: i16,
}

/// Full deflection on one axis, from a stick that reads 0..=255 about 128.
pub const STICK_FULL: i16 = 127;

/// Fine-aim response shared by first-person games.
///
/// The centre is deliberately gentle while full deflection remains near the
/// full input range: 45% linear plus 55% cubic. This is the response used by
/// `hl-psx`, promoted here so other PSoXide games do not grow subtly different
/// controller curves.
#[inline]
pub const fn aim_curve(value: i16) -> i16 {
    let value = if value < -128 {
        -128
    } else if value > 127 {
        127
    } else {
        value
    };
    aim_curve_symmetric(value)
}

/// Fine-aim curve accepting both -128 and +128, for axes inverted after
/// centering. Uses the same sequential integer rounding as [`aim_curve`].
#[inline]
pub const fn aim_curve_symmetric(value: i16) -> i16 {
    let value = if value < -128 {
        -128
    } else if value > 128 {
        128
    } else {
        value
    };
    let sign = if value < 0 { -1 } else { 1 };
    let magnitude = if value < 0 {
        -(value as i32)
    } else {
        value as i32
    };
    let cubic = magnitude * magnitude / 128 * magnitude / 128;
    (sign * ((magnitude * 45 + cubic * 55) / 100)) as i16
}

impl Deadzone {
    /// A dead region of `radius` counts around centre. Typical radii are 12 to
    /// 30 of the 127 counts a healthy stick reaches. A negative radius is no
    /// dead region (zero): the squared test would otherwise treat it as its
    /// absolute value while [`scaled`](Self::scaled) divided by a zero length.
    pub const fn new(radius: i16) -> Self {
        let radius = if radius < 0 { 0 } else { radius };
        Self {
            inner_squared: (radius as i32) * (radius as i32),
            inner: radius,
            outer: STICK_FULL,
        }
    }

    /// Treat any deflection at or beyond `outer` as full scale.
    ///
    /// A worn stick often cannot reach its corners, so without this the player
    /// simply never gets full input. Sutphin recommends exposing it as a
    /// setting for anything where magnitude matters.
    pub const fn with_outer(mut self, outer: i16) -> Self {
        self.outer = outer;
        self
    }

    /// The inner radius this was built with.
    pub const fn radius(self) -> i16 {
        self.inner
    }

    /// Is this reading real input rather than centre drift?
    ///
    /// Strictly outside, matching the boundary behaviour of the games this
    /// replaces.
    #[inline]
    pub const fn is_outside(self, x: i16, y: i16) -> bool {
        let (x, y) = (x as i32, y as i32);
        x * x + y * y > self.inner_squared
    }

    /// The reading unchanged if it is real input, or `None` inside the dead
    /// region. The plain radial form: direction is preserved, but magnitude
    /// jumps from zero to the boundary the moment it is crossed.
    #[inline]
    pub const fn gate(self, x: i16, y: i16) -> Option<(i16, i16)> {
        if self.is_outside(x, y) {
            Some((x, y))
        } else {
            None
        }
    }

    /// Scaled radial: `None` inside the dead region, otherwise the reading with
    /// its magnitude remapped from the dead edge onto full scale, keeping
    /// direction.
    ///
    /// The result is clamped to [`STICK_FULL`] per axis. Without that a full
    /// diagonal, which is 179 counts long rather than 127, remaps to 138 on
    /// each axis and hands the game 9% more than it believes the stick can
    /// produce.
    #[inline]
    pub fn scaled(self, x: i16, y: i16) -> Option<(i16, i16)> {
        let (fx, fy) = (x as i32, y as i32);
        let magnitude = psx_math::int32::isqrt_i32(fx * fx + fy * fy);
        let inner = self.inner as i32;
        if magnitude <= inner {
            return None;
        }
        let span = (self.outer as i32 - inner).max(1);
        let scaled = (((magnitude - inner) * STICK_FULL as i32) / span).min(STICK_FULL as i32);
        Some((
            ((fx * scaled) / magnitude) as i16,
            ((fy * scaled) / magnitude) as i16,
        ))
    }

    /// The one-axis form, for a control that reads a single axis.
    #[inline]
    pub const fn is_outside_axis(self, v: i16) -> bool {
        let v = v as i32;
        v * v > self.inner_squared
    }

    /// The one-axis form of [`Deadzone::scaled`].
    #[inline]
    pub const fn scaled_axis(self, v: i16) -> Option<i16> {
        if !self.is_outside_axis(v) {
            return None;
        }
        let span = if self.outer > self.inner {
            (self.outer - self.inner) as i32
        } else {
            1
        };
        let magnitude = if v < 0 { -(v as i32) } else { v as i32 };
        let scaled = (magnitude - self.inner as i32) * STICK_FULL as i32 / span;
        let scaled = if scaled > STICK_FULL as i32 {
            STICK_FULL as i32
        } else {
            scaled
        };
        Some(if v < 0 { -scaled as i16 } else { scaled as i16 })
    }
}

impl Default for AnalogSticks {
    fn default() -> Self {
        Self::CENTERED
    }
}

/// Result of one controller poll.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PadState {
    /// Active-high button state.
    pub buttons: ButtonState,
    /// Inferred controller mode.
    pub mode: PadMode,
    /// Stick bytes. Centred when the controller is not reporting sticks.
    pub sticks: AnalogSticks,
    /// Raw low ID byte returned by the controller.
    pub id_low: u8,
}

impl PadState {
    /// No controller connected / no response.
    pub const NONE: Self = Self {
        buttons: ButtonState::NONE,
        mode: PadMode::Disconnected,
        sticks: AnalogSticks::CENTERED,
        id_low: 0xFF,
    };

    /// `true` when a controller answered the poll.
    #[inline]
    pub const fn is_connected(self) -> bool {
        self.mode.is_connected()
    }

    /// `true` when the controller is in DualShock analog mode.
    #[inline]
    pub const fn is_analog(self) -> bool {
        matches!(self.mode, PadMode::Analog)
    }
}

/// What the two motors of a DualShock are asked to do on a poll.
///
/// The small motor is on or off, the large motor has a level. The request
/// rides in two bytes of every poll once the motors are mapped
/// ([`enable_rumble_on`]); a pad that is not a DualShock in analog mode
/// ignores it, and the driver sends it zeros.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Rumble {
    /// The small (high-frequency) motor: on or off.
    pub small: bool,
    /// The large (low-frequency) motor's level; below about 0x40 it does not
    /// spin.
    pub large: u8,
}

impl Rumble {
    /// Both motors off.
    pub const OFF: Rumble = Rumble {
        small: false,
        large: 0,
    };

    /// A request for the two motors.
    pub const fn new(small: bool, large: u8) -> Self {
        Rumble { small, large }
    }

    /// Whether both motors are off.
    pub const fn is_off(self) -> bool {
        !self.small && self.large == 0
    }

    /// The two poll bytes: the small motor's (bit 0) and the large motor's
    /// level, in the order [`MOTOR_MAP_PACKET`] maps them.
    pub(crate) const fn bytes(self) -> (u8, u8) {
        (self.small as u8, self.large)
    }
}

/// One logical game action bound to up to two physical button masks.
///
/// A mask may contain more than one button; any held bit activates the
/// binding. Zero means that binding slot is unused.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ActionBinding {
    /// Primary physical button mask.
    pub primary: u16,
    /// Optional secondary physical button mask.
    pub secondary: u16,
}

impl ActionBinding {
    /// An action with no physical button assigned.
    pub const UNBOUND: Self = Self::new(0, 0);

    /// Bind an action to primary and secondary button masks.
    pub const fn new(primary: u16, secondary: u16) -> Self {
        Self { primary, secondary }
    }

    #[inline]
    const fn mask(self) -> u16 {
        self.primary | self.secondary
    }
}

/// Fixed, allocation-free mapping from game-defined action indices to pads.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ActionMap<const ACTIONS: usize> {
    bindings: [ActionBinding; ACTIONS],
}

impl<const ACTIONS: usize> ActionMap<ACTIONS> {
    /// Construct a map in the same order as the game's action enum.
    pub const fn new(bindings: [ActionBinding; ACTIONS]) -> Self {
        Self { bindings }
    }

    /// Read the complete binding table.
    pub const fn bindings(&self) -> &[ActionBinding; ACTIONS] {
        &self.bindings
    }

    /// Replace one binding. Out-of-range indices are ignored.
    pub fn set(&mut self, action: usize, binding: ActionBinding) {
        if let Some(slot) = self.bindings.get_mut(action) {
            *slot = binding;
        }
    }

    /// Read one binding, returning [`ActionBinding::UNBOUND`] out of range.
    pub const fn binding(&self, action: usize) -> ActionBinding {
        if action < ACTIONS {
            self.bindings[action]
        } else {
            ActionBinding::UNBOUND
        }
    }

    /// Interpret current and previous pad states through this map.
    pub const fn input<'a>(
        &'a self,
        current: PadState,
        previous: PadState,
    ) -> ActionInput<'a, ACTIONS> {
        ActionInput {
            map: self,
            current,
            previous,
        }
    }
}

/// Per-tick logical action view over a pair of pad samples.
#[derive(Copy, Clone, Debug)]
pub struct ActionInput<'a, const ACTIONS: usize> {
    map: &'a ActionMap<ACTIONS>,
    current: PadState,
    previous: PadState,
}

impl<const ACTIONS: usize> ActionInput<'_, ACTIONS> {
    /// Whether an action is held now.
    #[inline]
    pub fn is_held(self, action: usize) -> bool {
        let mask = self.map.binding(action).mask();
        mask != 0 && self.current.buttons.is_held(mask)
    }

    /// Whether an action transitioned from released to pressed this tick.
    #[inline]
    pub fn just_pressed(self, action: usize) -> bool {
        let mask = self.map.binding(action).mask();
        mask != 0 && self.current.buttons.is_held(mask) && !self.previous.buttons.is_held(mask)
    }

    /// Whether an action transitioned from pressed to released this tick.
    #[inline]
    pub fn just_released(self, action: usize) -> bool {
        let mask = self.map.binding(action).mask();
        mask != 0 && !self.current.buttons.is_held(mask) && self.previous.buttons.is_held(mask)
    }
}

/// How a transaction paces its bytes across the serial link.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Pacing {
    /// Wait only for the RX FIFO between bytes; never wait for `/ACK`. This is
    /// the legacy timing: it works with fast (often third-party) controllers and
    /// in emulation, but desyncs slower original units (e.g. SCPH-1200) that
    /// pull `/ACK` low later. Exposed so a diagnostic can reproduce the failure
    /// side-by-side with [`Pacing::AckWait`].
    NoAckWait,
    /// Wait for the device's `/ACK` (DSR) pulse after each non-final byte before
    /// clocking the next one. Matches the BIOS pacing, but corrupted frames on a
    /// third-party clone pad, so this IRQ-latched path remains diagnostic.
    /// Production polling observes live ACK without rewriting CTRL between
    /// bytes; the analog-enable handshake retains its separate fixed timing.
    /// Kept so on-console diagnostics can compare the legacy pacings.
    AckWait,
}

/// Raw wire result of one poll, before active-low button inversion. Carries the
/// per-byte `/ACK` observations so a diagnostic can show whether the controller
/// acknowledges each byte, and which pacing produced a clean handshake.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct RawPoll {
    /// ID low byte: 0x41 digital, 0x73 analog, 0xF3 config, 0xFF none.
    pub id_low: u8,
    /// ID high byte; 0x5A on a valid controller, anything else is a desync.
    pub id_high: u8,
    /// First button byte, raw active-low wire value.
    pub buttons_low: u8,
    /// Second button byte, raw active-low wire value.
    pub buttons_high: u8,
    /// Raw stick bytes (centre 0x80) when the controller reports them.
    pub sticks: AnalogSticks,
    /// Mode inferred from the ID bytes (`Unknown` when the 0x5A magic is wrong).
    pub mode: PadMode,
    /// Bit `i` set if exchange `i` observed an `/ACK` pulse (bit 0 = select byte).
    pub ack_seen: u16,
    /// Number of exchanges performed (select byte + payload bytes).
    pub exchanges: u8,
}

impl RawPoll {
    /// A poll where nothing answered (id_low `0xFF`). Useful as a const seed.
    pub const NONE: Self = Self::disconnected(0xFF);

    /// A poll where nothing answered.
    const fn disconnected(id_low: u8) -> Self {
        Self {
            id_low,
            id_high: 0xFF,
            buttons_low: 0xFF,
            buttons_high: 0xFF,
            sticks: AnalogSticks::CENTERED,
            mode: PadMode::Disconnected,
            ack_seen: 0,
            exchanges: 0,
        }
    }

    /// `true` when every payload exchange that should have been acknowledged
    /// was. The select byte (bit 0) and all bytes up to (but not including) the
    /// final, never-acknowledged byte must each show an `/ACK`.
    #[inline]
    pub fn is_fully_acknowledged(self) -> bool {
        if self.exchanges < 2 {
            return false;
        }
        // The last exchange is the final packet byte; the device does not ACK
        // it. Every earlier exchange must have been acknowledged.
        let acked_needed = self.exchanges - 1;
        let mask = (1u16 << acked_needed) - 1;
        self.ack_seen & mask == mask
    }

    /// Convert to the cleaned [`PadState`] used by game code.
    pub fn to_state(self) -> PadState {
        PadState {
            buttons: decode_buttons(self.buttons_low, self.buttons_high),
            mode: self.mode,
            sticks: self.sticks,
            id_low: self.id_low,
        }
    }
}

// The driver talks to the port through `psx_io::controller_port::Transport`: the
// production poll and the configuration sequence use its select / exchange /
// finish steps, the diagnostic pacings below use its register accesses. A
// transaction needs the port for its whole length, so every entry point takes
// the `ControllerPort` token by `&mut` (a memory-card transfer borrows it the
// same way), and `PadReader` keeps no hardware state of its own.

// The controller IRQ stays masked in `I_MASK` (the runtime only unmasks VBlank),
// but arming it in CTRL is what latches `STAT` bit 9 on the `/ACK` edge for the
// diagnostic ack-wait pacing.
const CTRL_ACK: u16 = sio0::ctrl::ACK;

const STAT_TX_READY: u32 = sio0::stat::TX_READY;
const STAT_RX_NOT_EMPTY: u32 = sio0::stat::RX_NOT_EMPTY;
const STAT_DSR_LEVEL: u32 = sio0::stat::DSR_LEVEL;
const STAT_IRQ: u32 = sio0::stat::IRQ;

/// Spin budget waiting for the byte shift itself (TX-ready / RX-not-empty).
const EXCHANGE_WAIT_SPINS: u32 = Timing::PAD.byte_spins;
/// Spin budget waiting for the `/ACK` pulse. Comfortably exceeds the kernel's
/// ~100us DSR timeout on hardware and the emulator's ~1k-cycle ACK deadline,
/// so a genuinely slow original controller is still given time to answer.
const ACK_WAIT_SPINS: u32 = Timing::PAD.ack_spins;
/// Setup delay (bounded STAT reads) after asserting the select line, before the
/// first clock. The original SCPH-1200 gives NO response without it and a clean
/// `5A41` digital read with it; fast clones tolerate it either way (silicon,
/// 2026-06-22). The on-console sweep put the floor between 384 (no response) and
/// 768 (clean) under a 5-polls-per-frame stress harder than the in-game one poll
/// per frame, so 1024 is a comfortable margin while keeping the per-poll
/// busy-wait small. Production retains this measured setup margin and also
/// waits for each non-final byte's ACK readiness; the BIOS likewise paces bytes.
pub const DEFAULT_SETUP_SPINS: u32 = Timing::PAD.setup_spins;

/// Spin budget for the address byte's `/ACK`, about 7,000 cycles at 6.9 cycles
/// a spin on silicon. A socket with nothing in it never pulses `/ACK`, so this
/// is what finding it empty costs after the setup delay. [`Timing::PAD`]'s
/// 2,048 reads, which a slow byte part way through a packet may need, would
/// cost twice as much. The console measured the pad's `/ACK` rising 1,627 to
/// 1,712 cycles after the write, the address byte slowest (v2.1, one pad), so
/// 512 spins (about 3,550 cycles) was only 2.07 times the slowest answer seen;
/// 1,024 gives the margin to other pads and to a transaction that starts late.
const ADDRESS_ACK_SPINS: u32 = 1024;

/// Poll the controller in `socket` once.
///
/// The returned [`PadState`] always contains active-high buttons; in
/// analog mode it also contains the four DualShock stick bytes. A packet
/// that cannot be completed after bounded retries reports [`PadMode::Unknown`]
/// rather than synthetic button bytes. A consistently absent device reports
/// [`PadMode::Disconnected`] after the bounded acquisition attempts.
///
/// `port` is the [`ControllerPort`](psx_io::periph::ControllerPort) token; it
/// stays borrowed for the length of the poll.
#[doc(alias = "PadRead")]
pub fn poll_on<T: Transport>(port: &mut T, socket: Port) -> PadState {
    poll_state(port, socket, false, Rumble::OFF)
}

/// [`poll_on`] with the motors asked for `rumble`. A pad that is not a
/// DualShock reporting analog mode is sent zeros; the request takes effect on
/// one that has been through [`enable_rumble_on`].
pub fn poll_rumble_on<T: Transport>(port: &mut T, socket: Port, rumble: Rumble) -> PadState {
    poll_state(port, socket, false, rumble)
}

/// Poll port 1 once.
#[deprecated(note = "use `poll_on` with the `ControllerPort` token")]
pub fn poll_port1() -> PadState {
    poll_on(&mut steal_port(), Port::One)
}

/// Poll port 2 once.
#[deprecated(note = "use `poll_on` with the `ControllerPort` token")]
pub fn poll_port2() -> PadState {
    poll_on(&mut steal_port(), Port::Two)
}

/// The token for a deprecated forwarder that never took one.
fn steal_port() -> ControllerPort {
    // SAFETY: a token is a logic guard, not a memory-safety one (see
    // `psx_io::periph`), and the old free functions never took one.
    unsafe { ControllerPort::steal() }
}

/// Poll `socket` once and return the raw wire bytes plus `/ACK` observations,
/// using the requested [`Pacing`]. Intended for diagnostics that want to show
/// the unfiltered handshake (or reproduce the legacy [`Pacing::NoAckWait`]
/// failure); normal game code should use [`poll_on`].
pub fn poll_raw_on<T: Transport>(port: &mut T, socket: Port, pacing: Pacing) -> RawPoll {
    poll_once_raw(port, socket, pacing)
}

/// Poll `socket` once with explicit fixed timing, for hardware diagnostics:
/// `setup_spins` of delay after asserting the select line, plus `interbyte_spins`
/// of fixed delay after each byte (bounded STAT reads -- no CTRL writes, no
/// `/ACK` wait, no DSR IRQ). This isolates the two timings a strict original pad
/// (SCPH-1200) might need -- setup time after `/CS`, and an inter-byte gap --
/// without the machinery that corrupted the ack-wait path on silicon.
pub fn poll_diagnostics_on<T: Transport>(
    port: &mut T,
    socket: Port,
    setup_spins: u32,
    interbyte_spins: u32,
) -> RawPoll {
    poll_once_timed(port, socket, setup_spins, interbyte_spins)
}

/// Poll port 1 once with fixed timing.
#[deprecated(note = "use `poll_diagnostics_on` with the `ControllerPort` token")]
pub fn poll_port1_diagnostics(setup_spins: u32, interbyte_spins: u32) -> RawPoll {
    poll_diagnostics_on(&mut steal_port(), Port::One, setup_spins, interbyte_spins)
}

/// Ask the controller in `socket` to enter DualShock analog mode. Returns
/// `true` when a follow-up poll reports analog mode.
///
/// Digital-only controllers simply keep reporting digital mode, so
/// callers should still gate analog-only controls on
/// [`PadState::is_analog`].
pub fn enable_analog_on<T: Transport>(port: &mut T, socket: Port) -> bool {
    enable_analog(port, socket)
}

/// Ask the port-1 controller to enter analog mode.
#[deprecated(note = "use `enable_analog_on` with the `ControllerPort` token")]
pub fn enable_analog_port1() -> bool {
    enable_analog_on(&mut steal_port(), Port::One)
}

/// Ask the port-2 controller to enter analog mode.
#[deprecated(note = "use `enable_analog_on` with the `ControllerPort` token")]
pub fn enable_analog_port2() -> bool {
    enable_analog_on(&mut steal_port(), Port::Two)
}

/// What a port held after [`require_analog_on`] asked it for analog mode.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AnalogRequirement {
    /// A DualShock answered in analog mode, now locked there: the Analog
    /// button no longer switches it back to digital.
    Analog,
    /// A controller answered but did not settle in analog mode: an original
    /// digital pad, or a DualShock that refused or ignored the request.
    DigitalOnly,
    /// No controller gave a clean reply.
    Absent,
}

impl AnalogRequirement {
    /// Classify the mode a pad reported after the analog request.
    pub const fn from_mode(mode: PadMode) -> Self {
        match mode {
            PadMode::Analog => Self::Analog,
            PadMode::Digital | PadMode::Config => Self::DigitalOnly,
            PadMode::Disconnected | PadMode::Unknown => Self::Absent,
        }
    }
}

/// Put the controller in `socket` in analog mode, lock it there, and report
/// what it settled on. For programs that need sticks and must not let the
/// Analog button flip the pad back to digital mid-session. Call it at boot (a
/// chain-loaded program starts from whatever the previous one left) and
/// again while the answer is not [`AnalogRequirement::Analog`], so a pad
/// plugged in later is picked up.
pub fn require_analog_on<T: Transport>(port: &mut T, socket: Port) -> AnalogRequirement {
    AnalogRequirement::from_mode(request_analog(port, socket, REQUIRE_ANALOG_GAP_SPINS).mode)
}

/// [`require_analog_on`] for port 1.
#[deprecated(note = "use `require_analog_on` with the `ControllerPort` token")]
pub fn require_analog_port1() -> AnalogRequirement {
    require_analog_on(&mut steal_port(), Port::One)
}

/// A port reader that never hands a garbled packet to the game.
///
/// A poll that fails validation reports [`PadMode::Unknown`] with every
/// button released. Taken at face value, a held button would read as released
/// for that one frame and then as a fresh press on the next, so a held Select
/// or Cross fires twice. The reader returns the last clean state instead.
/// A clean report of an empty port is accepted as it is: unplugging a pad
/// does release its buttons.
#[derive(Copy, Clone, Debug)]
pub struct PadReader {
    socket: Port,
    last: PadState,
    /// What the motors are asked for on every poll.
    rumble: Rumble,
}

impl PadReader {
    /// A reader for port 1 that has seen nothing yet.
    pub const fn port1() -> Self {
        Self {
            socket: Port::One,
            last: PadState::NONE,
            rumble: Rumble::OFF,
        }
    }

    /// A reader for port 2 that has seen nothing yet.
    pub const fn port2() -> Self {
        Self {
            socket: Port::Two,
            last: PadState::NONE,
            rumble: Rumble::OFF,
        }
    }

    /// Poll the port once and return the latest clean state.
    pub fn poll_on<T: Transport>(&mut self, port: &mut T) -> PadState {
        // A pad that was there a moment ago has its absence confirmed before
        // the reader believes it; a socket that was already empty is told so
        // at once.
        let polled = poll_state(port, self.socket, self.last.is_connected(), self.rumble);
        self.accept(polled)
    }

    /// What the motors are asked to do on every poll from now on. Takes effect
    /// on a DualShock in analog mode that has been through
    /// [`enable_rumble_on`](Self::enable_rumble_on).
    pub fn set_rumble(&mut self, rumble: Rumble) {
        self.rumble = rumble;
    }

    /// What the motors are asked for now.
    pub const fn rumble(&self) -> Rumble {
        self.rumble
    }

    /// Map the motors on this reader's pad. See [`enable_rumble_on`].
    pub fn enable_rumble_on<T: Transport>(&mut self, port: &mut T) -> bool {
        enable_rumble_on(port, self.socket)
    }

    /// Stop both motors now: ask for none and poll once so the pad hears it.
    /// For a pause, a menu, an exit; an unplugged pad has nothing to stop.
    pub fn stop_motors_on<T: Transport>(&mut self, port: &mut T) -> PadState {
        self.rumble = Rumble::OFF;
        self.poll_on(port)
    }

    /// Poll the port once and return the latest clean state.
    #[deprecated(note = "use `poll_on` with the `ControllerPort` token")]
    pub fn poll(&mut self) -> PadState {
        self.poll_on(&mut steal_port())
    }

    /// Fold one poll result into the reader and return the latest clean
    /// state. [`PadReader::poll_on`] calls this; it is public so a caller that
    /// polls some other way gets the same rule.
    pub fn accept(&mut self, state: PadState) -> PadState {
        if state.mode != PadMode::Unknown {
            self.last = state;
        }
        self.last
    }

    /// The latest clean state without polling.
    pub const fn last(&self) -> PadState {
        self.last
    }
}

/// Poll a port, retrying a garbled response.
///
/// On real hardware a poll occasionally desyncs -- a stale byte lingering in
/// the RX FIFO from the previous transaction, or a slipped /ACK between bytes --
/// and the ID handshake comes back wrong. A single such frame reads as "every
/// button released", which makes a *held* button (jump, Start) look like a fresh
/// press the next frame. So: when a controller answered but the DualShock ID
/// handshake didn't validate, retry a few times and take the first clean read.
/// Transport failures are retried as whole transactions, and a failure at any
/// stage after the address byte reports Unknown instead of accepting partial
/// button bytes.
///
/// An address byte that is not acknowledged and answered `0xFF` is an empty
/// socket, and a poll that finds one returns at once: the setup delay and one
/// short wait are all it costs, where four attempts cost a hundred thousand
/// cycles a frame for a game that polls an empty port 2. `confirm_absent` is
/// for a caller that saw a pad on this socket last time: one such reply could
/// be a glitch on a pad that is still there, so it takes four before it says
/// the pad is gone.
fn poll_state<T: Transport>(
    bus: &mut T,
    socket: Port,
    confirm_absent: bool,
    rumble: Rumble,
) -> PadState {
    let mut last = PadState::NONE;
    let mut all_absent = true;
    let mut tries = 0;
    while tries < 4 {
        let s = poll_once(bus, socket, rumble).to_state();
        if matches!(s.mode, PadMode::Digital | PadMode::Analog | PadMode::Config) {
            return s;
        }
        if s.mode == PadMode::Disconnected && !confirm_absent {
            return s;
        }
        all_absent &= s.mode == PadMode::Disconnected;
        last = s;
        tries += 1;
    }
    // Four complete address-byte replies of FF without ACK establish an
    // absent device. Any other failed stage means an invalid transaction.
    if !all_absent {
        last.mode = PadMode::Unknown;
    }
    last
}

fn poll_once_raw<T: Transport>(bus: &mut T, socket: Port, pacing: Pacing) -> RawPoll {
    // Raise JOYN so the device's state machine starts from idle, then drain
    // any stale RX byte. Only the ack-wait path arms the DSR IRQ.
    bus.select_with(socket, matches!(pacing, Pacing::AckWait));
    bus.drain_receive();

    let mut ack = 0u16;
    let mut i = 0u8;

    // The select byte and the poll command are never the final byte of any
    // packet, so they are always `/ACK`-paced under `AckWait`.
    let _select = ex(bus, socket, pacing, 0x01, false, &mut ack, &mut i);
    let id_low = ex(bus, socket, pacing, 0x42, false, &mut ack, &mut i);
    let mut mode = mode_from_id_low(id_low);
    if !mode.is_connected() {
        bus.deselect();
        return RawPoll {
            exchanges: i,
            ack_seen: ack,
            ..RawPoll::disconnected(id_low)
        };
    }

    let analog = mode.has_sticks();
    let id_high = ex(bus, socket, pacing, 0x00, false, &mut ack, &mut i);
    if id_high != 0x5A {
        // Garbled handshake -- treat as Unknown and stop after the two
        // button bytes (we can no longer trust the reported length).
        mode = PadMode::Unknown;
    }

    // For a digital pad the second button byte is the final byte (no `/ACK`
    // follows). For an analog pad the four stick bytes come after it.
    let read_sticks = analog && mode != PadMode::Unknown;
    let b0 = ex(bus, socket, pacing, 0x00, false, &mut ack, &mut i);
    let b1 = ex(bus, socket, pacing, 0x00, !read_sticks, &mut ack, &mut i);

    let sticks = if read_sticks {
        let right_x = ex(bus, socket, pacing, 0x00, false, &mut ack, &mut i);
        let right_y = ex(bus, socket, pacing, 0x00, false, &mut ack, &mut i);
        let left_x = ex(bus, socket, pacing, 0x00, false, &mut ack, &mut i);
        let left_y = ex(bus, socket, pacing, 0x00, true, &mut ack, &mut i);
        AnalogSticks {
            right_x,
            right_y,
            left_x,
            left_y,
        }
    } else {
        AnalogSticks::CENTERED
    };

    bus.deselect();

    RawPoll {
        id_low,
        id_high,
        buttons_low: b0,
        buttons_high: b1,
        sticks,
        mode,
        ack_seen: ack,
        exchanges: i,
    }
}

/// Clock one byte of a transaction under the given pacing, recording whether the
/// device acknowledged it. `is_last` marks the final byte of the packet, which
/// the device never acknowledges, so it is always sent without an `/ACK` wait.
#[inline]
fn ex<T: Transport>(
    bus: &mut T,
    socket: Port,
    pacing: Pacing,
    tx: u8,
    is_last: bool,
    ack_seen: &mut u16,
    idx: &mut u8,
) -> u8 {
    let i = *idx;
    *idx = idx.wrapping_add(1);
    match pacing {
        Pacing::NoAckWait => exchange_nowait(bus, tx),
        Pacing::AckWait if is_last => exchange_nowait(bus, tx),
        Pacing::AckWait => {
            let (byte, acked) = exchange_ack(bus, socket, tx);
            if acked && i < 16 {
                *ack_seen |= 1u16 << i;
            }
            byte
        }
    }
}

/// The production poll: retain the measured post-select setup delay, then
/// wait for each non-final byte's live ACK assertion and release. RX-ready
/// only establishes that the current byte arrived, not that the controller
/// is ready for the next one. No IRQ enable or CTRL rewrite is needed.
fn poll_once<T: Transport>(bus: &mut T, socket: Port, rumble: Rumble) -> RawPoll {
    // A previous aborted transaction may have left ACK asserted: `begin` waits
    // for it to release, so an old pulse never counts as the new address
    // byte's ACK.
    let result = if bus.begin(socket, Timing::PAD) {
        poll_selected(bus, rumble)
    } else {
        None
    };
    // End the peripheral transaction on every path. On a failed byte, also
    // reset the deselected UART: deselect alone need not cancel a late RX byte
    // still in its shifter/FIFO. The next select restores MODE and BAUD before
    // asserting the port again.
    bus.finish(result.is_some());
    result.unwrap_or(RawPoll {
        mode: PadMode::Unknown,
        ..RawPoll::NONE
    })
}

/// A complete selected-port poll, or no usable packet. The current ID
/// determines the length; a mode toggle never reuses an earlier length.
fn poll_selected<T: Transport>(bus: &mut T, rumble: Rumble) -> Option<RawPoll> {
    // The address byte is the one that tells a pad from an empty socket, so it
    // gets the BIOS's own limit for a device to answer, not the longer budget
    // for a slow byte part way through a packet.
    let address = Timing {
        ack_spins: ADDRESS_ACK_SPINS,
        ..Timing::PAD
    };
    match bus.exchange(0x01, false, address) {
        Ok(_) => {}
        Err(ExchangeError::AckTimeout { reply: 0xFF }) => return Some(RawPoll::NONE),
        Err(_) => return None,
    }
    let id_low = bus.exchange(0x42, false, Timing::PAD).ok()?;
    let mode = mode_from_id_low(id_low);
    if matches!(mode, PadMode::Unknown | PadMode::Disconnected) {
        return None;
    }
    let id_high = bus.exchange(0x00, false, Timing::PAD).ok()?;
    if id_high != 0x5A {
        return None;
    }
    let analog = mode.has_sticks();
    // The two bytes the motors ride in, for a DualShock reporting analog
    // mode; every other pad, and a pad parked in configuration mode, gets
    // zeros.
    let (small, large) = if mode == PadMode::Analog {
        rumble.bytes()
    } else {
        (0, 0)
    };
    let buttons_low = bus.exchange(small, false, Timing::PAD).ok()?;
    let buttons_high = bus.exchange(large, !analog, Timing::PAD).ok()?;
    let sticks = if analog {
        AnalogSticks {
            right_x: bus.exchange(0x00, false, Timing::PAD).ok()?,
            right_y: bus.exchange(0x00, false, Timing::PAD).ok()?,
            left_x: bus.exchange(0x00, false, Timing::PAD).ok()?,
            left_y: bus.exchange(0x00, true, Timing::PAD).ok()?,
        }
    } else {
        AnalogSticks::CENTERED
    };
    Some(RawPoll {
        id_low,
        id_high,
        buttons_low,
        buttons_high,
        sticks,
        mode,
        ack_seen: if analog { 0xff } else { 0x0f },
        exchanges: if analog { 9 } else { 5 },
    })
}

/// Poll with fixed setup and inter-byte delays (no `/ACK` wait). Mirrors
/// [`poll_once_raw`]'s byte sequence but paces purely with time. Always inlined
/// so each caller above is specialised on its own timing.
#[inline(always)]
fn poll_once_timed<T: Transport>(
    bus: &mut T,
    socket: Port,
    setup_spins: u32,
    interbyte_spins: u32,
) -> RawPoll {
    bus.select(socket);
    // Setup time after asserting /CS, before the first clock -- the strict
    // original pad may need this where a fast clone does not.
    bus.delay(setup_spins);
    bus.drain_receive();

    let mut i = 0u8;
    let _select = exchange_delayed(bus, 0x01, interbyte_spins);
    i += 1;
    let id_low = exchange_delayed(bus, 0x42, interbyte_spins);
    i += 1;
    let mut mode = mode_from_id_low(id_low);
    if !mode.is_connected() {
        bus.deselect();
        return RawPoll {
            exchanges: i,
            ..RawPoll::disconnected(id_low)
        };
    }

    let analog = mode.has_sticks();
    let id_high = exchange_delayed(bus, 0x00, interbyte_spins);
    i += 1;
    if id_high != 0x5A {
        mode = PadMode::Unknown;
    }
    let read_sticks = analog && mode != PadMode::Unknown;
    let b0 = exchange_delayed(bus, 0x00, interbyte_spins);
    i += 1;
    let b1 = exchange_delayed(bus, 0x00, interbyte_spins);
    i += 1;

    let sticks = if read_sticks {
        let right_x = exchange_delayed(bus, 0x00, interbyte_spins);
        let right_y = exchange_delayed(bus, 0x00, interbyte_spins);
        let left_x = exchange_delayed(bus, 0x00, interbyte_spins);
        let left_y = exchange_delayed(bus, 0x00, interbyte_spins);
        i += 4;
        AnalogSticks {
            right_x,
            right_y,
            left_x,
            left_y,
        }
    } else {
        AnalogSticks::CENTERED
    };

    bus.deselect();

    RawPoll {
        id_low,
        id_high,
        buttons_low: b0,
        buttons_high: b1,
        sticks,
        mode,
        ack_seen: 0,
        exchanges: i,
    }
}

/// Spins between the configuration commands of [`enable_analog`], a few
/// milliseconds each. Sony's libpad spaces configuration commands one per
/// video frame; issuing the three back to back left an SCPH-110 (PS one
/// DualShock) parked in configuration mode on a console, answering ID 0xF3
/// with buttons and sticks intact but never analog (controller-test capture,
/// 2026-08-16), while an SCPH-1200 took the same burst fine. Not yet
/// re-measured on silicon.
const CONFIG_COMMAND_GAP_SPINS: u32 = 8 * DEFAULT_SETUP_SPINS;

/// Spins between the configuration commands of [`require_analog_on`]:
/// about one video frame, the spacing Sony's libpad uses, so the request
/// that a program must win (the SCPH-110 is the pad that needed spacing at
/// all) uses the reference pacing rather than the shorter, unmeasured one
/// above. A spin costs about 6.7 CPU cycles on silicon (the 2026-07-26
/// setup-delay sweep: 192 spins polled in 8587 cycles, 1536 in 17615), so
/// this is about 660k cycles, a little over one 60 Hz frame. Only boot-time
/// and redetect callers pay it; [`enable_analog_on`] keeps its short gap
/// because games call it from their frame loops.
const REQUIRE_ANALOG_GAP_SPINS: u32 = 96 * DEFAULT_SETUP_SPINS;

/// Exit-config retries when a pad is still answering ID 0xF3 after the
/// analog request. Each costs one gap; a pad left in configuration mode
/// still reports buttons, but never analog, and ignores the Analog lock.
const CONFIG_EXIT_RETRIES: u32 = 3;

/// The config-mode packet that maps the motors: the first mapping byte (`0x00`)
/// puts the small motor on the first byte after the poll command's two, the
/// second (`0x01`) the large motor on the next, and `0xFF` leaves a slot
/// unused. A DualShock forgets it when it loses power.
pub const MOTOR_MAP_PACKET: [u8; 8] = [0x4D, 0x00, 0x00, 0x01, 0xFF, 0xFF, 0xFF, 0xFF];

/// Put a DualShock's motors under the poll: enter configuration mode, map the
/// small motor to the first motor byte and the large motor to the second
/// ([`MOTOR_MAP_PACKET`]), and leave configuration mode, spaced a video frame
/// apart as [`require_analog_on`] spaces its commands. Returns whether the pad
/// took the mapping from configuration mode and then reports DualShock analog
/// mode, which is the only mode a poll sends motor bytes in; a digital pad, a
/// pad that refuses configuration mode, or one that drops out part way returns
/// `false` and is polled as before. The pad's analog or digital
/// mode is left as it was. Call it again after a pad is plugged in.
///
/// This blocks for a few frames; the interrupt engine does the same without
/// blocking (`console::enable_rumble`).
pub fn enable_rumble_on<T: Transport>(port: &mut T, socket: Port) -> bool {
    let (state, mapped) = request_rumble(port, socket, REQUIRE_ANALOG_GAP_SPINS);
    mapped && state.is_analog()
}

/// Send the mapping sequence and return the state the pad settled on, and
/// whether the pad answered the mapping packet from configuration mode (ID
/// `0xF3`, magic `0x5A`): one that refused to enter it, or dropped out, did not
/// take the mapping.
fn request_rumble<T: Transport>(bus: &mut T, socket: Port, gap: u32) -> (PadState, bool) {
    transaction(
        bus,
        socket,
        [0x43, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    bus.delay(gap);
    let answer = transaction(bus, socket, MOTOR_MAP_PACKET);
    let mapped = answer[0] == 0xF3 && answer[1] == 0x5A;
    bus.delay(gap);
    transaction(
        bus,
        socket,
        [0x43, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    bus.delay(gap);
    let mut state = poll_state(bus, socket, true, Rumble::OFF);
    let mut retries = 0;
    while state.mode == PadMode::Config && retries < CONFIG_EXIT_RETRIES {
        transaction(
            bus,
            socket,
            [0x43, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        );
        bus.delay(gap);
        state = poll_state(bus, socket, true, Rumble::OFF);
        retries += 1;
    }
    (state, mapped)
}

fn enable_analog<T: Transport>(bus: &mut T, socket: Port) -> bool {
    request_analog(bus, socket, CONFIG_COMMAND_GAP_SPINS).is_analog()
}

/// Send the analog-and-lock request, spacing the commands by `gap` spins,
/// and return the state the pad settled on. A pad still parked in
/// configuration mode (ID 0xF3, the SCPH-110 failure) is sent the exit
/// command again, a bounded number of times, before its state is reported.
fn request_analog<T: Transport>(bus: &mut T, socket: Port, gap: u32) -> PadState {
    // Enter config mode.
    transaction(
        bus,
        socket,
        [0x43, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    bus.delay(gap);
    // Request analog mode and lock it so the pad cannot toggle
    // back underneath analog-only game controls.
    transaction(
        bus,
        socket,
        [0x44, 0x00, 0x01, 0x03, 0x00, 0x00, 0x00, 0x00],
    );
    bus.delay(gap);
    // Exit config mode, restoring the requested analog mode.
    transaction(
        bus,
        socket,
        [0x43, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    bus.delay(gap);
    let mut state = poll_state(bus, socket, true, Rumble::OFF);
    let mut retries = 0;
    while state.mode == PadMode::Config && retries < CONFIG_EXIT_RETRIES {
        // The exit did not take: leave the pad in a playable mode rather
        // than parked in configuration, then re-read what it settled on.
        transaction(
            bus,
            socket,
            [0x43, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        );
        bus.delay(gap);
        state = poll_state(bus, socket, true, Rumble::OFF);
        retries += 1;
    }
    state
}

#[inline]
fn mode_from_id_low(id_low: u8) -> PadMode {
    match id_low {
        0x41 => PadMode::Digital,
        0x73 => PadMode::Analog,
        0xF3 => PadMode::Config,
        0xFF => PadMode::Disconnected,
        _ => PadMode::Unknown,
    }
}

#[inline]
fn decode_buttons(b0: u8, b1: u8) -> ButtonState {
    // Wire bytes are active-low; invert to match our active-high
    // ButtonState convention.
    ButtonState::from_bits(!((b0 as u16) | ((b1 as u16) << 8)))
}

/// CTRL value held while a port is selected: JOYN asserted, TX enabled, and
/// normal receive supplied implicitly by the active `/CS`,
/// (only when `ack_irq`) the DSR (`/ACK`) interrupt armed so STAT bit 9 latches
/// each pulse. The default no-wait poll leaves the DSR IRQ off -- enabling it
/// turned out to disturb real-hardware transfers (the official SCPH-1200 stopped
/// answering, the clone's bytes corrupted), so it is reserved for the opt-in
/// ack-wait diagnostic only.
#[inline]
const fn active_ctrl(socket: Port, ack_irq: bool) -> u16 {
    sio0::selected_ctrl(socket.is_two(), ack_irq)
}

/// Run a fixed eight-byte DualShock command after the port-level controller
/// select byte, using the default no-wait timing (matching the reverted poll
/// path).
#[inline]
fn transaction<T: Transport>(bus: &mut T, socket: Port, bytes: [u8; 8]) -> [u8; 8] {
    bus.select(socket);
    bus.delay(DEFAULT_SETUP_SPINS);
    let mut ack = 0u16;
    let mut idx = 0u8;
    let _select = ex(
        bus,
        socket,
        Pacing::NoAckWait,
        0x01,
        false,
        &mut ack,
        &mut idx,
    );
    let mut out = [0u8; 8];
    let mut i = 0;
    while i < bytes.len() {
        let is_last = i == bytes.len() - 1;
        out[i] = ex(
            bus,
            socket,
            Pacing::NoAckWait,
            bytes[i],
            is_last,
            &mut ack,
            &mut idx,
        );
        i += 1;
    }
    bus.deselect();
    out
}

/// Clock one byte across the serial link without waiting for `/ACK`: wait for
/// TX-ready, write the byte, wait for RX to fill, read it. This is the legacy
/// timing -- correct for the final byte of a packet (which is never
/// acknowledged) and for the [`Pacing::NoAckWait`] diagnostic path.
///
/// The STAT register layout (from PSX-SPX):
/// - bit 0: TX-ready-1 (FIFO can accept a byte)
/// - bit 1: RX FIFO not empty
/// - bit 7: `/ACK` (DSR) live input level
/// - bit 9: latched DSR/ACK interrupt
#[inline]
fn exchange_nowait<T: Transport>(bus: &mut T, tx: u8) -> u8 {
    if !bus.wait_status_set(STAT_TX_READY, EXCHANGE_WAIT_SPINS) {
        return 0xFF;
    }
    bus.transmit(tx);
    if !bus.wait_status_set(STAT_RX_NOT_EMPTY, EXCHANGE_WAIT_SPINS) {
        return 0xFF;
    }
    bus.receive()
}

/// One no-wait byte exchange followed by a fixed inter-byte delay, giving a
/// strict pad time to be ready for the next byte without any `/ACK`/CTRL games.
#[inline]
fn exchange_delayed<T: Transport>(bus: &mut T, tx: u8, interbyte_spins: u32) -> u8 {
    let rx = exchange_nowait(bus, tx);
    bus.delay(interbyte_spins);
    rx
}

/// Clock one byte, then wait for the device's `/ACK` (DSR) pulse before
/// returning so the caller does not clock the next byte until the controller is
/// ready. Returns `(rx_byte, ack_observed)`.
///
/// The wait is non-fatal: on timeout we still return the received byte (the
/// byte shift itself already completed), so a device that never `/ACK`s degrades
/// to legacy timing rather than dropping the poll entirely.
#[inline]
fn exchange_ack<T: Transport>(bus: &mut T, socket: Port, tx: u8) -> (u8, bool) {
    if !bus.wait_status_set(STAT_TX_READY, EXCHANGE_WAIT_SPINS) {
        return (0xFF, false);
    }
    bus.transmit(tx);
    if !bus.wait_status_set(STAT_RX_NOT_EMPTY, EXCHANGE_WAIT_SPINS) {
        return (0xFF, false);
    }
    let rx = bus.receive();
    // Wait for the latched `/ACK` interrupt (STAT bit 9). The latch cannot be
    // missed, unlike the brief live level; the controller IRQ is masked in
    // `I_MASK`, so this never reaches the CPU.
    let acked = bus.wait_status_set(STAT_IRQ, ACK_WAIT_SPINS);
    if acked {
        // SIO0 STAT.9 is not edge-triggered: it can only be cleared once the
        // live `/ACK` line has released (STAT.7 low). Wait for that, then
        // pulse CTRL.ACK while keeping JOYN asserted so the device stays
        // selected for the next byte.
        let _ = bus.wait_status_clear(STAT_DSR_LEVEL, ACK_WAIT_SPINS);
        bus.set_control(active_ctrl(socket, true) | CTRL_ACK);
    }
    (rx, acked)
}

#[cfg(test)]
mod tests {

    #[test]
    fn symmetric_aim_and_group_edges_preserve_axis_policy() {
        for v in -128..=128 {
            assert_eq!(aim_curve_symmetric(v), -aim_curve_symmetric(-v));
        }
        assert_eq!(aim_curve_symmetric(128), 128);
        assert_eq!(aim_curve(128), 125);
        let a = ButtonState::from_bits(1);
        let b = ButtonState::from_bits(2);
        assert!(a.pressed_since(ButtonState::NONE, 3));
        assert!(!a.pressed_since(b, 3));
        assert!(a.pressed_since(b, 1));
    }

    #[test]
    fn a_radial_deadzone_is_a_circle_not_a_square() {
        // The whole reason this exists. A per-axis threshold of 24, which is
        // what NitroXide used, passes (20, 20) because neither axis clears it
        // on its own -- so a gentle diagonal push reads as nothing, and the
        // moment one axis does clear it the input snaps to a cardinal. Radially
        // (20, 20) is 28.3 counts out and is real input.
        let dz = Deadzone::new(24);
        assert!(dz.is_outside(20, 20), "diagonal push must count as input");
        assert!(
            !dz.is_outside(20, 0),
            "same magnitude per axis is still drift"
        );
        assert!(!dz.is_outside(0, 0));
        assert!(dz.is_outside(25, 0));
    }

    #[test]
    fn a_negative_deadzone_radius_is_no_deadzone_not_a_division_by_zero() {
        // `scaled(0, 0)` passed the `magnitude <= inner` test with inner = -1
        // and divided by a zero magnitude.
        for radius in [-1, -24, i16::MIN] {
            let dz = Deadzone::new(radius);
            assert_eq!(dz.radius(), 0);
            assert_eq!(dz, Deadzone::new(0));
            assert_eq!(dz.scaled(0, 0), None);
            assert_eq!(dz.scaled_axis(0), None);
            assert!(!dz.is_outside(0, 0));
            // A real deflection behaves as it does with a radius of zero.
            assert_eq!(dz.scaled(60, 0), Deadzone::new(0).scaled(60, 0));
            assert!(dz.is_outside(1, 0));
        }
    }

    #[test]
    fn the_one_axis_form_ignores_the_other_axis() {
        // NitroXide's steering: Y is not read, so it must not gate X.
        let dz = Deadzone::new(24);
        assert!(dz.is_outside_axis(30));
        assert!(!dz.is_outside_axis(20));
        // Rescaled onto full span like the two-axis form, not merely offset:
        // (30 - 24) of the 103 counts left maps to 7 of 127.
        assert_eq!(dz.scaled_axis(30), Some(7), "starts from zero, not 30");
        assert_eq!(dz.scaled_axis(-30), Some(-7), "sign survives");
        assert_eq!(dz.scaled_axis(127), Some(STICK_FULL), "full stays full");
        assert_eq!(dz.scaled_axis(10), None);
    }

    #[test]
    fn the_gate_hands_back_the_reading_unchanged() {
        let dz = Deadzone::new(16);
        assert_eq!(dz.gate(4, 4), None);
        assert_eq!(dz.gate(100, -40), Some((100, -40)));
    }

    #[test]
    fn scaling_starts_real_input_from_zero_instead_of_the_boundary() {
        // Ungated, a reading one count outside a radius of 30 arrives at
        // magnitude 31: a lurch. Scaled, it starts near zero and keeps its
        // direction.
        let dz = Deadzone::new(30);
        let (x, y) = dz.scaled(31, 0).expect("just outside is real input");
        assert!(x < 5, "expected a small first step, got {x}");
        assert_eq!(dz.scaled(127, 0), Some((STICK_FULL, 0)), "full stays full");
        assert_eq!(y, 0, "direction must survive the rescale");
        assert_eq!(dz.scaled(10, 10), None);
    }

    #[test]
    fn a_full_diagonal_does_not_exceed_full_scale() {
        // A diagonal is 179 counts long, not 127. Remapping that onto full
        // scale without a clamp hands the game 138 on each axis and it has no
        // reason to expect anything over 127.
        let dz = Deadzone::new(30);
        let (x, y) = dz.scaled(127, 127).expect("full deflection is input");
        assert!(x <= STICK_FULL && y <= STICK_FULL, "got ({x}, {y})");
        assert_eq!(x, y, "a diagonal must stay a diagonal");
    }

    #[test]
    fn an_outer_zone_lets_a_worn_stick_reach_full_scale() {
        // A stick that only manages 100 counts never gives full input without
        // this, which is the tip Sutphin gives for anything where magnitude
        // matters.
        let worn = Deadzone::new(20).with_outer(100);
        assert_eq!(worn.scaled(100, 0), Some((STICK_FULL, 0)));
        // And past the outer edge it saturates rather than overshooting.
        assert_eq!(worn.scaled(120, 0), Some((STICK_FULL, 0)));
    }

    #[test]
    fn aim_curve_is_gentle_at_center_and_near_full_at_the_edge() {
        assert_eq!(aim_curve(0), 0);
        assert_eq!(aim_curve(16), 7);
        assert_eq!(aim_curve(-16), -7);
        assert_eq!(aim_curve(127), 125);
        assert_eq!(aim_curve(-128), -128);
        assert!(aim_curve(64) < 64);
        assert_eq!(aim_curve(-64), -aim_curve(64));
    }

    use super::*;

    #[test]
    fn mode_ids_match_dualshock_wire_values() {
        assert_eq!(mode_from_id_low(0x41), PadMode::Digital);
        assert_eq!(mode_from_id_low(0x73), PadMode::Analog);
        assert_eq!(mode_from_id_low(0xF3), PadMode::Config);
        assert_eq!(mode_from_id_low(0xFF), PadMode::Disconnected);
        assert_eq!(mode_from_id_low(0x12), PadMode::Unknown);
    }

    #[test]
    fn decode_buttons_converts_active_low_to_active_high() {
        let state = decode_buttons(0xEF, 0xBF);
        assert!(state.is_held(button::UP));
        assert!(state.is_held(button::CROSS));
        assert!(!state.is_held(button::DOWN));
    }

    #[test]
    fn centred_stick_helpers_return_zero() {
        assert_eq!(AnalogSticks::CENTERED.left_centered(), (0, 0));
        assert_eq!(AnalogSticks::CENTERED.right_centered(), (0, 0));
    }

    #[test]
    fn raw_poll_to_state_inverts_buttons() {
        let raw = RawPoll {
            id_low: 0x41,
            id_high: 0x5A,
            buttons_low: 0xEF,  // UP pressed (active-low)
            buttons_high: 0xBF, // CROSS pressed (active-low)
            sticks: AnalogSticks::CENTERED,
            mode: PadMode::Digital,
            ack_seen: 0,
            exchanges: 5,
        };
        let state = raw.to_state();
        assert!(state.buttons.is_held(button::UP));
        assert!(state.buttons.is_held(button::CROSS));
        assert_eq!(state.mode, PadMode::Digital);
        assert_eq!(state.id_low, 0x41);
    }

    #[test]
    fn ack_complete_requires_every_non_final_byte_acked() {
        // Digital poll: 5 exchanges (select, cmd, id_hi, b0, b1). The first four
        // must ACK; the final b1 does not.
        let mut raw = RawPoll {
            id_low: 0x41,
            id_high: 0x5A,
            buttons_low: 0xFF,
            buttons_high: 0xFF,
            sticks: AnalogSticks::CENTERED,
            mode: PadMode::Digital,
            ack_seen: 0b0_1111, // exchanges 0..=3 acked
            exchanges: 5,
        };
        assert!(raw.is_fully_acknowledged());

        // Drop one ACK in the middle -> incomplete handshake.
        raw.ack_seen = 0b0_1011;
        assert!(!raw.is_fully_acknowledged());

        // A no-ack poll (legacy / slow original under NoAckWait) is incomplete.
        raw.ack_seen = 0;
        assert!(!raw.is_fully_acknowledged());
    }

    #[test]
    fn pacing_round_trips() {
        assert_ne!(Pacing::NoAckWait, Pacing::AckWait);
    }

    #[test]
    fn action_map_unifies_primary_secondary_and_edges() {
        let map = ActionMap::new([
            ActionBinding::new(button::CROSS, button::CIRCLE),
            ActionBinding::new(button::START, 0),
        ]);
        let previous = PadState {
            buttons: ButtonState::from_bits(button::CROSS),
            mode: PadMode::Digital,
            sticks: AnalogSticks::CENTERED,
            id_low: 0x41,
        };
        let current = PadState {
            buttons: ButtonState::from_bits(button::CIRCLE | button::START),
            ..previous
        };
        let input = map.input(current, previous);
        assert!(input.is_held(0));
        assert!(!input.just_pressed(0), "the logical action stayed held");
        assert!(input.just_pressed(1));
        assert!(!input.just_released(0));
        assert!(!input.is_held(99));
    }

    fn state(mode: PadMode, buttons: u16) -> PadState {
        PadState {
            buttons: ButtonState::from_bits(buttons),
            mode,
            sticks: AnalogSticks::CENTERED,
            id_low: match mode {
                PadMode::Digital => 0x41,
                PadMode::Analog => 0x73,
                PadMode::Config => 0xF3,
                _ => 0xFF,
            },
        }
    }

    #[test]
    fn analog_requirement_classifies_every_mode() {
        use AnalogRequirement as R;
        assert_eq!(R::from_mode(PadMode::Analog), R::Analog);
        assert_eq!(R::from_mode(PadMode::Digital), R::DigitalOnly);
        assert_eq!(R::from_mode(PadMode::Config), R::DigitalOnly);
        assert_eq!(R::from_mode(PadMode::Disconnected), R::Absent);
        assert_eq!(R::from_mode(PadMode::Unknown), R::Absent);
    }

    #[test]
    fn analog_and_digital_frames_decode_the_same_buttons() {
        let digital = RawPoll {
            id_low: 0x41,
            id_high: 0x5A,
            buttons_low: 0xFE,  // SELECT (active-low)
            buttons_high: 0xBF, // CROSS
            sticks: AnalogSticks::CENTERED,
            mode: PadMode::Digital,
            ack_seen: 0x0f,
            exchanges: 5,
        };
        let sticks = AnalogSticks {
            right_x: 0x80,
            right_y: 0x80,
            left_x: 0x00,
            left_y: 0xFF,
        };
        let analog = RawPoll {
            id_low: 0x73,
            sticks,
            mode: PadMode::Analog,
            ack_seen: 0xff,
            exchanges: 9,
            ..digital
        };
        let (d, a) = (digital.to_state(), analog.to_state());
        assert_eq!(d.buttons, a.buttons);
        assert_eq!(a.buttons.bits(), button::SELECT | button::CROSS);
        assert!(a.is_analog() && !d.is_analog());
        assert_eq!(a.sticks, sticks);
        assert_eq!(d.sticks, AnalogSticks::CENTERED);
    }

    #[test]
    fn a_packet_slipped_by_one_byte_reads_as_select_and_both_directions() {
        // Why every packet must be complete and paced: an analog packet that
        // slips one byte delivers the ID's 0x5A as the first button byte.
        // That decodes as Select, R3, Left and Right held together, the
        // phantom-Select pattern seen on a console in analog mode.
        let slipped = decode_buttons(0x5A, 0xFF);
        assert_eq!(
            slipped.bits(),
            button::SELECT | button::R3 | button::LEFT | button::RIGHT
        );
    }

    #[test]
    fn reader_keeps_the_last_clean_state_across_a_garbled_poll() {
        let mut reader = PadReader::port1();
        let mut tracker = PadTracker::new();
        let held = state(PadMode::Analog, button::SELECT);
        tracker.update(reader.accept(held).buttons.bits());
        assert!(tracker.just_pressed(button::SELECT));
        // The failed packet must neither release nor re-press Select.
        let garbled = PadState {
            mode: PadMode::Unknown,
            ..PadState::NONE
        };
        let seen = reader.accept(garbled);
        assert_eq!(seen, held);
        tracker.update(seen.buttons.bits());
        assert!(!tracker.just_released(button::SELECT));
        tracker.update(reader.accept(held).buttons.bits());
        assert!(!tracker.just_pressed(button::SELECT), "no second press");
    }

    #[test]
    fn reader_accepts_every_clean_mode_including_an_empty_port() {
        let mut reader = PadReader::port1();
        assert_eq!(reader.last(), PadState::NONE);
        for clean in [
            state(PadMode::Digital, button::CROSS),
            state(PadMode::Analog, button::START),
            state(PadMode::Config, 0),
            PadState::NONE,
        ] {
            assert_eq!(reader.accept(clean), clean);
        }
        assert_eq!(reader.last(), PadState::NONE, "unplugging releases");
    }
}
