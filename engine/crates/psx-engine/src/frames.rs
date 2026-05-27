//! Frame- and tick-counter newtypes.
//!
//! # Why a newtype
//!
//! Engine code sometimes needs counters for a grab-bag of reasons:
//! visible-frame strobes, seeds for per-frame variation, or inputs to
//! the engine's [`Angle`][crate::Angle] math. Each call site wants a
//! `u32` back, but *what the counter measures* should be explicit -- is
//! this a visible frame, a simulation step, an SPU sample?
//!
//! Two distinct counters show up at different scales:
//!
//! - **[`SimTick`]** -- fixed simulation/control ticks since boot.
//!   Ticks once per display VBlank, even when visuals are paced down.
//!
//! - **[`VisualFrame`]** / **[`Frames`]** -- visible frames since boot.
//!   Ticks once per rendered/presented frame. This can diverge from
//!   [`SimTick`] when the app runner keeps the previous framebuffer visible.
//!
//! - **[`VideoHz`]** -- display cadence (`60` NTSC, `50` PAL), carried as
//!   a distinct type so frame counters are not confused with time base.
//!
//! - **[`Ticks`]** -- SPU-audio-rate counter (44100 Hz). Wraps at
//!   `u32::MAX` = ~27 hours of uptime, comfortably past any
//!   session length. Staying `u32` matches the R3000A's native
//!   word size and avoids synthesised add-with-carry sequences
//!   with zero practical benefit at PSX timescales.
//!
//! Code that needs raw arithmetic calls `as_u32` / `as_u16` to break
//! the newtype at the call site. Code that just wants "is this a
//! strobe frame?" can use `bit` which reads a single
//! bit without exposing the raw integer.
//!
//! # Semantics: when do the counters advance?
//!
//! `App::run` advances [`SimTick`] before each fixed update. It advances
//! [`VisualFrame`] only after a render/present. On the first update and
//! render, both counters are zero.

/// Fixed simulation/control tick.
///
/// This is the clock for gameplay, controls, animation phase, particles,
/// and other deterministic state. It advances once per display VBlank
/// regardless of visual render pacing.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, PartialOrd, Ord, Hash)]
pub struct SimTick(u32);

impl SimTick {
    /// The counter's zero point.
    pub const ZERO: SimTick = SimTick(0);

    /// Raw `u32` constructor -- use for test fixtures or when bridging
    /// to an external simulation tick count.
    pub const fn from_u32(n: u32) -> SimTick {
        SimTick(n)
    }

    /// Unwrap to the underlying `u32`.
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    /// Increment by one, wrapping at `u32::MAX`.
    #[inline]
    pub const fn advance(self) -> SimTick {
        SimTick(self.0.wrapping_add(1))
    }

    /// Add an arbitrary `u32` delta, wrapping on overflow.
    pub const fn wrapping_add(self, n: u32) -> SimTick {
        SimTick(self.0.wrapping_add(n))
    }

    /// Add an arbitrary `u32` delta, saturating at `u32::MAX`.
    pub const fn saturating_add(self, n: u32) -> SimTick {
        SimTick(self.0.saturating_add(n))
    }

    /// Subtract another simulation tick, returning a raw elapsed-tick
    /// delta. The result wraps to match the underlying counter.
    pub const fn wrapping_sub(self, other: SimTick) -> u32 {
        self.0.wrapping_sub(other.0)
    }

    /// Subtract another simulation tick, saturating at zero.
    pub const fn saturating_sub(self, other: SimTick) -> u32 {
        self.0.saturating_sub(other.0)
    }

    /// Read a specific bit of the underlying counter -- useful for
    /// strobe effects (`tick.bit(1)` flips every other simulation tick).
    #[inline]
    pub const fn bit(self, index: u8) -> bool {
        (self.0 >> index) & 1 != 0
    }

    /// `true` when the counter is divisible by `n` -- shorthand
    /// for "every `n`th simulation tick" cadences. [`SimTick::ZERO`]
    /// satisfies this for any `n ≥ 1`.
    #[inline]
    pub const fn every(self, n: u32) -> bool {
        n != 0 && self.0.is_multiple_of(n)
    }

    /// Elapsed simulation time as Q12 seconds.
    #[inline]
    pub fn elapsed_seconds_q12(self, hz: VideoHz) -> u32 {
        self.0.saturating_mul(1 << 12) / hz.as_nonzero_u32()
    }
}

/// Monotonic visible-frame counter.
///
/// This is the render/present counter. Use it for visual-only pacing and
/// profiling; use [`SimTick`] for gameplay or animation that must keep
/// stable speed when visual frames are skipped.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, PartialOrd, Ord, Hash)]
pub struct VisualFrame(u32);

impl VisualFrame {
    /// The counter's zero point. Both `update` and `render` see
    /// this value on the first iteration of the main loop.
    pub const ZERO: VisualFrame = VisualFrame(0);

    /// Raw `u32` constructor -- use for test fixtures or when
    /// bridging to an external frame count.
    pub const fn from_u32(n: u32) -> VisualFrame {
        VisualFrame(n)
    }

    /// Unwrap to the underlying `u32`.
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    /// Increment by one, wrapping at `u32::MAX`.
    #[inline]
    pub const fn advance(self) -> VisualFrame {
        VisualFrame(self.0.wrapping_add(1))
    }

    /// Add an arbitrary `u32` delta, wrapping on overflow.
    pub const fn wrapping_add(self, n: u32) -> VisualFrame {
        VisualFrame(self.0.wrapping_add(n))
    }

    /// Read a specific bit of the underlying counter -- useful for
    /// render-only strobe effects.
    #[inline]
    pub const fn bit(self, index: u8) -> bool {
        (self.0 >> index) & 1 != 0
    }

    /// `true` when the counter is divisible by `n` -- shorthand
    /// for "every `n`th visible frame" cadences. [`VisualFrame::ZERO`]
    /// satisfies this for any `n ≥ 1`.
    #[inline]
    pub const fn every(self, n: u32) -> bool {
        n != 0 && self.0.is_multiple_of(n)
    }
}

/// Backwards-compatible name for visible frames.
pub type Frames = VisualFrame;

/// Display cadence in whole frames per second.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VideoHz(u16);

impl VideoHz {
    /// NTSC display cadence.
    pub const NTSC: VideoHz = VideoHz(60);

    /// PAL display cadence.
    pub const PAL: VideoHz = VideoHz(50);

    /// Build from a raw Hz value. `0` is accepted but treated as `1`
    /// by conversion helpers to avoid divide-by-zero traps.
    pub const fn from_u16(hz: u16) -> VideoHz {
        VideoHz(hz)
    }

    /// Unwrap to the underlying `u16`.
    pub const fn as_u16(self) -> u16 {
        self.0
    }

    /// Unwrap as non-zero `u32`, clamping `0` to `1`.
    pub const fn as_nonzero_u32(self) -> u32 {
        let hz = self.0 as u32;
        if hz == 0 {
            1
        } else {
            hz
        }
    }

    /// Fixed simulation delta as Q12 seconds.
    #[inline]
    pub fn fixed_delta_seconds_q12(self) -> u32 {
        (1 << 12) / self.as_nonzero_u32()
    }
}

impl Default for VideoHz {
    fn default() -> Self {
        VideoHz::NTSC
    }
}

/// SPU-audio-rate tick counter.
///
/// `u32` at 44100 Hz wraps after ~27 hours of uptime -- comfortably
/// past any real session. The R3000A has 32-bit native registers;
/// wider counters would force every arithmetic op into carry-pair
/// sequences, spending instruction budget on range we cannot exhaust.
///
/// Reserved for engine subsystems that need finer time resolution
/// than per-frame (audio scheduling, profiling). No `Ctx` field yet.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, PartialOrd, Ord, Hash)]
pub struct Ticks(u32);

impl Ticks {
    /// Counter origin.
    pub const ZERO: Ticks = Ticks(0);

    /// Wrap a raw tick count.
    pub const fn from_u32(n: u32) -> Ticks {
        Ticks(n)
    }

    /// Unwrap to `u32`.
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    /// Add a delta, wrapping.
    pub const fn wrapping_add(self, n: u32) -> Ticks {
        Ticks(self.0.wrapping_add(n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_advance_wraps() {
        let f = Frames::from_u32(u32::MAX);
        assert_eq!(f.advance(), Frames::ZERO);
    }

    #[test]
    fn frames_bit_strobes() {
        assert!(!Frames::ZERO.bit(0));
        assert!(Frames::from_u32(1).bit(0));
        assert!(Frames::from_u32(2).bit(1));
        assert!(!Frames::from_u32(3).bit(2));
        assert!(Frames::from_u32(4).bit(2));
    }

    #[test]
    fn frames_every() {
        assert!(Frames::ZERO.every(40));
        assert!(!Frames::from_u32(1).every(40));
        assert!(Frames::from_u32(40).every(40));
        assert!(Frames::from_u32(80).every(40));
        // Zero denominator is a safe "never".
        assert!(!Frames::from_u32(5).every(0));
    }

    #[test]
    fn ticks_round_trip() {
        let t = Ticks::from_u32(0xCAFE_F00D);
        assert_eq!(t.as_u32(), 0xCAFE_F00D);
    }

    #[test]
    fn ticks_wraps_at_u32_max() {
        let t = Ticks::from_u32(u32::MAX);
        assert_eq!(t.wrapping_add(1), Ticks::ZERO);
    }
}
