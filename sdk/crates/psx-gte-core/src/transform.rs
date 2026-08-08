//! Composable 3D transform math operating on [`Mat3I16`] / [`Vec3I16`].
//!
//! Lives next to the type definitions because the inherent `impl`s
//! belong in the same crate as the type. Higher-level GTE register
//! access (`mtc2!` / `ctc2!` macros) lives in `psx-gte`, which
//! re-exports this module so callers can keep importing it from there.
//!
//! The PS1 has no hardware sin/cos -- rotations use the shared
//! [`psx_math::sincos::SIN_TABLE`] lookup. Angles are in
//! **256-per-revolution** units (`u16` with implicit wrap at 256)
//! rather than radians or degrees, matching the convention most PSX
//! homebrew uses. A full turn fits in a single byte, which indexes
//! every sixteenth entry of the SDK's Q0.12 sine table.
//!
//! Fixed-point conventions (inherited from the GTE):
//! - Matrix cells: **1.3.12** signed -- `0x1000` = 1.0.
//! - Translation / screen offset / H: integers in engine-chosen
//!   scaling; the math here just passes them through.
//! - Vectors: **1.3.12** `i16`; accumulator outputs are `i32` in MAC
//!   or `i16` in IR post-saturation.

#![allow(clippy::needless_range_loop)]

use crate::math::{Mat3I16, Vec3I16};
use psx_math::SIN_TABLE;

/// Sine of `angle` in 256-per-revolution units, returned in 1.3.12.
///
/// Angles wrap modulo 256. Values are piecewise-linear approximations
/// of `sin` -- accurate to within ±1 LSB of the 1.3.12 representation,
/// which is good enough for real-time 3D but not for scientific use.
#[inline]
pub const fn sin_1_3_12(angle: u16) -> i16 {
    SIN_TABLE[(angle & 0xFF) as usize]
}

/// Cosine of `angle` in 256-per-revolution units, returned in 1.3.12.
#[inline]
pub const fn cos_1_3_12(angle: u16) -> i16 {
    // cos(θ) = sin(θ + π/2). Our angle units put π/2 at 64.
    sin_1_3_12(angle.wrapping_add(64))
}

impl Mat3I16 {
    /// Matrix × matrix, with 1.3.12 fixed-point scaling applied so the
    /// output stays in 1.3.12. Saturates each cell to `i16` range --
    /// rotation compositions stay well within, but callers doing
    /// scale-by-100 before compose should watch for truncation.
    pub fn mul(&self, other: &Self) -> Self {
        let mut out = [[0i16; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                let column = [other.m[0][j], other.m[1][j], other.m[2][j]];
                out[i][j] = clamp_i32_to_i16(dot_q12_i16(self.m[i], column));
            }
        }
        Self { m: out }
    }

    /// Compose X, then Y, then Z rotations using the PS1 matrix convention.
    ///
    /// Angles use the same 256-per-revolution units as [`Self::rotate_x`].
    /// The explicit order matters for camera/model transforms with more than
    /// one non-zero Euler component.
    #[inline]
    pub fn rotate_xyz(x: u16, y: u16, z: u16) -> Self {
        Self::rotate_x(x)
            .mul(&Self::rotate_y(y))
            .mul(&Self::rotate_z(z))
    }

    /// Scale matrix rows by independent Q12 factors.
    ///
    /// This matches the classic PS1 `ScaleMatrixL` convention: row 0 receives
    /// X, row 1 receives Y, and row 2 receives Z. Results saturate like the
    /// GTE IR registers instead of wrapping at `i16` boundaries.
    pub fn scale_rows_q12(&self, scale: [i32; 3]) -> Self {
        let mut out = self.m;
        for row in 0..3 {
            for col in 0..3 {
                let value = ((self.m[row][col] as i64 * scale[row] as i64) >> 12)
                    .clamp(i16::MIN as i64, i16::MAX as i64);
                out[row][col] = value as i16;
            }
        }
        Self { m: out }
    }

    /// Scale matrix columns by independent Q12 factors.
    ///
    /// This matches the classic PS1 `ScaleMatrix` convention: column 0
    /// receives X, column 1 receives Y, and column 2 receives Z.
    pub fn scale_columns_q12(&self, scale: [i32; 3]) -> Self {
        let mut out = self.m;
        for row in 0..3 {
            for col in 0..3 {
                let value = ((self.m[row][col] as i64 * scale[col] as i64) >> 12)
                    .clamp(i16::MIN as i64, i16::MAX as i64);
                out[row][col] = value as i16;
            }
        }
        Self { m: out }
    }

    /// Matrix × vector, with 1.3.12 scaling. Returns `i32` per
    /// component so the caller can inspect the un-saturated product
    /// (useful for normal generation / light calculations before
    /// feeding back into the GTE as V0).
    pub fn transform(&self, v: Vec3I16) -> [i32; 3] {
        let mut out = [0i32; 3];
        for i in 0..3 {
            out[i] = dot_q12_i16(self.m[i], [v.x, v.y, v.z]);
        }
        out
    }

    /// Rotation around the X axis by `angle` (256-per-revolution units).
    ///
    /// ```text
    ///   | 1    0     0  |
    ///   | 0   cos  -sin |
    ///   | 0   sin   cos |
    /// ```
    pub const fn rotate_x(angle: u16) -> Self {
        let c = cos_1_3_12(angle);
        let s = sin_1_3_12(angle);
        Self {
            m: [[0x1000, 0, 0], [0, c, -s], [0, s, c]],
        }
    }

    /// Rotation around the Y axis.
    ///
    /// ```text
    ///   |  cos  0  sin |
    ///   |   0   1   0  |
    ///   | -sin  0  cos |
    /// ```
    pub const fn rotate_y(angle: u16) -> Self {
        let c = cos_1_3_12(angle);
        let s = sin_1_3_12(angle);
        Self {
            m: [[c, 0, s], [0, 0x1000, 0], [-s, 0, c]],
        }
    }

    /// Rotation around the Z axis.
    ///
    /// ```text
    ///   | cos  -sin  0 |
    ///   | sin   cos  0 |
    ///   |  0     0   1 |
    /// ```
    pub const fn rotate_z(angle: u16) -> Self {
        let c = cos_1_3_12(angle);
        let s = sin_1_3_12(angle);
        Self {
            m: [[c, -s, 0], [s, c, 0], [0, 0, 0x1000]],
        }
    }
}

#[inline]
fn dot_q12_i16(a: [i16; 3], b: [i16; 3]) -> i32 {
    let mut sum = 0i32;
    let mut i = 0;
    while i < 3 {
        sum = sum.saturating_add((a[i] as i32) * (b[i] as i32));
        i += 1;
    }
    sum >> 12
}

#[inline]
fn clamp_i32_to_i16(value: i32) -> i16 {
    value.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sin_cos_identity_at_cardinal_angles() {
        // Exact values at 0, π/2, π, 3π/2.
        assert_eq!(sin_1_3_12(0), 0);
        assert_eq!(cos_1_3_12(0), 0x1000);
        assert_eq!(sin_1_3_12(64), 0x1000);
        assert_eq!(cos_1_3_12(64), 0);
        assert_eq!(sin_1_3_12(128), 0);
        assert_eq!(cos_1_3_12(128), -0x1000);
        assert_eq!(sin_1_3_12(192), -0x1000);
        assert_eq!(cos_1_3_12(192), 0);
    }

    #[test]
    fn scene_rtps_matches_captured_gameplay_outputs() {
        // Register sets captured from a live cortex_ignition_v1 gameplay frame
        // (probe_gameplay_gte_trace). These vertices saturate the screen coords
        // and hit perspective-divide overflow (FLAG bit 17) -- the regime that
        // drives the on-hardware vertex explosion and that the synthetic GTE
        // battery never reaches. Host mirror of the hardware-tests disc cases:
        // locks PSoXide's RTPS outputs for the scene's exact inputs, so the
        // disc cases are green in-emulator and any hardware divergence is real.
        fn rtps(vxy0: u32, vz0: u32) -> (u32, u32) {
            let mut g = crate::Gte::new();
            g.write_control(0, 0x0000_0f19); // R11,R12
            g.write_control(1, 0x016e_fab4); // R13,R21
            g.write_control(2, 0x0411_f098); // R22,R23
            g.write_control(3, 0xfbb1_fae7); // R31,R32
            g.write_control(4, 0xffff_f177); // R33
            g.write_control(24, 0x00a0_0000); // OFX
            g.write_control(25, 0x0078_0000); // OFY
            g.write_control(26, 0x0000_0140); // H
            g.write_data(0, vxy0); // VXY0
            g.write_data(1, vz0); // VZ0
            g.execute(0x4a08_0001); // RTPS (sf=1)
            (g.read_data(14), g.read_control(31)) // (SXY2, FLAG)
        }
        assert_eq!(rtps(0x0c3e_0000, 0x0000_0a4d), (0xfc00_fc00, 0x8006_6000));
        assert_eq!(rtps(0x0c3e_0529, 0x0000_08eb).0, 0xfc00_03ff);
        assert_eq!(rtps(0x0c3e_0526, 0xffff_f714), (0xfc00_03b4, 0x8000_2000));
        assert_eq!(rtps(0x099c_f335, 0x0000_0000), (0xfc00_fc00, 0x8000_6000));
    }

    #[test]
    fn scene_ops_and_corners_match_psx_gte_core() {
        // Host mirror of the hardware-tests GTE conformance battery (part 2):
        // scene-captured MVMVA / RTPT / NCLIP plus the LZCS corner case. Locks
        // PSoXide's outputs for the exact inputs the disc replays, so the disc
        // cases are green in-emulator and any hardware divergence is real.
        fn tri(a: u32, b: u32, c: u32) -> u32 {
            a ^ b.rotate_left(11) ^ c.rotate_left(22)
        }
        fn seed_xform(g: &mut crate::Gte) {
            g.write_control(31, 0);
            g.write_control(0, 0x0000_0f19);
            g.write_control(1, 0x016e_fab4);
            g.write_control(2, 0x0411_f098);
            g.write_control(3, 0xfbb1_fae7);
            g.write_control(4, 0xffff_f177);
            g.write_control(5, 0xffff_eabc);
            g.write_control(6, 0xffff_fdb9);
            g.write_control(7, 0x0000_35be);
            g.write_control(24, 0x00a0_0000);
            g.write_control(25, 0x0078_0000);
            g.write_control(26, 0x0000_0140);
            g.write_control(27, 0);
            g.write_control(28, 0);
        }

        // MVMVA (RT*V0 + TR, sf=1): digest of MAC1/2/3.
        let mvmva = |vxy0: u32, vz0: u32| {
            let mut g = crate::Gte::new();
            seed_xform(&mut g);
            g.write_data(0, vxy0);
            g.write_data(1, vz0);
            g.execute(0x4a08_0012);
            tri(g.read_data(25), g.read_data(26), g.read_data(27))
        };
        assert_eq!(mvmva(0x2040_0340, 0x0000_09c0), 0xca74_6d65);
        assert_eq!(mvmva(0x2040_0340, 0x0000_16c0), 0xd65a_09bf);
        assert_eq!(mvmva(0x0b00_0340, 0x0000_23c0), 0x511b_ee0c);
        assert_eq!(mvmva(0x2040_09c0, 0x0000_09c0), 0x46ef_d743);

        // RTPT: SXY digest + FLAG + SZ3.
        let rtpt = |v: [u32; 6]| {
            let mut g = crate::Gte::new();
            seed_xform(&mut g);
            for (i, &w) in v.iter().enumerate() {
                g.write_data(i as u8, w);
            }
            g.execute(0x4a08_0030);
            (
                tri(g.read_data(12), g.read_data(13), g.read_data(14)),
                g.read_control(31),
                g.read_data(19),
            )
        };
        assert_eq!(
            rtpt([
                0x0480_2d80,
                0x0000_2700,
                0x0480_2080,
                0x0000_2d80,
                0x0480_1a00,
                0x0000_2d80
            ]),
            (0xfc1f_e61f, 0x8000_6000, 0x0000_02e9)
        );
        assert_eq!(
            rtpt([
                0x0480_2700,
                0x0000_2d80,
                0x0480_2d80,
                0x0000_2d80,
                0x0480_2080,
                0x0000_3400
            ]),
            (0xfbe0_066f, 0x8006_6000, 0x0000_0000)
        );
        assert_eq!(
            rtpt([
                0x10c0_0000,
                0x0000_0d00,
                0x10c0_0680,
                0x0000_0d00,
                0x1dc0_0680,
                0x0000_0d00
            ]),
            (0xaf36_5a05, 0x0000_0000, 0x0000_1fd9)
        );

        // NCLIP: real scene backface MAC0.
        let nclip = |s0: u32, s1: u32, s2: u32| {
            let mut g = crate::Gte::new();
            g.write_control(31, 0);
            g.write_data(12, s0);
            g.write_data(13, s1);
            g.write_data(14, s2);
            g.execute(0x4a00_0006);
            g.read_data(24)
        };
        assert_eq!(nclip(0x006e_0095, 0xffe2_0094, 0xffde_00dc), 0x0000_2764);
        assert_eq!(nclip(0x0073_00d5, 0xffde_00dc, 0xffd8_0130), 0x0000_30ba);
        assert_eq!(nclip(0x0079_011f, 0xffd8_0130, 0xffd2_0194), 0x0000_3e7e);

        // LZCS/LZCR leading-bit count.
        let lzcr = |v: u32| {
            let mut g = crate::Gte::new();
            g.write_data(30, v);
            g.read_data(31)
        };
        assert_eq!(lzcr(0x00ff_ffff), 8);
        assert_eq!(lzcr(0xffff_0000), 16);
        assert_eq!(lzcr(0x0000_0001), 31);
        assert_eq!(lzcr(0x7fff_ffff), 1);
        assert_eq!(lzcr(0x8000_0000), 1);

        // MVMVA bugged FC mode (cv=2): the far-color translation AND the
        // first matrix column are dropped -> MAC_i = (Mx_i2*Vy + Mx_i3*Vz).
        // Confirmed against real hardware (cortex GTE disc 2026-06-09).
        {
            let mut g = crate::Gte::new();
            seed_xform(&mut g);
            g.write_control(21, 0x0000_1000); // FCX
            g.write_control(22, 0x0000_2000); // FCY
            g.write_control(23, 0x0000_3000); // FCZ
            g.write_data(0, 0x2040_0340);
            g.write_data(1, 0x0000_09c0);
            g.execute(0x4a08_4012); // MVMVA mx=RT vx=V0 cv=FC sf=1 lm=0
            assert_eq!(
                (g.read_data(25), g.read_data(26), g.read_data(27)),
                (0xffff_fcc5, 0xffff_e36c, 0xffff_ee75)
            );
        }
    }

    #[test]
    fn sin_wraps_modulo_256() {
        // Angles outside 0..256 must wrap.
        assert_eq!(sin_1_3_12(256), sin_1_3_12(0));
        assert_eq!(sin_1_3_12(320), sin_1_3_12(64));
        assert_eq!(sin_1_3_12(65535), sin_1_3_12(255));
    }

    #[test]
    fn sin_cos_pythagorean_identity_holds_approximately() {
        // sin²+cos² = 1.0² = 0x1000² = 0x100_0000.
        // Our values are approximate so allow a small tolerance.
        for angle in (0..256).step_by(7) {
            let s = sin_1_3_12(angle as u16) as i32;
            let c = cos_1_3_12(angle as u16) as i32;
            let sum = s * s + c * c;
            let expected = 0x0100_0000;
            // ±0.5% tolerance -- quarter-sine table rounds each entry.
            let tolerance = expected / 200;
            assert!(
                (sum - expected).abs() <= tolerance,
                "angle {angle}: sin²+cos² = {sum:#x}, expected ≈ {expected:#x}"
            );
        }
    }

    #[test]
    fn identity_mul_identity_is_identity() {
        let id = Mat3I16::IDENTITY;
        let prod = id.mul(&id);
        assert_eq!(prod, id);
    }

    #[test]
    fn rotate_y_zero_is_identity() {
        assert_eq!(Mat3I16::rotate_y(0), Mat3I16::IDENTITY);
    }

    #[test]
    fn rotate_xyz_uses_x_then_y_then_z_order() {
        let expected = Mat3I16::rotate_x(17)
            .mul(&Mat3I16::rotate_y(39))
            .mul(&Mat3I16::rotate_z(73));
        assert_eq!(Mat3I16::rotate_xyz(17, 39, 73), expected);
        assert_ne!(
            Mat3I16::rotate_xyz(17, 39, 73),
            Mat3I16::rotate_z(73)
                .mul(&Mat3I16::rotate_y(39))
                .mul(&Mat3I16::rotate_x(17))
        );
    }

    #[test]
    fn row_and_column_scaling_are_explicit_and_saturating() {
        let matrix = Mat3I16 {
            m: [
                [0x1000, 0x0800, 0],
                [0, 0x1000, 0x0400],
                [0x0200, 0, 0x1000],
            ],
        };
        assert_eq!(
            matrix.scale_rows_q12([0x0800, 0x1000, 0x2000]).m,
            [
                [0x0800, 0x0400, 0],
                [0, 0x1000, 0x0400],
                [0x0400, 0, 0x2000]
            ]
        );
        assert_eq!(
            matrix.scale_columns_q12([0x0800, 0x1000, 0x2000]).m,
            [
                [0x0800, 0x0800, 0],
                [0, 0x1000, 0x0800],
                [0x0100, 0, 0x2000]
            ]
        );
        assert_eq!(
            Mat3I16::IDENTITY
                .scale_rows_q12([i32::MAX, 0x1000, 0x1000])
                .m[0][0],
            i16::MAX
        );
    }

    #[test]
    fn rotate_y_quarter_turn_swaps_axes() {
        // 90° Y rotation: (1,0,0) → (0,0,-1), (0,0,1) → (1,0,0).
        let m = Mat3I16::rotate_y(64);
        assert_eq!(m.m[0][0], 0);
        assert_eq!(m.m[0][2], 0x1000);
        assert_eq!(m.m[2][0], -0x1000);
        assert_eq!(m.m[2][2], 0);
        // Y axis untouched.
        assert_eq!(m.m[1][1], 0x1000);
    }

    #[test]
    fn rotate_x_then_inverse_round_trips() {
        // Rotating by θ then by -θ (256-θ in our units) should yield
        // the identity within rounding error.
        let r = Mat3I16::rotate_x(30);
        let r_inv = Mat3I16::rotate_x(226);
        let prod = r.mul(&r_inv);
        // Diagonal should be ≈ 0x1000, off-diagonal ≈ 0.
        for i in 0..3 {
            for j in 0..3 {
                let expected = if i == j { 0x1000 } else { 0 };
                let diff = (prod.m[i][j] as i32 - expected).abs();
                assert!(
                    diff < 8,
                    "rotate_x ∘ rotate_x⁻¹ cell ({i},{j}) = {:#x}, expected ≈ {expected:#x}",
                    prod.m[i][j]
                );
            }
        }
    }

    #[test]
    fn transform_identity_leaves_vector_unchanged() {
        let v = Vec3I16::new(100, -200, 300);
        let r = Mat3I16::IDENTITY.transform(v);
        assert_eq!(r, [100, -200, 300]);
    }

    #[test]
    fn transform_rotate_y_quarter_turn() {
        // (1, 0, 0) after 90° Y = (0, 0, -1) in our convention.
        let v = Vec3I16::new(0x1000, 0, 0);
        let r = Mat3I16::rotate_y(64).transform(v);
        assert_eq!(r, [0, 0, -0x1000]);
    }
}
