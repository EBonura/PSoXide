// SPDX-License-Identifier: GPL-2.0-or-later
//! The left stick as d-pad presses, for menus.
//!
//! [`StickDpad::update`] turns one reading of the centred stick into a button
//! mask in the [`button`] bit layout, so a game ORs it into the pad's own bits
//! and feeds the result to [`PadTracker`](crate::PadTracker): press edges come
//! from `just_pressed`, auto-repeat from `repeats` (a menu that must not
//! repeat simply never calls it). The thresholds and the shape of the test are
//! parameters, because the games this replaces felt different on purpose:
//! one fold judges each axis against a threshold with no hysteresis, another
//! judges the radial distance with two radii and picks one direction.
//!
//! ```ignore
//! let mut stick = StickDpad::per_axis(48, 48);
//! let mut pad = PadTracker::new();
//! loop {
//!     let state = poll_port1();
//!     let (x, y) = state.sticks.left_centered();
//!     pad.update(state.buttons.bits() | stick.update(x, y));
//!     if pad.repeats(button::DOWN, 15, 4) { cursor_down(); }
//! }
//! ```

use crate::{button, Deadzone};

/// The four d-pad bits a fold produces.
const DPAD: u16 = button::LEFT | button::RIGHT | button::UP | button::DOWN;

/// How [`StickDpad`] judges a reading.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum StickShape {
    /// Each axis against its own threshold: a diagonal push sets two bits.
    PerAxis,
    /// The radial distance from centre ([`Deadzone`]): a push sets one bit, the
    /// dominant axis at the moment it crossed the entry radius, and keeps it
    /// until the stick is back inside the exit radius.
    Radial,
}

/// Stick-to-d-pad fold with hysteresis. One per pad.
///
/// A push registers when it is strictly beyond `enter` counts (of the 127 a
/// healthy stick reaches) and is released when it is no longer strictly beyond
/// `exit`; `exit` at or below `enter` leaves a band where a resting stick
/// cannot chatter, and `exit == enter` is a plain threshold with no memory.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct StickDpad {
    shape: StickShape,
    enter: i16,
    exit: i16,
    /// What the last [`update`](Self::update) returned.
    held: u16,
}

impl StickDpad {
    /// A per-axis fold: each axis registers past `enter` and releases inside `exit`.
    pub const fn per_axis(enter: i16, exit: i16) -> Self {
        Self::new(StickShape::PerAxis, enter, exit)
    }

    /// A radial fold: one direction, entered past radius `enter`, released inside `exit`.
    pub const fn radial(enter: i16, exit: i16) -> Self {
        Self::new(StickShape::Radial, enter, exit)
    }

    const fn new(shape: StickShape, enter: i16, exit: i16) -> Self {
        Self {
            shape,
            enter,
            exit: if exit > enter { enter } else { exit },
            held: 0,
        }
    }

    /// The d-pad bits the stick holds down for this reading (`x` and `y` in
    /// counts from centre, negative left and up, as `AnalogSticks::left_centered`
    /// gives them). A pad that reports no stick passes `(0, 0)`.
    pub fn update(&mut self, x: i16, y: i16) -> u16 {
        self.held = match self.shape {
            StickShape::PerAxis => self.per_axis_bits(x, y),
            StickShape::Radial => self.radial_bits(x, y),
        };
        self.held
    }

    fn threshold(&self, bit: u16) -> i16 {
        if self.held & bit != 0 {
            self.exit
        } else {
            self.enter
        }
    }

    fn per_axis_bits(&self, x: i16, y: i16) -> u16 {
        let mut bits = 0;
        if x < -self.threshold(button::LEFT) {
            bits |= button::LEFT;
        } else if x > self.threshold(button::RIGHT) {
            bits |= button::RIGHT;
        }
        if y < -self.threshold(button::UP) {
            bits |= button::UP;
        } else if y > self.threshold(button::DOWN) {
            bits |= button::DOWN;
        }
        bits
    }

    fn radial_bits(&self, x: i16, y: i16) -> u16 {
        if self.held == 0 {
            if Deadzone::new(self.enter).is_outside(x, y) {
                dominant(x, y) & DPAD
            } else {
                0
            }
        } else if Deadzone::new(self.exit).is_outside(x, y) {
            self.held
        } else {
            0
        }
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
    use crate::PadTracker;

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
    fn per_axis_beyond_48_is_the_threshold_rule_at_every_reading() {
        let mut fold = StickDpad::per_axis(48, 48);
        for x in -128..=127 {
            for y in -128..=127 {
                assert_eq!(fold.update(x, y), rule_beyond_48(x, y), "{x},{y}");
            }
        }
    }

    #[test]
    fn per_axis_47_is_the_at_least_48_rule_at_every_reading() {
        let mut fold = StickDpad::per_axis(47, 47);
        for x in -128..=127 {
            for y in -128..=127 {
                assert_eq!(fold.update(x, y), rule_at_least_48(x, y), "{x},{y}");
            }
        }
    }

    #[test]
    fn radial_64_32_is_the_latched_rule_on_a_long_walk() {
        // A deterministic wander over the whole stick range, long enough to
        // enter, hover, leave and re-enter the band many times.
        let mut fold = StickDpad::radial(64, 32);
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
                let (mut fold, mut rule) = (StickDpad::radial(64, 32), RuleRadial { latch: 0 });
                assert_eq!(fold.update(x, y), rule.update(x, y), "{x},{y}");
            }
        }
    }

    #[test]
    fn a_resting_stick_inside_the_band_does_not_chatter() {
        let mut fold = StickDpad::per_axis(60, 40);
        assert_eq!(fold.update(61, 0), button::RIGHT);
        // Wobbling between the radii keeps the push; below exit releases it.
        assert_eq!(fold.update(50, 0), button::RIGHT);
        assert_eq!(fold.update(59, 0), button::RIGHT);
        assert_eq!(fold.update(40, 0), 0);
        // Re-entering needs the entry threshold again.
        assert_eq!(fold.update(55, 0), 0);
        assert_eq!(fold.update(61, 0), button::RIGHT);
    }

    #[test]
    fn an_exit_above_the_entry_is_clamped_to_it() {
        let mut fold = StickDpad::per_axis(30, 90);
        assert_eq!(fold.update(31, 0), button::RIGHT);
        assert_eq!(fold.update(30, 0), 0);
    }

    #[test]
    fn a_pad_without_a_stick_holds_nothing() {
        let mut fold = StickDpad::radial(64, 32);
        assert_eq!(fold.update(100, 0), button::RIGHT);
        assert_eq!(fold.update(0, 0), 0);
        let mut fold = StickDpad::per_axis(48, 48);
        assert_eq!(fold.update(0, 0), 0);
    }

    #[test]
    fn without_auto_repeat_a_push_is_one_press() {
        // A menu that only reads `just_pressed` gets one press per push,
        // however long the stick is held.
        let mut fold = StickDpad::radial(64, 32);
        let mut pad = PadTracker::new();
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
        let mut fold = StickDpad::per_axis(48, 48);
        let mut pad = PadTracker::new();
        let mut fired = [false; 8];
        for slot in fired.iter_mut() {
            pad.update(button::CROSS | fold.update(0, 100));
            *slot = pad.repeats(button::DOWN, 3, 2);
        }
        assert_eq!(fired, [true, false, false, true, false, true, false, true]);
        assert!(pad.is_held(button::CROSS));
    }
}
