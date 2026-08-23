//! Portal generation and leaf classification for compiled brush BSPs.

use crate::brush::{Brush, Plane, BASE_WINDING_EXTENT};
use crate::brush_compile::{
    normalized_plane, split_polygon, BspChild, BspLeafContents, CompiledSurfaceBsp, PolygonSplit,
};

const PORTAL_NUDGE: f64 = 1.0 / 1024.0;
/// Original QBSP `SIDESPACE`: padding between brush bounds and the six
/// headnode portals that face the global outside node.
const HEADNODE_SIDE_SPACE: f64 = 24.0;

/// One convex opening shared by two terminal BSP cells.
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledPortal {
    /// Splitter plane, oriented from `back_leaf` toward `front_leaf`.
    pub plane: Plane,
    pub front_leaf: usize,
    pub back_leaf: usize,
    pub vertices: Vec<[f64; 3]>,
}

#[derive(Clone, Copy)]
struct Halfspace {
    plane: Plane,
    keep_front: bool,
}

/// Generate exact leaf-to-leaf portal fragments for a surface BSP.
pub fn portalize_surface_bsp(bsp: &CompiledSurfaceBsp) -> Vec<CompiledPortal> {
    let mut portals = Vec::new();
    let mut constraints = headnode_constraints(bsp);
    portalize_branch(bsp.root, bsp, &mut constraints, &mut portals);
    portals
}

fn headnode_bounds(bsp: &CompiledSurfaceBsp) -> Option<([f64; 3], [f64; 3])> {
    let mut minimum = [f64::INFINITY; 3];
    let mut maximum = [f64::NEG_INFINITY; 3];
    for vertex in bsp.surfaces.iter().flat_map(|surface| &surface.vertices) {
        for axis in 0..3 {
            minimum[axis] = minimum[axis].min(vertex[axis]);
            maximum[axis] = maximum[axis].max(vertex[axis]);
        }
    }
    if minimum
        .into_iter()
        .chain(maximum)
        .any(|value| !value.is_finite())
    {
        return None;
    }
    Some((
        minimum.map(|value| value.floor() - HEADNODE_SIDE_SPACE),
        maximum.map(|value| value.ceil() + HEADNODE_SIDE_SPACE),
    ))
}

fn headnode_constraints(bsp: &CompiledSurfaceBsp) -> Vec<Halfspace> {
    let Some((minimum, maximum)) = headnode_bounds(bsp) else {
        return Vec::new();
    };
    let mut constraints = Vec::with_capacity(6);
    for axis in 0..3 {
        let mut normal = [0; 3];
        normal[axis] = 1;
        constraints.push(Halfspace {
            plane: Plane {
                normal,
                dist: minimum[axis] as i64,
            },
            keep_front: true,
        });
        constraints.push(Halfspace {
            plane: Plane {
                normal,
                dist: maximum[axis] as i64,
            },
            keep_front: false,
        });
    }
    constraints
}

fn portalize_branch(
    child: BspChild,
    bsp: &CompiledSurfaceBsp,
    constraints: &mut Vec<Halfspace>,
    portals: &mut Vec<CompiledPortal>,
) {
    let BspChild::Node(node_index) = child else {
        return;
    };
    let node = &bsp.nodes[node_index];
    let winding = constraints
        .iter()
        .fold(base_plane_winding(node.plane), |winding, constraint| {
            clip_to_halfspace(winding, *constraint)
        });
    if winding.len() >= 3 {
        route_portal(winding, node.plane, node.front, node.back, bsp, portals);
    }

    constraints.push(Halfspace {
        plane: node.plane,
        keep_front: true,
    });
    portalize_branch(node.front, bsp, constraints, portals);
    constraints.pop();

    constraints.push(Halfspace {
        plane: node.plane,
        keep_front: false,
    });
    portalize_branch(node.back, bsp, constraints, portals);
    constraints.pop();
}

fn route_portal(
    vertices: Vec<[f64; 3]>,
    portal_plane: Plane,
    front: BspChild,
    back: BspChild,
    bsp: &CompiledSurfaceBsp,
    portals: &mut Vec<CompiledPortal>,
) {
    if vertices.len() < 3 {
        return;
    }
    match (front, back) {
        (BspChild::Leaf(front_leaf), BspChild::Leaf(back_leaf)) => {
            if front_leaf != back_leaf {
                portals.push(CompiledPortal {
                    plane: portal_plane,
                    front_leaf,
                    back_leaf,
                    vertices,
                });
            }
        }
        (BspChild::Node(node_index), back) => {
            let node = &bsp.nodes[node_index];
            route_split_front(vertices, portal_plane, node, back, bsp, portals);
        }
        (front, BspChild::Node(node_index)) => {
            let node = &bsp.nodes[node_index];
            route_split_back(vertices, portal_plane, front, node, bsp, portals);
        }
    }
}

fn route_split_front(
    vertices: Vec<[f64; 3]>,
    portal_plane: Plane,
    node: &crate::brush_compile::CompiledBspNode,
    back: BspChild,
    bsp: &CompiledSurfaceBsp,
    portals: &mut Vec<CompiledPortal>,
) {
    match split_polygon(&vertices, node.plane) {
        PolygonSplit::Front(vertices) => {
            route_portal(vertices, portal_plane, node.front, back, bsp, portals)
        }
        PolygonSplit::Back(vertices) => {
            route_portal(vertices, portal_plane, node.back, back, bsp, portals)
        }
        PolygonSplit::Split { front, back: rear } => {
            route_portal(front, portal_plane, node.front, back, bsp, portals);
            route_portal(rear, portal_plane, node.back, back, bsp, portals);
        }
        PolygonSplit::Coplanar => {
            debug_assert!(false, "descendant splitter duplicates a portal plane");
        }
    }
}

fn route_split_back(
    vertices: Vec<[f64; 3]>,
    portal_plane: Plane,
    front: BspChild,
    node: &crate::brush_compile::CompiledBspNode,
    bsp: &CompiledSurfaceBsp,
    portals: &mut Vec<CompiledPortal>,
) {
    match split_polygon(&vertices, node.plane) {
        PolygonSplit::Front(vertices) => {
            route_portal(vertices, portal_plane, front, node.front, bsp, portals)
        }
        PolygonSplit::Back(vertices) => {
            route_portal(vertices, portal_plane, front, node.back, bsp, portals)
        }
        PolygonSplit::Split { front: ahead, back } => {
            route_portal(ahead, portal_plane, front, node.front, bsp, portals);
            route_portal(back, portal_plane, front, node.back, bsp, portals);
        }
        PolygonSplit::Coplanar => {
            debug_assert!(false, "descendant splitter duplicates a portal plane");
        }
    }
}

fn clip_to_halfspace(vertices: Vec<[f64; 3]>, constraint: Halfspace) -> Vec<[f64; 3]> {
    match split_polygon(&vertices, constraint.plane) {
        PolygonSplit::Front(vertices) if constraint.keep_front => vertices,
        PolygonSplit::Back(vertices) if !constraint.keep_front => vertices,
        PolygonSplit::Split { front, back } => {
            if constraint.keep_front {
                front
            } else {
                back
            }
        }
        PolygonSplit::Coplanar => vertices,
        PolygonSplit::Front(_) | PolygonSplit::Back(_) => Vec::new(),
    }
}

fn base_plane_winding(plane: Plane) -> Vec<[f64; 3]> {
    let (normal, distance) = normalized_plane(plane);
    let mut axis = 0;
    for candidate in 1..3 {
        if normal[candidate].abs() < normal[axis].abs() {
            axis = candidate;
        }
    }
    let mut seed = [0.0; 3];
    seed[axis] = 1.0;
    let right = scaled_to(cross(normal, seed), BASE_WINDING_EXTENT);
    let up = scaled_to(cross(right, normal), BASE_WINDING_EXTENT);
    let center = scale(normal, distance);
    vec![
        add(center, add(negate(right), negate(up))),
        add(center, add(right, negate(up))),
        add(center, add(right, up)),
        add(center, add(negate(right), up)),
    ]
}

/// Classify every terminal BSP cell against the union of valid brushes.
pub fn classify_bsp_leaves(
    bsp: &mut CompiledSurfaceBsp,
    portals: &[CompiledPortal],
    brushes: &[Brush],
) {
    let brush_planes: Vec<(crate::brush::BrushContents, Vec<Plane>)> = brushes
        .iter()
        .filter(|brush| brush.solve().is_valid())
        .map(|brush| {
            (
                brush.contents,
                brush
                    .faces
                    .iter()
                    .filter_map(|face| Plane::from_points(face.points))
                    .collect(),
            )
        })
        .collect();

    for leaf_index in 0..bsp.leaves.len() {
        let sample = leaf_sample(leaf_index, portals).unwrap_or([0.0; 3]);
        let contents = brush_planes
            .iter()
            .filter(|(_, planes)| {
                planes.iter().all(|plane| {
                    let (normal, distance) = normalized_plane(*plane);
                    dot(normal, sample) <= distance
                })
            })
            .max_by_key(|(contents, _)| contents.precedence())
            .map(|(contents, _)| BspLeafContents::from_brush(*contents));
        bsp.leaves[leaf_index].contents = contents.unwrap_or(BspLeafContents::Empty);
    }
}

/// Quake QBSP outside fill: preserve the portal-connected cells occupied by
/// authored entities, then turn the unreachable infinite exterior into solid.
///
#[derive(Clone, Debug, PartialEq)]
pub enum OutsideFillResult {
    Filled(usize),
    /// Portal-centroid pointfile from one occupant to the infinite exterior.
    Leaked(CompiledLeakPath),
    NoOccupants,
}

/// Exact route from an occupied leaf to the infinite exterior.
///
/// `portal_indices` is parallel to the interior points in `points`: portal
/// zero owns `points[1]`, portal one owns `points[2]`, and so on. Retaining
/// this topology lets editor diagnostics identify and outline a likely breach
/// instead of presenting every pointfile segment with equal visual weight.
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledLeakPath {
    pub points: Vec<[f64; 3]>,
    pub portal_indices: Vec<usize>,
}

pub fn fill_outside_bsp_leaves(
    bsp: &mut CompiledSurfaceBsp,
    portals: &[CompiledPortal],
    occupant_points: &[[f64; 3]],
) -> OutsideFillResult {
    let occupants: Vec<_> = occupant_points
        .iter()
        .enumerate()
        .map(|(point_index, &point)| (point_index, point_leaf_index(bsp, point)))
        .filter(|&(_, leaf)| bsp.leaves[leaf].contents.is_visible())
        .collect();
    if occupants.is_empty() {
        return OutsideFillResult::NoOccupants;
    }
    let occupant_leaves: Vec<_> = occupants.iter().map(|&(_, leaf)| leaf).collect();

    let mut adjacency = vec![Vec::new(); bsp.leaves.len()];
    for (portal_index, portal) in portals.iter().enumerate() {
        if bsp.leaves[portal.front_leaf].contents.is_visible()
            && bsp.leaves[portal.back_leaf].contents.is_visible()
        {
            adjacency[portal.front_leaf].push((portal.back_leaf, portal_index));
            adjacency[portal.back_leaf].push((portal.front_leaf, portal_index));
        }
    }
    let occupied = flood_leaves(&adjacency, &occupant_leaves);

    let outside_leaf_points = headnode_outside_leaf_points(bsp);
    let exterior_leaves: Vec<_> = outside_leaf_points
        .iter()
        .filter_map(|&(leaf, _)| bsp.leaves[leaf].contents.is_visible().then_some(leaf))
        .collect();
    if exterior_leaves.is_empty() {
        return OutsideFillResult::NoOccupants;
    }
    if let Some((exterior_leaf, exterior_point)) = outside_leaf_points
        .iter()
        .copied()
        .find(|(leaf, _)| occupied[*leaf] && bsp.leaves[*leaf].contents.is_visible())
    {
        let path = leak_path(
            &adjacency,
            portals,
            &occupants,
            occupant_points,
            exterior_leaf,
            exterior_point,
        );
        return OutsideFillResult::Leaked(path);
    }

    let exterior = flood_leaves(&adjacency, &exterior_leaves);
    let mut filled = 0;
    for (leaf, is_exterior) in bsp.leaves.iter_mut().zip(exterior) {
        if is_exterior && leaf.contents.is_visible() {
            leaf.contents = BspLeafContents::Solid;
            filled += 1;
        }
    }
    OutsideFillResult::Filled(filled)
}

/// Route the six padded headnode faces through the BSP and retain one point on
/// every terminal leaf they touch. In original QBSP these are actual portals
/// linked to `outside_node`; the compact host representation keeps the same
/// boundary seeds without adding a fake runtime leaf.
fn headnode_outside_leaf_points(bsp: &CompiledSurfaceBsp) -> Vec<(usize, [f64; 3])> {
    let Some((minimum, maximum)) = headnode_bounds(bsp) else {
        return Vec::new();
    };
    let mut output = Vec::new();
    for axis in 0..3 {
        let other = [(axis + 1) % 3, (axis + 2) % 3];
        for side in 0..2 {
            let mut polygon = Vec::with_capacity(4);
            for [a, b] in [[0, 0], [1, 0], [1, 1], [0, 1]] {
                let mut point = [0.0; 3];
                point[axis] = if side == 0 {
                    minimum[axis]
                } else {
                    maximum[axis]
                };
                point[other[0]] = if a == 0 {
                    minimum[other[0]]
                } else {
                    maximum[other[0]]
                };
                point[other[1]] = if b == 0 {
                    minimum[other[1]]
                } else {
                    maximum[other[1]]
                };
                polygon.push(point);
            }
            route_headnode_face(bsp.root, polygon, bsp, &mut output);
        }
    }
    output
}

fn route_headnode_face(
    child: BspChild,
    polygon: Vec<[f64; 3]>,
    bsp: &CompiledSurfaceBsp,
    output: &mut Vec<(usize, [f64; 3])>,
) {
    if polygon.len() < 3 {
        return;
    }
    match child {
        BspChild::Leaf(leaf) => {
            if output.iter().any(|&(known, _)| known == leaf) {
                return;
            }
            let count = polygon.len() as f64;
            output.push((
                leaf,
                scale(polygon.into_iter().fold([0.0; 3], add), 1.0 / count),
            ));
        }
        BspChild::Node(index) => {
            let node = &bsp.nodes[index];
            match split_polygon(&polygon, node.plane) {
                PolygonSplit::Front(front) => route_headnode_face(node.front, front, bsp, output),
                PolygonSplit::Back(back) => route_headnode_face(node.back, back, bsp, output),
                PolygonSplit::Split { front, back } => {
                    route_headnode_face(node.front, front, bsp, output);
                    route_headnode_face(node.back, back, bsp, output);
                }
                PolygonSplit::Coplanar => {
                    route_headnode_face(node.front, polygon.clone(), bsp, output);
                    route_headnode_face(node.back, polygon, bsp, output);
                }
            }
        }
    }
}

fn flood_leaves(adjacency: &[Vec<(usize, usize)>], starts: &[usize]) -> Vec<bool> {
    let mut reached = vec![false; adjacency.len()];
    let mut pending = std::collections::VecDeque::new();
    for &leaf in starts {
        if !reached[leaf] {
            reached[leaf] = true;
            pending.push_back(leaf);
        }
    }
    while let Some(leaf) = pending.pop_front() {
        for &(adjacent, _) in &adjacency[leaf] {
            if !reached[adjacent] {
                reached[adjacent] = true;
                pending.push_back(adjacent);
            }
        }
    }
    reached
}

/// Reconstruct Quake's pointfile shape: entity point, one centroid per portal
/// crossed, then a point known to live in the infinite outside leaf.
fn leak_path(
    adjacency: &[Vec<(usize, usize)>],
    portals: &[CompiledPortal],
    occupants: &[(usize, usize)],
    occupant_points: &[[f64; 3]],
    exterior_leaf: usize,
    exterior_point: [f64; 3],
) -> CompiledLeakPath {
    let mut parent = vec![None; adjacency.len()];
    let mut root = vec![None; adjacency.len()];
    let mut pending = std::collections::VecDeque::new();
    for &(occupant_index, leaf) in occupants {
        if root[leaf].is_none() {
            root[leaf] = Some(occupant_index);
            pending.push_back(leaf);
        }
    }
    while let Some(leaf) = pending.pop_front() {
        if leaf == exterior_leaf {
            break;
        }
        for &(adjacent, portal) in &adjacency[leaf] {
            if root[adjacent].is_none() {
                root[adjacent] = root[leaf];
                parent[adjacent] = Some((leaf, portal));
                pending.push_back(adjacent);
            }
        }
    }

    let Some(occupant_index) = root[exterior_leaf] else {
        return CompiledLeakPath {
            points: vec![exterior_point],
            portal_indices: Vec::new(),
        };
    };
    let mut portal_path = Vec::new();
    let mut leaf = exterior_leaf;
    while let Some((previous, portal)) = parent[leaf] {
        portal_path.push(portal);
        leaf = previous;
    }
    portal_path.reverse();

    let mut path = Vec::with_capacity(portal_path.len() + 2);
    let mut crossed_portals = Vec::with_capacity(portal_path.len());
    path.push(occupant_points[occupant_index]);
    for portal in portal_path {
        let vertices = &portals[portal].vertices;
        if !vertices.is_empty() {
            crossed_portals.push(portal);
            path.push(scale(
                vertices.iter().copied().fold([0.0; 3], add),
                1.0 / vertices.len() as f64,
            ));
        }
    }
    path.push(exterior_point);
    CompiledLeakPath {
        points: path,
        portal_indices: crossed_portals,
    }
}

fn leaf_sample(leaf_index: usize, portals: &[CompiledPortal]) -> Option<[f64; 3]> {
    let portal = portals
        .iter()
        .find(|portal| portal.front_leaf == leaf_index || portal.back_leaf == leaf_index)?;
    let inverse_count = 1.0 / portal.vertices.len() as f64;
    let centroid = portal.vertices.iter().copied().fold([0.0; 3], add);
    let centroid = scale(centroid, inverse_count);
    let (normal, _) = normalized_plane(portal.plane);
    let direction = if portal.front_leaf == leaf_index {
        PORTAL_NUDGE
    } else {
        -PORTAL_NUDGE
    };
    Some(add(centroid, scale(normal, direction)))
}

/// Descend a host-side compiled BSP to its terminal leaf.
pub fn point_leaf_index(bsp: &CompiledSurfaceBsp, point: [f64; 3]) -> usize {
    let mut child = bsp.root;
    loop {
        match child {
            BspChild::Leaf(index) => return index,
            BspChild::Node(index) => {
                let node = &bsp.nodes[index];
                let (normal, distance) = normalized_plane(node.plane);
                child = if dot(normal, point) > distance {
                    node.front
                } else {
                    node.back
                };
            }
        }
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

fn add(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
}

fn negate(value: [f64; 3]) -> [f64; 3] {
    [-value[0], -value[1], -value[2]]
}

fn scale(value: [f64; 3], amount: f64) -> [f64; 3] {
    [value[0] * amount, value[1] * amount, value[2] * amount]
}

fn scaled_to(value: [f64; 3], length: f64) -> [f64; 3] {
    scale(value, length / dot(value, value).sqrt())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::brush_compile::{build_surface_bsp, compile_csg_surfaces};

    fn compiled(brushes: &[Brush]) -> (CompiledSurfaceBsp, Vec<CompiledPortal>) {
        let surfaces = compile_csg_surfaces(brushes);
        let mut bsp = build_surface_bsp(&surfaces);
        let portals = portalize_surface_bsp(&bsp);
        classify_bsp_leaves(&mut bsp, &portals, brushes);
        (bsp, portals)
    }

    fn empty_path_exists(
        bsp: &CompiledSurfaceBsp,
        portals: &[CompiledPortal],
        start: usize,
        target: usize,
    ) -> bool {
        let mut reached = vec![false; bsp.leaves.len()];
        let mut pending = VecDeque::from([start]);
        reached[start] = true;
        while let Some(leaf) = pending.pop_front() {
            if leaf == target {
                return true;
            }
            for portal in portals {
                let adjacent = if portal.front_leaf == leaf {
                    Some(portal.back_leaf)
                } else if portal.back_leaf == leaf {
                    Some(portal.front_leaf)
                } else {
                    None
                };
                let Some(adjacent) = adjacent else {
                    continue;
                };
                if !reached[adjacent] && bsp.leaves[adjacent].contents == BspLeafContents::Empty {
                    reached[adjacent] = true;
                    pending.push_back(adjacent);
                }
            }
        }
        false
    }

    #[test]
    fn cuboid_portals_are_convex_leaf_connections() {
        let (bsp, portals) = compiled(&[Brush::cuboid([0, 0, 0], [128, 64, 256])]);
        assert!(!portals.is_empty());
        for portal in &portals {
            assert_ne!(portal.front_leaf, portal.back_leaf);
            assert!(portal.front_leaf < bsp.leaves.len());
            assert!(portal.back_leaf < bsp.leaves.len());
            assert!(portal.vertices.len() >= 3);
            assert!(portal.vertices.iter().all(|vertex| {
                let (normal, distance) = normalized_plane(portal.plane);
                (dot(normal, *vertex) - distance).abs() <= PORTAL_NUDGE
            }));
        }
    }

    #[test]
    fn cuboid_classifies_one_solid_terminal_cell() {
        let (bsp, _) = compiled(&[Brush::cuboid([0, 0, 0], [128, 64, 256])]);
        assert_eq!(
            bsp.leaves
                .iter()
                .filter(|leaf| leaf.contents == BspLeafContents::Solid)
                .count(),
            1
        );
        assert_eq!(
            bsp.leaves[point_leaf_index(&bsp, [64.0, 32.0, 128.0])].contents,
            BspLeafContents::Solid
        );
        assert_eq!(
            bsp.leaves[point_leaf_index(&bsp, [-64.0, 32.0, 128.0])].contents,
            BspLeafContents::Empty
        );
    }

    #[test]
    fn sealed_hollow_room_separates_cavity_wall_and_exterior() {
        let brushes = Brush::cuboid([0, 0, 0], [1024, 512, 1024])
            .hollow(64)
            .expect("hollow room");
        let (bsp, portals) = compiled(&brushes);
        let cavity = point_leaf_index(&bsp, [512.0, 256.0, 512.0]);
        let wall = point_leaf_index(&bsp, [512.0, 32.0, 512.0]);
        let exterior = point_leaf_index(&bsp, [-128.0, 256.0, 512.0]);
        assert_eq!(bsp.leaves[cavity].contents, BspLeafContents::Empty);
        assert_eq!(bsp.leaves[wall].contents, BspLeafContents::Solid);
        assert_eq!(bsp.leaves[exterior].contents, BspLeafContents::Empty);
        assert!(!empty_path_exists(&bsp, &portals, cavity, exterior));
    }

    #[test]
    fn outside_fill_solids_only_the_unoccupied_exterior() {
        let brushes = Brush::cuboid([0, 0, 0], [1024, 512, 1024])
            .hollow(64)
            .expect("hollow room");
        let (mut bsp, portals) = compiled(&brushes);
        let cavity = point_leaf_index(&bsp, [512.0, 256.0, 512.0]);
        let exterior = point_leaf_index(&bsp, [-128.0, 256.0, 512.0]);

        let OutsideFillResult::Filled(filled) =
            fill_outside_bsp_leaves(&mut bsp, &portals, &[[512.0, 256.0, 512.0]])
        else {
            panic!("sealed room must fill");
        };

        assert!(filled > 0);
        assert_eq!(bsp.leaves[cavity].contents, BspLeafContents::Empty);
        assert_eq!(bsp.leaves[exterior].contents, BspLeafContents::Solid);
    }

    #[test]
    fn outside_fill_preserves_a_leaking_world_for_diagnostics() {
        let mut brushes = Brush::cuboid([0, 0, 0], [1024, 512, 1024])
            .hollow(64)
            .expect("hollow room");
        brushes.pop().expect("remove one wall");
        let (mut bsp, portals) = compiled(&brushes);
        let contents_before: Vec<_> = bsp.leaves.iter().map(|leaf| leaf.contents).collect();

        let OutsideFillResult::Leaked(leak) =
            fill_outside_bsp_leaves(&mut bsp, &portals, &[[512.0, 256.0, 512.0]])
        else {
            panic!("open room must report its pointfile");
        };
        let path = &leak.points;
        assert!(path.len() >= 3, "entity, portal and exterior points");
        assert_eq!(
            leak.portal_indices.len() + 2,
            path.len(),
            "each interior point must retain the exact crossed portal"
        );
        assert_eq!(path[0], [512.0, 256.0, 512.0]);
        let exterior = path.last().expect("exterior");
        assert!(
            [
                exterior[0] + 24.0,
                exterior[0] - 1048.0,
                exterior[1] + 24.0,
                exterior[1] - 536.0,
                exterior[2] + 24.0,
                exterior[2] - 1048.0,
            ]
            .into_iter()
            .any(|distance| distance.abs() < 1.0e-6),
            "pointfile must end on one padded headnode face: {exterior:?}"
        );
        assert_eq!(
            bsp.leaves
                .iter()
                .map(|leaf| leaf.contents)
                .collect::<Vec<_>>(),
            contents_before
        );
    }

    #[test]
    fn separated_solids_classify_independently() {
        let brushes = [
            Brush::cuboid([0, 0, 0], [128, 128, 128]),
            Brush::cuboid([512, 0, 0], [640, 128, 128]),
        ];
        let (bsp, _) = compiled(&brushes);
        for point in [[64.0, 64.0, 64.0], [576.0, 64.0, 64.0]] {
            assert_eq!(
                bsp.leaves[point_leaf_index(&bsp, point)].contents,
                BspLeafContents::Solid
            );
        }
        assert_eq!(
            bsp.leaves[point_leaf_index(&bsp, [320.0, 64.0, 64.0])].contents,
            BspLeafContents::Empty
        );
    }
}
