// SPDX-License-Identifier: GPL-2.0-or-later
//! How long the polled CD reader waits: bounds in time, not in spins.
//!
//! A spin count is a time only through the cost of one spin, which nothing
//! measured on the console and which moves whenever the loop body does (code
//! generation, an idle hook, a slower register). The drive is mechanical: after
//! the pause of an idle drive the first sector has been seen to take 4.4 s
//! (hardware tests v2.7, record 0x312). So every wait of
//! [`SectorReader`](super::reader::SectorReader) is bounded by elapsed system
//! clocks, read from root counter 2.
//!
//! What the SDK guarantees about counter 2, and what the reader assumes: the
//! runtime's entry state leaves it free-running on the system clock
//! (`psx_rt::entry`, mode 0); `psx-cdstream` and `psx-fmv` switch it to system
//! clock / 8 while they run. The reader never writes it. It reads the counter
//! once per spin and counts every step as that many system clocks, the fastest
//! rate the counter can run. So:
//!
//! * at the entry rate a wait lasts its bound;
//! * at clock / 8, or on the HBlank or dot clock a game may choose, it lasts
//!   longer (up to 8 times at clock / 8): never shorter, so a slow but healthy
//!   drive cannot be cut off by another owner of the counter;
//! * a counter a game has given a short target (reset at target) wraps early,
//!   which reads as one huge step: a step is counted at most [`MAX_STEP`]
//!   clocks, so a wrap cannot end a wait early either;
//! * a stopped counter adds nothing, so a spin backstop bounds every wait
//!   regardless: at most one spin per [`MIN_SPIN_CLOCKS`] clocks of the bound.
//!   A real spin costs several times that (CD register reads alone are about 12
//!   clocks each on the console), so the backstop only ever lengthens a wait, and
//!   it ends one whose clock has stopped.
//!
//! No VBlank count is needed as a second clock: the backstop already bounds a
//! wait whose counter does not move, and the VBlank count lives above this crate
//! (in `psx-rt`) and stops with interrupts.

use crate::timers::{self, Timer};

/// System clocks a millisecond, rounded up (33,868,800 Hz).
const CLOCKS_PER_MS: u32 = 33_869;

/// The most one spin's counter step counts, in system clocks. Far more than a
/// spin costs, even one that runs the reader's idle hook (a pad poll is a few
/// thousand clocks), and far less than a whole wrap of the counter.
pub const MAX_STEP: u32 = 0x4000;

/// The fewest system clocks one spin of a reader wait can take: the backstop
/// allows one spin per this many clocks of the bound.
pub const MIN_SPIN_CLOCKS: u32 = 8;

/// How long a CD wait may last, in system clocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct WaitTime(u32);

impl WaitTime {
    /// `ms` milliseconds (at most 126,000: the bound is a 32-bit clock count).
    #[must_use]
    pub const fn from_millis(ms: u32) -> Self {
        Self(ms.saturating_mul(CLOCKS_PER_MS))
    }

    /// `clocks` system clocks.
    #[must_use]
    pub const fn from_clocks(clocks: u32) -> Self {
        Self(clocks)
    }

    /// The bound in system clocks.
    #[must_use]
    pub const fn clocks(self) -> u32 {
        self.0
    }

    /// The bound in whole milliseconds.
    #[must_use]
    pub const fn millis(self) -> u32 {
        self.0 / CLOCKS_PER_MS
    }
}

/// A command's acknowledge (INT3): under a millisecond normally.
pub const ACK_WAIT: WaitTime = WaitTime::from_millis(200);
/// A seek's completion, or a read's first sector, which carries the seek: the
/// console has taken 4.4 s after a pause and 15 s idle, so twice that and more.
pub const FIRST_SECTOR_WAIT: WaitTime = WaitTime::from_millis(10_000);
/// The sectors after the first: 6.7 ms each at double speed.
pub const NEXT_SECTOR_WAIT: WaitTime = WaitTime::from_millis(2_000);
/// The data FIFO filling after a sector's request: microseconds.
pub const FIFO_WAIT: WaitTime = WaitTime::from_millis(100);

/// The time one wait has used: counter steps and spins.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Elapsed {
    last: u16,
    clocks: u32,
    spins: u32,
}

impl Elapsed {
    /// Start counting from the counter's value `now`.
    pub(crate) const fn starting_at(now: u16) -> Self {
        Self {
            last: now,
            clocks: 0,
            spins: 0,
        }
    }

    /// Start counting from root counter 2 as it reads now.
    #[inline]
    pub(crate) fn start() -> Self {
        Self::starting_at(timers::counter(Timer::Timer2))
    }

    /// One spin, the counter reading `now`. `true` once `limit` is used up.
    pub(crate) fn step_to(&mut self, now: u16, limit: WaitTime) -> bool {
        let step = u32::from(now.wrapping_sub(self.last)).min(MAX_STEP);
        self.last = now;
        self.clocks = self.clocks.saturating_add(step);
        self.spins = self.spins.saturating_add(1);
        self.clocks >= limit.0 || self.spins >= limit.0 / MIN_SPIN_CLOCKS
    }

    /// One spin, reading root counter 2. `true` once `limit` is used up.
    #[inline]
    pub(crate) fn step(&mut self, limit: WaitTime) -> bool {
        self.step_to(timers::counter(Timer::Timer2), limit)
    }

    /// Spins taken so far.
    pub(crate) const fn spins(&self) -> u32 {
        self.spins
    }
}

/// Ask `ready` once a spin until it says yes or `limit` is used up, the time
/// read from root counter 2. `Err(spins)` when the wait ran out.
#[inline(always)]
pub(crate) fn poll_until(ready: impl FnMut() -> bool, limit: WaitTime) -> Result<(), u32> {
    poll_until_with(ready, || timers::counter(Timer::Timer2), limit)
}

/// [`poll_until`] with the counter read from `now`, so a host test can drive it.
#[inline(always)]
pub(crate) fn poll_until_with(
    mut ready: impl FnMut() -> bool,
    mut now: impl FnMut() -> u16,
    limit: WaitTime,
) -> Result<(), u32> {
    let mut elapsed = Elapsed::starting_at(now());
    loop {
        if ready() {
            return Ok(());
        }
        if elapsed.step_to(now(), limit) {
            return Err(elapsed.spins());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bounds_are_the_times_asked_for() {
        assert_eq!(ACK_WAIT.millis(), 200);
        assert_eq!(FIRST_SECTOR_WAIT.millis(), 10_000);
        assert_eq!(NEXT_SECTOR_WAIT.millis(), 2_000);
        // More than twice the slowest first sector the console has shown.
        assert!(FIRST_SECTOR_WAIT.clocks() > 2 * 149_000_000);
    }

    #[test]
    fn a_counter_on_the_system_clock_ends_the_wait_at_its_bound() {
        let limit = WaitTime::from_clocks(100_000);
        let mut elapsed = Elapsed::starting_at(0);
        let mut now = 0u16;
        let mut spins = 0u32;
        // 50 clocks a spin, wrapping the 16-bit counter many times.
        loop {
            now = now.wrapping_add(50);
            spins += 1;
            if elapsed.step_to(now, limit) {
                break;
            }
        }
        assert_eq!(spins, 2_000);
    }

    #[test]
    fn a_slower_counter_only_makes_the_wait_longer() {
        // Clock / 8: 50 clocks a spin is 6 counts.
        let limit = WaitTime::from_clocks(100_000);
        let mut elapsed = Elapsed::starting_at(0);
        let mut now = 0u16;
        let mut spins = 0u32;
        while !elapsed.step_to(now, limit) {
            now = now.wrapping_add(6);
            spins += 1;
        }
        // Counted at 6 a spin the bound would take 16,667 spins; the backstop (one spin per
        // 8 clocks of the bound) ends it at 12,500, still over six times the bound at 50 a spin.
        assert_eq!(spins + 1, 12_500);
        assert!(spins * 50 > 6 * 100_000);
    }

    #[test]
    fn an_early_wrap_cannot_end_the_wait() {
        // A counter reset at a target of 100: a wrap reads as a step of
        // 65,536 - 100 or so; it counts as MAX_STEP at most.
        let limit = WaitTime::from_clocks(1_000_000);
        let mut elapsed = Elapsed::starting_at(90);
        assert!(!elapsed.step_to(5, limit));
        assert!(elapsed.clocks <= MAX_STEP);
    }

    #[test]
    fn a_stopped_counter_is_ended_by_the_spin_backstop() {
        let limit = WaitTime::from_clocks(80_000);
        let mut elapsed = Elapsed::starting_at(1234);
        let mut spins = 0u32;
        while !elapsed.step_to(1234, limit) {
            spins += 1;
        }
        assert_eq!(spins + 1, 80_000 / MIN_SPIN_CLOCKS);
        assert_eq!(elapsed.spins(), 10_000);
    }

    #[test]
    fn a_fifo_that_never_fills_is_a_timeout_not_a_sector() {
        // The reader pops a sector only on Ok: a FIFO that never reports data
        // must come back as Err, after the bound, never as a ready FIFO.
        let mut asked = 0u32;
        let mut now = 0u16;
        let waited = poll_until_with(
            || {
                asked += 1;
                false
            },
            || {
                now = now.wrapping_add(50);
                now
            },
            FIFO_WAIT,
        );
        assert_eq!(waited, Err(FIFO_WAIT.clocks().div_ceil(50)));
        assert_eq!(asked, FIFO_WAIT.clocks().div_ceil(50), "one look a spin");
    }

    #[test]
    fn a_fifo_that_fills_ends_the_wait_at_once() {
        let mut asked = 0u32;
        let waited = poll_until_with(
            || {
                asked += 1;
                asked == 3
            },
            || 0,
            FIFO_WAIT,
        );
        assert_eq!(waited, Ok(()));
        assert_eq!(asked, 3);
    }
}
