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

/// Q0.12 angle of the vector `(x, y)`: the inverse of [`sin_q12`] / [`cos_q12`], so
/// `atan2_q12(sin, cos)` round-trips an angle. `atan2_q12(dx, dz)` is the yaw that faces
/// `(dx, dz)` when forward is `(sin yaw, cos yaw)`. `(0, 0)` returns 0.
///
/// Accuracy contract, the one thing a caller may rely on:
/// - exact (no error at all) on the four axes and the four diagonals, at every magnitude;
/// - worst error under 3.2 units (0.28 degrees, 4096 units = one turn) from the true angle of the
///   integer vector, everywhere;
/// - continuous: the output never decreases as the vector turns counter-clockwise (the
///   quotient it reads is exact for the slope, so only the integer input rounding can move it),
///   and no step is more than one unit for a one-unit change of slope; the octants join and the
///   turn wraps without a jump.
///
/// Folds to the first octant, takes the slope `small / large` in Q12 (one division), and
/// evaluates `x * (512 + 178 * (1 - x))` for the angle of `atan(x)` in those units: no table.
/// Integer-only; large coordinates are pre-scaled together so the slope never leaves u32.
///
/// Never inlined: at 284 B the compiler copies it into every caller once it is this small
/// (NitroXide, which calls it from dozens of places: +8,076 B text inlined, -100 B out of line
/// against the table form).
#[inline(never)]
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
    // Slope in Q12, 0..=4096; the angle of that slope in units, rounded: s * (512 + 178 * (1 - s))
    // with the second factor kept to 8 bits less so the product stays inside u32.
    let slope = (small << 12) / large;
    let q = ((slope * ((512 * 4096 + 178 * (4096 - slope)) >> 8) + (1 << 15)) >> 16) as i32;
    let q = if ax >= ay { q } else { 1024 - q };
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
    extern crate std;
    use super::*;
    use std::vec::Vec;

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
    fn atan2_is_within_3_2_units_of_the_exact_angle() {
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
        assert!(worst < 3.2, "worst error {worst} units");
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
    fn atan2_is_monotonic_in_the_slope_and_joins_the_octants_without_a_jump() {
        // y = s, x = 4096: the slope is exact, so every slope of the octant is visited.
        let mut last = atan2_q12(0, 4096);
        for s in 1..=4096 {
            let a = atan2_q12(s, 4096);
            assert!(a >= last && a - last <= 1, "slope {s}: {last} then {a}");
            last = a;
        }
        assert_eq!(last, 512);
        // Across the diagonal into the next octant: 45 degrees is shared, and each side moves
        // toward it by at most a unit per step.
        assert_eq!(atan2_q12(4095, 4096), 512);
        assert_eq!(atan2_q12(4096, 4095), 512);
    }

    #[test]
    fn atan2_turning_a_vector_all_the_way_round_never_steps_back_or_jumps() {
        // Sweep the full circle in fine steps at several radii (small ones, where the integer
        // input is coarse, are judged against the exact angle of the integer vector only).
        for r in [1000.0f64, 4096.0, 30000.0, 400_000.0] {
            let mut last: Option<u16> = None;
            for i in 0..=40960 {
                let t = f64::from(i) / 40960.0 * core::f64::consts::TAU;
                let (y, x) = ((r * t.sin()).round() as i32, (r * t.cos()).round() as i32);
                let a = atan2_q12(y, x);
                assert!(gap(a, exact(y, x)) < 3.2, "r {r} i {i}");
                if let Some(p) = last {
                    // Forward by at most one unit of angle per tenth of a unit of input angle
                    // plus the integer rounding of the input (about 0.35 units at r = 1000);
                    // a small dip is that rounding, never a step back of more than one unit.
                    let d = i32::from(a) - i32::from(p);
                    let d = if d > 2048 {
                        d - 4096
                    } else if d < -2048 {
                        d + 4096
                    } else {
                        d
                    };
                    assert!((-1..=2).contains(&d), "r {r} i {i}: {p} then {a}");
                }
                last = Some(a);
            }
        }
        for r in [1i32, 2, 3, 5, 8, 13, 40] {
            // Small radii: every integer vector on the circle of that size, in order of true angle.
            let mut pts: Vec<(f64, i32, i32)> = Vec::new();
            for y in -r..=r {
                for x in -r..=r {
                    if x != 0 || y != 0 {
                        pts.push((exact(y, x), y, x));
                    }
                }
            }
            pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            for p in pts.windows(2) {
                let (a, b) = (atan2_q12(p[0].1, p[0].2), atan2_q12(p[1].1, p[1].2));
                // Two vectors of true angles t0 <= t1 map to angles at most 6 units out of order.
                let d = i32::from(b) - i32::from(a);
                let d = if d > 2048 {
                    d - 4096
                } else if d < -2048 {
                    d + 4096
                } else {
                    d
                };
                assert!(d >= -6, "{:?} then {:?}", p[0], p[1]);
            }
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
