//! Fast authoring-time detection of coincident brush faces.
//!
//! The audit intentionally considers only *same-facing* coplanar faces.
//! Opposite-facing polygons are the ordinary seam between two brushes that
//! merely touch, while same-facing overlap is the useful signal for a copied
//! brush (or face) accidentally left in place.

use crate::brush::{Brush, Plane};
use std::collections::BTreeMap;

const OVERLAP_AREA_EPSILON: f64 = 1.0 / 65_536.0;

/// Positive-area overlap between two solved faces on different brushes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BrushFaceOverlap {
    pub brush_a: usize,
    pub face_a: usize,
    pub brush_b: usize,
    pub face_b: usize,
    /// Surface area in authored world units squared.
    pub area: f64,
}

#[derive(Clone, Debug)]
struct FaceCandidate {
    brush: usize,
    face: usize,
    polygon: Vec<[f64; 2]>,
    bounds: ([f64; 2], [f64; 2]),
}

/// Find every same-facing coplanar face pair with positive shared area.
///
/// Faces are bucketed by an exact reduced integer plane before any pairwise
/// polygon work, so ordinary maps avoid an O(all_faces²) scan. Invalid and
/// redundant brush faces are ignored in the same way as the BSP cooker.
pub fn find_brush_face_overlaps(brushes: &[Brush]) -> Vec<BrushFaceOverlap> {
    let mut planes: BTreeMap<[i64; 4], Vec<FaceCandidate>> = BTreeMap::new();

    for (brush_index, brush) in brushes.iter().enumerate() {
        let solved = brush.solve();
        if !solved.is_valid() {
            continue;
        }
        for (face_index, (face, polygon)) in
            brush.faces.iter().zip(solved.polygons.iter()).enumerate()
        {
            let (Some(plane), Some(polygon)) = (Plane::from_points(face.points), polygon) else {
                continue;
            };
            let key = reduced_oriented_plane(plane);
            let drop_axis = dominant_axis([key[0], key[1], key[2]]);
            let polygon: Vec<_> = polygon
                .verts
                .iter()
                .copied()
                .map(|point| project(point, drop_axis))
                .collect();
            if polygon.len() < 3 || signed_area_twice(&polygon).abs() <= OVERLAP_AREA_EPSILON {
                continue;
            }
            let bounds = polygon_bounds(&polygon);
            planes.entry(key).or_default().push(FaceCandidate {
                brush: brush_index,
                face: face_index,
                polygon,
                bounds,
            });
        }
    }

    let mut overlaps = Vec::new();
    for (plane, faces) in planes {
        let normal = [plane[0] as f64, plane[1] as f64, plane[2] as f64];
        let drop_axis = dominant_axis([plane[0], plane[1], plane[2]]);
        let area_scale = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2])
            .sqrt()
            / normal[drop_axis].abs();
        for left_index in 0..faces.len() {
            let left = &faces[left_index];
            for right in &faces[left_index + 1..] {
                if left.brush == right.brush
                    || !bounds_have_positive_area(left.bounds, right.bounds)
                {
                    continue;
                }
                let projected_area = convex_intersection_area(&left.polygon, &right.polygon);
                let area = projected_area * area_scale;
                if area <= OVERLAP_AREA_EPSILON {
                    continue;
                }
                overlaps.push(BrushFaceOverlap {
                    brush_a: left.brush,
                    face_a: left.face,
                    brush_b: right.brush,
                    face_b: right.face,
                    area,
                });
            }
        }
    }
    overlaps.sort_by(|left, right| {
        (left.brush_a, left.brush_b, left.face_a, left.face_b).cmp(&(
            right.brush_a,
            right.brush_b,
            right.face_a,
            right.face_b,
        ))
    });
    overlaps
}

fn reduced_oriented_plane(plane: Plane) -> [i64; 4] {
    let values = [
        plane.normal[0],
        plane.normal[1],
        plane.normal[2],
        plane.dist,
    ];
    let divisor = values
        .iter()
        .copied()
        .map(i64::unsigned_abs)
        .fold(0_u64, gcd)
        .max(1);
    values.map(|value| (i128::from(value) / i128::from(divisor)) as i64)
}

fn gcd(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

fn dominant_axis(normal: [i64; 3]) -> usize {
    let absolute = normal.map(i64::unsigned_abs);
    if absolute[1] > absolute[0] && absolute[1] >= absolute[2] {
        1
    } else if absolute[2] > absolute[0] {
        2
    } else {
        0
    }
}

fn project(point: [f64; 3], drop_axis: usize) -> [f64; 2] {
    match drop_axis {
        0 => [point[1], point[2]],
        1 => [point[0], point[2]],
        _ => [point[0], point[1]],
    }
}

fn polygon_bounds(polygon: &[[f64; 2]]) -> ([f64; 2], [f64; 2]) {
    let mut min = [f64::INFINITY; 2];
    let mut max = [f64::NEG_INFINITY; 2];
    for point in polygon {
        for axis in 0..2 {
            min[axis] = min[axis].min(point[axis]);
            max[axis] = max[axis].max(point[axis]);
        }
    }
    (min, max)
}

fn bounds_have_positive_area(left: ([f64; 2], [f64; 2]), right: ([f64; 2], [f64; 2])) -> bool {
    (0..2).all(|axis| {
        left.1[axis].min(right.1[axis]) - left.0[axis].max(right.0[axis]) > OVERLAP_AREA_EPSILON
    })
}

fn signed_area_twice(polygon: &[[f64; 2]]) -> f64 {
    polygon
        .iter()
        .zip(polygon.iter().cycle().skip(1))
        .take(polygon.len())
        .map(|(left, right)| left[0] * right[1] - left[1] * right[0])
        .sum()
}

fn edge_side(edge_a: [f64; 2], edge_b: [f64; 2], point: [f64; 2]) -> f64 {
    (edge_b[0] - edge_a[0]) * (point[1] - edge_a[1])
        - (edge_b[1] - edge_a[1]) * (point[0] - edge_a[0])
}

fn convex_intersection_area(subject: &[[f64; 2]], clip: &[[f64; 2]]) -> f64 {
    let orientation = signed_area_twice(clip).signum();
    if orientation == 0.0 {
        return 0.0;
    }
    let mut output = subject.to_vec();
    for edge_index in 0..clip.len() {
        if output.is_empty() {
            break;
        }
        let edge_a = clip[edge_index];
        let edge_b = clip[(edge_index + 1) % clip.len()];
        let input = std::mem::take(&mut output);
        let mut previous = *input.last().expect("non-empty clipped polygon");
        let mut previous_side = edge_side(edge_a, edge_b, previous) * orientation;
        for current in input {
            let current_side = edge_side(edge_a, edge_b, current) * orientation;
            let previous_inside = previous_side >= -OVERLAP_AREA_EPSILON;
            let current_inside = current_side >= -OVERLAP_AREA_EPSILON;
            if previous_inside != current_inside {
                let denominator = previous_side - current_side;
                if denominator.abs() > f64::EPSILON {
                    let t = (previous_side / denominator).clamp(0.0, 1.0);
                    output.push([
                        previous[0] + (current[0] - previous[0]) * t,
                        previous[1] + (current[1] - previous[1]) * t,
                    ]);
                }
            }
            if current_inside {
                output.push(current);
            }
            previous = current;
            previous_side = current_side;
        }
    }
    signed_area_twice(&output).abs() * 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coincident_cuboids_report_all_six_same_facing_faces() {
        let brush = Brush::cuboid([0, 0, 0], [64, 96, 128]);
        let overlaps = find_brush_face_overlaps(&[brush.clone(), brush]);
        assert_eq!(overlaps.len(), 6);
        assert!(overlaps.iter().all(|overlap| overlap.area > 0.0));
    }

    #[test]
    fn ordinary_opposite_facing_brush_seam_is_not_reported() {
        let left = Brush::cuboid([0, 0, 0], [64, 64, 64]);
        let right = Brush::cuboid([64, 0, 0], [128, 64, 64]);
        assert!(find_brush_face_overlaps(&[left, right]).is_empty());
    }

    #[test]
    fn partial_same_facing_overlap_reports_real_surface_area() {
        let left = Brush::cuboid([0, 0, 0], [10, 10, 10]);
        let right = Brush::cuboid([5, 2, 2], [15, 10, 8]);
        let overlaps = find_brush_face_overlaps(&[left, right]);
        assert_eq!(overlaps.len(), 1, "{overlaps:?}");
        assert!((overlaps[0].area - 30.0).abs() < 1e-6, "{overlaps:?}");
    }

    #[test]
    fn coplanar_faces_that_only_touch_along_an_edge_are_not_reported() {
        let left = Brush::cuboid([0, 0, 0], [10, 10, 10]);
        let right = Brush::cuboid([10, 2, 2], [20, 10, 8]);
        assert!(find_brush_face_overlaps(&[left, right]).is_empty());
    }
}
