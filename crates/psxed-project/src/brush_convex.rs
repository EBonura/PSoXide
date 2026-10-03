//! Convex brushes from an explicit polyhedron: vertices plus polygon faces.
//!
//! This is the TrenchBroom MCP's `add_convex_brushes` contract, which is what
//! made sculpted rock, angled walls and doorway plugs possible there. The
//! checks mirror it: a closed two-manifold boundary, planar faces, every
//! vertex behind every plane, and planes that rebuild exactly the input
//! solid. The difference is exactness. Vertices are integers here, so planes
//! are exact `i64` and planarity and convexity are equality tests rather than
//! tolerances.

use std::collections::HashMap;

use crate::brush::{Brush, BrushContents, BrushFace, Plane, BRUSH_EDIT_EXTENT_LIMIT};

/// Most vertices or faces one brush may have, matching the TrenchBroom MCP.
pub const CONVEX_BRUSH_ELEMENT_LIMIT: usize = 128;

/// Solved vertices further than this from every input vertex mean the planes
/// describe a different solid than the one given.
const RECONSTRUCT_EPSILON: f64 = 1.0 / 16.0;

impl Brush {
    /// Build a brush from integer vertices and polygon faces (vertex indices).
    ///
    /// Faces may be wound either way; each is oriented outward against the
    /// vertex centroid, as TrenchBroom does. The output keeps the input face
    /// order, so per-face materials and UVs can be assigned by index.
    pub fn from_convex_polyhedron(
        vertices: &[[i32; 3]],
        faces: &[Vec<usize>],
    ) -> Result<Self, String> {
        if vertices.len() < 4 || vertices.len() > CONVEX_BRUSH_ELEMENT_LIMIT {
            return Err(format!(
                "a convex brush needs 4..={CONVEX_BRUSH_ELEMENT_LIMIT} vertices, got {}",
                vertices.len()
            ));
        }
        if faces.len() < 4 || faces.len() > CONVEX_BRUSH_ELEMENT_LIMIT {
            return Err(format!(
                "a convex brush needs 4..={CONVEX_BRUSH_ELEMENT_LIMIT} faces, got {}",
                faces.len()
            ));
        }
        if let Some(vertex) = vertices.iter().find(|vertex| {
            vertex
                .iter()
                .any(|value| f64::from(value.abs()) > BRUSH_EDIT_EXTENT_LIMIT)
        }) {
            return Err(format!(
                "vertex {vertex:?} is beyond the {BRUSH_EDIT_EXTENT_LIMIT} unit editing extent"
            ));
        }
        // Six times the centroid, so the orientation test stays in integers.
        let count = vertices.len() as i64;
        let centroid_sum = (0..3).fold([0i64; 3], |mut sum, axis| {
            sum[axis] = vertices.iter().map(|vertex| i64::from(vertex[axis])).sum();
            sum
        });

        let mut edges: HashMap<(usize, usize), usize> = HashMap::new();
        let mut built = Vec::with_capacity(faces.len());
        for (face_index, face) in faces.iter().enumerate() {
            if face.len() < 3 {
                return Err(format!(
                    "face {face_index} has {} vertices; it needs 3 or more",
                    face.len()
                ));
            }
            let mut seen = face.clone();
            seen.sort_unstable();
            seen.dedup();
            if seen.len() != face.len() {
                return Err(format!("face {face_index} repeats a vertex index"));
            }
            if let Some(bad) = face.iter().find(|index| **index >= vertices.len()) {
                return Err(format!(
                    "face {face_index} names vertex {bad}, but there are only {}",
                    vertices.len()
                ));
            }
            for position in 0..face.len() {
                let a = face[position];
                let b = face[(position + 1) % face.len()];
                *edges.entry((a.min(b), a.max(b))).or_default() += 1;
            }

            // First three vertices that are not collinear define the plane.
            let first = vertices[face[0]];
            let mut plane_points = None;
            'search: for second in 1..face.len() {
                for third in second + 1..face.len() {
                    let points = [first, vertices[face[second]], vertices[face[third]]];
                    if Plane::from_points(points).is_some() {
                        plane_points = Some(points);
                        break 'search;
                    }
                }
            }
            let Some(mut points) = plane_points else {
                return Err(format!(
                    "face {face_index} is degenerate: its vertices are collinear"
                ));
            };
            let mut plane = Plane::from_points(points).ok_or("unreachable: checked above")?;
            // Outward means the centroid is behind the plane.
            let centroid_side = plane.normal[0] * centroid_sum[0]
                + plane.normal[1] * centroid_sum[1]
                + plane.normal[2] * centroid_sum[2]
                - plane.dist * count;
            if centroid_side > 0 {
                points.swap(1, 2);
                plane = Plane::from_points(points).ok_or("unreachable: checked above")?;
            }
            if let Some(off) = face.iter().find(|index| plane.side(vertices[**index]) != 0) {
                return Err(format!(
                    "face {face_index} is not planar: vertex {off} {:?} is off the plane of its first points",
                    vertices[*off]
                ));
            }
            if let Some((index, vertex)) = vertices
                .iter()
                .enumerate()
                .find(|(_, vertex)| plane.side(**vertex) > 0)
            {
                return Err(format!(
                    "the brush is not convex: vertex {index} {vertex:?} is outside face {face_index}"
                ));
            }
            built.push(BrushFace::from_points(points));
        }
        if let Some(((a, b), uses)) = edges.iter().find(|(_, uses)| **uses != 2) {
            return Err(format!(
                "the faces do not close: edge {a}-{b} is used by {uses} face(s), and a closed \
                 solid uses every edge exactly twice"
            ));
        }

        let brush = Brush {
            faces: built,
            contents: BrushContents::Solid,
            mover: None,
            group: None,
        };
        let solved = brush.solve();
        if let Some(lost) = solved.polygons.iter().position(Option::is_none) {
            return Err(format!(
                "face {lost} vanishes when the planes are solved, so the faces do not describe \
                 this solid"
            ));
        }
        let near = |a: [f64; 3], b: [f64; 3]| {
            (0..3).all(|axis| (a[axis] - b[axis]).abs() <= RECONSTRUCT_EPSILON)
        };
        let input: Vec<[f64; 3]> = vertices
            .iter()
            .map(|vertex| vertex.map(f64::from))
            .collect();
        let solved_points: Vec<[f64; 3]> = solved
            .polygons
            .iter()
            .flatten()
            .flat_map(|polygon| polygon.verts.iter().copied())
            .collect();
        if let Some(stray) = solved_points
            .iter()
            .find(|point| !input.iter().any(|vertex| near(**point, *vertex)))
        {
            return Err(format!(
                "the planes rebuild a corner at {stray:?} that is not one of the vertices given, \
                 so the faces do not describe this solid"
            ));
        }
        if let Some(index) = input
            .iter()
            .position(|vertex| !solved_points.iter().any(|point| near(*point, *vertex)))
        {
            return Err(format!(
                "vertex {index} {:?} is not a corner of the solved brush; drop it or fix the faces",
                vertices[index]
            ));
        }
        Ok(brush)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CUBE: [[i32; 3]; 8] = [
        [0, 0, 0],
        [64, 0, 0],
        [64, 64, 0],
        [0, 64, 0],
        [0, 0, 64],
        [64, 0, 64],
        [64, 64, 64],
        [0, 64, 64],
    ];

    fn cube_faces() -> Vec<Vec<usize>> {
        vec![
            vec![0, 3, 2, 1],
            vec![4, 5, 6, 7],
            vec![0, 1, 5, 4],
            vec![3, 7, 6, 2],
            vec![0, 4, 7, 3],
            vec![1, 2, 6, 5],
        ]
    }

    fn bounds(brush: &Brush) -> ([f64; 3], [f64; 3]) {
        let solved = brush.solve();
        (solved.min, solved.max)
    }

    #[test]
    fn a_cube_matches_the_cuboid_in_either_winding() {
        let built = Brush::from_convex_polyhedron(&CUBE, &cube_faces()).expect("a cube is convex");
        assert_eq!(bounds(&built), bounds(&Brush::cuboid([0; 3], [64; 3])));
        // Reverse every face: TrenchBroom orients them outward itself.
        let reversed: Vec<Vec<usize>> = cube_faces()
            .into_iter()
            .map(|mut face| {
                face.reverse();
                face
            })
            .collect();
        let flipped = Brush::from_convex_polyhedron(&CUBE, &reversed).expect("winding is free");
        assert_eq!(bounds(&flipped), bounds(&built));
        for face in &flipped.faces {
            let plane = Plane::from_points(face.points).expect("valid plane");
            assert!(
                CUBE.iter().all(|vertex| plane.side(*vertex) <= 0),
                "{face:?} points inward"
            );
        }
    }

    #[test]
    fn an_off_axis_wedge_builds() {
        // A ramp-like wedge with a sloped face: the case box tools cannot make.
        let vertices = [
            [0, 0, 0],
            [256, 0, 0],
            [256, 0, 128],
            [0, 0, 128],
            [0, 96, 0],
            [256, 96, 0],
        ];
        let faces = vec![
            vec![0, 1, 2, 3],
            vec![0, 4, 5, 1],
            vec![3, 2, 5, 4],
            vec![0, 3, 4],
            vec![1, 5, 2],
        ];
        let wedge = Brush::from_convex_polyhedron(&vertices, &faces).expect("a wedge is convex");
        assert_eq!(wedge.faces.len(), 5);
        assert_eq!(bounds(&wedge), ([0.0, 0.0, 0.0], [256.0, 96.0, 128.0]));
    }

    #[test]
    fn broken_solids_are_refused_with_a_reason() {
        // Missing face: the boundary does not close.
        let mut open = cube_faces();
        open.pop();
        assert!(Brush::from_convex_polyhedron(&CUBE, &open)
            .unwrap_err()
            .contains("do not close"));

        // A dented vertex makes it concave.
        let mut dented = CUBE;
        dented[6] = [32, 32, 32];
        let error = Brush::from_convex_polyhedron(&dented, &cube_faces()).unwrap_err();
        assert!(
            error.contains("not planar") || error.contains("not convex"),
            "{error}"
        );

        // A warped quad.
        let mut warped = CUBE;
        warped[7] = [0, 64, 80];
        let error = Brush::from_convex_polyhedron(&warped, &cube_faces()).unwrap_err();
        assert!(
            error.contains("not planar") || error.contains("not convex"),
            "{error}"
        );

        // Bad indices.
        let mut bad = cube_faces();
        bad[0][0] = 99;
        assert!(Brush::from_convex_polyhedron(&CUBE, &bad)
            .unwrap_err()
            .contains("names vertex 99"));
        let mut repeated = cube_faces();
        repeated[0] = vec![0, 0, 2, 1];
        assert!(Brush::from_convex_polyhedron(&CUBE, &repeated)
            .unwrap_err()
            .contains("repeats"));

        // An unused interior vertex is not a corner of the result.
        let mut extra = CUBE.to_vec();
        extra.push([32, 32, 32]);
        assert!(Brush::from_convex_polyhedron(&extra, &cube_faces())
            .unwrap_err()
            .contains("not a corner"));
    }
}
