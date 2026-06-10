//! Host-side mirror of the CPU rasterizer's silicon-matched triangle
//! coverage and attribute interpolation
//! (`emulator-core/src/gpu.rs::for_each_tri_pixel`).
//!
//! The CPU rasterizer was re-tuned to real PS1 silicon (hardware-tests
//! GPU read-back battery, ledger HWB-006): coverage is the
//! center-sampled Mednafen/DuckStation DDA walked in Q32.32, and
//! attributes (R/G/B/U/V) come from a determinant-plane equation
//! evaluated per pixel in wrapping u32 arithmetic with a top-left
//! anchor. This module re-runs exactly that setup on the host and
//! ships two things to the compute shader:
//!
//! - one [`RowState`] per covered scanline: the `[left_x, right_x)`
//!   span the DDA produced (right-exclusive, unclamped -- the shader
//!   applies the draw-area clamp exactly like the CPU loop does), and
//! - one [`ScanlineConsts`]: the five attribute planes as
//!   `(dadx, dady, base)` u32 triples, where
//!   `attr(x, y) = (base + x*dadx + y*dady) >> 24` in wrapping u32
//!   math -- the same `eval` the CPU runs per pixel, so the GPU result
//!   is bit-exact by construction.
//!
//! **DO NOT MODIFY** the math here without keeping
//! `emulator-core::gpu::for_each_tri_pixel` (and the flat
//! `rasterize_triangle` DDA) in sync -- the anchor selection, the
//! half-pixel edge bias and the step rounding were all verified
//! against recorded silicon hashes.

use bytemuck::{Pod, Zeroable};

/// One scanline's coverage span, `[left_x, right_x)` (right-exclusive),
/// exactly as the CPU DDA produced it (no draw-area clamping -- the
/// shader handles that per pixel, mirroring the CPU loop).
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct RowState {
    /// First covered pixel of the row (inclusive).
    pub left_x: i32,
    /// One past the last covered pixel of the row (exclusive).
    pub right_x: i32,
}

/// Per-primitive constants: the covered scanline range plus the five
/// attribute planes. `attr(x, y) = (base + u32(x)*dadx + u32(y)*dady)
/// >> 24` in wrapping u32 arithmetic, matching the CPU's `eval`.
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct ScanlineConsts {
    /// First covered scanline (the Y-sorted top vertex's y).
    pub y_min: i32,
    /// Last covered scanline, inclusive (`bottom vertex y - 1`).
    pub y_max: i32,
    pub _pad0: u32,
    pub _pad1: u32,
    pub r_dadx: u32,
    pub r_dady: u32,
    pub r_base: u32,
    pub _pad2: u32,
    pub g_dadx: u32,
    pub g_dady: u32,
    pub g_base: u32,
    pub _pad3: u32,
    pub b_dadx: u32,
    pub b_dady: u32,
    pub b_base: u32,
    pub _pad4: u32,
    pub u_dadx: u32,
    pub u_dady: u32,
    pub u_base: u32,
    pub _pad5: u32,
    pub v_dadx: u32,
    pub v_dady: u32,
    pub v_base: u32,
    pub _pad6: u32,
}

/// Bundles host-computed per-row spans + per-primitive constants.
pub struct ScanlineSetup {
    /// Length matches `consts.y_max - consts.y_min + 1`. Indexed by
    /// `(py - y_min)` from the shader.
    pub rows: Vec<RowState>,
    pub consts: ScanlineConsts,
}

/// Run the CPU rasterizer's triangle setup for a primitive.
///
/// `v` must be the vertices in ORIGINAL submission order (the
/// top-left attribute anchor is derived from that order before the
/// Y-sort, and it is bit-significant). `uv`/`rgb` carry the matching
/// per-vertex attributes; pass zeros for channels the primitive does
/// not interpolate.
///
/// Returns `None` when the CPU draws nothing:
/// - zero vertical extent (`top.y == bottom.y`), always, or
/// - zero determinant (collinear vertices) when `require_attrs` is
///   set. Attribute-interpolating primitives (shaded/textured) bail
///   on a zero determinant like `for_each_tri_pixel`; the flat mono
///   path has no determinant check and still walks the DDA, so mono
///   callers pass `require_attrs = false`.
pub fn build_setup(
    v: [(i32, i32); 3],
    uv: [(i32, i32); 3],
    rgb: [(i32, i32, i32); 3],
    require_attrs: bool,
) -> Option<ScanlineSetup> {
    let mut v = v;
    let mut rgb = rgb;
    let mut uv = uv;

    // Top-left vertex (attribute-interpolation anchor): computed from
    // the ORIGINAL vertex order, then carried through the Y-sort
    // swaps. Mirrors `for_each_tri_pixel` exactly.
    let (o0, o1, o2) = (v[0], v[1], v[2]);
    let mut tl: u32 = if o1.0 <= o0.0 {
        if o2.0 <= o1.0 {
            4
        } else {
            2
        }
    } else if o2.0 < o0.0 {
        4
    } else {
        1
    };
    macro_rules! swap_attr {
        ($i:expr, $j:expr) => {{
            v.swap($i, $j);
            rgb.swap($i, $j);
            uv.swap($i, $j);
        }};
    }
    if v[2].1 < v[1].1 {
        swap_attr!(2, 1);
        tl = ((tl >> 1) & 0x2) | ((tl << 1) & 0x4) | (tl & 0x1);
    }
    if v[1].1 < v[0].1 {
        swap_attr!(1, 0);
        tl = ((tl >> 1) & 0x1) | ((tl << 1) & 0x2) | (tl & 0x4);
    }
    if v[2].1 < v[1].1 {
        swap_attr!(2, 1);
        tl = ((tl >> 1) & 0x2) | ((tl << 1) & 0x4) | (tl & 0x1);
    }
    let tl = (tl >> 1) as usize;

    let (a, b, c) = (v[0], v[1], v[2]); // sorted top, middle, bottom by Y
    if a.1 == c.1 {
        return None; // zero vertical extent
    }
    let det =
        ((b.0 - a.0) as i64) * ((c.1 - b.1) as i64) - ((c.0 - b.0) as i64) * ((b.1 - a.1) as i64);
    if require_attrs && det == 0 {
        return None;
    }

    // Determinant-plane for one attribute channel -> (dadx, dady,
    // base), where attr(x, y) = (base + x*dadx + y*dady) >> 24 (low
    // byte). u32 wrapping math, identical to the CPU's `mk`. For the
    // mono path with det == 0 the planes are never read; emit zeros.
    let anchor = v[tl];
    let mk = |a0: i32, a1: i32, a2: i32, anchor_a: i32| -> (u32, u32, u32) {
        if det == 0 {
            return (0, 0, 0);
        }
        let num_dx =
            ((a1 - a0) as i64) * ((c.1 - b.1) as i64) - ((a2 - a1) as i64) * ((b.1 - a.1) as i64);
        let num_dy =
            ((b.0 - a.0) as i64) * ((a2 - a1) as i64) - ((c.0 - b.0) as i64) * ((a1 - a0) as i64);
        let dadx = ((num_dx * 4096 / det) as u32).wrapping_shl(12);
        let dady = ((num_dy * 4096 / det) as u32).wrapping_shl(12);
        let base = ((anchor_a as u32) << 24)
            .wrapping_add(1u32 << 23)
            .wrapping_sub((anchor.0 as u32).wrapping_mul(dadx))
            .wrapping_sub((anchor.1 as u32).wrapping_mul(dady));
        (dadx, dady, base)
    };
    let pr = mk(rgb[0].0, rgb[1].0, rgb[2].0, rgb[tl].0);
    let pg = mk(rgb[0].1, rgb[1].1, rgb[2].1, rgb[tl].1);
    let pb = mk(rgb[0].2, rgb[1].2, rgb[2].2, rgb[tl].2);
    let pu = mk(uv[0].0, uv[1].0, uv[2].0, uv[tl].0);
    let pv = mk(uv[0].1, uv[1].1, uv[2].1, uv[tl].1);

    // Center-sampled coverage DDA, Q32.32 (identical to the CPU).
    let makefp = |x: i32| -> i64 { ((x as i64) << 32) + ((1i64 << 32) - (1 << 11)) };
    let makestep = |dx: i32, dy: i32| -> i64 {
        let bias = if dx < 0 {
            -((dy - 1) as i64)
        } else if dx > 0 {
            (dy - 1) as i64
        } else {
            0
        };
        (((dx as i64) << 32) + bias) / (dy as i64)
    };
    let unfp = |xfp: i64| -> i32 { (xfp >> 32) as i32 };
    let base_coord = makefp(a.0);
    let base_step = makestep(c.0 - a.0, c.1 - a.1);
    let bound_us = if b.1 == a.1 {
        0
    } else {
        makestep(b.0 - a.0, b.1 - a.1)
    };
    let bound_ls = if c.1 == b.1 {
        0
    } else {
        makestep(c.0 - b.0, c.1 - b.1)
    };
    let right_facing = if b.1 == a.1 {
        b.0 > a.0
    } else {
        bound_us > base_step
    };
    let long_at_b = base_coord + ((b.1 - a.1) as i64) * base_step;
    let parts: [(i32, i32, i64, i64, i64, i64); 2] = if right_facing {
        [
            (a.1, b.1, base_coord, base_step, makefp(a.0), bound_us),
            (b.1, c.1, long_at_b, base_step, makefp(b.0), bound_ls),
        ]
    } else {
        [
            (a.1, b.1, makefp(a.0), bound_us, base_coord, base_step),
            (b.1, c.1, makefp(b.0), bound_ls, long_at_b, base_step),
        ]
    };
    let mut rows = Vec::with_capacity((c.1 - a.1) as usize);
    for (y0, y1, mut lx, ls, mut rx, rs) in parts {
        let mut y = y0;
        while y < y1 {
            rows.push(RowState {
                left_x: unfp(lx),
                right_x: unfp(rx),
            });
            lx += ls;
            rx += rs;
            y += 1;
        }
    }

    Some(ScanlineSetup {
        rows,
        consts: ScanlineConsts {
            y_min: a.1,
            y_max: c.1 - 1,
            _pad0: 0,
            _pad1: 0,
            r_dadx: pr.0,
            r_dady: pr.1,
            r_base: pr.2,
            _pad2: 0,
            g_dadx: pg.0,
            g_dady: pg.1,
            g_base: pg.2,
            _pad3: 0,
            b_dadx: pb.0,
            b_dady: pb.1,
            b_base: pb.2,
            _pad4: 0,
            u_dadx: pu.0,
            u_dady: pu.1,
            u_base: pu.2,
            _pad5: 0,
            v_dadx: pv.0,
            v_dady: pv.1,
            v_base: pv.2,
            _pad6: 0,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Host-side replica of the shader's plane evaluation.
    fn eval(dadx: u32, dady: u32, base: u32, x: i32, y: i32) -> u8 {
        (base
            .wrapping_add((x as u32).wrapping_mul(dadx))
            .wrapping_add((y as u32).wrapping_mul(dady))
            >> 24) as u8
    }

    #[test]
    fn struct_sizes_pinned() {
        assert_eq!(std::mem::size_of::<RowState>(), 8);
        assert_eq!(std::mem::size_of::<ScanlineConsts>(), 96);
    }

    #[test]
    fn build_setup_axis_aligned_right_triangle() {
        // (10,10)-(50,10)-(10,50): top edge horizontal, left edge
        // vertical at x=10.
        let setup = build_setup(
            [(10, 10), (50, 10), (10, 50)],
            [(0, 0), (32, 0), (0, 32)],
            [(0, 0, 0); 3],
            true,
        )
        .expect("setup");
        assert_eq!(setup.consts.y_min, 10);
        assert_eq!(setup.consts.y_max, 49);
        assert_eq!(setup.rows.len(), 40);
        // The vertical left edge keeps left_x at 10 on every row.
        assert!(setup.rows.iter().all(|r| r.left_x == 10));
        // Top row spans to the top-right vertex (right-exclusive).
        assert_eq!(setup.rows[0].right_x, 50);
        // Spans shrink monotonically down the hypotenuse.
        for w in setup.rows.windows(2) {
            assert!(w[1].right_x <= w[0].right_x);
        }
        // The plane anchors the attributes at the top-left vertex.
        let c = &setup.consts;
        assert_eq!(eval(c.u_dadx, c.u_dady, c.u_base, 10, 10), 0);
        assert_eq!(eval(c.v_dadx, c.v_dady, c.v_base, 10, 10), 0);
    }

    #[test]
    fn build_setup_degenerate_triangle_returns_none() {
        // Zero vertical extent is always rejected.
        let zero_h = build_setup(
            [(0, 5), (10, 5), (20, 5)],
            [(0, 0); 3],
            [(0, 0, 0); 3],
            false,
        );
        assert!(zero_h.is_none());
        // Collinear (zero determinant) with vertical extent: rejected
        // only when attributes are interpolated, like the CPU's
        // for_each_tri_pixel; the flat mono DDA still walks it.
        let collinear = [(0, 0), (5, 5), (10, 10)];
        assert!(build_setup(collinear, [(0, 0); 3], [(0, 0, 0); 3], true).is_none());
        assert!(build_setup(collinear, [(0, 0); 3], [(0, 0, 0); 3], false).is_some());
    }
}
