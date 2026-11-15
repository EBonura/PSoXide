//! Fixed-point math types for GTE inputs/outputs.
//!
//! The PS1 GTE works in two flavours of fixed point:
//!
//! - **1.3.12** for matrix entries, normals, small vectors -- 16-bit
//!   signed, divide by 4096 for real value. Range: roughly ±8.0 with
//!   12 bits of fraction.
//! - **19.12 / 31.0** for translations, colour bias, far colour --
//!   32-bit signed stored raw.
//!
//! Types here are **POD** (`Copy`, `no_std`, no heap). The GTE register
//! wire format is always a packed `u32`, so the types mainly exist for
//! static correctness -- the actual register I/O happens through the
//! `write_data!` / `write_control!` macros in `psx-gte`.

/// 3-component vector of `i16` in 1.3.12 fixed point.
/// Matches the GTE's V0/V1/V2 input slots and the IR accumulators.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
#[repr(C)]
pub struct Vec3I16 {
    /// X component.
    pub x: i16,
    /// Y component.
    pub y: i16,
    /// Z component.
    pub z: i16,
}

impl Vec3I16 {
    /// Zero vector.
    pub const ZERO: Self = Self { x: 0, y: 0, z: 0 };

    /// Construct from three components.
    #[inline(always)]
    pub const fn new(x: i16, y: i16, z: i16) -> Self {
        Self { x, y, z }
    }

    /// Pack the `(x, y)` pair into the 32-bit form expected by MTC2
    /// for V0/V1/V2 slots 0/2/4 -- low half = `x`, high half = `y`.
    #[inline(always)]
    pub const fn xy_packed(self) -> u32 {
        ((self.y as u16 as u32) << 16) | (self.x as u16 as u32)
    }

    /// Sign-extended `z` for MTC2 of V0/V1/V2 slots 1/3/5.
    #[inline(always)]
    pub const fn z_packed(self) -> u32 {
        self.z as i32 as u32
    }
}

/// 3-component vector of `i32` -- used for translation / colour bias
/// / far colour values (stored raw in 31.0 or 19.12 depending on which
/// register, see the GTE spec).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
#[repr(C)]
pub struct Vec3I32 {
    /// X / R component.
    pub x: i32,
    /// Y / G component.
    pub y: i32,
    /// Z / B component.
    pub z: i32,
}

impl Vec3I32 {
    /// Zero vector.
    pub const ZERO: Self = Self { x: 0, y: 0, z: 0 };

    /// Construct from three components.
    #[inline(always)]
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    /// Component-wise sum, each lane saturating at the `i32` limits.
    #[inline(always)]
    #[must_use]
    pub const fn saturating_add(self, rhs: Self) -> Self {
        Self {
            x: self.x.saturating_add(rhs.x),
            y: self.y.saturating_add(rhs.y),
            z: self.z.saturating_add(rhs.z),
        }
    }

    /// Component-wise difference, each lane saturating at the `i32` limits.
    #[inline(always)]
    #[must_use]
    pub const fn saturating_sub(self, rhs: Self) -> Self {
        Self {
            x: self.x.saturating_sub(rhs.x),
            y: self.y.saturating_sub(rhs.y),
            z: self.z.saturating_sub(rhs.z),
        }
    }

    /// Q12 dot product: the sum of `mul_q12_i32` of each lane pair, the sum
    /// saturating. `self` may carry a large Q12 offset; `rhs` should be a
    /// unit-scale vector so the products stay in range.
    #[inline]
    #[must_use]
    pub fn dot_q12(self, rhs: Self) -> i32 {
        psx_math::int32::mul_q12_i32(self.x, rhs.x)
            .saturating_add(psx_math::int32::mul_q12_i32(self.y, rhs.y))
            .saturating_add(psx_math::int32::mul_q12_i32(self.z, rhs.z))
    }

    /// Every lane times the Q12 `factor`, through `mul_q12_i32` (so a
    /// product past `i32` saturates).
    #[inline]
    #[must_use]
    pub fn scale_q12(self, factor: i32) -> Self {
        Self {
            x: psx_math::int32::mul_q12_i32(self.x, factor),
            y: psx_math::int32::mul_q12_i32(self.y, factor),
            z: psx_math::int32::mul_q12_i32(self.z, factor),
        }
    }
}

/// 3×3 rotation / light matrix of `i16` in 1.3.12. Laid out row-major
/// so `m[row][col]` matches PSX-SPX notation (`M_rc`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
#[repr(C)]
pub struct Mat3I16 {
    /// Row-major 3×3 cells.
    pub m: [[i16; 3]; 3],
}

impl Mat3I16 {
    /// Zero matrix.
    pub const ZERO: Self = Self { m: [[0; 3]; 3] };

    /// Identity matrix scaled 1.0 in 1.3.12 (diagonal = 0x1000).
    pub const IDENTITY: Self = Self {
        m: [[0x1000, 0, 0], [0, 0x1000, 0], [0, 0, 0x1000]],
    };

    /// Access a row.
    #[inline(always)]
    pub const fn row(&self, i: usize) -> [i16; 3] {
        self.m[i]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_diagonal_is_one_in_1_3_12() {
        // 0x1000 = 4096 = 1.0 when divided by 4096.
        assert_eq!(Mat3I16::IDENTITY.m[0][0], 0x1000);
        assert_eq!(Mat3I16::IDENTITY.m[1][1], 0x1000);
        assert_eq!(Mat3I16::IDENTITY.m[2][2], 0x1000);
        // Off-diagonal zero.
        assert_eq!(Mat3I16::IDENTITY.m[0][1], 0);
        assert_eq!(Mat3I16::IDENTITY.m[1][2], 0);
    }

    #[test]
    fn vec3_i16_xy_packs_little_endian() {
        // x in low half, y in high half -- matches MTC2 expectation.
        let v = Vec3I16::new(-1, 2, 3);
        assert_eq!(v.xy_packed(), 0x0002_ffff);
    }

    // The games' own copies (Quake movement.rs, mover.rs, push.rs, entity.rs),
    // kept here as the oracle the shared methods must equal.
    fn game_add(l: Vec3I32, r: Vec3I32) -> Vec3I32 {
        Vec3I32 {
            x: l.x.saturating_add(r.x),
            y: l.y.saturating_add(r.y),
            z: l.z.saturating_add(r.z),
        }
    }

    fn game_sub(l: Vec3I32, r: Vec3I32) -> Vec3I32 {
        Vec3I32 {
            x: l.x.saturating_sub(r.x),
            y: l.y.saturating_sub(r.y),
            z: l.z.saturating_sub(r.z),
        }
    }

    fn game_dot(l: Vec3I32, r: Vec3I32) -> i32 {
        psx_math::int32::mul_q12_i32(l.x, r.x)
            .saturating_add(psx_math::int32::mul_q12_i32(l.y, r.y))
            .saturating_add(psx_math::int32::mul_q12_i32(l.z, r.z))
    }

    fn game_scale(v: Vec3I32, f: i32) -> Vec3I32 {
        Vec3I32 {
            x: psx_math::int32::mul_q12_i32(v.x, f),
            y: psx_math::int32::mul_q12_i32(v.y, f),
            z: psx_math::int32::mul_q12_i32(v.z, f),
        }
    }

    #[test]
    fn vec3_i32_saturating_arithmetic_equals_the_games_copies() {
        const LANES: [i32; 9] = [
            i32::MIN,
            i32::MIN + 1,
            -4096,
            -1,
            0,
            1,
            4095,
            i32::MAX - 1,
            i32::MAX,
        ];
        for &ax in &LANES {
            for &ay in &LANES {
                for &bz in &LANES {
                    let a = Vec3I32::new(ax, ay, bz);
                    let b = Vec3I32::new(bz, ax, ay);
                    assert_eq!(a.saturating_add(b), game_add(a, b));
                    assert_eq!(a.saturating_sub(b), game_sub(a, b));
                    assert_eq!(a.dot_q12(b), game_dot(a, b));
                    assert_eq!(a.scale_q12(ax), game_scale(a, ax));
                }
            }
        }
    }

    #[test]
    fn vec3_i32_saturating_ops_clamp_and_are_const() {
        const SUM: Vec3I32 =
            Vec3I32::new(i32::MAX, 1, i32::MIN).saturating_add(Vec3I32::new(1, 1, -1));
        assert_eq!(SUM, Vec3I32::new(i32::MAX, 2, i32::MIN));
        const DIFF: Vec3I32 = Vec3I32::new(i32::MIN, 5, 0).saturating_sub(Vec3I32::new(1, 2, 0));
        assert_eq!(DIFF, Vec3I32::new(i32::MIN, 3, 0));
    }

    #[test]
    fn vec3_i16_z_sign_extends() {
        let v = Vec3I16::new(0, 0, -1);
        assert_eq!(v.z_packed(), 0xffff_ffff);
    }

    #[test]
    fn vec3_i32_construct_and_fields() {
        let t = Vec3I32::new(100, 200, 300);
        assert_eq!(t.x, 100);
        assert_eq!(t.y, 200);
        assert_eq!(t.z, 300);
    }

    #[test]
    fn zero_constants_are_zero() {
        assert_eq!(Vec3I16::ZERO.x, 0);
        assert_eq!(Vec3I16::ZERO.y, 0);
        assert_eq!(Vec3I16::ZERO.z, 0);
        assert_eq!(Vec3I32::ZERO.x, 0);
        assert_eq!(Mat3I16::ZERO.m[0][0], 0);
    }
}
