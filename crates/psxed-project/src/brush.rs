//! Convex-brush geometry kernel (docs/brush-editor-integration.md).
//!
//! A brush is a convex solid described as the intersection of half-spaces.
//! Each face stores three integer points that define its plane, following
//! the Quake `.map` convention: wound counter-clockwise when viewed from
//! outside, so the derived normal points out of the brush. Plane predicates
//! are exact (wide-integer cross/dot on the authored points); face polygons
//! are solved in f64 (three integer planes meet at rational points) and
//! snapped by tools, not by the kernel.

/// One brush face: three integer points defining the plane. Material and
/// UV state attach at scene integration (NodeKind::Brush), not here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrushFace {
    /// Plane points, counter-clockwise viewed from outside the brush.
    pub points: [[i32; 3]; 3],
}

/// A convex brush as an unordered set of faces.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
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
            faces: faces.map(|points| BrushFace { points }).into(),
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
        brush.faces[5] = BrushFace {
            points: [[0, 128, 0], [0, 128, 128], [128, 0, 128]],
        };
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
        brush.faces.push(BrushFace {
            points: [[0, 200, 0], [0, 200, 64], [64, 200, 64]],
        });
        let solved = brush.solve();
        assert!(solved.is_valid());
        assert!(solved.polygons[6].is_none());
        assert_eq!(unique_verts(&solved).len(), 8);
    }

    #[test]
    fn collinear_points_are_degenerate() {
        assert!(Plane::from_points([[0, 0, 0], [1, 1, 1], [2, 2, 2]]).is_none());
        let mut brush = Brush::cuboid([0, 0, 0], [64, 64, 64]);
        brush.faces[0] = BrushFace {
            points: [[0, 0, 0], [1, 1, 1], [2, 2, 2]],
        };
        let solved = brush.solve();
        assert!(solved.polygons[0].is_none());
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
