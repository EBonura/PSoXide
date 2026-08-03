// SPDX-License-Identifier: GPL-2.0-or-later
//! Digital / DualShock pad polling via SIO0.
//!
//! Talks directly to the SIO0 hardware (no BIOS syscalls) so the
//! same code works whether we side-load a homebrew (HLE BIOS) or
//! boot through the real BIOS. The controller protocol is simple
//! enough that a hand-rolled select + four-byte exchange beats
//! opening events and waiting on them.
//!
//! Typical use from a game loop:
//!
//! ```ignore
//! use psx_pad::poll_port1;
//!
//! let pad = poll_port1();
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
//! that require sticks should either call [`enable_analog_port1`] or
//! show an "enable analog mode" prompt when [`PadState::is_analog`]
//! is false.
//!
//! Our [`ButtonState`] stores active-high so `buttons.is_held` feels
//! natural in game code.

#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

use psx_hw::sio::sio0;
use psx_io::sio;

pub mod tracker;
pub use tracker::PadTracker;

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
    /// third-party clone pad, so no default path uses it anymore: [`poll_port1`]
    /// and the analog-enable handshake pace with a post-select setup delay and
    /// no `/ACK` wait instead (see [`DEFAULT_SETUP_SPINS`]). Kept so the
    /// on-console diagnostic can compare both pacings side-by-side.
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
    pub fn ack_complete(self) -> bool {
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

// --- SIO0 register layout, from the shared hardware-model crate ---
// (`psx_hw::sio::sio0` is the single source of truth for these bits; the spin
// budgets below are this driver's behavior, not layout, and stay local.)

// We never take the CPU interrupt (the controller source stays masked in
// `I_MASK`; the runtime only unmasks VBlank), but see the layout doc: enabling
// this is what latches `STAT` bit 9 on the `/ACK` edge for polling.
const CTRL_ACK: u16 = sio0::ctrl::ACK;

const STAT_TX_READY: u32 = sio0::stat::TX_READY;
const STAT_RX_NOT_EMPTY: u32 = sio0::stat::RX_NOT_EMPTY;
const STAT_DSR_LEVEL: u32 = sio0::stat::DSR_LEVEL;
const STAT_IRQ: u32 = sio0::stat::IRQ;

const MODE_8N1: u16 = sio0::MODE_8N1;
const BAUD_PAD: u16 = sio0::BAUD_250KHZ;
/// Spin budget waiting for the byte shift itself (TX-ready / RX-not-empty).
const EXCHANGE_WAIT_SPINS: u32 = 32_768;
/// Spin budget waiting for the `/ACK` pulse. Comfortably exceeds the kernel's
/// ~100us DSR timeout on hardware and the emulator's ~1k-cycle ACK deadline,
/// so a genuinely slow original controller is still given time to answer.
const ACK_WAIT_SPINS: u32 = 2_048;
/// Setup delay (bounded STAT reads) after asserting the select line, before the
/// first clock. The original SCPH-1200 gives NO response without it and a clean
/// `5A41` digital read with it; fast clones tolerate it either way (silicon,
/// 2026-06-22). The on-console sweep put the floor between 384 (no response) and
/// 768 (clean) under a 5-polls-per-frame stress harder than the in-game one poll
/// per frame, so 1024 is a comfortable margin while keeping the per-poll
/// busy-wait small. (The BIOS uses ~7us of setup but also paces every byte; our
/// setup-only path trades a bigger setup for no inter-byte delay.)
pub const DEFAULT_SETUP_SPINS: u32 = 1_024;

/// Poll the controller in port 1 once.
///
/// The returned [`PadState`] always contains active-high buttons; in
/// analog mode it also contains the four DualShock stick bytes.
pub fn poll_port1() -> PadState {
    poll_state(false)
}

/// Poll the controller in port 2 once.
///
/// The returned [`PadState`] always contains active-high buttons; in
/// analog mode it also contains the four DualShock stick bytes.
pub fn poll_port2() -> PadState {
    poll_state(true)
}

/// Poll port 1 once and return the raw wire bytes plus `/ACK` observations,
/// using the requested [`Pacing`]. Intended for diagnostics that want to show
/// the unfiltered handshake (or reproduce the legacy [`Pacing::NoAckWait`]
/// failure); normal game code should use [`poll_port1`].
pub fn poll_port1_raw(pacing: Pacing) -> RawPoll {
    unsafe { poll_once_raw(false, pacing) }
}

/// Port-2 counterpart of [`poll_port1_raw`].
pub fn poll_port2_raw(pacing: Pacing) -> RawPoll {
    unsafe { poll_once_raw(true, pacing) }
}

/// Poll port 1 once with explicit fixed timing, for hardware diagnostics:
/// `setup_spins` of delay after asserting the select line, plus `interbyte_spins`
/// of fixed delay after each byte (bounded STAT reads -- no CTRL writes, no
/// `/ACK` wait, no DSR IRQ). This isolates the two timings a strict original pad
/// (SCPH-1200) might need -- setup time after `/CS`, and an inter-byte gap --
/// without the machinery that corrupted the ack-wait path on silicon.
pub fn poll_port1_diag(setup_spins: u32, interbyte_spins: u32) -> RawPoll {
    unsafe { poll_once_diag(false, setup_spins, interbyte_spins) }
}

/// Ask the port-1 controller to enter DualShock analog mode. Returns
/// `true` when a follow-up poll reports analog mode.
///
/// Digital-only controllers simply keep reporting digital mode, so
/// callers should still gate analog-only controls on
/// [`PadState::is_analog`].
pub fn enable_analog_port1() -> bool {
    enable_analog(false)
}

/// Ask the port-2 controller to enter DualShock analog mode. Returns
/// `true` when a follow-up poll reports analog mode.
pub fn enable_analog_port2() -> bool {
    enable_analog(true)
}

/// Poll a port, retrying a garbled response.
///
/// On real hardware a poll occasionally desyncs -- a stale byte lingering in
/// the RX FIFO from the previous transaction, or a slipped /ACK between bytes --
/// and the ID handshake comes back wrong. A single such frame reads as "every
/// button released", which makes a *held* button (jump, Start) look like a fresh
/// press the next frame. So: when a controller answered but the DualShock ID
/// handshake didn't validate, retry a few times and take the first clean read.
/// A genuinely empty port returns immediately (no wasted retries).
fn poll_state(port2: bool) -> PadState {
    let mut last = PadState::NONE;
    let mut tries = 0;
    while tries < 4 {
        // The default poll uses a setup delay after select: the original
        // SCPH-1200 will not answer without it (silicon-confirmed), clones are
        // fine with it. No inter-byte delay is needed.
        let s = unsafe { poll_once_diag(port2, DEFAULT_SETUP_SPINS, 0) }.to_state();
        if !s.is_connected() {
            return s; // nothing attached -- not a glitch, don't retry
        }
        if s.mode != PadMode::Unknown {
            return s; // valid digital/analog response
        }
        last = s; // answered but ID garbled -- retry
        tries += 1;
    }
    last
}

/// Drain up to a few stale bytes from the RX FIFO so the poll's first read
/// lines up with the controller's first response byte. Bounded so a stuck
/// "RX not empty" flag can never spin forever.
#[inline]
unsafe fn drain_rx() {
    let mut n = 0;
    unsafe {
        while psx_io::read32(sio::STAT) & 0x2 != 0 && n < 16 {
            let _ = psx_io::read8(sio::DATA);
            n += 1;
        }
    }
}

unsafe fn poll_once_raw(port2: bool, pacing: Pacing) -> RawPoll {
    unsafe {
        // Raise JOYN so the device's state machine starts from idle, then drain
        // any stale RX byte. Only the ack-wait path arms the DSR IRQ.
        select(port2, matches!(pacing, Pacing::AckWait));
        drain_rx();

        let mut ack = 0u16;
        let mut i = 0u8;

        // The select byte and the poll command are never the final byte of any
        // packet, so they are always `/ACK`-paced under `AckWait`.
        let _select = ex(port2, pacing, 0x01, false, &mut ack, &mut i);
        let id_low = ex(port2, pacing, 0x42, false, &mut ack, &mut i);
        let mut mode = mode_from_id_low(id_low);
        if !mode.is_connected() {
            deselect();
            return RawPoll {
                exchanges: i,
                ack_seen: ack,
                ..RawPoll::disconnected(id_low)
            };
        }

        let analog = mode.has_sticks();
        let id_high = ex(port2, pacing, 0x00, false, &mut ack, &mut i);
        if id_high != 0x5A {
            // Garbled handshake -- treat as Unknown and stop after the two
            // button bytes (we can no longer trust the reported length).
            mode = PadMode::Unknown;
        }

        // For a digital pad the second button byte is the final byte (no `/ACK`
        // follows). For an analog pad the four stick bytes come after it.
        let read_sticks = analog && mode != PadMode::Unknown;
        let b0 = ex(port2, pacing, 0x00, false, &mut ack, &mut i);
        let b1 = ex(port2, pacing, 0x00, !read_sticks, &mut ack, &mut i);

        let sticks = if read_sticks {
            let right_x = ex(port2, pacing, 0x00, false, &mut ack, &mut i);
            let right_y = ex(port2, pacing, 0x00, false, &mut ack, &mut i);
            let left_x = ex(port2, pacing, 0x00, false, &mut ack, &mut i);
            let left_y = ex(port2, pacing, 0x00, true, &mut ack, &mut i);
            AnalogSticks {
                right_x,
                right_y,
                left_x,
                left_y,
            }
        } else {
            AnalogSticks::CENTERED
        };

        deselect();

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
}

/// Clock one byte of a transaction under the given pacing, recording whether the
/// device acknowledged it. `is_last` marks the final byte of the packet, which
/// the device never acknowledges, so it is always sent without an `/ACK` wait.
#[inline]
unsafe fn ex(
    port2: bool,
    pacing: Pacing,
    tx: u8,
    is_last: bool,
    ack_seen: &mut u16,
    idx: &mut u8,
) -> u8 {
    unsafe {
        let i = *idx;
        *idx = idx.wrapping_add(1);
        match pacing {
            Pacing::NoAckWait => exchange_nowait(tx),
            Pacing::AckWait if is_last => exchange_nowait(tx),
            Pacing::AckWait => {
                let (byte, acked) = exchange_ack(port2, tx);
                if acked && i < 16 {
                    *ack_seen |= 1u16 << i;
                }
                byte
            }
        }
    }
}

/// Diagnostic poll with fixed setup and inter-byte delays (no `/ACK` wait).
/// Mirrors [`poll_once_raw`]'s byte sequence but paces purely with time.
unsafe fn poll_once_diag(port2: bool, setup_spins: u32, interbyte_spins: u32) -> RawPoll {
    unsafe {
        select(port2, false);
        // Setup time after asserting /CS, before the first clock -- the strict
        // original pad may need this where a fast clone does not.
        delay_reads(setup_spins);
        drain_rx();

        let mut i = 0u8;
        let _select = exchange_delayed(0x01, interbyte_spins);
        i += 1;
        let id_low = exchange_delayed(0x42, interbyte_spins);
        i += 1;
        let mut mode = mode_from_id_low(id_low);
        if !mode.is_connected() {
            deselect();
            return RawPoll {
                exchanges: i,
                ..RawPoll::disconnected(id_low)
            };
        }

        let analog = mode.has_sticks();
        let id_high = exchange_delayed(0x00, interbyte_spins);
        i += 1;
        if id_high != 0x5A {
            mode = PadMode::Unknown;
        }
        let read_sticks = analog && mode != PadMode::Unknown;
        let b0 = exchange_delayed(0x00, interbyte_spins);
        i += 1;
        let b1 = exchange_delayed(0x00, interbyte_spins);
        i += 1;

        let sticks = if read_sticks {
            let right_x = exchange_delayed(0x00, interbyte_spins);
            let right_y = exchange_delayed(0x00, interbyte_spins);
            let left_x = exchange_delayed(0x00, interbyte_spins);
            let left_y = exchange_delayed(0x00, interbyte_spins);
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

        deselect();

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
}

fn enable_analog(port2: bool) -> bool {
    unsafe {
        // Enter config mode.
        transaction(port2, [0x43, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00]);
        // Request analog mode and lock it so the pad cannot toggle
        // back underneath analog-only game controls.
        transaction(port2, [0x44, 0x00, 0x01, 0x03, 0x00, 0x00, 0x00, 0x00]);
        // Exit config mode, restoring the requested analog mode.
        transaction(port2, [0x43, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    }
    poll_state(port2).is_analog()
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
const fn active_ctrl(port2: bool, ack_irq: bool) -> u16 {
    sio0::selected_ctrl(port2, ack_irq)
}

/// Select the requested controller port and prepare SIO0 for a new
/// transaction. `ack_irq` arms the DSR interrupt (only the ack-wait diagnostic
/// path wants it).
#[inline]
unsafe fn select(port2: bool, ack_irq: bool) {
    unsafe {
        psx_io::write16(sio::MODE, MODE_8N1);
        psx_io::write16(sio::BAUD, BAUD_PAD);
        // Clear any stale IRQ latch from the previous transaction, then assert
        // JOYN.
        psx_io::write16(sio::CTRL, CTRL_ACK);
        psx_io::write16(sio::CTRL, active_ctrl(port2, ack_irq));
    }
}

/// Run a fixed eight-byte DualShock command after the port-level controller
/// select byte, using the default no-wait timing (matching the reverted poll
/// path).
#[inline]
unsafe fn transaction(port2: bool, bytes: [u8; 8]) -> [u8; 8] {
    unsafe {
        select(port2, false);
        delay_reads(DEFAULT_SETUP_SPINS);
        let mut ack = 0u16;
        let mut idx = 0u8;
        let _select = ex(port2, Pacing::NoAckWait, 0x01, false, &mut ack, &mut idx);
        let mut out = [0u8; 8];
        let mut i = 0;
        while i < bytes.len() {
            let is_last = i == bytes.len() - 1;
            out[i] = ex(
                port2,
                Pacing::NoAckWait,
                bytes[i],
                is_last,
                &mut ack,
                &mut idx,
            );
            i += 1;
        }
        deselect();
        out
    }
}

/// Drop JOYN so the attached device's state machine resets before
/// the next poll.
#[inline]
unsafe fn deselect() {
    unsafe { psx_io::write16(sio::CTRL, 0) };
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
unsafe fn exchange_nowait(tx: u8) -> u8 {
    unsafe {
        if !wait_stat(STAT_TX_READY, EXCHANGE_WAIT_SPINS) {
            return 0xFF;
        }
        psx_io::write8(sio::DATA, tx);
        if !wait_stat(STAT_RX_NOT_EMPTY, EXCHANGE_WAIT_SPINS) {
            return 0xFF;
        }
        psx_io::read8(sio::DATA)
    }
}

/// One no-wait byte exchange followed by a fixed inter-byte delay, giving a
/// strict pad time to be ready for the next byte without any `/ACK`/CTRL games.
#[inline]
unsafe fn exchange_delayed(tx: u8, interbyte_spins: u32) -> u8 {
    unsafe {
        let rx = exchange_nowait(tx);
        delay_reads(interbyte_spins);
        rx
    }
}

/// Burn time by reading STAT `n` times -- a real, non-optimizable MMIO delay.
#[inline]
unsafe fn delay_reads(n: u32) {
    let mut k = n;
    unsafe {
        while k > 0 {
            let _ = psx_io::read32(sio::STAT);
            k -= 1;
            core::hint::spin_loop();
        }
    }
}

/// Clock one byte, then wait for the device's `/ACK` (DSR) pulse before
/// returning so the caller does not clock the next byte until the controller is
/// ready. Returns `(rx_byte, ack_observed)`.
///
/// The wait is non-fatal: on timeout we still return the received byte (the
/// byte shift itself already completed), so a device that never `/ACK`s degrades
/// to legacy timing rather than dropping the poll entirely.
#[inline]
unsafe fn exchange_ack(port2: bool, tx: u8) -> (u8, bool) {
    unsafe {
        if !wait_stat(STAT_TX_READY, EXCHANGE_WAIT_SPINS) {
            return (0xFF, false);
        }
        psx_io::write8(sio::DATA, tx);
        if !wait_stat(STAT_RX_NOT_EMPTY, EXCHANGE_WAIT_SPINS) {
            return (0xFF, false);
        }
        let rx = psx_io::read8(sio::DATA);
        // Wait for the latched `/ACK` interrupt (STAT bit 9). The latch cannot be
        // missed, unlike the brief live level; the controller IRQ is masked in
        // `I_MASK`, so this never reaches the CPU.
        let acked = wait_stat(STAT_IRQ, ACK_WAIT_SPINS);
        if acked {
            // SIO0 STAT.9 is not edge-triggered: it can only be cleared once the
            // live `/ACK` line has released (STAT.7 low). Wait for that, then
            // pulse CTRL.ACK while keeping JOYN asserted so the device stays
            // selected for the next byte.
            let _ = wait_stat_low(STAT_DSR_LEVEL, ACK_WAIT_SPINS);
            psx_io::write16(sio::CTRL, active_ctrl(port2, true) | CTRL_ACK);
        }
        (rx, acked)
    }
}

/// Spin until `mask` bits are set in STAT, bounded by `spins`. Returns `false`
/// on timeout.
#[inline]
unsafe fn wait_stat(mask: u32, spins: u32) -> bool {
    let mut spins = spins;
    unsafe {
        while psx_io::read32(sio::STAT) & mask == 0 {
            if spins == 0 {
                return false;
            }
            spins -= 1;
            core::hint::spin_loop();
        }
    }
    true
}

/// Spin until `mask` bits are clear in STAT, bounded by `spins`. Returns `false`
/// on timeout.
#[inline]
unsafe fn wait_stat_low(mask: u32, spins: u32) -> bool {
    let mut spins = spins;
    unsafe {
        while psx_io::read32(sio::STAT) & mask != 0 {
            if spins == 0 {
                return false;
            }
            spins -= 1;
            core::hint::spin_loop();
        }
    }
    true
}

#[cfg(test)]
mod tests {
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
        assert!(raw.ack_complete());

        // Drop one ACK in the middle -> incomplete handshake.
        raw.ack_seen = 0b0_1011;
        assert!(!raw.ack_complete());

        // A no-ack poll (legacy / slow original under NoAckWait) is incomplete.
        raw.ack_seen = 0;
        assert!(!raw.ack_complete());
    }

    #[test]
    fn pacing_round_trips() {
        assert_ne!(Pacing::NoAckWait, Pacing::AckWait);
    }
}
