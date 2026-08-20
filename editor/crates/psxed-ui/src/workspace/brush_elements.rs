//! Shared enumeration and identity for brush sub-elements (vertices,
//! edges, faces).
//!
//! Every pick and overlay path must consume these enumerators so a vertex
//! addressed in one view matches the set enumerated in another; the
//! previously inlined enumerations disagreed on dedup rules (the 3D edge
//! pick enumerated shared edges twice with swapped endpoints). Quantized
//! keys give sub-elements a stable identity across frames: solved corners
//! are re-derived every frame, so selection state stores keys and
//! re-resolves them against these enumerators.

use psxed_project::brush::{Brush, Plane, SolvedBrush};

/// Dedup epsilon in world units, matching `Brush::translate_points_near`.
pub(crate) const ELEMENT_EPSILON: f64 = 0.5;

/// Stable identity for a solved corner: nearest-integer world position.
/// Tool-authored brushes solve to integer corners, so this matches the
/// 0.5-eps convention the drag machinery mutates through.
pub(crate) fn quantize_element_point(point: [f64; 3]) -> [i64; 3] {
    point.map(|value| value.round() as i64)
}

/// Canonical edge identity: quantized endpoints in lexicographic order,
/// so the same edge produces the same key regardless of which adjacent
/// polygon (and winding) enumerated it.
pub(crate) fn edge_element_key(a: [f64; 3], b: [f64; 3]) -> ([i64; 3], [i64; 3]) {
    let qa = quantize_element_point(a);
    let qb = quantize_element_point(b);
    if qa <= qb {
        (qa, qb)
    } else {
        (qb, qa)
    }
}

/// Unique solved corners of a brush (0.5-eps dedup, enumeration order).
pub(crate) fn unique_vertices(solved: &SolvedBrush) -> Vec<[f64; 3]> {
    let mut vertices: Vec<[f64; 3]> = Vec::new();
    for vertex in solved
        .polygons
        .iter()
        .flatten()
        .flat_map(|polygon| polygon.verts.iter().copied())
    {
        if vertices
            .iter()
            .any(|seen| (0..3).all(|axis| (seen[axis] - vertex[axis]).abs() <= ELEMENT_EPSILON))
        {
            continue;
        }
        vertices.push(vertex);
    }
    vertices
}

/// Unique solved edges with canonically ordered endpoints; each shared
/// edge appears exactly once.
pub(crate) fn unique_edges(solved: &SolvedBrush) -> Vec<([f64; 3], [f64; 3])> {
    let mut keys: Vec<([i64; 3], [i64; 3])> = Vec::new();
    let mut edges = Vec::new();
    for polygon in solved.polygons.iter().flatten() {
        let count = polygon.verts.len();
        for edge in 0..count {
            let a = polygon.verts[edge];
            let b = polygon.verts[(edge + 1) % count];
            let key = edge_element_key(a, b);
            if keys.contains(&key) {
                continue;
            }
            keys.push(key);
            // Store endpoints in key order so downstream consumers see one
            // canonical representation.
            let qa = quantize_element_point(a);
            if (qa, quantize_element_point(b)) == key {
                edges.push((a, b));
            } else {
                edges.push((b, a));
            }
        }
    }
    edges
}

/// Face handle anchors: `(face index, solved-polygon centroid, unit
/// normal)`. Solves are the caller's: pass the brush's already-solved
/// polygons so enumeration stays one solve per frame.
pub(crate) fn face_handles(
    brush: &Brush,
    solved: &SolvedBrush,
) -> Vec<(usize, [f64; 3], [f64; 3])> {
    let mut out = Vec::new();
    for (face, polygon) in solved.polygons.iter().enumerate() {
        let Some(polygon) = polygon else { continue };
        let count = polygon.verts.len() as f64;
        if count <= 0.0 {
            continue;
        }
        let mut center = [0.0; 3];
        for vertex in &polygon.verts {
            for axis in 0..3 {
                center[axis] += vertex[axis] / count;
            }
        }
        let Some(points) = brush.faces.get(face).map(|face| face.points) else {
            continue;
        };
        let Some(plane) = Plane::from_points(points) else {
            continue;
        };
        let length = ((plane.normal[0] as f64).powi(2)
            + (plane.normal[1] as f64).powi(2)
            + (plane.normal[2] as f64).powi(2))
        .sqrt();
        if length <= f64::EPSILON {
            continue;
        }
        out.push((
            face,
            center,
            [
                plane.normal[0] as f64 / length,
                plane.normal[1] as f64 / length,
                plane.normal[2] as f64 / length,
            ],
        ));
    }
    out
}
