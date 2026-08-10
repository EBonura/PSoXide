//! Brush-world compiler cores (docs/bsp-engine-overhaul.md, the
//! first-playable slice): exterior CSG surfaces plus packed XBSP planes
//! and clipnodes consumed directly by `psx_bsp`.
//!
//! Construction is the union test over convex solids written as a BSP:
//! each brush contributes a chain of its face planes; a point inside
//! every plane of some brush is SOLID, escaping any plane falls through
//! to the next brush, and past the last brush is EMPTY.
// ponytail: chains are exact but unbalanced (depth = total face count)
// and planes are not deduplicated or sealed against the void; the
// qbsp-style balanced build with outer sealing replaces this when the
// full compiler lands.

use crate::ResourceId;
use crate::brush::{Brush, FaceUv, Plane};

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
const CSG_EPSILON: f64 = 1.0 / 65_536.0;

/// One exterior polygon after union CSG has removed brush interiors.
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledSurface {
    /// Exact authored face plane, wound outward from the solid union.
    pub plane: Plane,
    /// Convex exterior polygon in world coordinates.
    pub vertices: Vec<[f64; 3]>,
    /// Material inherited from the authored face.
    pub material: Option<ResourceId>,
    /// Texture transform inherited from the authored face.
    pub uv: FaceUv,
    /// Source brush index for diagnostics and deterministic coplanar ties.
    pub source_brush: usize,
    /// Source face index within `source_brush`.
    pub source_face: usize,
}

/// One child of a compiled surface BSP node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BspChild {
    Node(usize),
    Leaf(usize),
}

/// Brush-union contents assigned after BSP portalization.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BspLeafContents {
    #[default]
    Unclassified,
    Empty,
    Solid,
}

/// One plane partition in the compiled surface BSP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompiledBspNode {
    /// Exact splitter plane inherited from an authored face.
    pub plane: Plane,
    /// First coplanar polygon in `CompiledSurfaceBsp::surfaces`.
    pub first_surface: usize,
    /// Number of contiguous coplanar polygons owned by this node.
    pub surface_count: usize,
    /// Child in front of the splitter plane.
    pub front: BspChild,
    /// Child behind the splitter plane.
    pub back: BspChild,
}

/// One terminal cell in the compiled surface BSP.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompiledBspLeaf {
    /// Cell contents, filled by the portal and classification pass.
    pub contents: BspLeafContents,
    /// Conservative surface marks inherited from the leaf's split path.
    pub mark_surfaces: Vec<usize>,
}

/// Deterministic plane partition of exterior CSG polygons.
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledSurfaceBsp {
    pub root: BspChild,
    pub nodes: Vec<CompiledBspNode>,
    pub leaves: Vec<CompiledBspLeaf>,
    /// Polygons reordered by node, with split fragments inserted.
    pub surfaces: Vec<CompiledSurface>,
}

/// Compile the exterior boundary of the union of every valid brush.
///
/// Each solved face polygon is subtracted by every other convex brush.
/// Shared faces disappear, intersections split into convex fragments and
/// same-facing coplanar overlaps are owned by the lower brush index.
pub fn compile_csg_surfaces(brushes: &[Brush]) -> Vec<CompiledSurface> {
    let solved: Vec<_> = brushes.iter().map(Brush::solve).collect();
    let planes: Vec<Vec<Option<Plane>>> = brushes
        .iter()
        .map(|brush| {
            brush
                .faces
                .iter()
                .map(|face| Plane::from_points(face.points))
                .collect()
        })
        .collect();
    let volume_planes: Vec<Vec<Plane>> = planes
        .iter()
        .map(|brush| brush.iter().copied().flatten().collect())
        .collect();
    let valid: Vec<bool> = solved.iter().map(|brush| brush.is_valid()).collect();
    let mut output = Vec::new();

    for (brush_index, brush) in brushes.iter().enumerate() {
        if !valid[brush_index] {
            continue;
        }
        for (face_index, face) in brush.faces.iter().enumerate() {
            let Some(face_plane) = planes[brush_index][face_index] else {
                continue;
            };
            let Some(polygon) = solved[brush_index].polygons[face_index].as_ref() else {
                continue;
            };
            let mut fragments = vec![polygon.verts.clone()];
            for other_index in 0..brushes.len() {
                if other_index == brush_index || !valid[other_index] {
                    continue;
                }
                let other_planes = &volume_planes[other_index];
                let keep_coplanar_inside = brush_index < other_index
                    && other_planes
                        .iter()
                        .any(|other| same_facing_plane(face_plane, *other));
                if keep_coplanar_inside {
                    continue;
                }
                fragments = fragments
                    .into_iter()
                    .flat_map(|fragment| subtract_convex_brush(&fragment, other_planes))
                    .collect();
                if fragments.is_empty() {
                    break;
                }
            }
            output.extend(fragments.into_iter().map(|vertices| CompiledSurface {
                plane: face_plane,
                vertices,
                material: face.material,
                uv: face.uv,
                source_brush: brush_index,
                source_face: face_index,
            }));
        }
    }
    output
}

/// Recursively partition exterior CSG polygons into a render BSP.
///
/// Splitter selection minimizes polygon splits first and tree imbalance
/// second. Input order is the final tie breaker, so identical inputs
/// always produce identical node, leaf and surface ordering.
pub fn build_surface_bsp(surfaces: &[CompiledSurface]) -> CompiledSurfaceBsp {
    let mut bsp = CompiledSurfaceBsp {
        root: BspChild::Leaf(0),
        nodes: Vec::new(),
        leaves: Vec::new(),
        surfaces: Vec::new(),
    };
    bsp.root = build_surface_bsp_branch(surfaces.to_vec(), &[], &mut bsp);
    bsp
}

fn build_surface_bsp_branch(
    surfaces: Vec<CompiledSurface>,
    boundary_surfaces: &[usize],
    bsp: &mut CompiledSurfaceBsp,
) -> BspChild {
    if surfaces.is_empty() {
        let index = bsp.leaves.len();
        bsp.leaves.push(CompiledBspLeaf {
            contents: BspLeafContents::Unclassified,
            mark_surfaces: boundary_surfaces.to_vec(),
        });
        return BspChild::Leaf(index);
    }

    let splitter = choose_splitter(&surfaces);
    let splitter_plane = surfaces[splitter].plane;
    let mut coplanar = Vec::new();
    let mut front = Vec::new();
    let mut back = Vec::new();
    for surface in surfaces {
        match split_polygon(&surface.vertices, splitter_plane) {
            PolygonSplit::Front(vertices) => front.push(with_vertices(&surface, vertices)),
            PolygonSplit::Back(vertices) => back.push(with_vertices(&surface, vertices)),
            PolygonSplit::Coplanar => coplanar.push(surface),
            PolygonSplit::Split {
                front: front_vertices,
                back: back_vertices,
            } => {
                front.push(with_vertices(&surface, front_vertices));
                back.push(with_vertices(&surface, back_vertices));
            }
        }
    }

    debug_assert!(!coplanar.is_empty(), "splitter must own a polygon");
    let node_index = bsp.nodes.len();
    let first_surface = bsp.surfaces.len();
    let surface_count = coplanar.len();
    bsp.surfaces.extend(coplanar);
    bsp.nodes.push(CompiledBspNode {
        plane: splitter_plane,
        first_surface,
        surface_count,
        front: BspChild::Leaf(0),
        back: BspChild::Leaf(0),
    });

    // ponytail: ancestor split surfaces conservatively over-mark each
    // terminal cell until portal clipping supplies exact leaf windings.
    let mut child_boundaries = boundary_surfaces.to_vec();
    child_boundaries.extend(first_surface..first_surface + surface_count);
    let front_child = build_surface_bsp_branch(front, &child_boundaries, bsp);
    let back_child = build_surface_bsp_branch(back, &child_boundaries, bsp);
    bsp.nodes[node_index].front = front_child;
    bsp.nodes[node_index].back = back_child;
    BspChild::Node(node_index)
}

fn choose_splitter(surfaces: &[CompiledSurface]) -> usize {
    surfaces
        .iter()
        .enumerate()
        .map(|(candidate, surface)| {
            let mut front = 0usize;
            let mut back = 0usize;
            let mut splits = 0usize;
            for classified in surfaces {
                match split_polygon(&classified.vertices, surface.plane) {
                    PolygonSplit::Front(_) => front += 1,
                    PolygonSplit::Back(_) => back += 1,
                    PolygonSplit::Coplanar => {}
                    PolygonSplit::Split { .. } => {
                        front += 1;
                        back += 1;
                        splits += 1;
                    }
                }
            }
            let imbalance = front.abs_diff(back);
            ((splits, imbalance, candidate), candidate)
        })
        .min_by_key(|(score, _)| *score)
        .map(|(_, candidate)| candidate)
        .expect("non-empty surface set")
}

fn with_vertices(surface: &CompiledSurface, vertices: Vec<[f64; 3]>) -> CompiledSurface {
    let mut fragment = surface.clone();
    fragment.vertices = vertices;
    fragment
}

fn subtract_convex_brush(polygon: &[[f64; 3]], planes: &[Plane]) -> Vec<Vec<[f64; 3]>> {
    let mut inside = polygon.to_vec();
    let mut outside = Vec::new();
    for plane in planes {
        match split_polygon(&inside, *plane) {
            PolygonSplit::Front(front) => {
                outside.push(front);
                return outside;
            }
            PolygonSplit::Back(back) => inside = back,
            PolygonSplit::Coplanar => {}
            PolygonSplit::Split { front, back } => {
                outside.push(front);
                inside = back;
            }
        }
    }
    outside
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PolygonSplit {
    Front(Vec<[f64; 3]>),
    Back(Vec<[f64; 3]>),
    Coplanar,
    Split {
        front: Vec<[f64; 3]>,
        back: Vec<[f64; 3]>,
    },
}

pub(crate) fn split_polygon(vertices: &[[f64; 3]], plane: Plane) -> PolygonSplit {
    let (normal, distance) = normalized_plane(plane);
    let distances: Vec<f64> = vertices
        .iter()
        .map(|vertex| dot(normal, *vertex) - distance)
        .collect();
    let has_front = distances.iter().any(|distance| *distance > CSG_EPSILON);
    let has_back = distances.iter().any(|distance| *distance < -CSG_EPSILON);
    match (has_front, has_back) {
        (false, false) => return PolygonSplit::Coplanar,
        (true, false) => return PolygonSplit::Front(vertices.to_vec()),
        (false, true) => return PolygonSplit::Back(vertices.to_vec()),
        (true, true) => {}
    }

    let mut front = Vec::new();
    let mut back = Vec::new();
    for index in 0..vertices.len() {
        let next = (index + 1) % vertices.len();
        let current_vertex = vertices[index];
        let next_vertex = vertices[next];
        let current_distance = distances[index];
        let next_distance = distances[next];
        if current_distance >= -CSG_EPSILON {
            push_welded(&mut front, current_vertex);
        }
        if current_distance <= CSG_EPSILON {
            push_welded(&mut back, current_vertex);
        }
        if (current_distance > CSG_EPSILON && next_distance < -CSG_EPSILON)
            || (current_distance < -CSG_EPSILON && next_distance > CSG_EPSILON)
        {
            let amount = current_distance / (current_distance - next_distance);
            let intersection = [
                current_vertex[0] + (next_vertex[0] - current_vertex[0]) * amount,
                current_vertex[1] + (next_vertex[1] - current_vertex[1]) * amount,
                current_vertex[2] + (next_vertex[2] - current_vertex[2]) * amount,
            ];
            push_welded(&mut front, intersection);
            push_welded(&mut back, intersection);
        }
    }
    close_welded(&mut front);
    close_welded(&mut back);
    PolygonSplit::Split { front, back }
}

fn same_facing_plane(left: Plane, right: Plane) -> bool {
    let (left_normal, left_distance) = normalized_plane(left);
    let (right_normal, right_distance) = normalized_plane(right);
    dot(left_normal, right_normal) > 1.0 - CSG_EPSILON
        && (left_distance - right_distance).abs() <= CSG_EPSILON
}

pub(crate) fn normalized_plane(plane: Plane) -> ([f64; 3], f64) {
    let normal = plane.normal.map(|component| component as f64);
    let length = dot(normal, normal).sqrt();
    (
        [normal[0] / length, normal[1] / length, normal[2] / length],
        plane.dist as f64 / length,
    )
}

fn dot(left: [f64; 3], right: [f64; 3]) -> f64 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn push_welded(vertices: &mut Vec<[f64; 3]>, vertex: [f64; 3]) {
    let distinct = vertices
        .last()
        .is_none_or(|last| squared_distance(*last, vertex) > CSG_EPSILON * CSG_EPSILON);
    if distinct {
        vertices.push(vertex);
    }
}

fn close_welded(vertices: &mut Vec<[f64; 3]>) {
    if vertices.len() > 1
        && squared_distance(vertices[0], *vertices.last().unwrap()) <= CSG_EPSILON * CSG_EPSILON
    {
        vertices.pop();
    }
}

fn squared_distance(left: [f64; 3], right: [f64; 3]) -> f64 {
    let delta = [left[0] - right[0], left[1] - right[1], left[2] - right[2]];
    dot(delta, delta)
}

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
    use psx_bsp::collision::{CONTENTS_EMPTY, CONTENTS_SOLID, CollisionHull};
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

    fn surface_axis_coordinate(surface: &CompiledSurface, axis: usize) -> Option<f64> {
        let (normal, distance) = normalized_plane(surface.plane);
        (normal[axis].abs() > 1.0 - CSG_EPSILON
            && (0..3).all(|other| other == axis || normal[other].abs() <= CSG_EPSILON))
        .then(|| distance / normal[axis])
    }

    fn bsp_depth(child: BspChild, bsp: &CompiledSurfaceBsp) -> usize {
        match child {
            BspChild::Leaf(_) => 0,
            BspChild::Node(index) => {
                let node = &bsp.nodes[index];
                1 + bsp_depth(node.front, bsp).max(bsp_depth(node.back, bsp))
            }
        }
    }

    #[test]
    fn csg_single_cuboid_emits_six_exterior_quads() {
        let surfaces = compile_csg_surfaces(&[Brush::cuboid([0, 0, 0], [128, 64, 256])]);
        assert_eq!(surfaces.len(), 6);
        assert!(surfaces.iter().all(|surface| surface.vertices.len() == 4));
    }

    #[test]
    fn csg_adjacent_cuboids_remove_both_shared_faces() {
        let surfaces = compile_csg_surfaces(&[
            Brush::cuboid([0, 0, 0], [128, 128, 128]),
            Brush::cuboid([128, 0, 0], [256, 128, 128]),
        ]);
        assert_eq!(surfaces.len(), 10);
        assert!(!surfaces.iter().any(|surface| {
            surface_axis_coordinate(surface, 0).is_some_and(|x| (x - 128.0).abs() < CSG_EPSILON)
        }));
    }

    #[test]
    fn csg_overlapping_cuboids_clip_away_buried_face_regions() {
        let surfaces = compile_csg_surfaces(&[
            Brush::cuboid([0, 0, 0], [128, 128, 128]),
            Brush::cuboid([64, 0, 0], [192, 128, 128]),
        ]);
        assert_eq!(surfaces.len(), 10);
        assert!(!surfaces.iter().any(|surface| {
            surface_axis_coordinate(surface, 0)
                .is_some_and(|x| (x - 64.0).abs() < CSG_EPSILON || (x - 128.0).abs() < CSG_EPSILON)
        }));
    }

    #[test]
    fn csg_duplicate_brushes_keep_one_deterministic_owner() {
        let brush = Brush::cuboid([0, 0, 0], [128, 128, 128]);
        let surfaces = compile_csg_surfaces(&[brush.clone(), brush]);
        assert_eq!(surfaces.len(), 6);
        assert!(surfaces.iter().all(|surface| surface.source_brush == 0));
    }

    #[test]
    fn csg_preserves_authored_face_uvs() {
        let mut brush = Brush::cuboid([0, 0, 0], [128, 128, 128]);
        brush.faces[2].uv.offset_texels = [17, -9];
        brush.faces[2].uv.rotation_deg = 45;
        let surfaces = compile_csg_surfaces(&[brush]);
        let face = surfaces
            .iter()
            .find(|surface| surface.source_face == 2)
            .expect("source face survives");
        assert_eq!(face.uv.offset_texels, [17, -9]);
        assert_eq!(face.uv.rotation_deg, 45);
    }

    #[test]
    fn surface_bsp_empty_input_is_one_leaf() {
        let bsp = build_surface_bsp(&[]);
        assert_eq!(bsp.root, BspChild::Leaf(0));
        assert!(bsp.nodes.is_empty());
        assert_eq!(bsp.leaves, vec![CompiledBspLeaf::default()]);
        assert!(bsp.surfaces.is_empty());
    }

    #[test]
    fn surface_bsp_cuboid_builds_six_partitions_and_seven_leaves() {
        let surfaces = compile_csg_surfaces(&[Brush::cuboid([0, 0, 0], [128, 64, 256])]);
        let bsp = build_surface_bsp(&surfaces);
        assert_eq!(bsp.nodes.len(), 6);
        assert_eq!(bsp.leaves.len(), 7);
        assert_eq!(bsp.surfaces.len(), 6);
        assert!(bsp.nodes.iter().all(|node| node.surface_count == 1));
        assert!(
            bsp.surfaces
                .iter()
                .all(|surface| surface.vertices.len() >= 3)
        );
    }

    #[test]
    fn surface_bsp_groups_coplanar_csg_fragments() {
        let surfaces = compile_csg_surfaces(&[
            Brush::cuboid([0, 0, 0], [128, 128, 128]),
            Brush::cuboid([128, 0, 0], [256, 128, 128]),
        ]);
        let bsp = build_surface_bsp(&surfaces);
        assert_eq!(bsp.surfaces.len(), surfaces.len());
        assert!(bsp.nodes.iter().any(|node| node.surface_count > 1));
        for node in &bsp.nodes {
            let end = node.first_surface + node.surface_count;
            assert!(end <= bsp.surfaces.len());
            assert!(
                bsp.surfaces[node.first_surface..end]
                    .iter()
                    .all(|surface| matches!(
                        split_polygon(&surface.vertices, node.plane),
                        PolygonSplit::Coplanar
                    ))
            );
        }
    }

    #[test]
    fn surface_bsp_balances_separated_solids() {
        let surfaces = compile_csg_surfaces(&[
            Brush::cuboid([0, 0, 0], [128, 128, 128]),
            Brush::cuboid([512, 0, 0], [640, 128, 128]),
        ]);
        let bsp = build_surface_bsp(&surfaces);
        assert!(bsp.nodes.len() > 6);
        assert!(bsp_depth(bsp.root, &bsp) < bsp.nodes.len());
    }

    #[test]
    fn surface_bsp_splits_crossing_polygons() {
        let horizontal = CompiledSurface {
            plane: Plane::from_points([[-64, 0, -64], [64, 0, -64], [64, 0, 64]])
                .expect("horizontal plane"),
            vertices: vec![
                [-64.0, 0.0, -64.0],
                [64.0, 0.0, -64.0],
                [64.0, 0.0, 64.0],
                [-64.0, 0.0, 64.0],
            ],
            material: None,
            uv: FaceUv::default(),
            source_brush: 0,
            source_face: 0,
        };
        let vertical = CompiledSurface {
            plane: Plane::from_points([[0, -64, -64], [0, 64, -64], [0, 64, 64]])
                .expect("vertical plane"),
            vertices: vec![
                [0.0, -64.0, -64.0],
                [0.0, 64.0, -64.0],
                [0.0, 64.0, 64.0],
                [0.0, -64.0, 64.0],
            ],
            material: None,
            uv: FaceUv::default(),
            source_brush: 1,
            source_face: 0,
        };
        let bsp = build_surface_bsp(&[horizontal, vertical]);
        assert_eq!(bsp.surfaces.len(), 3);
        assert_eq!(
            bsp.surfaces
                .iter()
                .filter(|surface| surface.source_brush == 1)
                .count(),
            2
        );
        assert!(
            bsp.surfaces
                .iter()
                .all(|surface| surface.vertices.len() >= 3)
        );
    }

    #[test]
    fn surface_bsp_build_is_deterministic() {
        let surfaces = compile_csg_surfaces(&[
            Brush::cuboid([0, 0, 0], [128, 128, 128]),
            Brush::cuboid([64, 64, 64], [192, 192, 192]),
        ]);
        assert_eq!(build_surface_bsp(&surfaces), build_surface_bsp(&surfaces));
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
