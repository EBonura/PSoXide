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
//!     pad.update(state.buttons.bits() | stick_bits::<48>(x, y));
//!     if pad.just_pressed(button::DOWN) { cursor_down(); }
//! }
//! ```

use crate::{button, Deadzone};

/// The four d-pad bits a fold produces.
const DPAD: u16 = button::LEFT | button::RIGHT | button::UP | button::DOWN;

/// The d-pad bits a stick reading (`x` and `y` in counts from centre, negative
/// left and up, as `AnalogSticks::left_centered` gives them) holds down: an axis
/// registers when it is strictly beyond `THRESHOLD` counts, so a diagonal sets two
/// bits. A pad that reports no stick passes `(0, 0)`. `THRESHOLD` is at least 0.
#[inline(always)]
pub const fn stick_bits<const THRESHOLD: i16>(x: i16, y: i16) -> u16 {
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
    use crate::{PadEdges, PadTracker};

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
    fn beyond_48_is_the_threshold_rule_at_every_reading() {
        for x in -128..=127 {
            for y in -128..=127 {
                assert_eq!(stick_bits::<48>(x, y), rule_beyond_48(x, y), "{x},{y}");
            }
        }
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
            pad.update(fold.update(127, 0));
            presses += usize::from(pad.just_pressed(button::RIGHT));
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
