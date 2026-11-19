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
//! There are two arctangents on purpose: [`atan2_q12`] is the coarse one (inlined, 4.2 degrees
//! worst case, output frozen for replays) and [`atan2_q12_fine`] the accurate one (under one unit,
//! never inlined). Each one's documentation says which job it is for and why one body would not do.
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
/// The COARSE arctangent, for facing, steering, aiming and camera logic that runs many times a
/// frame. Octant-linear: exact on the axes and diagonals, worst error 47 units (4.2 degrees,
/// 4096 units = one turn) mid-octant. About 200 B and 80 cycles, and small enough that the
/// compiler inlines it into hot callers, which is the point of having it. Its output is part
/// of the contract: games' replays and tapes record what it returns, so it never changes (the
/// `atan2_coarse_output_is_frozen` test pins a hash of its output over a grid and 200,000
/// random vectors). Use [`atan2_q12_fine`] where a reading below a few units matters.
///
/// Integer-only; large coordinates are pre-scaled so the math never leaves u32.
///
/// Why two arctangents exist (measured, NitroXide ball-cam window, ticks 3150 to 3600): when
/// every caller was moved to one accurate out-of-line body, flips fell from 399 to 305 and
/// frames missing a vblank rose from 50 to 143 at equal total work, because each hot caller
/// stopped inlining a 200 B function and made a call per use, hundreds of times a frame.
pub fn atan2_q12(y: i32, x: i32) -> u16 {
    if x == 0 && y == 0 {
        return 0;
    }
    let (mut ax, mut ay) = (x.unsigned_abs(), y.unsigned_abs());
    // Keep `small * 512` inside u32: scale both magnitudes below 2^23
    // together, which preserves their ratio (the only thing used below).
    while ax >= (1 << 23) || ay >= (1 << 23) {
        ax >>= 8;
        ay >>= 8;
    }
    // First-octant fold: angle 0..1024 (= 90°), 45° exactly at ax == ay.
    let q = if ax >= ay {
        (ay * 512 / ax) as i32
    } else {
        1024 - (ax * 512 / ay) as i32
    };
    let a = match (x >= 0, y >= 0) {
        (true, true) => q,
        (false, true) => 2048 - q,
        (false, false) => 2048 + q,
        (true, false) => 4096 - q,
    };
    (a & 0xfff) as u16
}

/// `atan(i / 64)` for `i` in `0..=64`, in Q12 turns scaled by 16, so the table carries four
/// fractional bits and 45 degrees is exactly 8192.
const ATAN_X16: [u16; 65] = [
    0, 163, 326, 489, 651, 813, 975, 1136, 1297, 1457, 1617, 1775, 1933, 2090, 2246, 2401, 2555,
    2708, 2860, 3010, 3159, 3307, 3453, 3599, 3742, 3884, 4025, 4164, 4302, 4438, 4572, 4705, 4836,
    4966, 5094, 5220, 5344, 5467, 5589, 5708, 5826, 5943, 6058, 6171, 6282, 6392, 6500, 6607, 6712,
    6815, 6917, 7018, 7117, 7214, 7310, 7405, 7498, 7589, 7679, 7768, 7856, 7942, 8026, 8110, 8192,
];

/// Same angle as [`atan2_q12`], accurate: the FINE arctangent, for a reading taken back from an
/// angle (air control, attitude, ship heading, route following), not for per-frame fan-out.
///
/// Accuracy contract: exact on the axes and diagonals at every magnitude; worst error under one
/// unit (0.09 degrees) from the true angle of the integer vector; monotonic in the slope; the
/// octants join and the turn wraps without a jump. `(0, 0)` returns 0.
///
/// Folds to the first octant and reads `atan(slope)` from a 65-entry table (130 B) with linear
/// interpolation: about 320 B and 129 cycles a call. Never inlined: a body this size is copied
/// into every caller otherwise. The body is the one NitroXide shipped as its own
/// `atan2_q12_fine`, so its output is unchanged by moving here (a test compares every vector of a
/// wide grid against a verbatim copy of that body).
///
/// Under one unit is REQUIRED, not a luxury, wherever the result is fed back each tick (an air
/// control that reads the yaw out of an attitude and turns it again): a bias of even 3 units
/// accumulates. Measured on NitroXide's air control over ten stick patterns, a no-table two-term
/// form (284 B, worst error 3.09 units, per-call bias 1 to 1.4 units) left a steady heading
/// offset of 8 to 17 units through a full-yaw spin, 32 to 38 with mixed sticks and up to 85
/// units (7.5 degrees) on a gentle yaw over about a second; this table stays within a unit or
/// two of the truth. That form is not shipped.
#[inline(never)]
#[must_use]
pub fn atan2_q12_fine(y: i32, x: i32) -> u16 {
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
    fn atan2_fine_cardinals_and_diagonals() {
        assert_eq!(atan2_q12_fine(0, 0), 0);
        assert_eq!(atan2_q12_fine(0, 100), 0); //     +x =   0°
        assert_eq!(atan2_q12_fine(100, 100), 512); // diag = 45°
        assert_eq!(atan2_q12_fine(100, 0), 1024); //  +y =  90°
        assert_eq!(atan2_q12_fine(100, -100), 1536);
        assert_eq!(atan2_q12_fine(0, -100), 2048); // -x = 180°
        assert_eq!(atan2_q12_fine(-100, -100), 2560);
        assert_eq!(atan2_q12_fine(-100, 0), 3072); // -y = 270°
        assert_eq!(atan2_q12_fine(-100, 100), 3584);
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
                    worst = worst.max(gap(atan2_q12_fine(y, x), exact(y, x)));
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
                worst = worst.max(gap(atan2_q12_fine(y, x), exact(y, x)));
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
                assert_eq!(atan2_q12_fine(y, x), want, "y {y} x {x}");
            }
        }
    }

    #[test]
    fn atan2_roundtrips_sincos_within_the_table_error() {
        // The sine table itself is 8-bit interpolated; allow a few units.
        for &angle in &[0u16, 100, 512, 900, 1024, 2000, 2048, 3000, 3072, 4000] {
            let (s, c) = (sin_q12(angle), cos_q12(angle));
            let back = atan2_q12_fine(s, c);
            let diff = (back.wrapping_sub(angle) & 0xfff).min(angle.wrapping_sub(back) & 0xfff);
            assert!(diff <= 4, "angle {angle} came back as {back}");
        }
    }

    #[test]
    fn atan2_is_monotonic_in_the_slope_and_joins_the_octants_without_a_jump() {
        // y = s, x = 4096: the slope is exact, so every slope of the octant is visited.
        let mut last = atan2_q12_fine(0, 4096);
        for s in 1..=4096 {
            let a = atan2_q12_fine(s, 4096);
            assert!(a >= last && a - last <= 1, "slope {s}: {last} then {a}");
            last = a;
        }
        assert_eq!(last, 512);
        // Across the diagonal into the next octant: 45 degrees is shared, and each side moves
        // toward it by at most a unit per step.
        assert_eq!(atan2_q12_fine(4095, 4096), 512);
        assert_eq!(atan2_q12_fine(4096, 4095), 512);
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
                let a = atan2_q12_fine(y, x);
                assert!(gap(a, exact(y, x)) < 1.0, "r {r} i {i}");
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
                let (a, b) = (
                    atan2_q12_fine(p[0].1, p[0].2),
                    atan2_q12_fine(p[1].1, p[1].2),
                );
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
        assert_eq!(atan2_q12_fine(i32::MAX, i32::MAX), 512);
        assert_eq!(atan2_q12_fine(0, i32::MIN), 2048);
        assert_eq!(atan2_q12_fine(i32::MIN, 0), 3072);
        assert_eq!(atan2_q12_fine(1 << 28, 1 << 28), atan2_q12_fine(1, 1));
    }

    #[test]
    fn interpolates_across_table_wrap() {
        assert!(sin_q12(4095) < 0);
        assert!(sin_q12(4095) > SIN_TABLE[255] as i32);
        assert_eq!(sin_q12(4096), 0);
    }

    #[test]
    fn atan2_coarse_cardinals_and_diagonals() {
        assert_eq!(atan2_q12(0, 0), 0);
        for (y, x, want) in [
            (0, 100, 0),
            (100, 100, 512),
            (100, 0, 1024),
            (100, -100, 1536),
            (0, -100, 2048),
            (-100, -100, 2560),
            (-100, 0, 3072),
            (-100, 100, 3584),
        ] {
            assert_eq!(atan2_q12(y, x), want, "y {y} x {x}");
        }
    }

    /// FNV-1a over the outputs, the same fold the frozen constants below were computed with
    /// from the octant-linear body that shipped before the accurate one existed.
    fn fold(h: u32, v: u16) -> u32 {
        (h ^ u32::from(v)).wrapping_mul(16_777_619)
    }

    #[test]
    fn atan2_coarse_output_is_frozen() {
        // Replays and tapes of five games record this function's output: it must never move.
        for (y, x, want) in [
            (1, 1000, 0),
            (7, 100, 35),
            (100, 7, 989),
            (-37, 91, 3888),
            (13, -250, 2022),
            (-250, -13, 3046),
            (299, 300, 510),
            (300, 299, 514),
            (5, -3, 1331),
            (-1, -100_000, 2048),
            (123_456_789, 987_654, 1020),
            (i32::MIN, 5, 3072),
            (3, i32::MIN, 2048),
            (i32::MAX, 1, 1024),
            (1, i32::MAX, 0),
            (1000, 1, 1024),
        ] {
            assert_eq!(atan2_q12(y, x), want, "y {y} x {x}");
        }
        let mut h = 0x811c_9dc5u32;
        for y in -400..=400 {
            for x in -400..=400 {
                h = fold(h, atan2_q12(y, x));
            }
        }
        assert_eq!(h, 0xeb52_dabf, "grid hash");
        let (mut h, mut s) = (0x811c_9dc5u32, 12345u32);
        for _ in 0..200_000 {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let y = (s as i32) >> (s % 24);
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let x = (s as i32) >> (s % 24);
            h = fold(h, atan2_q12(y, x));
        }
        assert_eq!(h, 0x6ba5_a8e2, "random hash");
    }

    #[test]
    fn atan2_coarse_stays_within_its_documented_error() {
        let mut worst = 0.0f64;
        for y in -400..=400 {
            for x in -400..=400 {
                if x != 0 || y != 0 {
                    worst = worst.max(gap(atan2_q12(y, x), exact(y, x)));
                }
            }
        }
        assert!(worst < 48.0, "worst error {worst} units");
    }

    /// NitroXide's `sim/src/angle.rs` `atan2_q12_fine`, verbatim (64d30ff), with its table.
    fn nitro_reference(y: i32, x: i32) -> u16 {
        if x == 0 && y == 0 {
            return 0;
        }
        let (mut ax, mut ay) = (x.unsigned_abs(), y.unsigned_abs());
        while ax >= (1 << 19) || ay >= (1 << 19) {
            ax >>= 8;
            ay >>= 8;
        }
        let (small, large) = if ax >= ay { (ay, ax) } else { (ax, ay) };
        let slope = (small << 12) / large;
        let (i, frac) = ((slope >> 6) as usize, slope & 63);
        let a = ATAN_X16[i] as u32;
        let b = ATAN_X16[i + (i < 64) as usize] as u32;
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

    #[test]
    fn atan2_fine_equals_nitroxides_former_copy_over_a_wide_grid() {
        // The table is shared (the copy's constants are the same 65 numbers, pinned below).
        assert_eq!(ATAN_X16[0], 0);
        assert_eq!(ATAN_X16[1], 163);
        assert_eq!(ATAN_X16[32], 4836);
        assert_eq!(ATAN_X16[64], 8192);
        for y in -1200..=1200 {
            for x in -1200..=1200 {
                assert_eq!(atan2_q12_fine(y, x), nitro_reference(y, x), "y {y} x {x}");
            }
        }
        let mut s = 987_654_321u32;
        for _ in 0..2_000_000 {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let y = (s as i32) >> (s % 31);
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let x = (s as i32) >> (s % 31);
            assert_eq!(atan2_q12_fine(y, x), nitro_reference(y, x), "y {y} x {x}");
        }
        for y in [i32::MIN, i32::MAX, -1, 1, 0] {
            for x in [i32::MIN, i32::MAX, -1, 1, 0] {
                assert_eq!(atan2_q12_fine(y, x), nitro_reference(y, x), "y {y} x {x}");
            }
        }
    }
}
