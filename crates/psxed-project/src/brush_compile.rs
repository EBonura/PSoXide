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
// balanced build with outer sealing replaces this when the full
// compiler lands.

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
    /// Authored face override, retained through CSG and final patch cuts.
    pub edge_repair: Option<SurfaceEdgeRepair>,
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

/// Hard cap on face extent, world units: every cooked surface subdivides
/// so no piece spans more than this along any axis, with or without
/// lights. Small faces are the near-plane safety: an eye-plane-crossing
/// sliver saturates past the GPU's 1023x511 draw limit and is skipped
/// instead of wrapping. 2048 here is measured, not aesthetic: the
/// sanctum's worst PVS leaf sits at 2097 surfaces / 4198 packets against
/// the 4096-packet arena. 1024 cooks but overflows the arena (7472); 256
/// cooks only thanks to exact leaf marks and then wants 37918 packets,
/// which no runtime budget survives. Tighten only together with a
/// packet/RAM plan.
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

/// Make final render-patch boundaries conform before integer packing. Run after
/// lighting/UV cuts: rounding a midpoint independently of an unsplit neighbour
/// makes a real hole, even when both floating-point edges are collinear.
/// Positions are borrowed from existing corners (no centroid or texture changes).
/// Call separately for independently moving brush models.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum SurfaceEdgeRepair {
    #[default]
    None,
    /// Repair upward terrain patches; wall T junctions remain unchanged.
    Upward,
    /// Repair upward edges only when packing moves an existing split corner
    /// off the unsplit segment. Projection can still expose collinear T joints.
    UpwardQuantized,
    All,
}

pub fn stitch_surface_edges(surfaces: Vec<CompiledSurface>) -> Vec<CompiledSurface> {
    repair_surface_edges(surfaces, SurfaceEdgeRepair::All, 1.0)
}

/// `quantum` is one packed position unit in the supplied coordinate system:
/// 1 for the cooker, WORLD_UNIT_DIVISOR for authored editor coordinates.
pub fn repair_surface_edges(
    surfaces: Vec<CompiledSurface>,
    policy: SurfaceEdgeRepair,
    quantum: f64,
) -> Vec<CompiledSurface> {
    if surfaces
        .iter()
        .all(|s| s.edge_repair.unwrap_or(policy) == SurfaceEdgeRepair::None)
    {
        return surfaces;
    }
    assert!(quantum.is_finite() && quantum > 0.0);
    let epsilon = CSG_EPSILON * quantum;
    // Untouched neighbors keep their exact packed endpoints. A selective
    // repair may weld float noise only within one packed-position bin;
    // otherwise it could move one side of an existing shared endpoint.
    let preserve_packed_endpoints = surfaces
        .iter()
        .any(|s| s.edge_repair.unwrap_or(policy) == SurfaceEdgeRepair::None);

    // An explicit repair selection supplies its own corner cloud. Untouched
    // surfaces neither move nor supply cuts. Selected corners can still split an
    // outer edge: authors must include both sides wherever conformance is needed.
    let mut points: Vec<_> = surfaces
        .iter()
        .filter(|s| s.edge_repair.unwrap_or(policy) != SurfaceEdgeRepair::None)
        .flat_map(|s| s.vertices.iter().copied())
        .collect();
    points.sort_by(|a, b| {
        a[0].total_cmp(&b[0])
            .then(a[1].total_cmp(&b[1]))
            .then(a[2].total_cmp(&b[2]))
    });
    points.dedup();
    // Search the tightest axis interval for each edge, avoiding an all-pairs
    // scan on large maps while retaining points at axis-aligned boundaries.
    let axes: [Vec<usize>; 3] = std::array::from_fn(|axis| {
        let mut ids: Vec<_> = (0..points.len()).collect();
        ids.sort_by(|&a, &b| points[a][axis].total_cmp(&points[b][axis]));
        ids
    });
    // Independently reconstructed brush planes can differ by a few ulps at
    // an existing endpoint. Canonicalize those too: values on opposite sides
    // of an integer half-unit otherwise round to different packed corners.
    let mut canonical = std::collections::HashMap::new();
    for &p in &points {
        let (axis, start, end) = (0..3)
            .map(|axis| {
                let start = axes[axis].partition_point(|&i| points[i][axis] < p[axis] - epsilon);
                let end = axes[axis].partition_point(|&i| points[i][axis] <= p[axis] + epsilon);
                (axis, start, end)
            })
            .min_by_key(|&(_, start, end)| end - start)
            .unwrap();
        let id = axes[axis][start..end]
            .iter()
            .copied()
            .filter(|&i| {
                let d = std::array::from_fn(|j| points[i][j] - p[j]);
                dot(d, d) <= epsilon * epsilon
                    && (!preserve_packed_endpoints
                        || (0..3)
                            .all(|j| (points[i][j] / quantum).round() == (p[j] / quantum).round()))
            })
            .min()
            .unwrap();
        canonical.insert(
            p.map(|v| if v == 0.0 { 0 } else { v.to_bits() }),
            points[id],
        );
    }
    let mut output = Vec::with_capacity(surfaces.len());
    for mut surface in surfaces {
        let policy = surface.edge_repair.unwrap_or(policy);
        if policy == SurfaceEdgeRepair::None {
            output.push(surface);
            continue;
        }
        for p in &mut surface.vertices {
            *p = canonical[&p.map(|v| if v == 0.0 { 0 } else { v.to_bits() })];
        }
        if matches!(
            policy,
            SurfaceEdgeRepair::Upward | SurfaceEdgeRepair::UpwardQuantized
        ) && normalized_plane(surface.plane).0[1] <= 0.8
        {
            output.push(surface);
            continue;
        }
        let mut vertices = Vec::with_capacity(surface.vertices.len());
        for i in 0..surface.vertices.len() {
            let a = surface.vertices[i];
            let b = surface.vertices[(i + 1) % surface.vertices.len()];
            vertices.push(a);
            let delta = std::array::from_fn::<_, 3, _>(|j| b[j] - a[j]);
            let length2 = dot(delta, delta);
            if length2 <= epsilon * epsilon {
                continue;
            }
            let (axis, start, end) = (0..3)
                .map(|axis| {
                    let lo = a[axis].min(b[axis]) - epsilon;
                    let hi = a[axis].max(b[axis]) + epsilon;
                    let start = axes[axis].partition_point(|&p| points[p][axis] < lo);
                    let end = axes[axis].partition_point(|&p| points[p][axis] <= hi);
                    (axis, start, end)
                })
                .min_by_key(|&(_, start, end)| end - start)
                .unwrap();
            let mut cuts = Vec::new();
            for &id in &axes[axis][start..end] {
                let p = points[id];
                let offset = std::array::from_fn(|j| p[j] - a[j]);
                let t = dot(offset, delta) / length2;
                if t <= 0.0 || t >= 1.0 {
                    continue;
                }
                let error = std::array::from_fn(|j| offset[j] - t * delta[j]);
                if dot(error, error) <= epsilon * epsilon
                    && t * t * length2 > epsilon * epsilon
                    && (1.0 - t).powi(2) * length2 > epsilon * epsilon
                {
                    cuts.push((t, p));
                }
            }
            cuts.sort_by(|a, b| a.0.total_cmp(&b.0));
            cuts.dedup_by(|a, b| {
                (a.0 - b.0).abs() * length2.sqrt() <= epsilon
                    && (!preserve_packed_endpoints
                        || (0..3).all(|j| (a.1[j] / quantum).round() == (b.1[j] / quantum).round()))
            });
            for (_, p) in &mut cuts {
                *p = canonical[&p.map(|v| if v == 0.0 { 0 } else { v.to_bits() })];
            }
            if policy == SurfaceEdgeRepair::UpwardQuantized {
                // Simplify the resulting packed chain, not the original long
                // segment: a retained bent corner can make another cut vital.
                let mut i = 0;
                while i < cuts.len() {
                    let previous = if i == 0 { a } else { cuts[i - 1].1 };
                    let next = if i + 1 == cuts.len() {
                        b
                    } else {
                        cuts[i + 1].1
                    };
                    if packed_collinear_between(previous, cuts[i].1, next, quantum) {
                        cuts.remove(i);
                        i = i.saturating_sub(1);
                    } else {
                        i += 1;
                    }
                }
            }
            vertices.extend(cuts.into_iter().map(|(_, p)| p));
        }
        if vertices.len() == surface.vertices.len() {
            output.push(surface);
            continue;
        }
        surface.vertices = vertices;
        // A fan rooted on a subdivided boundary can contain a collinear
        // triangle, which may flip after packing and lose that segment to
        // backface culling. Keep a safe fan when possible; otherwise peel
        // convex ears until one exists, retaining all boundary segments.
        while surface.vertices.len() > 3 {
            let n = surface.vertices.len();
            let safe_root = (0..n).find(|&root| {
                (1..n - 1).all(|i| {
                    stitch_triangle_is_safe(
                        surface.vertices[root],
                        surface.vertices[(root + i) % n],
                        surface.vertices[(root + i + 1) % n],
                        quantum,
                    )
                })
            });
            if let Some(root) = safe_root {
                surface.vertices.rotate_left(root);
                break;
            }
            let Some(ear) = (0..n).find(|&i| {
                stitch_triangle_is_safe(
                    surface.vertices[(i + n - 1) % n],
                    surface.vertices[i],
                    surface.vertices[(i + 1) % n],
                    quantum,
                )
            }) else {
                break;
            };
            let mut piece = surface.clone();
            piece.vertices = vec![
                surface.vertices[(ear + n - 1) % n],
                surface.vertices[ear],
                surface.vertices[(ear + 1) % n],
            ];
            output.push(piece);
            surface.vertices.remove(ear);
        }
        output.push(surface);
    }
    output
}

fn packed_collinear_between(a: [f64; 3], p: [f64; 3], b: [f64; 3], quantum: f64) -> bool {
    let q = |p: [f64; 3]| p.map(|v| (v / quantum).round() as i64);
    let a = q(a);
    let b = q(b);
    let p = q(p);
    let d: [i64; 3] = std::array::from_fn(|i| b[i] - a[i]);
    let v: [i64; 3] = std::array::from_fn(|i| p[i] - a[i]);
    let length2 = d.iter().map(|v| v * v).sum::<i64>();
    if length2 == 0 {
        return v == [0; 3];
    }
    let along = (0..3).map(|i| v[i] * d[i]).sum::<i64>();
    d[1] * v[2] - d[2] * v[1] == 0
        && d[2] * v[0] - d[0] * v[2] == 0
        && d[0] * v[1] - d[1] * v[0] == 0
        && along >= 0
        && along <= length2
}

fn stitch_triangle_is_safe(a: [f64; 3], b: [f64; 3], c: [f64; 3], quantum: f64) -> bool {
    let cross = |u: [f64; 3], v: [f64; 3]| {
        [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ]
    };
    let u = std::array::from_fn(|i| b[i] - a[i]);
    let v = std::array::from_fn(|i| c[i] - a[i]);
    let normal = cross(u, v);
    let longest2 = dot(u, u).max(dot(v, v));
    // Reject numerical sliver ears by perpendicular distance, not absolute
    // area: CSG's near-collinear corners must survive as boundary segments.
    if dot(normal, normal) <= (CSG_EPSILON * quantum).powi(2) * longest2 {
        return false;
    }
    true
}

/// Fan-split every surface wider than `max_vertices` into pieces that share
/// its first vertex. The pieces tile the polygon exactly, keep every other
/// field, and draw the same triangles: the runtime fans each face from its
/// first vertex, so piece `{0, a..=b}` is triangles `(0, i, i + 1)` for
/// `a <= i < b`.
pub fn split_wide_surfaces(
    surfaces: Vec<CompiledSurface>,
    max_vertices: usize,
) -> Vec<CompiledSurface> {
    assert!(max_vertices >= 3, "a fan piece needs three vertices");
    let mut result = Vec::with_capacity(surfaces.len());
    for surface in surfaces {
        let count = surface.vertices.len();
        if count <= max_vertices {
            result.push(surface);
            continue;
        }
        let mut first = 1;
        while first + 1 < count {
            let end = (first + max_vertices - 1).min(count);
            let mut piece = surface.clone();
            piece.vertices.clear();
            piece.vertices.push(surface.vertices[0]);
            piece
                .vertices
                .extend_from_slice(&surface.vertices[first..end]);
            result.push(piece);
            first = end - 1;
        }
    }
    result
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
                edge_repair: face.edge_repair,
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
                edge_repair: face.edge_repair,
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

/// Exact leaf marking: each node surface is pushed down both
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
    // Adjacent terrain wedges touch at their sides, but sloped top planes
    // can split an entirely exterior neighbour before the side plane rejects
    // it. A separating plane proves there is nothing to subtract; preserve
    // the original low-poly face instead of emitting artificial fragments.
    if planes
        .iter()
        .any(|plane| classify_polygon(polygon, *plane) == PolygonSide::Front)
    {
        return vec![polygon.to_vec()];
    }
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
    fn selective_repair_preserves_fixed_neighbor_half_grid_endpoints_at_both_scales() {
        for quantum in [1.0, 16.0] {
            let scale = |p: [f64; 3]| p.map(|v| v * quantum);
            let shared = scale([0.500005, 0.0, 0.0]);
            let nearby = scale([0.499995, 0.0, 0.0]);
            let mut selected =
                compile_csg_surfaces(&[crate::brush::Brush::cuboid([0; 3], [64; 3])]).remove(0);
            selected.vertices = vec![shared, scale([2.0, 0.0, 0.0]), scale([0.0, 0.0, 2.0])];
            selected.edge_repair = Some(SurfaceEdgeRepair::All);
            let mut fixed = selected.clone();
            fixed.source_brush = 98;
            fixed.edge_repair = None;
            fixed.vertices = vec![shared, scale([0.0, 0.0, -2.0]), scale([2.0, 0.0, 0.0])];
            let mut third = fixed.clone();
            third.source_brush = 99;
            third.edge_repair = Some(SurfaceEdgeRepair::All);
            third.vertices[0] = nearby;
            let output = repair_surface_edges(
                vec![selected.clone(), fixed.clone(), third.clone()],
                SurfaceEdgeRepair::None,
                quantum,
            );
            assert_eq!(output.iter().find(|s| s.source_brush == 98), Some(&fixed));
            assert!(output
                .iter()
                .filter(|s| s.source_brush == 99)
                .any(|s| s.vertices.contains(&nearby)));
            assert!(output
                .iter()
                .filter(|s| s.source_brush == selected.source_brush)
                .any(|s| s.vertices.contains(&shared)));
            assert_eq!((shared[0] / quantum).round(), 1.0);
        }
    }

    #[test]
    fn face_override_repairs_selected_patches_and_preserves_inheriting_neighbors() {
        let mut a = compile_csg_surfaces(&[crate::brush::Brush::cuboid([0; 3], [64; 3])]).remove(0);
        a.vertices = vec![[0.0, 0.0, 0.0], [8.0, 0.0, 0.0], [0.0, 0.0, 8.0]];
        a.edge_repair = Some(SurfaceEdgeRepair::All);
        let mut b = a.clone();
        b.source_brush = 99;
        b.edge_repair = Some(SurfaceEdgeRepair::All);
        b.vertices = vec![[4.0, 0.0, 0.0], [4.0, 0.0, -4.0], [8.0, 0.0, 0.0]];
        let mut fixed = b.clone();
        fixed.source_brush = 100;
        fixed.edge_repair = None;
        fixed.vertices = vec![[2.0, 0.0, 0.0], [2.0, 0.0, -4.0], [4.0, 0.0, 0.0]];
        let output = repair_surface_edges(
            vec![a.clone(), b.clone(), fixed.clone()],
            SurfaceEdgeRepair::None,
            1.0,
        );
        assert!(output
            .iter()
            .filter(|s| s.source_brush == a.source_brush)
            .any(|s| s.vertices.contains(&[4.0, 0.0, 0.0])));
        assert_eq!(output.iter().find(|s| s.source_brush == 100), Some(&fixed));
        assert!(!output
            .iter()
            .filter(|s| s.source_brush == a.source_brush)
            .any(|s| s.vertices.contains(&[2.0, 0.0, 0.0])));
        a.edge_repair = Some(SurfaceEdgeRepair::None);
        assert_eq!(
            repair_surface_edges(vec![a.clone()], SurfaceEdgeRepair::All, 1.0),
            vec![a]
        );
    }

    #[test]
    fn authored_face_repair_survives_csg_and_patch_subdivision() {
        let mut brush = crate::brush::Brush::cuboid([0; 3], [256; 3]);
        brush.faces[0].edge_repair = Some(SurfaceEdgeRepair::All);
        let surfaces = subdivide_surfaces_to_extent(compile_csg_surfaces(&[brush]), 64.0);
        assert!(surfaces.iter().filter(|s| s.source_face == 0).count() > 1);
        for surface in surfaces {
            assert_eq!(
                surface.edge_repair,
                (surface.source_face == 0).then_some(SurfaceEdgeRepair::All)
            );
        }
    }

    #[test]
    fn cuts_are_canonicalized_once_even_for_close_point_chains_and_signed_zero() {
        let mut original =
            compile_csg_surfaces(&[crate::brush::Brush::cuboid([0; 3], [64; 3])]).remove(0);
        original.vertices = vec![[0.0, 0.0, 0.0], [1.0, -0.0, 0.0], [0.0, 1.0, 0.0]];
        let p1 = [0.5 - 0.000010, 0.0, 0.0];
        let p2 = [0.5 + 0.000001, 0.0, 0.0];
        let p3 = [0.5 + 0.000012, 0.0, 0.0];
        let mut a = original.clone();
        a.source_brush = 98;
        a.vertices = vec![p1, p2, [0.0, -1.0, 0.0]];
        let mut b = original.clone();
        b.source_brush = 99;
        b.vertices = vec![p3, [1.0, 0.0, 0.0], [1.0, -1.0, 0.0]];
        let repaired = stitch_surface_edges(vec![original.clone(), a, b]);
        assert!(
            repaired
                .iter()
                .filter(|s| s.source_brush == original.source_brush)
                .any(|s| s.vertices.contains(&p2)),
            "p3 maps once to p2, never twice to p1"
        );
        assert!(repaired
            .iter()
            .filter(|s| s.source_brush == 99)
            .any(|s| s.vertices.contains(&p2)));
    }

    #[test]
    fn selective_stitch_retains_cuts_required_by_a_bent_packed_chain() {
        let mut original =
            compile_csg_surfaces(&[crate::brush::Brush::cuboid([0; 3], [64; 3])]).remove(0);
        original.plane = Plane {
            normal: [0, 1, 0],
            dist: 0,
        };
        let a = [0.0, 0.0, 0.0];
        let b = [12.0, 0.0, 4.0];
        let p = [1.5, 0.0, 0.5];
        let q = [6.0, 0.0, 2.0];
        original.vertices = vec![a, b, [0.0, 0.0, 8.0]];
        let mut adjacent = original.clone();
        adjacent.source_brush = 99;
        adjacent.vertices = vec![b, q, p, a, [12.0, 0.0, -4.0]];
        let repaired = repair_surface_edges(
            vec![original.clone(), adjacent],
            SurfaceEdgeRepair::UpwardQuantized,
            1.0,
        );
        let vertices: Vec<_> = repaired
            .iter()
            .filter(|s| s.source_brush == original.source_brush)
            .flat_map(|s| s.vertices.iter().copied())
            .collect();
        assert!(vertices.contains(&p));
        assert!(vertices.contains(&q));
        assert!(packed_collinear_between(a, q, b, 1.0));
        assert!(!packed_collinear_between(p, q, b, 1.0));
    }

    #[test]
    fn repair_tolerance_scales_between_cooked_and_authored_coordinates() {
        let mut a = compile_csg_surfaces(&[crate::brush::Brush::cuboid([0; 3], [64; 3])]).remove(0);
        a.vertices = vec![[0.0, 0.0, 0.499995], [4.0, 0.0, 0.499995], [0.0, 4.0, 0.0]];
        let mut b = a.clone();
        b.vertices = vec![[4.0, 0.0, 0.500005], [0.0, 0.0, 0.500005], [4.0, -4.0, 0.0]];
        let source = vec![a, b];
        let cooked = repair_surface_edges(source.clone(), SurfaceEdgeRepair::All, 1.0);
        let mut authored = source;
        for s in &mut authored {
            for p in &mut s.vertices {
                for v in p {
                    *v *= 16.0;
                }
            }
        }
        let mut preview = repair_surface_edges(authored, SurfaceEdgeRepair::All, 16.0);
        for s in &mut preview {
            for p in &mut s.vertices {
                for v in p {
                    *v /= 16.0;
                }
            }
        }
        assert_eq!(cooked, preview);
        assert_eq!(cooked[0].vertices[0], cooked[1].vertices[1]);
    }

    #[test]
    fn repair_policy_defaults_off_and_roundtrips_explicitly() {
        let mut project = crate::ProjectDocument::from_ron_str(include_str!(
            "../../../archive/fixtures/brush-first-playable/project.ron"
        ))
        .unwrap();
        assert_eq!(project.bsp_render_edge_repair, SurfaceEdgeRepair::None);
        project.bsp_render_edge_repair = SurfaceEdgeRepair::UpwardQuantized;
        let restored =
            crate::ProjectDocument::from_ron_str(&project.to_ron_string().unwrap()).unwrap();
        assert_eq!(
            restored.bsp_render_edge_repair,
            SurfaceEdgeRepair::UpwardQuantized
        );
    }

    #[test]
    fn stitching_rejects_numerical_sliver_fans_that_collapse_when_packed() {
        let mut original =
            compile_csg_surfaces(&[crate::brush::Brush::cuboid([0; 3], [64; 3])]).remove(0);
        original.vertices = vec![[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, 10.0, 0.0]];
        let mut adjacent = original.clone();
        adjacent.source_brush = 99;
        adjacent.vertices = vec![[10.0, 0.0, 0.0], [5.0, 1e-9, 0.0], [0.0, 0.0, 0.0]];
        for piece in stitch_surface_edges(vec![original.clone(), adjacent])
            .into_iter()
            .filter(|p| p.source_brush == original.source_brush)
        {
            for i in 1..piece.vertices.len() - 1 {
                let [a, b, c] = [piece.vertices[0], piece.vertices[i], piece.vertices[i + 1]]
                    .map(|p| p.map(f64::round));
                assert!((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]) > 0.0);
            }
        }
    }

    #[test]
    fn stitching_welds_reconstructed_endpoints_before_half_unit_rounding() {
        let mut a = compile_csg_surfaces(&[crate::brush::Brush::cuboid([0; 3], [64; 3])]).remove(0);
        a.vertices = vec![
            [0.0, 0.0, 0.499999999],
            [4.0, 0.0, 0.499999999],
            [0.0, 4.0, 0.0],
        ];
        let mut b = a.clone();
        b.vertices = vec![
            [4.0, 0.0, 0.500000001],
            [0.0, 0.0, 0.500000001],
            [4.0, -4.0, 0.0],
        ];
        let repaired = stitch_surface_edges(vec![a, b]);
        assert_eq!(repaired[0].vertices[0], repaired[1].vertices[1]);
        assert_eq!(repaired[0].vertices[1], repaired[1].vertices[0]);
        assert_eq!(
            repaired[0].vertices[0].map(|v| v.round() as i32),
            repaired[1].vertices[1].map(|v| v.round() as i32)
        );
    }

    #[test]
    fn stitching_quantized_t_junction_retains_the_same_two_boundary_segments() {
        let a = [-583.0, 30.0, 200.0];
        let b = [-596.0, 32.0, 276.0];
        let c = std::array::from_fn(|i| a[i] + (b[i] - a[i]) * (14.0 / 76.0));
        let mut template =
            compile_csg_surfaces(&[crate::brush::Brush::cuboid([0; 3], [64; 3])]).remove(0);
        template.vertices = vec![a, b, [-650.0, 32.0, 200.0]];
        let mut adjacent = template.clone();
        adjacent.source_face += 1;
        adjacent.vertices = vec![b, c, a, [-550.0, 30.0, 240.0]];
        let repaired = stitch_surface_edges(vec![template.clone(), adjacent]);
        let q = |p: [f64; 3]| p.map(|v| v.round() as i32);
        let edges: Vec<_> = repaired
            .iter()
            .filter(|s| s.source_face == template.source_face)
            .flat_map(|s| {
                (0..s.vertices.len())
                    .map(|i| (q(s.vertices[i]), q(s.vertices[(i + 1) % s.vertices.len()])))
            })
            .collect();
        assert!(edges.contains(&(q(a), q(c))));
        assert!(edges.contains(&(q(c), q(b))));
        assert!(!edges.contains(&(q(a), q(b))));
        assert_eq!(q(c), [-585, 30, 214]);
        assert_eq!(
            stitch_surface_edges(repaired.clone()),
            repaired,
            "idempotent"
        );
    }

    #[test]
    fn stitching_preserves_area_winding_attributes_and_every_split_edge() {
        for all_edges in [false, true] {
            let mut original =
                compile_csg_surfaces(&[crate::brush::Brush::cuboid([0; 3], [64; 3])]).remove(0);
            original.vertices = vec![[0.0, 0.0, 0.0], [9.0, 0.0, 0.0], [0.0, 9.0, 0.0]];
            let mut inputs = vec![original.clone()];
            for edge in 0..if all_edges { 3 } else { 1 } {
                let a = original.vertices[edge];
                let b = original.vertices[(edge + 1) % 3];
                let middle = std::array::from_fn(|i| (a[i] + b[i]) * 0.5);
                let mut adjacent = original.clone();
                adjacent.source_brush = 99;
                adjacent.vertices = vec![b, middle, a];
                inputs.push(adjacent);
            }
            let repaired = stitch_surface_edges(inputs);
            let pieces: Vec<_> = repaired
                .iter()
                .filter(|s| s.source_brush == original.source_brush)
                .collect();
            let mut area = 0.0;
            for piece in &pieces {
                assert_eq!(piece.uv, original.uv);
                assert_eq!(piece.plane, original.plane);
                assert_eq!(piece.contents, original.contents);
                assert_eq!(piece.material, original.material);
                assert_eq!(piece.source_face, original.source_face);
                for i in 1..piece.vertices.len() - 1 {
                    let [a, b, c] = [piece.vertices[0], piece.vertices[i], piece.vertices[i + 1]];
                    let cross = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
                    assert!(cross > 0.0, "no degenerate or reversed fan triangles");
                    area += cross * 0.5;
                }
            }
            assert!((area - 40.5).abs() < 1e-9);
            for edge in 0..if all_edges { 3 } else { 1 } {
                let a = original.vertices[edge];
                let b = original.vertices[(edge + 1) % 3];
                let middle = std::array::from_fn(|i| (a[i] + b[i]) * 0.5);
                for segment in [(a, middle), (middle, b)] {
                    assert!(pieces
                        .iter()
                        .any(|p| (0..p.vertices.len())
                            .any(|i| (p.vertices[i], p.vertices[(i + 1) % p.vertices.len()])
                                == segment)));
                }
            }
        }
    }

    #[test]
    fn wide_surfaces_split_into_fans_that_draw_the_same_triangles() {
        use crate::brush::Brush;
        let square = Brush::cuboid([0, 0, 0], [64, 64, 64]);
        let mut surface = compile_csg_surfaces(&[square]).remove(0);
        // A convex 40-gon (a parabola arc); its plane is irrelevant here.
        surface.vertices = (0..40).map(|i| [i as f64, (i * i) as f64, 0.0]).collect();
        let fan = |vertices: &[[f64; 3]]| {
            (1..vertices.len() - 1)
                .map(|i| [vertices[0], vertices[i], vertices[i + 1]])
                .collect::<Vec<_>>()
        };
        let whole = fan(&surface.vertices);
        for max in [3, 4, 18, 39] {
            let pieces = split_wide_surfaces(vec![surface.clone()], max);
            assert!(pieces
                .iter()
                .all(|piece| (3..=max).contains(&piece.vertices.len())));
            assert!(pieces.iter().all(|piece| piece.plane == surface.plane));
            let drawn: Vec<_> = pieces
                .iter()
                .flat_map(|piece| fan(&piece.vertices))
                .collect();
            assert_eq!(drawn, whole, "max {max}");
        }
        let narrow = split_wide_surfaces(vec![surface.clone()], 40);
        assert_eq!(narrow.len(), 1);
        assert_eq!(narrow[0].vertices, surface.vertices);
    }

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
            edge_repair: None,
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
            edge_repair: None,
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
                    edge_repair: None,
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
