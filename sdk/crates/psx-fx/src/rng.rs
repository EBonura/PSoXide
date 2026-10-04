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

    /// Signed integer in `[-range, +range]`, symmetric about zero, sourced from
    /// five bits of the LCG: thirty-two evenly spaced values from `-range` to
    /// `+range`, rounded toward zero. Any `range` is fine, negative included
    /// (the result is then mirrored); the arithmetic is 32-bit, so it does not
    /// overflow for the whole `i16` range.
    #[inline]
    pub fn signed(&mut self, range: i16) -> i16 {
        let r = self.next();
        spread(((r >> 16) & 0x1F) as u8, range)
    }

    /// Current internal state -- useful if a caller wants to save /
    /// restore the RNG across reset boundaries.
    pub const fn state(self) -> u32 {
        self.0
    }
}

/// Map five random bits (`0..=31`) onto `[-range, +range]`: odd multiples of
/// `range / 31` from `-31` to `+31`, so the two ends are exactly `-range` and
/// `+range` and the mapping is odd (`spread(31 - raw) == -spread(raw)`).
const fn spread(raw: u8, range: i16) -> i16 {
    let steps = 2 * (raw as i32) - 31; // -31, -29, ..., 31
    (steps * range as i32 / 31) as i16
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
            assert!((-40..=40).contains(&v), "out of range: {v}");
        }
    }

    #[test]
    fn signed_spans_the_whole_range_and_is_symmetric() {
        // The old mapping gave [-range, 15 * range / 16]: at range = 40 the
        // maximum was 37, and every burst drifted toward -x / -y.
        for range in [1i16, 5, 40, 100, 1000, 2047, 2048, 20_000, i16::MAX] {
            let mut min = i16::MAX;
            let mut max = i16::MIN;
            let mut sum = 0i64;
            for raw in 0..32u8 {
                let v = spread(raw, range);
                assert_eq!(v, -spread(31 - raw, range), "range {range} raw {raw}");
                min = min.min(v);
                max = max.max(v);
                sum += v as i64;
            }
            assert_eq!((min, max), (-range, range), "range {range}");
            assert_eq!(sum, 0, "no net drift at range {range}");
        }
    }

    #[test]
    fn signed_does_not_overflow_on_large_ranges() {
        // (raw - 16) * range overflowed i16 above 2047: a panic in a host
        // debug build, a flipped sign on the console.
        let mut rng = LcgRng::new(9);
        for _ in 0..1000 {
            let v = rng.signed(i16::MAX);
            assert!(i32::from(v).abs() <= i32::from(i16::MAX));
            let w = rng.signed(i16::MIN + 1);
            assert!(i32::from(w).abs() <= i32::from(i16::MAX));
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
