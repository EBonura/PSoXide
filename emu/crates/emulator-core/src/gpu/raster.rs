// ======================================================================
// Triangle extent cull
// ======================================================================
//
// The triangle rasterizer itself now lives in `gpu.rs`: the flat path in
// `rasterize_triangle` and the attributed paths in `for_each_tri_pixel`,
// both a center-sampled Mednafen/DuckStation DDA verified pixel-exact
// against real PS1 silicon (hardware-tests GPU read-back battery).
//
// The old Redux-parity scanline-delta rasterizer that lived here was
// retired once hardware proved Redux samples pixel CORNERS while silicon
// samples pixel CENTERS -- every drawn triangle's read-back diverged on
// real hardware (burn ledger HWB-005) while quads matched. What remains is
// the hardware extent rule, shared by every triangle path.

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
