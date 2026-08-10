//! Draft brush collision compiler (docs/bsp-engine-overhaul.md, the
//! first-playable slice): brushes become packed XBSP plane and clipnode
//! records that `psx_bsp::collision::CollisionHull` traces directly.
//!
//! Construction is the union test over convex solids written as a BSP:
//! each brush contributes a chain of its face planes; a point inside
//! every plane of some brush is SOLID, escaping any plane falls through
//! to the next brush, and past the last brush is EMPTY.
// ponytail: chains are exact but unbalanced (depth = total face count)
// and planes are not deduplicated or sealed against the void; the
// qbsp-style balanced build with outer sealing replaces this when the
// full compiler lands.

use crate::brush::{Brush, Plane};

/// Fixed-point scale shared with the runtime: positions and plane
/// distances are Q20.12, normals Q3.12.
pub const Q12_ONE: i32 = 4096;

/// Packed collision BSP ready for `psx_bsp` record slices.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompiledCollision {
    /// Packed 14-byte XBSP plane records.
    pub planes: Vec<u8>,
    /// Packed 6-byte XBSP clipnode records.
    pub clipnodes: Vec<u8>,
    /// Root clipnode index, or a contents value when no brush compiled.
    pub head_node: i16,
}

const CONTENTS_EMPTY: i16 = -1;
const CONTENTS_SOLID: i16 = -2;

/// Compile every valid brush into one point-hull collision BSP.
pub fn compile_collision(brushes: &[Brush]) -> CompiledCollision {
    let mut planes: Vec<u8> = Vec::new();
    let mut nodes: Vec<[i16; 3]> = Vec::new();

    // Later brushes are appended first so the chain order matches the
    // authored order front to back.
    let mut escape = CONTENTS_EMPTY;
    for brush in brushes.iter().rev() {
        if !brush.solve().is_valid() {
            continue;
        }
        let mut face_planes: Vec<(i16, bool)> = Vec::new();
        for face in &brush.faces {
            let Some(plane) = Plane::from_points(face.points) else {
                continue;
            };
            let Some((packed, flipped)) = pack_plane(&plane) else {
                continue;
            };
            let index = planes.len() / 14;
            if index > i16::MAX as usize {
                break;
            }
            planes.extend_from_slice(&packed);
            face_planes.push((index as i16, flipped));
        }
        if face_planes.is_empty() {
            continue;
        }
        // Innermost decision first: inside every plane means solid.
        let mut inside: i16 = CONTENTS_SOLID;
        for &(plane, flipped) in face_planes.iter().rev() {
            let node_index = nodes.len();
            if node_index > i16::MAX as usize {
                break;
            }
            // Front child (children[0]) is taken where the stored
            // plane's signed distance is >= 0. With the outward face
            // normal stored as-is, front is outside the brush; axial
            // canonicalization flips that.
            let (front, back) = if flipped {
                (inside, escape)
            } else {
                (escape, inside)
            };
            nodes.push([plane, front, back]);
            inside = node_index as i16;
        }
        escape = inside;
    }

    let mut clipnodes = Vec::with_capacity(nodes.len() * 6);
    for node in &nodes {
        clipnodes.extend_from_slice(&node[0].to_le_bytes());
        clipnodes.extend_from_slice(&node[1].to_le_bytes());
        clipnodes.extend_from_slice(&node[2].to_le_bytes());
    }
    CompiledCollision {
        planes,
        clipnodes,
        head_node: escape,
    }
}

/// Pack one kernel plane as a 14-byte XBSP record: Q3.12 unit normal,
/// Q20.12 distance, axial kind. Returns the bytes and whether the
/// stored plane is flipped relative to the face's outward normal
/// (axial planes are canonicalized to positive normals).
fn pack_plane(plane: &Plane) -> Option<([u8; 14], bool)> {
    let n = plane.normal.map(|v| v as f64);
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len <= 0.0 {
        return None;
    }
    let unit = [n[0] / len, n[1] / len, n[2] / len];
    let dist_world = plane.dist as f64 / len;

    let axis = (0..3).find(|&a| {
        unit[a].abs() > 1.0 - 1e-9 && (0..3).all(|other| other == a || unit[other].abs() < 1e-9)
    });
    let (stored_unit, stored_dist, kind, flipped) = match axis {
        Some(a) if unit[a] < 0.0 => {
            let mut s = [0.0; 3];
            s[a] = 1.0;
            (s, -dist_world, a as i32, true)
        }
        Some(a) => {
            let mut s = [0.0; 3];
            s[a] = 1.0;
            (s, dist_world, a as i32, false)
        }
        None => (unit, dist_world, 3, false),
    };

    let q12 = |v: f64| ((v * Q12_ONE as f64).round() as i32).clamp(-4096, 4096) as i16;
    let distance_q12 = (stored_dist * Q12_ONE as f64).round();
    if distance_q12.abs() >= i32::MAX as f64 {
        return None;
    }
    let mut bytes = [0u8; 14];
    bytes[0..2].copy_from_slice(&q12(stored_unit[0]).to_le_bytes());
    bytes[2..4].copy_from_slice(&q12(stored_unit[1]).to_le_bytes());
    bytes[4..6].copy_from_slice(&q12(stored_unit[2]).to_le_bytes());
    bytes[6..10].copy_from_slice(&(distance_q12 as i32).to_le_bytes());
    bytes[10..14].copy_from_slice(&kind.to_le_bytes());
    Some((bytes, flipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_bsp::collision::{CollisionHull, CONTENTS_EMPTY, CONTENTS_SOLID};
    use psx_bsp::{ClipNode, Plane as BspPlane, RecordSlice, Vec3I32};

    fn hull(compiled: &CompiledCollision) -> CollisionHull<'_> {
        CollisionHull::new(
            RecordSlice::<BspPlane>::new(&compiled.planes).expect("plane bytes"),
            RecordSlice::<ClipNode>::new(&compiled.clipnodes).expect("clipnode bytes"),
            compiled.head_node,
        )
    }

    fn q12(world: i32) -> i32 {
        world * Q12_ONE
    }

    fn at(x: i32, y: i32, z: i32) -> Vec3I32 {
        Vec3I32 {
            x: q12(x),
            y: q12(y),
            z: q12(z),
        }
    }

    #[test]
    fn solid_brush_classifies_inside_and_outside() {
        let compiled = compile_collision(&[Brush::cuboid([0, 0, 0], [512, 256, 512])]);
        let hull = hull(&compiled);
        assert_eq!(hull.point_contents(at(256, 128, 256)), Some(CONTENTS_SOLID));
        assert_eq!(hull.point_contents(at(-64, 128, 256)), Some(CONTENTS_EMPTY));
        assert_eq!(hull.point_contents(at(256, 300, 256)), Some(CONTENTS_EMPTY));
    }

    #[test]
    fn hollow_room_has_an_empty_cavity_with_solid_walls() {
        let slabs = Brush::cuboid([0, 0, 0], [1024, 512, 1024])
            .hollow(64)
            .expect("hollowable");
        let compiled = compile_collision(&slabs);
        let hull = hull(&compiled);
        assert_eq!(hull.point_contents(at(512, 256, 512)), Some(CONTENTS_EMPTY));
        assert_eq!(hull.point_contents(at(512, 32, 512)), Some(CONTENTS_SOLID));
        assert_eq!(hull.point_contents(at(32, 256, 512)), Some(CONTENTS_SOLID));
    }

    #[test]
    fn downward_trace_lands_on_the_floor_slab() {
        let slabs = Brush::cuboid([0, 0, 0], [1024, 512, 1024])
            .hollow(64)
            .expect("hollowable");
        let compiled = compile_collision(&slabs);
        let hull = hull(&compiled);
        let trace = hull
            .trace(at(512, 400, 512), at(512, -400, 512))
            .expect("trace");
        assert!(!trace.start_solid);
        assert!(trace.in_open);
        assert!(trace.fraction < Q12_ONE, "floor must obstruct");
        // Floor top is y=64; the tracer backs off by its epsilon.
        let end_world = trace.end.y as f64 / Q12_ONE as f64;
        assert!(
            (63.9..66.0).contains(&end_world),
            "end y {end_world} should rest on the floor top"
        );
        assert_eq!(trace.normal.y, 4096, "floor normal points up");
    }

    #[test]
    fn union_of_two_brushes_traces_across_the_gap() {
        let compiled = compile_collision(&[
            Brush::cuboid([0, 0, 0], [128, 128, 128]),
            Brush::cuboid([512, 0, 0], [640, 128, 128]),
        ]);
        let hull = hull(&compiled);
        assert_eq!(hull.point_contents(at(300, 64, 64)), Some(CONTENTS_EMPTY));
        let trace = hull.trace(at(300, 64, 64), at(700, 64, 64)).expect("trace");
        assert!(trace.fraction < Q12_ONE);
        let end_world = trace.end.x as f64 / Q12_ONE as f64;
        assert!(
            (509.0..512.1).contains(&end_world),
            "end x {end_world} should stop at the second brush"
        );
        assert_eq!(trace.normal.x, -4096, "hit face looks back along -X");
    }

    #[test]
    fn empty_scene_is_all_open() {
        let compiled = compile_collision(&[]);
        assert_eq!(compiled.head_node, -1);
        let hull = hull(&compiled);
        assert_eq!(hull.point_contents(at(0, 0, 0)), Some(CONTENTS_EMPTY));
    }
}
