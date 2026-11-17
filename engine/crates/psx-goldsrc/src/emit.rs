//! The world emitter both GoldSrc ports share: near-quad refinement and its telemetry.
//!
//! Each port names itself with a zero-sized type that implements [`Game`]. Every function here is
//! generic over it, so a port gets its own monomorphised copy with every hook inlined: no hook is
//! an indirect call. A hook exists only where the two ports really differ (Half-Life's view is the
//! full screen, Counter-Strike's a split-screen rectangle) or where the next stage of the emitter
//! still lives in the port.

use psx_engine::PrimitivePacketArena;
use psx_gpu::material::TexturedGouraudPacketMaterial;

use crate::render;

/// Back faces are culled (front faces wind with a negative screen cross product).
pub const CULL: bool = true;
/// Recursion depth after which a soft cell is emitted as a leaf without further LOD tests.
pub const SOFT_SPLIT_MAX_DEPTH: usize = 6;
/// Target on-screen span of a soft cell, in pixels.
pub const SOFT_SPLIT_TARGET_SPAN_PX: i32 = 96;
/// Texture-warp budget of a soft cell edge in Q3 pixels (`psx_engine::tess::error_level`).
pub const WARP_CELL_Q3: u32 = 96;

/// Screen corners of the triangle being emitted that the caller has already projected exactly,
/// or `None` per corner. While any is set a soft leaf takes the per-triangle path that reuses
/// them.
pub static mut SOFT_CACHED_SCREEN: [Option<(i16, i16)>; 3] = [None; 3];
/// Triangles the affine and soft refinement emitted this frame (telemetry).
pub static mut WORLD_AFFINE_SPLIT_TRIS: u32 = 0;

/// What a port supplies to the shared emitter.
pub trait Game {
    /// The view the port projects into.
    type View: render::View + 'static;
    /// Packets a refinement keeps free for the affine pass before it splits a cell.
    const AFFINE_PACKET_RESERVE: usize;

    /// The current view.
    ///
    /// # Safety
    /// Rendering is serialised: nothing mutates the view while the reference is live.
    unsafe fn view() -> &'static Self::View;

    /// Emit one projected triangle.
    ///
    /// # Safety
    /// As the port's own emitter: called only while the frame's packet arena is being filled.
    unsafe fn emit_screen_triangle(
        packets: &mut PrimitivePacketArena<'_>,
        a: &render::SVert,
        b: &render::SVert,
        c: &render::SVert,
        mat: TexturedGouraudPacketMaterial,
        texture_backdrop: bool,
        np: &mut usize,
    );

    /// Emit one view-space triangle through the port's clipper.
    ///
    /// # Safety
    /// As [`Game::emit_screen_triangle`].
    unsafe fn emit_cv_flat(
        packets: &mut PrimitivePacketArena<'_>,
        cv: [&render::CVert; 3],
        mat: TexturedGouraudPacketMaterial,
        np: &mut usize,
    );

    /// Replace the soft projection of a leaf's corners by exact screen positions the port
    /// already holds for them. The default keeps the soft projection.
    ///
    /// # Safety
    /// As [`Game::emit_screen_triangle`].
    #[inline(always)]
    unsafe fn snap_soft_corners(c: [&render::CVert; 4], p: &mut [render::SVert; 4]) {
        let _ = (c, p);
    }
}

/// View-space edge of a soft cell over budget: its screen length estimated
/// at the nearer end (an over-estimate, so the cell splits no later). All
/// 32-bit: the extent is clamped so `ext * SOFT_H` fits, one hardware divide.
#[inline(always)]
fn warp_cell_edge_over(a: &render::CVert, b: &render::CVert) -> bool {
    let (za, zb) = (a.v[2].max(render::NEAR_Z), b.v[2].max(render::NEAR_Z));
    let ext = (a.v[0] - b.v[0])
        .unsigned_abs()
        .max((a.v[1] - b.v[1]).unsigned_abs())
        .min(0xffff);
    let len = (ext * render::SOFT_H as u32 / za.min(zb) as u32).min(8191);
    psx_engine::tess::error_level(len, za, zb, WARP_CELL_Q3) != 0
}

/// Midpoint of a soft-cell edge. Sum-then-halve, so a shared edge produces
/// the identical vertex whichever neighbouring cell computes it.
#[inline(always)]
fn soft_cell_mid(a: &render::CVert, b: &render::CVert) -> render::CVert {
    render::CVert {
        v: [
            (a.v[0] + b.v[0]) / 2,
            (a.v[1] + b.v[1]) / 2,
            (a.v[2] + b.v[2]) / 2,
        ],
        rgb: (
            (a.rgb.0 + b.rgb.0) / 2,
            (a.rgb.1 + b.rgb.1) / 2,
            (a.rgb.2 + b.rgb.2) / 2,
        ),
        uv: ((a.uv.0 + b.uv.0) / 2, (a.uv.1 + b.uv.1) / 2),
    }
}

/// One cell of the near-quad refinement tree: screen-space LOD. A cell
/// splits while an axis still projects wider than the target pixel span, so
/// the leaf mesh approaches uniform screen-size cells and the level count
/// rises directly with proximity. Skewed cells split their LONG axis only:
/// a duct wall seen down its length halves into two long cells instead of
/// fanning into ever-thinner slivers (the black-streak coverage cracks).
///
/// # Safety
/// As [`Game::emit_screen_triangle`].
#[inline(never)]
pub unsafe fn emit_soft_cell<P: Game>(
    packets: &mut PrimitivePacketArena<'_>,
    c: [&render::CVert; 4],
    mat: TexturedGouraudPacketMaterial,
    depth: usize,
    np: &mut usize,
    target_span: i32,
) {
    // Prune whole subtrees the clipper would otherwise discard leaf by leaf:
    // a near-plane-crossing quad recursed into every child regardless of
    // visibility, and with most of the underfoot quad behind the camera that
    // was thousands of wasted midpoint/recursion nodes per frame -- the
    // measured 3 fps route collapse. All-out on ONE half-space is an exact
    // cull; the 45-degree lateral planes are conservative for the real FOV.
    // Near uses the clip plane itself (a cell wholly closer than NEAR_Z is
    // clipped to nothing anyway); vertical uses the exact display planes --
    // both prune strictly more than the old conservative forms while staying
    // pixel-identical.
    let nz = render::NEAR_Z;
    if c.iter().all(|v| v.v[2] < nz)
        || c.iter().all(|v| v.v[0] > v.v[2])
        || c.iter().all(|v| -v.v[0] > v.v[2])
        || render::quad_outside_vertical(P::view(), &c)
    {
        return;
    }
    // At maximum depth the span cannot affect tessellation. Keep the same
    // clipped leaves without computing edge lengths and projected LOD first.
    if depth >= SOFT_SPLIT_MAX_DEPTH {
        emit_soft_leaf::<P>(packets, c, mat, np);
        return;
    }
    let cheb = |a: &render::CVert, b: &render::CVert| -> i32 {
        (a.v[0] - b.v[0])
            .abs()
            .max((a.v[1] - b.v[1]).abs())
            .max((a.v[2] - b.v[2]).abs())
    };
    // World extent of each parameter axis (the longer of its two edges, so a
    // trapezoid cannot under-report).
    let du = cheb(c[0], c[1]).max(cheb(c[2], c[3]));
    let dv = cheb(c[0], c[2]).max(cheb(c[1], c[3]));
    // NB: the span floor stays at the renderer NEAR (not NEAR_Z) -- raising it
    // would coarsen split decisions for cells partly inside 8 units and change
    // the emitted tessellation.
    let near_z = c[0].v[2]
        .min(c[1].v[2])
        .min(c[2].v[2])
        .min(c[3].v[2])
        .max(render::NEAR_Z);
    // span_px ~ extent * H / z. An axis is oversize while it would project
    // wider than SOFT_SPLIT_TARGET_SPAN_PX at the cell's nearest depth.
    let over = |extent: i32| -> bool {
        extent.saturating_mul(render::SOFT_H) > near_z.saturating_mul(target_span)
    };
    let (split_u, split_v) = if WARP_CELL_Q3 != 0 {
        let big = |extent: i32| extent.saturating_mul(render::SOFT_H) > near_z.saturating_mul(250);
        (
            big(du) || warp_cell_edge_over(c[0], c[1]) || warp_cell_edge_over(c[2], c[3]),
            big(dv) || warp_cell_edge_over(c[0], c[2]) || warp_cell_edge_over(c[1], c[3]),
        )
    } else {
        (over(du), over(dv))
    };

    if (split_u || split_v) && packets.remaining() >= P::AFFINE_PACKET_RESERVE + 16 {
        // Anisotropic: when one axis is oversize (or twice the other), halve
        // it alone. Only near-square oversize cells pay the full 2x2.
        if split_u && (!split_v || du >= dv.saturating_mul(2)) {
            let top = soft_cell_mid(c[0], c[1]);
            let bottom = soft_cell_mid(c[2], c[3]);
            emit_soft_cell::<P>(
                packets,
                [c[0], &top, c[2], &bottom],
                mat,
                depth + 1,
                np,
                target_span,
            );
            emit_soft_cell::<P>(
                packets,
                [&top, c[1], &bottom, c[3]],
                mat,
                depth + 1,
                np,
                target_span,
            );
            return;
        }
        if split_v && (!split_u || dv >= du.saturating_mul(2)) {
            let left = soft_cell_mid(c[0], c[2]);
            let right = soft_cell_mid(c[1], c[3]);
            emit_soft_cell::<P>(
                packets,
                [c[0], c[1], &left, &right],
                mat,
                depth + 1,
                np,
                target_span,
            );
            emit_soft_cell::<P>(
                packets,
                [&left, &right, c[2], c[3]],
                mat,
                depth + 1,
                np,
                target_span,
            );
            return;
        }
        let top = soft_cell_mid(c[0], c[1]);
        let left = soft_cell_mid(c[0], c[2]);
        let right = soft_cell_mid(c[1], c[3]);
        let bottom = soft_cell_mid(c[2], c[3]);
        let center = soft_cell_mid(&top, &bottom);
        emit_soft_cell::<P>(
            packets,
            [c[0], &top, &left, &center],
            mat,
            depth + 1,
            np,
            target_span,
        );
        emit_soft_cell::<P>(
            packets,
            [&top, c[1], &center, &right],
            mat,
            depth + 1,
            np,
            target_span,
        );
        emit_soft_cell::<P>(
            packets,
            [&left, &center, c[2], &bottom],
            mat,
            depth + 1,
            np,
            target_span,
        );
        emit_soft_cell::<P>(
            packets,
            [&center, &right, &bottom, c[3]],
            mat,
            depth + 1,
            np,
            target_span,
        );
        return;
    }
    // The two hardware triangles of a GT4 (v0,v1,v2) and (v1,v3,v2).
    emit_soft_leaf::<P>(packets, c, mat, np);
}

/// Project the four unique corners once while retaining the two independent
/// triangles and their original ordering keys. Clipped/cached-corner cases
/// still use the original per-triangle path.
#[inline(never)]
unsafe fn emit_soft_leaf<P: Game>(
    packets: &mut PrimitivePacketArena<'_>,
    c: [&render::CVert; 4],
    mat: TexturedGouraudPacketMaterial,
    np: &mut usize,
) {
    if (*core::ptr::addr_of!(SOFT_CACHED_SCREEN))
        .iter()
        .all(Option::is_none)
        && c.iter().all(|v| v.v[2] >= render::NEAR_Z)
    {
        let mut p = [
            render::project_soft(P::view(), c[0]),
            render::project_soft(P::view(), c[1]),
            render::project_soft(P::view(), c[2]),
            render::project_soft(P::view(), c[3]),
        ];
        P::snap_soft_corners(c, &mut p);
        if p.iter().all(render::in_band) {
            let before = *np;
            for t in [[0usize, 1, 2], [1, 3, 2]] {
                // By reference: three 32-byte SVert copies per triangle lower
                // to memcpy calls under the sample profile's size mode.
                let (a, b, c) = (&p[t[0]], &p[t[1]], &p[t[2]]);
                // Inside the guard band (1000 by 500 px) the products fit i32.
                let cr = (b.x - a.x) * (c.y - a.y) - (c.x - a.x) * (b.y - a.y);
                if !CULL || cr < 0 {
                    P::emit_screen_triangle(packets, a, b, c, mat, false, np);
                }
            }
            WORLD_AFFINE_SPLIT_TRIS =
                WORLD_AFFINE_SPLIT_TRIS.saturating_add(np.saturating_sub(before) as u32);
            return;
        }
    }
    P::emit_cv_flat(packets, [c[0], c[1], c[2]], mat, np);
    P::emit_cv_flat(packets, [c[1], c[3], c[2]], mat, np);
}

/// Buckets of the affine error histograms: one texel each, the last one 127 texels and over.
pub const AFFINE_ERROR_BUCKETS: usize = 128;

/// The 50th, 95th and 99th percentile of `total` samples counted in `histogram`, in Q8 texels
/// (a bucket's lower edge). Telemetry only.
#[inline(never)]
#[must_use]
pub fn affine_error_percentiles(histogram: &[u16; AFFINE_ERROR_BUCKETS], total: u32) -> [u32; 3] {
    let percentile = |numerator: u32, denominator: u32| -> u32 {
        let target = total.saturating_mul(numerator).div_ceil(denominator);
        let mut cumulative = 0u32;
        let mut i = 0usize;
        while i < AFFINE_ERROR_BUCKETS {
            cumulative = cumulative.saturating_add(histogram[i] as u32);
            if cumulative >= target {
                return (i as u32) * 256;
            }
            i += 1;
        }
        ((AFFINE_ERROR_BUCKETS - 1) as u32) * 256
    };
    [
        percentile(50, 100),
        percentile(95, 100),
        percentile(99, 100),
    ]
}
