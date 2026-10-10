//! Per-region payload accounting (design 3.3).
//!
//! Every final cell is cut out of the world exactly the way the runtime
//! region will carry it: render and topology surfaces are clipped to the cell
//! box, a surface BSP is built inside the cell from its own topology pieces,
//! leaves are classified against the brushes, and collision is costed with
//! the shipping hull compiler on the brushes that reach the cell.
//!
//! The coplanar rule keeps the clip lossless: a polygon that lies exactly on
//! a cell face belongs to the cell its normal points into, so two cells never
//! both carry one wall, and the outward faces of a sealed shell on the world
//! boundary belong to no cell at all.

use std::collections::{BTreeSet, HashMap};

use psx_bsp::render::PXBSP_MAX_FACE_VERTICES;

use crate::brush::Brush;
use crate::brush_collision_hulls::compile_collision_hulls;
use crate::brush_compile::{build_surface_bsp, CompiledSurface};
use crate::brush_portal::{classify_bsp_leaves, portalize_surface_bsp};

use super::closure::Closure;
use super::cuts::CutTree;
use super::geometry::{
    clip_polygon_to_box, polygon_area, polygon_centroid, polygon_on_axis_plane, Aabb, V3,
};
use super::input::PartitionInput;
use super::{record, PartitionParams, SECTOR_BYTES};

/// Raw lump counts of one region, before any run-length gain.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PayloadCounts {
    pub faces: u32,
    pub vertices: u32,
    pub planes: u32,
    pub marks: u32,
    /// Visible leaves plus the solid sentinel.
    pub leaves: u32,
    pub nodes: u32,
    pub clip_nodes: u32,
    pub topology_pieces: u32,
    pub brushes: u32,
    pub spawns: u32,
    /// Texture bytes duplicated into this payload (small textures).
    pub inline_texture_bytes: u32,
    pub header_bytes: u32,
    /// Dense PVS rows (design 3.4), filled once visibility is known.
    pub pvs_bytes: u32,
    /// The encoded payload size, once a cook has produced it. It replaces the
    /// count based estimate everywhere a size is read.
    pub measured_bytes: Option<u32>,
}

impl PayloadCounts {
    /// Payload bytes excluding the PVS rows.
    pub fn bytes_without_pvs(&self) -> u32 {
        self.header_bytes
            + self.vertices * record::VERTEX
            + self.planes * record::PLANE
            + self.faces * record::FACE
            + self.marks * record::MARK
            + self.leaves * record::LEAF
            + self.nodes * record::NODE
            + self.clip_nodes * record::CLIPNODE
            + self.inline_texture_bytes
            + self.spawns * record::ENTITY
    }

    pub fn bytes(&self) -> u32 {
        self.measured_bytes
            .unwrap_or_else(|| self.bytes_without_pvs() + self.pvs_bytes)
    }

    /// Bytes that stay in the page pool once installed. Inline textures are
    /// uploaded to VRAM during install and their landing bytes are freed, so
    /// they cost read time but not pool space. [E, design 4.5 install flow]
    pub fn resident_bytes(&self) -> u32 {
        match self.measured_bytes {
            Some(bytes) => bytes,
            None => self.bytes() - self.inline_texture_bytes,
        }
    }

    pub fn sectors(&self) -> u32 {
        self.bytes().div_ceil(SECTOR_BYTES).max(1)
    }
}

/// One sight target: a point just in front of a surface piece.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SightTarget {
    pub point: V3,
    pub normal: V3,
}

/// One region of the partition.
#[derive(Clone, Debug)]
pub struct Region {
    pub id: u32,
    pub bounds: Aabb,
    pub counts: PayloadCounts,
    /// Indices into `PartitionInput::materials`, sorted.
    pub materials: Vec<usize>,
    /// Materials whose textures are shared rather than inlined, sorted.
    pub shared_textures: Vec<usize>,
    /// Archetype indices of the spawns here, sorted.
    pub archetypes: Vec<usize>,
    pub spawn_ids: Vec<u32>,
    pub hook_ids: Vec<u32>,
    pub checkpoint_ids: Vec<u32>,
    pub targets: Vec<SightTarget>,
    /// Walkable floor points inside the cell (all upward surfaces when the
    /// project has no player start).
    pub walk_points: Vec<V3>,
    /// Collision compile failure text, when the shipping compiler refused.
    pub collision_error: Option<String>,
    /// The cut search could find no further admissible plane.
    pub unsplittable: bool,
}

impl Region {
    /// Binding slot cap: the largest used fraction over all lumps, percent.
    pub fn binding_fill_pct(&self, params: &PartitionParams) -> (u32, &'static str) {
        let c = &self.counts;
        let caps = &params.caps;
        let rows = [
            (c.faces, caps.faces, "faces"),
            (c.vertices, caps.vertices, "vertices"),
            (c.nodes, caps.nodes, "nodes"),
            (c.leaves, caps.leaves, "leaves"),
            (c.marks, caps.mark_surfaces, "marks"),
            (c.clip_nodes, caps.clip_nodes, "clipnodes"),
        ];
        let mut best = (0u32, "faces");
        for (used, cap, name) in rows {
            let pct = (u64::from(used) * 100 / u64::from(cap.max(1))) as u32;
            if pct > best.0 {
                best = (pct, name);
            }
        }
        let bytes_pct =
            (u64::from(c.bytes()) * 100 / u64::from(params.region_hard_cap_bytes.max(1))) as u32;
        if bytes_pct > best.0 {
            best = (bytes_pct, "bytes");
        }
        best
    }

    /// Which hard limits this region breaks, as names.
    pub fn violations(&self, params: &PartitionParams) -> Vec<&'static str> {
        let c = &self.counts;
        let caps = &params.caps;
        let mut out = Vec::new();
        if c.bytes() > params.region_hard_cap_bytes {
            out.push("bytes");
        }
        if c.faces > caps.faces {
            out.push("faces");
        }
        if c.vertices > caps.vertices {
            out.push("vertices");
        }
        if c.nodes > caps.nodes {
            out.push("nodes");
        }
        if c.leaves > caps.leaves {
            out.push("leaves");
        }
        if c.marks > caps.mark_surfaces {
            out.push("marks");
        }
        if c.clip_nodes > caps.clip_nodes {
            out.push("clipnodes");
        }
        out
    }
}

/// Memoised per-cell results across refinement passes, keyed by cell bounds.
pub(crate) type AccountCache = HashMap<[u64; 6], Region>;

fn key(bounds: &Aabb) -> [u64; 6] {
    [
        bounds.min[0].to_bits(),
        bounds.min[1].to_bits(),
        bounds.min[2].to_bits(),
        bounds.max[0].to_bits(),
        bounds.max[1].to_bits(),
        bounds.max[2].to_bits(),
    ]
}

/// Account every leaf of the tree. Unchanged cells come from `cache`.
pub(crate) fn account_regions(
    input: &PartitionInput,
    params: &PartitionParams,
    tree: &CutTree,
    cache: &mut AccountCache,
) -> Vec<Region> {
    let todo: Vec<u32> = (0..tree.region_count() as u32)
        .filter(|&r| !cache.contains_key(&key(&tree.leaf_bounds(r))))
        .collect();
    let computed = super::par_map(&todo, |&r| account_cell(input, params, tree.leaf_bounds(r)));
    for (&r, region) in todo.iter().zip(computed) {
        cache.insert(key(&tree.leaf_bounds(r)), region);
    }
    (0..tree.region_count() as u32)
        .map(|r| {
            let node = tree.leaves[r as usize] as usize;
            let mut region = cache[&key(&tree.leaf_bounds(r))].clone();
            region.id = r;
            region.unsplittable = matches!(
                &tree.nodes[node],
                super::cuts::CutNode::Leaf {
                    unsplittable: true,
                    ..
                }
            );
            region
        })
        .collect()
}

/// Surfaces of `source` that fall in `bounds`, clipped to it.
pub(crate) fn clip_surfaces(
    source: &[CompiledSurface],
    source_bounds: &[Aabb],
    bounds: &Aabb,
) -> Vec<CompiledSurface> {
    clip_surfaces_indexed(source, source_bounds, bounds, false)
        .into_iter()
        .map(|(_, piece)| piece)
        .collect()
}

/// [`clip_surfaces`] that also returns each piece's index in `source`.
///
/// With `share_liquid_boundary`, a non-solid surface lying exactly on a cell
/// face belongs to both neighbours instead of the one its normal points into:
/// water is drawn from either side, so each cell needs its own copy.
pub(crate) fn clip_surfaces_indexed(
    source: &[CompiledSurface],
    source_bounds: &[Aabb],
    bounds: &Aabb,
    share_liquid_boundary: bool,
) -> Vec<(usize, CompiledSurface)> {
    let mut out = Vec::new();
    for (index, (surface, aabb)) in source.iter().zip(source_bounds).enumerate() {
        if !aabb.overlaps(bounds) {
            continue;
        }
        if (!share_liquid_boundary || surface.contents.is_solid())
            && !owns_coplanar(surface, bounds)
        {
            continue;
        }
        let vertices = if aabb.min.iter().zip(&bounds.min).all(|(a, b)| a >= b)
            && aabb.max.iter().zip(&bounds.max).all(|(a, b)| a <= b)
        {
            Some(surface.vertices.clone())
        } else {
            clip_polygon_to_box(&surface.vertices, bounds)
        };
        if let Some(vertices) = vertices {
            let mut piece = surface.clone();
            piece.vertices = vertices;
            out.push((index, piece));
        }
    }
    out
}

/// The coplanar ownership rule from the module docs.
fn owns_coplanar(surface: &CompiledSurface, bounds: &Aabb) -> bool {
    for axis in 0..3 {
        let normal = surface.plane.normal[axis];
        if polygon_on_axis_plane(&surface.vertices, axis, bounds.min[axis]) && normal <= 0 {
            return false;
        }
        if polygon_on_axis_plane(&surface.vertices, axis, bounds.max[axis]) && normal >= 0 {
            return false;
        }
    }
    true
}

fn unit_normal(surface: &CompiledSurface) -> V3 {
    let n = surface.plane.normal.map(|v| v as f64);
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2])
        .sqrt()
        .max(1.0e-12);
    [n[0] / len, n[1] / len, n[2] / len]
}

fn account_cell(input: &PartitionInput, params: &PartitionParams, bounds: Aabb) -> Region {
    let render = clip_surfaces(&input.render, &input.render_bounds, &bounds);
    let topology = clip_surfaces(&input.topology, &input.topology_bounds, &bounds);

    let mut counts = PayloadCounts::default();
    let mut planes: BTreeSet<([i64; 3], i64)> = BTreeSet::new();
    let mut materials: BTreeSet<usize> = BTreeSet::new();
    let mut targets: Vec<(f64, SightTarget)> = Vec::new();
    for piece in &render {
        let n = piece.vertices.len();
        let fans = if n <= PXBSP_MAX_FACE_VERTICES {
            1
        } else {
            (n - 2).div_ceil(PXBSP_MAX_FACE_VERTICES - 2)
        };
        counts.faces += fans as u32;
        counts.vertices += (n + 2 * (fans - 1)) as u32;
        planes.insert((piece.plane.normal, piece.plane.dist));
        materials.insert(input.material_index(piece.material));
        let normal = unit_normal(piece);
        let centroid = polygon_centroid(&piece.vertices);
        // One unit in front of the surface, into the air that sees it.
        let point = [
            centroid[0] + normal[0],
            centroid[1] + normal[1],
            centroid[2] + normal[2],
        ];
        if !input.materials[input.material_index(piece.material)].sky_aperture {
            targets.push((polygon_area(&piece.vertices), SightTarget { point, normal }));
        }
    }
    counts.planes = planes.len() as u32;
    counts.topology_pieces = topology.len() as u32;

    // The cell's own surface BSP, as the runtime region would carry it.
    let cell_brushes: Vec<Brush> = input
        .brushes
        .iter()
        .zip(&input.brush_bounds)
        .filter(|(_, aabb)| aabb.overlaps_strict(&bounds))
        .map(|(brush, _)| brush.clone())
        .collect();
    counts.brushes = cell_brushes.len() as u32;
    if topology.is_empty() {
        counts.leaves = 1;
        counts.marks = 0;
    } else {
        let mut bsp = build_surface_bsp(&topology);
        let portals = portalize_surface_bsp(&bsp);
        classify_bsp_leaves(&mut bsp, &portals, &cell_brushes);
        counts.nodes = bsp.nodes.len() as u32;
        let mut visible = 0u32;
        let mut topology_marks = 0u32;
        for leaf in &bsp.leaves {
            if leaf.contents.is_visible() {
                visible += 1;
                topology_marks += leaf.mark_surfaces.len() as u32;
            }
        }
        counts.leaves = visible + 1;
        // Render faces are finer than topology pieces; scale the marks by the
        // same ratio. [E]
        let scale = f64::from(counts.faces) / f64::from(counts.topology_pieces.max(1));
        counts.marks = (f64::from(topology_marks) * scale).ceil() as u32;
    }

    // Collision, costed by the shipping compiler on the brushes that reach
    // the cell (brushes crossing a cut are carried whole by both cells; the
    // design clips them, which only shrinks this figure). [E]
    let mut collision_error = None;
    if !cell_brushes.is_empty() {
        match compile_collision_hulls(&cell_brushes, &input.hull_bounds[1..]) {
            Ok(hulls) => {
                counts.clip_nodes = (hulls.clipnodes.len() / record::CLIPNODE as usize) as u32 + 1;
                // Collision planes are mostly the render planes again (the
                // cooked Planes lump merges them), so the lump is the larger
                // of the two rather than their sum. Records are 14 bytes in
                // the compiler's output. [E]
                counts.planes = counts.planes.max((hulls.planes.len() / 14) as u32);
            }
            Err(error) => collision_error = Some(format!("{error:?}")),
        }
    }

    // Spawns, hooks and checkpoints by half-open containment.
    let in_cell = |p: &V3| bounds.contains_half_open(*p);
    let spawn_ids: Vec<u32> = input
        .spawns
        .iter()
        .enumerate()
        .filter(|(_, s)| in_cell(&s.position))
        .map(|(i, _)| i as u32)
        .collect();
    let hook_ids: Vec<u32> = input
        .hooks
        .iter()
        .enumerate()
        .filter(|(_, p)| in_cell(p))
        .map(|(i, _)| i as u32)
        .collect();
    let checkpoint_ids: Vec<u32> = input
        .checkpoints
        .iter()
        .enumerate()
        .filter(|(_, p)| in_cell(p))
        .map(|(i, _)| i as u32)
        .collect();
    counts.spawns = spawn_ids.len() as u32;
    let archetypes: BTreeSet<usize> = spawn_ids
        .iter()
        .filter_map(|&i| input.spawns[i as usize].archetype)
        .collect();

    // Inline versus shared textures is a world-level choice, made once all
    // cells are known (see `apply_texture_policy`).
    let shared: Vec<usize> = Vec::new();
    counts.header_bytes =
        params.header_bytes + 4 * materials.len() as u32 + 2 * archetypes.len() as u32 + 24;

    let walk_points: Vec<V3> = match &input.walk {
        Some(walk) => walk
            .points
            .iter()
            .copied()
            .filter(|p| bounds.contains_half_open(*p))
            .collect(),
        None => render
            .iter()
            .filter(|piece| unit_normal(piece)[1] > 0.7)
            .map(|piece| {
                let c = polygon_centroid(&piece.vertices);
                [c[0], c[1] + 1.0, c[2]]
            })
            .collect(),
    };

    // Strongest targets first (largest area), then spatial order: stable.
    targets.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then(a.1.point[0].total_cmp(&b.1.point[0]))
            .then(a.1.point[1].total_cmp(&b.1.point[1]))
            .then(a.1.point[2].total_cmp(&b.1.point[2]))
    });
    Region {
        id: 0,
        bounds,
        counts,
        materials: materials.into_iter().collect(),
        shared_textures: shared,
        archetypes: archetypes.into_iter().collect(),
        spawn_ids,
        hook_ids,
        checkpoint_ids,
        targets: targets.into_iter().map(|(_, t)| t).collect(),
        walk_points,
        collision_error,
        unsplittable: false,
    }
}

/// Fill each region's dense PVS rows from the closure (design 3.4):
/// one row per visible leaf, one bit per visible leaf of every region in V(R).
pub(crate) fn apply_pvs(params: &PartitionParams, regions: &mut [Region], closure: &Closure) {
    let leaves: Vec<u32> = regions
        .iter()
        .map(|r| r.counts.leaves.saturating_sub(1))
        .collect();
    for (r, region) in regions.iter_mut().enumerate() {
        let row_bits: u32 = closure.visible[r].iter().map(|&q| leaves[q as usize]).sum();
        let row_bytes = row_bits.div_ceil(8);
        let dense = u64::from(leaves[r]) * u64::from(row_bytes);
        region.counts.pvs_bytes = (dense * u64::from(params.pvs_stored_pct) / 100) as u32;
    }
}

/// Regions that break a hard limit and may still be split.
pub(crate) fn oversize_regions(params: &PartitionParams, regions: &[Region]) -> Vec<u32> {
    regions
        .iter()
        .filter(|r| !r.unsplittable && !r.violations(params).is_empty())
        .map(|r| r.id)
        .collect()
}

/// Texture placement (design 3.3), decided with the whole world in view.
///
/// A small texture rides inline in every region that uses it, which costs
/// read time per region but no extra seek. A texture that most regions use is
/// cheaper kept in a shared pack that stays resident, so a texture used by
/// more than `hot_texture_share_pct` of the regions is shared whatever its
/// size. Large textures are always shared. [E: the design's cost model
/// `(copies - 1) x sectors x 6.49 ms` against `extra seeks x 137 ms`, reduced
/// to a share threshold.]
pub(crate) fn apply_texture_policy(
    input: &PartitionInput,
    params: &PartitionParams,
    regions: &mut [Region],
) {
    let mut users = vec![0u32; input.materials.len()];
    for region in regions.iter() {
        for &m in &region.materials {
            users[m] += 1;
        }
    }
    let small_limit = params.small_texture_sectors * SECTOR_BYTES;
    let hot_limit = u64::from(params.hot_texture_share_pct) * regions.len() as u64;
    for region in regions.iter_mut() {
        region.shared_textures.clear();
        region.counts.inline_texture_bytes = 0;
        for &m in &region.materials {
            let bytes = input.materials[m].texture_bytes;
            if bytes == 0 {
                continue;
            }
            let hot = u64::from(users[m]) * 100 > hot_limit;
            if bytes <= small_limit && !hot {
                region.counts.inline_texture_bytes += bytes;
            } else {
                region.shared_textures.push(m);
            }
        }
    }
}
