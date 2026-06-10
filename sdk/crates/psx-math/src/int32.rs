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

#[cfg(test)]
mod tests {
    use super::*;

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
