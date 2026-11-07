//! Integer cell lookup and the one floor/ceiling height function.
//!
//! The same code runs in the guest (collision), in the cook (light bake,
//! walkability) and in the editor (`GridWorld::floor_height_at`), so what is
//! drawn, baked and stood on cannot drift. It is scale agnostic: callers pass
//! corner heights and offsets in one unit and the sector edge in that unit,
//! runtime units in the guest, editor units in the editor.

/// Reciprocal for `x / S` as a multiply and a shift.
///
/// `cell = (x * inv) >> k`. A 16-bit reciprocal is not exact over a whole
/// room, so `k` grows with the largest local extent the level uses. Both the
/// cook and the guest derive it with [`Recip::for_sector`] from `S` and the
/// extent; nothing about it is stored in the file.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Recip {
    /// `ceil(2^k / S)`.
    pub inv: u32,
    /// Shift.
    pub k: u32,
    /// Sector edge the reciprocal is for.
    pub sector: u32,
}

impl Recip {
    /// Smallest exact reciprocal for sector size `sector` over local offsets
    /// `0 ..= extent`, or `None` when no shift in 16..=24 keeps the product in
    /// 32 bits and the quotient exact.
    ///
    /// `ceil(2^k / S)` is exact for every `x` with `x * e < 2^k`, where `e =
    /// inv * S - 2^k < S` is its rounding excess.
    pub fn for_sector(sector: u32, extent: u32) -> Option<Self> {
        if sector == 0 {
            return None;
        }
        for k in 16..=24u32 {
            let inv = (1u32 << k).div_ceil(sector);
            let excess = inv.checked_mul(sector)? - (1u32 << k);
            let exact = u32::checked_mul(extent, excess).is_some_and(|v| v < (1u32 << k));
            if exact && extent.checked_mul(inv).is_some() {
                return Some(Self { inv, k, sector });
            }
        }
        None
    }

    /// Cell index of local offset `x`.
    #[inline(always)]
    pub fn cell(&self, x: u32) -> u32 {
        (x * self.inv) >> self.k
    }
}

/// Reciprocal `ceil(2^24 / S)` used by the interpolation weights.
#[inline]
pub fn weight_recip(sector: u32) -> u32 {
    (1u32 << 24).div_ceil(sector.max(1))
}

/// Q12 interpolation weight of `amount` along a sector edge: exact at both
/// ends, so neighbouring cells agree on a shared edge.
#[inline(always)]
pub fn weight_q12(amount: u32, inv24: u32) -> i32 {
    let w = (amount * inv24 + 2048) >> 12;
    if w > 4096 {
        4096
    } else {
        w as i32
    }
}

/// Height of the triangle `second` (false: the one containing the NW corner
/// for a NW-SE split, the NW-NE-SW one for a NE-SW split) at offset
/// `(lx, lz)` inside a sector of edge `s`.
///
/// `h` holds the four corner heights `[NW, NE, SE, SW]`. `nesw` selects the
/// diagonal. Which triangle the point is in follows from the diagonal:
/// NW-SE splits at `x == z` (first triangle where `x >= z`), NE-SW splits at
/// `x + z == s` (first triangle where `x + z <= s`).
///
/// Corner deltas must stay within +-65535 (the cook refuses larger) so each
/// product fits `i32`. Error against exact rational arithmetic is under one unit per
/// 8192 of corner delta plus the final floor, two units at the extreme.
#[inline]
pub fn height_at(h: [i16; 4], nesw: bool, lx: u32, lz: u32, s: u32, inv24: u32) -> i32 {
    let (nw, ne, se, sw) = (
        i32::from(h[0]),
        i32::from(h[1]),
        i32::from(h[2]),
        i32::from(h[3]),
    );
    let wx = weight_q12(lx, inv24);
    let wz = weight_q12(lz, inv24);
    let (base, dx, dz, wx, wz) = if !nesw {
        if lx >= lz {
            // Triangle NW, NE, SE.
            (nw, ne - nw, se - ne, wx, wz)
        } else {
            // Triangle NW, SE, SW.
            (nw, se - sw, sw - nw, wx, wz)
        }
    } else if lx + lz <= s {
        // Triangle NW, NE, SW.
        (nw, ne - nw, sw - nw, wx, wz)
    } else {
        // Triangle NE, SE, SW: h = sw + (se - sw) wx + (ne - se) (1 - wz).
        (sw, se - sw, ne - se, wx, 4096 - wz)
    };
    base + ((dx * wx + dz * wz) >> 12)
}

/// Same as [`height_at`] but over `i32` corner heights, for callers whose
/// units are finer than `i16` (the editor works in editor units).
#[inline]
pub fn height_at_i32(h: [i32; 4], nesw: bool, lx: u32, lz: u32, s: u32, inv24: u32) -> i32 {
    let [nw, ne, se, sw] = h;
    let wx = weight_q12(lx, inv24);
    let wz = weight_q12(lz, inv24);
    let (base, dx, dz, wx, wz) = if !nesw {
        if lx >= lz {
            (nw, ne - nw, se - ne, wx, wz)
        } else {
            (nw, se - sw, sw - nw, wx, wz)
        }
    } else if lx + lz <= s {
        (nw, ne - nw, sw - nw, wx, wz)
    } else {
        (sw, se - sw, ne - se, wx, 4096 - wz)
    };
    base + ((dx * wx + dz * wz) >> 12)
}

/// Whether `(lx, lz)` is in the sector's first triangle for the split.
#[inline(always)]
pub fn in_first_triangle(nesw: bool, lx: u32, lz: u32, s: u32) -> bool {
    if nesw {
        lx + lz <= s
    } else {
        lx >= lz
    }
}

/// Upward normal component, Q12, of the triangle with the given corner
/// heights over a sector of edge `s`. `second` picks the triangle.
///
/// The plane is `h = base + a x + b z` with `a = dx / s`, `b = dz / s`, so its
/// normal is proportional to `(-dx, s, -dz)`.
pub fn triangle_normal_q12(h: [i16; 4], nesw: bool, second: bool, s: u32) -> [i16; 3] {
    let (nw, ne, se, sw) = (
        i32::from(h[0]),
        i32::from(h[1]),
        i32::from(h[2]),
        i32::from(h[3]),
    );
    let (dx, dz) = match (nesw, second) {
        (false, false) => (ne - nw, se - ne),
        (false, true) => (se - sw, sw - nw),
        (true, false) => (ne - nw, sw - nw),
        (true, true) => (se - sw, se - ne),
    };
    // Heights are room-local i16, so every delta is below 2^17 and the
    // squared length below 2^35 for extreme data; clamp the deltas so the
    // squares fit 32 bits. Real slopes are far below the clamp.
    let dx = dx.clamp(-16_384, 16_384);
    let dz = dz.clamp(-16_384, 16_384);
    let s = s as i32;
    let squared = dx * dx + dz * dz + s * s;
    let length = psx_math::int32::isqrt_i32(squared).max(1);
    let q = |v: i32| ((v * 4096) / length) as i16;
    let up = ((s * 4096) / length) as i16;
    let nz = q(-dz);
    [q(-dx), up, nz]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exact rational height of the triangle plane, floor division.
    fn reference(h: [i32; 4], nesw: bool, x: i64, z: i64, s: i64) -> i64 {
        let [nw, ne, se, sw] = h.map(i64::from);
        let first = if nesw { x + z <= s } else { x >= z };
        let num = match (nesw, first) {
            (false, true) => nw * s + (ne - nw) * x + (se - ne) * z,
            (false, false) => nw * s + (se - sw) * x + (sw - nw) * z,
            (true, true) => nw * s + (ne - nw) * x + (sw - nw) * z,
            (true, false) => sw * s + (se - sw) * x + (ne - se) * (s - z),
        };
        num.div_euclid(s)
    }

    #[test]
    fn reciprocal_is_exact_over_the_whole_extent() {
        for sector in [36u32, 56, 64, 80, 84, 85, 100, 127, 255] {
            let extent = sector * 128;
            let recip = Recip::for_sector(sector, extent).expect("a shift exists");
            for x in 0..=extent {
                assert_eq!(recip.cell(x), x / sector, "S={sector} x={x}");
            }
        }
    }

    #[test]
    fn a_sixteen_bit_reciprocal_would_not_be_enough() {
        // The counterexample from the design: S = 84, x = 32000.
        let inv = (1u32 << 16).div_ceil(84);
        assert_eq!((32_000 * inv) >> 16, 381);
        assert_eq!(32_000 / 84, 380);
        let recip = Recip::for_sector(84, 32_000).unwrap();
        assert_eq!(recip.cell(32_000), 380);
    }

    #[test]
    fn height_is_exact_at_corners_and_within_two_units_everywhere() {
        let sets: [[i32; 4]; 6] = [
            [0, 0, 0, 0],
            [0, 40, 80, 40],
            [0, 0, 64, 0],
            [100, -50, 30, 7],
            [-300, 300, -300, 300],
            [2000, -2000, 1999, -2000],
        ];
        for s in [36u32, 80, 84, 1280, 1344] {
            let inv24 = weight_recip(s);
            for h in sets {
                let h16 = h.map(|v| v as i16);
                for nesw in [false, true] {
                    for lx in (0..=s).step_by(((s / 20).max(1)) as usize).chain([s]) {
                        for lz in (0..=s).step_by(((s / 20).max(1)) as usize).chain([s]) {
                            let got = height_at(h16, nesw, lx, lz, s, inv24);
                            let want =
                                reference(h, nesw, i64::from(lx), i64::from(lz), i64::from(s));
                            assert!(
                                (i64::from(got) - want).abs() <= 2,
                                "S={s} {h:?} nesw={nesw} ({lx},{lz}): {got} vs {want}"
                            );
                            assert_eq!(got, height_at_i32(h, nesw, lx, lz, s, inv24));
                        }
                    }
                    let c = |x, z| height_at(h16, nesw, x, z, s, inv24);
                    assert_eq!(c(0, 0), h[0]);
                    assert_eq!(c(s, 0), h[1]);
                    assert_eq!(c(s, s), h[2]);
                    assert_eq!(c(0, s), h[3]);
                }
            }
        }
    }

    #[test]
    fn neighbouring_sectors_agree_on_a_shared_edge() {
        let s = 84u32;
        let inv24 = weight_recip(s);
        // East edge of A is the west edge of B with matching corners.
        let a = [0i16, 30, 50, 20];
        let b = [30i16, 90, 70, 50];
        for lz in 0..=s {
            for nesw in [false, true] {
                let east = height_at(a, nesw, s, lz, s, inv24);
                let west = height_at(b, nesw, 0, lz, s, inv24);
                assert_eq!(east, west, "lz={lz} nesw={nesw}");
            }
        }
    }

    #[test]
    fn flat_triangles_have_an_upright_normal() {
        let n = triangle_normal_q12([8; 4], false, false, 80);
        assert_eq!(n, [0, 4096, 0]);
        let slope = triangle_normal_q12([0, 0, 80, 80], false, false, 80);
        assert!(slope[1] < 4096 && slope[1] > 2867, "{slope:?}");
    }
}
