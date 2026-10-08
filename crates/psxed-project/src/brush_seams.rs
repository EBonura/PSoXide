//! Watertightness audit for packed world polygons.
//!
//! The packer rounds every polygon vertex to an integer world unit and the
//! GTE then projects each vertex independently, so two polygons only meet
//! pixel-exactly where they share the same integer vertices. A vertex of one
//! polygon sitting on the interior of another polygon's edge (a T-junction)
//! projects to a different screen point than the edge it touches, and the
//! gap between them is a crack.

use std::collections::HashMap;

/// One vertex lying on the interior of a foreign polygon edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TJunction {
    /// Polygon owning the edge that is missing the vertex.
    pub polygon: usize,
    /// Index of the edge's first corner within `polygon`.
    pub edge: usize,
    /// Polygon whose corner touches the edge.
    pub touching_polygon: usize,
    /// Corner index within `touching_polygon`.
    pub touching_corner: usize,
    /// Integer position of the touching corner.
    pub position: [i32; 3],
}

const BUCKET: i32 = 128;

fn bucket(position: [i32; 3]) -> [i32; 3] {
    position.map(|value| value.div_euclid(BUCKET))
}

fn sub(a: [i32; 3], b: [i32; 3]) -> [i64; 3] {
    [0, 1, 2].map(|axis| i64::from(a[axis]) - i64::from(b[axis]))
}

fn dot(a: [i64; 3], b: [i64; 3]) -> i64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross_len_sq(a: [i64; 3], b: [i64; 3]) -> i64 {
    let c = [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ];
    dot(c, c)
}

/// Whether `point` lies strictly inside the segment `a`-`b`, within
/// `tolerance` world units of the line.
pub fn point_on_edge_interior(
    point: [i32; 3],
    a: [i32; 3],
    b: [i32; 3],
    tolerance: i64,
) -> bool {
    if point == a || point == b {
        return false;
    }
    let edge = sub(b, a);
    let length_sq = dot(edge, edge);
    if length_sq == 0 {
        return false;
    }
    let along = dot(sub(point, a), edge);
    if along <= 0 || along >= length_sq {
        return false;
    }
    // distance^2 = |cross|^2 / |edge|^2
    cross_len_sq(sub(point, a), edge) <= tolerance * tolerance * length_sq
}

/// Every corner that touches a foreign edge interior without being one of
/// that edge's corners. `tolerance` is in world units.
pub fn find_t_junctions(polygons: &[Vec<[i32; 3]>], tolerance: i64) -> Vec<TJunction> {
    let mut grid: HashMap<[i32; 3], Vec<(usize, usize, [i32; 3])>> = HashMap::new();
    for (index, polygon) in polygons.iter().enumerate() {
        for (corner_index, &corner) in polygon.iter().enumerate() {
            grid.entry(bucket(corner))
                .or_default()
                .push((index, corner_index, corner));
        }
    }
    let mut found = Vec::new();
    for (index, polygon) in polygons.iter().enumerate() {
        for edge in 0..polygon.len() {
            let a = polygon[edge];
            let b = polygon[(edge + 1) % polygon.len()];
            let lo = bucket([0, 1, 2].map(|k| a[k].min(b[k]) - tolerance as i32));
            let hi = bucket([0, 1, 2].map(|k| a[k].max(b[k]) + tolerance as i32));
            for bx in lo[0]..=hi[0] {
                for by in lo[1]..=hi[1] {
                    for bz in lo[2]..=hi[2] {
                        let Some(corners) = grid.get(&[bx, by, bz]) else {
                            continue;
                        };
                        for &(other, corner_index, corner) in corners {
                            if other != index && point_on_edge_interior(corner, a, b, tolerance) {
                                found.push(TJunction {
                                    polygon: index,
                                    edge,
                                    touching_polygon: other,
                                    touching_corner: corner_index,
                                    position: corner,
                                });
                            }
                        }
                    }
                }
            }
        }
    }
    found.sort_by_key(|t| (t.polygon, t.edge, t.position, t.touching_polygon));
    found.dedup();
    found
}

/// Two vertices closer than this on every axis are one point that two CSG
/// or split computations reached with different floating-point noise.
pub const WELD_UNITS: f64 = 1.0 / 1024.0;

/// Snap vertices that are the same point up to floating-point noise onto one
/// canonical position. The packer rounds each vertex on its own, so a point
/// sitting on a half-unit (a midpoint of two odd heights) can otherwise land
/// on different whole units in two polygons that share it.
pub fn weld_vertices(polygons: &mut [Vec<[f64; 3]>], skip: impl Fn(usize) -> bool) -> usize {
    let cell = WELD_UNITS * 4.0;
    let key = |v: [f64; 3]| v.map(|value| (value / cell).floor() as i64);
    let mut canonical: HashMap<[i64; 3], Vec<[f64; 3]>> = HashMap::new();
    let mut moved = 0;
    for (index, polygon) in polygons.iter_mut().enumerate() {
        if skip(index) {
            continue;
        }
        for vertex in polygon.iter_mut() {
            let k = key(*vertex);
            let mut found = None;
            'search: for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        let Some(points) = canonical.get(&[k[0] + dx, k[1] + dy, k[2] + dz]) else {
                            continue;
                        };
                        for &point in points {
                            if (0..3).all(|a| (point[a] - vertex[a]).abs() <= WELD_UNITS) {
                                found = Some(point);
                                break 'search;
                            }
                        }
                    }
                }
            }
            match found {
                Some(point) => {
                    if point != *vertex {
                        moved += 1;
                        *vertex = point;
                    }
                }
                None => canonical.entry(k).or_default().push(*vertex),
            }
        }
    }
    moved
}

/// Growth caused by [`conform_t_junctions`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConformStats {
    /// Polygons that gained at least one vertex.
    pub polygons_changed: usize,
    /// Vertices inserted across all polygons.
    pub vertices_added: usize,
}

/// Distance in world units within which a corner counts as touching an
/// edge. The packer rounds every vertex to a whole unit, so two points that
/// coincide before rounding can sit up to about 0.87 units off the rounded
/// edge between them.
pub const CONFORM_TOLERANCE_UNITS: i64 = 1;

fn round_corner(vertex: [f64; 3]) -> [i32; 3] {
    vertex.map(|value| value.round() as i32)
}

/// Insert every T-junction corner into the polygon edge it touches, so that
/// the packed (integer) polygons meet only at shared vertices.
///
/// `polygons` are world-space vertex loops; the packer rounds each vertex to
/// a whole unit, so edges are matched in that rounded space. A polygon only
/// ever gains vertices that are existing corners of another polygon, so one
/// pass reaches a fixed point. Polygons for which `skip` is true neither
/// receive nor provide vertices (sky apertures are never drawn).
pub fn conform_t_junctions(
    polygons: &mut [Vec<[f64; 3]>],
    skip: impl Fn(usize) -> bool,
) -> ConformStats {
    let active: Vec<usize> = (0..polygons.len()).filter(|&i| !skip(i)).collect();
    let rounded: Vec<Vec<[i32; 3]>> = active
        .iter()
        .map(|&i| polygons[i].iter().copied().map(round_corner).collect())
        .collect();
    let junctions = find_t_junctions(&rounded, CONFORM_TOLERANCE_UNITS);
    let mut stats = ConformStats::default();
    let mut cursor = 0;
    while cursor < junctions.len() {
        let polygon = junctions[cursor].polygon;
        let end = junctions[cursor..]
            .iter()
            .position(|t| t.polygon != polygon)
            .map_or(junctions.len(), |n| cursor + n);
        let group = &junctions[cursor..end];
        cursor = end;
        let source = &rounded[polygon];
        let count = source.len();
        let mut rebuilt = Vec::with_capacity(count + group.len());
        for edge in 0..count {
            rebuilt.push(polygons[active[polygon]][edge]);
            let a = source[edge];
            let b = source[(edge + 1) % count];
            let direction = sub(b, a);
            let mut inserts: Vec<(i64, [i32; 3], [f64; 3])> = group
                .iter()
                .filter(|t| t.edge == edge)
                .map(|t| {
                    (
                        dot(sub(t.position, a), direction),
                        t.position,
                        polygons[active[t.touching_polygon]][t.touching_corner],
                    )
                })
                .collect();
            inserts.sort_by_key(|&(along, position, _)| (along, position));
            inserts.dedup_by_key(|&mut (_, position, _)| position);
            for (_, _, vertex) in inserts {
                rebuilt.push(vertex);
                stats.vertices_added += 1;
            }
        }
        polygons[active[polygon]] = rebuilt;
        stats.polygons_changed += 1;
    }
    stats
}

/// Weld, then conform, the final render polygons of one model.
pub fn make_watertight(
    polygons: &mut [Vec<[f64; 3]>],
    skip: impl Fn(usize) -> bool + Copy,
) -> ConformStats {
    weld_vertices(polygons, skip);
    conform_t_junctions(polygons, skip)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad(x0: f64, x1: f64, z0: f64, z1: f64) -> Vec<[f64; 3]> {
        vec![[x0, 0.0, z0], [x1, 0.0, z0], [x1, 0.0, z1], [x0, 0.0, z1]]
    }

    fn rounded(polygons: &[Vec<[f64; 3]>]) -> Vec<Vec<[i32; 3]>> {
        polygons
            .iter()
            .map(|p| p.iter().copied().map(round_corner).collect())
            .collect()
    }

    #[test]
    fn a_long_edge_gains_the_corners_of_two_short_neighbours() {
        let mut polygons = vec![
            quad(0.0, 100.0, 0.0, 50.0),
            quad(0.0, 30.0, 50.0, 90.0),
            quad(30.0, 100.0, 50.0, 90.0),
        ];
        // Both neighbours own a corner at x=30, so two records name one point.
        assert_eq!(find_t_junctions(&rounded(&polygons), 1).len(), 2);
        let stats = conform_t_junctions(&mut polygons, |_| false);
        assert_eq!(stats.vertices_added, 1);
        assert_eq!(stats.polygons_changed, 1);
        assert_eq!(polygons[0].len(), 5);
        assert!(find_t_junctions(&rounded(&polygons), 1).is_empty());
        // The new corner sits between the edge's own corners, in order.
        assert_eq!(polygons[0][2], [30.0, 0.0, 50.0]);
    }

    #[test]
    fn several_corners_on_one_edge_insert_in_edge_order() {
        let mut polygons = vec![
            quad(0.0, 100.0, 0.0, 10.0),
            quad(10.0, 20.0, 10.0, 20.0),
            quad(60.0, 70.0, 10.0, 20.0),
        ];
        conform_t_junctions(&mut polygons, |_| false);
        assert!(find_t_junctions(&rounded(&polygons), 1).is_empty());
        let xs: Vec<f64> = polygons[0].iter().map(|v| v[0]).collect();
        assert_eq!(xs, vec![0.0, 100.0, 100.0, 70.0, 60.0, 20.0, 10.0, 0.0]);
    }

    #[test]
    fn a_vertex_off_by_rounding_still_conforms() {
        // Both corners round to x=30 on the long edge's line within a unit.
        let mut polygons = vec![
            vec![[0.0, 0.0, 0.0], [100.0, 0.0, 33.0], [100.0, 0.0, 60.0]],
            vec![[50.0, 0.0, 16.5], [50.0, 0.0, 80.0], [20.0, 0.0, 80.0]],
        ];
        let stats = conform_t_junctions(&mut polygons, |_| false);
        assert_eq!(stats.vertices_added, 1);
        assert!(find_t_junctions(&rounded(&polygons), 1).is_empty());
    }

    #[test]
    fn a_half_unit_point_rounds_identically_after_welding() {
        // The same midpoint reached with opposite noise around x.5.
        let mut polygons = vec![
            vec![[0.0, 122.499_999_9, 0.0], [10.0, 0.0, 0.0], [0.0, 0.0, 10.0]],
            vec![[0.0, 122.500_000_1, 0.0], [0.0, 0.0, 10.0], [-10.0, 0.0, 0.0]],
        ];
        assert_ne!(
            round_corner(polygons[0][0]),
            round_corner(polygons[1][0]),
            "the fixture must straddle the rounding boundary"
        );
        weld_vertices(&mut polygons, |_| false);
        assert_eq!(round_corner(polygons[0][0]), round_corner(polygons[1][0]));
    }

    #[test]
    fn skipped_polygons_neither_give_nor_take() {
        let mut polygons = vec![quad(0.0, 100.0, 0.0, 50.0), quad(30.0, 60.0, 50.0, 90.0)];
        let stats = conform_t_junctions(&mut polygons, |i| i == 1);
        assert_eq!(stats, ConformStats::default());
        assert_eq!(polygons[0].len(), 4);
    }

    #[test]
    fn already_watertight_input_is_untouched() {
        let mut polygons = vec![quad(0.0, 50.0, 0.0, 50.0), quad(50.0, 100.0, 0.0, 50.0)];
        let before = polygons.clone();
        assert_eq!(
            conform_t_junctions(&mut polygons, |_| false),
            ConformStats::default()
        );
        assert_eq!(polygons, before);
    }
}
