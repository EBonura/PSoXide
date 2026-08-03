//! Deterministic linear-congruential PRNG.
//!
//! Not cryptographically interesting -- perfect for sprinkling
//! variability across particle velocity / enemy-shot cadence /
//! any "looks random but must replay identically" effect. The
//! constants match the venerable `glibc` LCG, which has
//! good-enough statistical properties for game use and produces
//! the same output on every PS1 / host / emulator.

/// 32-bit integer LCG. Step once per `next()` / `signed()` call.
#[derive(Copy, Clone, Debug)]
#[repr(transparent)]
pub struct LcgRng(u32);

impl LcgRng {
    /// Build with an explicit seed. Same seed → same sequence.
    pub const fn new(seed: u32) -> Self {
        Self(seed)
    }

    /// One LCG step. Multiplier + increment are `glibc`'s constants.
    /// Returns the fresh internal state.
    ///
    /// # Take the high bits, not the low ones
    ///
    /// The low bits of any power-of-two LCG are very weak, and bit 0 of this
    /// one is not random at all. The multiplier and the increment are both
    /// odd, so `x' = x*m + c` gives `x'0 = x0 ^ 1`: bit 0 strictly alternates,
    /// period two. Bit `k` has period at most `2^(k+1)`.
    ///
    /// So `next() & 1` ping-pongs, `next() & 127` cycles inside 128 draws, and
    /// `next() % 25` leans on the same weak bits. Use [`LcgRng::next_mixed`]
    /// for anything that masks or takes a remainder, or shift the high half
    /// down yourself as [`LcgRng::signed`] does.
    #[inline]
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1_103_515_245).wrapping_add(12345);
        self.0
    }

    /// One step with the strong high half folded down over the weak low half,
    /// for callers that mask or take a remainder.
    ///
    /// VoXide worked this out and carried it as a private wrapper: its callers
    /// lean on `& 1`, `% 4` and `% 120`, and the raw low bits cycle with tiny
    /// periods. It belongs on the generator rather than in one game.
    #[inline]
    pub fn next_mixed(&mut self) -> u32 {
        let x = self.next();
        x ^ (x >> 16)
    }

    /// Uniform-ish value in `[0, max)`, or 0 when `max` is 0.
    ///
    /// Sourced from [`LcgRng::next_mixed`], so it is safe against the low-bit
    /// weakness a bare `next() % max` walks into.
    #[inline]
    pub fn below(&mut self, max: u32) -> u32 {
        if max == 0 {
            return 0;
        }
        self.next_mixed() % max
    }

    /// Signed integer in roughly `[-range, +range]`, sourced from
    /// five bits of the LCG. Bias is ≤ 1 unit at the extremes,
    /// good enough for cosmetic particle spread.
    #[inline]
    pub fn signed(&mut self, range: i16) -> i16 {
        let r = self.next();
        let raw = ((r >> 16) & 0x1F) as i16; // 0..=31
        (raw - 16) * range / 16
    }

    /// Current internal state -- useful if a caller wants to save /
    /// restore the RNG across reset boundaries.
    pub const fn state(self) -> u32 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_sequence() {
        let mut a = LcgRng::new(0xC0DE_F00D);
        let mut b = LcgRng::new(0xC0DE_F00D);
        for _ in 0..100 {
            assert_eq!(a.next(), b.next());
        }
    }

    #[test]
    fn signed_stays_in_range() {
        let mut rng = LcgRng::new(0xBEEF_0042);
        for _ in 0..10_000 {
            let v = rng.signed(40);
            assert!((-42..=40).contains(&v), "out of range: {v}");
        }
    }

    #[test]
    fn signed_zero_range_is_zero() {
        let mut rng = LcgRng::new(1);
        for _ in 0..100 {
            assert_eq!(rng.signed(0), 0);
        }
    }
}
