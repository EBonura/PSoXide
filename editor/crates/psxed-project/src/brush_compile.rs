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

use crate::brush::{Brush, BrushContents, FaceUv, Plane};
use crate::ResourceId;
use psx_bsp::CookedRecord;
use std::collections::HashSet;

/// Fixed-point scale shared with the runtime: positions and plane
/// distances are Q20.12, normals Q3.12.
pub const Q12_ONE: i32 = 4096;

/// Packed collision BSP ready for `psx_bsp` record slices.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompiledCollision {
    /// Packed compact BSP plane records.
    pub planes: Vec<u8>,
    /// Packed 6-byte XBSP clipnode records.
    pub clipnodes: Vec<u8>,
    /// Root clipnode index, or a contents value when no brush compiled.
    pub head_node: i16,
}

const CONTENTS_EMPTY: i16 = psx_bsp::collision::CONTENTS_EMPTY;
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
    /// Contents of the authored volume that owns this boundary.
    pub contents: BrushContents,
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
    Water,
    Slime,
    Lava,
}

impl BspLeafContents {
    pub const fn from_brush(contents: BrushContents) -> Self {
        match contents {
            BrushContents::Solid => Self::Solid,
            BrushContents::Water => Self::Water,
            BrushContents::Slime => Self::Slime,
            BrushContents::Lava => Self::Lava,
        }
    }

    pub const fn runtime_contents(self) -> i16 {
        match self {
            Self::Unclassified | Self::Empty => psx_bsp::collision::CONTENTS_EMPTY,
            Self::Solid => psx_bsp::collision::CONTENTS_SOLID,
            Self::Water => psx_bsp::collision::CONTENTS_WATER,
            Self::Slime => psx_bsp::collision::CONTENTS_SLIME,
            Self::Lava => psx_bsp::collision::CONTENTS_LAVA,
        }
    }

    pub const fn is_visible(self) -> bool {
        !matches!(self, Self::Solid | Self::Unclassified)
    }
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

/// qbsp-parity hard cap on face extent, world units: every cooked
/// surface subdivides so no piece spans more than this along any axis,
/// with or without lights (Quake's qbsp splits every face to its
/// lightmap extents the same way; small faces are also its near-plane
/// safety, since an eye-plane-crossing sliver saturates past the GPU's
/// 1023x511 draw limit and skips instead of wrapping). 2048 here is
/// measured, not aesthetic: PSoXide content runs ~18x Quake's unit
/// scale, so this is already finer than Quake's ~272 units scaled
/// proportionally, and the sanctum's worst PVS leaf sits at 2097
/// surfaces / 4198 packets against the 4096-packet arena. 1024 cooks
/// but overflows the arena (7472); 256 cooks only thanks to exact
/// leaf marks and then wants 37918 packets, which no runtime budget
/// survives. Tighten only together with a packet/RAM plan.
pub const SURFACE_EXTENT_UNITS: f64 = 2048.0;

/// Large host-side branches divide exact splitter scoring across CPU cores.
/// Smaller descendants stay single-threaded so thread setup never dominates
/// ordinary editor rooms.
const PARALLEL_SPLITTER_SURFACES: usize = 2048;

/// Split every surface into patches no wider than `max_extent` on any
/// world axis, unconditionally: [`subdivide_surfaces_for_lighting`]
/// with a light gate that always passes.
pub fn subdivide_surfaces_to_extent(
    surfaces: Vec<CompiledSurface>,
    max_extent: f64,
) -> Vec<CompiledSurface> {
    subdivide_surfaces_for_lighting(surfaces, max_extent, &[([0.0; 3], f64::INFINITY)])
}

/// Fit PS1-safe render patches into a packed face budget.
///
/// Small levels keep `fine_extent` everywhere. For a full resident level, the
/// cap doubles uniformly until the packed face budget is met. Lighting still
/// bakes on every retained vertex; its spatial resolution coarsens together
/// with geometry instead of silently making a level impossible to link.
pub fn subdivide_surfaces_to_budget(
    surfaces: &[CompiledSurface],
    fine_extent: f64,
    max_faces: usize,
    _light_spheres: &[([f64; 3], f64)],
) -> Vec<CompiledSurface> {
    let mut coarse_extent = fine_extent;
    loop {
        let patches = subdivide_surfaces_to_extent(surfaces.to_vec(), coarse_extent);
        let is_unsplit = patches.len() == surfaces.len();
        if patches.len() <= max_faces || is_unsplit {
            return patches;
        }
        coarse_extent *= 2.0;
    }
}

/// Replace partition fragments with the final authored render surfaces.
///
/// The BSP builder may split one polygon many times to derive exact convex
/// leaves and portals. Those temporary fragments describe visibility space;
/// serializing them as independent draw faces duplicates the same authored
/// surface across branches and makes full levels exceed the packed face cap.
/// Render surfaces instead stay single-owner records referenced by every leaf
/// they touch. Nodes deliberately own no faces in this layout, so the runtime
/// selects the already-deduplicated PVS face chain.
pub fn replace_bsp_render_surfaces(bsp: &mut CompiledSurfaceBsp, surfaces: Vec<CompiledSurface>) {
    for node in &mut bsp.nodes {
        node.first_surface = 0;
        node.surface_count = 0;
    }
    for leaf in &mut bsp.leaves {
        leaf.mark_surfaces.clear();
    }
    let mut retained = Vec::with_capacity(surfaces.len());
    for surface in surfaces {
        let mut reached_leaves = Vec::new();
        let mut stack = vec![(bsp.root, surface.vertices.clone())];
        while let Some((child, polygon)) = stack.pop() {
            match child {
                BspChild::Leaf(leaf) => {
                    if bsp.leaves[leaf].contents.is_visible() && !reached_leaves.contains(&leaf) {
                        reached_leaves.push(leaf);
                    }
                }
                BspChild::Node(index) => {
                    let node = &bsp.nodes[index];
                    match split_polygon(&polygon, node.plane) {
                        PolygonSplit::Front(vertices) => stack.push((node.front, vertices)),
                        PolygonSplit::Back(vertices) => stack.push((node.back, vertices)),
                        PolygonSplit::Coplanar => {
                            stack.push((node.front, polygon.clone()));
                            stack.push((node.back, polygon));
                        }
                        PolygonSplit::Split { front, back } => {
                            stack.push((node.front, front));
                            stack.push((node.back, back));
                        }
                    }
                }
            }
        }
        if reached_leaves.is_empty() {
            continue;
        }
        let surface_index = retained.len();
        retained.push(surface);
        for leaf in reached_leaves {
            bsp.leaves[leaf].mark_surfaces.push(surface_index);
        }
    }
    bsp.surfaces = retained;
}

/// Split every surface into patches no wider than `max_extent` along
/// any world axis, cutting on grid-aligned world planes so adjacent
/// patches share exact edges (no cracks between sibling patches).
/// Piece metadata (plane, material, UV, contents, provenance) carries
/// over untouched; the cuts never move the surface's plane, so texture
/// mappings are unaffected.
///
/// `light_spheres` are `(position, radius)` in world units. A piece is
/// only split while it is within range of at least one light: past every
/// radius the attenuation is zero and every vertex bakes flat ambient,
/// so extra vertices there change nothing. Detail therefore concentrates
/// around lights instead of exploding the whole map past the packer's
/// mark-surface limits.
pub fn subdivide_surfaces_for_lighting(
    surfaces: Vec<CompiledSurface>,
    max_extent: f64,
    light_spheres: &[([f64; 3], f64)],
) -> Vec<CompiledSurface> {
    let mut out = Vec::with_capacity(surfaces.len());
    for surface in surfaces {
        let pieces =
            subdivide_polygon_for_lighting(surface.vertices.clone(), max_extent, light_spheres);
        for vertices in pieces {
            let mut piece = surface.clone();
            piece.vertices = vertices;
            out.push(piece);
        }
    }
    out
}

/// Polygon core of [`subdivide_surfaces_for_lighting`], shared with
/// the editor preview so viewport patches match the cook's exactly
/// (same grid, same light-range gate): that is what lets per-vertex
/// shadows and hotspots resolve mid-face in the editor too.
pub fn subdivide_polygon_for_lighting(
    vertices: Vec<[f64; 3]>,
    max_extent: f64,
    light_spheres: &[([f64; 3], f64)],
) -> Vec<Vec<[f64; 3]>> {
    let mut out = Vec::new();
    let mut queue = vec![vertices];
    while let Some(polygon) = queue.pop() {
        let mut min = [f64::MAX; 3];
        let mut max = [f64::MIN; 3];
        for vertex in &polygon {
            for axis in 0..3 {
                min[axis] = min[axis].min(vertex[axis]);
                max[axis] = max[axis].max(vertex[axis]);
            }
        }
        let in_light_range = light_spheres.iter().any(|(center, radius)| {
            let mut dist_sq = 0.0;
            for axis in 0..3 {
                let d = (min[axis] - center[axis])
                    .max(center[axis] - max[axis])
                    .max(0.0);
                dist_sq += d * d;
            }
            dist_sq <= radius * radius
        });
        if !in_light_range {
            out.push(polygon);
            continue;
        }
        // Widest axis exceeding the limit, if any.
        let mut split_axis = None;
        let mut widest = max_extent;
        for axis in 0..3 {
            let extent = max[axis] - min[axis];
            if extent > widest {
                widest = extent;
                split_axis = Some(axis);
            }
        }
        let Some(axis) = split_axis else {
            out.push(polygon);
            continue;
        };
        // Grid-aligned cut nearest the middle, strictly inside the span.
        let mid = (min[axis] + max[axis]) * 0.5;
        let mut cut = (mid / max_extent).round() * max_extent;
        if cut <= min[axis] + 1.0 || cut >= max[axis] - 1.0 {
            cut = mid.round();
        }
        let cut = cut.round() as i32;
        let mut a = [0i32; 3];
        let mut b = [0i32; 3];
        let mut c = [0i32; 3];
        a[axis] = cut;
        b[axis] = cut;
        c[axis] = cut;
        b[(axis + 1) % 3] = 1;
        c[(axis + 2) % 3] = 1;
        let Some(plane) = Plane::from_points([a, b, c]) else {
            out.push(polygon);
            continue;
        };
        match split_polygon(&polygon, plane) {
            PolygonSplit::Split { front, back } => {
                queue.push(front);
                queue.push(back);
            }
            // Degenerate cut (numerical edge): keep the polygon whole
            // rather than looping.
            _ => out.push(polygon),
        }
    }
    out
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
            let polygon_bounds = polygon_bounds(&polygon.verts);
            let mut fragments = vec![polygon.verts.clone()];
            for other_index in 0..brushes.len() {
                if other_index == brush_index || !valid[other_index] {
                    continue;
                }
                // A convex volume can only carve this face when their AABBs
                // overlap. Besides avoiding pointless plane walks, this is
                // essential for full maps: subtracting thousands of remote
                // brushes can partition a polygon into outside fragments even
                // though none of those volumes reaches the face. The broad
                // phase is conservative at touching boundaries, so it cannot
                // discard a real CSG interaction.
                if !bounds_overlap(
                    polygon_bounds,
                    (solved[other_index].min, solved[other_index].max),
                ) {
                    continue;
                }
                // A liquid never carves structural geometry. Solid faces
                // remain visible below the surface. A liquid face is clipped
                // by solid, the same liquid (union CSG), or a stronger liquid.
                // It must survive a weaker overlapping liquid so the render
                // BSP retains every effective contents-transition plane.
                if brush.contents.is_solid() && !brushes[other_index].contents.is_solid() {
                    continue;
                }
                if !brush.contents.is_solid()
                    && !brushes[other_index].contents.is_solid()
                    && brushes[other_index].contents.precedence() < brush.contents.precedence()
                {
                    continue;
                }
                let other_planes = &volume_planes[other_index];
                let keep_coplanar_inside = brush.contents == brushes[other_index].contents
                    && brush_index < other_index
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
                contents: brush.contents,
                source_brush: brush_index,
                source_face: face_index,
            }));
        }
    }
    output
}

fn polygon_bounds(vertices: &[[f64; 3]]) -> ([f64; 3], [f64; 3]) {
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for vertex in vertices {
        for axis in 0..3 {
            min[axis] = min[axis].min(vertex[axis]);
            max[axis] = max[axis].max(vertex[axis]);
        }
    }
    (min, max)
}

fn bounds_overlap(a: ([f64; 3], [f64; 3]), b: ([f64; 3], [f64; 3])) -> bool {
    (0..3).all(|axis| a.1[axis] + CSG_EPSILON >= b.0[axis] && b.1[axis] + CSG_EPSILON >= a.0[axis])
}

/// Compile one solved polygon for every authored brush face.
///
/// Full maps can make pairwise union CSG fragment a few thousand source faces
/// into hundreds of thousands of coplanar pieces. The classified BSP leaves
/// already distinguish open space from buried solid boundaries, so this
/// representation can retain each authored face once and let leaf marks keep
/// internal faces out of visible PVS chains.
pub fn compile_authored_surfaces(brushes: &[Brush]) -> Vec<CompiledSurface> {
    let solved: Vec<_> = brushes.iter().map(Brush::solve).collect();
    let mut output = Vec::new();
    for (brush_index, brush) in brushes.iter().enumerate() {
        if !solved[brush_index].is_valid() {
            continue;
        }
        for (face_index, face) in brush.faces.iter().enumerate() {
            let Some(plane) = Plane::from_points(face.points) else {
                continue;
            };
            let Some(polygon) = solved[brush_index].polygons[face_index].as_ref() else {
                continue;
            };
            output.push(CompiledSurface {
                plane,
                vertices: polygon.verts.clone(),
                material: face.material,
                uv: face.uv,
                contents: brush.contents,
                source_brush: brush_index,
                source_face: face_index,
            });
        }
    }
    output
}

/// Recursively partition exterior CSG polygons into a render BSP.
///
/// Splitter selection exhaustively minimizes polygon splits first and tree
/// imbalance second. Coplanar surfaces produce the same classification, so
/// only the first occurrence of each packed plane is scored. Large branches
/// distribute those exact scores across host CPU cores. Classification is
/// allocation-free and input order is the final tie breaker, so identical
/// inputs always produce identical output.
pub fn build_surface_bsp(surfaces: &[CompiledSurface]) -> CompiledSurfaceBsp {
    let mut bsp = CompiledSurfaceBsp {
        root: BspChild::Leaf(0),
        nodes: Vec::new(),
        leaves: Vec::new(),
        surfaces: Vec::new(),
    };
    bsp.root = build_surface_bsp_branch(surfaces.to_vec(), &mut bsp);
    rebuild_exact_leaf_marks(&mut bsp);
    bsp
}

/// qbsp-parity leaf marking: each node surface is pushed down both
/// subtrees of its owning node, split by every deeper plane, and marks
/// exactly the leaves its fragments reach. Mark totals therefore scale
/// with surface count instead of combinatorially with tree depth, which
/// is what keeps heavily subdivided maps inside the packer's 64k
/// mark-surface budget. Back-side descents mostly land in solid leaves
/// (dropped at pack time) and keep two-sided liquid faces exact.
fn rebuild_exact_leaf_marks(bsp: &mut CompiledSurfaceBsp) {
    for leaf in &mut bsp.leaves {
        leaf.mark_surfaces.clear();
    }
    for node_index in 0..bsp.nodes.len() {
        let (front, back, first, count) = {
            let node = &bsp.nodes[node_index];
            (
                node.front,
                node.back,
                node.first_surface,
                node.surface_count,
            )
        };
        for surface_index in first..first + count {
            let polygon = bsp.surfaces[surface_index].vertices.clone();
            let mut stack = vec![(front, polygon.clone()), (back, polygon)];
            while let Some((child, polygon)) = stack.pop() {
                match child {
                    BspChild::Leaf(leaf) => {
                        let marks = &mut bsp.leaves[leaf].mark_surfaces;
                        if !marks.contains(&surface_index) {
                            marks.push(surface_index);
                        }
                    }
                    BspChild::Node(index) => {
                        let node_front = bsp.nodes[index].front;
                        let node_back = bsp.nodes[index].back;
                        match split_polygon(&polygon, bsp.nodes[index].plane) {
                            PolygonSplit::Front(vertices) => stack.push((node_front, vertices)),
                            PolygonSplit::Back(vertices) => stack.push((node_back, vertices)),
                            // In a deeper node's plane: fragments of both
                            // facing directions border both sides.
                            PolygonSplit::Coplanar => {
                                stack.push((node_front, polygon.clone()));
                                stack.push((node_back, polygon));
                            }
                            PolygonSplit::Split {
                                front: front_vertices,
                                back: back_vertices,
                            } => {
                                stack.push((node_front, front_vertices));
                                stack.push((node_back, back_vertices));
                            }
                        }
                    }
                }
            }
        }
    }
}

fn build_surface_bsp_branch(
    surfaces: Vec<CompiledSurface>,
    bsp: &mut CompiledSurfaceBsp,
) -> BspChild {
    if surfaces.is_empty() {
        let index = bsp.leaves.len();
        bsp.leaves.push(CompiledBspLeaf {
            contents: BspLeafContents::Unclassified,
            // Filled by rebuild_exact_leaf_marks after the tree exists.
            mark_surfaces: Vec::new(),
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

    let front_child = build_surface_bsp_branch(front, bsp);
    let back_child = build_surface_bsp_branch(back, bsp);
    bsp.nodes[node_index].front = front_child;
    bsp.nodes[node_index].back = back_child;
    BspChild::Node(node_index)
}

fn choose_splitter(surfaces: &[CompiledSurface]) -> usize {
    let mut seen = HashSet::with_capacity(surfaces.len());
    let candidates: Vec<_> = surfaces
        .iter()
        .enumerate()
        .filter_map(|(index, surface)| {
            let (plane, _) = pack_plane(&surface.plane)?;
            seen.insert(plane).then_some(index)
        })
        .collect();
    assert!(!candidates.is_empty(), "surface set has no valid plane");

    let worker_count = if surfaces.len() >= PARALLEL_SPLITTER_SURFACES {
        std::thread::available_parallelism()
            .map_or(1, usize::from)
            .min(candidates.len())
    } else {
        1
    };
    if worker_count == 1 {
        return candidates
            .into_iter()
            .map(|candidate| score_splitter(surfaces, candidate))
            .min_by_key(|(score, _)| *score)
            .unwrap()
            .1;
    }

    let chunk_size = candidates.len().div_ceil(worker_count);
    std::thread::scope(|scope| {
        let handles: Vec<_> = candidates
            .chunks(chunk_size)
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .copied()
                        .map(|candidate| score_splitter(surfaces, candidate))
                        .min_by_key(|(score, _)| *score)
                        .unwrap()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("splitter worker panicked"))
            .min_by_key(|(score, _)| *score)
            .unwrap()
            .1
    })
}

fn score_splitter(
    surfaces: &[CompiledSurface],
    candidate: usize,
) -> ((usize, usize, usize), usize) {
    let surface = &surfaces[candidate];
    let mut front = 0usize;
    let mut back = 0usize;
    let mut splits = 0usize;
    for classified in surfaces {
        match classify_polygon(&classified.vertices, surface.plane) {
            PolygonSide::Front => front += 1,
            PolygonSide::Back => back += 1,
            PolygonSide::Coplanar => {}
            PolygonSide::Split => {
                front += 1;
                back += 1;
                splits += 1;
            }
        }
    }
    let imbalance = front.abs_diff(back);
    ((splits, imbalance, candidate), candidate)
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PolygonSide {
    Front,
    Back,
    Coplanar,
    Split,
}

/// Classify without constructing either child polygon. Splitter selection
/// calls this for every candidate/surface pair, so keeping that hot scan
/// allocation-free matters much more than the final one-time partition.
fn classify_polygon(vertices: &[[f64; 3]], plane: Plane) -> PolygonSide {
    let (normal, distance) = normalized_plane(plane);
    let mut has_front = false;
    let mut has_back = false;
    for vertex in vertices {
        let signed_distance = dot(normal, *vertex) - distance;
        has_front |= signed_distance > CSG_EPSILON;
        has_back |= signed_distance < -CSG_EPSILON;
        if has_front && has_back {
            return PolygonSide::Split;
        }
    }
    match (has_front, has_back) {
        (true, false) => PolygonSide::Front,
        (false, true) => PolygonSide::Back,
        (false, false) => PolygonSide::Coplanar,
        (true, true) => unreachable!("split polygons return from the scan"),
    }
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

pub fn normalized_plane(plane: Plane) -> ([f64; 3], f64) {
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

    // Higher-precedence contents are installed last and become the root,
    // making overlap classification independent of authored order.
    let mut ordered: Vec<_> = brushes.iter().enumerate().collect();
    ordered.sort_by_key(|(index, brush)| (core::cmp::Reverse(brush.contents.precedence()), *index));
    let mut escape = CONTENTS_EMPTY;
    for (_, brush) in ordered.into_iter().rev() {
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
            let index = planes.len() / psx_bsp::Plane::SIZE;
            if index > i16::MAX as usize {
                break;
            }
            planes.extend_from_slice(&packed);
            face_planes.push((index as i16, flipped));
        }
        if face_planes.is_empty() {
            continue;
        }
        // Innermost decision first: inside every plane means this brush's
        // authored structural/liquid contents.
        let mut inside: i16 = brush.contents.runtime_contents();
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

/// Pack one kernel plane as the canonical 14-byte BSP record: Q3.12 unit
/// normal, Q20.12 distance, and the stored i32 axial fast-path kind. Returns
/// the bytes and whether the stored plane is flipped relative to the face's
/// outward normal (axial planes are canonicalized to positive normals).
pub(crate) fn pack_plane(plane: &Plane) -> Option<([u8; 14], bool)> {
    let n = plane.normal.map(|v| v as f64);
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len <= 0.0 {
        return None;
    }
    let unit = [n[0] / len, n[1] / len, n[2] / len];
    let dist_world = plane.dist as f64 / len;

    pack_normalized_plane(unit, dist_world)
}

pub(crate) fn pack_normalized_plane(unit: [f64; 3], dist_world: f64) -> Option<([u8; 14], bool)> {
    if !unit.into_iter().all(f64::is_finite) || !dist_world.is_finite() {
        return None;
    }

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

    #[test]
    fn lighting_subdivision_bounds_patches_and_preserves_area() {
        use crate::brush::Brush;
        let surfaces = compile_csg_surfaces(&[Brush::cuboid([0, 0, 0], [2048, 256, 1024])]);
        let area = |vertices: &[[f64; 3]]| -> f64 {
            let mut total = [0.0f64; 3];
            for i in 1..vertices.len().saturating_sub(1) {
                let u = [
                    vertices[i][0] - vertices[0][0],
                    vertices[i][1] - vertices[0][1],
                    vertices[i][2] - vertices[0][2],
                ];
                let v = [
                    vertices[i + 1][0] - vertices[0][0],
                    vertices[i + 1][1] - vertices[0][1],
                    vertices[i + 1][2] - vertices[0][2],
                ];
                total[0] += u[1] * v[2] - u[2] * v[1];
                total[1] += u[2] * v[0] - u[0] * v[2];
                total[2] += u[0] * v[1] - u[1] * v[0];
            }
            (total[0].powi(2) + total[1].powi(2) + total[2].powi(2)).sqrt() * 0.5
        };
        let before: f64 = surfaces.iter().map(|s| area(&s.vertices)).sum();
        let everywhere = [([1024.0, 128.0, 512.0], 1_000_000.0)];
        let pieces = subdivide_surfaces_for_lighting(surfaces.clone(), 1024.0, &everywhere);
        assert!(pieces.len() > surfaces.len(), "big faces were subdivided");
        let after: f64 = pieces.iter().map(|s| area(&s.vertices)).sum();
        assert!(
            (before - after).abs() <= before * 1e-6,
            "area preserved: {before} vs {after}"
        );
        for piece in &pieces {
            let mut min = [f64::MAX; 3];
            let mut max = [f64::MIN; 3];
            for vertex in &piece.vertices {
                for axis in 0..3 {
                    min[axis] = min[axis].min(vertex[axis]);
                    max[axis] = max[axis].max(vertex[axis]);
                }
            }
            for axis in 0..3 {
                assert!(
                    max[axis] - min[axis] <= 1024.0 + 2.0,
                    "patch exceeds the lighting grid: {:?}..{:?}",
                    min,
                    max
                );
            }
            assert!(piece.vertices.len() >= 3);
        }
    }

    #[test]
    fn full_level_subdivision_coarsens_only_when_the_face_budget_requires_it() {
        let surfaces = compile_csg_surfaces(&[Brush::cuboid([0, 0, 0], [4096, 256, 4096])]);
        let fine = subdivide_surfaces_to_extent(surfaces.clone(), 128.0);
        assert!(fine.len() > 64);

        let fitted = subdivide_surfaces_to_budget(&surfaces, 128.0, 64, &[]);
        assert!(fitted.len() <= 64, "fitted {} faces", fitted.len());
        assert_eq!(
            subdivide_surfaces_to_budget(&surfaces, 128.0, fine.len(), &[]),
            fine
        );
    }

    use super::*;
    use psx_bsp::collision::{
        CollisionHull, Trace, TraceScratch, CONTENTS_EMPTY, CONTENTS_LAVA, CONTENTS_SLIME,
        CONTENTS_SOLID, CONTENTS_WATER,
    };
    use psx_bsp::{ClipNode, Plane as BspPlane, RecordSlice, Vec3I32};

    #[test]
    fn packed_planes_retain_the_cooker_authored_classifier() {
        for (normal, expected_kind) in [
            ([1.0, 0.0, 0.0], 0),
            ([0.0, 1.0, 0.0], 1),
            ([0.0, 0.0, 1.0], 2),
            ([0.5, 0.5, std::f64::consts::FRAC_1_SQRT_2], 3),
        ] {
            let (packed, _) = pack_normalized_plane(normal, 12.0).expect("valid plane");
            assert_eq!(
                i32::from_le_bytes(packed[10..14].try_into().unwrap()),
                expected_kind
            );
        }
    }

    fn hull(compiled: &CompiledCollision) -> CollisionHull<'_> {
        CollisionHull::new(
            RecordSlice::<BspPlane>::new(&compiled.planes).expect("plane bytes"),
            RecordSlice::<ClipNode>::new(&compiled.clipnodes).expect("clipnode bytes"),
            compiled.head_node,
        )
        .expect("aligned collision records")
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

    fn trace(hull: &CollisionHull<'_>, start: Vec3I32, end: Vec3I32) -> Trace {
        let mut output = Trace::default();
        assert!(hull.trace_into(&start, &end, &mut TraceScratch::new(), &mut output,));
        output
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
        let brushes = [
            Brush::cuboid([0, 0, 0], [128, 128, 128]),
            Brush::cuboid([128, 0, 0], [256, 128, 128]),
        ];
        let surfaces = compile_csg_surfaces(&brushes);
        assert_eq!(surfaces.len(), 10);
        assert!(!surfaces.iter().any(|surface| {
            surface_axis_coordinate(surface, 0).is_some_and(|x| (x - 128.0).abs() < CSG_EPSILON)
        }));
        let authored = compile_authored_surfaces(&brushes);
        assert_eq!(authored.len(), 12);
        assert_eq!(authored[0].source_brush, 0);
        assert_eq!(authored[6].source_brush, 1);
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
        assert!(bsp
            .surfaces
            .iter()
            .all(|surface| surface.vertices.len() >= 3));
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
            assert!(bsp.surfaces[node.first_surface..end]
                .iter()
                .all(|surface| matches!(
                    split_polygon(&surface.vertices, node.plane),
                    PolygonSplit::Coplanar
                )));
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
            contents: BrushContents::Solid,
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
            contents: BrushContents::Solid,
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
        assert!(bsp
            .surfaces
            .iter()
            .all(|surface| surface.vertices.len() >= 3));
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
    fn allocation_free_polygon_classifier_matches_split_results() {
        let plane = Plane::from_points([[0, 0, 0], [0, 1, 0], [0, 0, 1]]).unwrap();
        for (vertices, expected) in [
            (
                vec![[1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [1.0, 0.0, 1.0]],
                PolygonSide::Front,
            ),
            (
                vec![[-1.0, 0.0, 0.0], [-1.0, 0.0, 1.0], [-1.0, 1.0, 0.0]],
                PolygonSide::Back,
            ),
            (
                vec![[0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                PolygonSide::Coplanar,
            ),
            (
                vec![[-1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [1.0, 0.0, 1.0]],
                PolygonSide::Split,
            ),
        ] {
            assert_eq!(classify_polygon(&vertices, plane), expected);
            let actual = match split_polygon(&vertices, plane) {
                PolygonSplit::Front(_) => PolygonSide::Front,
                PolygonSplit::Back(_) => PolygonSide::Back,
                PolygonSplit::Coplanar => PolygonSide::Coplanar,
                PolygonSplit::Split { .. } => PolygonSide::Split,
            };
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn large_branch_splitter_chooses_a_balanced_parallel_plane() {
        let surfaces: Vec<_> = (0..600)
            .map(|index| {
                let x = index as i32 * 16;
                CompiledSurface {
                    plane: Plane::from_points([[x, 0, 0], [x, 1, 0], [x, 0, 1]]).unwrap(),
                    vertices: vec![
                        [f64::from(x), 0.0, 0.0],
                        [f64::from(x), 1.0, 0.0],
                        [f64::from(x), 1.0, 1.0],
                        [f64::from(x), 0.0, 1.0],
                    ],
                    material: None,
                    uv: FaceUv::default(),
                    contents: BrushContents::Solid,
                    source_brush: index,
                    source_face: 0,
                }
            })
            .collect();
        let splitter = choose_splitter(&surfaces);
        assert!(
            (280..=320).contains(&splitter),
            "large branch chose unbalanced plane {splitter}"
        );
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
    fn liquid_brushes_classify_without_blocking_traces() {
        for (contents, expected) in [
            (BrushContents::Water, CONTENTS_WATER),
            (BrushContents::Slime, CONTENTS_SLIME),
            (BrushContents::Lava, CONTENTS_LAVA),
        ] {
            let mut brush = Brush::cuboid([0, 0, 0], [512, 256, 512]);
            brush.contents = contents;
            let compiled = compile_collision(&[brush]);
            let hull = hull(&compiled);
            assert_eq!(hull.point_contents(at(256, 128, 256)), Some(expected));
            assert_eq!(hull.point_contents(at(-64, 128, 256)), Some(CONTENTS_EMPTY));
            let trace = trace(&hull, at(-64, 128, 256), at(576, 128, 256));
            assert_eq!(trace.fraction, Q12_ONE, "{} blocked", contents.label());
            assert!(!trace.start_solid.is_set());
            assert!(trace.in_open.is_set());
            assert!(trace.in_water.is_set());
        }
    }

    #[test]
    fn solid_and_hazard_overlap_precedence_is_authored_order_independent() {
        let mut water = Brush::cuboid([0, 0, 0], [512, 256, 512]);
        water.contents = BrushContents::Water;
        let solid = Brush::cuboid([128, 0, 128], [384, 256, 384]);
        for brushes in [
            vec![water.clone(), solid.clone()],
            vec![solid.clone(), water.clone()],
        ] {
            let compiled = compile_collision(&brushes);
            let hull = hull(&compiled);
            assert_eq!(hull.point_contents(at(256, 128, 256)), Some(CONTENTS_SOLID));
            assert_eq!(hull.point_contents(at(64, 128, 64)), Some(CONTENTS_WATER));
        }
    }

    #[test]
    fn liquid_overlap_precedence_is_authored_order_independent() {
        let mut water = Brush::cuboid([0, 0, 0], [512, 256, 512]);
        water.contents = BrushContents::Water;
        let mut slime = Brush::cuboid([64, 0, 64], [448, 256, 448]);
        slime.contents = BrushContents::Slime;
        let mut lava = Brush::cuboid([128, 0, 128], [384, 256, 384]);
        lava.contents = BrushContents::Lava;
        for brushes in [
            vec![water.clone(), slime.clone(), lava.clone()],
            vec![lava.clone(), water.clone(), slime.clone()],
            vec![slime.clone(), lava.clone(), water.clone()],
        ] {
            let compiled = compile_collision(&brushes);
            let hull = hull(&compiled);
            assert_eq!(hull.point_contents(at(32, 128, 32)), Some(CONTENTS_WATER));
            assert_eq!(hull.point_contents(at(96, 128, 96)), Some(CONTENTS_SLIME));
            assert_eq!(hull.point_contents(at(256, 128, 256)), Some(CONTENTS_LAVA));
        }
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
        let trace = trace(&hull, at(512, 400, 512), at(512, -400, 512));
        assert!(!trace.start_solid.is_set());
        assert!(trace.in_open.is_set());
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
        let trace = trace(&hull, at(300, 64, 64), at(700, 64, 64));
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
