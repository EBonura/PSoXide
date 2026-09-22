//! Saturating 32-bit integer scalar helpers.
//!
//! The PS1 has no FPU and the engine keeps all gameplay math in
//! i32/i16 fixed-point, so the same small scalar helpers (absolute
//! value that cannot overflow at `MIN`, a clamp into the GTE's i16
//! vertex range, an integer square root for vector lengths) get
//! re-implemented next to every consumer. This module owns them
//! once. Every function is branch-light, allocation-free, and safe
//! on the full input domain:
//!
//! - `abs_*` saturate `MIN` to `MAX` instead of overflowing.
//! - [`square_i32_saturating`] returns `i32::MAX` once the true
//!   square would exceed i32.
//! - [`isqrt_i32`] is the classic digit-by-digit (binary restoring)
//!   integer square root: exact floor(sqrt(n)), no multiplies.
//! - [`mul_q12_i32`] multiplies an i32 by a Q1.12 factor without a
//!   64-bit intermediate, splitting whole and fractional parts so
//!   the products stay inside i32.
//! - [`mul_div_i32`], [`mul_shr_i32`] and [`mul_shr_trunc_i32`] are the
//!   signed `a * b / c` forms games otherwise spell as `i64` expressions
//!   (which link `__divdi3`): bit-identical results, native `mult`/`divu`.
//! - [`isqrt_u64`] and [`div_u64_by_u32`] give full-width roots and
//!   64-by-32 quotients in 32-bit operations.

/// Absolute value of an `i32`, saturating `i32::MIN` to `i32::MAX`.
#[inline]
pub fn abs_i32(value: i32) -> i32 {
    if value == i32::MIN {
        i32::MAX
    } else if value < 0 {
        -value
    } else {
        value
    }
}

/// Absolute value of an `i16`, saturating `i16::MIN` to `i16::MAX`.
#[inline]
pub fn abs_i16(value: i16) -> i16 {
    if value == i16::MIN {
        i16::MAX
    } else if value < 0 {
        -value
    } else {
        value
    }
}

/// Clamp an `i32` into the `i16` range (the GTE's vertex domain).
#[inline]
pub fn clamp_i16(value: i32) -> i16 {
    value.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

/// `value * value`, saturating to `i32::MAX` when the square would
/// overflow (|value| > 46340, the largest i32 whose square fits).
#[inline]
pub fn square_i32_saturating(value: i32) -> i32 {
    let abs = abs_i32(value);
    if abs > 46_340 {
        return i32::MAX;
    }
    abs * abs
}

/// Integer square root: exact `floor(sqrt(value))` for positive
/// inputs, `0` for zero and negative inputs.
///
/// Digit-by-digit (binary restoring) method: shifts and adds only,
/// no multiplies or divides, so it is cheap on the R3000.
#[inline]
pub fn isqrt_i32(value: i32) -> i32 {
    if value <= 0 {
        return 0;
    }
    isqrt_u32(value as u32) as i32
}

/// Exact floor square root across the complete unsigned 32-bit domain.
#[inline]
pub fn isqrt_u32(mut value: u32) -> u32 {
    let mut root = 0u32;
    let mut bit = 1u32 << 30;
    while bit > value {
        bit >>= 2;
    }
    while bit != 0 {
        if value >= root + bit {
            value -= root + bit;
            root = (root >> 1) + bit;
        } else {
            root >>= 1;
        }
        bit >>= 2;
    }
    root
}

/// Floor square roots of `0..=255`, the seed of [`isqrt_u64`].
const ROOT8: [u8; 256] = {
    let mut out = [0u8; 256];
    let mut n = 0;
    while n < 256 {
        let mut r = 0;
        while (r + 1) * (r + 1) <= n {
            r += 1;
        }
        out[n] = r as u8;
        n += 1;
    }
    out
};

/// Exact floor square root of `high * 2^32 + low` for `high < 2^16`
/// (inputs below `2^48`), in 32-bit operations only.
///
/// A 256-entry table seeds the root of the high byte, two-bit restoring
/// steps finish the high word, then eight steps consume the low word four
/// bits at a time. The invariant is `prefix = root^2 + remainder` with
/// `0 <= remainder <= 2 * root`. A nibble selects `4 * root + [0..3]`: test
/// the `+2` threshold (`16 * root + 4`), then `+1` against the updated
/// root. The root stays below `2^24`, so the expanded remainder is below
/// `2^29` and the threshold below `2^28`, and the sign bit of their wrapping
/// difference is the exact comparison. Ported from Hollow Knight's
/// `integer_root`. Through it, [`isqrt_u64`] below `2^48` takes 218 cycles a
/// call on the emulator microbench against 908 for the 32-step `u64`
/// restoring loop it replaced.
#[inline(always)]
const fn isqrt_below_2_48(high: u32, low: u32) -> u32 {
    debug_assert!(high >> 16 == 0);
    let mut root = if high < 256 {
        ROOT8[high as usize] as u32
    } else {
        let mut r = ROOT8[(high >> 8) as usize] as u32;
        let mut remainder = (high >> 8) - r * r;
        let mut shift = 8;
        while shift != 0 {
            shift -= 2;
            remainder = (remainder << 2) | ((high >> shift) & 3);
            let trial = (r << 2) | 1;
            let take = (remainder >= trial) as u32;
            remainder -= trial & take.wrapping_neg();
            r = (r << 1) | take;
        }
        r
    };
    let mut remainder = high - root * root;
    let mut low = low;
    let mut step = 0;
    while step < 8 {
        step += 1;
        remainder = (remainder << 4) | (low >> 28);
        low <<= 4;
        let mid = (root << 4) | 4;
        let upper = (remainder.wrapping_sub(mid) >> 31) ^ 1;
        remainder -= mid & upper.wrapping_neg();
        root = (root << 2) | (upper << 1);
        let trial = (root << 1) | 1;
        let lower = (remainder >= trial) as u32;
        remainder -= trial & lower.wrapping_neg();
        root |= lower;
    }
    root
}

/// Exact `floor(sqrt(value))` across the complete unsigned 64-bit domain,
/// with 32-bit operations and native `multu`/`divu` only.
///
/// Inputs below `2^48` (every squared distance of two coordinates under
/// `2^23`) take the table-seeded nibble root. Larger inputs take the same
/// root of `value >> 16`, scale it back up (`(seed + 1) * 256` exceeds
/// `sqrt(value)` by at most 256) and take one Newton step through
/// [`div_u64_by_u32`]. The step lands on the floor or one past it, and a
/// single `multu` check settles which.
#[inline]
// psx-numeric-allow-next-line: HL/CS/HK squared-distance roots require full u64 input; the body splits it into two words.
pub const fn isqrt_u64(value: u64) -> u32 {
    let high = (value >> 32) as u32;
    let low = value as u32;
    let wide = high >> 16 != 0;
    if high == u32::MAX {
        // value >= (2^32 - 1)^2 + 2^33 - 2, so the floor is the top root,
        // and the Newton divide below would need high < divisor.
        return u32::MAX;
    }
    // One inlined copy of the nibble root (about 1 KB unrolled) serves both
    // paths: the value itself below 2^48, else `value >> 16` as the Newton
    // seed. The operands are selected with masks rather than a branch, which
    // LLVM would thread into two copies of the root.
    let mask = (wide as u32).wrapping_neg();
    let shift = mask & 16;
    let seed = isqrt_below_2_48(
        high >> shift,
        (((high << 16) | (low >> 16)) & mask) | (low & !mask),
    );
    if !wide {
        return seed;
    }
    // The seed is at least 2^16 here. (seed + 1) * 256 exceeds sqrt(value),
    // and high < sqrt(value) because value < 2^64, so the divide's
    // hi < divisor precondition holds. Seed + 1 reaches 2^24 only for
    // value >= 2^64 - 2^41; u32::MAX still exceeds high there, and a Newton
    // step from it cannot fall below the floor root.
    let start = if seed >= (1 << 24) - 1 {
        u32::MAX
    } else {
        (seed + 1) << 8
    };
    let quotient = div_u64_by_u32(high, low, start);
    // (start + quotient) / 2 without the 33rd bit.
    let mut root = (start >> 1) + (quotient >> 1) + (start & quotient & 1);
    // psx-numeric-allow-next-line: MULTU high/low product compared as two words, no helper
    if (root as u64) * (root as u64) > value {
        root -= 1;
    }
    root
}

/// Multiply an `i32` by a Q1.12 factor (`4096` = 1.0) without a
/// 64-bit intermediate.
///
/// The value splits into whole sectors (`value >> 12`) and the
/// Q0.12 fraction (`value & 0xFFF`); each part multiplies the
/// factor separately and the partial products saturate, so the
/// result stays exact wherever the true product fits in i32 and
/// saturates instead of wrapping where it does not.
#[inline]
pub fn mul_q12_i32(value: i32, q12: i32) -> i32 {
    const Q12_SHIFT: i32 = 12;
    const Q12_ONE: i32 = 1 << Q12_SHIFT;
    let whole = (value >> Q12_SHIFT).saturating_mul(q12);
    let frac = ((value & (Q12_ONE - 1)).saturating_mul(q12)) >> Q12_SHIFT;
    whole.saturating_add(frac)
}

/// `floor(value * q12 / 4096)` through the full 64-bit product.
///
/// Identical to [`mul_q12_i32`] wherever that function does not saturate,
/// which is every product that fits in `i32` (both are exactly the floored
/// true product there; `mul_q12_i32_matches_wide_where_exact` pins it). It
/// wraps instead of saturating beyond that range, so it is for hot paths whose
/// inputs are bounded by construction: BSP plane distances (Q20.12 world
/// points against Q3.12 unit normals) and segment interpolation. On MIPS this
/// is one `mult` and a two-word shift instead of two saturating multiplies.
#[inline(always)]
pub fn mul_q12_i32_wide(value: i32, q12: i32) -> i32 {
    // psx-numeric-allow-next-line: R3000 MULT natively produces this 64-bit product; no wide division or helper call
    ((i64::from(value) * i64::from(q12)) >> 12) as i32
}

/// One base-65536 digit of a normalized long division: the quotient and
/// remainder of `(remainder * 2^16 + next) / divisor`.
///
/// `divisor` has its top bit set and `remainder < divisor`, so the estimate
/// from the divisor's upper half is at most two too large (Knuth, TAOCP vol.
/// 2, 4.3.1, algorithm D) and two explicit corrections settle it. They are
/// spelled out rather than looped so LLVM cannot turn them back into a wide
/// division. Every product and sum fits 32 bits: the estimate is at most
/// `2^16 + 1` and the divisor's lower half at most `2^16 - 1`.
#[inline(always)]
const fn divide_digit(remainder: u32, next: u32, divisor: u32) -> (u32, u32) {
    const RADIX: u32 = 1 << 16;
    let upper = divisor >> 16;
    let lower = divisor & 0xFFFF;
    let mut q = remainder / upper;
    let mut r = remainder % upper;
    if q >= RADIX || q * lower > (r << 16) + next {
        q -= 1;
        r += upper;
        if r < RADIX && (q >= RADIX || q * lower > (r << 16) + next) {
            q -= 1;
        }
    }
    // The true remainder is below the divisor, so the low word alone is exact.
    let rem = (remainder << 16)
        .wrapping_add(next)
        .wrapping_sub(q.wrapping_mul(divisor));
    (q, rem)
}

/// `floor((hi * 2^32 + lo) / divisor)` for a quotient that fits `u32`, in
/// 32-bit operations only.
///
/// Requires `divisor != 0` and `hi < divisor`, which is exactly the shape of
/// a Q12 fraction (`numerator <= denominator`) or a segment interpolation
/// (`|delta| * fraction_numerator / denominator` with the numerator at most
/// the denominator). The R3000A divides 32 by 32 in hardware but has no
/// 64-by-32 form. This normalizes the divisor and produces the quotient as
/// two base-65536 digits, each from one native `divu` plus at most two
/// corrections (Hollow Knight's `quotient32`). On the emulator microbench it
/// costs 178 cycles a call against 510 for the 32-step restoring loop it
/// replaced (random divisors of every magnitude). The generic
/// compiler-builtins `u64_div_rem` before that was 1,412 bytes of branchy
/// code and the only 64-bit divide reachable from runtime code.
#[inline]
pub const fn div_u64_by_u32(hi: u32, lo: u32, divisor: u32) -> u32 {
    debug_assert!(divisor != 0 && hi < divisor);
    let shift = divisor.leading_zeros();
    let normalized = divisor << shift;
    // hi < divisor keeps the shifted top word below the normalized divisor.
    let top = if shift == 0 {
        hi
    } else {
        (hi << shift) | (lo >> (32 - shift))
    };
    let tail = lo << shift;
    let (upper, remainder) = divide_digit(top, tail >> 16, normalized);
    let (lower, _) = divide_digit(remainder, tail & 0xFFFF, normalized);
    (upper << 16) | lower
}

/// Exact `floor(a * b / divisor)` when the quotient fits `u32`.
///
/// Requires a nonzero divisor. R3000 MULTU supplies both product words;
/// ordinary products use native 32-bit DIVU, and only overflowing products
/// need the two-word restoring division. No compiler wide-division helper.
#[inline]
pub fn mul_div_u32(a: u32, b: u32, divisor: u32) -> u32 {
    // psx-numeric-allow-next-line: native R3000 MULTU high/low product, no wide division
    let product = u64::from(a) * u64::from(b);
    let hi = (product >> 32) as u32;
    let lo = product as u32;
    debug_assert!(divisor != 0 && hi < divisor);
    if hi == 0 {
        lo / divisor
    } else {
        div_u64_by_u32(hi, lo, divisor)
    }
}

/// `a * b / c` through the full 64-bit product, truncated toward zero:
/// identical to `(i64::from(a) * i64::from(b) / i64::from(c)) as i32` for
/// every input, with no 64-bit helper call.
///
/// This is the signed fixed-point workhorse: a Q-format multiply is
/// `mul_div_i32(a, b, ONE)`, a Q-format divide `mul_div_i32(a, ONE, b)`, and
/// a proportional interpolation `mul_div_i32(delta, elapsed, duration)`.
///
/// - **Rounding:** truncation toward zero, like Rust's `/` (so `-7 * 1 / 2`
///   is `-3`, not `-4`). For floor semantics with a power-of-two divisor use
///   [`mul_shr_i32`].
/// - **Overflow:** where the true quotient does not fit `i32` the result
///   wraps to its low 32 bits, exactly as the `as i32` cast above does
///   (`mul_div_i32(i32::MIN, -1, 1) == i32::MIN`). Only that case pays a
///   third native divide.
/// - **`c == 0`:** panics, like the 64-bit expression.
///
/// On the R3000A: one `multu` of the magnitudes, then a native `divu` when
/// the product fits 32 bits, else [`div_u64_by_u32`] (two `divu`).
#[inline]
pub fn mul_div_i32(a: i32, b: i32, c: i32) -> i32 {
    let negative = (a < 0) ^ (b < 0) ^ (c < 0);
    let divisor = c.unsigned_abs();
    // psx-numeric-allow-next-line: native R3000 MULTU high/low product, no wide division
    let product = u64::from(a.unsigned_abs()) * u64::from(b.unsigned_abs());
    let hi = (product >> 32) as u32;
    let lo = product as u32;
    let magnitude = if hi == 0 {
        lo / divisor
    } else if hi < divisor {
        div_u64_by_u32(hi, lo, divisor)
    } else {
        // The quotient needs more than 32 bits. Its low word is the quotient
        // of the remainder of the high word (hi = qh * c + rh, and the qh
        // part only adds multiples of 2^32).
        div_u64_by_u32(hi % divisor, lo, divisor)
    };
    if negative {
        magnitude.wrapping_neg() as i32
    } else {
        magnitude as i32
    }
}

/// `(a * b) >> shift` through the full 64-bit product: rounds toward negative
/// infinity like an arithmetic shift, and wraps to the low 32 bits where the
/// shifted value does not fit `i32`.
///
/// Identical to `((i64::from(a) * i64::from(b)) >> shift) as i32`, which is
/// one `mult` and a two-word shift on MIPS; this names it so games stop
/// spelling out the `i64` cast. The Q12 case is [`mul_q12_i32_wide`].
/// Requires `shift < 32`.
#[inline(always)]
pub fn mul_shr_i32(a: i32, b: i32, shift: u32) -> i32 {
    debug_assert!(shift < 32);
    // psx-numeric-allow-next-line: R3000 MULT natively produces this 64-bit product; no wide division or helper call
    ((i64::from(a) * i64::from(b)) >> shift) as i32
}

/// `a * b / 2^shift`, truncated toward zero, through the full 64-bit product.
///
/// Identical to `(i64::from(a) * i64::from(b) / (1i64 << shift)) as i32`
/// (and, for `shift < 31`, to `mul_div_i32(a, b, 1 << shift)`) for every input
/// (wrapping the same way where the quotient does not fit `i32`), but costs
/// one `mult`, a bias add and a two-word shift instead of a divide: a
/// negative product is biased by `2^shift - 1` before the arithmetic shift.
/// This is the Q-format multiply of code that truncates (Hollow Knight's
/// Q16 `mulq`). Requires `shift < 32`.
#[inline(always)]
pub fn mul_shr_trunc_i32(a: i32, b: i32, shift: u32) -> i32 {
    debug_assert!(shift < 32);
    // psx-numeric-allow-next-line: R3000 MULT natively produces this 64-bit product; no wide division or helper call
    let product = i64::from(a) * i64::from(b);
    let bias = if product < 0 { (1i64 << shift) - 1 } else { 0 };
    ((product + bias) >> shift) as i32
}

/// Signed Q8 texels/second, truncated toward zero and wrapped to a texture.
/// `period` must be in `1..=256`; a zero tick rate leaves only the phase.
#[inline]
pub fn scroll_q8_wrapped(speed: i16, phase: u8, period: u16, tick: u32, hz: u16) -> u8 {
    debug_assert!(period > 0 && period <= 256);
    let period = u32::from(period);
    if speed == 0 || hz == 0 {
        return (u32::from(phase) % period) as u8;
    }
    // Truncating by hz then 256 equals truncating by their product. Dropping
    // an entire (hz * 256 * period)-tick cycle changes the result by exactly
    // speed * period texels, so wrapping is unchanged even at u32::MAX ticks.
    // The tick modulus fits u32 at the maximum hz=65535 and period=256.
    let divisor = u32::from(hz) << 8;
    let wrap_ticks = divisor * period;
    let tick = if tick >= wrap_ticks {
        tick % wrap_ticks
    } else {
        tick
    };
    // The reduced quotient is < abs(speed) * period <= 2^23, hence fits i32.
    let distance = mul_div_u32(u32::from(speed.unsigned_abs()), tick, divisor) as i32;
    let distance = if speed < 0 { -distance } else { distance };
    (distance + i32::from(phase)).rem_euclid(period as i32) as u8
}

/// Exact `n / d` for `0 <= n < 2^31` by one `multu` and a shift, for a
/// divisor that is fixed across many dividends.
///
/// Granlund and Montgomery, "Division by invariant integers using
/// multiplication" (1994), theorem 4.2 with `N = 31`: for `l = ceil(log2 d)`
/// and `m = floor(2^(31+l) / d) + 1`, `floor(m * n / 2^(31+l))` equals
/// `floor(n / d)` for every `n < 2^31`, and `m` fits 32 bits. The R3000A's
/// `div` interlocks the pipeline for about 35 cycles and `multu` for about
/// 12 (fewer for small operands), so a per-model depth-slot divide that ran
/// once per drawn face pays for its `new` after a handful of faces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvariantDivisor31 {
    magic: u32,
    /// `l - 1`; `u32::MAX` marks a divisor of one (the product path would
    /// need a 33-bit magic there).
    shift: u32,
}

impl InvariantDivisor31 {
    /// Prepare `divisor`, which must be at least one. This is the 32-step
    /// software divide, so prepare once where the divisor is decided (a
    /// depth range, a table size), not per use.
    #[inline]
    pub const fn new(divisor: u32) -> Self {
        debug_assert!(divisor != 0);
        if divisor <= 1 {
            return Self {
                magic: 0,
                shift: u32::MAX,
            };
        }
        // ceil(log2 d) for d >= 2, so 2^(l-1) < d <= 2^l and the 64-by-32
        // divide below has hi < divisor as it requires.
        let l = 32 - (divisor - 1).leading_zeros();
        let magic = div_u64_by_u32(1 << (l - 1), 0, divisor).wrapping_add(1);
        Self {
            magic,
            shift: l - 1,
        }
    }

    /// `n / divisor` for `n < 2^31`.
    #[inline(always)]
    pub fn divide(self, n: u32) -> u32 {
        debug_assert!(n < 1 << 31);
        if self.shift == u32::MAX {
            return n;
        }
        // psx-numeric-allow-next-line: the high word of R3000 MULTU, no wide divide
        let high = ((u64::from(self.magic) * u64::from(n)) >> 32) as u32;
        high >> self.shift
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsigned_square_root_full_domain_boundaries() {
        let check = |n: u64| {
            let r = u64::from(isqrt_u64(n));
            assert!(r * r <= n);
            assert!(r == u64::from(u32::MAX) || (r + 1) * (r + 1) > n);
        };
        for root in [0u64, 1, 2, 255, 65535, 65536, 1 << 31, u64::from(u32::MAX)] {
            let square = root * root;
            for n in [
                square.saturating_sub(1),
                square,
                square.saturating_add(1),
                u64::MAX,
            ] {
                check(n);
            }
        }
        for n in 0..100_000 {
            check(n);
        }
    }

    #[test]
    fn mul_div_u32_preserves_full_width_animation_phases() {
        let check = |a: u32, b: u32, d: u32| {
            let expected = u64::from(a) * u64::from(b) / u64::from(d);
            assert!(expected <= u64::from(u32::MAX));
            assert_eq!(u64::from(mul_div_u32(a, b, d)), expected, "{a} * {b} / {d}");
        };
        let edges = [1, 2, 59, 60, 65535, 65536, 65537, 1 << 31, u32::MAX];
        for d in edges {
            for b in edges {
                for a in [0, 1, d / 2, d - 1, d] {
                    check(a, b, d);
                }
            }
        }
        check(u32::MAX, 1, 1);
        check(65536, 65536, 2);
        let mut seed = 0x5817_9513u32;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for _ in 0..100_000 {
            let d = next().max(1);
            check(next() % d, next(), d);
        }
    }

    #[test]
    fn wrapped_scroll_matches_wide_oracle_at_wrap_and_tick_boundaries() {
        let check = |speed: i16, phase: u8, period: u16, tick: u32, hz: u16| {
            let distance = if hz == 0 {
                0
            } else {
                i64::from(speed) * i64::from(tick) / i64::from(hz) / 256
            };
            let expected = (distance + i64::from(phase)).rem_euclid(i64::from(period)) as u8;
            assert_eq!(
                scroll_q8_wrapped(speed, phase, period, tick, hz),
                expected,
                "speed={speed} phase={phase} period={period} tick={tick} hz={hz}"
            );
        };
        // Every signed speed, including MIN (whose absolute value is 32768).
        for speed in i16::MIN..=i16::MAX {
            for tick in [0, 1, 59, 60, 15359, 15360, u32::MAX] {
                check(speed, 255, 63, tick, 60);
            }
        }
        // Arbitrary periods, byte wrapping, PAL/NTSC and the full rate domain.
        for period in 1..=256 {
            for hz in [1u16, 50, 60, 256, 512, 65535] {
                let cycle = (u32::from(hz) << 8) * u32::from(period);
                for tick in [0, 1, cycle - 1, cycle, cycle + 1, u32::MAX - 1, u32::MAX] {
                    for speed in [i16::MIN, -32767, -257, -1, 0, 1, 257, i16::MAX] {
                        for phase in [0, 127, 255] {
                            check(speed, phase, period, tick, hz);
                        }
                    }
                }
            }
        }
        for phase in [0, 127, 255] {
            check(i16::MIN, phase, 63, u32::MAX, 0);
        }
        let mut seed = 0x3211_8a53u32;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for _ in 0..100_000 {
            check(
                next() as i16,
                next() as u8,
                (next() % 256 + 1) as u16,
                next(),
                next() as u16,
            );
        }
    }

    #[test]
    fn invariant_divisor_31_is_exact() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let check = |n: u32, d: u32| {
            assert_eq!(InvariantDivisor31::new(d).divide(n), n / d, "n={n} d={d}");
        };
        for d in 1..=70u32 {
            for n in 0..2048u32 {
                check(n, d);
            }
            let top = (1u32 << 31) - 1;
            for n in [top, top - 1, top - d, d, d - 1, d + 1, d * 3 - 1, d * 3] {
                check(n, d);
            }
        }
        for shift in 1..31u32 {
            let d = 1u32 << shift;
            for n in [0, 1, d - 1, d, d + 1, (1u32 << 31) - 1] {
                check(n, d);
                check(n, d - 1);
                check(n, d + 1);
            }
        }
        for _ in 0..200_000 {
            let d = (next() as u32 >> (next() % 32) as u32).max(1);
            let n = next() as u32 >> 1;
            check(n, d);
            check(n % d, d);
            check(n - n % d, d);
        }
    }

    #[test]
    fn div_u64_by_u32_matches_the_wide_reference() {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..20_000 {
            let divisor = ((next() as u32) | 1).max(2);
            let hi = (next() as u32) % divisor;
            let lo = next() as u32;
            let wide = ((u64::from(hi) << 32) | u64::from(lo)) / u64::from(divisor);
            assert_eq!(u64::from(div_u64_by_u32(hi, lo, divisor)), wide);
        }
        assert_eq!(div_u64_by_u32(0, 0, 1), 0);
        assert_eq!(div_u64_by_u32(u32::MAX - 1, u32::MAX, u32::MAX), u32::MAX);
        assert_eq!(div_u64_by_u32(0, 4096 << 12, 4096), 4096);
    }

    #[test]
    fn mul_q12_i32_matches_wide_where_exact() {
        // Random pairs across the whole tracer domain (Q20.12 points up to
        // +-2^24 against Q3.12 normals up to +-4096) plus the classic edges;
        // wherever the exact product fits in i32 both forms agree bit for bit.
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut checked = 0u32;
        for _ in 0..200_000 {
            let r = next();
            let value = ((r as i32) >> 7).clamp(-(1 << 24), 1 << 24);
            let q12 = (((r >> 32) as i32) >> 19).clamp(-4096, 4096);
            let exact = (i64::from(value) * i64::from(q12)) >> 12;
            if exact > i64::from(i32::MAX) || exact < i64::from(i32::MIN) {
                continue;
            }
            assert_eq!(
                mul_q12_i32(value, q12),
                mul_q12_i32_wide(value, q12),
                "{value} {q12}"
            );
            assert_eq!(mul_q12_i32_wide(value, q12), exact as i32);
            checked += 1;
        }
        assert!(checked > 190_000, "{checked}");
        for &(value, q12) in &[
            (0, 0),
            (-1, 4096),
            (-4095, 4096),
            (4095, -4096),
            (1 << 24, 4096),
            (-(1 << 24), 4096),
            (i32::MAX >> 12, 4096),
            (i32::MIN >> 12, 4096),
            (7, -1),
            (-7, 1),
        ] {
            assert_eq!(
                mul_q12_i32(value, q12),
                mul_q12_i32_wide(value, q12),
                "{value} {q12}"
            );
        }
    }

    fn xorshift(mut state: u64) -> impl FnMut() -> u64 {
        move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        }
    }

    /// Exact floor root by definition: r^2 <= n < (r + 1)^2, in u128.
    fn assert_floor_root(n: u64) {
        let r = u128::from(isqrt_u64(n));
        let n128 = u128::from(n);
        assert!(
            r * r <= n128 && (r + 1) * (r + 1) > n128,
            "isqrt_u64({n}) = {r}"
        );
    }

    #[test]
    fn isqrt_u64_is_exact_at_every_path_boundary() {
        // Squares either side of the table seed, the 2^48 fast-path limit,
        // the clamped Newton start (seed 2^24 - 1) and the top of the domain.
        let roots = (0u64..300)
            .chain((1 << 16) - 300..(1 << 16) + 300)
            .chain((1 << 24) - 300..(1 << 24) + 300)
            .chain((1 << 32) - 70_000..1 << 32);
        for root in roots {
            let square = root * root;
            for n in [square.saturating_sub(1), square, square.saturating_add(1)] {
                assert_floor_root(n);
            }
            assert_floor_root(square.saturating_add(2 * root));
        }
        for n in ((1u64 << 48) - 100_000..(1 << 48) + 100_000).chain(u64::MAX - 100_000..=u64::MAX)
        {
            assert_floor_root(n);
        }
        for n in 0..1 << 20 {
            assert_floor_root(n);
        }
    }

    #[test]
    fn isqrt_u64_matches_definition_at_every_magnitude() {
        let mut next = xorshift(0x2545_f491_4f6c_dd1d);
        for i in 0..2_000_000u64 {
            let n = next() >> (i % 64);
            assert_floor_root(n);
            // Perfect squares and their neighbours at the same magnitude.
            let r = u64::from(isqrt_u64(n));
            assert_floor_root((r * r).wrapping_sub(1));
        }
    }

    #[test]
    fn div_u64_by_u32_every_short_divisor_and_digit_boundary() {
        let check = |n: u64, d: u32| {
            assert_eq!(
                u64::from(div_u64_by_u32((n >> 32) as u32, n as u32, d)),
                n / u64::from(d),
                "{n} / {d}"
            );
        };
        let wide = [
            65537,
            0x7fff_ffff,
            0x8000_0000,
            0x8000_ffff,
            0xffff_0001,
            u32::MAX,
        ];
        for d in (1u32..=65536).chain(wide) {
            for q in [
                0u32,
                1,
                65534,
                65535,
                65536,
                65537,
                0x7fff_ffff,
                0xffff_fffe,
                u32::MAX,
            ] {
                for rem in [0, d / 2, d - 1] {
                    check(u64::from(q) * u64::from(d) + u64::from(rem), d);
                }
            }
        }
        let mut next = xorshift(0x9ab4_3f87_6546_bcad);
        for i in 0..2_000_000u32 {
            let seed = next();
            let d = ((seed as u32) >> (i % 32)).max(1);
            let hi = ((seed >> 32) as u32) % d;
            check((u64::from(hi) << 32) | u64::from(next() as u32), d);
        }
    }

    /// The expression every `mul_div_i32` caller used to write.
    fn mul_div_wide(a: i32, b: i32, c: i32) -> i32 {
        (i64::from(a) * i64::from(b) / i64::from(c)) as i32
    }

    const SIGNED_EDGES: [i32; 23] = [
        i32::MIN,
        i32::MIN + 1,
        -(1 << 30) - 1,
        -65537,
        -65536,
        -65535,
        -4096,
        -7,
        -2,
        -1,
        0,
        1,
        2,
        3,
        7,
        4096,
        46341,
        65535,
        65536,
        65537,
        1 << 30,
        i32::MAX - 1,
        i32::MAX,
    ];

    #[test]
    fn mul_div_i32_matches_i64_on_every_edge_triple() {
        for a in SIGNED_EDGES {
            for b in SIGNED_EDGES {
                for c in SIGNED_EDGES.into_iter().filter(|&c| c != 0) {
                    assert_eq!(
                        mul_div_i32(a, b, c),
                        mul_div_wide(a, b, c),
                        "{a} * {b} / {c}"
                    );
                }
            }
        }
        // Truncation toward zero, not floor; wrapping where i64 -> i32 wraps.
        assert_eq!(mul_div_i32(-7, 1, 2), -3);
        assert_eq!(mul_div_i32(7, -1, 2), -3);
        assert_eq!(mul_div_i32(-7, -1, -2), -3);
        assert_eq!(mul_div_i32(i32::MIN, 1, -1), i32::MIN);
        assert_eq!(mul_div_i32(i32::MIN, i32::MIN, 1), 0);
        assert_eq!(mul_div_i32(i32::MAX, i32::MAX, 1), 1);
        assert_eq!(mul_div_i32(123_456, 789, 1), 123_456 * 789);
    }

    #[test]
    fn mul_div_i32_matches_i64_on_random_inputs_at_every_magnitude() {
        let mut next = xorshift(0x5817_9513_0bad_cafe);
        for i in 0..2_000_000u32 {
            let r = next();
            let a = (r as i32) >> (i % 32);
            let b = ((r >> 32) as i32) >> ((i / 32) % 32);
            let c = ((next() as i32) >> (i % 31)) | 1;
            assert_eq!(
                mul_div_i32(a, b, c),
                mul_div_wide(a, b, c),
                "{a} * {b} / {c}"
            );
            // The Q16 multiply and divide shapes the games use.
            assert_eq!(mul_div_i32(a, b, 65536), mul_div_wide(a, b, 65536));
            assert_eq!(mul_div_i32(a, 65536, c), mul_div_wide(a, 65536, c));
        }
    }

    #[test]
    #[should_panic]
    fn mul_div_i32_panics_on_zero_divisor_with_a_small_product() {
        mul_div_i32(3, 4, core::hint::black_box(0));
    }

    #[test]
    #[should_panic]
    fn mul_div_i32_panics_on_zero_divisor_with_a_wide_product() {
        mul_div_i32(i32::MAX, i32::MAX, core::hint::black_box(0));
    }

    #[test]
    fn shift_multiplies_match_i64_for_every_shift() {
        let check = |a: i32, b: i32, shift: u32| {
            let product = i64::from(a) * i64::from(b);
            assert_eq!(
                mul_shr_i32(a, b, shift),
                (product >> shift) as i32,
                "{a} {b} {shift}"
            );
            assert_eq!(
                mul_shr_trunc_i32(a, b, shift),
                (product / (1i64 << shift)) as i32,
                "{a} {b} {shift}"
            );
            if shift < 31 {
                assert_eq!(
                    mul_shr_trunc_i32(a, b, shift),
                    mul_div_i32(a, b, 1 << shift)
                );
            }
        };
        for shift in 0..32 {
            for a in SIGNED_EDGES {
                for b in SIGNED_EDGES {
                    check(a, b, shift);
                }
            }
        }
        let mut next = xorshift(0x3211_8a53_dead_beef);
        for i in 0..1_000_000u32 {
            let r = next();
            check(
                r as i32 >> (i % 32),
                (r >> 32) as i32 >> ((i / 7) % 32),
                i % 32,
            );
        }
        assert_eq!(mul_shr_i32(-1, 1, 16), -1);
        assert_eq!(mul_shr_trunc_i32(-1, 1, 16), 0);
        assert_eq!(mul_shr_i32(-65537, 1, 16), -2);
        assert_eq!(mul_shr_trunc_i32(-65537, 1, 16), -1);
    }

    #[test]
    fn abs_saturates_at_min() {
        assert_eq!(abs_i32(i32::MIN), i32::MAX);
        assert_eq!(abs_i32(-5), 5);
        assert_eq!(abs_i32(7), 7);
        assert_eq!(abs_i16(i16::MIN), i16::MAX);
        assert_eq!(abs_i16(-5), 5);
    }

    #[test]
    fn clamp_i16_covers_both_ends() {
        assert_eq!(clamp_i16(40_000), i16::MAX);
        assert_eq!(clamp_i16(-40_000), i16::MIN);
        assert_eq!(clamp_i16(123), 123);
    }

    #[test]
    fn square_saturates_past_46340() {
        assert_eq!(square_i32_saturating(46_340), 46_340 * 46_340);
        assert_eq!(square_i32_saturating(46_341), i32::MAX);
        assert_eq!(square_i32_saturating(i32::MIN), i32::MAX);
        assert_eq!(square_i32_saturating(-3), 9);
    }

    #[test]
    fn isqrt_matches_floor_sqrt() {
        assert_eq!(isqrt_i32(0), 0);
        assert_eq!(isqrt_i32(-9), 0);
        assert_eq!(isqrt_i32(1), 1);
        assert_eq!(isqrt_i32(15), 3);
        assert_eq!(isqrt_i32(16), 4);
        assert_eq!(isqrt_i32(i32::MAX), 46_340);
    }

    #[test]
    fn mul_q12_handles_sign_and_scale() {
        assert_eq!(mul_q12_i32(1000, 4096), 1000);
        assert_eq!(mul_q12_i32(1000, 2048), 500);
        assert_eq!(mul_q12_i32(-1000, 2048), -500);
        assert_eq!(mul_q12_i32(0, 4096), 0);
    }
}
