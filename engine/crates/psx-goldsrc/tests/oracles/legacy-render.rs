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
/// Screen centre (matches `set_screen_offset(160<<16, 120<<16)`).
pub const OFX: i32 = 160;
pub const OFY: i32 = 120;
/// Default software projection focal length (matches the normal 90-degree
/// `set_projection_plane(H_PROJ)`). Crossbow zoom swaps the active focal
/// length, while this constant retains the fast reciprocal table's shape.
pub const SOFT_H: i32 = 160;
static mut ACTIVE_H: i32 = SOFT_H;
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
    let delta = if first >= second {
        first - second
    } else {
        second - first
    };
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
const VX0: i32 = -1;
const VX1: i32 = 320;
const VY0: i32 = -1;
const VY1: i32 = 240;

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
pub fn close_inv_q12(z: i32) -> i32 {
    let h = projection_h();
    if h == SOFT_H && (CLOSE_LUT_Z0..SOFT_H / 2).contains(&z) {
        CLOSE_INV_Q12[(z - CLOSE_LUT_Z0) as usize] as i32
    } else {
        (h << 12) / z.max(NEAR_Z)
    }
}

/// Keep software near clipping and the GTE on the same focal length. Gameplay
/// is single-threaded; the setter runs once immediately before each render.
#[inline(always)]
pub unsafe fn set_projection_h(h: i32) {
    ACTIVE_H = h;
}

#[inline(always)]
pub fn projection_h() -> i32 {
    unsafe { ACTIVE_H }
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
pub fn project_soft(cv: &CVert) -> SVert {
    let z = cv.v[2].max(NEAR_Z);
    let h = projection_h();
    let (x, y) = if h == SOFT_H && z >= CLOSE_LUT_Z0 {
        let inv = if z < h / 2 {
            close_inv_q12(z)
        } else {
            (h << 12) / z
        }; // Q12 H/z; the normal-FOV products remain within i32.
        (((cv.v[0] * inv) >> 12) + OFX, ((cv.v[1] * inv) >> 12) + OFY)
    } else {
        // At 8..15 units (and at 20-degree zoom) H/z is too large for the
        // compact reciprocal/multiply path. Multiply by the small focal length
        // first, then divide; this is algebraically identical and stays i32.
        (cv.v[0] * h / z + OFX, cv.v[1] * h / z + OFY)
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
    x >= GX0 && x <= GX1 && y >= GY0 && y <= GY1
}

/// All four corners outside one vertical display half-space, by the exact
/// `view_outcode` planes. Every triangle of such a convex cell (and of any
/// midpoint-subdivided child) clips to nothing in `visible_clip`, so callers
/// may prune the whole subtree pixel-identically. The vertical FOV is
/// narrower than the 45-degree lateral planes (120/h vs 160/h), which is why
/// this test exists separately: a grazing floor keeps a wide invisible band
/// between the two angles that a 45-degree prune never catches.
#[inline]
pub fn quad_outside_vertical(c: &[&CVert; 4]) -> bool {
    let h = projection_h();
    c.iter().all(|v| h * v.v[1] + (OFY - VY0) * v.v[2] < 0)
        || c.iter().all(|v| (VY1 - OFY) * v.v[2] - h * v.v[1] < 0)
}

/// A vertex produced by `visible_clip` lies on (or one integer rounding step
/// inside) the display-frustum boundary. Adaptive T-junction underlays are
/// only needed for these clipped edge fans; fully interior triangles retain
/// the zero-extra-packet path.
#[inline(always)]
pub fn on_visible_boundary(p: &SVert) -> bool {
    p.x <= VX0 + 1 || p.x >= VX1 - 1 || p.y <= VY0 + 1 || p.y >= VY1 - 1
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

/// Clip a convex screen polygon to the guard band. Returns vertices in `out`.
// Ping-pong scratch for guard_clip. Statics, not locals: the stack arrays
// memset 512 B per call. Single-threaded render loop, never live across calls.
static mut GC_A: [SVert; 8] = [EMPTY_SV; 8];
static mut GC_B: [SVert; 8] = [EMPTY_SV; 8];

#[derive(Clone, Copy)]
enum ViewPlane {
    Near,
    Left,
    Right,
    Top,
    Bottom,
}

#[inline(always)]
fn view_plane_distance(v: &SVert, plane: ViewPlane) -> i32 {
    let h = projection_h();
    match plane {
        ViewPlane::Near => v.z - NEAR_Z,
        ViewPlane::Left => h * v.x + (OFX - VX0) * v.z,
        ViewPlane::Right => (VX1 - OFX) * v.z - h * v.x,
        ViewPlane::Top => h * v.y + (OFY - VY0) * v.z,
        ViewPlane::Bottom => (VY1 - OFY) * v.z - h * v.y,
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

struct GoldSrcViewClipPlane(ViewPlane);

impl AttributedClipPlane<SVert> for GoldSrcViewClipPlane {
    type Distance = i32;

    #[inline(always)]
    fn distance(&self, _: usize, vertex: &SVert) -> Self::Distance {
        view_plane_distance(vertex, self.0)
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
        lerp_view_plane(first, second, first_distance, second_distance, self.0)
    }
}

fn clip_view_plane(inp: &[SVert], n: usize, out: &mut [SVert; 8], plane: ViewPlane) -> usize {
    let m = unsafe {
        clip_convex_plane::<_, _, true>(
            &inp[..n],
            out,
            &GoldSrcViewClipPlane(plane),
            ClipTraversal::PreviousToCurrent,
        )
    };
    m
}

#[inline(always)]
fn view_outcode(v: &SVert, h: i32) -> u8 {
    half_space_outcode5([
        v.z - NEAR_Z,
        h * v.x + (OFX - VX0) * v.z,
        (VX1 - OFX) * v.z - h * v.x,
        h * v.y + (OFY - VY0) * v.z,
        (VY1 - OFY) * v.z - h * v.y,
    ])
}

/// Clip one textured view-space triangle to the near plane and the actual
/// display frustum. Returned `SVert` values intentionally still contain view
/// coordinates in x/y/z; callers convert them back to `CVert` before project.
/// Reusing the guard-clip ping-pong storage adds no static PS1 RAM.
///
/// The pointer aliases the global clip scratch and remains valid only until the
/// next clipping call. Returning it directly avoids copying every surviving
/// polygon into a second scratch array before the caller immediately consumes
/// it.
pub unsafe fn visible_clip(poly: [&CVert; 3]) -> (*const SVert, usize) {
    let (a, b) = unsafe {
        (
            &mut *core::ptr::addr_of_mut!(GC_A),
            &mut *core::ptr::addr_of_mut!(GC_B),
        )
    };
    let h = projection_h();
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
        let code = view_outcode(&vertex, h);
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
            clip_view_plane(a, n, b, plane)
        } else {
            clip_view_plane(b, n, a, plane)
        };
        cur_is_a = !cur_is_a;
        if n < 3 {
            return (core::ptr::null(), 0);
        }
    }
    let cur: &[SVert; 8] = if cur_is_a { a } else { b };
    (cur.as_ptr(), n)
}

pub fn guard_clip(poly: &[SVert], n: usize, out: &mut [SVert; 8]) -> usize {
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
    let (a, b) = unsafe {
        (
            &mut *core::ptr::addr_of_mut!(GC_A),
            &mut *core::ptr::addr_of_mut!(GC_B),
        )
    };
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

#[cfg(test)]
mod tests {
    #[test]
    fn shared_quad_extent_matches_both_triangle_edges() {
        let reference = |p: [[i32; 2]; 4]| {
            [(0, 1), (1, 2), (2, 0), (0, 3), (3, 2)]
                .iter()
                .all(|&(a, b)| {
                    (p[a][0] - p[b][0]).abs() <= 1023 && (p[a][1] - p[b][1]).abs() <= 511
                })
        };
        let check = |p: [[i32; 2]; 4]| {
            let xy = p.map(|p| p[0] as u16 as u32 | ((p[1] as u16 as u32) << 16));
            assert_eq!(
                super::quad_fits_gpu_xy(xy[0], xy[1], xy[2], xy[3]),
                reference(p),
                "{p:?}"
            );
        };
        for span in [0, 510, 511, 512, 1022, 1023, 1024, 2046, 65535] {
            for delta in [-1, 0, 1] {
                check([
                    [-32768, -32768],
                    [-32768 + span, -32768],
                    [-32768 + span, -32768 + (span + delta).clamp(0, 65535)],
                    [-32768, -32768 + span],
                ]);
            }
        }
        let mut seed = 0x1845_937bu32;
        for narrow in [false, true] {
            for _ in 0..100_000 {
                let mut p = [[0; 2]; 4];
                for xy in &mut p {
                    for value in xy {
                        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                        *value = (seed >> 16) as i16 as i32;
                        if narrow {
                            *value >>= 5;
                        }
                    }
                }
                check(p);
            }
        }
    }

    use super::*;

    #[test]
    fn screen_midpoint_matches_perspective_and_full_u16_depth_range() {
        for za in [8, 31, 128, 1024, 32768, 65535] {
            for zb in [8, 31, 128, 1024, 32768, 65535] {
                let a = SVert {
                    x: -101,
                    y: 29,
                    z: za,
                    uv: (0, 255),
                    rgb: (4, 31, 255),
                };
                let b = SVert {
                    x: 200,
                    y: -12,
                    z: zb,
                    uv: (255, 0),
                    rgb: (255, 70, 0),
                };
                let mid = perspective_screen_midpoint(a, b);
                let reverse = perspective_screen_midpoint(b, a);
                let expected_u =
                    ((255.0 / zb as f64) / (1.0 / za as f64 + 1.0 / zb as f64)).round() as i32;
                assert_eq!(mid.uv.0, expected_u);
                assert_eq!(
                    mid.z,
                    (2u64 * za as u64 * zb as u64 / (za + zb) as u64) as i32
                );
                assert_eq!(
                    (mid.x, mid.y, mid.z, mid.uv, mid.rgb),
                    (reverse.x, reverse.y, reverse.z, reverse.uv, reverse.rgb)
                );
                assert_eq!((mid.x, mid.y), (49, 8));
                assert_eq!(mid.rgb, (130, 51, 128));
            }
        }
    }

    #[test]
    fn very_close_projection_does_not_wrap_the_u16_lut() {
        unsafe { set_projection_h(SOFT_H) };
        assert_eq!(close_inv_q12(8), 81_920);
        assert_eq!(close_inv_q12(9), 72_817);
        assert_eq!(close_inv_q12(10), 65_536);
        assert_eq!(close_inv_q12(16), 40_960);

        let p = project_soft(&CVert {
            v: [100, 0, 10],
            rgb: (0, 0, 0),
            uv: (0, 0),
        });
        assert_eq!((p.x, p.y), (1_760, OFY));
    }

    #[test]
    fn guard_ratio_and_mix_survive_near_eight_extents() {
        // A full signed-i16 view-space span projected at z=8 is wider than
        // 2^20 pixels. The old num<<12 and delta*t forms both overflowed here.
        let left = -655_180;
        let right = 655_500;
        let t = t_q12(-GX0 - left, right - left);
        assert!((2046..=2050).contains(&t));
        let x = mix(left, right, t);
        assert!((GX0 - 512..=GX0 + 512).contains(&x));
    }

    #[test]
    fn visible_clip_interpolates_at_the_3d_frustum_intersection() {
        unsafe { set_projection_h(SOFT_H) };
        let tri = [
            CVert {
                v: [-32, 64, 16],
                rgb: (0, 0, 0),
                uv: (0, 0),
            },
            CVert {
                v: [32, 64, 16],
                rgb: (0, 0, 0),
                uv: (0, 0),
            },
            CVert {
                v: [0, -32, 128],
                rgb: (0, 0, 0),
                uv: (100, 100),
            },
        ];
        let (clipped, n) = unsafe { visible_clip([&tri[0], &tri[1], &tri[2]]) };
        let clipped = unsafe { core::slice::from_raw_parts(clipped, n) };
        assert!(n >= 3);
        let bottom: Vec<_> = clipped[..n]
            .iter()
            .filter_map(|v| {
                let p = project_soft(&CVert {
                    v: [v.x, v.y, v.z],
                    rgb: v.rgb,
                    uv: v.uv,
                });
                (p.y >= 239).then_some((p.y, v.uv.1))
            })
            .collect();
        assert!(!bottom.is_empty());
        // The geometric intersection is about 29% along the 3D edge. A
        // post-projection screen lerp would incorrectly produce roughly 76, so
        // the window only has to be tight enough to tell those apart: the Q12
        // clip fraction rounds the coordinate by a couple of units, and this
        // assert had never actually run to show it.
        assert!(
            bottom
                .iter()
                .all(|&(y, v)| (239..=243).contains(&y) && (26..=32).contains(&v)),
            "{bottom:?}"
        );
    }

    #[test]
    fn projected_midpoint_is_screen_balanced_and_symmetric() {
        unsafe { set_projection_h(SOFT_H) };
        let a = CVert {
            v: [0, 0, 16],
            rgb: (0, 0, 0),
            uv: (0, 0),
        };
        let b = CVert {
            v: [100, 0, 160],
            rgb: (100, 100, 100),
            uv: (100, 100),
        };
        let ab = projected_midpoint_cv(a, b);
        let ba = projected_midpoint_cv(b, a);
        assert_eq!(ab.v, ba.v);
        assert_eq!(ab.uv, ba.uv);
        assert_eq!(ab.rgb, ba.rgb);
        let pa = project_soft(&a);
        let pb = project_soft(&b);
        let pm = project_soft(&ab);
        let target = (pa.x + pb.x) / 2;
        assert!((pm.x - target).abs() <= 1, "{} vs {}", pm.x, target);
        assert!((8..=10).contains(&ab.uv.0));
    }

    #[test]
    fn clipped_shared_edges_are_direction_invariant() {
        let a = CVert {
            v: [-37, 91, 3],
            rgb: (17, 29, 43),
            uv: (11, 197),
        };
        let b = CVert {
            v: [111, -73, 29],
            rgb: (211, 101, 53),
            uv: (239, 7),
        };
        let ab = lerp_cv_near(&a, &b);
        let ba = lerp_cv_near(&b, &a);
        assert_eq!(ab.v, ba.v);
        assert_eq!(ab.uv, ba.uv);
        assert_eq!(ab.rgb, ba.rgb);

        let sa = SVert {
            x: -500,
            y: 311,
            z: 64,
            rgb: a.rgb,
            uv: a.uv,
        };
        let sb = SVert {
            x: 100,
            y: -47,
            z: 173,
            rgb: b.rgb,
            uv: b.uv,
        };
        let sab = lerp_sv(&sa, &sb, Axis::X, GX0);
        let sba = lerp_sv(&sb, &sa, Axis::X, GX0);
        assert_eq!((sab.x, sab.y, sab.z), (sba.x, sba.y, sba.z));
        assert_eq!(sab.uv, sba.uv);
        assert_eq!(sab.rgb, sba.rgb);

        let va = SVert {
            x: -32,
            y: 32,
            z: 64,
            rgb: a.rgb,
            uv: a.uv,
        };
        let vb = SVert {
            x: 48,
            y: 100,
            z: 64,
            rgb: b.rgb,
            uv: b.uv,
        };
        let da = view_plane_distance(&va, ViewPlane::Bottom);
        let db = view_plane_distance(&vb, ViewPlane::Bottom);
        let vab = lerp_view_plane(&va, &vb, da, db, ViewPlane::Bottom);
        let vba = lerp_view_plane(&vb, &va, db, da, ViewPlane::Bottom);
        assert_eq!((vab.x, vab.y, vab.z), (vba.x, vba.y, vba.z));
        assert_eq!(vab.uv, vba.uv);
        assert_eq!(vab.rgb, vba.rgb);
    }

    #[test]
    fn animated_material_patch_preserves_triangle_packet_type() {
        let current = 0x3411_2233;
        let translucent_material = 0x3600_0000;
        assert_eq!(
            patch_textured_gouraud_command(current, translucent_material),
            0x3611_2233
        );
    }

    #[test]
    fn animated_material_patch_preserves_quad_packet_type() {
        let current = 0x3ca1_b2c3;
        let triangle_material = 0x3400_0000;
        let translucent_triangle_material = 0x3600_0000;
        assert_eq!(
            patch_textured_gouraud_command(current, triangle_material),
            0x3ca1_b2c3
        );
        assert_eq!(
            patch_textured_gouraud_command(current, translucent_triangle_material),
            0x3ea1_b2c3
        );
    }

    #[test]
    fn refined_quad_pair_rejects_depth_spanning_children() {
        assert!(refined_pair_depth_compatible(80, 84));
        assert!(!refined_pair_depth_compatible(80, 85));
        assert!(!refined_pair_depth_compatible(85, 80));
    }

    #[test]
    fn packet_cache_depth_round_trips_beyond_u8() {
        let otz = 300usize;
        assert_eq!(cache_otz_u16(otz) as usize, otz);
        assert_eq!(otz as u8 as usize, 44);
    }

    #[test]
    fn affine_error_metric_rejects_flat_depth_and_tiny_edges() {
        assert!(!affine_edge_needs_split(
            (0, 0),
            100,
            (0, 0),
            (160, 0),
            100,
            (128, 0),
            4,
            48,
        ));
        assert!(!affine_edge_needs_split(
            (0, 0),
            100,
            (0, 0),
            (32, 0),
            400,
            (128, 0),
            4,
            48,
        ));
    }

    #[test]
    fn affine_error_metric_finds_visible_depth_skew() {
        // 128 * 300 / (2 * 500) = 38.4 texels of midpoint error.
        assert!(affine_edge_needs_split(
            (0, 0),
            100,
            (0, 0),
            (160, 0),
            400,
            (128, 0),
            4,
            48,
        ));
    }
    #[test]
    fn underlay_keeps_gpu_strip_order_and_covers_rectangles_in_both_windings() {
        for quad in [
            [(10, 20), (50, 20), (10, 60), (50, 60)],
            [(50, 20), (10, 20), (50, 60), (10, 60)],
            [(10, 60), (50, 60), (10, 20), (50, 20)],
        ] {
            let expanded = quad_underlay_corners(quad);
            for i in 0..4 {
                assert_eq!(
                    expanded[i].0,
                    quad[i].0 + if quad[i].0 == 10 { -1 } else { 1 }
                );
                assert_eq!(
                    expanded[i].1,
                    quad[i].1 + if quad[i].1 == 20 { -1 } else { 1 }
                );
            }
            let area = |ids: [usize; 3]| {
                let [a, b, c] = ids.map(|i| expanded[i]);
                (b.0 as i32 - a.0 as i32) * (c.1 as i32 - a.1 as i32)
                    - (b.1 as i32 - a.1 as i32) * (c.0 as i32 - a.0 as i32)
            };
            // A GT4 is two triangles with the same winding and a shared
            // diagonal. Swapping the final two corners folds them together.
            assert_eq!(area([0, 1, 2]).signum(), area([1, 3, 2]).signum());
            assert_eq!(area([0, 1, 2]).abs() + area([1, 3, 2]).abs(), 2 * 42 * 42);
        }
    }

    #[test]
    fn underlay_expands_a_sloped_quad_and_clamps_gpu_coordinates() {
        let q = [(20, 20), (60, 30), (10, 60), (50, 70)];
        assert_eq!(
            quad_underlay_corners(q),
            [(19, 19), (61, 29), (9, 61), (51, 71)]
        );
        assert_eq!(
            quad_underlay_corners([(-1022, -1022), (1022, -1022), (-1022, 1022), (1022, 1022)]),
            [(-1023, -1023), (1023, -1023), (-1023, 1023), (1023, 1023)]
        );
    }

    #[test]
    fn residue_budget_ignores_distant_slivers_and_offscreen_faces() {
        assert!(!residue_screen_coverage([
            (169, 132),
            (134, 132),
            (125, 136)
        ]));
        assert!(!residue_screen_coverage([
            (141, 138),
            (131, 138),
            (123, 144)
        ]));
        assert!(residue_screen_coverage([
            (165, 240),
            (321, 237),
            (233, 173)
        ]));
        assert!(residue_screen_coverage([(79, 173), (233, 173), (203, 151)]));
        assert!(!residue_screen_coverage([(321, 0), (400, 120), (321, 240)]));
        assert!(!residue_screen_coverage([(0, -1), (160, -120), (320, -1)]));
    }

    #[test]
    fn residue_area_threshold_is_winding_independent_and_inclusive() {
        let a = [(0, 0), (16, 0), (0, 16)];
        assert!(residue_screen_coverage(a));
        assert!(residue_screen_coverage([a[2], a[1], a[0]]));
        assert!(!residue_screen_coverage([(0, 0), (16, 0), (0, 15)]));
        assert!(!residue_screen_coverage([(0, 0), (16, 16), (32, 32)]));
        assert!(residue_screen_coverage([
            (-1022, -1022),
            (1022, -1022),
            (0, 1022)
        ]));
    }
}
