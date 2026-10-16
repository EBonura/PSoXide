// SPDX-License-Identifier: GPL-2.0-or-later
//! Exact division of a [`Wide`] / [`WideU`] value, in 32-bit instructions only.
//!
//! [`crate::wide`] leaves division out on purpose: `i64 / i64` links the
//! compiler's `__divdi3` / `u64_div_rem`, which the guest symbol gate refuses.
//! A game that really has a wide numerator (a dot product over a Q12 length, a
//! cross product against a cross product, a tick count over a rate) divides it
//! here instead. Nothing below names a 64-bit integer type: every operation is
//! built from `WideU::mul` (`multu`), 64-bit add / subtract / compare / shift
//! and 32-bit `divu`, so it links no helper. The operations are exact; none
//! estimates or drops low bits.
//!
//! # The operations
//!
//! | operation | divisor | result |
//! |---|---|---|
//! | [`WideU::div_rem_u32`] | `u32` | full 64-bit quotient and the remainder |
//! | [`WideU::quotient`] | [`WideU`] | `floor(n / d)`, clamped to `u32` |
//! | [`Wide::div_i32_wrap`] | `i32` | quotient (a chosen [`Rounding`]) as the low 32 bits |
//! | [`Wide::div_i32_sat`] | `i32` | the same, clamped to the `i32` range |
//! | [`Wide::div_wide_sat`] | [`Wide`] | the same for a wide divisor |
//! | [`WideU::mul_full`] | [`WideU`] | the exact 128-bit product, as two words |
//! | [`WideU::cmp_ratio`] | | order of `a / b` against `c / d`, no divide |
//! | [`WideU::scale_by_ratio`] | [`WideU`] | `floor(a * t / den)` for `t <= den` |
//! | [`Wide::div_pow2`] | `2^shift` | quotient truncated toward zero, still wide |
//!
//! A divisor of zero panics, like `/`.
//!
//! # Cost
//!
//! Measured on the emulator by `sdk/examples/hello-wide-div` (cycles per call,
//! random operands of every magnitude, call and return included). The table is
//! in that example's header and in `docs`; the shape is: a numerator that fits
//! 32 bits is one native `divu`; otherwise a high-word `divu` plus the two-digit
//! [`crate::int32::div_u64_by_u32`]; a divisor wider than 32 bits adds one
//! `div_u64_by_u32` for the estimate and a 96-bit product and compare to settle
//! it. The division bodies are `#[inline(never)]`: one shared copy per image,
//! because an inlined copy is several hundred bytes.

use crate::int32::div_u64_by_u32;
use crate::wide::{Wide, WideU};
use core::cmp::Ordering;

/// How an inexact quotient is rounded to an integer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rounding {
    /// Toward zero, like Rust's `/`.
    TowardZero,
    /// Toward negative infinity, like `div_euclid` for a positive divisor.
    Floor,
    /// To the nearest integer; an exact half goes away from zero.
    Nearest,
}

/// `floor(n / d)` and `n - floor(n / d) * d` for a divisor that fits 32 bits.
#[inline(never)]
fn divrem_word(n: WideU, d: u32) -> (WideU, u32) {
    assert!(d != 0, "attempt to divide by zero");
    if n.high() == 0 {
        // One native divide (the compiler fuses the remainder): the common case.
        return (WideU::of(n.low() / d), n.low() % d);
    }
    // n = (hi * 2^32 + lo). hi = qh * d + rh; the low quotient word is
    // (rh * 2^32 + lo) / d, whose high part rh < d is the shape the two-digit
    // divide needs, and the remainder is what is left of that low part.
    let rh = n.high() % d;
    let q_low = div_u64_by_u32(rh, n.low(), d);
    let rest = WideU::from_parts(rh, n.low()).sub(WideU::mul(q_low, d));
    (WideU::from_parts(n.high() / d, q_low), rest.low())
}

/// `q * d` as (bits 64.., bits 0..64) for a 32-bit `q`.
#[inline(always)]
fn mul_word_wide(q: u32, d: WideU) -> (u32, WideU) {
    let low = WideU::mul(q, d.low());
    let high = WideU::mul(q, d.high());
    // high * 2^32 + low: the low word of `high` joins at bit 32, its high word
    // is bits 64.., plus the carry out of that add.
    let sum = low.add(WideU::from_parts(high.low(), 0));
    let carry = u32::from(sum < low);
    (high.high() + carry, sum)
}

/// Steps `q` down to the largest value with `q * d <= top * 2^64 + low` (a 96-bit
/// numerator), from an estimate that is too large. Returns `q` and `q * d` as
/// (bits 64.., bits 0..64). Correctness does not depend on how far off the estimate
/// is, only the loop count does.
#[inline(always)]
fn settle_down(mut q: u32, top: u32, low: WideU, d: WideU) -> (u32, u32, WideU) {
    let (mut over, mut product) = mul_word_wide(q, d);
    while over > top || (over == top && product > low) {
        q -= 1;
        let next = product.sub(d);
        over -= u32::from(product < d);
        product = next;
        #[cfg(test)]
        tests::step(true);
    }
    (q, over, product)
}

/// `floor(n / d)` and the remainder for `d >= 2^32`, so the quotient fits `u32`.
#[inline(never)]
fn divrem_wide(n: WideU, d: WideU) -> (u32, WideU) {
    // The top 32 bits of d (bit 31 is set) and the same shift of n give one two-digit
    // divide. With n' = floor(n / 2^s) and d' = floor(d / 2^s) the estimate
    // floor(n' / d') is never below the true quotient q (n >= q d, so
    // n' >= floor(q d / 2^s) >= q d'), and the cut-off bits make it one too high at
    // most in practice (two is the bound), so only the downward settle is needed.
    let shift = 32 - d.high().leading_zeros();
    let narrow = d.shr_by(shift).low();
    let top = n.shr_by(shift);
    // top.high() < 2^(32 - shift) <= 2^31 <= narrow: the two-digit divide's precondition.
    let estimate = div_u64_by_u32(top.high(), top.low(), narrow);
    let (q, _, product) = settle_down(estimate, 0, n, d);
    // q * d <= n < 2^64: the product fits the low pair and nothing wraps.
    (q, n.sub(product))
}

/// The quotient magnitude of `|n| / |d|` after rounding, for the signed forms.
///
/// `negative` is the sign of the true quotient. One shared copy for every rounding mode
/// and both divisor widths.
#[inline(never)]
fn rounded_magnitude(n: WideU, d: WideU, negative: bool, round: Rounding) -> WideU {
    if n.high() == 0 && d.high() == 0 {
        // Both fit a word: one native divide and 32-bit rounding. The common case.
        let (n, d) = (n.low(), d.low());
        assert!(d != 0, "attempt to divide by zero");
        let (q, rest) = (n / d, n % d);
        let up = match round {
            Rounding::TowardZero => false,
            Rounding::Floor => negative && rest != 0,
            Rounding::Nearest => rest >= d - rest,
        };
        // q + 1 cannot wrap: a rounded-up quotient has d >= 2.
        return WideU::of(q + u32::from(up));
    }
    let (q, rest) = if d.high() == 0 {
        let (q, r) = divrem_word(n, d.low());
        (q, WideU::of(r))
    } else {
        let (q, r) = divrem_wide(n, d);
        (WideU::of(q), r)
    };
    let up = match round {
        Rounding::TowardZero => false,
        Rounding::Floor => negative && !rest.is_zero(),
        // rest * 2 >= d, without the doubling that could overflow.
        Rounding::Nearest => rest >= d.sub(rest),
    };
    if up {
        q.add(WideU::of(1))
    } else {
        q
    }
}

/// The magnitude of `x` as an unsigned value (`Wide::MIN` gives `2^63`).
#[inline(always)]
fn magnitude(x: Wide) -> WideU {
    let a = x.abs();
    WideU::from_parts(a.high() as u32, a.low_u32())
}

impl WideU {
    /// `(self / d, self % d)`: the exact 64-bit quotient and the remainder.
    ///
    /// A numerator below 2^32 is one `divu`. Panics if `d` is zero.
    #[inline]
    pub fn div_rem_u32(self, d: u32) -> (WideU, u32) {
        divrem_word(self, d)
    }

    /// `floor(self / d)`, clamped to `u32::MAX` when the true quotient is
    /// larger. Exact: no estimate survives to the result. Panics if `d` is zero.
    ///
    /// A divisor wider than 32 bits makes the quotient fit `u32` by itself; the
    /// clamp only matters for a divisor that fits 32 bits.
    #[inline]
    pub fn quotient(self, d: WideU) -> u32 {
        assert!(!d.is_zero(), "attempt to divide by zero");
        if d.high() == 0 {
            divrem_word(self, d.low()).0.saturate_u32()
        } else {
            divrem_wide(self, d).0
        }
    }

    /// The exact 128-bit product of two values, as `(bits 64..128, bits 0..64)`.
    #[inline(never)]
    pub fn mul_full(self, o: WideU) -> (WideU, WideU) {
        let low = WideU::mul(self.low(), o.low());
        let cross_a = WideU::mul(self.low(), o.high());
        let cross_b = WideU::mul(self.high(), o.low());
        let high = WideU::mul(self.high(), o.high());
        // Neither sum below wraps before the second: each stays under 2^64 until the
        // second, whose carry is read off the compare.
        let first = cross_a.add(WideU::of(low.high()));
        let middle = first.add(cross_b);
        let carry = u32::from(middle < first);
        (
            high.add(WideU::from_parts(carry, middle.high())),
            WideU::from_parts(middle.low(), low.low()),
        )
    }

    /// Orders `a / b` against `c / d` without dividing: `a * d` against `c * b`,
    /// each an exact 128-bit product. `b` and `d` must be nonzero.
    #[inline(never)]
    pub fn cmp_ratio(a: WideU, b: WideU, c: WideU, d: WideU) -> Ordering {
        debug_assert!(!b.is_zero() && !d.is_zero());
        a.mul_full(d).cmp(&c.mul_full(b))
    }

    /// `floor(a * self / den)` for `self <= den` (a fraction of at most one), so the
    /// result is at most `a`. Exact. Panics if `den` is zero or `self > den`.
    #[inline(never)]
    pub fn scale_by_ratio(self, a: u32, den: WideU) -> u32 {
        assert!(!den.is_zero() && self <= den, "ratio outside [0, 1]");
        if den.high() == 0 {
            // self <= den < 2^32: the product is one multu.
            return WideU::mul(a, self.low()).quotient(den);
        }
        // The estimate divides by the top 32 bits of den: self >> shift fits 32 bits
        // because self <= den, and the quotient is at most a.
        let shift = 32 - den.high().leading_zeros();
        let narrow = den.shr_by(shift).low();
        let estimate = crate::int32::mul_div_u32(a, self.shr_by(shift).low(), narrow).min(a);
        let (high, low) = WideU::of(a).mul_full(self);
        let top = high.low();
        // The estimate cuts both numerator and denominator, so it can land either side.
        let (mut q, mut over, mut product) = settle_down(estimate, top, low, den);
        while q < a {
            let next = product.add(den);
            let carried = over + u32::from(next < product);
            if carried > top || (carried == top && next > low) {
                break;
            }
            q += 1;
            over = carried;
            product = next;
            #[cfg(test)]
            tests::step(false);
        }
        q
    }
}

impl Wide {
    /// `self / 2^shift`, truncated toward zero, for `shift < 63`: the bias and
    /// arithmetic shift a compiler emits for `i64 / (1 << shift)`.
    #[inline(always)]
    pub const fn div_pow2(self, shift: u32) -> Wide {
        if self.is_negative() {
            self.add_w(Wide::ONE.shl_by(shift).plus(-1)).sar(shift)
        } else {
            self.sar(shift)
        }
    }

    /// `self / d` rounded as asked, as the low 32 bits of the quotient: where the
    /// quotient does not fit `i32` it wraps, exactly like `(n / d) as i32`.
    ///
    /// Exact for every `self` and every nonzero `d`; panics if `d` is zero.
    #[inline]
    pub fn div_i32_wrap(self, d: i32, round: Rounding) -> i32 {
        let negative = self.is_negative() != (d < 0);
        let q = rounded_magnitude(
            magnitude(self),
            WideU::of(d.unsigned_abs()),
            negative,
            round,
        )
        .low();
        if negative {
            q.wrapping_neg() as i32
        } else {
            q as i32
        }
    }

    /// `self / d` rounded as asked and clamped to the `i32` range. Panics if `d` is zero.
    #[inline]
    pub fn div_i32_sat(self, d: i32, round: Rounding) -> i32 {
        let negative = self.is_negative() != (d < 0);
        let q = rounded_magnitude(
            magnitude(self),
            WideU::of(d.unsigned_abs()),
            negative,
            round,
        );
        clamp_signed(negative, q)
    }

    /// `self / d` for a wide divisor, rounded as asked and clamped to the `i32` range.
    /// Panics if `d` is zero.
    #[inline]
    pub fn div_wide_sat(self, d: Wide, round: Rounding) -> i32 {
        let negative = self.is_negative() != d.is_negative();
        let q = rounded_magnitude(magnitude(self), magnitude(d), negative, round);
        clamp_signed(negative, q)
    }
}

/// A signed quotient from its sign and magnitude, clamped to `i32`.
#[inline(always)]
fn clamp_signed(negative: bool, magnitude: WideU) -> i32 {
    if negative {
        if magnitude > WideU::of(1 << 31) {
            i32::MIN
        } else {
            (magnitude.low() as i32).wrapping_neg()
        }
    } else if magnitude > WideU::of(i32::MAX as u32) {
        i32::MAX
    } else {
        magnitude.low() as i32
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    /// xorshift64*, the same stream on every run.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }
        /// A value of random bit length, so every magnitude occurs.
        fn magnitude(&mut self) -> u64 {
            let v = self.next();
            v >> (self.next() % 64)
        }
        fn word(&mut self) -> u32 {
            self.magnitude() as u32
        }
        fn signed(&mut self) -> i64 {
            let m = self.magnitude() as i64;
            if self.next() & 1 == 0 {
                m
            } else {
                m.wrapping_neg()
            }
        }
    }

    fn wu(x: u64) -> WideU {
        WideU::from_parts((x >> 32) as u32, x as u32)
    }
    fn uw(x: WideU) -> u64 {
        (u64::from(x.high()) << 32) | u64::from(x.low())
    }
    fn ws(x: i64) -> Wide {
        Wide::from_parts((x >> 32) as i32, x as u32)
    }

    const CASES: u32 = 1_500_000;

    /// Edge values of a 64-bit magnitude: around every word boundary.
    fn edges() -> std::vec::Vec<u64> {
        let mut v = std::vec![0, 1, 2, 3, u64::MAX, u64::MAX - 1, 1 << 63, (1 << 63) - 1];
        for s in [16u32, 31, 32, 33, 48, 62] {
            for d in [-2i64, -1, 0, 1, 2] {
                v.push(((1u64 << s) as i64 + d) as u64);
            }
        }
        v
    }

    /// The reference rounding, on i128.
    fn oracle(n: i128, d: i128, round: Rounding) -> i128 {
        let (q, r) = (n / d, n % d);
        match round {
            Rounding::TowardZero => q,
            Rounding::Floor => {
                if r != 0 && ((n < 0) != (d < 0)) {
                    q - 1
                } else {
                    q
                }
            }
            Rounding::Nearest => {
                // Round half away from zero: |n| / |d| + 1/2 on the magnitude.
                let (an, ad) = (n.abs(), d.abs());
                let m = (2 * an + ad) / (2 * ad);
                if (n < 0) != (d < 0) {
                    -m
                } else {
                    m
                }
            }
        }
    }
    const ROUNDS: [Rounding; 3] = [Rounding::TowardZero, Rounding::Floor, Rounding::Nearest];

    #[test]
    fn div_rem_u32_matches() {
        let mut r = Rng(0x1234_5678_9abc_def1);
        let check = |n: u64, d: u32| {
            let (q, rem) = wu(n).div_rem_u32(d);
            assert_eq!(
                (uw(q), u64::from(rem)),
                (n / u64::from(d), n % u64::from(d)),
                "{n} / {d}"
            );
        };
        for &n in &edges() {
            for d in [1u32, 2, 3, 0x7fff_ffff, 0x8000_0000, u32::MAX, 0x1_0001 - 2] {
                check(n, d);
            }
        }
        for _ in 0..CASES {
            check(r.magnitude(), r.word().max(1));
        }
    }

    #[test]
    fn quotient_matches_for_every_divisor_width() {
        let mut r = Rng(0x0bad_5eed_1234_5678);
        let check = |n: u64, d: u64| {
            let want = (u128::from(n) / u128::from(d)).min(u128::from(u32::MAX)) as u32;
            assert_eq!(wu(n).quotient(wu(d)), want, "{n} / {d}");
        };
        let e = edges();
        for &n in &e {
            for &d in e.iter().filter(|d| **d != 0) {
                check(n, d);
            }
        }
        for _ in 0..CASES {
            check(r.magnitude(), r.magnitude().max(1));
        }
        // Quotients that are exact multiples and one short of one, the settle edge.
        for _ in 0..CASES {
            let d = r.magnitude().max(1);
            let q = u64::from(r.word());
            let Some(m) = d.checked_mul(q) else { continue };
            check(m, d);
            check(m.saturating_add(d - 1), d);
            check(m.saturating_sub(1), d);
        }
    }

    #[test]
    fn signed_division_matches_i128() {
        let mut r = Rng(0x9e37_79b9_7f4a_7c15);
        let mut edge_n: std::vec::Vec<i64> = edges()
            .iter()
            .flat_map(|&u| [u as i64, (u as i64).wrapping_neg()])
            .collect();
        edge_n.dedup();
        let edge_d: [i32; 11] = [
            1,
            -1,
            2,
            -2,
            3,
            -3,
            32767,
            0x7fff_ffff,
            i32::MIN,
            409_600,
            -409_600,
        ];
        let check = |n: i64, d: i32, round: Rounding| {
            let q = oracle(i128::from(n), i128::from(d), round);
            assert_eq!(
                ws(n).div_i32_wrap(d, round),
                q as i32,
                "wrap {n} / {d} {round:?}"
            );
            let sat = q.clamp(i128::from(i32::MIN), i128::from(i32::MAX)) as i32;
            assert_eq!(ws(n).div_i32_sat(d, round), sat, "sat {n} / {d} {round:?}");
        };
        for &n in &edge_n {
            for &d in &edge_d {
                for round in ROUNDS {
                    check(n, d, round);
                }
            }
        }
        for _ in 0..CASES {
            let n = r.signed();
            let d = r.signed() as i32;
            if d == 0 {
                continue;
            }
            check(n, d, ROUNDS[(r.next() % 3) as usize]);
        }
    }

    #[test]
    fn wide_divisor_matches_i128() {
        let mut r = Rng(0x2468_ace0_1357_9bdf);
        let check = |n: i64, d: i64, round: Rounding| {
            let q = oracle(i128::from(n), i128::from(d), round);
            let sat = q.clamp(i128::from(i32::MIN), i128::from(i32::MAX)) as i32;
            assert_eq!(ws(n).div_wide_sat(ws(d), round), sat, "{n} / {d} {round:?}");
        };
        let e: std::vec::Vec<i64> = edges()
            .iter()
            .flat_map(|&u| [u as i64, (u as i64).wrapping_neg()])
            .collect();
        for &n in &e {
            for &d in e.iter().filter(|d| **d != 0) {
                for round in ROUNDS {
                    check(n, d, round);
                }
            }
        }
        for _ in 0..CASES {
            let d = r.signed();
            if d == 0 {
                continue;
            }
            // Bias toward quotients that fit, the case that matters.
            let n = if r.next() % 2 == 0 {
                r.signed()
            } else {
                let q = i64::from(r.word() as i32);
                q.wrapping_mul(d).wrapping_add((r.next() % 5) as i64 - 2)
            };
            check(n, d, ROUNDS[(r.next() % 3) as usize]);
        }
    }

    #[test]
    fn div_pow2_matches() {
        let mut r = Rng(77);
        for _ in 0..CASES {
            let n = r.signed();
            let s = (r.next() % 63) as u32;
            assert_eq!(ws(n).div_pow2(s), ws(n / (1i64 << s)), "{n} / 2^{s}");
        }
        assert_eq!(ws(-1).div_pow2(5), ws(0));
        assert_eq!(ws(-33).div_pow2(5), ws(-1));
    }

    #[test]
    fn mul_full_matches_u128() {
        let mut r = Rng(31337);
        let e = edges();
        for &a in &e {
            for &b in &e {
                let (h, l) = wu(a).mul_full(wu(b));
                assert_eq!(
                    (u128::from(uw(h)) << 64) | u128::from(uw(l)),
                    u128::from(a) * u128::from(b),
                    "{a} {b}"
                );
            }
        }
        for _ in 0..CASES {
            let (a, b) = (r.magnitude(), r.magnitude());
            let (h, l) = wu(a).mul_full(wu(b));
            assert_eq!(
                (u128::from(uw(h)) << 64) | u128::from(uw(l)),
                u128::from(a) * u128::from(b),
                "{a} {b}"
            );
        }
    }

    #[test]
    fn cmp_ratio_orders_like_u128() {
        let mut r = Rng(12345);
        for _ in 0..CASES {
            let (a, c) = (r.magnitude(), r.magnitude());
            let (b, d) = (r.magnitude().max(1), r.magnitude().max(1));
            let want = (u128::from(a) * u128::from(d)).cmp(&(u128::from(c) * u128::from(b)));
            assert_eq!(WideU::cmp_ratio(wu(a), wu(b), wu(c), wu(d)), want);
            assert_eq!(
                WideU::cmp_ratio(wu(a), wu(b), wu(a), wu(b)),
                Ordering::Equal
            );
        }
        // Equal ratios with different terms, and neighbours of them.
        for _ in 0..CASES {
            let (b, k) = (u64::from(r.word()).max(1), u64::from(r.word()).max(1));
            let a = u64::from(r.word()).min(b);
            let (c, d) = (a * k, b * k);
            assert_eq!(
                WideU::cmp_ratio(wu(a), wu(b), wu(c), wu(d)),
                Ordering::Equal
            );
            assert_eq!(
                WideU::cmp_ratio(wu(a + 1), wu(b), wu(c), wu(d)),
                Ordering::Greater
            );
        }
    }

    #[test]
    fn scale_by_ratio_matches_u128() {
        let mut r = Rng(99);
        let check = |a: u32, t: u64, den: u64| {
            let want = (u128::from(a) * u128::from(t) / u128::from(den)) as u32;
            assert_eq!(wu(t).scale_by_ratio(a, wu(den)), want, "{a} * {t} / {den}");
        };
        for _ in 0..CASES {
            let den = r.magnitude().max(1);
            let t = match r.next() % 4 {
                0 => den,
                1 => 0,
                _ => (u128::from(r.next()) % (u128::from(den) + 1)) as u64,
            };
            check(r.word(), t, den);
        }
        for &den in edges().iter().filter(|d| **d != 0) {
            for t in [0, 1, den / 2, den - 1, den] {
                for a in [0, 1, 255, u32::MAX] {
                    check(a, t, den);
                }
            }
        }
    }

    std::thread_local! {
        static STEPS: core::cell::Cell<(u32, u32)> = const { core::cell::Cell::new((0, 0)) };
    }
    pub(super) fn step(down: bool) {
        STEPS.with(|c| {
            let (d, u) = c.get();
            c.set(if down { (d + 1, u) } else { (d, u + 1) });
        });
    }

    /// The estimate from the top 32 bits of the divisor is a couple off at most: count the
    /// steps `settle` takes and bound them per call and on average.
    #[test]
    fn settle_loops_stay_short() {
        let mut r = Rng(5150);
        let calls = 400_000u32;
        let (mut worst, mut down_total, mut up_total) = (0u32, 0u32, 0u32);
        for _ in 0..calls {
            let d = (r.magnitude() | (1 << 32)).max(1 << 32);
            let n = r.next();
            STEPS.with(|c| c.set((0, 0)));
            let q = wu(n).quotient(wu(d));
            assert_eq!(u64::from(q), n / d);
            let (down, up) = STEPS.with(|c| c.get());
            worst = worst.max(down + up);
            down_total += down;
            up_total += up;
        }
        assert!(worst <= 2, "settle took {worst} steps in one call");
        std::eprintln!(
            "quotient steps over {calls} calls: down {down_total}, up {up_total}, worst {worst}"
        );
        let (mut worst, mut down_total, mut up_total) = (0u32, 0u32, 0u32);
        for _ in 0..calls {
            let den = (r.magnitude() | (1 << 32)).max(1 << 32);
            let t = (u128::from(r.next()) % (u128::from(den) + 1)) as u64;
            STEPS.with(|c| c.set((0, 0)));
            let _ = wu(t).scale_by_ratio(r.word(), wu(den));
            let (down, up) = STEPS.with(|c| c.get());
            worst = worst.max(down + up);
            down_total += down;
            up_total += up;
        }
        assert!(worst <= 3, "scale_by_ratio took {worst} steps in one call");
        std::eprintln!(
            "scale steps over {calls} calls: down {down_total}, up {up_total}, worst {worst}"
        );
    }

    #[test]
    #[should_panic(expected = "divide by zero")]
    fn zero_divisor_panics() {
        let _ = ws(5).div_i32_wrap(0, Rounding::TowardZero);
    }
}
