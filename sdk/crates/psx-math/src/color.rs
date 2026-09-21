//! RGB arithmetic with explicit rounding contracts.
/// Scale each channel by a signed ratio, truncating toward zero, then clamp.
/// `den` must be nonzero, each channel product must fit `i32`, and a
/// product of `i32::MIN` must not be divided by -1.
#[inline]
pub fn scale_rgb(rgb: (u8, u8, u8), num: i32, den: i32) -> (u8, u8, u8) {
    let channel = |v: u8| ((i32::from(v) * num) / den).clamp(0, 255) as u8;
    (channel(rgb.0), channel(rgb.1), channel(rgb.2))
}
/// Interpolate using signed division toward zero. Clamp the numerator to the
/// original denominator before replacing a zero divisor with one. Therefore
/// a zero denominator always returns `a`, matching the caller contract.
#[inline]
pub fn lerp_rgb(a: (u8, u8, u8), b: (u8, u8, u8), num: u16, den: u16) -> (u8, u8, u8) {
    let num = i32::from(num.min(den));
    let den = den.max(1);
    let channel =
        |x: u8, y: u8| (i32::from(x) + (i32::from(y) - i32::from(x)) * num / i32::from(den)) as u8;
    (channel(a.0, b.0), channel(a.1, b.1), channel(a.2, b.2))
}
/// Q8 interpolation with arithmetic shift (rounding negative deltas down).
/// `t` is a fraction over 256; 255 deliberately does not quite reach `b`.
#[inline]
pub fn lerp_rgb_q8(a: (u8, u8, u8), b: (u8, u8, u8), t: u8) -> (u8, u8, u8) {
    let channel =
        |x: u8, y: u8| (i32::from(x) + (((i32::from(y) - i32::from(x)) * i32::from(t)) >> 8)) as u8;
    (channel(a.0, b.0), channel(a.1, b.1), channel(a.2, b.2))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ratio_clamping_matches_legacy_formula_including_zero_denominator() {
        for den in [0u16, 1, 2, 255, u16::MAX] {
            for num in [0u16, 1, 9, 255, u16::MAX] {
                for (a, b) in [((3, 4, 5), (9, 8, 7)), ((255, 0, 1), (0, 255, 0))] {
                    let (n, d) = (num.min(den) as i32, den.max(1) as i32);
                    let channel = |x: u8, y: u8| (x as i32 + (y as i32 - x as i32) * n / d) as u8;
                    assert_eq!(
                        lerp_rgb(a, b, num, den),
                        (channel(a.0, b.0), channel(a.1, b.1), channel(a.2, b.2))
                    );
                }
            }
        }
    }
    #[test]
    fn descending_rounding_is_explicit() {
        assert_eq!(lerp_rgb((3, 0, 255), (0, 3, 0), 1, 2), (2, 1, 128));
        assert_eq!(lerp_rgb_q8((3, 0, 255), (0, 3, 0), 128), (1, 1, 127));
        assert_eq!(lerp_rgb((3, 4, 5), (9, 8, 7), 9, 0), (3, 4, 5));
        assert_eq!(scale_rgb((1, 128, 255), 3, 2), (1, 192, 255));
        assert_eq!(scale_rgb((1, 128, 255), -1, 2), (0, 0, 0));
    }
}
