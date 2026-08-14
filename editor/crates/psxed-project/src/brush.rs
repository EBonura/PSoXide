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
        self.offset_texels[0] = (f64::from(self.offset_texels[0]) - linear[0]).round() as i16;
        self.offset_texels[1] = (f64::from(self.offset_texels[1]) - linear[1]).round() as i16;
    }

    /// Apply to a raw paraxial texel coordinate.
    pub fn apply(&self, uv: [f64; 2]) -> [f64; 2] {
        let linear = self.apply_linear(uv);
        [
            linear[0] + f64::from(self.offset_texels[0]),
            linear[1] + f64::from(self.offset_texels[1]),
        ]
    }

    /// [`Self::apply`] without the offset term: the scale and rotation
    /// alone. Re-anchoring solves for the offset, so it needs the rest of
    /// the mapping separately.
    pub fn apply_linear(&self, uv: [f64; 2]) -> [f64; 2] {
        let sx = axis_scale_q8(self.scale_q8[0]);
        let sy = axis_scale_q8(self.scale_q8[1]);
        let scaled = [uv[0] / sx, uv[1] / sy];
        let radians = f64::from(self.rotation_deg).to_radians();
        let (sin, cos) = radians.sin_cos();
        [
            scaled[0] * cos - scaled[1] * sin,
            scaled[0] * sin + scaled[1] * cos,
        ]
    }

    /// Re-solve this mapping's offset so `anchor` keeps the UV `previous`
    /// gave it.
    ///
    /// Scale and rotation are applied about the raw projection's own
    /// origin, which is the world origin, not the face. A face 4000 units
    /// away therefore SLIDES by thousands of texels when its scale changes
    /// by a few percent, which is why changing U% read as sliding rather
    /// than resizing. Anchoring at a point on the face itself (the caller
    /// passes the solved polygon centroid) leaves the texture where the
    /// eye expects it and only changes how densely it repeats.
    ///
    /// `slide_texels` is any offset edit the same frame made, re-applied
    /// after the compensation so typing into Offset U/V still slides the
    /// texture deliberately.
    pub fn reanchor(&mut self, previous: &Self, anchor: [f64; 2], slide_texels: [f64; 2]) {
        self.reanchor_to(previous.apply(anchor), anchor, slide_texels);
    }

    /// [`Self::reanchor`] against an explicit target UV.
    ///
    /// A multi-frame edit has to keep solving against the phase the whole
    /// interaction started at. Re-deriving the target from the previous
    /// frame's mapping instead banks this function's own `i16` rounding
    /// once per frame, which walks the texture off the face over a drag.
    pub fn reanchor_to(&mut self, target: [f64; 2], anchor: [f64; 2], slide_texels: [f64; 2]) {
        let linear = self.apply_linear(anchor);
        self.offset_texels = [
            clamp_offset_texels(target[0] - linear[0] + slide_texels[0]),
            clamp_offset_texels(target[1] - linear[1] + slide_texels[1]),
        ];
    }
}

/// Q8 axis scale to a factor. Negative mirrors the axis (Flip H/V);
/// zero is treated as identity so damaged data cannot divide by zero.
fn axis_scale_q8(q8: i16) -> f64 {
    if q8 == 0 { 1.0 } else { f64::from(q8) / 256.0 }
}

/// Offsets are stored in an `i16`; a re-anchor of a far-away face at a
/// steep scale change can solve past that, and saturating is the only
/// behaviour that keeps the mapping finite.
fn clamp_offset_texels(value: f64) -> i16 {
    if value.is_nan() {
        return 0;
    }
    value
        .round()
        .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
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

/// Volume contents assigned to one authored convex brush.
///
/// `Solid` is the historical/default behavior. The liquid variants are
/// non-blocking PXBSP contents volumes: their boundary faces still render,
/// while point-contents queries distinguish the medium inside them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BrushContents {
    #[default]
    Solid,
    Water,
    Slime,
    Lava,
}

impl BrushContents {
    pub const ALL: [Self; 4] = [Self::Solid, Self::Water, Self::Slime, Self::Lava];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Solid => "Solid",
            Self::Water => "Water",
            Self::Slime => "Slime",
            Self::Lava => "Lava",
        }
    }

    pub const fn is_solid(&self) -> bool {
        matches!(self, Self::Solid)
    }

    /// PXBSP terminal contents code shared with `psx-bsp` and Quake.
    pub const fn runtime_contents(self) -> i16 {
        match self {
            Self::Solid => psx_bsp::collision::CONTENTS_SOLID,
            Self::Water => psx_bsp::collision::CONTENTS_WATER,
            Self::Slime => psx_bsp::collision::CONTENTS_SLIME,
            Self::Lava => psx_bsp::collision::CONTENTS_LAVA,
        }
    }

    /// Deterministic overlap precedence. Structural solid always wins.
    pub(crate) const fn precedence(self) -> u8 {
        match self {
            Self::Solid => 4,
            Self::Lava => 3,
            Self::Slime => 2,
            Self::Water => 1,
        }
    }
}

/// A convex brush as an unordered set of faces.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Brush {
    /// Boundary faces; outward normals derived from their points.
    pub faces: Vec<BrushFace>,
    /// Structural solid or one of the Quake-compatible liquid contents.
    /// Omitted for solid brushes so existing project files remain byte-stable.
    #[serde(default, skip_serializing_if = "BrushContents::is_solid")]
    pub contents: BrushContents,
    /// Logic mover that owns this brush, or `None` for static world geometry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mover: Option<crate::NodeId>,
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
    ///
    /// CAUTION: four surviving faces do NOT imply a bounded solid; an
    /// infinite wedge (planes tilted parallel by an edge/vertex drag)
    /// passes this test with polygons clipped only by the base winding
    /// (coordinates at [`BASE_WINDING_EXTENT`]). Editing previews must
    /// also check [`Self::within_extent`] or huge vertices reach the
    /// renderer and overflow its i32 camera math.
    pub fn is_valid(&self) -> bool {
        self.polygons.iter().flatten().count() >= 4
    }

    /// Whether every solved coordinate stays within `limit` world units
    /// of the origin. The bounded-solid check `is_valid` cannot provide.
    pub fn within_extent(&self, limit: f64) -> bool {
        self.min
            .iter()
            .chain(self.max.iter())
            .all(|value| value.is_finite() && value.abs() <= limit)
    }
}

/// Extent cap for brush editing previews. Generous for any real map
/// (the largest authored worlds span ~25k units) while safely below the
/// renderer's i32 overflow threshold (|vertex - camera| * 4096 must fit
/// in i32, i.e. ~524k combined).
pub const BRUSH_EDIT_EXTENT_LIMIT: f64 = (1 << 17) as f64;

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
            contents: BrushContents::Solid,
            mover: None,
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

    /// The point a face's UV edits should pivot around: its solved polygon
    /// centroid, in the raw texel space [`FaceUv::apply`] consumes. `None`
    /// when the face is degenerate or fully cut away.
    pub fn face_uv_anchor(&self, face: usize) -> Option<[f64; 2]> {
        let plane = Plane::from_points(self.faces.get(face)?.points)?;
        let polygon = self.solve().polygons.get(face)?.clone()?;
        if polygon.verts.is_empty() {
            return None;
        }
        let count = polygon.verts.len() as f64;
        let mut centroid = [0.0; 3];
        for vert in &polygon.verts {
            for axis in 0..3 {
                centroid[axis] += vert[axis];
            }
        }
        let centroid = centroid.map(|sum| sum / count);
        let raw = paraxial_uv(&plane, centroid);
        Some([
            raw[0] / BRUSH_UV_UNITS_PER_TEXEL,
            raw[1] / BRUSH_UV_UNITS_PER_TEXEL,
        ])
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
            contents: self.contents,
            mover: self.mover,
        }
    }

    /// Split by the plane of `points` (wound as usual: outward normal).
    /// The clip-tool core: keep back, front, or both.
    pub fn clip(&self, points: [[i32; 3]; 3]) -> ClippedBrush {
        let back = self.with_cap(BrushFace::from_points(points));
        let front = self.with_cap(BrushFace::from_points([points[0], points[2], points[1]]));
        ClippedBrush { back, front }
    }

    /// Half of `self` behind `cap`'s plane, or `None` when nothing
    /// survives there. A side survives when it still encloses volume: a
    /// redundant cap (plane misses the brush) prunes away and leaves
    /// the whole brush on that side; an all-consuming cap leaves
    /// nothing valid and the side is dropped.
    fn with_cap(&self, cap: BrushFace) -> Option<Brush> {
        let mut half = self.clone();
        half.faces.push(cap);
        let half = half.pruned();
        half.solve().is_valid().then_some(half)
    }

    /// Pieces of `self` outside `cutter`, or `None` when the solids do
    /// not intersect (subtract is then the identity). Classic per-plane
    /// carve: each cutter plane splits the running remainder, the part
    /// outside becomes a kept piece, the part inside keeps carving, and
    /// whatever survives every plane is the intersection, which is
    /// discarded. A target fully inside the cutter returns `Some([])`
    /// (swallowed whole). New faces take the cutter face's material and
    /// UV, so a textured cutter paints its own reveal.
    pub fn subtracted_by(&self, cutter: &Brush) -> Option<Vec<Brush>> {
        let mut remainder = self.clone();
        let mut pieces = Vec::new();
        for cutter_face in &cutter.faces {
            let outside_cap = BrushFace {
                points: [
                    cutter_face.points[0],
                    cutter_face.points[2],
                    cutter_face.points[1],
                ],
                material: cutter_face.material,
                uv: cutter_face.uv,
            };
            let inside_cap = BrushFace {
                points: cutter_face.points,
                material: cutter_face.material,
                uv: cutter_face.uv,
            };
            let outside = remainder.with_cap(outside_cap);
            match (remainder.with_cap(inside_cap), outside) {
                (Some(inside), Some(piece)) => {
                    pieces.push(piece);
                    remainder = inside;
                }
                (Some(inside), None) => remainder = inside,
                // The remainder lies fully outside this cutter plane:
                // the solids never intersect, so nothing was cut.
                (None, _) => return None,
            }
        }
        Some(pieces)
    }

    /// Copy with every face plane moved inward along its outward unit
    /// normal by `thickness` world units (rounded to integer points).
    /// `None` when the inset collapses the solid.
    pub fn inset(&self, thickness: i32) -> Option<Brush> {
        if thickness <= 0 {
            return None;
        }
        let mut inner = self.clone();
        for face in &mut inner.faces {
            let plane = Plane::from_points(face.points)?;
            let (normal, _) = plane.normalized();
            let shift =
                normal.map(|component| (-component * f64::from(thickness)).round() as i32);
            for point in &mut face.points {
                for axis in 0..3 {
                    point[axis] += shift[axis];
                }
            }
        }
        inner.is_pickable().then_some(inner)
    }

    /// Wall shell of `self`: the solid minus an inward inset copy, the
    /// one-keystroke room. `None` when the brush is too thin to hollow
    /// at `thickness`.
    pub fn hollowed(&self, thickness: i32) -> Option<Vec<Brush>> {
        self.subtracted_by(&self.inset(thickness)?)
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
        (0..3)
            .all(|axis| min[axis] < max[axis])
            .then(|| Self::cuboid(min, max))
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
    /// Translate every authored face point within `epsilon` of any
    /// `target` by `delta`; returns how many points moved. The vertex
    /// and edge tools build on this: solved corners of tool-authored
    /// brushes coincide with authored points, so dragging a corner
    /// re-tilts exactly the planes anchored there. A face with no
    /// coincident authored point keeps its plane, and the corner slides
    /// along it as the neighbouring planes move.
    /// Apply an affine map about `center` to every authored point within
    /// `epsilon` of a target: `p' = round(map * (p - center) + center)`.
    /// The element gizmo's rotate/scale core; planes stay planar because
    /// each face's three points transform rigidly (up to i32 rounding).
    /// Returns how many authored points moved.
    pub fn transform_points_near(
        &mut self,
        targets: &[[f64; 3]],
        center: [f64; 3],
        map: [[f64; 3]; 3],
        epsilon: f64,
    ) -> usize {
        self.transform_selected(&[], targets, center, map, epsilon)
    }

    /// [`Self::transform_points_near`], plus EVERY authored point of the
    /// listed faces regardless of proximity. Authored plane points need
    /// not coincide with solved corners (clips and rotations move them
    /// off), so transforming a selected FACE by corner proximity alone
    /// misses points and tilts the plane; whole-plane inclusion keeps
    /// face gestures rigid.
    pub fn transform_selected(
        &mut self,
        selected_faces: &[usize],
        targets: &[[f64; 3]],
        center: [f64; 3],
        map: [[f64; 3]; 3],
        epsilon: f64,
    ) -> usize {
        let mut moved = 0;
        for (face_index, face) in self.faces.iter_mut().enumerate() {
            let whole_face = selected_faces.contains(&face_index);
            for point in &mut face.points {
                let hit = whole_face
                    || targets.iter().any(|target| {
                        (0..3).all(|axis| (f64::from(point[axis]) - target[axis]).abs() <= epsilon)
                    });
                if !hit {
                    continue;
                }
                let local = [
                    f64::from(point[0]) - center[0],
                    f64::from(point[1]) - center[1],
                    f64::from(point[2]) - center[2],
                ];
                for axis in 0..3 {
                    let value = map[axis][0] * local[0]
                        + map[axis][1] * local[1]
                        + map[axis][2] * local[2]
                        + center[axis];
                    point[axis] = value.round() as i32;
                }
                moved += 1;
            }
        }
        moved
    }

    pub fn translate_points_near(
        &mut self,
        targets: &[[f64; 3]],
        delta: [i32; 3],
        epsilon: f64,
    ) -> usize {
        self.translate_selected(&[], targets, delta, epsilon, false)
    }

    /// Re-author this brush from its solved geometry: each surviving
    /// face's plane points are rewritten to lie on its polygon corners
    /// (spread for a stable plane), dead planes are pruned, and
    /// material, UV, contents, and mover carry over. The plane of every
    /// face is unchanged, so texture mappings are untouched. Returns
    /// `None` unless the result round-trips: same surviving faces, same
    /// bounds (within a unit), still pickable. Legacy brushes authored
    /// with far-flung plane points become identical to freshly drawn
    /// ones, which is what the editing gestures are tuned for.
    pub fn normalized(&self) -> Option<Brush> {
        let solved = self.solve();
        if !solved.is_valid() || !solved.within_extent(BRUSH_EDIT_EXTENT_LIMIT) {
            return None;
        }
        // Already-normal fast path: every authored point sits on its own
        // face's polygon corners and no dead planes remain. Freshly
        // drawn brushes take this branch, so normalization never churns
        // (or dirties) a healthy project.
        let already_normal = self.faces.iter().enumerate().all(|(index, face)| {
            solved
                .polygons
                .get(index)
                .and_then(Option::as_ref)
                .is_some_and(|polygon| {
                    face.points.iter().all(|point| {
                        polygon.verts.iter().any(|vertex| {
                            (0..3).all(|axis| (vertex[axis] - f64::from(point[axis])).abs() <= 0.5)
                        })
                    })
                })
        });
        if already_normal {
            return Some(self.clone());
        }
        let mut faces = Vec::new();
        for (index, polygon) in solved.polygons.iter().enumerate() {
            let Some(polygon) = polygon else { continue };
            let source = self.faces.get(index)?;
            let count = polygon.verts.len();
            if count < 3 {
                return None;
            }
            // First vertex plus the two spreading the widest area keeps
            // the plane numerically stable after i32 rounding.
            let a = polygon.verts[0];
            let mut best = (1usize, 2usize, 0.0f64);
            for i in 1..count {
                for j in (i + 1)..count {
                    let u = [
                        polygon.verts[i][0] - a[0],
                        polygon.verts[i][1] - a[1],
                        polygon.verts[i][2] - a[2],
                    ];
                    let v = [
                        polygon.verts[j][0] - a[0],
                        polygon.verts[j][1] - a[1],
                        polygon.verts[j][2] - a[2],
                    ];
                    let cross = [
                        u[1] * v[2] - u[2] * v[1],
                        u[2] * v[0] - u[0] * v[2],
                        u[0] * v[1] - u[1] * v[0],
                    ];
                    let area =
                        (cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2]).sqrt();
                    if area > best.2 {
                        best = (i, j, area);
                    }
                }
            }
            let round = |p: [f64; 3]| {
                [
                    p[0].round() as i32,
                    p[1].round() as i32,
                    p[2].round() as i32,
                ]
            };
            let mut candidate = BrushFace {
                points: [
                    round(a),
                    round(polygon.verts[best.0]),
                    round(polygon.verts[best.1]),
                ],
                material: source.material,
                uv: source.uv,
            };
            // Keep the authored outward winding: the rebuilt plane must
            // face the same way as the source plane.
            let source_plane = Plane::from_points(source.points)?;
            let plane = Plane::from_points(candidate.points)?;
            let dot = (0..3)
                .map(|axis| source_plane.normal[axis] as f64 * plane.normal[axis] as f64)
                .sum::<f64>();
            if dot < 0.0 {
                candidate.points.swap(1, 2);
            }
            faces.push(candidate);
        }
        if faces.len() < 4 {
            return None;
        }
        let candidate = Brush {
            faces,
            contents: self.contents,
            mover: self.mover,
        };
        // Round-trip gate: the normalized brush must be the same solid.
        let resolved = candidate.solve();
        if !resolved.is_valid()
            || !resolved.within_extent(BRUSH_EDIT_EXTENT_LIMIT)
            || !candidate.is_pickable()
            || resolved.polygons.iter().flatten().count()
                != solved.polygons.iter().flatten().count()
        {
            return None;
        }
        for axis in 0..3 {
            if (resolved.min[axis] - solved.min[axis]).abs() > 1.0
                || (resolved.max[axis] - solved.max[axis]).abs() > 1.0
            {
                return None;
            }
        }
        Some(candidate)
    }

    /// Grow a NEW prism brush out of `face`: base on the face's plane,
    /// cap `distance` units along its outward normal, one side per
    /// polygon edge (TrenchBroom-style face extrude). The cap inherits
    /// the face's material and texture, offset-compensated so it looks
    /// identical to the source face; the sides take the material with a
    /// fresh mapping. `None` for non-positive distances or degenerate
    /// results.
    pub fn extruded_from_face(&self, face: usize, distance: i32) -> Option<Brush> {
        if distance <= 0 {
            return None;
        }
        let solved = self.solve();
        let polygon = solved.polygons.get(face)?.as_ref()?;
        if polygon.verts.len() < 3 {
            return None;
        }
        let source = self.faces.get(face)?;
        let plane = Plane::from_points(source.points)?;
        let length = ((plane.normal[0] as f64).powi(2)
            + (plane.normal[1] as f64).powi(2)
            + (plane.normal[2] as f64).powi(2))
        .sqrt();
        if length <= f64::EPSILON {
            return None;
        }
        let normal = [
            plane.normal[0] as f64 / length,
            plane.normal[1] as f64 / length,
            plane.normal[2] as f64 / length,
        ];
        let offset = [
            (normal[0] * f64::from(distance)).round() as i32,
            (normal[1] * f64::from(distance)).round() as i32,
            (normal[2] * f64::from(distance)).round() as i32,
        ];
        if offset == [0; 3] {
            return None;
        }
        let round = |p: [f64; 3]| {
            [
                p[0].round() as i32,
                p[1].round() as i32,
                p[2].round() as i32,
            ]
        };
        let base: Vec<[i32; 3]> = polygon.verts.iter().copied().map(round).collect();
        let cap: Vec<[i32; 3]> = base
            .iter()
            .map(|p| [p[0] + offset[0], p[1] + offset[1], p[2] + offset[2]])
            .collect();
        let count = base.len();
        let side_material = source.material;
        let build = |flip: bool| -> Brush {
            // `flip` mirrors EVERY winding: the solved polygon's vertex
            // order is orientation-dependent, so one of the two mirrored
            // prisms is the correctly wound solid.
            let wind = |a: [i32; 3], b: [i32; 3], c: [i32; 3]| {
                if flip {
                    [a, c, b]
                } else {
                    [a, b, c]
                }
            };
            let mut faces = Vec::with_capacity(count + 2);
            // Base: on the source plane, facing back toward the old brush.
            faces.push(BrushFace {
                points: wind(base[0], base[2], base[1]),
                material: side_material,
                uv: FaceUv::default(),
            });
            // Cap: the moved copy of the source face, same appearance.
            let mut cap_face = BrushFace {
                points: wind(cap[0], cap[1], cap[2]),
                material: source.material,
                uv: source.uv,
            };
            let raw = paraxial_uv(
                &plane,
                [
                    f64::from(offset[0]),
                    f64::from(offset[1]),
                    f64::from(offset[2]),
                ],
            );
            cap_face.uv.compensate_shift([
                raw[0] / BRUSH_UV_UNITS_PER_TEXEL,
                raw[1] / BRUSH_UV_UNITS_PER_TEXEL,
            ]);
            faces.push(cap_face);
            for edge in 0..count {
                let (a, b) = (base[edge], base[(edge + 1) % count]);
                let c = cap[(edge + 1) % count];
                faces.push(BrushFace {
                    points: wind(a, b, c),
                    material: side_material,
                    uv: FaceUv::default(),
                });
            }
            Brush {
                faces,
                contents: self.contents,
                mover: None,
            }
        };
        // Polygon winding orientation varies; try both side windings and
        // keep whichever encloses a sane solid.
        for flip in [false, true] {
            let candidate = build(flip);
            if candidate.is_pickable() {
                return Some(candidate);
            }
        }
        None
    }

    /// Whether this brush is a sane, clickable solid: it solves to a
    /// BOUNDED volume. A plane re-authored inside-out still yields a
    /// "valid" solve, but one clipped only by the base winding
    /// (coordinates at [`BASE_WINDING_EXTENT`]): the preview draws a
    /// partial shell while pick rays in the visible area miss, so every
    /// click passes through.
    pub fn is_pickable(&self) -> bool {
        let solved = self.solve();
        solved.is_valid() && solved.within_extent(BRUSH_EDIT_EXTENT_LIMIT)
    }

    /// [`Self::translate_points_near`], plus every authored point of the
    /// listed faces (see [`Self::transform_selected`] for why).
    ///
    /// With `uv_lock`, any face whose plane translated RIGIDLY (all three
    /// authored points moved) keeps its applied texture: the paraxial
    /// shift the move causes is compensated out of its UV offset, exactly
    /// like [`Self::translate_with_uv_lock`]. Faces that tilt or stretch
    /// stay world-anchored: their surface deforms, so no rigid mapping
    /// exists to preserve.
    pub fn translate_selected(
        &mut self,
        selected_faces: &[usize],
        targets: &[[f64; 3]],
        delta: [i32; 3],
        epsilon: f64,
        uv_lock: bool,
    ) -> usize {
        let delta_f = [
            f64::from(delta[0]),
            f64::from(delta[1]),
            f64::from(delta[2]),
        ];
        let mut moved = 0;
        for (face_index, face) in self.faces.iter_mut().enumerate() {
            let whole_face = selected_faces.contains(&face_index);
            let mut moved_in_face = 0;
            for point in &mut face.points {
                let hit = whole_face
                    || targets.iter().any(|target| {
                        (0..3).all(|axis| (f64::from(point[axis]) - target[axis]).abs() <= epsilon)
                    });
                if hit {
                    for axis in 0..3 {
                        point[axis] += delta[axis];
                    }
                    moved += 1;
                    moved_in_face += 1;
                }
            }
            if uv_lock && moved_in_face == 3 {
                if let Some(plane) = Plane::from_points(face.points) {
                    let raw = paraxial_uv(&plane, delta_f);
                    face.uv.compensate_shift([
                        raw[0] / BRUSH_UV_UNITS_PER_TEXEL,
                        raw[1] / BRUSH_UV_UNITS_PER_TEXEL,
                    ]);
                }
            }
        }
        moved
    }

    /// [`Self::translate_face`] with the texture riding along: the plane
    /// translates rigidly, so the paraxial shift is compensated out of
    /// the face's UV offset (extrude with texture lock).
    pub fn translate_face_with_uv_lock(&mut self, face: usize, delta: [i32; 3]) {
        self.translate_face(face, delta);
        if let Some(face) = self.faces.get_mut(face) {
            if let Some(plane) = Plane::from_points(face.points) {
                let raw = paraxial_uv(
                    &plane,
                    [
                        f64::from(delta[0]),
                        f64::from(delta[1]),
                        f64::from(delta[2]),
                    ],
                );
                face.uv.compensate_shift([
                    raw[0] / BRUSH_UV_UNITS_PER_TEXEL,
                    raw[1] / BRUSH_UV_UNITS_PER_TEXEL,
                ]);
            }
        }
    }
}

impl Brush {
    /// Hollow an axis-aligned brush into six wall slabs of `thickness`,
    /// keeping the outer bounds. Returns `None` when the brush is too
    /// small to hollow or has no volume. Every new face inherits material
    /// and UV defaults from the source face with the same signed axis.
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
        let mut slabs = vec![
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
        ];
        let source_faces = self
            .faces
            .iter()
            .filter_map(|face| {
                let plane = Plane::from_points(face.points)?;
                Some((signed_dominant_axis(plane.normal), face.material, face.uv))
            })
            .collect::<Vec<_>>();
        for slab in &mut slabs {
            slab.contents = self.contents;
            slab.mover = self.mover;
            // Every slab face has one of the source cuboid's six signed axis
            // directions. Carry both the material and UV transform by that
            // direction, including the newly exposed cavity faces. A Hollow
            // action on a textured brush therefore remains immediately
            // cookable instead of silently producing untextured interior
            // walls.
            for face in &mut slab.faces {
                let Some(plane) = Plane::from_points(face.points) else {
                    continue;
                };
                let key = signed_dominant_axis(plane.normal);
                if let Some((_, material, uv)) =
                    source_faces.iter().find(|(source, _, _)| *source == key)
                {
                    face.material = *material;
                    face.uv = *uv;
                }
            }
        }
        Some(slabs)
    }
}

fn signed_dominant_axis(normal: [i64; 3]) -> (usize, bool) {
    let mut axis = 0;
    for candidate in 1..3 {
        if normal[candidate].unsigned_abs() > normal[axis].unsigned_abs() {
            axis = candidate;
        }
    }
    (axis, normal[axis] >= 0)
}

/// World units per paraxial texel, the renderer's brush texel density
/// (matches the grid default: 64 texels across a 1024-unit sector).
pub const BRUSH_UV_UNITS_PER_TEXEL: f64 = 16.0;

/// Shift a face's applied texel UVs by whole texture repeats so the
/// whole set fits the GPU's u8 window without straddling a 256 wrap.
/// A straddling polygon packs per-vertex wrapped coordinates and
/// rasterizes a backwards texture gradient; power-of-two repeat sizes
/// make the shift sampling-identical. Axes whose span cannot fit the
/// window are left alone (the historic wrap behaviour).
pub fn rebase_texel_uvs(uvs: &mut [[f64; 2]], repeat: [f64; 2]) {
    for axis in 0..2 {
        let repeat = repeat[axis].max(1.0);
        let min = uvs.iter().map(|uv| uv[axis]).fold(f64::MAX, f64::min);
        let max = uvs.iter().map(|uv| uv[axis]).fold(f64::MIN, f64::max);
        if !min.is_finite() || !max.is_finite() {
            continue;
        }
        let shift = (min / repeat).floor() * repeat;
        if max - shift <= 255.0 {
            for uv in uvs.iter_mut() {
                uv[axis] -= shift;
            }
        }
    }
}

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
            .map(|v| {
                [
                    v[0].round() as i64,
                    v[1].round() as i64,
                    v[2].round() as i64,
                ]
            })
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
        brush.faces.push(BrushFace::from_points([
            [0, 200, 0],
            [0, 200, 64],
            [64, 200, 64],
        ]));
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

    fn contains_point(brush: &Brush, point: [f64; 3]) -> bool {
        brush.faces.iter().all(|face| {
            let plane = Plane::from_points(face.points).expect("face plane");
            let (normal, distance) = plane.normalized();
            normal[0] * point[0] + normal[1] * point[1] + normal[2] * point[2] - distance
                <= 1e-6
        })
    }

    #[test]
    fn subtract_carves_an_overlapping_corner() {
        let target = Brush::cuboid([0, 0, 0], [256, 256, 256]);
        let cutter = Brush::cuboid([128, 0, 128], [384, 256, 384]);
        let pieces = target.subtracted_by(&cutter).expect("solids intersect");
        assert!(!pieces.is_empty());
        for piece in &pieces {
            assert!(piece.is_pickable());
        }
        // The carved corner is gone from every piece; the far corner
        // survives in exactly one.
        let carved = [192.0, 128.0, 192.0];
        let kept = [64.0, 128.0, 64.0];
        assert!(pieces.iter().all(|piece| !contains_point(piece, carved)));
        assert_eq!(
            pieces
                .iter()
                .filter(|piece| contains_point(piece, kept))
                .count(),
            1
        );
    }

    #[test]
    fn subtract_misses_a_disjoint_brush() {
        let target = Brush::cuboid([0, 0, 0], [256, 256, 256]);
        let cutter = Brush::cuboid([512, 0, 512], [768, 256, 768]);
        assert!(target.subtracted_by(&cutter).is_none());
    }

    #[test]
    fn subtract_swallows_a_contained_brush() {
        let target = Brush::cuboid([64, 64, 64], [128, 128, 128]);
        let cutter = Brush::cuboid([0, 0, 0], [256, 256, 256]);
        let pieces = target.subtracted_by(&cutter).expect("contained");
        assert!(pieces.is_empty());
    }

    #[test]
    fn subtract_paints_new_faces_with_the_cutter_material() {
        let material = crate::ResourceId(77);
        let target = Brush::cuboid([0, 0, 0], [256, 256, 256]);
        let mut cutter = Brush::cuboid([128, 0, 0], [384, 256, 256]);
        for face in &mut cutter.faces {
            face.material = Some(material);
        }
        let pieces = target.subtracted_by(&cutter).expect("solids intersect");
        let cut_faces: Vec<_> = pieces
            .iter()
            .flat_map(|piece| piece.faces.iter())
            .filter(|face| face.material == Some(material))
            .collect();
        assert!(
            !cut_faces.is_empty(),
            "the reveal must take the cutter's material"
        );
    }

    #[test]
    fn hollow_leaves_walls_around_an_empty_interior() {
        let brush = Brush::cuboid([0, 0, 0], [512, 512, 512]);
        let walls = brush.hollowed(64).expect("hollow succeeds");
        assert!(walls.len() >= 6, "a box hollows into its six walls");
        let interior = [256.0, 256.0, 256.0];
        assert!(walls.iter().all(|wall| !contains_point(wall, interior)));
        let inside_wall = [32.0, 256.0, 256.0];
        assert_eq!(
            walls
                .iter()
                .filter(|wall| contains_point(wall, inside_wall))
                .count(),
            1
        );
        for wall in &walls {
            assert!(wall.is_pickable());
        }
    }

    #[test]
    fn hollow_refuses_a_brush_too_thin_for_the_shell() {
        let brush = Brush::cuboid([0, 0, 0], [256, 64, 256]);
        assert!(brush.hollowed(64).is_none());
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
        assert!(brush
            .raycast([-36.0, 32.0, 32.0], [-72.0, 0.0, 0.0])
            .is_none());
        // Parallel ray outside a slab misses.
        assert!(brush
            .raycast([-36.0, 200.0, 32.0], [72.0, 0.0, 0.0])
            .is_none());
        // Origin inside: entry is behind the origin -> no hit (t >= 0 rule).
        assert!(brush
            .raycast([32.0, 32.0, 32.0], [72.0, 0.0, 0.0])
            .is_none());
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
        assert!(Brush::cuboid([0, 0, 0], [30, 128, 256])
            .hollow(16)
            .is_none());
    }

    #[test]
    fn hollow_carries_material_and_uv_to_every_new_face() {
        let mut brush = Brush::cuboid([0, 0, 0], [256, 128, 256]);
        for (index, face) in brush.faces.iter_mut().enumerate() {
            face.material = Some(crate::ResourceId(index as u64 + 1));
            face.uv = FaceUv {
                offset_texels: [index as i16, -(index as i16)],
                rotation_deg: index as i16 * 15,
                scale_q8: [256 + index as i16, 512 - index as i16],
            };
        }
        let source = brush
            .faces
            .iter()
            .map(|face| {
                let plane = Plane::from_points(face.points).expect("source face plane");
                (signed_dominant_axis(plane.normal), face.material, face.uv)
            })
            .collect::<Vec<_>>();

        for slab in brush.hollow(16).expect("hollowable") {
            for face in slab.faces {
                let plane = Plane::from_points(face.points).expect("slab face plane");
                let key = signed_dominant_axis(plane.normal);
                let (_, material, uv) = source
                    .iter()
                    .find(|(candidate, _, _)| *candidate == key)
                    .expect("source signed axis");
                assert_eq!(face.material, *material);
                assert_eq!(face.uv, *uv);
            }
        }
    }

    #[test]
    fn hollow_preserves_liquid_contents_on_every_slab() {
        let mut brush = Brush::cuboid([0, 0, 0], [256, 128, 256]);
        brush.contents = BrushContents::Slime;
        let slabs = brush.hollow(16).expect("hollowable liquid");
        assert_eq!(slabs.len(), 6);
        assert!(slabs
            .iter()
            .all(|slab| slab.contents == BrushContents::Slime));
    }

    #[test]
    fn edge_drag_into_the_floor_makes_a_valid_but_unbounded_wedge() {
        // Regression for the editor crash: dragging the top-front edge
        // exactly onto the floor plane tilts the top plane parallel to a
        // side, leaving an INFINITE wedge that still passes `is_valid`
        // (4+ surviving faces) with solved coordinates at the base
        // winding extent. `within_extent` is the check that catches it.
        let mut brush = Brush::cuboid([0, 0, 0], [128, 128, 128]);
        let edge = [[0.0, 128.0, 0.0], [128.0, 128.0, 0.0]];
        brush.translate_points_near(&edge, [0, -128, 0], 0.5);
        let solved = brush.solve();
        assert!(solved.is_valid(), "the wedge still counts 4+ faces");
        assert!(
            !solved.within_extent(BRUSH_EDIT_EXTENT_LIMIT),
            "unbounded wedge must fail the extent check, bounds {:?}..{:?}",
            solved.min,
            solved.max
        );
        // A sane edit passes both.
        let mut sane = Brush::cuboid([0, 0, 0], [128, 128, 128]);
        sane.translate_points_near(&edge, [0, 64, 0], 0.5);
        let solved = sane.solve();
        assert!(solved.is_valid() && solved.within_extent(BRUSH_EDIT_EXTENT_LIMIT));
    }

    #[test]
    fn translate_points_near_retilts_only_anchored_planes() {
        // Drag the top +X/+Z corner column of a cube inward along X:
        // the two vertical faces anchored there tilt, the rest stay.
        let mut brush = Brush::cuboid([0, 0, 0], [128, 128, 128]);
        let column = [[128.0, 0.0, 128.0], [128.0, 128.0, 128.0]];
        let moved = brush.translate_points_near(&column, [-64, 0, 0], 0.5);
        // Authored points at that column: +Z holds two, +X / -Y / +Y one
        // each. Only +X actually tilts; the others slide the point within
        // their own (unchanged) plane, which is what keeps the drag exact.
        assert_eq!(moved, 5);
        let solved = brush.solve();
        assert!(solved.is_valid());
        // The dragged corners moved; the opposite corners stayed.
        let verts = unique_verts(&solved);
        assert!(verts.contains(&[64, 0, 128]));
        assert!(verts.contains(&[64, 128, 128]));
        assert!(verts.contains(&[0, 0, 0]));
        assert!(verts.contains(&[128, 0, 0]));

        // No target within epsilon: nothing moves.
        let mut untouched = Brush::cuboid([0, 0, 0], [64, 64, 64]);
        assert_eq!(
            untouched.translate_points_near(&[[500.0, 0.0, 0.0]], [8, 0, 0], 0.5),
            0
        );
        assert_eq!(untouched, Brush::cuboid([0, 0, 0], [64, 64, 64]));
    }

    #[test]
    fn mover_binding_survives_geometry_edits_and_ron_round_trip() {
        let mover = crate::NodeId(41);
        let mut brush = Brush::cuboid([0, 0, 0], [128, 128, 128]);
        brush.mover = Some(mover);
        let clipped = brush.clip([[64, 0, 0], [64, 128, 0], [64, 128, 128]]);
        assert_eq!(clipped.back.expect("back").mover, Some(mover));
        assert_eq!(clipped.front.expect("front").mover, Some(mover));
        assert!(brush
            .hollow(16)
            .expect("slabs")
            .iter()
            .all(|slab| slab.mover == Some(mover)));

        let encoded = ron::ser::to_string(&brush).expect("serialize brush");
        let decoded: Brush = ron::from_str(&encoded).expect("deserialize brush");
        assert_eq!(decoded.mover, Some(mover));
        let legacy: Brush = ron::from_str("(faces:[])").expect("legacy brush");
        assert_eq!(legacy.mover, None);
    }

    /// A face far from the world origin is where the old behaviour hurt:
    /// scale is applied about the projection's origin, so a few percent of
    /// scale slid the texture by hundreds of texels. Re-anchoring holds the
    /// face's own centroid still and changes only how densely it repeats.
    #[test]
    fn reanchoring_holds_the_anchor_while_the_uv_span_changes() {
        // Wall at x = 4096: paraxial U is Z, V is -Y.
        let brush = Brush::cuboid([4096, 0, 2048], [4160, 128, 2304]);
        let face = brush
            .faces
            .iter()
            .position(|face| {
                Plane::from_points(face.points)
                    .map(|plane| plane.normal[0] > 0)
                    .unwrap_or(false)
            })
            .expect("+X face");
        let anchor = brush.face_uv_anchor(face).expect("anchor");

        let before = FaceUv {
            offset_texels: [7, -3],
            rotation_deg: 0,
            scale_q8: [256, 256],
        };
        let held = before.apply(anchor);

        // U only: the anchor stays, the U span halves, V is untouched.
        let mut wider = FaceUv {
            scale_q8: [512, 256],
            ..before
        };
        let uncompensated = wider.apply(anchor);
        assert!(
            (uncompensated[0] - held[0]).abs() > 8.0,
            "the raw scale change has to slide this far-away face"
        );
        wider.reanchor(&before, anchor, [0.0, 0.0]);
        let moved = wider.apply(anchor);
        assert!(
            (moved[0] - held[0]).abs() <= 1.0 && (moved[1] - held[1]).abs() <= 1.0,
            "anchor must hold: {held:?} became {moved:?}"
        );
        let span_before = span(&before, anchor);
        let span_wider = span(&wider, anchor);
        assert!(
            span_wider[0] < span_before[0] * 0.6,
            "U% must change the U span: {span_before:?} -> {span_wider:?}"
        );
        assert!(
            (span_wider[1] - span_before[1]).abs() < 1e-9,
            "a U-only edit must leave the V span alone"
        );

        // V only, on a floor this time, to catch an axis swap.
        let floor = Brush::cuboid([2048, 512, 3072], [2304, 576, 3328]);
        let face = floor
            .faces
            .iter()
            .position(|face| {
                Plane::from_points(face.points)
                    .map(|plane| plane.normal[1] > 0)
                    .unwrap_or(false)
            })
            .expect("+Y face");
        let anchor = floor.face_uv_anchor(face).expect("anchor");
        let before = FaceUv::default();
        let held = before.apply(anchor);
        let mut taller = FaceUv {
            scale_q8: [256, 512],
            ..before
        };
        taller.reanchor(&before, anchor, [0.0, 0.0]);
        let moved = taller.apply(anchor);
        assert!(
            (moved[0] - held[0]).abs() <= 1.0 && (moved[1] - held[1]).abs() <= 1.0,
            "floor anchor must hold: {held:?} became {moved:?}"
        );
        let span_before = span(&before, anchor);
        let span_taller = span(&taller, anchor);
        assert!(
            (span_taller[0] - span_before[0]).abs() < 1e-9,
            "a V-only edit must leave the U span alone"
        );
        assert!(
            span_taller[1] < span_before[1] * 0.6,
            "V% must change the V span: {span_before:?} -> {span_taller:?}"
        );

        // Rotation holds the anchor too, and a deliberate slide still slides.
        let mut turned = FaceUv {
            rotation_deg: 37,
            ..before
        };
        turned.reanchor(&before, anchor, [0.0, 0.0]);
        let moved = turned.apply(anchor);
        assert!(
            (moved[0] - held[0]).abs() <= 1.0 && (moved[1] - held[1]).abs() <= 1.0,
            "rotation must hold the anchor: {held:?} became {moved:?}"
        );
        let mut slid = FaceUv {
            rotation_deg: 37,
            ..before
        };
        slid.reanchor(&before, anchor, [12.0, -5.0]);
        let slid_uv = slid.apply(anchor);
        assert!(
            (slid_uv[0] - (moved[0] + 12.0)).abs() <= 1.0
                && (slid_uv[1] - (moved[1] - 5.0)).abs() <= 1.0,
            "an offset edit in the same frame must still slide"
        );
    }

    /// How far apart two raw texels one unit either side of the anchor land
    /// under a mapping: the visible repetition, per axis.
    fn span(uv: &FaceUv, anchor: [f64; 2]) -> [f64; 2] {
        let u = uv.apply([anchor[0] + 1.0, anchor[1]]);
        let v = uv.apply([anchor[0], anchor[1] + 1.0]);
        let base = uv.apply(anchor);
        [
            ((u[0] - base[0]).powi(2) + (u[1] - base[1]).powi(2)).sqrt(),
            ((v[0] - base[0]).powi(2) + (v[1] - base[1]).powi(2)).sqrt(),
        ]
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

#[cfg(test)]
mod contents_tests {
    use super::*;

    #[test]
    fn solid_contents_remain_implicit_while_liquids_round_trip() {
        let solid = Brush::cuboid([0, 0, 0], [64, 64, 64]);
        let solid_ron = ron::to_string(&solid).expect("solid RON");
        assert!(!solid_ron.contains("contents"));
        assert_eq!(
            ron::from_str::<Brush>(&solid_ron).expect("solid round trip"),
            solid
        );

        for contents in [
            BrushContents::Water,
            BrushContents::Slime,
            BrushContents::Lava,
        ] {
            let mut liquid = solid.clone();
            liquid.contents = contents;
            let ron = ron::to_string(&liquid).expect("liquid RON");
            assert!(ron.contains(&format!("contents:{}", contents.label())));
            assert_eq!(
                ron::from_str::<Brush>(&ron).expect("liquid round trip"),
                liquid
            );
        }
    }
}
