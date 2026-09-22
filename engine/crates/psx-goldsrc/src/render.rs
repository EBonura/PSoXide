//! Near-plane + guard-band clipping and software re-projection, ported from the
//! proven oot-psx `room.rs`. Triangles that straddle the camera near plane are
//! clipped in view space (Sutherland-Hodgman) and re-projected in software
//! instead of being dropped (which made geometry pop out near walls).
//!
//! Fixed-point only: the mipsel target miscompiles `i64 / runtime`, so the
//! ratio/interpolation helpers keep every intermediate in 32 bits.

use psx_engine::attributed_clip::{
    clip_convex_plane, lerp_q12_i32_wide as mix, ratio_q12_i32 as t_q12, AttributedClipPlane,
    ClipTraversal,
};
use psx_engine::projection::{
    half_space_outcode5, triangle_outside_common_plane, ScreenClipBounds,
};

/// View-space near plane (world units). Eight keeps the projection plane inside
/// the standing player's 32-unit hull at the normal 90-degree FOV, preventing
/// close walls from being cut open when the player pitches the camera.
pub const NEAR_Z: i32 = 8;
/// Default software projection focal length (matches the normal 90-degree
/// `set_projection_plane(H_PROJ)`). Crossbow zoom swaps the active focal
/// length, while this constant retains the fast reciprocal table's shape.
pub const SOFT_H: i32 = 160;

// Q12 H/z no longer fits u16 below z=11. Keep the old compact LUT from 16
// upward; the rare 8..15 band uses the overflow-safe H*x/z projection below.
pub const CLOSE_LUT_Z0: i32 = 16;
/// Maximum separation between the two local OT keys allowed to share one GT4
/// link. With four-unit OT buckets this caps a paired refined cell at sixteen
/// units of hidden child-depth disagreement.
pub const REFINED_PAIR_MAX_OTZ_DELTA: usize = 4;

/// The two triangles share a-c. Reuse that edge's bounds when checking b/d.
/// Inputs use the GPU's packed signed-16-bit XY format.
#[inline(never)]
pub fn quad_fits_gpu_xy(a: u32, b: u32, c: u32, d: u32) -> bool {
    let axis_fits = |a: i32, b: i32, c: i32, d: i32, limit: i32| {
        let low = a.min(c);
        let high = a.max(c);
        let span = high - low;
        if span > limit {
            return false;
        }
        let lower = high - limit;
        let width = (2 * limit - span) as u32;
        (b - lower) as u32 <= width && (d - lower) as u32 <= width
    };
    axis_fits(
        a as i16 as i32,
        b as i16 as i32,
        c as i16 as i32,
        d as i16 as i32,
        1023,
    ) && axis_fits(
        (a >> 16) as i16 as i32,
        (b >> 16) as i16 as i32,
        (c >> 16) as i16 as i32,
        (d >> 16) as i16 as i32,
        511,
    )
}

/// Lossless compact storage for ordering-table indices in persistent packet
/// caches. The live renderer currently has more than 256 buckets, so u8 is
/// not wide enough even though individual cache packet counts are small.
#[inline(always)]
pub const fn cache_otz_u16(otz: usize) -> u16 {
    otz as u16
}

#[inline(always)]
pub const fn refined_pair_depth_compatible(first: usize, second: usize) -> bool {
    let delta = first.abs_diff(second);
    delta <= REFINED_PAIR_MAX_OTZ_DELTA
}
const CLOSE_INV_Q12: [u16; (SOFT_H / 2 - CLOSE_LUT_Z0) as usize] = {
    let mut out = [0u16; (SOFT_H / 2 - CLOSE_LUT_Z0) as usize];
    let mut i = 0;
    while i < out.len() {
        out[i] = ((SOFT_H << 12) / (CLOSE_LUT_Z0 + i as i32)) as u16;
        i += 1;
    }
    out
};
/// Guard band: keep screen coords within the GPU's span limits (±1023 / 511).
const GX0: i32 = -340;
const GX1: i32 = 660;
const GY0: i32 = -130;
const GY1: i32 = 370;
const GUARD_BOUNDS: ScreenClipBounds = ScreenClipBounds::new(GX0, GX1, GY0, GY1);
// The soft fallback clips in view space to a one-pixel apron around the real
// display before it starts affine correction. Uniform world-space subdivision
// of a polygon that projects thousands of pixels off-screen wastes almost all
// children outside the image and leaves the nearest visible child enormous.
// Frustum clipping first creates geometrically correct UVs at the image edge.

/// Replace the material-dependent opcode flags on a cached textured Gouraud
/// packet without changing its geometry type. Texture animation materials are
/// stored as triangle templates (`0x34`/`0x36`), while a cached world packet
/// may be a quad (`0x3c`/`0x3e`); clearing the quad bit corrupts the GP0 stream
/// and leaves the second half of the surface undrawn.
#[inline(always)]
pub fn patch_textured_gouraud_command(current: u32, material: u32) -> u32 {
    const COMMAND_MASK: u32 = 0xff00_0000;
    const QUAD_OPCODE_BIT: u32 = 0x0800_0000;

    let material_command = (material & COMMAND_MASK) & !QUAD_OPCODE_BIT;
    let geometry_type = current & QUAD_OPCODE_BIT;
    (current & !COMMAND_MASK) | material_command | geometry_type
}

/// View-space vertex carried through near clipping (position + interpolated
/// per-corner colour and UV).
#[derive(Clone, Copy)]
pub struct CVert {
    pub v: [i32; 3],
    pub rgb: (i32, i32, i32),
    pub uv: (i32, i32),
}

/// Screen-space vertex (true, unclamped coords) carried through the guard-band
/// clip and into the emitter.
#[derive(Clone, Copy)]
pub struct SVert {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub rgb: (i32, i32, i32),
    pub uv: (i32, i32),
}

/// Exact screen midpoint with perspective UVs and affine Gouraud colours.
/// Inputs use positive u16 GTE depths and byte-range UV/RGB attributes.
/// Depth is the harmonic mean. `lo + lo*(hi-lo)/(lo+hi)` avoids overflowing
/// the doubled u16-depth product while retaining the exact integer quotient.
#[inline(never)]
pub fn perspective_screen_midpoint(a: SVert, b: SVert) -> SVert {
    let (za, zb) = (a.z.max(1) as u32, b.z.max(1) as u32);
    let d = za + zb;
    let lo = za.min(zb);
    let hi = za.max(zb);
    let uv = |a: i32, b: i32| ((a as u32 * zb + b as u32 * za + d / 2) / d) as i32;
    SVert {
        x: (a.x + b.x) >> 1,
        y: (a.y + b.y) >> 1,
        z: (lo + lo * (hi - lo) / d) as i32,
        uv: (uv(a.uv.0, b.uv.0), uv(a.uv.1, b.uv.1)),
        rgb: (
            (a.rgb.0 + b.rgb.0 + 1) >> 1,
            (a.rgb.1 + b.rgb.1 + 1) >> 1,
            (a.rgb.2 + b.rgb.2 + 1) >> 1,
        ),
    }
}

/// Return whether affine UV interpolation along one projected edge can differ
/// from perspective-correct interpolation by more than `max_error_texels` at
/// the screen-space midpoint.
///
/// For an edge with endpoint depths z0/z1, the exact midpoint error is:
///
/// `uv_span * abs(z1 - z0) / (2 * (z0 + z1))`
///
/// The comparison stays multiplication-only on the R3000. Tiny projected
/// edges are ignored because their error cannot occupy enough screen pixels to
/// justify four replacement packets.
#[inline(always)]
pub fn affine_edge_needs_split(
    screen0: (i32, i32),
    z0: i32,
    uv0: (i32, i32),
    screen1: (i32, i32),
    z1: i32,
    uv1: (i32, i32),
    max_error_texels: i32,
    min_screen_span: i32,
) -> bool {
    if z0 < NEAR_Z || z1 < NEAR_Z {
        return false;
    }
    let screen_span = (screen1.0 - screen0.0)
        .abs()
        .max((screen1.1 - screen0.1).abs());
    if screen_span < min_screen_span {
        return false;
    }
    let uv_span = (uv1.0 - uv0.0).abs().max((uv1.1 - uv0.1).abs());
    let depth_span = (z1 - z0).abs();
    uv_span * depth_span > 2 * max_error_texels * (z0 + z1)
}

/// Spend the bounded residue allowance on visible patches of at least
/// 128 projected pixels. Long, thin distant triangles otherwise consume it
/// before the close clipped floor. Inputs have passed the GPU guard bounds.
#[inline(always)]
pub fn residue_screen_coverage(p: [(i32, i32); 3]) -> bool {
    if p.iter().all(|v| v.0 < 0)
        || p.iter().all(|v| v.0 >= 320)
        || p.iter().all(|v| v.1 < 0)
        || p.iter().all(|v| v.1 >= 240)
    {
        return false;
    }
    let area2 = (p[1].0 - p[0].0) * (p[2].1 - p[0].1) - (p[2].0 - p[0].0) * (p[1].1 - p[0].1);
    area2.abs() >= 256
}

/// Expand a native quad's crack backstop by one pixel per axis to limit UV stretch.
/// Input and output use GPU strip order: triangles (0,1,2) and (1,3,2).
/// Edge normals must walk perimeter order (0,1,3,2); strip order crosses
/// the interior diagonals and can move corners inward instead.
#[inline(always)]
pub fn quad_underlay_corners(projected: [(i16, i16); 4]) -> [(i16, i16); 4] {
    let pts = [
        (projected[0].0 as i32, projected[0].1 as i32),
        (projected[1].0 as i32, projected[1].1 as i32),
        (projected[3].0 as i32, projected[3].1 as i32),
        (projected[2].0 as i32, projected[2].1 as i32),
    ];
    // Inputs use GPU strip order (0,1,2,3). Walk the boundary in
    // perimeter order (0,1,3,2), then restore strip order below.
    // Winding sign from the polygon area decides which side is outward.
    let mut area2 = 0i32;
    let mut i = 0usize;
    while i < 4 {
        let a = pts[i];
        let b = pts[(i + 1) & 3];
        area2 += a.0 * b.1 - b.0 * a.1;
        i += 1;
    }
    let sgn = if area2 >= 0 { 1i32 } else { -1i32 };
    let mut out = [(0i32, 0i32); 4];
    let mut i = 0usize;
    while i < 4 {
        let prev = pts[(i + 3) & 3];
        let cur = pts[i];
        let next = pts[(i + 1) & 3];
        // Outward normals of the two adjacent edges (sign-corrected),
        // each reduced to a +-1px step per axis.
        let n1 = (sgn * (cur.1 - prev.1), sgn * (prev.0 - cur.0));
        let n2 = (sgn * (next.1 - cur.1), sgn * (cur.0 - next.0));
        let nx = (n1.0 + n2.0).signum();
        let ny = (n1.1 + n2.1).signum();
        out[i] = (
            (cur.0 + nx).clamp(-1023, 1023),
            (cur.1 + ny).clamp(-1023, 1023),
        );
        i += 1;
    }
    [out[0], out[1], out[3], out[2]].map(|p| (p.0 as i16, p.1 as i16))
}

pub const EMPTY_CV: CVert = CVert {
    v: [0; 3],
    rgb: (0, 0, 0),
    uv: (0, 0),
};
pub const EMPTY_SV: SVert = SVert {
    x: 0,
    y: 0,
    z: 0,
    rgb: (0, 0, 0),
    uv: (0, 0),
};

#[inline(always)]
pub fn close_inv_q12<V: View>(view: &V, z: i32) -> i32 {
    let h = view.projection_h();
    if h == SOFT_H && (CLOSE_LUT_Z0..SOFT_H / 2).contains(&z) {
        CLOSE_INV_Q12[(z - CLOSE_LUT_Z0) as usize] as i32
    } else {
        (h << 12) / z.max(NEAR_Z)
    }
}

fn lerp_cv_near(a: &CVert, b: &CVert) -> CVert {
    // Adjacent triangles visit their shared edge in opposite directions. Pick
    // one geometric endpoint order before fixed-point interpolation so both
    // triangles round the clipped vertex identically instead of exposing a
    // one-pixel crack along the shared diagonal.
    let (a, b) = if (b.v[0], b.v[1], b.v[2]) < (a.v[0], a.v[1], a.v[2]) {
        (b, a)
    } else {
        (a, b)
    };
    let t = t_q12(NEAR_Z - a.v[2], b.v[2] - a.v[2]);
    CVert {
        v: [mix(a.v[0], b.v[0], t), mix(a.v[1], b.v[1], t), NEAR_Z],
        rgb: (
            mix(a.rgb.0, b.rgb.0, t),
            mix(a.rgb.1, b.rgb.1, t),
            mix(a.rgb.2, b.rgb.2, t),
        ),
        uv: (mix(a.uv.0, b.uv.0, t), mix(a.uv.1, b.uv.1, t)),
    }
}

/// Point on a 3D edge that projects to the midpoint of its two screen-space
/// endpoints. This is deliberately not the ordinary world midpoint: under
/// perspective, the near half of an edge can occupy nearly the entire screen.
/// Canonical endpoint order keeps shared-edge rounding identical.
pub fn projected_midpoint_cv(mut a: CVert, mut b: CVert) -> CVert {
    let ka = (a.v[0], a.v[1], a.v[2], a.uv.0, a.uv.1);
    let kb = (b.v[0], b.v[1], b.v[2], b.uv.0, b.uv.1);
    if kb < ka {
        core::mem::swap(&mut a, &mut b);
    }
    let za = a.v[2].max(NEAR_Z);
    let zb = b.v[2].max(NEAR_Z);
    // For a screen-space lambda of 1/2, the corresponding view-edge
    // parameter is za/(za+zb). t_q12 scales before shifting, so the close/far
    // ratio remains safe without runtime i64 division on MIPS-I.
    let t = t_q12(za, za.saturating_add(zb));
    CVert {
        v: [
            mix(a.v[0], b.v[0], t),
            mix(a.v[1], b.v[1], t),
            mix(a.v[2], b.v[2], t),
        ],
        rgb: (
            mix(a.rgb.0, b.rgb.0, t),
            mix(a.rgb.1, b.rgb.1, t),
            mix(a.rgb.2, b.rgb.2, t),
        ),
        uv: (mix(a.uv.0, b.uv.0, t), mix(a.uv.1, b.uv.1, t)),
    }
}

/// Clip a triangle against `z >= NEAR_Z` in view space. Writes up to 4 verts.
pub fn near_clip(cv: &[CVert; 3], out: &mut [CVert; 4]) -> usize {
    // Keep GoldSrc's canonical endpoint interpolation and fan order.
    unsafe {
        clip_convex_plane::<_, _, true>(cv, out, &NearPlane, ClipTraversal::PreviousToCurrent)
    }
}

struct NearPlane;

impl AttributedClipPlane<CVert> for NearPlane {
    type Distance = bool;
    #[inline(always)]
    fn distance(&self, _: usize, vertex: &CVert) -> bool {
        vertex.v[2] >= NEAR_Z
    }
    #[inline(always)]
    fn inside(&self, inside: bool) -> bool {
        inside
    }
    #[inline(always)]
    fn intersection(
        &self,
        _: usize,
        first: &CVert,
        _: bool,
        _: usize,
        second: &CVert,
        _: bool,
    ) -> CVert {
        lerp_cv_near(first, second)
    }
}

/// Project a clipped view-space vertex to true screen coords (one reciprocal
/// `H/z` in Q12 shared by X and Y).
pub fn project_soft<V: View>(view: &V, cv: &CVert) -> SVert {
    let z = cv.v[2].max(NEAR_Z);
    let h = view.projection_h();
    let (x, y) = if h == SOFT_H && z >= CLOSE_LUT_Z0 {
        let inv = if z < h / 2 {
            close_inv_q12(view, z)
        } else {
            (h << 12) / z
        }; // Q12 H/z; the normal-FOV products remain within i32.
        (
            ((cv.v[0] * inv) >> 12) + view.ofx(),
            ((cv.v[1] * inv) >> 12) + view.ofy(),
        )
    } else {
        // At 8..15 units (and at 20-degree zoom) H/z is too large for the
        // compact reciprocal/multiply path. Multiply by the small focal length
        // first, then divide; this is algebraically identical and stays i32.
        (cv.v[0] * h / z + view.ofx(), cv.v[1] * h / z + view.ofy())
    };
    SVert {
        x,
        y,
        z: cv.v[2],
        rgb: cv.rgb,
        uv: cv.uv,
    }
}

/// Is this screen vertex inside the guard band (safe to draw without clipping)?
#[inline]
pub fn in_band(p: &SVert) -> bool {
    in_band_xy(p.x, p.y)
}

/// Coordinate-only guard-band test for hardware-projected vertices.
#[inline(always)]
pub fn in_band_xy(x: i32, y: i32) -> bool {
    (GX0..=GX1).contains(&x) && (GY0..=GY1).contains(&y)
}

/// All four corners outside one vertical display half-space, by the exact
/// `view_outcode` planes. Every triangle of such a convex cell (and of any
/// midpoint-subdivided child) clips to nothing in `visible_clip`, so callers
/// may prune the whole subtree pixel-identically. The vertical FOV is
/// narrower than the 45-degree lateral planes (120/h vs 160/h), which is why
/// this test exists separately: a grazing floor keeps a wide invisible band
/// between the two angles that a 45-degree prune never catches.
#[inline]
pub fn quad_outside_vertical<V: View>(view: &V, c: &[&CVert; 4]) -> bool {
    let h = view.projection_h();
    c.iter()
        .all(|v| h * v.v[1] + (view.ofy() - view.vy0()) * v.v[2] < 0)
        || c.iter()
            .all(|v| (view.vy1() - view.ofy()) * v.v[2] - h * v.v[1] < 0)
}

/// A vertex produced by `visible_clip` lies on (or one integer rounding step
/// inside) the display-frustum boundary. Adaptive T-junction underlays are
/// only needed for these clipped edge fans; fully interior triangles retain
/// the zero-extra-packet path.
#[inline(always)]
pub fn on_visible_boundary<V: View>(view: &V, p: &SVert) -> bool {
    p.x <= view.vx0() + 1 || p.x >= view.vx1() - 1 || p.y <= view.vy0() + 1 || p.y >= view.vy1() - 1
}

/// A fully-front projected triangle wholly beyond one guard-band edge cannot
/// contribute a pixel. Strict comparisons match `guard_clip`: a vertex on the
/// edge is retained, while GTE saturation preserves which side it is on.
#[inline(always)]
pub fn tri_outside_band(p: [(i16, i16); 3]) -> bool {
    triangle_outside_common_plane(
        [
            [p[0].0 as i32, p[0].1 as i32],
            [p[1].0 as i32, p[1].1 as i32],
            [p[2].0 as i32, p[2].1 as i32],
        ],
        GUARD_BOUNDS,
    )
}

/// Screen-space back-face test (cross product; >= 0 = back-facing).
#[inline]
pub fn back_facing(a: (i32, i32), b: (i32, i32), c: (i32, i32)) -> bool {
    (b.0 - a.0) * (c.1 - a.1) - (c.0 - a.0) * (b.1 - a.1) >= 0
}

#[derive(Clone, Copy, PartialEq)]
enum Axis {
    X,
    Y,
}

fn lerp_sv(a: &SVert, b: &SVert, axis: Axis, bound: i32) -> SVert {
    let (a, b) = if (b.x, b.y, b.z) < (a.x, a.y, a.z) {
        (b, a)
    } else {
        (a, b)
    };
    let (ca, cb) = if axis == Axis::X {
        (a.x, b.x)
    } else {
        (a.y, b.y)
    };
    let t = t_q12(bound - ca, cb - ca);
    SVert {
        x: mix(a.x, b.x, t),
        y: mix(a.y, b.y, t),
        z: mix(a.z, b.z, t),
        rgb: (
            mix(a.rgb.0, b.rgb.0, t),
            mix(a.rgb.1, b.rgb.1, t),
            mix(a.rgb.2, b.rgb.2, t),
        ),
        uv: (mix(a.uv.0, b.uv.0, t), mix(a.uv.1, b.uv.1, t)),
    }
}

fn clip_edge(
    inp: &[SVert],
    n: usize,
    out: &mut [SVert; 8],
    axis: Axis,
    bound: i32,
    keep_ge: bool,
) -> usize {
    let plane = ScreenPlane {
        axis,
        bound,
        keep_ge,
    };
    unsafe {
        clip_convex_plane::<_, _, true>(&inp[..n], out, &plane, ClipTraversal::PreviousToCurrent)
    }
}

struct ScreenPlane {
    axis: Axis,
    bound: i32,
    keep_ge: bool,
}

impl AttributedClipPlane<SVert> for ScreenPlane {
    type Distance = bool;
    #[inline(always)]
    fn distance(&self, _: usize, vertex: &SVert) -> bool {
        let coordinate = if self.axis == Axis::X {
            vertex.x
        } else {
            vertex.y
        };
        if self.keep_ge {
            coordinate >= self.bound
        } else {
            coordinate <= self.bound
        }
    }
    #[inline(always)]
    fn inside(&self, inside: bool) -> bool {
        inside
    }
    #[inline(always)]
    fn intersection(
        &self,
        _: usize,
        first: &SVert,
        _: bool,
        _: usize,
        second: &SVert,
        _: bool,
    ) -> SVert {
        lerp_sv(first, second, self.axis, self.bound)
    }
}

// Both clipping paths borrow the same caller-owned ClipScratch. The ports keep
// it in static RAM, avoiding a 512-byte stack clear on every clipping call.

#[derive(Clone, Copy)]
enum ViewPlane {
    Near,
    Left,
    Right,
    Top,
    Bottom,
}

#[inline(always)]
fn view_plane_distance<V: View>(view: &V, v: &SVert, plane: ViewPlane) -> i32 {
    let h = view.projection_h();
    match plane {
        ViewPlane::Near => v.z - NEAR_Z,
        ViewPlane::Left => h * v.x + (view.ofx() - view.vx0()) * v.z,
        ViewPlane::Right => (view.vx1() - view.ofx()) * v.z - h * v.x,
        ViewPlane::Top => h * v.y + (view.ofy() - view.vy0()) * v.z,
        ViewPlane::Bottom => (view.vy1() - view.ofy()) * v.z - h * v.y,
    }
}

fn lerp_view_plane(a: &SVert, b: &SVert, mut da: i32, mut db: i32, plane: ViewPlane) -> SVert {
    // `visible_clip` runs independently for each source triangle. Canonical
    // ordering makes a shared geometric edge survive every frustum plane with
    // exactly the same rounded position on both sides.
    let (a, b) = if (b.x, b.y, b.z) < (a.x, a.y, a.z) {
        core::mem::swap(&mut da, &mut db);
        (b, a)
    } else {
        (a, b)
    };
    let t = t_q12(da, da - db);
    let mut v = SVert {
        x: mix(a.x, b.x, t),
        y: mix(a.y, b.y, t),
        z: mix(a.z, b.z, t),
        rgb: (
            mix(a.rgb.0, b.rgb.0, t),
            mix(a.rgb.1, b.rgb.1, t),
            mix(a.rgb.2, b.rgb.2, t),
        ),
        uv: (mix(a.uv.0, b.uv.0, t), mix(a.uv.1, b.uv.1, t)),
    };
    if matches!(plane, ViewPlane::Near) {
        v.z = NEAR_Z;
    }
    v
}

struct GoldSrcViewClipPlane<'a, V: View> {
    plane: ViewPlane,
    view: &'a V,
}

impl<V: View> AttributedClipPlane<SVert> for GoldSrcViewClipPlane<'_, V> {
    type Distance = i32;

    #[inline(always)]
    fn distance(&self, _: usize, vertex: &SVert) -> Self::Distance {
        view_plane_distance(self.view, vertex, self.plane)
    }

    #[inline(always)]
    fn inside(&self, distance: Self::Distance) -> bool {
        distance >= 0
    }

    #[inline(always)]
    fn intersection(
        &self,
        _: usize,
        first: &SVert,
        first_distance: Self::Distance,
        _: usize,
        second: &SVert,
        second_distance: Self::Distance,
    ) -> SVert {
        lerp_view_plane(first, second, first_distance, second_distance, self.plane)
    }
}

fn clip_view_plane<V: View>(
    view: &V,
    inp: &[SVert],
    n: usize,
    out: &mut [SVert; 8],
    plane: ViewPlane,
) -> usize {
    unsafe {
        clip_convex_plane::<_, _, true>(
            &inp[..n],
            out,
            &GoldSrcViewClipPlane { plane, view },
            ClipTraversal::PreviousToCurrent,
        )
    }
}

#[inline(always)]
fn view_outcode<V: View>(view: &V, v: &SVert, h: i32) -> u8 {
    half_space_outcode5([
        v.z - NEAR_Z,
        h * v.x + (view.ofx() - view.vx0()) * v.z,
        (view.vx1() - view.ofx()) * v.z - h * v.x,
        h * v.y + (view.ofy() - view.vy0()) * v.z,
        (view.vy1() - view.ofy()) * v.z - h * v.y,
    ])
}

/// Clip one textured view-space triangle to the near plane and the actual
/// display frustum. Returned `SVert` values intentionally still contain view
/// coordinates in x/y/z; callers convert them back to `CVert` before project.
/// Reusing the guard-clip ping-pong storage adds no static PS1 RAM.
///
/// The pointer aliases the supplied clip scratch and remains valid only until the
/// next clipping call using that scratch, or until the scratch is dropped. Returning it directly avoids copying every surviving
/// polygon into a second scratch array before the caller immediately consumes
/// it.
pub fn visible_clip<V: View>(
    view: &V,
    scratch: &mut ClipScratch,
    poly: [&CVert; 3],
) -> (*const SVert, usize) {
    let (a, b) = (&mut scratch.a, &mut scratch.b);
    let h = view.projection_h();
    let mut any_outside = 0u8;
    let mut all_outside = 0x1fu8;
    for (dst, src) in a.iter_mut().zip(poly) {
        let vertex = SVert {
            x: src.v[0],
            y: src.v[1],
            z: src.v[2],
            rgb: src.rgb,
            uv: src.uv,
        };
        *dst = vertex;
        let code = view_outcode(view, &vertex, h);
        any_outside |= code;
        all_outside &= code;
    }
    // A triangle wholly outside one half-space cannot become visible; a
    // triangle wholly inside all five needs no interpolation or ping-pong copy.
    if all_outside != 0 {
        return (core::ptr::null(), 0);
    }
    if any_outside == 0 {
        return (a.as_ptr(), 3);
    }
    let mut n = 3usize;
    let mut cur_is_a = true;
    for (bit, plane) in [
        (1 << 0, ViewPlane::Near),
        (1 << 1, ViewPlane::Left),
        (1 << 2, ViewPlane::Right),
        (1 << 3, ViewPlane::Top),
        (1 << 4, ViewPlane::Bottom),
    ] {
        // Clipping a convex polygon cannot cross a plane that contained all
        // original vertices, so only visit the half-spaces present in the
        // triangle's combined outcode. Edge-of-screen cases normally pay for
        // one pass instead of all five.
        if any_outside & bit == 0 {
            continue;
        }
        n = if cur_is_a {
            clip_view_plane(view, a, n, b, plane)
        } else {
            clip_view_plane(view, b, n, a, plane)
        };
        cur_is_a = !cur_is_a;
        if n < 3 {
            return (core::ptr::null(), 0);
        }
    }
    let cur: &[SVert; 8] = if cur_is_a { a } else { b };
    (cur.as_ptr(), n)
}

pub fn guard_clip(
    scratch: &mut ClipScratch,
    poly: &[SVert],
    n: usize,
    out: &mut [SVert; 8],
) -> usize {
    let n = n.min(8);
    // Bounds once; run only the passes an edge actually crosses (most clipped
    // triangles cross ONE band edge -- the old code always ran all four, each
    // a full copy pass).
    let (mut minx, mut maxx, mut miny, mut maxy) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
    for v in poly.iter().take(n) {
        minx = minx.min(v.x);
        maxx = maxx.max(v.x);
        miny = miny.min(v.y);
        maxy = maxy.max(v.y);
    }
    if minx >= GX0 && maxx <= GX1 && miny >= GY0 && maxy <= GY1 {
        out[..n].copy_from_slice(&poly[..n]);
        return n;
    }
    let (a, b) = (&mut scratch.a, &mut scratch.b);
    a[..n].copy_from_slice(&poly[..n]);
    let mut cur_is_a = true;
    let mut na = n;
    let mut pass = |na: usize, cur_is_a: &mut bool, axis: Axis, bound: i32, keep_ge: bool| {
        let m = if *cur_is_a {
            clip_edge(a, na, b, axis, bound, keep_ge)
        } else {
            clip_edge(b, na, a, axis, bound, keep_ge)
        };
        *cur_is_a = !*cur_is_a;
        m
    };
    if minx < GX0 {
        na = pass(na, &mut cur_is_a, Axis::X, GX0, true);
        if na < 3 {
            return 0;
        }
    }
    if maxx > GX1 {
        na = pass(na, &mut cur_is_a, Axis::X, GX1, false);
        if na < 3 {
            return 0;
        }
    }
    if miny < GY0 {
        na = pass(na, &mut cur_is_a, Axis::Y, GY0, true);
        if na < 3 {
            return 0;
        }
    }
    if maxy > GY1 {
        na = pass(na, &mut cur_is_a, Axis::Y, GY1, false);
        if na < 3 {
            return 0;
        }
    }
    let cur: &[SVert; 8] = if cur_is_a { a } else { b };
    out[..na].copy_from_slice(&cur[..na]);
    na
}

/// Per-view projection policy, statically dispatched and immutable during a draw.
pub trait View {
    fn projection_h(&self) -> i32;
    fn ofx(&self) -> i32;
    fn ofy(&self) -> i32;
    fn vx0(&self) -> i32;
    fn vx1(&self) -> i32;
    fn vy0(&self) -> i32;
    fn vy1(&self) -> i32;
}
/// Full-screen HL policy: only the existing focal length occupies RAM.
pub struct FullView {
    h: i32,
}
impl FullView {
    pub const fn new() -> Self {
        Self { h: SOFT_H }
    }
    #[inline(always)]
    pub fn set_projection_h(&mut self, h: i32) {
        self.h = h;
    }
}
impl Default for FullView {
    fn default() -> Self {
        Self::new()
    }
}
impl View for FullView {
    #[inline(always)]
    fn projection_h(&self) -> i32 {
        self.h
    }
    #[inline(always)]
    fn ofx(&self) -> i32 {
        160
    }
    #[inline(always)]
    fn ofy(&self) -> i32 {
        120
    }
    #[inline(always)]
    fn vx0(&self) -> i32 {
        -1
    }
    #[inline(always)]
    fn vx1(&self) -> i32 {
        320
    }
    #[inline(always)]
    fn vy0(&self) -> i32 {
        -1
    }
    #[inline(always)]
    fn vy1(&self) -> i32 {
        240
    }
}
/// CS split-screen policy: the existing focal length plus six view values.
pub struct RectView {
    h: i32,
    ofx: i32,
    ofy: i32,
    vx0: i32,
    vx1: i32,
    vy0: i32,
    vy1: i32,
}
impl RectView {
    pub const fn new() -> Self {
        Self {
            h: SOFT_H,
            ofx: 160,
            ofy: 120,
            vx0: -1,
            vx1: 320,
            vy0: -1,
            vy1: 240,
        }
    }
    #[inline(always)]
    pub fn set_projection_h(&mut self, h: i32) {
        self.h = h;
    }
    /// Select the one-pixel apron of the view being rendered.
    #[inline(always)]
    pub fn set_view_rect(&mut self, x0: i32, y0: i32, w: i32, h: i32) {
        self.ofx = x0 + w / 2;
        self.ofy = y0 + h / 2;
        self.vx0 = x0 - 1;
        self.vx1 = x0 + w;
        self.vy0 = y0 - 1;
        self.vy1 = y0 + h;
    }
}
impl Default for RectView {
    fn default() -> Self {
        Self::new()
    }
}
impl View for RectView {
    #[inline(always)]
    fn projection_h(&self) -> i32 {
        self.h
    }
    #[inline(always)]
    fn ofx(&self) -> i32 {
        self.ofx
    }
    #[inline(always)]
    fn ofy(&self) -> i32 {
        self.ofy
    }
    #[inline(always)]
    fn vx0(&self) -> i32 {
        self.vx0
    }
    #[inline(always)]
    fn vx1(&self) -> i32 {
        self.vx1
    }
    #[inline(always)]
    fn vy0(&self) -> i32 {
        self.vy0
    }
    #[inline(always)]
    fn vy1(&self) -> i32 {
        self.vy1
    }
}
/// Caller-owned ping-pong storage. Reused by visible and guard clipping;
/// 512 bytes, matching the two former arrays, with no per-call clearing.
pub struct ClipScratch {
    a: [SVert; 8],
    b: [SVert; 8],
}
impl ClipScratch {
    pub const fn new() -> Self {
        Self {
            a: [EMPTY_SV; 8],
            b: [EMPTY_SV; 8],
        }
    }
}
impl Default for ClipScratch {
    fn default() -> Self {
        Self::new()
    }
}
