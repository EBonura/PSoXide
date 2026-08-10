//! Multi-hull brush collision cooking with box-expansion bevels.

use crate::brush::{Brush, Plane};
use crate::brush_compile::{pack_normalized_plane, pack_plane};

const CONTENTS_EMPTY: i16 = -1;
const CONTENTS_SOLID: i16 = -2;
const HULL_EPSILON: f64 = 1.0 / 1024.0;
pub const MAX_MAP_HULLS: usize = 4;

/// Local bounds of the traced body relative to its world-space origin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CollisionHullBounds {
    pub mins: [i32; 3],
    pub maxs: [i32; 3],
}

impl CollisionHullBounds {
    pub const POINT: Self = Self {
        mins: [0; 3],
        maxs: [0; 3],
    };
}

/// Shared plane and clipnode records for every requested collision hull.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompiledCollisionHulls {
    pub planes: Vec<u8>,
    pub clipnodes: Vec<u8>,
    pub head_nodes: Vec<i16>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CollisionHullCompileError {
    InvalidBounds(usize),
    InvalidPlane,
    LimitExceeded {
        kind: &'static str,
        count: usize,
        max: usize,
    },
}

/// Compile point and box-expanded collision trees into one record table.
pub fn compile_collision_hulls(
    brushes: &[Brush],
    hulls: &[CollisionHullBounds],
) -> Result<CompiledCollisionHulls, CollisionHullCompileError> {
    limit("collision hulls", hulls.len(), MAX_MAP_HULLS)?;
    let solved: Vec<_> = brushes.iter().map(Brush::solve).collect();
    let mut plane_records = Vec::new();
    let mut nodes: Vec<[i16; 3]> = Vec::new();
    let mut head_nodes = Vec::with_capacity(hulls.len());

    for (hull_index, hull) in hulls.iter().copied().enumerate() {
        if (0..3).any(|axis| hull.mins[axis] > hull.maxs[axis]) {
            return Err(CollisionHullCompileError::InvalidBounds(hull_index));
        }
        let mut escape = CONTENTS_EMPTY;
        for (brush, solved) in brushes.iter().zip(&solved).rev() {
            if !solved.is_valid() {
                continue;
            }
            let planes = if hull == CollisionHullBounds::POINT {
                point_hull_planes(brush)?
            } else {
                expanded_hull_planes(brush, solved, hull)?
            };
            if planes.is_empty() {
                continue;
            }

            let mut face_planes = Vec::with_capacity(planes.len());
            for (record, flipped) in planes {
                let plane = intern_plane(&mut plane_records, record)?;
                face_planes.push((plane, flipped));
            }
            let mut inside = CONTENTS_SOLID;
            for &(plane, flipped) in face_planes.iter().rev() {
                limit("clipnodes", nodes.len() + 1, i16::MAX as usize + 1)?;
                let (front, back) = if flipped {
                    (inside, escape)
                } else {
                    (escape, inside)
                };
                nodes.push([plane, front, back]);
                inside = (nodes.len() - 1) as i16;
            }
            escape = inside;
        }
        head_nodes.push(escape);
    }

    let mut clipnodes = Vec::with_capacity(nodes.len() * 6);
    for node in nodes {
        for value in node {
            clipnodes.extend_from_slice(&value.to_le_bytes());
        }
    }
    Ok(CompiledCollisionHulls {
        planes: plane_records.into_iter().flatten().collect(),
        clipnodes,
        head_nodes,
    })
}

fn point_hull_planes(brush: &Brush) -> Result<Vec<([u8; 14], bool)>, CollisionHullCompileError> {
    brush
        .faces
        .iter()
        .filter_map(|face| Plane::from_points(face.points))
        .map(|plane| pack_plane(&plane).ok_or(CollisionHullCompileError::InvalidPlane))
        .collect()
}

fn expanded_hull_planes(
    brush: &Brush,
    solved: &crate::brush::SolvedBrush,
    hull: CollisionHullBounds,
) -> Result<Vec<([u8; 14], bool)>, CollisionHullCompileError> {
    let brush_vertices = unique_brush_vertices(solved);
    let expanded_points = expanded_points(&brush_vertices, hull);
    let mut planes = Vec::new();

    for (face, polygon) in brush.faces.iter().zip(&solved.polygons) {
        if polygon.is_none() {
            continue;
        }
        let plane =
            Plane::from_points(face.points).ok_or(CollisionHullCompileError::InvalidPlane)?;
        add_support_plane(
            &mut planes,
            plane.normal.map(|value| value as f64),
            &expanded_points,
        );
    }
    for axis in 0..3 {
        let mut normal = [0.0; 3];
        normal[axis] = 1.0;
        add_support_plane(&mut planes, normal, &expanded_points);
        normal[axis] = -1.0;
        add_support_plane(&mut planes, normal, &expanded_points);
    }
    for edge in unique_edge_directions(solved) {
        for axis in 0..3 {
            let mut axial = [0.0; 3];
            axial[axis] = 1.0;
            let bevel = cross(edge, axial);
            add_support_plane(&mut planes, bevel, &expanded_points);
            add_support_plane(&mut planes, negate(bevel), &expanded_points);
        }
    }

    planes
        .into_iter()
        .map(|plane| {
            pack_normalized_plane(plane.normal, plane.distance)
                .ok_or(CollisionHullCompileError::InvalidPlane)
        })
        .collect()
}

#[derive(Clone, Copy)]
struct SupportPlane {
    normal: [f64; 3],
    distance: f64,
}

fn add_support_plane(planes: &mut Vec<SupportPlane>, normal: [f64; 3], points: &[[f64; 3]]) {
    let length_squared = dot(normal, normal);
    if length_squared <= HULL_EPSILON * HULL_EPSILON {
        return;
    }
    let normal = scale(normal, length_squared.sqrt().recip());
    let distance = points
        .iter()
        .map(|point| dot(normal, *point))
        .fold(f64::NEG_INFINITY, f64::max);
    let supporting: Vec<_> = points
        .iter()
        .copied()
        .filter(|point| (dot(normal, *point) - distance).abs() <= HULL_EPSILON)
        .collect();
    if !contains_face(&supporting) {
        return;
    }
    if planes.iter().any(|plane| {
        dot(plane.normal, normal) > 1.0 - 1e-9 && (plane.distance - distance).abs() <= HULL_EPSILON
    }) {
        return;
    }
    planes.push(SupportPlane { normal, distance });
}

fn contains_face(points: &[[f64; 3]]) -> bool {
    let Some((&first, tail)) = points.split_first() else {
        return false;
    };
    let Some(second) = tail
        .iter()
        .copied()
        .find(|point| squared_distance(first, *point) > HULL_EPSILON * HULL_EPSILON)
    else {
        return false;
    };
    let edge = subtract(second, first);
    tail.iter().copied().any(|point| {
        let other = subtract(point, first);
        dot(cross(edge, other), cross(edge, other)) > HULL_EPSILON * HULL_EPSILON
    })
}

fn unique_brush_vertices(solved: &crate::brush::SolvedBrush) -> Vec<[f64; 3]> {
    let mut vertices = Vec::new();
    for vertex in solved
        .polygons
        .iter()
        .flatten()
        .flat_map(|polygon| polygon.verts.iter().copied())
    {
        if !vertices
            .iter()
            .any(|existing| squared_distance(*existing, vertex) <= HULL_EPSILON * HULL_EPSILON)
        {
            vertices.push(vertex);
        }
    }
    vertices
}

fn expanded_points(brush_vertices: &[[f64; 3]], hull: CollisionHullBounds) -> Vec<[f64; 3]> {
    let mut points = Vec::with_capacity(brush_vertices.len() * 8);
    for &vertex in brush_vertices {
        for corner in 0..8 {
            let local = [
                if corner & 1 == 0 {
                    hull.mins[0]
                } else {
                    hull.maxs[0]
                },
                if corner & 2 == 0 {
                    hull.mins[1]
                } else {
                    hull.maxs[1]
                },
                if corner & 4 == 0 {
                    hull.mins[2]
                } else {
                    hull.maxs[2]
                },
            ];
            points.push([
                vertex[0] - local[0] as f64,
                vertex[1] - local[1] as f64,
                vertex[2] - local[2] as f64,
            ]);
        }
    }
    points
}

fn unique_edge_directions(solved: &crate::brush::SolvedBrush) -> Vec<[f64; 3]> {
    let mut edges = Vec::new();
    for polygon in solved.polygons.iter().flatten() {
        for index in 0..polygon.verts.len() {
            let edge = subtract(
                polygon.verts[(index + 1) % polygon.verts.len()],
                polygon.verts[index],
            );
            let length_squared = dot(edge, edge);
            if length_squared <= HULL_EPSILON * HULL_EPSILON {
                continue;
            }
            let mut direction = scale(edge, length_squared.sqrt().recip());
            if direction
                .into_iter()
                .find(|component| component.abs() > HULL_EPSILON)
                .is_some_and(|component| component < 0.0)
            {
                direction = negate(direction);
            }
            if !edges
                .iter()
                .any(|existing| dot(*existing, direction) > 1.0 - 1e-9)
            {
                edges.push(direction);
            }
        }
    }
    edges
}

fn intern_plane(
    planes: &mut Vec<[u8; 14]>,
    plane: [u8; 14],
) -> Result<i16, CollisionHullCompileError> {
    let index = planes
        .iter()
        .position(|existing| *existing == plane)
        .unwrap_or_else(|| {
            let index = planes.len();
            planes.push(plane);
            index
        });
    limit("planes", index + 1, i16::MAX as usize + 1)?;
    Ok(index as i16)
}

fn limit(kind: &'static str, count: usize, max: usize) -> Result<(), CollisionHullCompileError> {
    if count > max {
        Err(CollisionHullCompileError::LimitExceeded { kind, count, max })
    } else {
        Ok(())
    }
}

fn cross(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn dot(left: [f64; 3], right: [f64; 3]) -> f64 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn subtract(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn negate(value: [f64; 3]) -> [f64; 3] {
    [-value[0], -value[1], -value[2]]
}

fn scale(value: [f64; 3], amount: f64) -> [f64; 3] {
    [value[0] * amount, value[1] * amount, value[2] * amount]
}

fn squared_distance(left: [f64; 3], right: [f64; 3]) -> f64 {
    dot(subtract(left, right), subtract(left, right))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::BrushFace;
    use psx_bsp::collision::{CollisionHull, Trace, TraceScratch, Q12_ONE};
    use psx_bsp::{ClipNode, Plane as BspPlane, RecordSlice, Vec3I32};

    fn hull(compiled: &CompiledCollisionHulls, hull: usize) -> CollisionHull<'_> {
        CollisionHull::new(
            RecordSlice::<BspPlane>::new(&compiled.planes).expect("planes"),
            RecordSlice::<ClipNode>::new(&compiled.clipnodes).expect("nodes"),
            compiled.head_nodes[hull],
        )
    }

    fn at(x: i32, y: i32, z: i32) -> Vec3I32 {
        Vec3I32 {
            x: x * Q12_ONE,
            y: y * Q12_ONE,
            z: z * Q12_ONE,
        }
    }

    fn trace(hull: &CollisionHull<'_>, start: Vec3I32, end: Vec3I32) -> Trace {
        let mut output = Trace::default();
        assert!(hull.trace_into(
            &start,
            &end,
            &mut TraceScratch::new(),
            &mut output,
        ));
        output
    }

    const PLAYER: CollisionHullBounds = CollisionHullBounds {
        mins: [-16, 0, -16],
        maxs: [16, 56, 16],
    };

    #[test]
    fn player_hull_stops_at_wall_by_its_horizontal_radius() {
        let wall = Brush::cuboid([128, 0, 0], [192, 256, 256]);
        let compiled =
            compile_collision_hulls(&[wall], &[CollisionHullBounds::POINT, PLAYER]).expect("cook");
        let point = trace(
            &hull(&compiled, 0),
            at(0, 64, 64),
            at(200, 64, 64),
        );
        let player = trace(
            &hull(&compiled, 1),
            at(0, 64, 64),
            at(200, 64, 64),
        );
        let point_x = point.end.x as f64 / Q12_ONE as f64;
        let player_x = player.end.x as f64 / Q12_ONE as f64;
        assert!((127.0..128.1).contains(&point_x));
        assert!((111.0..112.1).contains(&player_x));
    }

    #[test]
    fn feet_origin_lands_on_floor_and_head_stops_below_ceiling() {
        let brushes = [
            Brush::cuboid([0, 0, 0], [512, 64, 512]),
            Brush::cuboid([0, 256, 0], [512, 320, 512]),
        ];
        let compiled = compile_collision_hulls(&brushes, &[PLAYER]).expect("cook");
        let hull = hull(&compiled, 0);
        let floor = trace(&hull, at(256, 160, 256), at(256, -32, 256));
        let ceiling = trace(&hull, at(256, 80, 256), at(256, 300, 256));
        let floor_y = floor.end.y as f64 / Q12_ONE as f64;
        let ceiling_y = ceiling.end.y as f64 / Q12_ONE as f64;
        assert!((63.0..64.1).contains(&floor_y));
        assert!((199.0..200.1).contains(&ceiling_y));
    }

    #[test]
    fn sloped_brush_adds_edge_bevel_planes_for_box_hull() {
        let mut wedge = Brush::cuboid([0, 0, 0], [128, 128, 128]);
        wedge.faces[5] = BrushFace::from_points([[0, 128, 0], [0, 128, 128], [128, 0, 128]]);
        let point = compile_collision_hulls(&[wedge.clone()], &[CollisionHullBounds::POINT])
            .expect("point hull");
        let player = compile_collision_hulls(&[wedge], &[PLAYER]).expect("player hull");
        assert!(player.planes.len() > point.planes.len());
        assert!(player.clipnodes.len() > point.clipnodes.len());
    }

    #[test]
    fn multi_hull_cook_is_deterministic() {
        let brushes = Brush::cuboid([0, 0, 0], [1024, 512, 1024])
            .hollow(64)
            .expect("room");
        let hulls = [CollisionHullBounds::POINT, PLAYER];
        assert_eq!(
            compile_collision_hulls(&brushes, &hulls),
            compile_collision_hulls(&brushes, &hulls)
        );
    }

    #[test]
    fn invalid_hull_bounds_fail_loudly() {
        let error = compile_collision_hulls(
            &[Brush::cuboid([0, 0, 0], [64, 64, 64])],
            &[CollisionHullBounds {
                mins: [1, 0, 0],
                maxs: [0, 0, 0],
            }],
        )
        .expect_err("bad bounds");
        assert_eq!(error, CollisionHullCompileError::InvalidBounds(0));
    }
}
