// SPDX-License-Identifier: GPL-2.0-or-later
//! The left stick as d-pad presses, for menus.
//!
//! The stick is folded into a button mask in the [`button`] bit layout, so a game
//! ORs it into the pad's own bits and feeds the result to [`PadEdges`](crate::PadEdges)
//! or [`PadTracker`](crate::PadTracker): press edges come from there, and a menu that
//! must not repeat simply never asks for repeats. Two rules exist because the games
//! this replaces felt different on purpose:
//!
//! * [`stick_bits`]: each axis against one threshold, a diagonal sets two bits, no
//!   memory. It is a pure function of the reading, and the threshold is a const
//!   parameter, so a game with one fixed rule pays for that rule and nothing else.
//! * [`RadialStick`]: the distance from centre against two radii (hysteresis), one
//!   direction at a time, kept until the stick is back inside the exit radius.
//!
//! ```ignore
//! let mut pad = PadEdges::new();
//! loop {
//!     let state = poll_port1();
//!     let (x, y) = state.sticks.left_centered();
//!     let frame = pad.update(state.buttons.bits() | stick_bits::<48>(x, y));
//!     if frame.just_pressed(button::DOWN) { cursor_down(); }
//! }
//! ```

use crate::{button, Deadzone, PadState};

/// The four d-pad bits a fold produces.
const DPAD: u16 = button::LEFT | button::RIGHT | button::UP | button::DOWN;

/// The d-pad bits a stick reading (`x` and `y` in counts from centre, negative
/// left and up, as `AnalogSticks::left_centered` gives them) holds down: an axis
/// registers when it is strictly beyond `THRESHOLD` counts, so a diagonal sets two
/// bits. A pad that reports no stick passes `(0, 0)`. `THRESHOLD` is at least 0.
///
/// Code size depends on how the call site uses the mask, and no single body is smallest at every
/// shape (mipsel, release, measured against the hand-written fold each game replaced; a minus is
/// smaller):
///
/// * `buttons | stick_bits::<T>(x, y)` inlined at several sites (a platformer's menu, four sites):
///   -220 bytes. The else-if bodies merge the two axes in registers and OR once; an accumulating
///   fold (`buttons |= ..` inside the branches) duplicates the tail of every path.
/// * The same call feeding one edge tracker beside the plain buttons (a first-person game's menu,
///   one site): +12 bytes. The hand-written independent tests OR straight into the button word;
///   the mask form materialises a zero, moves the partial masks between registers and ORs once.
/// * A mask tested bit by bit (`mask & LEFT != 0`, a racing game's four direction booleans):
///   +72 bytes against comparing the centred value directly, because the booleans no longer fold
///   into the byte compares.
///
/// Branch-free bodies and a body that takes and returns the button word measured larger or equal
/// in the cases above, not smaller, so the else-if form stays.
#[must_use]
#[inline(always)]
pub const fn stick_bits<const THRESHOLD: i16>(x: i16, y: i16) -> u16 {
    const { assert!(THRESHOLD >= 0) };
    // `else if` per axis: with a threshold of at least 0 the two sides of an axis exclude each
    // other, and written that way the compiler emits one compare chain per axis (measured: four
    // independent tests came out 220 bytes larger over the three call sites of a game).
    let mut bits = 0;
    if x < -THRESHOLD {
        bits |= button::LEFT;
    } else if x > THRESHOLD {
        bits |= button::RIGHT;
    }
    if y < -THRESHOLD {
        bits |= button::UP;
    } else if y > THRESHOLD {
        bits |= button::DOWN;
    }
    bits
}

/// [`stick_bits`] of a pad state: the d-pad bits its left stick holds down, none unless the pad
/// reports sticks ([`PadState::menu_stick`]).
#[must_use]
#[inline(always)]
pub const fn stick_bits_of<const THRESHOLD: i16>(pad: &PadState) -> u16 {
    // The early return, rather than folding `(0, 0)`, lets the compiler turn the compares into
    // unsigned compares on the stick bytes themselves (measured: -44 bytes at one call site).
    if !pad.mode.has_sticks() {
        return 0;
    }
    let (x, y) = pad.sticks.left_centered();
    stick_bits::<THRESHOLD>(x, y)
}

/// Radial stick-to-d-pad fold with hysteresis. One per pad.
///
/// A push registers when the stick is strictly beyond radius `ENTER` ([`Deadzone`]
/// counts, of the 127 a healthy stick reaches); the direction is the dominant axis at
/// that moment, and it is held until the stick is no longer strictly beyond `EXIT`.
/// `EXIT` at or below `ENTER` leaves a band where a resting stick cannot chatter.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct RadialStick<const ENTER: i16, const EXIT: i16> {
    held: u16,
}

impl<const ENTER: i16, const EXIT: i16> RadialStick<ENTER, EXIT> {
    const VALID: () = assert!(EXIT <= ENTER && EXIT >= 0);

    /// A stick at rest.
    #[must_use]
    pub const fn new() -> Self {
        let () = Self::VALID;
        Self { held: 0 }
    }

    /// [`update`](Self::update) from a pad state: a pad that reports no sticks holds nothing
    /// ([`PadState::menu_stick`]).
    pub fn update_pad(&mut self, pad: &PadState) -> u16 {
        let (x, y) = pad.menu_stick();
        self.update(x, y)
    }

    /// The d-pad bit the stick holds down for this reading (zero or one of
    /// them); `x` and `y` as for [`stick_bits`].
    pub fn update(&mut self, x: i16, y: i16) -> u16 {
        self.held = if self.held == 0 {
            if Deadzone::new(ENTER).is_outside(x, y) {
                dominant(x, y) & DPAD
            } else {
                0
            }
        } else if Deadzone::new(EXIT).is_outside(x, y) {
            self.held
        } else {
            0
        };
        self.held
    }
}

impl<const ENTER: i16, const EXIT: i16> Default for RadialStick<ENTER, EXIT> {
    fn default() -> Self {
        Self::new()
    }
}

/// The d-pad bit for the larger axis. On a tie horizontal wins. Stick Y is 0 at
/// the top, like the d-pad's UP.
fn dominant(x: i16, y: i16) -> u16 {
    if x.abs() >= y.abs() {
        if x < 0 {
            button::LEFT
        } else {
            button::RIGHT
        }
    } else if y < 0 {
        button::UP
    } else {
        button::DOWN
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PadEdges, PadMode, PadTracker};

    #[test]
    fn stick_bits_of_equals_the_fold_of_menu_stick_in_every_mode_at_every_reading() {
        for mode in [
            PadMode::Disconnected,
            PadMode::Digital,
            PadMode::Analog,
            PadMode::Config,
            PadMode::Negcon,
            PadMode::Unknown,
        ] {
            for left_x in 0..=255u8 {
                for left_y in (0..=255u8).step_by(3) {
                    let mut pad = PadState::NONE;
                    pad.mode = mode;
                    pad.sticks.left_x = left_x;
                    pad.sticks.left_y = left_y;
                    let (x, y) = pad.menu_stick();
                    assert_eq!(stick_bits_of::<44>(&pad), stick_bits::<44>(x, y));
                    assert_eq!(stick_bits_of::<0>(&pad), stick_bits::<0>(x, y));
                    assert_eq!(
                        pad.mode.has_sticks(),
                        matches!(mode, PadMode::Analog | PadMode::Config)
                    );
                }
            }
        }
    }

    // The rules the games kept by hand, written out as they were (one count at
    // a time) so the fold is held to them exactly.

    /// Per axis, strictly beyond 48 each way (a first-person game's menu).
    fn rule_beyond_48(x: i16, y: i16) -> u16 {
        let mut bits = 0;
        if y < -48 {
            bits |= button::UP;
        }
        if y > 48 {
            bits |= button::DOWN;
        }
        if x < -48 {
            bits |= button::LEFT;
        }
        if x > 48 {
            bits |= button::RIGHT;
        }
        bits
    }

    /// Per axis, 48 or more each way (a platformer's menu; else-if per axis).
    fn rule_at_least_48(x: i16, y: i16) -> u16 {
        let mut bits = 0;
        if x <= -48 {
            bits |= button::LEFT;
        } else if x >= 48 {
            bits |= button::RIGHT;
        }
        if y <= -48 {
            bits |= button::UP;
        } else if y >= 48 {
            bits |= button::DOWN;
        }
        bits
    }

    /// Radial 64 in, 32 out, latched direction (a launcher's carousel).
    struct RuleRadial {
        latch: u16,
    }

    impl RuleRadial {
        fn update(&mut self, x: i16, y: i16) -> u16 {
            if self.latch == 0 {
                if Deadzone::new(64).is_outside(x, y) {
                    self.latch = dominant(x, y);
                    self.latch
                } else {
                    0
                }
            } else if Deadzone::new(32).is_outside(x, y) {
                self.latch
            } else {
                self.latch = 0;
                0
            }
        }
    }

    #[test]
    fn sticks_count_in_analog_and_config_modes_and_in_no_other() {
        for (mode, counts) in [
            (PadMode::Analog, true),
            (PadMode::Config, true),
            (PadMode::Digital, false),
            (PadMode::Unknown, false),
            (PadMode::Disconnected, false),
        ] {
            let mut pad = PadState::NONE;
            pad.mode = mode;
            pad.sticks.left_x = 0xff;
            let held = stick_bits_of::<48>(&pad);
            assert_eq!(held != 0, counts, "{mode:?}");
            let mut radial = RadialStick::<64, 32>::new();
            assert_eq!(radial.update_pad(&pad) != 0, counts, "{mode:?}");
        }
    }

    #[test]
    fn beyond_48_is_the_threshold_rule_at_every_reading() {
        for x in -128..=127 {
            for y in -128..=127 {
                assert_eq!(stick_bits::<48>(x, y), rule_beyond_48(x, y), "{x},{y}");
            }
        }
    }

    /// The rule every game wrote by hand, per axis and strictly beyond `t` counts, as a plain
    /// comparison on the centred value.
    fn rule_beyond(t: i16, x: i16, y: i16) -> u16 {
        let mut bits = 0;
        if y < -t {
            bits |= button::UP;
        }
        if y > t {
            bits |= button::DOWN;
        }
        if x < -t {
            bits |= button::LEFT;
        }
        if x > t {
            bits |= button::RIGHT;
        }
        bits
    }

    /// The same rule on the raw stick bytes (centre 128), as a game that never centres writes it:
    /// an axis is held when the byte is at most `127 - t` or at least `129 + t`.
    fn rule_on_bytes(t: i16, bx: u8, by: u8) -> u16 {
        let (bx, by) = (i16::from(bx), i16::from(by));
        let mut bits = 0;
        if bx <= 127 - t {
            bits |= button::LEFT;
        } else if bx >= 129 + t {
            bits |= button::RIGHT;
        }
        if by <= 127 - t {
            bits |= button::UP;
        } else if by >= 129 + t {
            bits |= button::DOWN;
        }
        bits
    }

    fn every_byte_pair_at<const T: i16>() {
        for bx in 0..=255u8 {
            for by in 0..=255u8 {
                let (x, y) = (i16::from(bx) - 128, i16::from(by) - 128);
                let got = stick_bits::<T>(x, y);
                assert_eq!(got, rule_beyond(T, x, y), "T={T} x={x} y={y}");
                assert_eq!(got, rule_on_bytes(T, bx, by), "T={T} bytes {bx},{by}");
            }
        }
    }

    macro_rules! every_threshold {
        ($($t:literal)*) => { $( every_byte_pair_at::<$t>(); )* };
    }

    /// All 65,536 stick byte pairs at every threshold from 0 to 127.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn every_byte_pair_at_every_threshold_matches_the_hand_written_rules() {
        every_threshold!(
            0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31
            32 33 34 35 36 37 38 39 40 41 42 43 44 45 46 47 48 49 50 51 52 53 54 55 56 57 58 59 60
            61 62 63 64 65 66 67 68 69 70 71 72 73 74 75 76 77 78 79 80 81 82 83 84 85 86 87 88 89
            90 91 92 93 94 95 96 97 98 99 100 101 102 103 104 105 106 107 108 109 110 111 112 113
            114 115 116 117 118 119 120 121 122 123 124 125 126 127
        );
    }

    #[test]
    fn threshold_47_is_the_at_least_48_rule_at_every_reading() {
        for x in -128..=127 {
            for y in -128..=127 {
                assert_eq!(stick_bits::<47>(x, y), rule_at_least_48(x, y), "{x},{y}");
            }
        }
    }

    #[test]
    fn radial_64_32_is_the_latched_rule_on_a_long_walk() {
        // A deterministic wander over the whole stick range, long enough to
        // enter, hover, leave and re-enter the band many times.
        let mut fold = RadialStick::<64, 32>::new();
        let mut rule = RuleRadial { latch: 0 };
        let (mut x, mut y, mut seed) = (0i16, 0i16, 12345u32);
        for _ in 0..200_000 {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let step = |r: u32| ((r >> 16) % 41) as i16 - 20;
            x = (x + step(seed)).clamp(-128, 127);
            y = (y + step(seed >> 7)).clamp(-128, 127);
            assert_eq!(fold.update(x, y), rule.update(x, y), "{x},{y}");
        }
        // And every reading from a fresh latch.
        for x in -128..=127 {
            for y in -128..=127 {
                let (mut fold, mut rule) = (RadialStick::<64, 32>::new(), RuleRadial { latch: 0 });
                assert_eq!(fold.update(x, y), rule.update(x, y), "{x},{y}");
            }
        }
    }

    #[test]
    fn a_resting_stick_inside_the_band_does_not_chatter() {
        let mut fold = RadialStick::<60, 40>::new();
        assert_eq!(fold.update(61, 0), button::RIGHT);
        // Wobbling between the radii keeps the push; inside exit releases it.
        assert_eq!(fold.update(50, 0), button::RIGHT);
        assert_eq!(fold.update(59, 0), button::RIGHT);
        assert_eq!(fold.update(40, 0), 0);
        // Re-entering needs the entry radius again.
        assert_eq!(fold.update(55, 0), 0);
        assert_eq!(fold.update(61, 0), button::RIGHT);
    }

    #[test]
    fn a_pad_without_a_stick_holds_nothing() {
        let mut fold = RadialStick::<64, 32>::new();
        assert_eq!(fold.update(100, 0), button::RIGHT);
        assert_eq!(fold.update(0, 0), 0);
        assert_eq!(stick_bits::<48>(0, 0), 0);
    }

    #[test]
    fn without_auto_repeat_a_push_is_one_press() {
        // A menu that only reads `just_pressed` gets one press per push,
        // however long the stick is held.
        let mut fold = RadialStick::<64, 32>::new();
        let mut pad = PadEdges::new();
        let mut presses = 0;
        for _ in 0..300 {
            presses += usize::from(pad.update(fold.update(127, 0)).just_pressed(button::RIGHT));
        }
        assert_eq!(presses, 1);
    }

    #[test]
    fn with_auto_repeat_the_stick_repeats_like_the_d_pad() {
        // The folded mask is ORed with the real buttons, so the d-pad and the
        // stick share one cadence: press, then after `delay`, every `rate`.
        let mut pad = PadTracker::new();
        let mut fired = [false; 8];
        for slot in fired.iter_mut() {
            pad.update(button::CROSS | stick_bits::<48>(0, 100));
            *slot = pad.repeats(button::DOWN, 3, 2);
        }
        assert_eq!(fired, [true, false, false, true, false, true, false, true]);
        assert!(pad.is_held(button::CROSS));
    }
}
