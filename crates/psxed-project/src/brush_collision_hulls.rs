//! Multi-hull brush collision cooking with box-expansion bevels.

use crate::brush::{Brush, Plane};
use crate::brush_compile::{pack_normalized_plane, pack_plane};

const CONTENTS_EMPTY: i16 = psx_bsp::collision::CONTENTS_EMPTY;
pub(crate) const HULL_EPSILON: f64 = 1.0 / 1024.0;
// E1M1's authored brush hull stays below the signed 16-bit clipnode ceiling
// at 48 while cutting the worst moving-player leaf walks substantially. A
// 32-brush leaf crosses 32,768 clipnodes and is therefore not representable.
// Smaller leaves are an explicit per-project opt-in (`CollisionLeafTarget`).
const SPATIAL_LEAF_BRUSHES: usize = 48;

/// Maximum brush target per spatial collision leaf. Smaller leaves trade
/// additional cooked collision RAM for shorter traversal chains. Both policies
/// retain the depth cap, unique-node limit and exact ordered brush decisions.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum CollisionLeafTarget {
    /// Legacy policy, used when older project files omit the setting.
    #[default]
    Brushes48,
    /// Shorter chains with a potentially larger collision table.
    Brushes32,
}
impl CollisionLeafTarget {
    fn brushes(self) -> usize {
        match self {
            Self::Brushes48 => 48,
            Self::Brushes32 => 32,
        }
    }
}
const MAX_SPATIAL_DEPTH: usize = 12;
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
    /// A brush face produced a degenerate plane. Carries the brush index when
    /// the failure happened inside a per-brush hull expansion; `None` when it
    /// came from a whole-tree pass with no single brush in scope.
    InvalidPlane(Option<usize>),
    LimitExceeded {
        kind: &'static str,
        count: usize,
        max: usize,
    },
}

/// How the per-hull collision tree is built from the expanded brush polytopes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CollisionHullStrategy {
    /// Axial split down to small leaves, each ending in per-brush plane chains.
    SpatialChains,
    /// One long per-brush plane chain per hull (reference for tests).
    LinearChains,
    /// Solid-leaf hull BSP over the expanded polytopes
    /// (`brush_region_hulls`).
    #[default]
    HullBsp,
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedHullBrush {
    /// Interned plane indices; empty under [`CollisionHullStrategy::HullBsp`],
    /// which interns only the planes it actually splits on.
    planes: Vec<(i16, bool)>,
    /// Packed plane records and whether each is flipped, in face order.
    pub(crate) records: Vec<([u8; 14], bool)>,
    pub(crate) mins: [f64; 3],
    pub(crate) maxs: [f64; 3],
    pub(crate) contents: i16,
}

#[derive(Clone, Copy, Debug)]
struct SpatialSplit {
    axis: usize,
    distance: f64,
}

/// Compile point and box-expanded collision trees into one record table.
///
/// This is the proven chain compiler, kept as the reference the hull BSP is
/// tested against. The playtest cook picks its strategy from the project's
/// `collision_hull_bsp` setting (default on).
pub fn compile_collision_hulls(
    brushes: &[Brush],
    hulls: &[CollisionHullBounds],
) -> Result<CompiledCollisionHulls, CollisionHullCompileError> {
    compile_collision_hulls_with(brushes, hulls, CollisionHullStrategy::SpatialChains)
}

/// [`compile_collision_hulls`] with an explicit tree-building strategy.
///
/// [`CollisionHullStrategy::HullBsp`] falls back to the spatial chains for
/// any hull whose BSP would not fit the clipnode or plane limits.
pub fn compile_collision_hulls_with(
    brushes: &[Brush],
    hulls: &[CollisionHullBounds],
    strategy: CollisionHullStrategy,
) -> Result<CompiledCollisionHulls, CollisionHullCompileError> {
    compile_collision_hulls_inner(brushes, hulls, strategy)
}

/// Compile collision hulls using an explicit strategy and leaf-size policy.
/// The leaf target applies wherever spatial chains are built, including the
/// fallback for a hull BSP that does not fit the clipnode limits.
pub fn compile_collision_hulls_with_target(
    brushes: &[Brush],
    hulls: &[CollisionHullBounds],
    strategy: CollisionHullStrategy,
    target: CollisionLeafTarget,
) -> Result<CompiledCollisionHulls, CollisionHullCompileError> {
    compile_collision_hulls_with_leaf_size(brushes, hulls, strategy, target.brushes())
}

fn compile_collision_hulls_inner(
    brushes: &[Brush],
    hulls: &[CollisionHullBounds],
    strategy: CollisionHullStrategy,
) -> Result<CompiledCollisionHulls, CollisionHullCompileError> {
    compile_collision_hulls_with_leaf_size(brushes, hulls, strategy, SPATIAL_LEAF_BRUSHES)
}

fn compile_collision_hulls_with_leaf_size(
    brushes: &[Brush],
    hulls: &[CollisionHullBounds],
    strategy: CollisionHullStrategy,
    leaf_size: usize,
) -> Result<CompiledCollisionHulls, CollisionHullCompileError> {
    limit("collision hulls", hulls.len(), MAX_MAP_HULLS)?;
    let mut ordered: Vec<_> = brushes.iter().enumerate().collect();
    ordered.sort_by_key(|(index, brush)| (core::cmp::Reverse(brush.contents.precedence()), *index));
    let solved: Vec<_> = ordered.iter().map(|(_, brush)| brush.solve()).collect();
    let mut plane_records = Vec::new();
    let mut nodes = NodeBuilder::default();
    let mut head_nodes = Vec::with_capacity(hulls.len());

    for (hull_index, hull) in hulls.iter().copied().enumerate() {
        if (0..3).any(|axis| hull.mins[axis] > hull.maxs[axis]) {
            return Err(CollisionHullCompileError::InvalidBounds(hull_index));
        }
        let mut prepared = Vec::new();
        for ((brush_index, brush), solved) in ordered.iter().zip(&solved).rev() {
            if !solved.is_valid() {
                continue;
            }
            // Name the brush: the hull helpers only see one brush at a time
            // and cannot know where it sits in the authored list.
            let blame = |error| match error {
                CollisionHullCompileError::InvalidPlane(_) => {
                    CollisionHullCompileError::InvalidPlane(Some(*brush_index))
                }
                other => other,
            };
            let (planes, mins, maxs) = if hull == CollisionHullBounds::POINT {
                (
                    point_hull_planes(brush).map_err(blame)?,
                    solved.min,
                    solved.max,
                )
            } else {
                let points = expanded_points(&unique_brush_vertices(solved), hull);
                let (mins, maxs) = point_bounds(&points);
                (
                    expanded_hull_planes(brush, solved, hull).map_err(blame)?,
                    mins,
                    maxs,
                )
            };
            if planes.is_empty() {
                continue;
            }

            let mut face_planes = Vec::with_capacity(planes.len());
            if strategy != CollisionHullStrategy::HullBsp {
                for &(record, flipped) in &planes {
                    let plane = intern_plane(&mut plane_records, record)?;
                    face_planes.push((plane, flipped));
                }
            }
            prepared.push(PreparedHullBrush {
                planes: face_planes,
                records: planes,
                mins,
                maxs,
                contents: brush.contents.runtime_contents(),
            });
        }
        // The authored precedence order above is low-to-high because the old
        // chain compiler wrapped each successive brush around the previous
        // root. Restore high-to-low before building spatial leaves so their
        // small fallback chains preserve the exact overlap semantics.
        prepared.reverse();
        let brushes: Vec<_> = (0..prepared.len()).collect();
        head_nodes.push(match strategy {
            CollisionHullStrategy::SpatialChains => build_spatial_hull(
                &prepared,
                &brushes,
                0,
                leaf_size,
                &mut plane_records,
                &mut nodes,
            )?,
            CollisionHullStrategy::LinearChains => {
                build_brush_chain(&prepared, &brushes, &mut nodes)?
            }
            CollisionHullStrategy::HullBsp => {
                let plane_mark = plane_records.len();
                let node_mark = nodes.records.len();
                // The hull BSP appends raw records. They bypass the interning
                // table, which stays valid because it only refers to earlier
                // slots; the final identical-subtree pass still merges them.
                match crate::brush_region_hulls::build_hull_bsp(
                    &prepared,
                    &mut plane_records,
                    &mut nodes.records,
                ) {
                    Ok(head) => head,
                    Err(CollisionHullCompileError::LimitExceeded { .. }) => {
                        // Roll back and keep the proven chain tree for this hull.
                        plane_records.truncate(plane_mark);
                        nodes.records.truncate(node_mark);
                        for brush in &mut prepared {
                            brush.planes.clear();
                            for &(record, flipped) in &brush.records {
                                let plane = intern_plane(&mut plane_records, record)?;
                                brush.planes.push((plane, flipped));
                            }
                        }
                        build_spatial_hull(
                            &prepared,
                            &brushes,
                            0,
                            leaf_size,
                            &mut plane_records,
                            &mut nodes,
                        )?
                    }
                    Err(other) => return Err(other),
                }
            }
        });
    }

    let nodes = intern_identical_nodes(&nodes.records, &mut head_nodes);
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

/// Merge only recursively identical node triples. Plane and contents labels,
/// branch order and every root's decision tree remain unchanged. The input is
/// the acyclic graph constructed above, not an untrusted serialized node table.
fn intern_identical_nodes(nodes: &[[i16; 3]], heads: &mut [i16]) -> Vec<[i16; 3]> {
    let mut remap = vec![None; nodes.len()];
    let mut interned = std::collections::HashMap::new();
    let mut output = Vec::new();
    let mut pending = Vec::new();
    for root in 0..nodes.len() {
        pending.push((root, false));
        while let Some((index, expanded)) = pending.pop() {
            if remap[index].is_some() {
                continue;
            }
            let mut node = nodes[index];
            if !expanded {
                pending.push((index, true));
                for &child in node[1..].iter().rev() {
                    if child >= 0 && remap[child as usize].is_none() {
                        pending.push((child as usize, false));
                    }
                }
                continue;
            }
            for child in &mut node[1..] {
                if *child >= 0 {
                    *child = remap[*child as usize].expect("child completed before parent");
                }
            }
            let canonical = *interned.entry(node).or_insert_with(|| {
                // Output never exceeds the already range-checked input count.
                let canonical = output.len() as i16;
                output.push(node);
                canonical
            });
            remap[index] = Some(canonical);
        }
    }
    for head in heads {
        if *head >= 0 {
            *head = remap[*head as usize].expect("root completed");
        }
    }
    output
}

/// Only complete nodes enter the table; all children already have canonical IDs.
#[derive(Default)]
struct NodeBuilder {
    records: Vec<[i16; 3]>,
    indices: std::collections::HashMap<[i16; 3], i16>,
}
impl NodeBuilder {
    fn intern(&mut self, node: [i16; 3]) -> Result<i16, CollisionHullCompileError> {
        if let Some(&index) = self.indices.get(&node) {
            return Ok(index);
        }
        limit("clipnodes", self.records.len() + 1, i16::MAX as usize + 1)?;
        let index = self.records.len() as i16;
        self.records.push(node);
        self.indices.insert(node, index);
        Ok(index)
    }
}

fn build_spatial_hull(
    brushes: &[PreparedHullBrush],
    active: &[usize],
    depth: usize,
    leaf_size: usize,
    planes: &mut Vec<[u8; 14]>,
    nodes: &mut NodeBuilder,
) -> Result<i16, CollisionHullCompileError> {
    if active.len() <= leaf_size || depth == MAX_SPATIAL_DEPTH {
        return build_brush_chain(brushes, active, nodes);
    }
    let Some(split) = choose_spatial_split(brushes, active) else {
        return build_brush_chain(brushes, active, nodes);
    };
    let mut front = Vec::new();
    let mut back = Vec::new();
    partition_brushes(brushes, active, split, &mut front, &mut back);
    if front.len() == active.len() || back.len() == active.len() {
        return build_brush_chain(brushes, active, nodes);
    }

    let mut normal = [0.0; 3];
    normal[split.axis] = 1.0;
    let (record, flipped) = pack_normalized_plane(normal, split.distance)
        .ok_or(CollisionHullCompileError::InvalidPlane(None))?;
    debug_assert!(!flipped, "positive axial spatial plane cannot flip");
    let plane = intern_plane(planes, record)?;
    let front = build_spatial_hull(brushes, &front, depth + 1, leaf_size, planes, nodes)?;
    let back = build_spatial_hull(brushes, &back, depth + 1, leaf_size, planes, nodes)?;
    nodes.intern([plane, front, back])
}

fn build_brush_chain(
    brushes: &[PreparedHullBrush],
    active: &[usize],
    nodes: &mut NodeBuilder,
) -> Result<i16, CollisionHullCompileError> {
    let mut escape = CONTENTS_EMPTY;
    // `active` retains high-to-low precedence. Wrap low first so the
    // strongest contents brush becomes the root, exactly as before.
    for &brush in active.iter().rev() {
        let brush = &brushes[brush];
        let mut inside = brush.contents;
        for &(plane, flipped) in brush.planes.iter().rev() {
            let (front, back) = if flipped {
                (inside, escape)
            } else {
                (escape, inside)
            };
            inside = nodes.intern([plane, front, back])?;
        }
        escape = inside;
    }
    Ok(escape)
}

fn choose_spatial_split(brushes: &[PreparedHullBrush], active: &[usize]) -> Option<SpatialSplit> {
    let mut best: Option<((usize, usize, usize, usize), SpatialSplit)> = None;
    for axis in 0..3 {
        let mut centers: Vec<_> = active
            .iter()
            .map(|&index| (brushes[index].mins[axis] + brushes[index].maxs[axis]) * 0.5)
            .collect();
        centers.sort_by(f64::total_cmp);
        centers.dedup_by(|left, right| (*left - *right).abs() <= HULL_EPSILON);
        if centers.len() < 2 {
            continue;
        }
        for numerator in [1usize, 2, 3] {
            let pivot = centers.len().saturating_mul(numerator) / 4;
            if pivot == 0 || pivot >= centers.len() {
                continue;
            }
            let distance = (centers[pivot - 1] + centers[pivot]) * 0.5;
            let split = SpatialSplit { axis, distance };
            let (front, back) = partition_counts(brushes, active, split);
            let largest = front.max(back);
            if front == 0 || back == 0 || largest == active.len() {
                continue;
            }
            let duplicates = front + back - active.len();
            // A duplicated brush repeats its complete convex plane chain in
            // both leaves. Penalize that more than a modest imbalance so the
            // PS1 clipnode table remains compact as well as shallow.
            let score = (
                largest.saturating_add(duplicates.saturating_mul(4)),
                largest,
                duplicates,
                front.abs_diff(back),
            );
            if best.is_none_or(|(best_score, _)| score < best_score) {
                best = Some((score, split));
            }
        }
    }
    best.map(|(_, split)| split)
}

fn partition_counts(
    brushes: &[PreparedHullBrush],
    active: &[usize],
    split: SpatialSplit,
) -> (usize, usize) {
    let mut front = 0;
    let mut back = 0;
    for &index in active {
        let brush = &brushes[index];
        front += usize::from(brush.maxs[split.axis] + HULL_EPSILON >= split.distance);
        back += usize::from(brush.mins[split.axis] < split.distance - HULL_EPSILON);
    }
    (front, back)
}

fn partition_brushes(
    brushes: &[PreparedHullBrush],
    active: &[usize],
    split: SpatialSplit,
    front: &mut Vec<usize>,
    back: &mut Vec<usize>,
) {
    for &index in active {
        let brush = &brushes[index];
        if brush.maxs[split.axis] + HULL_EPSILON >= split.distance {
            front.push(index);
        }
        if brush.mins[split.axis] < split.distance - HULL_EPSILON {
            back.push(index);
        }
    }
}

fn point_bounds(points: &[[f64; 3]]) -> ([f64; 3], [f64; 3]) {
    let mut mins = [f64::INFINITY; 3];
    let mut maxs = [f64::NEG_INFINITY; 3];
    for point in points {
        for axis in 0..3 {
            mins[axis] = mins[axis].min(point[axis]);
            maxs[axis] = maxs[axis].max(point[axis]);
        }
    }
    (mins, maxs)
}

fn point_hull_planes(brush: &Brush) -> Result<Vec<([u8; 14], bool)>, CollisionHullCompileError> {
    brush
        .faces
        .iter()
        .filter_map(|face| Plane::from_points(face.points))
        .map(|plane| pack_plane(&plane).ok_or(CollisionHullCompileError::InvalidPlane(None)))
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
            Plane::from_points(face.points).ok_or(CollisionHullCompileError::InvalidPlane(None))?;
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
                .ok_or(CollisionHullCompileError::InvalidPlane(None))
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

pub(crate) fn intern_plane(
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

pub(crate) fn limit(
    kind: &'static str,
    count: usize,
    max: usize,
) -> Result<(), CollisionHullCompileError> {
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
    use crate::brush::{BrushContents, BrushFace};
    use psx_bsp::collision::{
        CollisionHull, Trace, TraceScratch, CONTENTS_EMPTY, CONTENTS_LAVA, CONTENTS_SOLID,
        CONTENTS_WATER, Q12_ONE,
    };
    use psx_bsp::{ClipNode, Plane as BspPlane, RecordSlice, Vec3I32};

    #[test]
    fn smaller_leaf_preserves_mixed_contents_and_corner_contacts() {
        let mut brushes = Vec::new();
        for z in 0..9 {
            for x in 0..9 {
                let mut b = Brush::cuboid([x * 128, 0, z * 128], [x * 128 + 48, 64, z * 128 + 48]);
                b.contents = match (x + z) % 3 {
                    0 => BrushContents::Water,
                    1 => BrushContents::Lava,
                    _ => BrushContents::Solid,
                };
                brushes.push(b);
            }
        }
        let mut pool = Brush::cuboid([128, -32, 128], [896, 32, 896]);
        pool.contents = BrushContents::Water;
        brushes.push(pool);
        brushes.push(Brush::cuboid([448, -16, 448], [576, 80, 576]));
        let a = compile_collision_hulls_with_leaf_size(
            &brushes,
            &[CollisionHullBounds::POINT, PLAYER],
            CollisionHullStrategy::SpatialChains,
            48,
        )
        .unwrap();
        let b = compile_collision_hulls_with_leaf_size(
            &brushes,
            &[CollisionHullBounds::POINT, PLAYER],
            CollisionHullStrategy::SpatialChains,
            32,
        )
        .unwrap();
        for h in 0..2 {
            let a = hull(&a, h);
            let b = hull(&b, h);
            for x in 0..9 {
                for z in 0..9 {
                    for d in [-1, 0, 1] {
                        for (start, end) in [
                            (
                                at(x * 128 - 32, 0, z * 128 - 32),
                                at(x * 128 + 80, 0, z * 128 + 80),
                            ),
                            (
                                at(x * 128 + 24, 96, z * 128 + 24),
                                at(x * 128 + 24, -64, z * 128 + 24),
                            ),
                            (
                                at(x * 128 + 24, 0, z * 128 + 24),
                                at(x * 128 + 80, 0, z * 128 + 80),
                            ),
                        ] {
                            let start = Vec3I32 {
                                x: start.x + d,
                                y: start.y + d,
                                z: start.z + d,
                            };
                            assert_eq!(a.point_contents(start), b.point_contents(start));
                            assert_eq!(
                                trace(&a, start, end),
                                trace(&b, start, end),
                                "h{h} x{x} z{z} d{d}"
                            );
                            assert_eq!(
                                trace(&a, end, start),
                                trace(&b, end, start),
                                "reverse h{h} x{x} z{z} d{d}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn construction_interning_checks_unique_capacity_after_lookup() {
        let mut builder = NodeBuilder::default();
        for plane in 0..=i16::MAX {
            assert_eq!(builder.intern([plane, -1, -2]).unwrap(), plane);
        }
        assert_eq!(builder.intern([0, -1, -2]).unwrap(), 0);
        assert!(matches!(
            builder.intern([0, -2, -1]),
            Err(CollisionHullCompileError::LimitExceeded {
                kind: "clipnodes",
                count: 32769,
                max: 32768
            })
        ));
        assert_eq!(builder.records.len(), 32768);
    }

    #[test]
    fn node_interning_preserves_order_contents_and_all_roots() {
        let nodes = [
            [1, -1, -2],
            [1, -1, -2],
            [2, 0, 1],
            [2, 1, 0],
            [1, -2, -1],
            [7, 4, -3],
        ];
        let mut heads = [-1, 2, 3, 5];
        let result = intern_identical_nodes(&nodes, &mut heads);
        assert_eq!(result, [[1, -1, -2], [2, 0, 0], [1, -2, -1], [7, 2, -3]]);
        assert_eq!(heads, [-1, 1, 1, 3]);
        let mut again = [-1, 2, 3, 5];
        assert_eq!(intern_identical_nodes(&nodes, &mut again), result);
        assert_eq!(again, heads);
    }

    #[test]
    fn identical_body_hulls_share_the_complete_tree() {
        let brushes = [Brush::cuboid([0; 3], [64; 3])];
        let one = compile_collision_hulls(&brushes, &[PLAYER]).unwrap();
        let two = compile_collision_hulls(&brushes, &[PLAYER, PLAYER]).unwrap();
        assert_eq!(two.head_nodes, [one.head_nodes[0], one.head_nodes[0]]);
        assert_eq!(two.clipnodes, one.clipnodes);
        assert_eq!(two.planes, one.planes);
    }

    fn hull(compiled: &CompiledCollisionHulls, hull: usize) -> CollisionHull<'_> {
        CollisionHull::new(
            RecordSlice::<BspPlane>::new(&compiled.planes).expect("planes"),
            RecordSlice::<ClipNode>::new(&compiled.clipnodes).expect("nodes"),
            compiled.head_nodes[hull],
        )
        .expect("aligned collision records")
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
        assert!(hull.trace_into(&start, &end, &mut TraceScratch::new(), &mut output,));
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
        let point = trace(&hull(&compiled, 0), at(0, 64, 64), at(200, 64, 64));
        let player = trace(&hull(&compiled, 1), at(0, 64, 64), at(200, 64, 64));
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
    fn spatial_hull_matches_linear_reference_on_a_large_brush_grid() {
        let mut brushes = Vec::new();
        for z in 0..9 {
            for x in 0..9 {
                let min = [x * 128, 0, z * 128];
                brushes.push(Brush::cuboid(min, [min[0] + 48, 64, min[2] + 48]));
            }
        }
        let spatial = compile_collision_hulls_inner(
            &brushes,
            &[PLAYER],
            CollisionHullStrategy::SpatialChains,
        )
        .expect("spatial");
        let linear =
            compile_collision_hulls_inner(&brushes, &[PLAYER], CollisionHullStrategy::LinearChains)
                .expect("linear");
        let spatial = hull(&spatial, 0);
        let linear = hull(&linear, 0);

        for z in 0..9 {
            for x in 0..9 {
                for point in [
                    at(x * 128 + 24, 32, z * 128 + 24),
                    at(x * 128 + 88, 32, z * 128 + 88),
                ] {
                    assert_eq!(
                        spatial.point_contents(point),
                        linear.point_contents(point),
                        "contents mismatch in cell ({x}, {z})"
                    );
                }
            }
        }
        for row in [0, 4, 8] {
            for column in [0, 4, 8] {
                let z = row * 128 + 24;
                let start = at(column * 128 - 48, 32, z);
                let end = at(column * 128 + 32, 32, z);
                assert_eq!(
                    trace(&spatial, start, end),
                    trace(&linear, start, end),
                    "trace mismatch at ({column}, {row})"
                );
            }
        }
    }

    #[test]
    fn every_hull_preserves_nonblocking_liquid_contents_and_solid_precedence() {
        let mut water = Brush::cuboid([0, 0, 0], [512, 256, 512]);
        water.contents = BrushContents::Water;
        let mut lava = Brush::cuboid([64, 0, 64], [448, 256, 448]);
        lava.contents = BrushContents::Lava;
        let solid = Brush::cuboid([192, 0, 192], [320, 256, 320]);
        let compiled =
            compile_collision_hulls(&[water, lava, solid], &[CollisionHullBounds::POINT, PLAYER])
                .expect("cook");
        for hull_index in 0..2 {
            let hull = hull(&compiled, hull_index);
            assert_eq!(
                hull.point_contents(at(-128, 128, -128)),
                Some(CONTENTS_EMPTY)
            );
            assert_eq!(hull.point_contents(at(32, 128, 32)), Some(CONTENTS_WATER));
            assert_eq!(hull.point_contents(at(128, 128, 128)), Some(CONTENTS_LAVA));
            assert_eq!(hull.point_contents(at(256, 128, 256)), Some(CONTENTS_SOLID));
            let crossed = trace(&hull, at(-128, 128, 32), at(160, 128, 32));
            assert_eq!(crossed.fraction, Q12_ONE);
            assert!(crossed.in_water.is_set());
        }
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
