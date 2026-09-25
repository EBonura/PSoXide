//! Error-bounded tessellation predicates for affine-textured PS1 polygons.
//!
//! The GPU interpolates texture coordinates linearly in screen space. Along a
//! projected edge from `a` to `b` (view depths `za`, `zb`, screen length `L`
//! pixels) the perspective-correct image of the 3D midpoint and the affine
//! screen midpoint differ by exactly
//!
//! ```text
//! e = L * |zb - za| / (2 * (za + zb))      screen pixels
//! ```
//!
//! That is the largest displacement on the edge, and the texel error is the
//! same expression with the edge's UV span in place of `L`. Halving an edge
//! halves `L` and roughly halves `|zb - za|`, so each bisection divides `e` by
//! about four.
//!
//! Splitting by `e` rather than by depth bands or texel error budgets what the
//! viewer sees (texels are not pixels: a minified far wall can be "8 texels"
//! off and look right, a magnified floor 1 texel off and swim), depends only
//! on an edge's two projected endpoints (two faces sharing an edge decide it
//! the same way), and needs no guest divide: everything fits `u32` for GTE
//! screen coordinates and SZ depths.
//!
//! Used by `classic_affine` (feature `classic-affine-lattice`) and the
//! GoldSrc ports' world emitters. Validated against PSoXide-emulator's
//! texture-warp probe (`gpu/warp_probe.rs`, `examples/warp_probe_check.rs`).

/// A projected vertex as the policy sees it: GTE screen position and SZ.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ScreenDepth {
    /// Screen X in pixels.
    pub x: i32,
    /// Screen Y in pixels.
    pub y: i32,
    /// View depth (GTE SZ units); non-positive means unknown or behind.
    pub z: i32,
}

/// Screen length of an edge, octagonal approximation (within 4 percent,
/// never more than 1 percent under the Euclidean length).
#[inline(always)]
pub fn screen_length(a: ScreenDepth, b: ScreenDepth) -> u32 {
    let dx = a.x.abs_diff(b.x).min(4095);
    let dy = a.y.abs_diff(b.y).min(4095);
    let (hi, lo) = if dx > dy { (dx, dy) } else { (dy, dx) };
    hi + ((lo * 3) >> 3)
}

/// Whether an edge's affine displacement exceeds `budget_q3` eighths of a
/// pixel after `level` bisections (each divides it by four).
#[inline(always)]
pub fn edge_exceeds(a: ScreenDepth, b: ScreenDepth, budget_q3: u32, level: u32) -> bool {
    if a.z <= 0 || b.z <= 0 {
        return false;
    }
    let za = (a.z as u32).min(u16::MAX as u32);
    let zb = (b.z as u32).min(u16::MAX as u32);
    // L * dz * 8 / (2 * (za + zb)) > budget * 4^level
    // L < 2^13, dz < 2^16 -> 2^29 * 4 = 2^31: fits u32.
    let lhs = screen_length(a, b) * za.abs_diff(zb) * 4;
    let rhs = (za + zb) * (budget_q3 << (2 * level));
    lhs > rhs
}

/// Bisection level (0, 1 or 2) that brings a displacement of
/// `len * |zb - za| / (2 (za + zb))` pixels within `budget_q3` eighths of a
/// pixel: two multiplies and shifted compares, no loop. `len` is a screen
/// length in pixels (clamp it below 8192); depths are GTE SZ values. A
/// non-positive depth means unknown and returns 0.
#[inline(always)]
pub fn error_level(len: u32, za: i32, zb: i32, budget_q3: u32) -> u8 {
    if za <= 0 || zb <= 0 {
        return 0;
    }
    let za = (za as u32).min(u16::MAX as u32);
    let zb = (zb as u32).min(u16::MAX as u32);
    // len < 2^13, |dz| < 2^16, times 4: under 2^31. (za + zb) < 2^17 times
    // an 8-bit budget, shifted by four: under 2^29.
    let lhs = len.min(8191) * za.abs_diff(zb) * 4;
    let rhs = (za + zb) * budget_q3;
    if lhs <= rhs {
        0
    } else if lhs <= rhs << 2 {
        1
    } else {
        2
    }
}

/// Screen length of the segment between two screen points (octagonal
/// approximation, see [`screen_length`]).
#[inline(always)]
pub fn screen_span(a: [i16; 2], b: [i16; 2]) -> u32 {
    let dx = (a[0].abs_diff(b[0]) as u32).min(4095);
    let dy = (a[1].abs_diff(b[1]) as u32).min(4095);
    let (hi, lo) = if dx > dy { (dx, dy) } else { (dy, dx) };
    hi + ((lo * 3) >> 3)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exact affine displacement at an edge midpoint, pixels.
    fn exact(a: ScreenDepth, b: ScreenDepth) -> f64 {
        let l = (((a.x - b.x) as f64).powi(2) + ((a.y - b.y) as f64).powi(2)).sqrt();
        l * (a.z - b.z).abs() as f64 / (2.0 * (a.z + b.z) as f64)
    }

    #[test]
    fn predicate_brackets_exact_error() {
        let a = ScreenDepth {
            x: 10,
            y: 200,
            z: 100,
        };
        let b = ScreenDepth {
            x: 300,
            y: 150,
            z: 900,
        };
        let e = exact(a, b); // ~117 px
        let q3 = (e * 8.0) as u32;
        assert!(edge_exceeds(a, b, q3 - q3 / 20, 0));
        assert!(!edge_exceeds(a, b, q3 + q3 / 20, 0));
    }

    #[test]
    fn equal_depth_never_splits() {
        assert_eq!(error_level(1800, 500, 500, 1), 0);
        let a = ScreenDepth {
            x: -900,
            y: 0,
            z: 500,
        };
        let b = ScreenDepth {
            x: 900,
            y: 0,
            z: 500,
        };
        assert!(!edge_exceeds(a, b, 1, 0));
    }

    #[test]
    fn levels_divide_error_by_four() {
        let (a, b) = ([0i16, 239], [0i16, 130]);
        let (za, zb) = (60, 600);
        let e = 109.0 * (zb - za) as f64 / (2.0 * (za + zb) as f64);
        let len = screen_span(a, b);
        assert_eq!(len, 109);
        assert_eq!(error_level(len, za, zb, (e * 8.0 * 1.05) as u32), 0);
        assert_eq!(error_level(len, za, zb, (e * 8.0 / 3.0) as u32), 1);
        assert_eq!(error_level(len, za, zb, (e * 8.0 / 10.0) as u32), 2);
        assert_eq!(error_level(len, 0, zb, 1), 0, "unknown depth never splits");
    }
}
