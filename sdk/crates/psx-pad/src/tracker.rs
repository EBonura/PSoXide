// SPDX-License-Identifier: GPL-2.0-or-later
//! Frame-to-frame button tracking: edges, auto-repeat, handoff suppression.
//!
//! Every project grew this trio by hand: gameplay wants just-pressed edges
//! (`(new ^ prev) & new`), menu cursors want delay-then-rate auto-repeat,
//! and anything that switches contexts (a collection launcher starting a
//! game, a menu opening over gameplay) wants buttons already held at the
//! handoff ignored until re-pressed. One tracker, fed the polled button
//! mask once per frame, answers all three.
//!
//! ```ignore
//! let mut pad = PadTracker::new();
//! loop {
//!     pad.update(poll_port1().buttons);
//!     if pad.just_pressed(button::CROSS) { jump(); }
//!     if pad.repeats(button::DOWN, 15, 4) { cursor_down(); }
//! }
//! ```

/// Frame-to-frame tracker over an active-high 16-bit button mask
/// (the [`PadState::buttons`](crate::PadState) representation).
#[derive(Copy, Clone)]
pub struct PadTracker {
    held: u16,
    prev: u16,
    /// Buttons held across a context handoff; masked out of every query
    /// until released once. See [`PadTracker::prime`].
    suppressed: u16,
    /// Frames each button has been continuously held (saturating).
    hold_frames: [u8; 16],
}

impl PadTracker {
    /// A tracker with nothing held and nothing suppressed.
    pub const fn new() -> Self {
        Self {
            held: 0,
            prev: 0,
            suppressed: 0,
            hold_frames: [0; 16],
        }
    }

    /// Feed the current frame's polled buttons. Call exactly once per
    /// polled frame, before any queries.
    pub fn update(&mut self, buttons: u16) {
        self.prev = self.held;
        self.held = buttons;
        // A suppressed button stays suppressed only while it is still held;
        // releasing it re-arms it as a normal button.
        self.suppressed &= buttons;
        for (i, frames) in self.hold_frames.iter_mut().enumerate() {
            if buttons & (1 << i) != 0 {
                *frames = frames.saturating_add(1);
            } else {
                *frames = 0;
            }
        }
    }

    /// Any button in `mask` is currently held (and not suppressed).
    pub fn is_held(&self, mask: u16) -> bool {
        self.held & !self.suppressed & mask != 0
    }

    /// Any button in `mask` went from released to held this frame.
    pub fn just_pressed(&self, mask: u16) -> bool {
        self.held & !self.prev & !self.suppressed & mask != 0
    }

    /// Any button in `mask` went from held to released this frame.
    pub fn just_released(&self, mask: u16) -> bool {
        self.prev & !self.held & mask != 0
    }

    /// Delay-then-rate auto-repeat for menu cursors: true on the initial
    /// press, again `delay` frames later, then every `rate` frames while
    /// held (PICO-8's cadence is `delay = 15, rate = 4`). A `rate` of 0 is
    /// treated as 1 (every frame).
    pub fn repeats(&self, mask: u16, delay: u8, rate: u8) -> bool {
        let rate = rate.max(1) as u32;
        let delay = delay as u32;
        let live = self.held & !self.suppressed & mask;
        for i in 0..16 {
            if live & (1 << i) == 0 {
                continue;
            }
            let held_for = self.hold_frames[i] as u32;
            if held_for == 1 {
                return true; // fresh press
            }
            if held_for > delay && (held_for - delay - 1).is_multiple_of(rate) {
                return true;
            }
        }
        false
    }

    /// Suppress every button held right now until it is released and pressed
    /// again. Call at a context handoff (launcher starts a game, pause menu
    /// closes) so a button held across the transition does not fire in the
    /// new context.
    pub fn prime(&mut self) {
        self.suppressed = self.held;
    }
}

impl Default for PadTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: u16 = 1 << 0;
    const B: u16 = 1 << 1;

    #[test]
    fn edges() {
        let mut t = PadTracker::new();
        t.update(A);
        assert!(t.just_pressed(A));
        assert!(t.is_held(A));
        assert!(!t.just_released(A));
        t.update(A);
        assert!(!t.just_pressed(A)); // still held, no new edge
        assert!(t.is_held(A));
        t.update(0);
        assert!(t.just_released(A));
        assert!(!t.is_held(A));
    }

    #[test]
    fn repeat_cadence() {
        // delay 3, rate 2: fires on the press (frame 1), first repeat 3
        // frames later (frame 4), then every 2 frames (6, 8, 10, ...).
        let mut t = PadTracker::new();
        let mut fired = [false; 10];
        for slot in fired.iter_mut() {
            t.update(A);
            *slot = t.repeats(A, 3, 2);
        }
        assert_eq!(
            fired,
            [true, false, false, true, false, true, false, true, false, true]
        );
    }

    #[test]
    fn repeat_rate_zero_means_every_frame_after_delay() {
        let mut t = PadTracker::new();
        t.update(A); // frame 1: fresh press
        assert!(t.repeats(A, 1, 0));
        t.update(A); // frame 2: past delay, rate 0 -> every frame
        assert!(t.repeats(A, 1, 0));
        t.update(A);
        assert!(t.repeats(A, 1, 0));
    }

    #[test]
    fn prime_suppresses_until_released() {
        let mut t = PadTracker::new();
        t.update(A | B);
        t.prime();
        t.update(A | B);
        assert!(!t.is_held(A) && !t.just_pressed(A));
        assert!(!t.repeats(A, 2, 2));
        // Release A, keep B held: A re-arms, B stays suppressed.
        t.update(B);
        t.update(A | B);
        assert!(t.just_pressed(A));
        assert!(t.is_held(A));
        assert!(!t.is_held(B));
    }

    #[test]
    fn independent_buttons() {
        let mut t = PadTracker::new();
        t.update(A);
        t.update(A | B);
        assert!(!t.just_pressed(A));
        assert!(t.just_pressed(B));
        assert!(t.is_held(A | B));
    }
}
