//! Compute-shader encoding of the CPU rasterizer's triangle setup.
//!
//! Coverage and attribute planes come from
//! `emulator_core::gpu::tri_raster_setup` -- the SAME function the
//! CPU's flat and attribute-interpolating walkers consume, so the GPU
//! result is bit-exact by construction (it is not a hand-synced copy;
//! it used to be, and drifted). This module only reshapes that setup
//! for the shader:
//!
//! - one [`RowState`] per covered scanline: the `[left_x, right_x)`
//!   span the DDA produced (right-exclusive, unclamped -- the shader
//!   applies the draw-area clamp exactly like the CPU loop does), and
//! - one [`ScanlineConsts`]: the five attribute planes as
//!   `(dadx, dady, base)` u32 triples, where
//!   `attr(x, y) = (base + x*dadx + y*dady) >> 24` in wrapping u32
//!   math -- the same `tri_plane_eval` the CPU runs per pixel.

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
    let setup = emulator_core::gpu::tri_raster_setup(v, rgb, uv, require_attrs)?;
    let [pr, pg, pb, pu, pv] = setup.planes;

    // One RowState per covered scanline, in the exact order the CPU
    // walks them: segment 0 (top..mid), then segment 1 (mid..bottom).
    let y_min = setup.parts[0].0;
    let y_max = setup.parts[1].1 - 1;
    let mut rows = Vec::with_capacity((y_max - y_min + 1).max(0) as usize);
    for (y0, y1, mut lx, ls, mut rx, rs) in setup.parts {
        let mut y = y0;
        while y < y1 {
            rows.push(RowState {
                left_x: emulator_core::gpu::tri_span_x(lx),
                right_x: emulator_core::gpu::tri_span_x(rx),
            });
            lx += ls;
            rx += rs;
            y += 1;
        }
    }

    Some(ScanlineSetup {
        rows,
        consts: ScanlineConsts {
            y_min,
            y_max,
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
