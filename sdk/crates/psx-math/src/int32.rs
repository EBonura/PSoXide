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
    let mut x = value as u32;
    let mut r = 0u32;
    let mut bit = 1u32 << 30;
    while bit > x {
        bit >>= 2;
    }
    while bit != 0 {
        if x >= r + bit {
            x -= r + bit;
            r = (r >> 1) + bit;
        } else {
            r >>= 1;
        }
        bit >>= 2;
    }
    r as i32
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
    ((i64::from(value) * i64::from(q12)) >> 12) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

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
