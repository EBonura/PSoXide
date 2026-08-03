// ======================================================================
// Triangle rasterization: extent cull + the shared setup
// ======================================================================
//
// Home of the silicon-verified triangle coverage/interpolation math:
// the hardware extent rule, the center-sampled Q32.32 DDA setup
// (`tri_raster_setup`), and the attributed per-pixel walker
// (`for_each_tri_pixel`). The `Gpu` methods in `gpu.rs` (flat
// `rasterize_triangle`, the shaded/textured paths) and
// psx-gpu-render's compute-span encoder all consume the SAME setup,
// so CPU and GPU coverage cannot drift.
//
// The old Redux-parity scanline-delta rasterizer that lived here was
// retired once hardware proved Redux samples pixel CORNERS while
// silicon samples pixel CENTERS (hardware-tests GPU read-back
// battery).

/// Hardware extent rule: any triangle whose vertex pairs span more than 1023
/// pixels horizontally or 511 vertically is silently dropped on real PS1
/// hardware. Off-screen geometry coming out of projection lands here
/// constantly -- without this gate it rasterises as a giant garbage smear
/// instead of being culled.
///
/// The check is per-edge, not bounding-box: hardware compares each pair of
/// vertices independently. Quads are already split into two triangles by the
/// caller, so each half is gated separately -- matching hardware behaviour
/// where one half of a quad can survive while the other gets dropped.
pub(super) fn triangle_exceeds_hw_extent(v0: (i32, i32), v1: (i32, i32), v2: (i32, i32)) -> bool {
    const MAX_DX: i32 = 1023;
    const MAX_DY: i32 = 511;
    let edges = [(v0, v1), (v1, v2), (v2, v0)];
    edges
        .iter()
        .any(|(a, b)| (a.0 - b.0).abs() > MAX_DX || (a.1 - b.1).abs() > MAX_DY)
}

/// Rasterize a triangle with PS1-silicon coverage AND attribute
/// interpolation, calling `plot(x, y, r, g, b, u, v)` for every covered
/// pixel. Coverage is the center-sampled DDA; attributes use the
/// determinant-plane interpolation with the exact `tl` (top-left) anchor,
/// so the integer-truncated gradients land bit-identically to hardware.
/// The coverage/interpolation RULE is the documented PS1 (PSX-SPX) behavior;
/// this implementation was written from that public hardware documentation
/// and is verified pixel-exact against real silicon (hardware-tests GPU
/// read-back battery).
/// The caller's closure owns texture sampling, dither, blend and the
/// actual VRAM write -- this just supplies coverage and the per-pixel
/// interpolated R/G/B/U/V.
///
/// Caller must apply the polygon-too-large extent cull first; this assumes the
/// triangle is drawable.
pub(super) fn for_each_tri_pixel(
    v: [(i32, i32); 3],
    rgb: [(i32, i32, i32); 3],
    uv: [(i32, i32); 3],
    draw_top: i32,
    draw_bottom: i32,
    draw_left: i32,
    draw_right: i32,
    mut plot: impl FnMut(i32, i32, u8, u8, u8, u8, u8),
) {
    let Some(setup) = tri_raster_setup(v, rgb, uv, true) else {
        return;
    };
    let [pr, pg, pb, pu, pv] = setup.planes;
    for (y0, y1, mut lx, ls, mut rx, rs) in setup.parts {
        let mut y = y0;
        while y < y1 {
            if y >= draw_top && y <= draw_bottom {
                let xs = tri_span_x(lx).max(draw_left);
                let xe = tri_span_x(rx).min(draw_right + 1); // right-exclusive
                let mut x = xs;
                while x < xe {
                    plot(
                        x,
                        y,
                        tri_plane_eval(pr, x, y),
                        tri_plane_eval(pg, x, y),
                        tri_plane_eval(pb, x, y),
                        tri_plane_eval(pu, x, y),
                        tri_plane_eval(pv, x, y),
                    );
                    x += 1;
                }
            }
            lx += ls;
            rx += rs;
            y += 1;
        }
    }
}

/// The CPU rasterizer's complete triangle setup: top-left attribute
/// anchor from the ORIGINAL vertex order, Y-sort, determinant planes,
/// and the center-sampled Q32.32 coverage DDA. This is the SINGLE
/// source of truth -- the flat and attribute-interpolating CPU walkers
/// above/below consume it, and `psx-gpu-render::scanline` builds the
/// compute shader's row spans + plane uniforms from the same call, so
/// CPU and GPU coverage/interpolation cannot drift (they used to be
/// three hand-synced copies; two of this session's parity bugs lived
/// in exactly that drift).
pub struct TriRasterSetup {
    /// `(dadx, dady, base)` per channel, order `[r, g, b, u, v]`;
    /// `attr(x, y) = (base + x*dadx + y*dady) >> 24` in wrapping u32
    /// math (see [`tri_plane_eval`]). All zeros when the determinant
    /// is zero and the caller passed `require_attrs = false`.
    pub planes: [(u32, u32, u32); 5],
    /// Two Y-segments `(y0, y1, left_x, left_step, right_x,
    /// right_step)` in Q32.32: walk `y0..y1`, span is
    /// `[tri_span_x(left), tri_span_x(right))`, stepping both edges
    /// per row. Segment 0 is `top..mid`, segment 1 `mid..bottom`.
    pub parts: [(i32, i32, i64, i64, i64, i64); 2],
}

/// Evaluate one attribute plane at a pixel -- the PS1 GPU's
/// integer-truncated gradient rule.
#[inline]
pub fn tri_plane_eval(p: (u32, u32, u32), x: i32, y: i32) -> u8 {
    (p.2.wrapping_add((x as u32).wrapping_mul(p.0))
        .wrapping_add((y as u32).wrapping_mul(p.1))
        >> 24) as u8
}

/// Convert a Q32.32 span coordinate to its pixel X.
#[inline]
pub fn tri_span_x(xfp: i64) -> i32 {
    (xfp >> 32) as i32
}

/// Build the triangle setup, or `None` when the CPU draws nothing:
/// zero vertical extent always, zero determinant (collinear vertices)
/// only when `require_attrs` is set. Attribute-interpolating
/// primitives bail on a zero determinant; the flat mono path has no
/// determinant check and still walks the DDA, so flat callers pass
/// `require_attrs = false` (planes come back as zeros, never read).
///
/// `v` must be in ORIGINAL submission order: the top-left anchor is
/// derived from that order before the Y-sort and is bit-significant
/// because the gradients are integer-truncated.
pub fn tri_raster_setup(
    mut v: [(i32, i32); 3],
    mut rgb: [(i32, i32, i32); 3],
    mut uv: [(i32, i32); 3],
    require_attrs: bool,
) -> Option<TriRasterSetup> {
    // Top-left vertex (attribute-interpolation anchor), carried
    // through the Y-sort swaps.
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
        return None;
    }
    let det =
        ((b.0 - a.0) as i64) * ((c.1 - b.1) as i64) - ((c.0 - b.0) as i64) * ((b.1 - a.1) as i64);
    if require_attrs && det == 0 {
        return None;
    }
    let anchor = v[tl];
    // Determinant-plane for one attribute channel -> (dadx, dady, base) as u32.
    // Matches the PS1 GPU's attribute-step scaling (Q12 + Q12 post-shift) and
    // the Init/StepX(-anchor)/StepY(-anchor) origin fold, in u32 wrapping math.
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
    let planes = [
        mk(rgb[0].0, rgb[1].0, rgb[2].0, rgb[tl].0),
        mk(rgb[0].1, rgb[1].1, rgb[2].1, rgb[tl].1),
        mk(rgb[0].2, rgb[1].2, rgb[2].2, rgb[tl].2),
        mk(uv[0].0, uv[1].0, uv[2].0, uv[tl].0),
        mk(uv[0].1, uv[1].1, uv[2].1, uv[tl].1),
    ];

    // Center-sampled coverage DDA, Q32.32, half-pixel edge bias.
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
    let parts = if right_facing {
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
    Some(TriRasterSetup { planes, parts })
}
