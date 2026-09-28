//! Fixed-timestep game clock shared by every PSoXide game.
//!
//! Game logic runs in fixed ticks of a whole number of VBlanks; rendering runs
//! as often as the frame cost allows and shows the latest tick (optionally
//! interpolated with [`FixedClock::phase_q12`]). Logic speed then never
//! depends on how long a frame takes to draw.
//!
//! Each game picks its own [`TickConfig`] (tick rate and catch-up policy);
//! the functions are the same everywhere, so behaviour and the
//! [`TickStats`] consistency counters mean the same thing in every game.
//!
//! The clock never reads hardware. Callers pass the current VBlank count
//! (`psx_rt::interrupts::vblank_count()` on the console), which keeps every
//! rule host-testable.
//!
//! ```
//! use psx_tick::{FixedClock, TickConfig, TickRate};
//! let now = 100;
//! let mut clock = FixedClock::new(TickConfig::new(TickRate::HZ60), now);
//! // A frame that took three VBlanks owes three ticks.
//! let mut ran = 0;
//! while clock.due(now + 2) {
//!     ran += 1; // simulate one tick
//! }
//! clock.end_frame();
//! assert_eq!(ran, 3);
//! ```
#![no_std]

/// Tick length in VBlanks: 1 is 60 Hz on NTSC, 2 is 30 Hz, 3 is 20 Hz.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TickRate(u16);

impl TickRate {
    /// One tick per VBlank (60 Hz NTSC, 50 Hz PAL).
    pub const HZ60: TickRate = TickRate(1);
    /// One tick every second VBlank.
    pub const HZ30: TickRate = TickRate(2);
    /// One tick every third VBlank.
    pub const HZ20: TickRate = TickRate(3);

    /// One tick every `vblanks` VBlanks (0 is treated as 1).
    pub const fn every_vblanks(vblanks: u16) -> TickRate {
        TickRate(if vblanks == 0 { 1 } else { vblanks })
    }

    /// Tick length in VBlanks.
    pub const fn vblanks(self) -> u16 {
        self.0
    }
}

/// What to do when a frame falls further behind than the game allows.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CatchUp {
    /// Run every owed tick before the next render. Logic time never drops.
    Unbounded,
    /// Run at most this many ticks per frame, render, and carry the rest of
    /// the debt into the next frames. Logic time never drops; a long stall is
    /// caught up over several rendered frames instead of one frozen burst.
    Interleave(u16),
    /// Run at most this many ticks per frame and drop the rest of the debt
    /// (counted in [`TickStats::dropped`]). Game time then falls behind real
    /// time, so use it only where a burst would be worse than a slowdown.
    Cap(u16),
}

/// A game's clock settings.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TickConfig {
    /// Tick length.
    pub rate: TickRate,
    /// Catch-up policy.
    pub catch_up: CatchUp,
}

impl TickConfig {
    /// `rate` with unbounded catch-up.
    pub const fn new(rate: TickRate) -> TickConfig {
        TickConfig {
            rate,
            catch_up: CatchUp::Unbounded,
        }
    }

    /// At most `max_ticks` ticks per frame, the rest carried forward.
    pub const fn with_interleave(self, max_ticks: u16) -> TickConfig {
        TickConfig {
            rate: self.rate,
            catch_up: CatchUp::Interleave(if max_ticks == 0 { 1 } else { max_ticks }),
        }
    }

    /// At most `max_ticks` ticks per frame, the rest dropped.
    pub const fn with_cap(self, max_ticks: u16) -> TickConfig {
        TickConfig {
            rate: self.rate,
            catch_up: CatchUp::Cap(if max_ticks == 0 { 1 } else { max_ticks }),
        }
    }
}

/// Buckets in [`TickStats::ticks_per_frame`]; the last one collects
/// every frame with that many ticks or more.
pub const TICK_HISTOGRAM_BUCKETS: usize = 9;

/// Frame-consistency counters since construction or [`FixedClock::reset_stats`].
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct TickStats {
    /// Frames closed with [`FixedClock::end_frame`].
    pub frames: u32,
    /// Ticks run.
    pub ticks: u32,
    /// Ticks the catch-up cap dropped (only [`CatchUp::Cap`] drops any).
    pub dropped: u32,
    /// Most ticks any single frame ran.
    pub max_frame_ticks: u16,
    /// `ticks_per_frame[k]`: frames that ran `k` ticks (the last bucket is
    /// "that many or more"). A steady game sits in one or two buckets.
    pub ticks_per_frame: [u32; TICK_HISTOGRAM_BUCKETS],
}

/// Fixed-timestep clock. See the crate docs.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FixedClock {
    rate: u16,
    cap: u16,
    drop_excess: bool,
    next_due: u32,
    tick: u32,
    frame_ticks: u16,
    stats: TickStats,
}

#[inline(always)]
fn reached(now: u32, target: u32) -> bool {
    now.wrapping_sub(target) < 0x8000_0000
}

impl FixedClock {
    /// A clock whose first tick is due at VBlank `first_due`.
    pub const fn new(config: TickConfig, first_due: u32) -> FixedClock {
        FixedClock {
            rate: config.rate.vblanks(),
            cap: match config.catch_up {
                CatchUp::Unbounded => 0,
                CatchUp::Interleave(n) | CatchUp::Cap(n) => n,
            },
            drop_excess: matches!(config.catch_up, CatchUp::Cap(_)),
            next_due: first_due,
            tick: 0,
            frame_ticks: 0,
            stats: TickStats {
                frames: 0,
                ticks: 0,
                dropped: 0,
                max_frame_ticks: 0,
                ticks_per_frame: [0; TICK_HISTOGRAM_BUCKETS],
            },
        }
    }

    /// Forget any owed time and make the next tick due at `first_due`.
    /// Call after a load, a pause menu or anything else that should not be
    /// caught up. Not counted as dropped ticks.
    pub fn realign(&mut self, first_due: u32) {
        self.next_due = first_due;
    }

    /// `true` when a tick is due at VBlank `now`: run one tick, then ask
    /// again. The tick is counted as run when this returns `true`.
    ///
    /// Reading `now` fresh each time lets a frame keep catching up while its
    /// own ticks take time, exactly as a `while vblank >= next` loop does.
    pub fn due(&mut self, now: u32) -> bool {
        if !reached(now, self.next_due) {
            return false;
        }
        if self.cap != 0 && self.frame_ticks >= self.cap {
            if !self.drop_excess {
                return false;
            }
            let behind = now.wrapping_sub(self.next_due) / u32::from(self.rate) + 1;
            self.stats.dropped = self.stats.dropped.saturating_add(behind);
            self.next_due = self
                .next_due
                .wrapping_add(behind.wrapping_mul(u32::from(self.rate)));
            return false;
        }
        self.consume();
        true
    }

    /// `true` when a tick is due at VBlank `now`, without running it and
    /// ignoring the catch-up policy. For schedulers that decide first and
    /// commit later with [`consume`](Self::consume); game loops use
    /// [`due`](Self::due).
    pub fn is_due(&self, now: u32) -> bool {
        reached(now, self.next_due)
    }

    /// Record one tick as run: advance the deadline and the counters. The
    /// caller has already decided it was due (see [`is_due`](Self::is_due)).
    pub fn consume(&mut self) {
        self.next_due = self.next_due.wrapping_add(u32::from(self.rate));
        self.tick = self.tick.wrapping_add(1);
        self.frame_ticks = self.frame_ticks.saturating_add(1);
        self.stats.ticks = self.stats.ticks.saturating_add(1);
    }

    /// Close the current frame: record how many ticks it ran and start
    /// counting the next frame's.
    pub fn end_frame(&mut self) {
        let k = usize::from(self.frame_ticks).min(TICK_HISTOGRAM_BUCKETS - 1);
        self.stats.ticks_per_frame[k] = self.stats.ticks_per_frame[k].saturating_add(1);
        self.stats.frames = self.stats.frames.saturating_add(1);
        self.stats.max_frame_ticks = self.stats.max_frame_ticks.max(self.frame_ticks);
        self.frame_ticks = 0;
    }

    /// Ticks run since construction.
    pub const fn tick(&self) -> u32 {
        self.tick
    }

    /// Ticks run so far in the current frame.
    pub const fn frame_ticks(&self) -> u16 {
        self.frame_ticks
    }

    /// Tick length in VBlanks.
    pub const fn rate(&self) -> TickRate {
        TickRate(self.rate)
    }

    /// VBlank at which the next tick is due.
    pub const fn next_due(&self) -> u32 {
        self.next_due
    }

    /// How far VBlank `now` is from the last tick toward the next one, in
    /// Q12 (0 = the last tick has just been reached, 4096 = the next one is
    /// due). Renderers draw `lerp(previous, latest, phase)`: one tick of
    /// latency in exchange for motion that moves on every rendered frame at
    /// any tick rate. A game ticking every VBlank can simply draw the latest
    /// state instead.
    pub fn phase_q12(&self, now: u32) -> u16 {
        let ahead = self.next_due.wrapping_sub(now) as i32;
        let rate = i32::from(self.rate);
        if ahead <= 0 {
            4096
        } else {
            (((rate - ahead.min(rate)) << 12) / rate) as u16
        }
    }

    /// Consistency counters.
    pub const fn stats(&self) -> TickStats {
        self.stats
    }

    /// Zero the consistency counters (for example at the start of a level).
    pub fn reset_stats(&mut self) {
        self.stats = TickStats::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(clock: &mut FixedClock, now: u32) -> u32 {
        let mut n = 0;
        while clock.due(now) {
            n += 1;
        }
        clock.end_frame();
        n
    }

    #[test]
    fn sixty_hz_runs_one_tick_per_vblank_and_catches_up_a_slow_frame() {
        let mut c = FixedClock::new(TickConfig::new(TickRate::HZ60), 10);
        assert_eq!(run(&mut c, 9), 0);
        assert_eq!(run(&mut c, 10), 1);
        assert_eq!(run(&mut c, 11), 1);
        assert_eq!(run(&mut c, 15), 4);
        assert_eq!(c.tick(), 6);
        assert_eq!(c.stats().ticks_per_frame[..5], [1, 2, 0, 0, 1]);
        assert_eq!(c.stats().max_frame_ticks, 4);
        assert_eq!(c.stats().dropped, 0);
    }

    #[test]
    fn twenty_hz_ticks_every_third_vblank() {
        let mut c = FixedClock::new(TickConfig::new(TickRate::HZ20), 3);
        let ticks: u32 = (0..=30).map(|now| run(&mut c, now)).sum();
        assert_eq!(ticks, 10); // due at 3, 6, ..., 30
        assert_eq!(c.next_due(), 33);
    }

    #[test]
    fn logic_time_matches_real_time_whatever_the_frame_lengths() {
        // Frames of 1..7 VBlanks in a fixed pattern: every tick owed by
        // VBlank 1000 has run, the same count a 60 fps game would have run.
        let mut c = FixedClock::new(TickConfig::new(TickRate::HZ30), 0);
        let mut now = 0u32;
        let mut k = 0u32;
        while now < 1000 {
            run(&mut c, now);
            now += 1 + (k % 7);
            k += 1;
        }
        run(&mut c, 1000);
        assert_eq!(c.tick(), 501); // ticks at 0, 2, ..., 1000
        assert_eq!(c.stats().dropped, 0);
    }

    #[test]
    fn cap_drops_the_excess_and_counts_it() {
        let mut c = FixedClock::new(TickConfig::new(TickRate::HZ60).with_cap(4), 0);
        assert_eq!(run(&mut c, 0), 1);
        // 10 ticks owed (1..=10), 4 allowed.
        assert_eq!(run(&mut c, 10), 4);
        assert_eq!(c.stats().dropped, 6);
        assert_eq!(c.next_due(), 11);
        assert_eq!(run(&mut c, 11), 1);
    }

    #[test]
    fn interleave_renders_between_bursts_and_loses_no_time() {
        let mut c = FixedClock::new(TickConfig::new(TickRate::HZ60).with_interleave(4), 0);
        assert_eq!(run(&mut c, 0), 1);
        assert_eq!(run(&mut c, 10), 4); // ticks 1..=4
        assert_eq!(run(&mut c, 10), 4); // 5..=8
        assert_eq!(run(&mut c, 11), 3); // 9..=11: caught up
        assert_eq!(c.tick(), 12);
        assert_eq!(c.stats().dropped, 0);
    }

    #[test]
    fn is_due_and_consume_split_the_decision_from_the_commit() {
        let mut c = FixedClock::new(TickConfig::new(TickRate::HZ60), 5);
        assert!(!c.is_due(4));
        assert!(c.is_due(5));
        assert!(c.is_due(5)); // a query does not run the tick
        c.consume();
        assert!(!c.is_due(5));
        assert_eq!((c.tick(), c.frame_ticks(), c.next_due()), (1, 1, 6));
    }

    #[test]
    fn realign_forgets_debt_without_counting_it_dropped() {
        let mut c = FixedClock::new(TickConfig::new(TickRate::HZ60), 0);
        run(&mut c, 0);
        c.realign(501); // a 500-VBlank load
        assert_eq!(run(&mut c, 501), 1);
        assert_eq!(c.stats().dropped, 0);
    }

    #[test]
    fn phase_sweeps_from_last_tick_to_next() {
        let mut c = FixedClock::new(TickConfig::new(TickRate::HZ20), 0);
        run(&mut c, 0); // next due at 3
        assert_eq!(c.phase_q12(0), 0);
        assert_eq!(c.phase_q12(1), 1365);
        assert_eq!(c.phase_q12(2), 2730);
        assert_eq!(c.phase_q12(3), 4096);
        let mut s = FixedClock::new(TickConfig::new(TickRate::HZ60), 0);
        run(&mut s, 0);
        assert_eq!(s.phase_q12(0), 0);
        assert_eq!(s.phase_q12(1), 4096);
    }

    #[test]
    fn survives_vblank_counter_wraparound() {
        let start = u32::MAX - 2;
        let mut c = FixedClock::new(TickConfig::new(TickRate::HZ60), start);
        assert_eq!(run(&mut c, start.wrapping_add(5)), 6);
        assert_eq!(c.next_due(), start.wrapping_add(6));
    }
}
