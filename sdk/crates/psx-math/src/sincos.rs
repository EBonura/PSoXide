//! Compact sin/cos lookup for Q0.12 angles.
//!
//! A Q0.12 angle represents one full revolution in a 12-bit
//! unsigned: `0..4096` maps linearly onto `0°..360°`. This
//! matches the PSX GTE's `ONE` / `4096` angle precision. The lookup
//! table stays compact at 256 entries, and `sin_q12` linearly
//! interpolates between entries so every Q0.12 angle step produces a
//! distinct result when the table slope permits it.
//!
//! Values are Q1.12 fixed-point: `4096` = 1.0, `-4096` = -1.0.
//!
//! # Example
//!
//! ```ignore
//! // Rotate point (x, y) by `angle` around origin.
//! let s = sin_q12(angle);
//! let c = cos_q12(angle);
//! let xr = (x * c - y * s) >> 12;
//! let yr = (x * s + y * c) >> 12;
//! ```
//!
//! Integer multiplication before the shift keeps the arithmetic
//! inside i32; `>> 12` drops the Q1.12 fractional bits after the
//! multiplies combine.

/// 256-entry sine table, Q1.12 fixed-point (one revolution).
///
/// Index = `(angle_q12 >> 4) & 0xFF`, where `angle_q12` is a u16
/// in `[0, 4096)` mapping linearly to `[0°, 360°)`. `sin_q12`
/// interpolates using the low 4 angle bits. Values are i16 in
/// `[-4096, 4096]`.
pub const SIN_TABLE: [i16; 256] = [
    0, 101, 201, 301, 401, 501, 601, 700, 799, 897, 995, 1092, 1189, 1285, 1380, 1474, 1567, 1660,
    1751, 1842, 1931, 2019, 2106, 2191, 2276, 2359, 2440, 2520, 2598, 2675, 2751, 2824, 2896, 2967,
    3035, 3102, 3166, 3229, 3290, 3349, 3406, 3461, 3513, 3564, 3612, 3659, 3703, 3745, 3784, 3822,
    3857, 3889, 3920, 3948, 3973, 3996, 4017, 4036, 4052, 4065, 4076, 4085, 4091, 4095, 4096, 4095,
    4091, 4085, 4076, 4065, 4052, 4036, 4017, 3996, 3973, 3948, 3920, 3889, 3857, 3822, 3784, 3745,
    3703, 3659, 3612, 3564, 3513, 3461, 3406, 3349, 3290, 3229, 3166, 3102, 3035, 2967, 2896, 2824,
    2751, 2675, 2598, 2520, 2440, 2359, 2276, 2191, 2106, 2019, 1931, 1842, 1751, 1660, 1567, 1474,
    1380, 1285, 1189, 1092, 995, 897, 799, 700, 601, 501, 401, 301, 201, 101, 0, -101, -201, -301,
    -401, -501, -601, -700, -799, -897, -995, -1092, -1189, -1285, -1380, -1474, -1567, -1660,
    -1751, -1842, -1931, -2019, -2106, -2191, -2276, -2359, -2440, -2520, -2598, -2675, -2751,
    -2824, -2896, -2967, -3035, -3102, -3166, -3229, -3290, -3349, -3406, -3461, -3513, -3564,
    -3612, -3659, -3703, -3745, -3784, -3822, -3857, -3889, -3920, -3948, -3973, -3996, -4017,
    -4036, -4052, -4065, -4076, -4085, -4091, -4095, -4096, -4095, -4091, -4085, -4076, -4065,
    -4052, -4036, -4017, -3996, -3973, -3948, -3920, -3889, -3857, -3822, -3784, -3745, -3703,
    -3659, -3612, -3564, -3513, -3461, -3406, -3349, -3290, -3229, -3166, -3102, -3035, -2967,
    -2896, -2824, -2751, -2675, -2598, -2520, -2440, -2359, -2276, -2191, -2106, -2019, -1931,
    -1842, -1751, -1660, -1567, -1474, -1380, -1285, -1189, -1092, -995, -897, -799, -700, -601,
    -501, -401, -301, -201, -101,
];

/// `sin(angle_q12)` as a Q1.12 value. Input wraps modulo 4096.
#[inline]
pub fn sin_q12(angle_q12: u16) -> i32 {
    let angle = angle_q12 & 0x0fff;
    let idx = (angle >> 4) as usize;
    let frac = (angle & 0x000f) as i32;
    let a = SIN_TABLE[idx] as i32;
    let b = SIN_TABLE[(idx + 1) & 0xff] as i32;
    a + (((b - a) * frac) / 16)
}

/// `cos(angle_q12)` as a Q1.12 value, implemented as
/// `sin(angle + 90°)`. Input wraps modulo 4096.
#[inline]
pub fn cos_q12(angle_q12: u16) -> i32 {
    // 90° = 1024 in Q0.12 (4096 / 4). Wrap-add is automatic in u16.
    sin_q12(angle_q12.wrapping_add(1024))
}

/// `atan(i / 64)` for `i` in `0..=64`, in Q12 turns scaled by 16, so the table carries four
/// fractional bits and 45 degrees is exactly 8192.
const ATAN_X16: [u16; 65] = [
    0, 163, 326, 489, 651, 813, 975, 1136, 1297, 1457, 1617, 1775, 1933, 2090, 2246, 2401, 2555,
    2708, 2860, 3010, 3159, 3307, 3453, 3599, 3742, 3884, 4025, 4164, 4302, 4438, 4572, 4705, 4836,
    4966, 5094, 5220, 5344, 5467, 5589, 5708, 5826, 5943, 6058, 6171, 6282, 6392, 6500, 6607, 6712,
    6815, 6917, 7018, 7117, 7214, 7310, 7405, 7498, 7589, 7679, 7768, 7856, 7942, 8026, 8110, 8192,
];

/// Q0.12 angle of the vector `(x, y)`: the inverse of [`sin_q12`] / [`cos_q12`], so
/// `atan2_q12(sin, cos)` round-trips an angle.
///
/// Folds to the first octant and reads `atan(slope)` from a 65-entry table with linear
/// interpolation: exact on the axes and diagonals, worst error under one unit (0.09 degrees)
/// everywhere (the octant-linear form this replaced erred by up to 47 units, 4.2 degrees).
/// `(0, 0)` returns 0. Integer-only; large coordinates are pre-scaled together so the slope
/// never leaves u32. `atan2_q12(dx, dz)` is the yaw that faces `(dx, dz)` when forward is
/// `(sin yaw, cos yaw)`.
pub fn atan2_q12(y: i32, x: i32) -> u16 {
    if x == 0 && y == 0 {
        return 0;
    }
    let (mut ax, mut ay) = (x.unsigned_abs(), y.unsigned_abs());
    // `small << 12` has to fit a u32. Scaling both magnitudes together keeps the ratio, which
    // is all that is used below.
    while ax >= (1 << 19) || ay >= (1 << 19) {
        ax >>= 8;
        ay >>= 8;
    }
    let (small, large) = if ax >= ay { (ay, ax) } else { (ax, ay) };
    // Slope in Q12, 0..=4096: six bits pick the table entry, six interpolate.
    let slope = (small << 12) / large;
    let (i, frac) = ((slope >> 6) as usize, slope & 63);
    let a = u32::from(ATAN_X16[i]);
    let b = u32::from(ATAN_X16[i + usize::from(i < 64)]);
    let octant = ((a * 64 + (b - a) * frac + 512) >> 10) as i32;
    let q = if ax >= ay { octant } else { 1024 - octant };
    let angle = match (x >= 0, y >= 0) {
        (true, true) => q,
        (false, true) => 2048 - q,
        (false, false) => 2048 + q,
        (true, false) => 4096 - q,
    };
    (angle & 0xfff) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sin_anchors() {
        assert_eq!(sin_q12(0), 0);
        assert_eq!(sin_q12(1024), 4096); // 90°
        assert_eq!(sin_q12(2048), 0); //    180°
        assert_eq!(sin_q12(3072), -4096); // 270°
    }

    #[test]
    fn cos_anchors() {
        assert_eq!(cos_q12(0), 4096);
        assert_eq!(cos_q12(1024), 0);
        assert_eq!(cos_q12(2048), -4096);
        assert_eq!(cos_q12(3072), 0);
    }

    #[test]
    fn wraps_modulo_4096() {
        // 4096 is one full revolution; the u16 wrap makes it 0.
        assert_eq!(sin_q12(4096), sin_q12(0));
        assert_eq!(sin_q12(5120), sin_q12(1024));
    }

    #[test]
    fn interpolates_low_angle_bits() {
        assert_eq!(sin_q12(0), SIN_TABLE[0] as i32);
        assert!(sin_q12(1) > sin_q12(0));
        assert!(sin_q12(1) < SIN_TABLE[1] as i32);
        assert_eq!(sin_q12(16), SIN_TABLE[1] as i32);
    }

    #[test]
    fn atan2_cardinals_and_diagonals() {
        assert_eq!(atan2_q12(0, 0), 0);
        assert_eq!(atan2_q12(0, 100), 0); //     +x =   0°
        assert_eq!(atan2_q12(100, 100), 512); // diag = 45°
        assert_eq!(atan2_q12(100, 0), 1024); //  +y =  90°
        assert_eq!(atan2_q12(100, -100), 1536);
        assert_eq!(atan2_q12(0, -100), 2048); // -x = 180°
        assert_eq!(atan2_q12(-100, -100), 2560);
        assert_eq!(atan2_q12(-100, 0), 3072); // -y = 270°
        assert_eq!(atan2_q12(-100, 100), 3584);
    }

    /// The exact angle in Q12 turns, as a float, for the accuracy oracle (host tests only).
    fn exact(y: i32, x: i32) -> f64 {
        let a = f64::from(y).atan2(f64::from(x)) / core::f64::consts::TAU * 4096.0;
        if a < 0.0 {
            a + 4096.0
        } else {
            a
        }
    }

    fn gap(a: u16, b: f64) -> f64 {
        let d = (f64::from(a) - b).abs() % 4096.0;
        d.min(4096.0 - d)
    }

    #[test]
    fn atan2_is_within_one_unit_of_the_exact_angle() {
        let mut worst = 0.0f64;
        for y in -400..=400 {
            for x in -400..=400 {
                if x != 0 || y != 0 {
                    worst = worst.max(gap(atan2_q12(y, x), exact(y, x)));
                }
            }
        }
        // Magnitudes across the whole i32 range, including both prescale paths.
        let mut s = 12345u32;
        for _ in 0..1_000_000 {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let y = (s as i32) >> (s % 24);
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let x = (s as i32) >> (s % 24);
            if x != 0 || y != 0 {
                worst = worst.max(gap(atan2_q12(y, x), exact(y, x)));
            }
        }
        assert!(worst < 1.0, "worst error {worst} units");
    }

    #[test]
    fn atan2_is_exact_on_the_axes_and_diagonals_at_every_magnitude() {
        for m in [1, 2, 3, 17, 255, 4096, 1 << 18, 1 << 19, 1 << 24, i32::MAX] {
            for (y, x, want) in [
                (0, m, 0),
                (m, m, 512),
                (m, 0, 1024),
                (m, -m, 1536),
                (0, -m, 2048),
                (-m, -m, 2560),
                (-m, 0, 3072),
                (-m, m, 3584),
            ] {
                assert_eq!(atan2_q12(y, x), want, "y {y} x {x}");
            }
        }
    }

    #[test]
    fn atan2_roundtrips_sincos_within_the_table_error() {
        // The sine table itself is 8-bit interpolated; allow a few units.
        for &angle in &[0u16, 100, 512, 900, 1024, 2000, 2048, 3000, 3072, 4000] {
            let (s, c) = (sin_q12(angle), cos_q12(angle));
            let back = atan2_q12(s, c);
            let diff = (back.wrapping_sub(angle) & 0xfff).min(angle.wrapping_sub(back) & 0xfff);
            assert!(diff <= 4, "angle {angle} came back as {back}");
        }
    }

    #[test]
    fn atan2_large_coordinates_stay_in_u32() {
        // Same direction at any magnitude; the pre-scale must not change it.
        assert_eq!(atan2_q12(i32::MAX, i32::MAX), 512);
        assert_eq!(atan2_q12(0, i32::MIN), 2048);
        assert_eq!(atan2_q12(i32::MIN, 0), 3072);
        assert_eq!(atan2_q12(1 << 28, 1 << 28), atan2_q12(1, 1));
    }

    #[test]
    fn interpolates_across_table_wrap() {
        assert!(sin_q12(4095) < 0);
        assert!(sin_q12(4095) > SIN_TABLE[255] as i32);
        assert_eq!(sin_q12(4096), 0);
    }
}
