//! Streamed brush world cook (design 2026-10-08, milestone M6).
//!
//! Turns the M4 partition into the streamed PXBSP variant: a small resident
//! *top* container plus one payload per region (see
//! `psx_bsp::pxbsp_resident::stream` for the wire format and the runtime).
//!
//! The legacy cook is not touched. A project whose partition is a single
//! region goes through [`super::compile_brush_world`] and is byte identical to
//! what it always was ([`cook_project_streamed`]).
//!
//! # How the tree is built
//!
//! The partitioner's axis-aligned cut tree becomes the top of one surface
//! BSP: every cut is a node that owns no surface, and under each cell hangs
//! that cell's *own* BSP, built from the topology clipped to the cell. The
//! whole combined tree then runs through the unchanged portal, classify,
//! outside-fill and portal-flow stages, so cross-region visibility is exact
//! leaf level PVS rather than an estimate. Afterwards each subtree is packed
//! with region-local indices and the PVS rows are rewritten into the
//! canonical rank layout.
//!
//! Topology polygons lying on a cut plane are left out of the cell BSPs (the
//! cut node itself separates the two sides, and a descendant splitter equal to
//! an ancestor plane would make the portalizer drop the portal). Render
//! surfaces are clipped to cells, so a face lives in exactly one region and a
//! non-solid surface on a cut is carried by both neighbours.
//!
//! # Known limits (also in the report notes)
//!
//! * Brush movers (doors, destructibles) are refused: a mover is a submodel
//!   with its own tables, which the top container does not carry yet.
//! * Visibility is computed per region by the clustered flow
//!   ([`crate::brush_vis`]), bounded by the far-reject distance, so a world is
//!   no longer limited by one dense `u16` leaf index. The whole-world flow
//!   ([`StreamPvs::Global`]) is kept for the tests that prove the two agree and
//!   still stops at 32,767 visible leaves.
//! * The player spawn entity carries leaf 0; a streamed world locates it by
//!   position.

use std::collections::BTreeMap;
use std::path::Path;

use psx_bsp::collision::{CollisionHull, CONTENTS_SOLID};
use psx_bsp::collision_provider::select_body_hull;
use psx_bsp::pxbsp::{
    entity_class, entity_flags, PxbspEntity, PxbspIndex, PxbspLumpKind, PXBSP_LUMP_COUNT,
    PXBSP_MAX_VISIBILITY_BYTES,
};
use psx_bsp::pxbsp_resident::stream::{
    fnv1a32, RegionBuild, RegionEntry, SlotCaps, StreamingIndex, TopCounts, CONTENTS_UNRESIDENT,
    REGION_HEADER_BYTES, SECTOR_BYTES,
};
use psx_bsp::{ClipNode, Plane as WirePlane, RecordSlice, SliceReader, Vec3I16};
use psx_render_contract::CookedDrawSurface;

use super::{
    authored_body_hulls, brush_texture_dims, collision_hull_bounds, compile_brush_world,
    compile_model_surfaces, compile_model_topology_from_bsp, fit_surfaces_to_uv_window,
    mark_page_local_faces, material_tints, merge_render_rectangles, page_local_texture_dims_for,
    player_occupant_points, player_spawn_body, point_entity_origin, resolve_materials,
    scene_lights, sky_aperture_materials, subdivide_drawable_surfaces, BrushWorldCookError,
    BrushWorldCookMode, BrushWorldCookOptions, CompiledBrushTexture, CompiledBrushWorld,
    UvWindowStats,
};
use crate::brush::{Brush, Plane};
use crate::brush_collision_hulls::{compile_collision_hulls, CollisionHullBounds};
use crate::brush_compile::{
    build_surface_bsp, pack_plane, split_polygon, split_wide_surfaces, BspChild, CompiledBspLeaf,
    CompiledBspNode, CompiledSurface, CompiledSurfaceBsp, PolygonSplit,
};
use crate::brush_light::bake_brush_vertex_lighting;
use crate::brush_pack::{
    compress_visibility, intern_material, intern_plane, node_render_bounds, pack_leaf_record,
    pack_vertex, push_i16, push_u16, surface_bounds, BrushPackError, PackedBspGeometry,
};
use crate::brush_portal::point_leaf_index;
use crate::brush_pxbsp::{
    pack_entities, pack_materials, pack_runtime_planes, write_pxbsp, PxbspBuildError,
    PxbspEntityInput,
};
use crate::brush_region::geometry::{polygon_on_axis_plane, Aabb};
use crate::brush_region::{
    clip_surfaces_indexed, partition, CookMeasured, CutNode, CutTree, GateFailure, Partition,
    PartitionInput, PartitionParams,
};
use crate::brush_vis::{
    clustered_portal_rows, quake_portal_fast_rows, quake_portal_flow_rows, ClusterFlow,
};
use crate::units::ENGINE_UV_UNITS_PER_TEXEL;
use crate::{NodeKind, ProjectDocument, ResourceData};

/// Everything that can stop a streamed cook.
#[derive(Debug)]
pub enum StreamCookError {
    World(BrushWorldCookError),
    Partition(String),
    Unsupported(&'static str),
    /// A region breaks a hard wire limit.
    Limit {
        region: u32,
        what: &'static str,
        count: usize,
        max: usize,
    },
    /// A PVS row would be wider than the 1024 bytes the runtime decodes.
    RowTooWide {
        region: u32,
        ranks: usize,
        leaf_cap: usize,
    },
    /// Cut positions rounded to whole units collapsed a cell.
    CollapsedCell(usize),
    Pxbsp(PxbspBuildError),
}

impl std::fmt::Display for StreamCookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::World(error) => write!(f, "{error}"),
            Self::Partition(text) => write!(f, "partition failed: {text}"),
            Self::Unsupported(text) => write!(f, "unsupported in a streamed world: {text}"),
            Self::Limit {
                region,
                what,
                count,
                max,
            } => write!(f, "region {region}: {count} {what} exceeds the limit of {max}"),
            Self::RowTooWide {
                region,
                ranks,
                leaf_cap,
            } => write!(
                f,
                "region {region} sees {ranks} regions of {leaf_cap} leaves: its PVS row exceeds {PXBSP_MAX_VISIBILITY_BYTES} bytes"
            ),
            Self::CollapsedCell(node) => write!(f, "cut node {node} collapsed a cell when rounded"),
            Self::Pxbsp(error) => write!(f, "PXBSP assembly failed: {error:?}"),
        }
    }
}

impl std::error::Error for StreamCookError {}

impl From<BrushWorldCookError> for StreamCookError {
    fn from(error: BrushWorldCookError) -> Self {
        Self::World(error)
    }
}

impl From<BrushPackError> for StreamCookError {
    fn from(error: BrushPackError) -> Self {
        Self::World(BrushWorldCookError::Pack(error))
    }
}

impl From<PxbspBuildError> for StreamCookError {
    fn from(error: PxbspBuildError) -> Self {
        Self::Pxbsp(error)
    }
}

/// Per-region census, for the report and the tests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RegionStats {
    pub id: u32,
    pub faces: usize,
    pub vertices: usize,
    pub planes: usize,
    pub marks: usize,
    pub leaves: usize,
    pub nodes: usize,
    pub clip_nodes: usize,
    pub vis_bytes: usize,
    pub vis_count: usize,
    pub payload_bytes: usize,
    pub sectors: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamStats {
    pub regions: Vec<RegionStats>,
    pub top_nodes: usize,
    pub top_planes: usize,
    pub top_clip_nodes: usize,
    pub container_bytes: usize,
    pub pack_bytes: usize,
    pub visible_leaves: usize,
    pub portals: usize,
    /// Widest decompressed PVS row, bytes (limit 1024).
    pub widest_row_bytes: usize,
    /// Largest `|V(R)|`.
    pub max_vis_count: usize,
    pub render_surfaces: usize,
    pub clipped_pieces: usize,
}

/// The streamed cook's result: the resident top, the region pack, and the
/// pieces the equivalence tests need.
#[derive(Clone, Debug)]
pub struct StreamedBrushWorld {
    /// PXBSP v6 top container (resident tree, materials, entities, index).
    pub container: Vec<u8>,
    /// Sector aligned region payloads in disc order.
    pub region_pack: Vec<u8>,
    pub index: StreamingIndex,
    /// Wire payload of each region, by region id (unpadded).
    pub payloads: Vec<Vec<u8>>,
    pub textures: Vec<CompiledBrushTexture>,
    pub body_hulls: [psx_bsp::collision_provider::CookedBodyHull; 2],
    pub leak_path: Vec<[i32; 3]>,
    pub uv_window: UvWindowStats,
    pub stats: StreamStats,
    pub debug: StreamDebug,
}

/// Debug tables: stable per-face cook ids and the dense pre-rank PVS, which
/// let a test build an independent flat reference of the same geometry.
#[derive(Clone, Debug, Default)]
pub struct StreamDebug {
    /// `face_source[region][face]`: index of the pre-clip render surface.
    pub face_source: Vec<Vec<u32>>,
    /// `sparse_rows[region][local leaf - 1]`: the world visible-leaf ids the
    /// leaf sees, sorted. Visible leaf `dense_base[q] + local - 1` is leaf
    /// `local` of region `q`.
    pub sparse_rows: Vec<Vec<Vec<u32>>>,
    pub dense_base: Vec<usize>,
    pub dense_total: usize,
    pub regions: Vec<RegionBuild>,
    pub top: TopTables,
    pub world_bounds: ([i16; 3], [i16; 3]),
}

/// The resident top as record bytes (before the container is written).
#[derive(Clone, Debug, Default)]
pub struct TopTables {
    /// 12 byte compact planes.
    pub planes: Vec<u8>,
    pub nodes: Vec<u8>,
    pub clip_nodes: Vec<u8>,
    pub leaves: Vec<u8>,
    pub regions: usize,
    pub rpad: usize,
}

// ---- cells ----------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum CellNode {
    Split {
        axis: usize,
        pos: i64,
        low: usize,
        high: usize,
    },
    Leaf {
        region: u32,
    },
}

/// The partitioner's cut tree with every cut rounded to a whole unit, so a
/// cut is an exact integer plane for the BSP and the portalizer.
struct CellTree {
    nodes: Vec<CellNode>,
    cells: Vec<Aabb>,
    /// Cut planes above each cell, `(axis, position)`.
    ancestors: Vec<Vec<(usize, i64)>>,
}

/// Engine units beyond any authored coordinate: the outermost cells reach it,
/// so the outer faces of the world are never "on" a cell face.
const WORLD_REACH: f64 = 1.0e6;

impl CellTree {
    fn new(tree: &CutTree) -> Result<Self, StreamCookError> {
        let mut nodes = Vec::with_capacity(tree.nodes.len());
        for node in &tree.nodes {
            nodes.push(match node {
                CutNode::Split {
                    axis,
                    position,
                    low,
                    high,
                    ..
                } => CellNode::Split {
                    axis: *axis as usize,
                    pos: position.round() as i64,
                    low: *low as usize,
                    high: *high as usize,
                },
                CutNode::Leaf { region, .. } => CellNode::Leaf { region: *region },
            });
        }
        let regions = tree.region_count();
        let mut out = Self {
            nodes,
            cells: vec![Aabb::EMPTY; regions],
            ancestors: vec![Vec::new(); regions],
        };
        let root = Aabb {
            min: [-WORLD_REACH; 3],
            max: [WORLD_REACH; 3],
        };
        out.walk(0, root, &mut Vec::new())?;
        Ok(out)
    }

    fn walk(
        &mut self,
        node: usize,
        bounds: Aabb,
        path: &mut Vec<(usize, i64)>,
    ) -> Result<(), StreamCookError> {
        match self.nodes[node] {
            CellNode::Leaf { region } => {
                self.cells[region as usize] = bounds;
                self.ancestors[region as usize] = path.clone();
                Ok(())
            }
            CellNode::Split {
                axis,
                pos,
                low,
                high,
            } => {
                let at = pos as f64;
                if at <= bounds.min[axis] || at >= bounds.max[axis] {
                    return Err(StreamCookError::CollapsedCell(node));
                }
                let (below, above) = bounds.split(axis, at);
                path.push((axis, pos));
                self.walk(low, below, path)?;
                self.walk(high, above, path)?;
                path.pop();
                Ok(())
            }
        }
    }

    fn plane(axis: usize, pos: i64) -> Plane {
        let mut normal = [0i64; 3];
        normal[axis] = 1;
        Plane { normal, dist: pos }
    }

    /// Region whose cell holds `point` (half open, high side owns the cut).
    fn locate(&self, point: [f64; 3]) -> u32 {
        let mut node = 0;
        loop {
            match self.nodes[node] {
                CellNode::Leaf { region } => return region,
                CellNode::Split {
                    axis,
                    pos,
                    low,
                    high,
                } => node = if point[axis] < pos as f64 { low } else { high },
            }
        }
    }
}

// ---- the combined surface BSP ---------------------------------------------

/// Where each cell's pieces sit in the combined arrays.
#[derive(Clone, Debug, Default)]
struct CellSpans {
    nodes: std::ops::Range<usize>,
    leaves: std::ops::Range<usize>,
    root: Option<BspChild>,
}

/// One resident top node before its bounds are known.
#[derive(Clone, Copy, Debug)]
struct TopNodeDraft {
    axis: usize,
    pos: i64,
    /// `[front (high), back (low)]`.
    children: [TopChild; 2],
    bsp_index: usize,
}

#[derive(Clone, Copy, Debug)]
enum TopChild {
    Top(usize),
    Region(u32),
}

struct Combined {
    bsp: CompiledSurfaceBsp,
    spans: Vec<CellSpans>,
    top: Vec<TopNodeDraft>,
}

fn build_combined(
    cells: &CellTree,
    topology: &[CompiledSurface],
    topology_bounds: &[Aabb],
) -> Combined {
    let regions = cells.cells.len();
    // Each cell's own BSP, built independently (in parallel, results kept in
    // region order so the output never depends on scheduling).
    let region_ids: Vec<usize> = (0..regions).collect();
    let cell_bsps: Vec<CompiledSurfaceBsp> = crate::brush_region::par_map(&region_ids, |&r| {
        let mut pieces = crate::brush_region::clip_surfaces_indexed(
            topology,
            topology_bounds,
            &cells.cells[r],
            false,
        )
        .into_iter()
        .map(|(_, piece)| piece)
        .collect::<Vec<_>>();
        pieces.retain(|piece| {
            !cells.ancestors[r]
                .iter()
                .any(|&(axis, pos)| polygon_on_axis_plane(&piece.vertices, axis, pos as f64))
        });
        if pieces.is_empty() {
            CompiledSurfaceBsp {
                root: BspChild::Leaf(0),
                nodes: Vec::new(),
                leaves: vec![CompiledBspLeaf::default()],
                surfaces: Vec::new(),
            }
        } else {
            build_surface_bsp(&pieces)
        }
    });

    let mut combined = Combined {
        bsp: CompiledSurfaceBsp {
            root: BspChild::Leaf(0),
            nodes: Vec::new(),
            leaves: Vec::new(),
            surfaces: Vec::new(),
        },
        spans: vec![CellSpans::default(); regions],
        top: Vec::new(),
    };
    let mut cell_bsps: Vec<Option<CompiledSurfaceBsp>> = cell_bsps.into_iter().map(Some).collect();
    let (root, _) = emit_cell_node(cells, 0, &mut cell_bsps, &mut combined);
    combined.bsp.root = root;
    combined
}

fn emit_cell_node(
    cells: &CellTree,
    node: usize,
    cell_bsps: &mut [Option<CompiledSurfaceBsp>],
    out: &mut Combined,
) -> (BspChild, TopChild) {
    match cells.nodes[node] {
        CellNode::Leaf { region } => {
            let cell = cell_bsps[region as usize]
                .take()
                .expect("cell emitted once");
            let node_base = out.bsp.nodes.len();
            let leaf_base = out.bsp.leaves.len();
            let surface_base = out.bsp.surfaces.len();
            let shift = |child: BspChild| match child {
                BspChild::Node(i) => BspChild::Node(i + node_base),
                BspChild::Leaf(i) => BspChild::Leaf(i + leaf_base),
            };
            for n in cell.nodes {
                out.bsp.nodes.push(CompiledBspNode {
                    plane: n.plane,
                    first_surface: n.first_surface + surface_base,
                    surface_count: n.surface_count,
                    front: shift(n.front),
                    back: shift(n.back),
                });
            }
            out.bsp.leaves.extend(cell.leaves);
            out.bsp.surfaces.extend(cell.surfaces);
            let root = shift(cell.root);
            out.spans[region as usize] = CellSpans {
                nodes: node_base..out.bsp.nodes.len(),
                leaves: leaf_base..out.bsp.leaves.len(),
                root: Some(root),
            };
            (root, TopChild::Region(region))
        }
        CellNode::Split {
            axis,
            pos,
            low,
            high,
        } => {
            let index = out.bsp.nodes.len();
            out.bsp.nodes.push(CompiledBspNode {
                plane: CellTree::plane(axis, pos),
                first_surface: 0,
                surface_count: 0,
                front: BspChild::Leaf(0),
                back: BspChild::Leaf(0),
            });
            let top_index = out.top.len();
            out.top.push(TopNodeDraft {
                axis,
                pos,
                children: [TopChild::Region(0); 2],
                bsp_index: index,
            });
            // Low (back) first so region ids ascend along the arrays.
            let (back, back_top) = emit_cell_node(cells, low, cell_bsps, out);
            let (front, front_top) = emit_cell_node(cells, high, cell_bsps, out);
            out.bsp.nodes[index].front = front;
            out.bsp.nodes[index].back = back;
            out.top[top_index].children = [front_top, back_top];
            (BspChild::Node(index), TopChild::Top(top_index))
        }
    }
}

/// Hang each render surface piece under its cell root and mark the leaves it
/// reaches, replacing the topology fragments as the BSP's surface list.
/// Returns the source id of every retained face, in order.
fn assign_render_surfaces(
    combined: &mut Combined,
    cells: &CellTree,
    render: &[CompiledSurface],
    render_bounds: &[Aabb],
) -> (Vec<u32>, usize, Vec<usize>) {
    let bsp = &mut combined.bsp;
    for node in &mut bsp.nodes {
        node.first_surface = 0;
        node.surface_count = 0;
    }
    for leaf in &mut bsp.leaves {
        leaf.mark_surfaces.clear();
    }
    let region_ids: Vec<usize> = (0..cells.cells.len()).collect();
    let pieces_per_cell: Vec<Vec<(usize, CompiledSurface)>> =
        crate::brush_region::par_map(&region_ids, |&r| {
            let mut out = Vec::new();
            for (source, piece) in
                clip_surfaces_indexed(render, render_bounds, &cells.cells[r], true)
            {
                for fan in
                    split_wide_surfaces(vec![piece], psx_bsp::render::PXBSP_MAX_FACE_VERTICES)
                {
                    out.push((source, fan));
                }
            }
            out
        });
    let clipped: usize = pieces_per_cell.iter().map(Vec::len).sum();
    let mut retained = Vec::new();
    let mut ids = Vec::new();
    let mut counts = Vec::new();
    for (region, pieces) in pieces_per_cell.into_iter().enumerate() {
        let before = retained.len();
        let root = combined.spans[region].root.expect("every cell has a root");
        for (source, piece) in pieces {
            let mut reached = Vec::new();
            let mut stack = vec![(root, piece.vertices.clone())];
            while let Some((child, polygon)) = stack.pop() {
                match child {
                    BspChild::Leaf(leaf) => {
                        if bsp.leaves[leaf].contents.is_visible() && !reached.contains(&leaf) {
                            reached.push(leaf);
                        }
                    }
                    BspChild::Node(index) => {
                        let node = &bsp.nodes[index];
                        match split_polygon(&polygon, node.plane) {
                            PolygonSplit::Front(v) => stack.push((node.front, v)),
                            PolygonSplit::Back(v) => stack.push((node.back, v)),
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
            if reached.is_empty() {
                continue;
            }
            let surface_index = retained.len();
            retained.push(piece);
            ids.push(source as u32);
            reached.sort_unstable();
            for leaf in reached {
                bsp.leaves[leaf].mark_surfaces.push(surface_index);
            }
        }
        counts.push(retained.len() - before);
    }
    bsp.surfaces = retained;
    (ids, clipped, counts)
}

// ---- packing --------------------------------------------------------------

struct Ctx<'a> {
    bsp: &'a CompiledSurfaceBsp,
    spans: &'a [CellSpans],
    node_bounds: Vec<([i16; 3], [i16; 3])>,
    /// Surface index range of each region in `bsp.surfaces`.
    surface_spans: Vec<std::ops::Range<usize>>,
    lighting: Option<&'a [Vec<u32>]>,
    texture_dims: &'a std::collections::HashMap<Option<crate::ResourceId>, [u16; 2]>,
    material_slots: Vec<Option<crate::ResourceId>>,
    /// Dense base of each region (visible-leaf order), and the region of
    /// every dense id.
    dense_base: Vec<usize>,
    region_of_dense: Vec<u16>,
    /// Host leaf to world visible-leaf id plus one (zero: not visible).
    dense_of_leaf: Vec<u32>,
    /// Visible-leaf ids each visible leaf sees, sorted.
    rows: Vec<Vec<u32>>,
    leaf_cap: usize,
    hull_bounds: [CollisionHullBounds; 3],
    brushes: &'a [Brush],
    cells_box: Vec<Aabb>,
}

struct RegionPacked {
    build: RegionBuild,
    face_source: Vec<u32>,
    sparse_rows: Vec<Vec<u32>>,
    /// Raw 14 byte collision planes and clip nodes for the spawn check.
    hull: (Vec<u8>, Vec<u8>, [i16; 2]),
}

fn leaf_code(number: i16) -> i16 {
    -1 - number
}

/// Dense-row bit scan.
fn for_each_bit(row: &[u8], mut f: impl FnMut(usize)) {
    for (byte_index, &byte) in row.iter().enumerate() {
        if byte == 0 {
            continue;
        }
        for bit in 0..8 {
            if byte & (1 << bit) != 0 {
                f(byte_index * 8 + bit);
            }
        }
    }
}

fn pack_region(
    ctx: &mut Ctx<'_>,
    region: usize,
    face_ids: &[u32],
) -> Result<RegionPacked, StreamCookError> {
    let bsp = ctx.bsp;
    let span = ctx.spans[region].clone();
    let surfaces = ctx.surface_spans[region].clone();
    let region_u = region as u32;
    let mut planes: Vec<[u8; 14]> = Vec::new();

    // Faces and vertices.
    let mut vertices = Vec::new();
    let mut faces = Vec::new();
    for (local, surface_index) in surfaces.clone().enumerate() {
        let surface = &bsp.surfaces[surface_index];
        let first_vertex = vertices.len() / 12;
        let (plane_record, flipped) =
            pack_plane(&surface.plane).ok_or(BrushPackError::InvalidPlane(surface_index))?;
        let plane_index = intern_plane(&mut planes, plane_record)?;
        let texture_index = intern_material(&mut ctx.material_slots, surface.material)?;
        let mut uvs: Vec<[f64; 2]> = surface
            .vertices
            .iter()
            .map(|&vertex| {
                let raw = crate::brush::paraxial_uv(&surface.plane, vertex);
                surface.uv.apply([
                    raw[0] / ENGINE_UV_UNITS_PER_TEXEL,
                    raw[1] / ENGINE_UV_UNITS_PER_TEXEL,
                ])
            })
            .collect();
        let page_local = if let Some(dims) = ctx.texture_dims.get(&surface.material) {
            crate::brush::rebase_texel_uvs(
                &mut uvs,
                [f64::from(dims[0].max(1)), f64::from(dims[1].max(1))],
            );
            uvs.iter().all(|uv| {
                uv.iter().zip(dims).all(|(&value, &size)| {
                    value.is_finite()
                        && value.round() >= 0.0
                        && value.round() < f64::from(size.max(1))
                })
            })
        } else {
            false
        };
        for (vertex_index, vertex) in surface.vertices.iter().copied().enumerate() {
            let light = match ctx.lighting {
                Some(colors) => colors[surface_index][vertex_index],
                None => 0x00ff_ffff,
            };
            pack_vertex(
                &mut vertices,
                surface_index,
                vertex_index,
                vertex,
                uvs[vertex_index],
                light,
            )?;
        }
        let flags = psx_bsp::FACE_BAKED_LIGHT
            | if page_local {
                psx_bsp::FACE_PAGE_LOCAL_UV
            } else {
                0
            }
            | if flipped { psx_bsp::FACE_BACKSIDE } else { 0 }
            | if surface.contents.is_solid() {
                0
            } else {
                psx_bsp::FACE_TWO_SIDED
            };
        faces.extend_from_slice(
            &CookedDrawSurface {
                plane: plane_index as u16,
                first_corner: first_vertex as u16,
                material: texture_index as u16,
                flags: flags as u8,
                corner_count: surface.vertices.len() as u8,
                light_styles: [0, 64],
            }
            .encode(),
        );
        debug_assert_eq!(local, surface_index - surfaces.start);
    }

    // Local leaf numbering: the region's visible leaves in host order.
    let mut leaf_number = vec![0i16; span.leaves.len()];
    let mut visible_hosts = Vec::new();
    for host in span.leaves.clone() {
        if bsp.leaves[host].contents.is_visible() {
            visible_hosts.push(host);
            leaf_number[host - span.leaves.start] = visible_hosts.len() as i16;
        }
    }
    if visible_hosts.len() > ctx.leaf_cap {
        return Err(StreamCookError::Limit {
            region: region_u,
            what: "leaves",
            count: visible_hosts.len(),
            max: ctx.leaf_cap,
        });
    }

    // PVS rows in the canonical rank layout.
    let leaf_cap = ctx.leaf_cap;
    let regions = ctx.spans.len();
    let mut seen = vec![false; regions];
    seen[region] = true;
    for &host in &visible_hosts {
        for &target in &ctx.rows[ctx.dense_of_leaf[host] as usize - 1] {
            seen[ctx.region_of_dense[target as usize] as usize] = true;
        }
    }
    let mut list: Vec<u16> = vec![region as u16];
    list.extend(
        (0..regions)
            .filter(|&q| q != region && seen[q])
            .map(|q| q as u16),
    );
    if list.len() * leaf_cap / 8 > PXBSP_MAX_VISIBILITY_BYTES {
        return Err(StreamCookError::RowTooWide {
            region: region_u,
            ranks: list.len(),
            leaf_cap,
        });
    }
    let mut rank_of = vec![u16::MAX; regions];
    for (rank, &q) in list.iter().enumerate() {
        rank_of[q as usize] = rank as u16;
    }
    let row_bytes = list.len() * leaf_cap / 8;
    let mut vis = Vec::new();
    let mut interned = BTreeMap::<Vec<u8>, i32>::new();
    let mut offsets = Vec::with_capacity(visible_hosts.len());
    let mut sparse_rows = Vec::with_capacity(visible_hosts.len());
    for &host in &visible_hosts {
        let row = &ctx.rows[ctx.dense_of_leaf[host] as usize - 1];
        let mut ranked = vec![0u8; row_bytes];
        for &bit in row {
            let q = ctx.region_of_dense[bit as usize] as usize;
            let local = bit as usize - ctx.dense_base[q];
            let target = rank_of[q] as usize * leaf_cap + local;
            ranked[target >> 3] |= 1 << (target & 7);
        }
        let compressed = compress_visibility(&ranked);
        let offset = *interned.entry(compressed.clone()).or_insert_with(|| {
            let at = vis.len() as i32;
            vis.extend_from_slice(&compressed);
            at
        });
        offsets.push(offset);
        sparse_rows.push(row.clone());
    }

    // Marks and leaf records.
    let mut marks = Vec::new();
    let mut leaves = Vec::new();
    for (index, &host) in visible_hosts.iter().enumerate() {
        let leaf = &bsp.leaves[host];
        let first_mark = marks.len() / 2;
        for &surface in &leaf.mark_surfaces {
            if !surfaces.contains(&surface) {
                return Err(StreamCookError::Unsupported(
                    "a leaf marks a face of another region",
                ));
            }
            push_u16(&mut marks, (surface - surfaces.start) as u16);
        }
        pack_leaf_record(
            &mut leaves,
            leaf.contents.runtime_contents(),
            offsets[index],
            first_mark as u16,
            leaf.mark_surfaces.len() as u16,
        )?;
    }

    // Render nodes.
    let child = |c: BspChild| -> Result<i16, StreamCookError> {
        match c {
            BspChild::Node(i) if span.nodes.contains(&i) => Ok((i - span.nodes.start) as i16),
            BspChild::Leaf(h) if span.leaves.contains(&h) => {
                Ok(leaf_code(leaf_number[h - span.leaves.start]))
            }
            _ => Err(StreamCookError::Unsupported("a cell node leaves its cell")),
        }
    };
    let mut nodes = Vec::new();
    for host in span.nodes.clone() {
        let node = &bsp.nodes[host];
        let (record, flipped) =
            pack_plane(&node.plane).ok_or(BrushPackError::InvalidPlane(host))?;
        let plane = intern_plane(&mut planes, record)?;
        push_u16(&mut nodes, plane as u16);
        let mut children = [child(node.front)?, child(node.back)?];
        if flipped {
            children.swap(0, 1);
        }
        push_i16(&mut nodes, children[0]);
        push_i16(&mut nodes, children[1]);
        let (mins, maxs) = ctx.node_bounds[host];
        for value in mins {
            nodes.push(psx_bsp::encode_node_bound_min(value) as u8);
        }
        for value in maxs {
            nodes.push(psx_bsp::encode_node_bound_max(value) as u8);
        }
        push_u16(&mut nodes, 0);
        push_u16(&mut nodes, 0);
    }
    let render_root = child(ctx.spans[region].root.expect("root"))?;

    // Collision: the shipping hull compiler over every brush that can touch
    // the cell, so each point of the cell reads exactly what the whole map
    // would. Brushes straddling a cut are carried by both sides.
    let margin = ctx.hull_bounds[1..]
        .iter()
        .flat_map(|h| h.mins.iter().chain(&h.maxs))
        .map(|v| f64::from(v.abs()))
        .fold(0.0, f64::max)
        + 2.0;
    let cell_brushes = brushes_for_cell(ctx.brushes, &ctx.cells_box[region], margin);
    let hulls = compile_collision_hulls(&cell_brushes, &ctx.hull_bounds[1..])
        .map_err(BrushWorldCookError::Collision)?;
    let mut clip_nodes = Vec::new();
    let mut remap = Vec::new();
    for record in hulls.planes.chunks_exact(14) {
        let mut fixed = [0u8; 14];
        fixed.copy_from_slice(record);
        remap.push(intern_plane(&mut planes, fixed)?);
    }
    for node in hulls.clipnodes.chunks_exact(6) {
        let plane = i16::from_le_bytes([node[0], node[1]]);
        let mapped = *remap
            .get(plane as usize)
            .ok_or(StreamCookError::Unsupported("clip node plane out of range"))?;
        push_i16(&mut clip_nodes, mapped);
        clip_nodes.extend_from_slice(&node[2..6]);
    }
    let clip_roots = [hulls.head_nodes[0], hulls.head_nodes[1]];

    let planes_flat: Vec<u8> = planes.iter().flatten().copied().collect();
    let build = RegionBuild {
        id: region as u16,
        vis_count: list.len() as u16,
        planes: pack_runtime_planes(&planes_flat)?,
        vertices,
        faces,
        marks,
        leaves,
        nodes,
        clip_nodes,
        vis,
        render_root,
        clip_roots,
    };
    let face_source = surfaces.clone().map(|i| face_ids[i]).collect();
    Ok(RegionPacked {
        build,
        face_source,
        sparse_rows,
        hull: (hulls.planes, hulls.clipnodes, clip_roots),
    })
}

fn brushes_for_cell(brushes: &[Brush], cell: &Aabb, margin: f64) -> Vec<Brush> {
    let grown = cell.expanded(margin);
    brushes
        .iter()
        .filter(|brush| {
            let solved = brush.solve();
            let bounds = Aabb {
                min: solved.min,
                max: solved.max,
            };
            bounds.overlaps(&grown)
        })
        .cloned()
        .collect()
}

fn next_leaf_cap(leaves: usize) -> usize {
    leaves.max(8).next_power_of_two()
}

fn clamp_i16(value: f64) -> i16 {
    value.clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
}

fn check_limit(
    region: u32,
    what: &'static str,
    count: usize,
    max: usize,
) -> Result<(), StreamCookError> {
    if count > max {
        Err(StreamCookError::Limit {
            region,
            what,
            count,
            max,
        })
    } else {
        Ok(())
    }
}

/// How the cooker computes leaf visibility.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StreamPvs {
    /// Per-region flow bounded by the far-reject distance `reach` (engine
    /// units). Scales to any world: the working set is one neighbourhood.
    Clustered { reach: f64 },
    /// One flow over every visible leaf, as the whole-map cook does. Limited
    /// to 32,767 visible leaves and quadratic in their number; kept so the
    /// tests can prove the clustered flow against it.
    Global,
}

/// Cook an engine-unit project as a streamed world over `tree`'s cells.
/// `order` is the disc order of the regions (the layout the partitioner chose).
pub fn compile_brush_world_streamed(
    project: &ProjectDocument,
    options: BrushWorldCookOptions<'_>,
    tree: &CutTree,
    order: &[u32],
    pvs: StreamPvs,
) -> Result<StreamedBrushWorld, StreamCookError> {
    let scene = project.active_scene();
    let body_hulls = authored_body_hulls(project);
    let hull_bounds = collision_hull_bounds(body_hulls);
    let mut static_brushes = Vec::new();
    for (brush_index, brush) in scene.brushes.iter().enumerate() {
        if !brush.solve().is_valid() {
            return Err(BrushWorldCookError::InvalidBrush {
                brush: brush_index,
                face: None,
            }
            .into());
        }
        if brush.mover.is_some() {
            return Err(StreamCookError::Unsupported(
                "brush movers (doors, destructibles) have no top-container submodel yet",
            ));
        }
        static_brushes.push(brush.clone());
    }
    if static_brushes.is_empty() {
        return Err(BrushWorldCookError::EmptyStaticWorld.into());
    }
    let all_brushes: Vec<Brush> = scene
        .brushes
        .iter()
        .filter(|brush| brush.contents.is_solid())
        .cloned()
        .collect();
    let (lights, light_nodes) = scene_lights(scene);
    let tints = material_tints(project);
    let texture_dims = brush_texture_dims(project, scene, &options);
    let uv_skip = sky_aperture_materials(project);
    let patch_extent = match &scene.node(crate::NodeId::ROOT).unwrap().kind {
        NodeKind::World { culling, .. } => culling.bsp_patch_extent.clamp(64, 256) as f64,
        _ => crate::units::ENGINE_SURFACE_EXTENT_UNITS,
    };
    let occupant_points = player_occupant_points(scene);

    // Surfaces, exactly as the whole-map cook prepares them, minus the global
    // face budget (the budget is per region here).
    let cells = CellTree::new(tree)?;
    let (topology, render) = compile_model_surfaces(&static_brushes);
    let light_spheres: Vec<_> = lights.iter().map(|l| (l.position, l.radius)).collect();
    let render = subdivide_drawable_surfaces(
        merge_render_rectangles(render),
        &uv_skip,
        patch_extent,
        usize::MAX,
        &light_spheres,
    );
    let (render, uv_window) = fit_surfaces_to_uv_window(render, &texture_dims, &uv_skip);
    let render = split_wide_surfaces(render, psx_bsp::render::PXBSP_MAX_FACE_VERTICES);
    let topology_bounds: Vec<Aabb> = topology
        .iter()
        .map(|s| Aabb::from_points(&s.vertices))
        .collect();
    let render_bounds: Vec<Aabb> = render
        .iter()
        .map(|s| Aabb::from_points(&s.vertices))
        .collect();

    // One BSP: cuts on top, each cell's own tree below.
    let mut combined = build_combined(&cells, &topology, &topology_bounds);
    let raw = std::mem::replace(
        &mut combined.bsp,
        CompiledSurfaceBsp {
            root: BspChild::Leaf(0),
            nodes: Vec::new(),
            leaves: Vec::new(),
            surfaces: Vec::new(),
        },
    );
    let (bsp, portals, leak) =
        compile_model_topology_from_bsp(raw, &static_brushes, &occupant_points, true);
    combined.bsp = bsp;
    let (face_ids, clipped_pieces, surface_counts) =
        assign_render_surfaces(&mut combined, &cells, &render, &render_bounds);
    let Combined { bsp, spans, top } = combined;
    let regions = cells.cells.len();

    // Dense runtime numbering of the visible leaves, region by region.
    let mut dense_of_leaf = vec![0u32; bsp.leaves.len()];
    let mut dense_base = vec![0usize; regions];
    let mut region_of_dense: Vec<u16> = Vec::new();
    for r in 0..regions {
        dense_base[r] = region_of_dense.len();
        for host in spans[r].leaves.clone() {
            if bsp.leaves[host].contents.is_visible() {
                region_of_dense.push(r as u16);
                dense_of_leaf[host] = region_of_dense.len() as u32;
            }
        }
    }
    let visible = region_of_dense.len();
    if visible == 0 {
        return Err(BrushPackError::EmptyWorld.into());
    }
    let rows = match pvs {
        StreamPvs::Clustered { reach } => {
            let cluster_of: Vec<u32> = region_of_dense.iter().map(|&r| u32::from(r)).collect();
            let (rows, _) = clustered_portal_rows(&ClusterFlow {
                bsp: &bsp,
                portals: &portals,
                dense_of_leaf: &dense_of_leaf,
                visible,
                cluster_of: &cluster_of,
                clusters: regions,
                reach,
                exact: true,
            });
            rows
        }
        StreamPvs::Global => {
            if visible > i16::MAX as usize {
                return Err(StreamCookError::Unsupported(
                    "more than 32767 visible leaves for the whole-world flow; use the clustered flow",
                ));
            }
            let mapping: Vec<i16> = dense_of_leaf.iter().map(|&d| d as i16).collect();
            let dense_rows = if visible <= 512 && portals.len() <= 10_000
                || options.mode == BrushWorldCookMode::Release
            {
                quake_portal_flow_rows(&bsp, &portals, &mapping, visible)
            } else {
                quake_portal_fast_rows(&bsp, &portals, &mapping, visible)
            };
            let mut rows = vec![Vec::new(); visible];
            for (host, row) in dense_rows.iter().enumerate() {
                if let Some(row) = row {
                    let leaf = dense_of_leaf[host] as usize - 1;
                    for_each_bit(row, |bit| rows[leaf].push(bit as u32));
                }
            }
            rows
        }
    };

    let lighting = if lights.is_empty() {
        None
    } else {
        let occluders: &[Brush] = match options.mode {
            BrushWorldCookMode::Draft => &[],
            BrushWorldCookMode::Release => &all_brushes,
        };
        Some(
            bake_brush_vertex_lighting(&bsp.surfaces, occluders, options.ambient, &lights, &tints)
                .map_err(|error| BrushWorldCookError::Light {
                    node: match error {
                        crate::brush_light::BrushLightError::InvalidLight(index) => {
                            light_nodes.get(index).copied()
                        }
                    },
                    error,
                })?,
        )
    };

    let leaf_cap = next_leaf_cap(
        (0..regions)
            .map(|r| {
                spans[r]
                    .leaves
                    .clone()
                    .filter(|&h| bsp.leaves[h].contents.is_visible())
                    .count()
            })
            .max()
            .unwrap_or(0),
    );
    let mut surface_spans = Vec::with_capacity(regions);
    let mut at = 0;
    for count in &surface_counts {
        surface_spans.push(at..at + count);
        at += count;
    }
    let node_bounds: Vec<_> = node_render_bounds(&bsp)
        .into_iter()
        .map(|b| b.packed())
        .collect();
    let mut ctx = Ctx {
        bsp: &bsp,
        spans: &spans,
        node_bounds,
        surface_spans,
        lighting: lighting.as_deref(),
        texture_dims: &texture_dims,
        material_slots: Vec::new(),
        dense_base: dense_base.clone(),
        region_of_dense,
        dense_of_leaf,
        rows,
        leaf_cap,
        hull_bounds,
        brushes: &static_brushes,
        cells_box: cells.cells.clone(),
    };
    let mut packed = Vec::with_capacity(regions);
    for r in 0..regions {
        packed.push(pack_region(&mut ctx, r, &face_ids)?);
    }
    let slots = ctx.material_slots.clone();

    // Page-local texture promotion sees every region's faces at once.
    let mut geometries: Vec<PackedBspGeometry> = packed
        .iter_mut()
        .map(|p| PackedBspGeometry {
            vertices: std::mem::take(&mut p.build.vertices),
            planes: Vec::new(),
            faces: std::mem::take(&mut p.build.faces),
            mark_surfaces: Vec::new(),
            visibility: Vec::new(),
            leaves: Vec::new(),
            nodes: Vec::new(),
            material_slots: slots.clone(),
            root_node: 0,
            visible_leaves: 0,
            mins: [0; 3],
            maxs: [0; 3],
        })
        .collect();
    let refs: Vec<&PackedBspGeometry> = geometries.iter().collect();
    let page_dims =
        page_local_texture_dims_for(project, &slots, &refs, &texture_dims, options.project_root);
    for geometry in &mut geometries {
        mark_page_local_faces(geometry, &page_dims);
    }
    for (p, geometry) in packed.iter_mut().zip(geometries) {
        p.build.vertices = geometry.vertices;
        p.build.faces = geometry.faces;
    }
    let (materials, textures) = resolve_materials(project, &slots, &options, &page_dims)?;

    // Wire limits, per region.
    for (r, p) in packed.iter().enumerate() {
        let b = &p.build;
        let r = r as u32;
        check_limit(r, "faces", b.faces.len() / 10, 0xffff)?;
        check_limit(r, "vertices", b.vertices.len() / 12, 0xffff)?;
        check_limit(r, "marks", b.marks.len() / 2, 0xffff)?;
        check_limit(r, "planes", b.planes.len() / 12, 0x7fff)?;
        check_limit(r, "nodes", b.nodes.len() / 16, 0x7fff)?;
        check_limit(r, "clip nodes", b.clip_nodes.len() / 6, 0x7fff)?;
    }

    // The resident top.
    let top_n = top.len();
    let mut top_planes: Vec<[u8; 14]> = Vec::new();
    let mut top_plane_of = Vec::with_capacity(top_n);
    for node in &top {
        let (record, flipped) = pack_plane(&CellTree::plane(node.axis, node.pos))
            .ok_or(BrushPackError::InvalidPlane(node.bsp_index))?;
        debug_assert!(!flipped, "a positive axial plane never flips");
        top_plane_of.push(intern_plane(&mut top_planes, record)?);
    }
    let stub = |region: u32| -(region as i16 + 2);
    let mut parents = vec![[0u16; 3]; regions];
    let mut sides = vec![0u8; regions];
    let mut top_nodes = Vec::new();
    for (t, node) in top.iter().enumerate() {
        push_u16(&mut top_nodes, top_plane_of[t] as u16);
        for child in node.children {
            push_i16(
                &mut top_nodes,
                match child {
                    TopChild::Top(i) => i as i16,
                    TopChild::Region(r) => stub(r),
                },
            );
        }
        let (mins, maxs) = ctx.node_bounds[node.bsp_index];
        for value in mins {
            top_nodes.push(psx_bsp::encode_node_bound_min(value) as u8);
        }
        for value in maxs {
            top_nodes.push(psx_bsp::encode_node_bound_max(value) as u8);
        }
        push_u16(&mut top_nodes, 0);
        push_u16(&mut top_nodes, 0);
        for (side, child) in node.children.into_iter().enumerate() {
            if let TopChild::Region(r) = child {
                parents[r as usize][0] = t as u16;
                sides[r as usize] |= side as u8;
            }
        }
    }
    // Clip: one sentinel node (the model record's collision head zero), then
    // the hull 1 and hull 2 top trees.
    let mut top_clip = Vec::new();
    push_i16(&mut top_clip, 0);
    push_i16(&mut top_clip, psx_bsp::collision::CONTENTS_EMPTY);
    push_i16(&mut top_clip, psx_bsp::collision::CONTENTS_EMPTY);
    for hull in 0..2usize {
        let base = 1 + hull * top_n;
        for (t, node) in top.iter().enumerate() {
            push_i16(&mut top_clip, top_plane_of[t]);
            for (side, child) in node.children.into_iter().enumerate() {
                push_i16(
                    &mut top_clip,
                    match child {
                        TopChild::Top(i) => (base + i) as i16,
                        TopChild::Region(r) => {
                            parents[r as usize][1 + hull] = (base + t) as u16;
                            sides[r as usize] |= (side as u8) << (1 + hull);
                            CONTENTS_SOLID
                        }
                    },
                );
            }
        }
    }
    let rpad = regions.div_ceil(8) * 8;
    let mut top_leaves = Vec::new();
    pack_leaf_record(&mut top_leaves, CONTENTS_SOLID, -1, 0, 0)?;
    for r in 0..regions {
        pack_leaf_record(&mut top_leaves, CONTENTS_UNRESIDENT, -1, 0, 0)?;
        let at = top_leaves.len() - 4;
        top_leaves[at..at + 2].copy_from_slice(&(r as u16).to_le_bytes());
    }
    for _ in regions..rpad {
        pack_leaf_record(&mut top_leaves, CONTENTS_SOLID, -1, 0, 0)?;
    }
    let top_plane_bytes: Vec<u8> = top_planes.iter().flatten().copied().collect();
    let top_planes_compact = pack_runtime_planes(&top_plane_bytes)?;
    let world_bounds = surface_bounds(&bsp.surfaces);

    // Entities.
    let character_resources: Vec<_> = project
        .resources
        .iter()
        .filter_map(|resource| match &resource.data {
            ResourceData::Character(character) => Some(character),
            _ => None,
        })
        .collect();
    let mut entities = Vec::new();
    for node in scene.nodes() {
        if !matches!(node.kind, NodeKind::SpawnPoint { player: true, .. }) {
            continue;
        }
        let origin = point_entity_origin(node.id, node.transform.translation)?;
        let point = [
            f64::from(origin.x) / 4096.0,
            f64::from(origin.y) / 4096.0,
            f64::from(origin.z) / 4096.0,
        ];
        let leaf = point_leaf_index(&bsp, point);
        if !bsp.leaves[leaf].contents.is_visible() {
            return Err(BrushWorldCookError::PlayerSpawnInSolid(node.id).into());
        }
        if let Some((radius, height)) = player_spawn_body(project, node, &character_resources) {
            let hull_index = select_body_hull(&body_hulls, radius, height)
                .ok_or(BrushWorldCookError::PlayerSpawnInSolid(node.id))?;
            let region = cells.locate(point) as usize;
            let (planes, clipnodes, heads) = &packed[region].hull;
            let head = *heads
                .get(hull_index.saturating_sub(1))
                .ok_or(BrushWorldCookError::InvalidWorldTree)?;
            let wire_planes = RecordSlice::<WirePlane>::new(planes)
                .ok_or(BrushWorldCookError::InvalidWorldTree)?;
            let wire_nodes = RecordSlice::<ClipNode>::new(clipnodes)
                .ok_or(BrushWorldCookError::InvalidWorldTree)?;
            if CollisionHull::new(wire_planes, wire_nodes, head)
                .and_then(|hull| hull.point_contents(origin))
                .is_none_or(|contents| contents == CONTENTS_SOLID)
            {
                return Err(BrushWorldCookError::PlayerSpawnInSolid(node.id).into());
            }
        }
        let angles = node
            .transform
            .rotation_degrees
            .map(|degrees| crate::spatial::euler_degrees_to_q12(degrees) as i16);
        entities.push(PxbspEntityInput {
            entity: PxbspEntity {
                class_id: entity_class::PLAYER_SPAWN,
                flags: entity_flags::ENABLED,
                model: u16::MAX,
                leaf: 0,
                origin,
                angles: Vec3I16 {
                    x: angles[0],
                    y: angles[1],
                    z: angles[2],
                },
                ..PxbspEntity::default()
            },
            payload: Vec::new(),
        });
    }

    // Payloads, disc order, directory.
    let mut disc: Vec<u32> = order.to_vec();
    {
        let mut check = disc.clone();
        check.sort_unstable();
        if check != (0..regions as u32).collect::<Vec<_>>() {
            disc = (0..regions as u32).collect();
        }
    }
    let payloads: Vec<Vec<u8>> = packed.iter().map(|p| p.build.encode()).collect();
    let mut entries = vec![RegionEntry::default(); regions];
    let mut region_pack = Vec::new();
    for &r in &disc {
        let blob = &payloads[r as usize];
        let sector = (region_pack.len() / SECTOR_BYTES as usize) as u32;
        region_pack.extend_from_slice(blob);
        region_pack.resize(
            region_pack.len().div_ceil(SECTOR_BYTES as usize) * SECTOR_BYTES as usize,
            0,
        );
        entries[r as usize].sector_start = sector;
    }
    let mut vis_lists = Vec::new();
    let mut max_vis = 0;
    let mut widest = 0;
    for r in 0..regions {
        let b = &packed[r].build;
        let cell = &cells.cells[r];
        entries[r] = RegionEntry {
            sector_start: entries[r].sector_start,
            payload_bytes: payloads[r].len() as u32,
            fnv: fnv1a32(&payloads[r]),
            mins: [
                clamp_i16(cell.min[0]),
                clamp_i16(cell.min[1]),
                clamp_i16(cell.min[2]),
            ],
            maxs: [
                clamp_i16(cell.max[0]),
                clamp_i16(cell.max[1]),
                clamp_i16(cell.max[2]),
            ],
            parents: parents[r],
            sides: sides[r],
            vis_offset: vis_lists.len() as u32,
            vis_count: b.vis_count,
        };
        // The list is recoverable from the rank layout's construction: region
        // first, then the rest ascending. Rebuild it the same way.
        vis_lists.extend(region_list(&ctx, r));
        max_vis = max_vis.max(b.vis_count as usize);
        widest = widest.max(b.vis_count as usize * leaf_cap / 8);
    }
    let caps = SlotCaps {
        faces: packed
            .iter()
            .map(|p| p.build.faces.len() / 10)
            .max()
            .unwrap_or(0)
            .max(1) as u16,
        vertices: packed
            .iter()
            .map(|p| p.build.vertices.len() / 12)
            .max()
            .unwrap_or(0)
            .max(1) as u16,
        planes: packed
            .iter()
            .map(|p| p.build.planes.len() / 12)
            .max()
            .unwrap_or(0)
            .max(1) as u16,
        marks: packed
            .iter()
            .map(|p| p.build.marks.len() / 2)
            .max()
            .unwrap_or(0)
            .max(1) as u16,
        nodes: packed
            .iter()
            .map(|p| p.build.nodes.len() / 16)
            .max()
            .unwrap_or(0)
            .max(1) as u16,
        clip_nodes: packed
            .iter()
            .map(|p| p.build.clip_nodes.len() / 6)
            .max()
            .unwrap_or(0)
            .max(1) as u16,
        leaves: leaf_cap as u16,
        vis_bytes: packed
            .iter()
            .map(|p| p.build.vis.len())
            .max()
            .unwrap_or(0)
            .max(1) as u32,
    };
    let index = StreamingIndex {
        caps,
        top: TopCounts {
            planes: top_planes.len() as u16,
            vertices: 0,
            faces: 0,
            marks: 0,
            leaves: (1 + rpad) as u16,
            nodes: top_n as u16,
            clip_nodes: (1 + 2 * top_n) as u16,
        },
        regions: entries,
        vis_lists,
    };

    let mut model = Vec::new();
    crate::brush_pxbsp::pack_vec3_i16(&mut model, world_bounds.0);
    crate::brush_pxbsp::pack_vec3_i16(&mut model, world_bounds.1);
    crate::brush_pxbsp::pack_vec3_i16(&mut model, [0; 3]);
    for head in [0i16, 0, 1, 1 + top_n as i16] {
        push_i16(&mut model, head);
    }
    push_i16(&mut model, 0);
    push_u16(&mut model, 0);
    push_u16(&mut model, 0);

    let mut lumps: [Vec<u8>; PXBSP_LUMP_COUNT] = core::array::from_fn(|_| Vec::new());
    lumps[PxbspLumpKind::Planes as usize] = top_planes_compact.clone();
    lumps[PxbspLumpKind::Materials as usize] = pack_materials(&materials)?;
    lumps[PxbspLumpKind::Leaves as usize] = top_leaves.clone();
    lumps[PxbspLumpKind::Nodes as usize] = top_nodes.clone();
    lumps[PxbspLumpKind::ClipNodes as usize] = top_clip.clone();
    lumps[PxbspLumpKind::Models as usize] = model;
    lumps[PxbspLumpKind::Entities as usize] = pack_entities(&entities)?;
    lumps[PxbspLumpKind::StreamingIndex as usize] = index.encode();
    let container = write_pxbsp(&lumps)?;

    let mut stats = StreamStats {
        top_nodes: top_n,
        top_planes: top_planes.len(),
        top_clip_nodes: 1 + 2 * top_n,
        container_bytes: container.len(),
        pack_bytes: region_pack.len(),
        visible_leaves: visible,
        portals: portals.len(),
        widest_row_bytes: widest,
        max_vis_count: max_vis,
        render_surfaces: render.len(),
        clipped_pieces,
        ..StreamStats::default()
    };
    for (r, p) in packed.iter().enumerate() {
        let b = &p.build;
        stats.regions.push(RegionStats {
            id: r as u32,
            faces: b.faces.len() / 10,
            vertices: b.vertices.len() / 12,
            planes: b.planes.len() / 12,
            marks: b.marks.len() / 2,
            leaves: b.leaves.len() / 14,
            nodes: b.nodes.len() / 16,
            clip_nodes: b.clip_nodes.len() / 6,
            vis_bytes: b.vis.len(),
            vis_count: b.vis_count as usize,
            payload_bytes: payloads[r].len(),
            sectors: payloads[r].len().div_ceil(SECTOR_BYTES as usize),
        });
    }
    let _ = REGION_HEADER_BYTES;
    let debug = StreamDebug {
        face_source: packed.iter().map(|p| p.face_source.clone()).collect(),
        sparse_rows: packed.iter().map(|p| p.sparse_rows.clone()).collect(),
        dense_base,
        dense_total: visible,
        regions: packed.into_iter().map(|p| p.build).collect(),
        top: TopTables {
            planes: top_planes_compact,
            nodes: top_nodes,
            clip_nodes: top_clip,
            leaves: top_leaves,
            regions,
            rpad,
        },
        world_bounds,
    };
    Ok(StreamedBrushWorld {
        container,
        region_pack,
        index,
        payloads,
        textures,
        body_hulls,
        leak_path: leak.path,
        uv_window,
        stats,
        debug,
    })
}

/// `V(R)` of a region as the rank layout was built: itself first, then every
/// other region that holds a leaf some leaf of `R` sees, ascending.
fn region_list(ctx: &Ctx<'_>, region: usize) -> Vec<u16> {
    let regions = ctx.spans.len();
    let mut seen = vec![false; regions];
    seen[region] = true;
    for host in ctx.spans[region].leaves.clone() {
        let dense = ctx.dense_of_leaf[host];
        if dense > 0 {
            for &target in &ctx.rows[dense as usize - 1] {
                seen[ctx.region_of_dense[target as usize] as usize] = true;
            }
        }
    }
    let mut list = vec![region as u16];
    list.extend(
        (0..regions)
            .filter(|&q| q != region && seen[q])
            .map(|q| q as u16),
    );
    list
}

/// What a cook produced for one project.
pub enum CookedWorld {
    /// One region: the ordinary whole-map cook, byte for byte.
    Whole(Box<CompiledBrushWorld>),
    Streamed(Box<StreamedBrushWorld>),
}

/// Partition an authored project and cook it: a project that fits one region
/// yields exactly the whole-map cook, a larger one the streamed variant.
pub fn cook_project_streamed(
    authored: &ProjectDocument,
    project_root: &Path,
    mode: BrushWorldCookMode,
    ambient: [u8; 3],
    params: &PartitionParams,
) -> Result<CookedWorld, StreamCookError> {
    let options = BrushWorldCookOptions {
        project_root,
        mode,
        ambient,
        texture_asset_base: 0,
        collision_hulls: whole_map_hull_strategy(authored),
    };
    cook_project_streamed_with(authored, options, params)
}

/// The tree builder the whole-map cook uses for this project. A streamed
/// cook builds each cell's hulls with the spatial chains instead; only the
/// one-region fallback reads this.
fn whole_map_hull_strategy(
    authored: &ProjectDocument,
) -> crate::brush_collision_hulls::CollisionHullStrategy {
    if authored.collision_hull_bsp {
        crate::brush_collision_hulls::CollisionHullStrategy::HullBsp
    } else {
        crate::brush_collision_hulls::CollisionHullStrategy::SpatialChains
    }
}

/// [`cook_project_streamed`] with explicit cook options, so the playtest cook
/// can pass the texture asset base its material table is numbered from.
/// `authored` is the unscaled document; it is scaled to engine units here.
pub fn cook_project_streamed_with(
    authored: &ProjectDocument,
    options: BrushWorldCookOptions<'_>,
    params: &PartitionParams,
) -> Result<CookedWorld, StreamCookError> {
    let pvs = StreamPvs::Clustered {
        reach: params.vis_distance,
    };
    Ok(cook_project_gated_with(authored, options, params, pvs)?.world)
}

/// A cook together with the partition that drove it and, for a streamed
/// world, the partition judged again with the cook's own numbers.
pub struct GatedCook {
    pub world: CookedWorld,
    pub input: PartitionInput,
    /// The partitioner's own verdict: sampled visibility, estimated payloads.
    pub estimated: Partition,
    /// The same partition with the cook's portal-flow closure, encoded
    /// payload sizes and real container size. `None` for a one-region world.
    pub measured: Option<Partition>,
    /// Seconds spent partitioning and cooking.
    pub partition_seconds: f64,
    pub cook_seconds: f64,
}

/// Partition and cook, then re-run the gates on what the cook measured.
pub fn cook_project_gated(
    authored: &ProjectDocument,
    project_root: &Path,
    mode: BrushWorldCookMode,
    ambient: [u8; 3],
    params: &PartitionParams,
    pvs: StreamPvs,
) -> Result<GatedCook, StreamCookError> {
    let options = BrushWorldCookOptions {
        project_root,
        mode,
        ambient,
        texture_asset_base: 0,
        collision_hulls: whole_map_hull_strategy(authored),
    };
    cook_project_gated_with(authored, options, params, pvs)
}

/// [`cook_project_gated`] with explicit cook options.
pub fn cook_project_gated_with(
    authored: &ProjectDocument,
    options: BrushWorldCookOptions<'_>,
    params: &PartitionParams,
    pvs: StreamPvs,
) -> Result<GatedCook, StreamCookError> {
    let started = std::time::Instant::now();
    let input = PartitionInput::from_project(authored, options.project_root)
        .map_err(|error| StreamCookError::Partition(error.to_string()))?;
    let mut plan = partition(&input, params);
    let partition_seconds = started.elapsed().as_secs_f64();
    let mut scaled = authored.clone();
    crate::units::scale_project_to_engine_units(&mut scaled);
    let started = std::time::Instant::now();
    if plan.regions.len() <= 1 {
        let world = CookedWorld::Whole(Box::new(compile_brush_world(&scaled, options)?));
        return Ok(GatedCook {
            world,
            input,
            estimated: plan,
            measured: None,
            partition_seconds,
            cook_seconds: started.elapsed().as_secs_f64(),
        });
    }
    // Cook, judge the cook's own numbers, and while the drive gate fails on
    // rho cook again with another sliver extent (the one choice in the cut
    // search that moves how many regions a crossing newly needs). The gate is
    // a property of the cut tree, so the best tree seen wins.
    let mut best: Option<(Partition, StreamedBrushWorld, Partition)> = None;
    let mut rho_passes = 0;
    let mut attempt_params = params.clone();
    loop {
        let world =
            compile_brush_world_streamed(&scaled, options, &plan.tree, &plan.layout.order, pvs)?;
        let measured = plan.with_measured(
            &input,
            &CookMeasured {
                visible: (0..world.index.regions.len())
                    .map(|r| {
                        world
                            .index
                            .vis_list(r)
                            .iter()
                            .map(|&q| u32::from(q))
                            .collect()
                    })
                    .map(|mut list: Vec<u32>| {
                        list.sort_unstable();
                        list
                    })
                    .collect(),
                payload_bytes: world.payloads.iter().map(|p| p.len() as u32).collect(),
                container_bytes: world.container.len() as u64,
                leaf_cap: u32::from(world.index.caps.leaves),
            },
        );
        let rho_failed = measured
            .gate
            .failures
            .iter()
            .any(|f| matches!(f, GateFailure::RhoExceeded { .. }));
        // Fewer failures first, then the lower worst rho.
        let score = |m: &Partition| (m.gate.failures.len(), m.gate.rho_worst().to_bits());
        if best
            .as_ref()
            .is_none_or(|(_, _, held)| score(&measured) < score(held))
        {
            best = Some((plan, world, measured));
        }
        if !rho_failed || rho_passes >= params.max_rho_passes {
            break;
        }
        rho_passes += 1;
        if rho_passes <= 3 {
            // No sliver rule at all, then a lower and a higher bar.
            attempt_params.min_sliver_extent = [0.0, 128.0, 384.0][rho_passes as usize - 1];
            plan = partition(&input, &attempt_params);
        } else {
            // Cut the heaviest region the worst window of the best tree enters:
            // a long crossing is bytes over its own length, so fewer bytes
            // behind its far side lowers the rate.
            let Some((_, _, held)) = best.as_ref() else {
                break;
            };
            let path = held.gate.failures.iter().find_map(|f| match f {
                GateFailure::RhoExceeded { path, .. } => Some(path.clone()),
                _ => None,
            });
            let Some(path) = path else { break };
            let mut entered: Vec<u32> = path.iter().skip(1).copied().collect();
            entered.sort_by_key(|&r| std::cmp::Reverse(held.regions[r as usize].counts.bytes()));
            let mut tree = held.tree.clone();
            let cut = entered
                .iter()
                .any(|&r| crate::brush_region::cuts::resplit(&input, params, &mut tree, &[r]));
            if !cut {
                break;
            }
            plan = crate::brush_region::partition_from(&input, params, tree);
        }
        if plan.regions.len() <= 1 {
            break;
        }
    }
    let (plan, world, measured) = best.expect("one cook pass ran");
    Ok(GatedCook {
        world: CookedWorld::Streamed(Box::new(world)),
        input,
        estimated: plan,
        measured: Some(measured),
        partition_seconds,
        cook_seconds: started.elapsed().as_secs_f64(),
    })
}

/// How the playtest cook treats world streaming, read from
/// `PSXED_STREAM_WORLD` (design 2026-10-08, M7).
///
/// * unset or anything else: the whole-map cook, unchanged (the default until
///   the M7 gates pass);
/// * `ref`: partition and cook the streamed geometry, then flatten it into one
///   ordinary whole-map container (the reference the streamed build is
///   compared against);
/// * `stream`: the streamed form, a top container plus a region pack.
///
/// A project that fits one region cooks the whole-map path in every mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamWorldMode {
    Off,
    Reference,
    Streamed,
}

impl StreamWorldMode {
    pub fn from_env() -> Self {
        match std::env::var("PSXED_STREAM_WORLD").as_deref() {
            Ok("ref") | Ok("reference") => Self::Reference,
            Ok("1") | Ok("stream") | Ok("streamed") => Self::Streamed,
            _ => Self::Off,
        }
    }
}

/// Partition parameters for the playtest cook: the defaults, with the RON
/// overrides named by `PSXED_STREAM_PARAMS` applied when set.
pub fn partition_params_from_env() -> Result<PartitionParams, String> {
    let mut params = PartitionParams::default();
    if let Ok(path) = std::env::var("PSXED_STREAM_PARAMS") {
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
        crate::brush_region::CookOverrides::from_ron_str(&text)
            .map_err(|e| format!("{path}: {e}"))?
            .apply(&mut params);
    }
    Ok(params)
}

/// The streamed pieces of a playtest cook that go on the disc.
pub struct StreamedParts {
    pub container: Vec<u8>,
    pub region_pack: Vec<u8>,
    pub regions: usize,
}

/// Cook the world for the playtest package under `mode`. The returned
/// [`CompiledBrushWorld`] always carries a container the legacy loader reads
/// (for a streamed cook, the flattened whole map of the same geometry); the
/// second value is the streamed form when `mode` asks for it.
pub fn cook_playtest_world(
    authored: &ProjectDocument,
    options: BrushWorldCookOptions<'_>,
    mode: StreamWorldMode,
) -> Result<(CompiledBrushWorld, Option<StreamedParts>), StreamCookError> {
    let params = partition_params_from_env().map_err(StreamCookError::Partition)?;
    match cook_project_streamed_with(authored, options, &params)? {
        CookedWorld::Whole(world) => Ok((*world, None)),
        CookedWorld::Streamed(world) => {
            let flat = flatten_streamed(&world);
            let compiled = CompiledBrushWorld {
                pxbsp: crate::brush_pxbsp::CompiledPxbsp {
                    resident_bytes: flat.bytes.len(),
                    max_visible_faces: flat.max_visible_faces,
                    bytes: flat.bytes,
                },
                textures: world.textures.clone(),
                movers: Vec::new(),
                body_hulls: world.body_hulls,
                leak_path: world.leak_path.clone(),
                uv_window: world.uv_window,
            };
            let parts = (mode == StreamWorldMode::Streamed).then(|| StreamedParts {
                regions: world.index.regions.len(),
                container: world.container.clone(),
                region_pack: world.region_pack.clone(),
            });
            Ok((compiled, parts))
        }
    }
}

/// A sparse visible-leaf row as the dense bit row the whole-map format stores.
fn flat_dense_row(sparse: &[u32], total: usize) -> Vec<u8> {
    let mut row = vec![0u8; total.div_ceil(8)];
    for &id in sparse {
        row[id as usize >> 3] |= 1 << (id & 7);
    }
    row
}

fn flat_rd16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn flat_wr16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

/// A streamed world concatenated into one ordinary whole-map PXBSP.
///
/// Built from the cooked region tables with plain additive bases and the
/// dense, slot-free PVS rows the cooker computed before it rewrote them into
/// the rank layout, independently of the runtime install code. It is the
/// "whole-map build of the same geometry": the streamed world must draw and
/// collide exactly like it once every region is resident.
pub struct FlatWorld {
    /// A legacy-loadable PXBSP v6 container.
    pub bytes: Vec<u8>,
    /// Region and local face of every flat face.
    pub face_of: Vec<(usize, usize)>,
    /// Region and local leaf (1-based) of every flat leaf (index 0 unused).
    pub leaf_of: Vec<(usize, usize)>,
    /// Most faces any one leaf's PVS makes potentially visible: the bound
    /// the renderer's face chains are sized from.
    pub max_visible_faces: usize,
}

fn flat_lump(container: &[u8], index: &PxbspIndex, kind: PxbspLumpKind) -> Vec<u8> {
    let r = index.lump(kind);
    container[r.offset as usize..r.end() as usize].to_vec()
}

/// Concatenate every region of `world` into a [`FlatWorld`].
pub fn flatten_streamed(world: &StreamedBrushWorld) -> FlatWorld {
    let debug = &world.debug;
    let regions = debug.regions.len();
    let top = &debug.top;
    let index = PxbspIndex::read(&mut SliceReader::new(&world.container)).unwrap();
    let mut lumps: [Vec<u8>; PXBSP_LUMP_COUNT] = core::array::from_fn(|_| Vec::new());
    lumps[PxbspLumpKind::Materials as usize] =
        flat_lump(&world.container, &index, PxbspLumpKind::Materials);
    lumps[PxbspLumpKind::Entities as usize] =
        flat_lump(&world.container, &index, PxbspLumpKind::Entities);

    // Additive bases, region by region.
    let mut base = vec![[0usize; 8]; regions]; // planes, vertices, faces, marks, nodes, clip, leaf, vis
    let top_planes = top.planes.len() / 12;
    let top_nodes = top.nodes.len() / 16;
    let top_clip = top.clip_nodes.len() / 6;
    let mut at = [top_planes, 0, 0, 0, top_nodes, top_clip, 1, 0];
    for (r, b) in debug.regions.iter().enumerate() {
        base[r] = at;
        at[0] += b.planes.len() / 12;
        at[1] += b.vertices.len() / 12;
        at[2] += b.faces.len() / 10;
        at[3] += b.marks.len() / 2;
        at[4] += b.nodes.len() / 16;
        at[5] += b.clip_nodes.len() / 6;
        at[6] += b.leaves.len() / 14;
    }

    // Region child -> flat child.
    let render_child = |r: usize, c: i16| -> i16 {
        if c >= 0 {
            (c as usize + base[r][4]) as i16
        } else {
            let l = (-1 - c) as usize;
            if l == 0 {
                c
            } else {
                (-1 - (base[r][6] + l - 1) as i32) as i16
            }
        }
    };
    let clip_child = |r: usize, c: i16| -> i16 {
        if c >= 0 {
            (c as usize + base[r][5]) as i16
        } else {
            c
        }
    };

    let mut planes = top.planes.clone();
    let mut vertices = Vec::new();
    let mut faces = Vec::new();
    let mut marks = Vec::new();
    let mut nodes = top.nodes.clone();
    let mut clips = top.clip_nodes.clone();
    let mut leaves = Vec::new();
    let mut face_of = Vec::new();
    let mut leaf_of = vec![(0, 0)];
    // Sentinel leaf.
    {
        let mut sentinel = vec![0u8; 14];
        sentinel[0] = CONTENTS_SOLID as i8 as u8;
        sentinel[4..8].copy_from_slice(&(-1i32).to_le_bytes());
        sentinel[13] = 64;
        leaves.extend(sentinel);
    }
    // Dense visibility, interned.
    let mut vis = Vec::new();
    let mut interned = std::collections::BTreeMap::<Vec<u8>, i32>::new();
    for (r, b) in debug.regions.iter().enumerate() {
        planes.extend_from_slice(&b.planes);
        vertices.extend_from_slice(&b.vertices);
        for (local, face) in b.faces.chunks_exact(10).enumerate() {
            let mut f = face.to_vec();
            flat_wr16(&mut f, 0, flat_rd16(face, 0) + base[r][0] as u16);
            flat_wr16(&mut f, 2, flat_rd16(face, 2) + base[r][1] as u16);
            faces.extend(f);
            face_of.push((r, local));
        }
        for mark in b.marks.chunks_exact(2) {
            marks.extend_from_slice(&(flat_rd16(mark, 0) + base[r][2] as u16).to_le_bytes());
        }
        for node in b.nodes.chunks_exact(16) {
            let mut n = node.to_vec();
            flat_wr16(&mut n, 0, flat_rd16(node, 0) + base[r][0] as u16);
            for side in 0..2 {
                flat_wr16(
                    &mut n,
                    2 + side * 2,
                    render_child(r, flat_rd16(node, 2 + side * 2) as i16) as u16,
                );
            }
            nodes.extend(n);
        }
        for node in b.clip_nodes.chunks_exact(6) {
            let mut n = node.to_vec();
            flat_wr16(&mut n, 0, flat_rd16(node, 0) + base[r][0] as u16);
            for side in 0..2 {
                flat_wr16(
                    &mut n,
                    2 + side * 2,
                    clip_child(r, flat_rd16(node, 2 + side * 2) as i16) as u16,
                );
            }
            clips.extend(n);
        }
        for (k, leaf) in b.leaves.chunks_exact(14).enumerate() {
            let mut l = leaf.to_vec();
            flat_wr16(&mut l, 8, flat_rd16(leaf, 8) + base[r][3] as u16);
            let row = flat_dense_row(&debug.sparse_rows[r][k], debug.dense_total);
            let compressed = crate::brush_pack::compress_visibility(&row);
            let offset = *interned.entry(compressed.clone()).or_insert_with(|| {
                let at = vis.len() as i32;
                vis.extend_from_slice(&compressed);
                at
            });
            l[4..8].copy_from_slice(&offset.to_le_bytes());
            leaves.extend(l);
            leaf_of.push((r, k + 1));
        }
    }
    // Top nodes: a stub child becomes the region's root.
    for t in 0..top_nodes {
        for side in 0..2 {
            let at = t * 16 + 2 + side * 2;
            let child = flat_rd16(&nodes, at) as i16;
            if child < 0 {
                let r = (-child - 2) as usize;
                let root = render_child(r, debug.regions[r].render_root);
                flat_wr16(&mut nodes, at, root as u16);
            }
        }
    }
    // Top clip nodes: patch each region's two links.
    for (r, entry) in world.index.regions.iter().enumerate() {
        for hull in 0..2 {
            let parent = entry.parents[1 + hull] as usize;
            let side = entry.side(1 + hull);
            let root = clip_child(r, debug.regions[r].clip_roots[hull]);
            flat_wr16(&mut clips, parent * 6 + 2 + side * 2, root as u16);
        }
    }
    let n = top_nodes;
    let mut model = Vec::new();
    for v in debug
        .world_bounds
        .0
        .into_iter()
        .chain(debug.world_bounds.1)
        .chain([0; 3])
    {
        model.extend_from_slice(&v.to_le_bytes());
    }
    for head in [0i16, 0, 1, 1 + n as i16] {
        model.extend_from_slice(&head.to_le_bytes());
    }
    model.extend_from_slice(&(debug.dense_total as i16).to_le_bytes());
    model.extend_from_slice(&0u16.to_le_bytes());
    model.extend_from_slice(&((faces.len() / 10) as u16).to_le_bytes());
    lumps[PxbspLumpKind::Vertices as usize] = vertices;
    lumps[PxbspLumpKind::Planes as usize] = planes;
    lumps[PxbspLumpKind::Faces as usize] = faces;
    lumps[PxbspLumpKind::MarkSurfaces as usize] = marks;
    lumps[PxbspLumpKind::Visibility as usize] = vis;
    lumps[PxbspLumpKind::Leaves as usize] = leaves;
    lumps[PxbspLumpKind::Nodes as usize] = nodes;
    lumps[PxbspLumpKind::ClipNodes as usize] = clips;
    lumps[PxbspLumpKind::Models as usize] = model;
    let bytes = write_pxbsp(&lumps).expect("flat container");
    let max_visible_faces = flat_max_visible_faces(&bytes);
    FlatWorld {
        bytes,
        face_of,
        leaf_of,
        max_visible_faces,
    }
}

/// The most faces any leaf's PVS shows, measured on the loaded flat map.
fn flat_max_visible_faces(bytes: &[u8]) -> usize {
    let mut map = psx_bsp::pxbsp_resident::PxbspResidentMap::with_capacity(bytes.len());
    map.load(0, &mut psx_bsp::SliceReader::new(bytes))
        .expect("the flat container loads as a legacy map");
    let faces = map.faces().len();
    let mut row = vec![0u8; PXBSP_MAX_VISIBILITY_BYTES];
    let mut marked = vec![false; faces];
    let mut best = 0usize;
    for leaf in 1..map.leaves().len() {
        let Some(bits) = map.leaf_visibility_into(leaf, &mut row) else {
            continue;
        };
        marked.fill(false);
        let mut count = 0usize;
        for bit in 0..bits {
            if row[bit >> 3] & (1 << (bit & 7)) == 0 {
                continue;
            }
            let Some(visible) = map.leaves().get(bit + 1) else {
                continue;
            };
            let first = visible.first_mark_surface as usize;
            for mark in first..first + visible.mark_surface_count as usize {
                if let Some(&face) = map.mark_surfaces_native().get(mark) {
                    if !marked[face as usize] {
                        marked[face as usize] = true;
                        count += 1;
                    }
                }
            }
        }
        best = best.max(count);
    }
    best
}

#[cfg(test)]
#[allow(clippy::print_stdout)]
#[path = "brush_world_stream_tests.rs"]
mod tests;
