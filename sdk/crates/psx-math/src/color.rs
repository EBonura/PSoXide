//! RGB arithmetic with explicit rounding contracts.
/// Scale each channel by a signed ratio, truncating toward zero, then clamp.
/// `den` must be nonzero, each channel product must fit `i32`, and a
/// product of `i32::MIN` must not be divided by -1.
#[inline]
pub fn scale_rgb(rgb: (u8, u8, u8), num: i32, den: i32) -> (u8, u8, u8) {
    let channel = |v: u8| ((i32::from(v) * num) / den).clamp(0, 255) as u8;
    (channel(rgb.0), channel(rgb.1), channel(rgb.2))
}
/// Scale each channel by `num / 256`, truncating (a right shift of the product), for a brightness
/// or fade `num` in `0..=256`. No division and no clamp: three multiplies and three shifts, the
/// body the launcher and the loader's disc art wrote by hand. Unlike [`scale_rgb`] it does not
/// clamp, so a `num` above 256 wraps the channel (`as u8`) the way those callers' code did; keep
/// `num` at 256 or below, or use [`scale_rgb`] when a boost must saturate. `num` up to
/// 16,843,009 cannot overflow the 32-bit product.
#[inline]
#[must_use]
pub const fn scale_rgb_q8(rgb: (u8, u8, u8), num: u32) -> (u8, u8, u8) {
    (
        ((rgb.0 as u32 * num) >> 8) as u8,
        ((rgb.1 as u32 * num) >> 8) as u8,
        ((rgb.2 as u32 * num) >> 8) as u8,
    )
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
    fn scale_rgb_q8_is_the_disc_art_body_and_truncates() {
        // The launcher/loader `scale`, as the oracle, over every channel and a numerator range.
        let oracle = |c: (u8, u8, u8), num: u32| {
            (
                ((c.0 as u32 * num) >> 8) as u8,
                ((c.1 as u32 * num) >> 8) as u8,
                ((c.2 as u32 * num) >> 8) as u8,
            )
        };
        for v in 0..=255u8 {
            for num in 0..=300u32 {
                assert_eq!(
                    scale_rgb_q8((v, 255 - v, v / 2), num),
                    oracle((v, 255 - v, v / 2), num)
                );
            }
        }
        assert_eq!(scale_rgb_q8((255, 128, 1), 256), (255, 128, 1));
        assert_eq!(scale_rgb_q8((255, 128, 1), 128), (127, 64, 0));
        assert_eq!(scale_rgb_q8((255, 128, 1), 0), (0, 0, 0));
        // Equal to scale_rgb on the range where nothing clamps.
        for v in [0u8, 1, 77, 128, 255] {
            for num in 0..=256u32 {
                assert_eq!(
                    scale_rgb_q8((v, v, v), num).0,
                    ((i32::from(v) * num as i32) / 256) as u8
                );
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
