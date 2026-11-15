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

/// Frame-to-frame edges over an active-high 16-bit button mask: what is held,
/// what was held the frame before, and so what was just pressed or released.
/// Two words of state and no loops. It is the whole job of a menu or a game
/// that reads edges and never auto-repeats; [`PadTracker`] adds the per-button
/// hold counters, repeat and the handoff guard on top of it.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct PadEdges {
    held: u16,
    prev: u16,
}

impl PadEdges {
    /// Nothing held, nothing held before.
    #[must_use]
    pub const fn new() -> Self {
        Self { held: 0, prev: 0 }
    }

    /// Feed the current frame's polled buttons. Call exactly once per polled frame.
    #[inline]
    pub fn update(&mut self, buttons: u16) {
        self.prev = self.held;
        self.held = buttons;
    }

    /// The buttons held this frame.
    #[must_use]
    pub const fn held(&self) -> u16 {
        self.held
    }

    /// The buttons held on the frame before the last [`update`](Self::update).
    #[must_use]
    pub const fn previous(&self) -> u16 {
        self.prev
    }

    /// The buttons that went from released to held this frame.
    #[must_use]
    pub const fn pressed(&self) -> u16 {
        self.held & !self.prev
    }

    /// The buttons that went from held to released this frame.
    #[must_use]
    pub const fn released(&self) -> u16 {
        self.prev & !self.held
    }

    /// Any button in `mask` is held.
    #[must_use]
    pub const fn is_held(&self, mask: u16) -> bool {
        self.held & mask != 0
    }

    /// Any button in `mask` went from released to held this frame.
    #[must_use]
    pub const fn just_pressed(&self, mask: u16) -> bool {
        self.pressed() & mask != 0
    }

    /// Any button in `mask` went from held to released this frame.
    #[must_use]
    pub const fn just_released(&self, mask: u16) -> bool {
        self.released() & mask != 0
    }

    /// Make the previous state equal to the current one, so what is held now is
    /// not a fresh press (see [`PadTracker::settle`]).
    pub fn settle(&mut self) {
        self.prev = self.held;
    }
}

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

    /// The buttons held on the frame before the last [`update`](Self::update),
    /// as polled (suppression does not hide them here).
    #[must_use]
    pub const fn previous(&self) -> u16 {
        self.prev
    }

    /// Make the previous state equal to the current one, for a resume point
    /// (a menu closing, a scene starting): buttons held now stay held, but none
    /// counts as just pressed, and none of them reads as just released later
    /// than a real release. Unlike [`prime`](Self::prime) nothing is hidden
    /// from [`is_held`](Self::is_held), and held buttons keep their repeat count.
    pub fn settle(&mut self) {
        self.prev = self.held;
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
    fn pad_edges_match_the_tracker_on_every_frame_pair() {
        // The edge-only type is the tracker's edge half: same answers for every
        // pair of consecutive masks.
        for prev in [0u16, A, B, A | B, 0x8001, 0xffff] {
            for now in [0u16, A, B, A | B, 0x8001, 0xffff] {
                let (mut e, mut t) = (PadEdges::new(), PadTracker::new());
                e.update(prev);
                t.update(prev);
                e.update(now);
                t.update(now);
                for mask in [A, B, A | B, 0xffff] {
                    assert_eq!(e.just_pressed(mask), t.just_pressed(mask));
                    assert_eq!(e.just_released(mask), t.just_released(mask));
                    assert_eq!(e.is_held(mask), t.is_held(mask));
                }
                assert_eq!(e.previous(), t.previous());
                assert_eq!(e.pressed(), now & !prev);
                assert_eq!(e.released(), prev & !now);
            }
        }
    }

    #[test]
    fn pad_edges_settle_ends_the_press_and_keeps_the_hold() {
        let mut e = PadEdges::new();
        e.update(A);
        e.settle();
        assert!(e.is_held(A) && !e.just_pressed(A));
    }

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
    fn previous_is_the_last_frames_polled_mask() {
        let mut t = PadTracker::new();
        assert_eq!(t.previous(), 0);
        t.update(A);
        assert_eq!(t.previous(), 0);
        t.update(A | B);
        assert_eq!(t.previous(), A);
        t.prime();
        t.update(A | B);
        assert_eq!(t.previous(), A | B); // not hidden by prime
    }

    #[test]
    fn settle_keeps_held_but_ends_the_press() {
        let mut t = PadTracker::new();
        t.update(A);
        assert!(t.just_pressed(A));
        t.settle();
        assert!(t.is_held(A));
        assert!(!t.just_pressed(A));
        assert_eq!(t.previous(), A);
        // A button pressed after the settle is still a fresh press.
        t.update(A | B);
        assert!(t.just_pressed(B) && !t.just_pressed(A));
        // Releasing a settled button is a normal release.
        t.settle();
        t.update(B);
        assert!(t.just_released(A));
    }

    #[test]
    fn settle_does_not_hide_buttons_like_prime() {
        let mut t = PadTracker::new();
        t.update(A);
        t.settle();
        t.update(A);
        assert!(t.is_held(A));
        let mut p = PadTracker::new();
        p.update(A);
        p.prime();
        p.update(A);
        assert!(!p.is_held(A));
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
