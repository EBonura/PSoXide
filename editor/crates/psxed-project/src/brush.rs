//! Convex-brush geometry kernel (docs/brush-editor-integration.md).
//!
//! A brush is a convex solid described as the intersection of half-spaces.
//! Each face stores three integer points that define its plane, following
//! the Quake `.map` convention: wound counter-clockwise when viewed from
//! outside, so the derived normal points out of the brush. Plane predicates
//! are exact (wide-integer cross/dot on the authored points); face polygons
//! are solved in f64 (three integer planes meet at rational points) and
//! snapped by tools, not by the kernel.

use serde::{Deserialize, Serialize};

/// Per-face texture placement over the paraxial projection:
/// `uv' = rotate(rotation_deg, uv * 256 / scale_q8) + offset_texels`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaceUv {
    /// Texel offset added after rotation/scale.
    pub offset_texels: [i16; 2],
    /// Free rotation in degrees.
    pub rotation_deg: i16,
    /// Per-axis scale, Q8 (256 = 1.0). Larger = texture appears bigger.
    pub scale_q8: [i16; 2],
}

impl Default for FaceUv {
    fn default() -> Self {
        Self {
            offset_texels: [0, 0],
            rotation_deg: 0,
            scale_q8: [256, 256],
        }
    }
}

impl FaceUv {
    /// Whether this is the identity placement.
    pub fn is_identity(&self) -> bool {
        *self == Self::default()
    }

    /// Compensate the offset so a raw-texel shift of the underlying
    /// surface leaves applied UVs unchanged (texture lock). Exact up to
    /// the i16 offset rounding.
    pub fn compensate_shift(&mut self, raw_shift: [f64; 2]) {
        let with_offset = self.apply(raw_shift);
        let linear = [
            with_offset[0] - f64::from(self.offset_texels[0]),
            with_offset[1] - f64::from(self.offset_texels[1]),
        ];
        self.offset_texels[0] =
            (f64::from(self.offset_texels[0]) - linear[0]).round() as i16;
        self.offset_texels[1] =
            (f64::from(self.offset_texels[1]) - linear[1]).round() as i16;
    }

    /// Apply to a raw paraxial texel coordinate.
    pub fn apply(&self, uv: [f64; 2]) -> [f64; 2] {
        let sx = f64::from(self.scale_q8[0].max(1)) / 256.0;
        let sy = f64::from(self.scale_q8[1].max(1)) / 256.0;
        let scaled = [uv[0] / sx, uv[1] / sy];
        let radians = f64::from(self.rotation_deg).to_radians();
        let (sin, cos) = radians.sin_cos();
        [
            scaled[0] * cos - scaled[1] * sin + f64::from(self.offset_texels[0]),
            scaled[0] * sin + scaled[1] * cos + f64::from(self.offset_texels[1]),
        ]
    }
}

/// One brush face: three integer points defining the plane, plus the
/// face's material (None = untextured default) and texture placement.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrushFace {
    /// Plane points, counter-clockwise viewed from outside the brush.
    pub points: [[i32; 3]; 3],
    /// Material applied to this face.
    #[serde(default)]
    pub material: Option<crate::ResourceId>,
    /// Texture placement over the paraxial projection.
    #[serde(default)]
    pub uv: FaceUv,
}

impl BrushFace {
    /// Face from plane points with no material and identity UVs.
    pub fn from_points(points: [[i32; 3]; 3]) -> Self {
        Self {
            points,
            material: None,
            uv: FaceUv::default(),
        }
    }
}

/// A convex brush as an unordered set of faces.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Brush {
    /// Boundary faces; outward normals derived from their points.
    pub faces: Vec<BrushFace>,
}

/// Exact unnormalized plane `dot(normal, p) == dist` from integer points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plane {
    /// Outward normal (cross product of the two face edges, unnormalized).
    pub normal: [i64; 3],
    /// Plane distance in the same unnormalized scale.
    pub dist: i64,
}

impl Plane {
    /// Derive the plane from three points; `None` when they are collinear.
    pub fn from_points(points: [[i32; 3]; 3]) -> Option<Self> {
        let [a, b, c] = points.map(|p| p.map(i64::from));
        let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let normal = [
            ab[1] * ac[2] - ab[2] * ac[1],
            ab[2] * ac[0] - ab[0] * ac[2],
            ab[0] * ac[1] - ab[1] * ac[0],
        ];
        if normal == [0, 0, 0] {
            return None;
        }
        let dist = normal[0] * a[0] + normal[1] * a[1] + normal[2] * a[2];
        Some(Self { normal, dist })
    }

    /// Signed side of an integer point: >0 outside, 0 on, <0 inside.
    pub fn side(&self, p: [i32; 3]) -> i64 {
        let p = p.map(i64::from);
        self.normal[0] * p[0] + self.normal[1] * p[1] + self.normal[2] * p[2] - self.dist
    }

    fn normalized(&self) -> ([f64; 3], f64) {
        let n = self.normal.map(|v| v as f64);
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        ([n[0] / len, n[1] / len, n[2] / len], self.dist as f64 / len)
    }
}

/// Solved polygon of one face, wound like the face (outward-facing).
#[derive(Clone, Debug, PartialEq)]
pub struct FacePolygon {
    /// Polygon vertices; rational in general, snapped by tools.
    pub verts: Vec<[f64; 3]>,
}

/// Result of solving a brush's faces against each other.
#[derive(Clone, Debug, PartialEq)]
pub struct SolvedBrush {
    /// Per input face: its boundary polygon, or `None` when the face is
    /// degenerate or entirely cut away by the other planes (a redundant
    /// face; clip workflows produce these legitimately).
    pub polygons: Vec<Option<FacePolygon>>,
    /// Axis-aligned bounds over every polygon vertex (empty brush: zeros).
    pub min: [f64; 3],
    /// See `min`.
    pub max: [f64; 3],
}

impl SolvedBrush {
    /// A brush encloses volume only if at least four faces survived.
    pub fn is_valid(&self) -> bool {
        self.polygons.iter().flatten().count() >= 4
    }
}

/// Half the side length of the base winding a face polygon is clipped
/// from; bounds the world size a single brush face can span.
pub const BASE_WINDING_EXTENT: f64 = (1 << 21) as f64;
const CLIP_EPSILON: f64 = 1.0 / 1024.0;
const WELD_EPSILON: f64 = 1.0 / 256.0;

impl Brush {
    /// Axis-aligned cuboid between two opposite corners.
    pub fn cuboid(min: [i32; 3], max: [i32; 3]) -> Self {
        let [x0, y0, z0] = min;
        let [x1, y1, z1] = max;
        // ponytail: face points written out; a loop obscures the windings.
        let faces = [
            [[x0, y0, z0], [x0, y1, z0], [x1, y1, z0]], // -Z
            [[x1, y0, z1], [x1, y1, z1], [x0, y1, z1]], // +Z
            [[x0, y0, z1], [x0, y1, z1], [x0, y1, z0]], // -X
            [[x1, y0, z0], [x1, y1, z0], [x1, y1, z1]], // +X
            [[x0, y0, z0], [x1, y0, z0], [x1, y0, z1]], // -Y
            [[x0, y1, z1], [x1, y1, z1], [x1, y1, z0]], // +Y
        ];
        Self {
            faces: faces.map(BrushFace::from_points).into(),
        }
    }

    /// Solve every face polygon by clipping its base winding against all
    /// other face planes.
    pub fn solve(&self) -> SolvedBrush {
        let planes: Vec<Option<Plane>> = self
            .faces
            .iter()
            .map(|f| Plane::from_points(f.points))
            .collect();

        let mut polygons = Vec::with_capacity(self.faces.len());
        for (index, plane) in planes.iter().enumerate() {
            let Some(plane) = plane else {
                polygons.push(None);
                continue;
            };
            let mut winding = base_winding(plane, self.faces[index].points[0]);
            for (other_index, other) in planes.iter().enumerate() {
                if other_index == index {
                    continue;
                }
                let Some(other) = other else { continue };
                winding = clip_winding(&winding, other);
                if winding.len() < 3 {
                    break;
                }
            }
            polygons.push((winding.len() >= 3).then(|| FacePolygon {
                verts: weld(winding),
            }));
        }

        let mut min = [f64::INFINITY; 3];
        let mut max = [f64::NEG_INFINITY; 3];
        let mut any = false;
        for v in polygons.iter().flatten().flat_map(|p| p.verts.iter()) {
            any = true;
            for axis in 0..3 {
                min[axis] = min[axis].min(v[axis]);
                max[axis] = max[axis].max(v[axis]);
            }
        }
        if !any {
            min = [0.0; 3];
            max = [0.0; 3];
        }
        SolvedBrush { polygons, min, max }
    }
}

/// Result of splitting a brush by a plane: the kept halves, already
/// pruned of dead faces. A side is `None` when the plane misses the brush
/// on that side.
#[derive(Clone, Debug, PartialEq)]
pub struct ClippedBrush {
    /// Half on the inside of the clip plane (behind its outward normal),
    /// capped with the clip face as given.
    pub back: Option<Brush>,
    /// Half on the outside, capped with the clip face reversed.
    pub front: Option<Brush>,
}

impl Brush {
    /// Translate the whole brush by an integer delta.
    pub fn translate(&mut self, delta: [i32; 3]) {
        for face in &mut self.faces {
            for point in &mut face.points {
                for axis in 0..3 {
                    point[axis] += delta[axis];
                }
            }
        }
    }

    /// Translate while keeping each face's texture anchored to the brush
    /// (texture lock): every face's UV offset is compensated by the
    /// paraxial shift the move causes. Exact for any rotation/scale up to
    /// the i16 offset rounding; `units_per_texel` is the paraxial texel
    /// density the renderer uses.
    pub fn translate_with_uv_lock(&mut self, delta: [i32; 3], units_per_texel: f64) {
        let delta_f = [
            f64::from(delta[0]),
            f64::from(delta[1]),
            f64::from(delta[2]),
        ];
        for face in &mut self.faces {
            if let Some(plane) = Plane::from_points(face.points) {
                // Paraxial projection is linear, so the raw shift of any
                // surface point equals the projection of the delta itself.
                let raw = paraxial_uv(&plane, delta_f);
                let shift = [raw[0] / units_per_texel, raw[1] / units_per_texel];
                face.uv.compensate_shift(shift);
            }
        }
        self.translate(delta);
    }

    /// Translate one face's plane by an integer delta (the extrude/resize
    /// core: the tool constrains `delta` to the face normal's direction;
    /// the kernel only moves the plane and lets `solve` reshape the brush).
    pub fn translate_face(&mut self, face: usize, delta: [i32; 3]) {
        if let Some(face) = self.faces.get_mut(face) {
            for point in &mut face.points {
                for axis in 0..3 {
                    point[axis] += delta[axis];
                }
            }
        }
    }

    /// Drop faces that no longer bound the solid (their solved polygon is
    /// gone). Used after clips so brushes don't accumulate dead planes.
    pub fn pruned(&self) -> Brush {
        let solved = self.solve();
        Brush {
            faces: self
                .faces
                .iter()
                .zip(&solved.polygons)
                .filter_map(|(face, polygon)| polygon.is_some().then_some(*face))
                .collect(),
        }
    }

    /// Split by the plane of `points` (wound as usual: outward normal).
    /// The clip-tool core: keep back, front, or both.
    pub fn clip(&self, points: [[i32; 3]; 3]) -> ClippedBrush {
        let make = |cap: BrushFace| {
            let mut half = self.clone();
            half.faces.push(cap);
            let half = half.pruned();
            // A side survives when it still encloses volume. A redundant
            // cap (plane misses the brush) prunes away and leaves the
            // whole brush on that side; an all-consuming cap leaves
            // nothing valid and the side is dropped.
            half.solve().is_valid().then_some(half)
        };
        let back = make(BrushFace::from_points(points));
        let front = make(BrushFace::from_points([points[0], points[2], points[1]]));
        ClippedBrush { back, front }
    }

    /// Nearest ray hit against the convex solid: `Some((t, face_index))`
    /// with `t` in units of `dir` length. The picking core for brush
    /// selection; slab test over the face planes.
    pub fn raycast(&self, origin: [f64; 3], dir: [f64; 3]) -> Option<(f64, usize)> {
        let mut t_enter = f64::NEG_INFINITY;
        let mut t_exit = f64::INFINITY;
        let mut enter_face = None;
        for (index, face) in self.faces.iter().enumerate() {
            let plane = Plane::from_points(face.points)?;
            let (n, d) = plane.normalized();
            let denom = dir[0] * n[0] + dir[1] * n[1] + dir[2] * n[2];
            let start = origin[0] * n[0] + origin[1] * n[1] + origin[2] * n[2] - d;
            if denom.abs() < f64::EPSILON {
                if start > 0.0 {
                    return None; // parallel and fully outside this plane
                }
                continue;
            }
            let t = -start / denom;
            if denom < 0.0 {
                // Entering through this face.
                if t > t_enter {
                    t_enter = t;
                    enter_face = Some(index);
                }
            } else if t < t_exit {
                t_exit = t;
            }
            if t_enter > t_exit {
                return None;
            }
        }
        let face = enter_face?;
        (t_enter >= 0.0).then_some((t_enter, face))
    }
}

impl Brush {
    /// Cuboid between two arbitrary opposite corners (create-tool core:
    /// drag order does not matter). `None` when any axis is flat.
    pub fn cuboid_from_corners(a: [i32; 3], b: [i32; 3]) -> Option<Self> {
        let min = [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])];
        let max = [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])];
        (0..3).all(|axis| min[axis] < max[axis]).then(|| Self::cuboid(min, max))
    }

    /// Snap every authored point to a grid step (kept positive; the
    /// resulting faces may degenerate, which `solve` reports per face).
    pub fn snap_to_grid(&mut self, step: i32) {
        let step = step.max(1);
        // Round to nearest: div_euclid floors, so one +half shift is
        // correct for negatives too (-1 -> 0, -17 -> -32 at step 32).
        let snap = |v: i32| (v + step / 2).div_euclid(step) * step;
        for face in &mut self.faces {
            for point in &mut face.points {
                for axis in 0..3 {
                    point[axis] = snap(point[axis]);
                }
            }
        }
    }
}

impl Brush {
    /// Hollow an axis-aligned brush into six wall slabs of `thickness`,
    /// keeping the outer bounds. Returns `None` when the brush is too
    /// small to hollow or has no volume. Face materials carry to every
    /// slab from the source face where unambiguous (outer faces).
    pub fn hollow(&self, thickness: i32) -> Option<Vec<Brush>> {
        let solved = self.solve();
        if !solved.is_valid() {
            return None;
        }
        let min = [
            solved.min[0].round() as i32,
            solved.min[1].round() as i32,
            solved.min[2].round() as i32,
        ];
        let max = [
            solved.max[0].round() as i32,
            solved.max[1].round() as i32,
            solved.max[2].round() as i32,
        ];
        let thickness = thickness.max(1);
        if (0..3).any(|a| max[a] - min[a] <= 2 * thickness) {
            return None;
        }
        let inner_min = [min[0] + thickness, min[1] + thickness, min[2] + thickness];
        let inner_max = [max[0] - thickness, max[1] - thickness, max[2] - thickness];
        // Six slabs: floor, ceiling, then four walls between them.
        Some(vec![
            Brush::cuboid(min, [max[0], inner_min[1], max[2]]),
            Brush::cuboid([min[0], inner_max[1], min[2]], max),
            Brush::cuboid(
                [min[0], inner_min[1], min[2]],
                [inner_min[0], inner_max[1], max[2]],
            ),
            Brush::cuboid(
                [inner_max[0], inner_min[1], min[2]],
                [max[0], inner_max[1], max[2]],
            ),
            Brush::cuboid(
                [inner_min[0], inner_min[1], min[2]],
                [inner_max[0], inner_max[1], inner_min[2]],
            ),
            Brush::cuboid(
                [inner_min[0], inner_min[1], inner_max[2]],
                [inner_max[0], inner_max[1], max[2]],
            ),
        ])
    }
}

/// World units per paraxial texel, the renderer's brush texel density
/// (matches the grid default: 64 texels across a 1024-unit sector).
pub const BRUSH_UV_UNITS_PER_TEXEL: f64 = 16.0;

/// Paraxial (dominant-axis) texture projection of a world position for a
/// face with the given plane: the Quake-style default mapping. Returns
/// texel-space (u, v) before per-face offset/rotation/scale.
pub fn paraxial_uv(plane: &Plane, world: [f64; 3]) -> [f64; 2] {
    let n = plane.normal.map(|v| v as f64);
    let abs = n.map(f64::abs);
    if abs[1] >= abs[0] && abs[1] >= abs[2] {
        [world[0], world[2]] // floor/ceiling: XZ
    } else if abs[0] >= abs[2] {
        [world[2], -world[1]] // X-facing wall: ZY
    } else {
        [world[0], -world[1]] // Z-facing wall: XY
    }
}

/// Large quad on `plane` around `anchor`, wound to face the same way.
fn base_winding(plane: &Plane, anchor: [i32; 3]) -> Vec<[f64; 3]> {
    let (n, _) = plane.normalized();
    // Least-dominant axis seeds a tangent that cannot be parallel to n.
    let mut axis = 0;
    for candidate in 1..3 {
        if n[candidate].abs() < n[axis].abs() {
            axis = candidate;
        }
    }
    let mut up = [0.0; 3];
    up[axis] = 1.0;
    let right = cross(n, up);
    let right = scale(right, BASE_WINDING_EXTENT / length(right));
    let up = cross(right, n);
    let up = scale(up, BASE_WINDING_EXTENT / length(up));

    let center = anchor.map(f64::from);
    vec![
        add(center, add(neg(right), neg(up))),
        add(center, add(right, neg(up))),
        add(center, add(right, up)),
        add(center, add(neg(right), up)),
    ]
}

/// Keep the half of `winding` on the inside (`dot <= dist`) of `plane`.
fn clip_winding(winding: &[[f64; 3]], plane: &Plane) -> Vec<[f64; 3]> {
    let (n, d) = plane.normalized();
    let dist = |v: [f64; 3]| v[0] * n[0] + v[1] * n[1] + v[2] * n[2] - d;

    let mut out = Vec::with_capacity(winding.len() + 2);
    for (i, &current) in winding.iter().enumerate() {
        let next = winding[(i + 1) % winding.len()];
        let dc = dist(current);
        let dn = dist(next);
        if dc <= CLIP_EPSILON {
            out.push(current);
        }
        if (dc < -CLIP_EPSILON && dn > CLIP_EPSILON) || (dc > CLIP_EPSILON && dn < -CLIP_EPSILON) {
            let t = dc / (dc - dn);
            out.push([
                current[0] + t * (next[0] - current[0]),
                current[1] + t * (next[1] - current[1]),
                current[2] + t * (next[2] - current[2]),
            ]);
        }
    }
    out
}

/// Drop consecutive duplicate vertices within the weld epsilon.
fn weld(winding: Vec<[f64; 3]>) -> Vec<[f64; 3]> {
    let mut out: Vec<[f64; 3]> = Vec::with_capacity(winding.len());
    for v in winding {
        if let Some(last) = out.last() {
            if (0..3).all(|a| (last[a] - v[a]).abs() <= WELD_EPSILON) {
                continue;
            }
        }
        out.push(v);
    }
    if out.len() >= 2 {
        let first = out[0];
        if let Some(last) = out.last() {
            if (0..3).all(|a| (last[a] - first[a]).abs() <= WELD_EPSILON) {
                out.pop();
            }
        }
    }
    out
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn length(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

fn scale(v: [f64; 3], s: f64) -> [f64; 3] {
    [v[0] * s, v[1] * s, v[2] * s]
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn neg(v: [f64; 3]) -> [f64; 3] {
    [-v[0], -v[1], -v[2]]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_verts(solved: &SolvedBrush) -> Vec<[i64; 3]> {
        let mut verts: Vec<[i64; 3]> = solved
            .polygons
            .iter()
            .flatten()
            .flat_map(|p| p.verts.iter())
            .map(|v| [v[0].round() as i64, v[1].round() as i64, v[2].round() as i64])
            .collect();
        verts.sort_unstable();
        verts.dedup();
        verts
    }

    #[test]
    fn cuboid_solves_to_six_quads() {
        let brush = Brush::cuboid([0, 0, 0], [256, 128, 512]);
        let solved = brush.solve();
        assert!(solved.is_valid());
        assert_eq!(solved.polygons.len(), 6);
        for polygon in solved.polygons.iter() {
            assert_eq!(polygon.as_ref().unwrap().verts.len(), 4);
        }
        assert_eq!(unique_verts(&solved).len(), 8);
        assert_eq!(solved.min, [0.0, 0.0, 0.0]);
        assert_eq!(solved.max, [256.0, 128.0, 512.0]);
    }

    #[test]
    fn cuboid_normals_point_outward() {
        let brush = Brush::cuboid([0, 0, 0], [64, 64, 64]);
        for face in &brush.faces {
            let plane = Plane::from_points(face.points).unwrap();
            assert!(plane.side([32, 32, 32]) < 0, "centre must be inside");
        }
    }

    #[test]
    fn wedge_from_five_planes() {
        let mut brush = Brush::cuboid([0, 0, 0], [128, 128, 128]);
        // Replace +Y with a slope through (0,128,*) and (128,0,*): CCW from
        // outside (above), normal (1,1,0)-ish.
        brush.faces[5] = BrushFace::from_points([[0, 128, 0], [0, 128, 128], [128, 0, 128]]);
        let solved = brush.solve();
        assert!(solved.is_valid());
        let counts: Vec<usize> = solved
            .polygons
            .iter()
            .map(|p| p.as_ref().map_or(0, |p| p.verts.len()))
            .collect();
        // -Z and +Z become triangles, -X keeps 4, +X is cut away by the
        // slope through its top edge? No: slope passes through x=128,y=0,
        // so +X survives as a degenerate edge -> dropped or triangle.
        assert_eq!(counts[0], 3, "-Z face becomes a triangle");
        assert_eq!(counts[1], 3, "+Z face becomes a triangle");
        assert_eq!(counts[2], 4, "-X face stays a quad");
        assert_eq!(counts[4], 4, "-Y floor stays a quad");
        assert_eq!(counts[5], 4, "slope is a quad");
        assert_eq!(counts[3], 0, "+X collapses to an edge and is dropped");
        assert_eq!(unique_verts(&solved).len(), 6);
    }

    #[test]
    fn redundant_face_is_dropped_not_fatal() {
        let mut brush = Brush::cuboid([0, 0, 0], [64, 64, 64]);
        // A plane far outside the cube contributes no polygon.
        brush.faces.push(BrushFace::from_points([[0, 200, 0], [0, 200, 64], [64, 200, 64]]));
        let solved = brush.solve();
        assert!(solved.is_valid());
        assert!(solved.polygons[6].is_none());
        assert_eq!(unique_verts(&solved).len(), 8);
    }

    #[test]
    fn collinear_points_are_degenerate() {
        assert!(Plane::from_points([[0, 0, 0], [1, 1, 1], [2, 2, 2]]).is_none());
        let mut brush = Brush::cuboid([0, 0, 0], [64, 64, 64]);
        brush.faces[0] = BrushFace::from_points([[0, 0, 0], [1, 1, 1], [2, 2, 2]]);
        let solved = brush.solve();
        assert!(solved.polygons[0].is_none());
    }

    #[test]
    fn translate_moves_bounds() {
        let mut brush = Brush::cuboid([0, 0, 0], [64, 64, 64]);
        brush.translate([128, -32, 256]);
        let solved = brush.solve();
        assert_eq!(solved.min, [128.0, -32.0, 256.0]);
        assert_eq!(solved.max, [192.0, 32.0, 320.0]);
    }

    #[test]
    fn face_translate_extrudes() {
        let mut brush = Brush::cuboid([0, 0, 0], [64, 64, 64]);
        // Push +X (face index 3) out by 64: the box widens.
        brush.translate_face(3, [64, 0, 0]);
        let solved = brush.solve();
        assert!(solved.is_valid());
        assert_eq!(solved.max, [128.0, 64.0, 64.0]);
        // Pull it past -X: the brush inverts and stops enclosing volume.
        brush.translate_face(3, [-256, 0, 0]);
        assert!(!brush.solve().is_valid());
    }

    #[test]
    fn clip_splits_into_two_halves() {
        let brush = Brush::cuboid([0, 0, 0], [128, 64, 64]);
        // Vertical clip plane at x=32, wound for a +X outward normal.
        let clipped = brush.clip([[32, 0, 0], [32, 64, 0], [32, 0, 64]]);
        let back = clipped.back.expect("back half");
        let front = clipped.front.expect("front half");
        assert_eq!(back.solve().max, [32.0, 64.0, 64.0]);
        assert_eq!(front.solve().min, [32.0, 0.0, 0.0]);
        assert_eq!(back.faces.len(), 6);
        assert_eq!(front.faces.len(), 6);
    }

    #[test]
    fn clip_missing_the_brush_keeps_one_side() {
        let brush = Brush::cuboid([0, 0, 0], [64, 64, 64]);
        let clipped = brush.clip([[200, 0, 0], [200, 64, 0], [200, 0, 64]]);
        assert!(clipped.front.is_none(), "nothing outside x=200");
        let back = clipped.back.expect("whole brush behind the plane");
        assert_eq!(back.faces.len(), 6, "redundant cap pruned");
    }

    #[test]
    fn raycast_hits_entry_face() {
        let brush = Brush::cuboid([0, 0, 0], [64, 64, 64]);
        // From -X toward the box: hits the -X face (index 2) at t=36/72=0.5.
        let hit = brush.raycast([-36.0, 32.0, 32.0], [72.0, 0.0, 0.0]);
        let (t, face) = hit.expect("ray hits the box");
        assert!((t - 0.5).abs() < 1e-9);
        assert_eq!(face, 2);
        // Pointing away misses.
        assert!(brush.raycast([-36.0, 32.0, 32.0], [-72.0, 0.0, 0.0]).is_none());
        // Parallel ray outside a slab misses.
        assert!(brush.raycast([-36.0, 200.0, 32.0], [72.0, 0.0, 0.0]).is_none());
        // Origin inside: entry is behind the origin -> no hit (t >= 0 rule).
        assert!(brush.raycast([32.0, 32.0, 32.0], [72.0, 0.0, 0.0]).is_none());
    }

    #[test]
    fn cuboid_from_corners_normalizes_and_rejects_flat() {
        let brush = Brush::cuboid_from_corners([64, 0, 128], [0, 32, 0]).unwrap();
        let solved = brush.solve();
        assert_eq!(solved.min, [0.0, 0.0, 0.0]);
        assert_eq!(solved.max, [64.0, 32.0, 128.0]);
        assert!(Brush::cuboid_from_corners([0, 0, 0], [64, 0, 64]).is_none());
    }

    #[test]
    fn snap_to_grid_rounds_points() {
        let mut brush = Brush::cuboid([1, 30, -1], [65, 63, 62]);
        brush.snap_to_grid(32);
        let solved = brush.solve();
        assert_eq!(solved.min, [0.0, 32.0, 0.0]);
        assert_eq!(solved.max, [64.0, 64.0, 64.0]);
    }

    #[test]
    fn paraxial_uv_picks_dominant_axis() {
        let floor = Plane::from_points([[0, 0, 0], [0, 0, 64], [64, 0, 64]]).unwrap();
        assert_eq!(paraxial_uv(&floor, [10.0, 0.0, 20.0]), [10.0, 20.0]);
        let wall_x = Plane::from_points([[0, 0, 0], [0, 64, 0], [0, 0, 64]]).unwrap();
        assert_eq!(paraxial_uv(&wall_x, [0.0, 10.0, 20.0]), [20.0, -10.0]);
        let wall_z = Plane::from_points([[0, 0, 0], [0, 64, 0], [64, 0, 0]]).unwrap();
        assert_eq!(paraxial_uv(&wall_z, [10.0, 20.0, 0.0]), [10.0, -20.0]);
    }

    #[test]
    fn hollow_makes_six_slabs_enclosing_a_cavity() {
        let brush = Brush::cuboid([0, 0, 0], [256, 128, 256]);
        let slabs = brush.hollow(16).expect("hollowable");
        assert_eq!(slabs.len(), 6);
        for slab in &slabs {
            assert!(slab.solve().is_valid());
        }
        // Outer bounds preserved.
        let mut min = [f64::INFINITY; 3];
        let mut max = [f64::NEG_INFINITY; 3];
        for slab in &slabs {
            let s = slab.solve();
            for a in 0..3 {
                min[a] = min[a].min(s.min[a]);
                max[a] = max[a].max(s.max[a]);
            }
        }
        assert_eq!(min, [0.0, 0.0, 0.0]);
        assert_eq!(max, [256.0, 128.0, 256.0]);
        // The cavity centre is inside no slab.
        for slab in &slabs {
            let outside = slab.faces.iter().any(|face| {
                Plane::from_points(face.points)
                    .unwrap()
                    .side([128, 64, 128])
                    > 0
            });
            assert!(outside, "cavity centre must be outside every slab");
        }
        // Too-thin brushes refuse to hollow.
        assert!(Brush::cuboid([0, 0, 0], [30, 128, 256]).hollow(16).is_none());
    }

    #[test]
    fn face_uv_apply_offset_scale_rotation() {
        assert_eq!(FaceUv::default().apply([3.0, 4.0]), [3.0, 4.0]);
        let uv = FaceUv {
            offset_texels: [10, -5],
            rotation_deg: 0,
            scale_q8: [512, 256],
        };
        // Scale 512 = 2x texture size = raw texels halve, then offset.
        assert_eq!(uv.apply([8.0, 4.0]), [14.0, -1.0]);
        let rot = FaceUv {
            rotation_deg: 90,
            ..Default::default()
        };
        let r = rot.apply([1.0, 0.0]);
        assert!(r[0].abs() < 1e-9 && (r[1] - 1.0).abs() < 1e-9);
    }

    #[test]
    fn texture_lock_keeps_applied_uv_through_moves() {
        let mut brush = Brush::cuboid([0, 0, 0], [128, 64, 128]);
        brush.faces[5].uv = FaceUv {
            offset_texels: [7, 3],
            rotation_deg: 30,
            scale_q8: [512, 256],
        };
        let sample = |brush: &Brush, world: [f64; 3]| {
            let plane = Plane::from_points(brush.faces[5].points).unwrap();
            let raw = paraxial_uv(&plane, world);
            brush.faces[5].uv.apply([
                raw[0] / BRUSH_UV_UNITS_PER_TEXEL,
                raw[1] / BRUSH_UV_UNITS_PER_TEXEL,
            ])
        };
        let before = sample(&brush, [32.0, 64.0, 48.0]);
        brush.translate_with_uv_lock([160, 0, -96], BRUSH_UV_UNITS_PER_TEXEL);
        let after = sample(&brush, [192.0, 64.0, -48.0]);
        // Exact up to i16 offset rounding.
        assert!((after[0] - before[0]).abs() <= 1.0);
        assert!((after[1] - before[1]).abs() <= 1.0);
    }

    #[test]
    fn face_materials_survive_clip_and_extrude() {
        let mut brush = Brush::cuboid([0, 0, 0], [128, 64, 64]);
        brush.faces[3].material = Some(crate::ResourceId(7)); // +X face
        let clipped = brush.clip([[64, 0, 0], [64, 64, 0], [64, 0, 64]]);
        let front = clipped.front.expect("front half");
        assert!(
            front
                .faces
                .iter()
                .any(|f| f.material == Some(crate::ResourceId(7))),
            "+X face keeps its material in the front half"
        );
        let back = clipped.back.expect("back half");
        assert!(back.faces.iter().all(|f| f.material.is_none()));

        let mut extruded = Brush::cuboid([0, 0, 0], [64, 64, 64]);
        extruded.faces[0].material = Some(crate::ResourceId(9));
        extruded.translate_face(0, [0, 0, -32]);
        assert_eq!(extruded.faces[0].material, Some(crate::ResourceId(9)));
    }

    #[test]
    fn integer_plane_side_is_exact() {
        let plane = Plane::from_points([[0, 128, 0], [0, 128, 128], [128, 0, 128]]).unwrap();
        assert_eq!(plane.side([0, 128, 64]), 0);
        assert_eq!(plane.side([128, 0, 0]), 0);
        assert!(plane.side([0, 0, 0]) < 0);
        assert!(plane.side([128, 128, 0]) > 0);
    }
}
