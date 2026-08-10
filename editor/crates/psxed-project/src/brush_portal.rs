//! Portal generation and leaf classification for compiled brush BSPs.

use crate::brush::{BASE_WINDING_EXTENT, Brush, Plane};
use crate::brush_compile::{
    BspChild, BspLeafContents, CompiledSurfaceBsp, PolygonSplit, normalized_plane, split_polygon,
};

const PORTAL_NUDGE: f64 = 1.0 / 1024.0;

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
    let mut constraints = Vec::new();
    portalize_branch(bsp.root, bsp, &mut constraints, &mut portals);
    portals
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
    let brush_planes: Vec<Vec<Plane>> = brushes
        .iter()
        .filter(|brush| brush.solve().is_valid())
        .map(|brush| {
            brush
                .faces
                .iter()
                .filter_map(|face| Plane::from_points(face.points))
                .collect()
        })
        .collect();

    for leaf_index in 0..bsp.leaves.len() {
        let sample = leaf_sample(leaf_index, portals).unwrap_or([0.0; 3]);
        let solid = brush_planes.iter().any(|planes| {
            planes.iter().all(|plane| {
                let (normal, distance) = normalized_plane(*plane);
                dot(normal, sample) <= distance
            })
        });
        bsp.leaves[leaf_index].contents = if solid {
            BspLeafContents::Solid
        } else {
            BspLeafContents::Empty
        };
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
