//! Automatic world partitioner for streamed worlds (design 2026-10-08,
//! sections 3 and 4, milestone M4).
//!
//! Host only. Nothing here changes the PXBSP format or the runtime: the
//! partitioner reads the same brush world the cooker compiles, decides where
//! to cut it, accounts the payload of every region, estimates what each
//! region needs resident, and judges the result against the drive and the
//! RAM pool. The output is a [`Partition`] and a deterministic report.
//!
//! Labels used in comments and in the report mirror the design:
//! `[M]` measured or stated in code, `[D]` derived from `[M]` values, `[E]`
//! an estimate or tunable default to be replaced by a measurement.
//!
//! Pipeline, one module per stage:
//! 1. [`input`]: scale to engine units, run the shared CSG/subdivision front
//!    end, collect spawns, hooks and the player start.
//! 2. [`cuts`]: top-down axis-aligned bisection of the world (the cut tree).
//! 3. [`account`]: clip surfaces to each cell, build the cell's own surface
//!    BSP, cost its collision, and total the payload.
//! 4. [`graph`]: open-aperture adjacency between cells and walk distances.
//! 5. [`closure`]: which regions can be seen from where (V(R)), and the
//!    resident requirement Need(R) including hook edges.
//! 6. [`gate`]: rank-row width, bytes-per-unit-of-travel (rho) and RAM pool
//!    checks, all as named, labelled parameters.
//! 7. [`layout`]: disc order for seek locality.
//! 8. [`report`]: text and JSON, both byte-stable for equal inputs.
//!
//! Deliberate simplifications versus the design, each repeated in the report
//! where it matters: cuts are axis-aligned only; visibility is a sampled
//! line-of-sight estimate over the brush solids rather than leaf-level portal
//! flow; archetype pack bytes are not yet measured and count as zero.

pub mod fixtures;
pub mod geometry;

mod account;
mod budget;
mod closure;
pub(crate) mod cuts;
mod gate;
mod graph;
mod input;
mod layout;
mod report;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_world;

pub(crate) use account::clip_surfaces_indexed;
pub use account::{PayloadCounts, Region};
pub use budget::{union_regions, PoolPlan};
pub use closure::{Closure, ClosureSource};
pub use cuts::{CutNode, CutTree};
pub use gate::{GateFailure, GateReport, PoolPeak, RhoEdge, RhoWindow};
pub use graph::{Aperture, RegionGraph};
pub use input::{ArchetypeInfo, MaterialInfo, PartitionInput, Spawn};
pub use layout::Layout;
pub use report::{StreamReport, REPORT_VERSION};

use geometry::{Aabb, V3};

/// Bytes per CD sector. [M]
pub const SECTOR_BYTES: u32 = 2048;

/// Main RAM the world pool can be carved from, as measured on the Graybox
/// Reach guest by `occupancy-report` (design 4.5, 2026-10-08). Every field
/// is a measurement or a labelled estimate, and the pool is derived from them,
/// never typed in.
///
/// The persistent-asset arena (clips, models and atlases) is the bulk of the
/// guest's RAM. The compressed-animation passes (dense repair, then keyed
/// tracks) have landed and the headroom below is measured after them, so the
/// 153 KB and 182 KB they gave back are already in it. `arena_reclaim_bytes`
/// is for a pass that has not landed, and is zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RamBudget {
    /// Region cap minus the linked static image. [M, link map: cap 1,998,848
    /// minus image 1,665,892 (`__bss_end` 0x801a6764 less 0x80010000)]
    pub static_headroom_bytes: u32,
    /// Heap in use at gameplay, apart from the region pool itself. The heap
    /// lives inside the headroom, so it is not available to the pool. [M,
    /// headless emulator RAM dump of the streamed guest at poll 1500,
    /// 2026-10-10: 107,466 B in use, less the 102,477 B pool image and the
    /// 708 B top container copy; the whole-map guest holds 3,761 B]
    pub heap_in_use_bytes: u32,
    /// The baked PXBSP a streamed world stops baking into `.data`; the
    /// resident skeleton takes some of it back. [M, link map: the
    /// `PXBSP_WORLD` symbol is 0x16e70]
    pub baked_bsp_image_bytes: u32,
    /// Static RAM the streamed guest spends beyond the whole-map one, apart
    /// from the world it no longer bakes: the region reader and streamer code,
    /// their buffers and counters. [M, link maps, 2026-10-10: the whole-map
    /// headroom 331,676 B plus the 93,808 B image, less the streamed guest's
    /// 389,340 B headroom and its 708 B baked container]
    pub stream_overhead_bytes: u32,
    /// Fragmentation margin, install scratch and growth. [E]
    pub safety_floor_bytes: u32,
    /// The persistent-asset arena. Information only. [M, cook manifest:
    /// 150 pages of 2,048 B]
    pub persistent_asset_arena_bytes: u32,
    /// Part of that arena handed to the world once models are compressed.
    /// Zero today. [E]
    pub arena_reclaim_bytes: u32,
}

/// Graybox Reach, the editor-playtest guest linked from editor main 1974d8e1's
/// tree (SDK pin be42239a5), 2026-10-10, whole-map and streamed
/// (`world-stream`, three regions). The 2026-10-09 figures (a2d5039d) were
/// headroom 332,956 and heap 3,764, with no streamed guest to measure; the
/// streamed guest turned out to cost 35,436 B of static RAM that the model had
/// not counted.
pub const RAM_BUDGET: RamBudget = RamBudget {
    static_headroom_bytes: 331_676,
    heap_in_use_bytes: 4_281,
    baked_bsp_image_bytes: 93_808,
    stream_overhead_bytes: 35_436,
    safety_floor_bytes: 16_384,
    persistent_asset_arena_bytes: 307_200,
    arena_reclaim_bytes: 0,
};

impl RamBudget {
    /// Bytes for region pages before the resident skeleton is paid for:
    /// headroom minus the heap in use, plus the image no longer baked, plus
    /// what the arena gives back, minus the streaming code's static cost and
    /// the safety floor. [D]
    pub const fn world_pool_bytes(&self) -> u32 {
        self.static_headroom_bytes - self.heap_in_use_bytes
            + self.baked_bsp_image_bytes
            + self.arena_reclaim_bytes
            - self.stream_overhead_bytes
            - self.safety_floor_bytes
    }

    /// The arithmetic as a line for the report.
    pub fn describe(&self) -> String {
        format!(
            "{} headroom - {} heap in use + {} baked PXBSP no longer baked + {} arena reclaimed - {} streaming code - {} safety floor = {} B",
            self.static_headroom_bytes,
            self.heap_in_use_bytes,
            self.baked_bsp_image_bytes,
            self.arena_reclaim_bytes,
            self.stream_overhead_bytes,
            self.safety_floor_bytes,
            self.world_pool_bytes()
        )
    }
}

/// Pool for region pages, the design's `P_world`, before the skeleton:
/// 331,676 - 4,281 + 93,808 - 35,436 - 16,384 = 369,383 B with today's RAM.
/// [D from [`RAM_BUDGET`]]
pub const P_WORLD_ESTIMATE_BYTES: u32 = RAM_BUDGET.world_pool_bytes();

/// Bytes the resident skeleton of a streamed world occupies in RAM, from the
/// wire sizes of `psx-bsp` (the streaming index, the top tree with its stub
/// leaves and two clip hulls, and the visibility lists), for `regions`
/// regions whose visibility lists hold `vis_entries` ids in all. The
/// materials and entity lumps come on top and are only known from a cook. [D]
pub fn skeleton_bytes(regions: usize, vis_entries: usize) -> u64 {
    use psx_bsp::pxbsp_resident::stream::{REGION_ENTRY_BYTES, STREAMING_INDEX_HEADER_BYTES};
    let r = regions as u64;
    let top_nodes = r.saturating_sub(1);
    let padded = r.div_ceil(8) * 8;
    STREAMING_INDEX_HEADER_BYTES as u64
        + r * REGION_ENTRY_BYTES as u64
        + 2 * vis_entries as u64
        + top_nodes * record::NODE as u64
        + top_nodes * record::PLANE as u64
        + (1 + padded) * record::LEAF as u64
        + (1 + 2 * top_nodes) * record::CLIPNODE as u64
}

/// Wire sizes of the cooked records a region carries, from `psx-bsp`. [M]
pub mod record {
    pub const VERTEX: u32 = 12;
    pub const PLANE: u32 = 12;
    pub const FACE: u32 = 10;
    pub const MARK: u32 = 2;
    pub const LEAF: u32 = 14;
    pub const NODE: u32 = 16;
    pub const CLIPNODE: u32 = 6;
    pub const ENTITY: u32 = 50;
}

/// Fixed capacities of one resident slot, per lump. [E]
///
/// The design asks for per-lump slot caps with a fill target of at least 70%
/// of the binding cap. The values are chosen so that a slot full of quads
/// lands near the hard payload cap; M6/M7 replace them with the real pool
/// geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotCaps {
    pub faces: u32,
    pub vertices: u32,
    pub nodes: u32,
    pub leaves: u32,
    pub mark_surfaces: u32,
    pub clip_nodes: u32,
}

impl Default for SlotCaps {
    fn default() -> Self {
        Self {
            faces: 1024,
            vertices: 4096,
            nodes: 512,
            leaves: 256,
            mark_surfaces: 2048,
            clip_nodes: 2048,
        }
    }
}

/// Named cook parameters. Defaults carry the design's labels.
#[derive(Clone, Debug, PartialEq)]
pub struct PartitionParams {
    // ---- payload caps (design 3.2) --------------------------------------
    /// Target region payload: 16 sectors. [E]
    pub region_target_bytes: u32,
    /// Hard cap per region: 32 sectors. [E]
    pub region_hard_cap_bytes: u32,
    /// Slot capacities per lump. [E]
    pub caps: SlotCaps,
    /// Desired fill of the binding cap, percent. [E]
    pub fill_target_pct: u32,
    /// Textures at or under this many sectors are duplicated into every
    /// region that uses them; larger ones live in shared packs. [E]
    pub small_texture_sectors: u32,
    /// A texture used by more than this share of regions is shared rather
    /// than duplicated inline, percent. [E]
    pub hot_texture_share_pct: u32,
    /// Fixed per-region header, plus per-material and per-spawn extras. [E]
    pub header_bytes: u32,
    /// Dense (uncompressed) PVS rows are the upper bound; the share of that
    /// bound assumed stored. 100 = no run-length gain assumed. [E]
    pub pvs_stored_pct: u32,

    // ---- cut search ------------------------------------------------------
    /// Lattice spacing for candidate cut planes, engine units. [E]
    pub cut_lattice: f64,
    /// No cell may be thinner than this along a cut axis. [E]
    pub min_cell_extent: f64,
    /// Candidate cuts evaluated per node and axis. [E]
    pub max_cut_candidates: usize,
    /// Sample budget for the open-area test of one cut or aperture. [E]
    pub aperture_samples: usize,
    /// Re-split passes after the estimate-driven tree. [E]
    pub max_refine_passes: u32,
    /// A cut that leaves a child thinner than this along its axis is a
    /// sliver and ranks behind every cut that leaves none (see `cuts`). [E]
    pub min_sliver_extent: f64,
    /// Further cook passes after the measured drive gate has failed on rho:
    /// three with another sliver extent, then cuts of the region the worst
    /// window enters; the cook keeps the best tree. [E]
    pub max_rho_passes: u32,

    // ---- visibility (design 3.4, estimated) -----------------------------
    /// Furthest anything is ever drawn, `D_vis`. [D, design 3.1]
    pub vis_distance: f64,
    /// Standing viewpoints sampled per region. [E]
    pub viewer_samples: usize,
    /// Surface points sampled per region as sight targets. [E]
    pub target_samples: usize,
    /// Camera eye above the floor sample. [E]
    pub eye_height: f64,

    // ---- drive and player model (design 4.2, 4.3) -----------------------
    /// Transfer time per sector at 2x, hundredths of a millisecond. [M]
    pub sector_ms_x100: u32,
    /// Seek to a region within 128 sectors, ms. [M]
    pub seek_near_ms: u32,
    /// Seek to a region about 512 sectors away, ms. [M]
    pub seek_far_ms: u32,
    /// Regions batched per seek for the batched figure. [E]
    pub batch_k: u32,
    /// Judge rho against the batched drive rate instead of the unbatched one.
    /// Valid when the disc layout really groups the regions a crossing
    /// brings in (the design: "rises to about 252 B/u when batched"). The
    /// default is the design's unbatched gate. [E]
    pub rho_batched: bool,
    /// Player top run speed, units per second, when the project names none.
    /// 100 authored units per tick is 6.25 engine units per tick, 375/s. [D]
    pub default_run_speed: f64,
    /// Safety factor on the run speed, percent. [E]
    pub speed_kappa_pct: u32,
    /// Target drive utilisation, percent. [E]
    pub utilisation_pct: u32,
    /// Hook reach. [M, docs/cortex/hook-points.md]
    pub hook_range: f64,
    /// Camera ball radius around the player, engine units. [D]
    pub viewer_radius: f64,
    /// Regions outstanding in the lead queue. [E]
    pub lead_depth_q: u32,
    /// Install CPU per region folded into the lead time, ms. Unmeasured until
    /// M7, so zero here. [?]
    pub install_ms: u32,
    /// RAM pool for region pages. [E]
    pub pool_bytes: u32,
    /// Human-readable list of parameters changed from the defaults, printed
    /// in the report so a verdict is never read against the wrong budget.
    pub overrides: Vec<String>,
}

impl Default for PartitionParams {
    fn default() -> Self {
        Self {
            region_target_bytes: 16 * SECTOR_BYTES,
            region_hard_cap_bytes: 32 * SECTOR_BYTES,
            caps: SlotCaps::default(),
            fill_target_pct: 70,
            small_texture_sectors: 4,
            hot_texture_share_pct: 25,
            header_bytes: 96,
            pvs_stored_pct: 100,
            cut_lattice: 64.0,
            min_cell_extent: 64.0,
            max_cut_candidates: 32,
            aperture_samples: 144,
            max_refine_passes: 3,
            min_sliver_extent: 256.0,
            max_rho_passes: 5,
            vis_distance: 2860.0,
            viewer_samples: 6,
            target_samples: 8,
            eye_height: 40.0,
            sector_ms_x100: 649,
            seek_near_ms: 137,
            seek_far_ms: 310,
            batch_k: 4,
            rho_batched: false,
            default_run_speed: 375.0,
            speed_kappa_pct: 125,
            utilisation_pct: 50,
            hook_range: 1600.0,
            viewer_radius: 125.0 + 40.0,
            lead_depth_q: 2,
            install_ms: 0,
            pool_bytes: P_WORLD_ESTIMATE_BYTES,
            overrides: Vec::new(),
        }
    }
}

/// Per-project cook parameter overrides, loadable from a small RON file.
/// Only the budgets worth adjusting per world are exposed.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct CookOverrides {
    pub pool_bytes: Option<u32>,
    pub region_target_bytes: Option<u32>,
    pub region_hard_cap_bytes: Option<u32>,
    pub utilisation_pct: Option<u32>,
    pub speed_kappa_pct: Option<u32>,
    pub lead_depth_q: Option<u32>,
    pub rho_batched: Option<bool>,
}

impl CookOverrides {
    pub fn from_ron_str(source: &str) -> Result<Self, String> {
        ron::from_str(source).map_err(|e| format!("cook overrides: {e}"))
    }

    /// Apply to `params`, recording each change.
    pub fn apply(&self, params: &mut PartitionParams) {
        macro_rules! set {
            ($field:ident) => {
                if let Some(value) = self.$field {
                    if params.$field != value {
                        params.overrides.push(format!(
                            "{} = {} (default {})",
                            stringify!($field),
                            value,
                            params.$field
                        ));
                        params.$field = value;
                    }
                }
            };
        }
        set!(pool_bytes);
        set!(region_target_bytes);
        set!(region_hard_cap_bytes);
        set!(utilisation_pct);
        set!(speed_kappa_pct);
        set!(lead_depth_q);
        set!(rho_batched);
    }
}

impl PartitionParams {
    /// `v_max`: top run speed with the safety factor applied, units/s. [D]
    pub fn v_max(&self, run_speed: f64) -> f64 {
        (run_speed * f64::from(self.speed_kappa_pct) / 100.0).ceil()
    }

    /// Milliseconds to read `sectors` once positioned.
    pub fn read_ms(&self, sectors: u32) -> f64 {
        f64::from(sectors) * f64::from(self.sector_ms_x100) / 100.0
    }
}

/// Everything that can stop a partition before a report exists.
#[derive(Debug)]
pub enum PartitionError {
    /// The shared front end rejected the world (invalid brush, empty world).
    FrontEnd(crate::brush_world::BrushWorldCookError),
    /// The world has no surfaces to cut.
    NoGeometry,
}

impl std::fmt::Display for PartitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FrontEnd(error) => write!(f, "world front end failed: {error:?}"),
            Self::NoGeometry => write!(f, "the world has no surfaces to partition"),
        }
    }
}

impl std::error::Error for PartitionError {}

/// A finished partition: regions, the graph between them, visibility and
/// requirement sets, disc layout and the gate verdicts.
#[derive(Clone, Debug)]
pub struct Partition {
    pub params: PartitionParams,
    pub world_bounds: Aabb,
    pub tree: CutTree,
    pub regions: Vec<Region>,
    pub graph: RegionGraph,
    pub closure: Closure,
    pub layout: Layout,
    pub gate: GateReport,
    /// Player run speed used, units per second.
    pub run_speed: f64,
    /// Region holding the player start, when the project has one.
    pub start_region: Option<u32>,
    /// Number of re-split passes that changed the tree.
    pub refine_passes: u32,
    /// Per-stage wall time in milliseconds is deliberately not stored: it
    /// would break byte-identical reports. See [`StreamReport`] for the
    /// optional timing line.
    pub total_input_faces: u32,
}

/// What a finished streamed cook knows that the partitioner estimated: the
/// real visibility lists, the real payload sizes and the real skeleton.
#[derive(Clone, Debug)]
pub struct CookMeasured {
    /// `V(R)` per region from the clustered portal flow, sorted, R included.
    pub visible: Vec<Vec<u32>>,
    /// Encoded payload bytes per region.
    pub payload_bytes: Vec<u32>,
    /// Size of the resident container (the skeleton plus materials and
    /// entities).
    pub container_bytes: u64,
    /// Leaf capacity of a slot in the rank rows.
    pub leaf_cap: u32,
}

impl Partition {
    /// The partition judged again with the cook's own numbers in place of the
    /// estimates: the sampled closure becomes the portal-flow closure, the
    /// estimated payloads become the encoded ones, and the skeleton is the
    /// real container. The gates, `Need` sets and layout use these, so a
    /// verdict from here no longer depends on a sampled line of sight.
    pub fn with_measured(&self, input: &PartitionInput, measured: &CookMeasured) -> Partition {
        let mut part = self.clone();
        for (region, &bytes) in part.regions.iter_mut().zip(&measured.payload_bytes) {
            region.counts.measured_bytes = Some(bytes);
        }
        let (need, hook_pulls) =
            closure::needs_from_visible(input, &part.params, &part.regions, &measured.visible);
        part.closure = Closure {
            visible: measured.visible.clone(),
            need,
            viewers: self.closure.viewers.clone(),
            hook_pulls,
            rays_cast: 0,
            regions: self.closure.regions,
            source: ClosureSource::PortalFlow,
        };
        part.gate = gate::evaluate(
            input,
            &part.params,
            &part.regions,
            &part.graph,
            &part.closure,
            part.run_speed,
            Some(gate::GateMeasured {
                skeleton_bytes: measured.container_bytes,
                leaf_cap: measured.leaf_cap,
                median_region_bytes: {
                    let mut sizes = measured.payload_bytes.clone();
                    sizes.sort_unstable();
                    sizes.get(sizes.len() / 2).copied().unwrap_or(0)
                },
            }),
        );
        part
    }

    /// Region containing `point`, by walking the cut tree.
    pub fn region_at(&self, point: V3) -> u32 {
        self.tree.locate(point)
    }
}

/// Partition an authored project and render its report in one step: the
/// entry point the cook path and the MCP query share.
pub fn report_for_project(
    project: &crate::ProjectDocument,
    project_root: &std::path::Path,
    params: &PartitionParams,
) -> Result<(Partition, StreamReport), PartitionError> {
    let input = PartitionInput::from_project(project, project_root)?;
    let partition = partition(&input, params);
    let report = StreamReport::build(&project.name, &input, &partition);
    Ok((partition, report))
}

/// Run the partitioner as part of a cook: write `stream_report.txt` and
/// `stream_report.json` into `generated_dir` and return the report. The
/// partitioner is host only and changes nothing the cook emits.
pub fn cook_stream_report(
    project: &crate::ProjectDocument,
    project_root: &std::path::Path,
    generated_dir: &std::path::Path,
    params: &PartitionParams,
) -> Result<StreamReport, PartitionError> {
    let (_, report) = report_for_project(project, project_root, params)?;
    if std::fs::create_dir_all(generated_dir).is_ok() {
        // Reports are advisory; a write failure must not fail the cook.
        let _ = std::fs::write(generated_dir.join("stream_report.txt"), &report.text);
        let _ = std::fs::write(generated_dir.join("stream_report.json"), &report.json);
    }
    Ok(report)
}

/// Partition `input` under `params`.
pub fn partition(input: &PartitionInput, params: &PartitionParams) -> Partition {
    partition_from(input, params, cuts::build_tree(input, params))
}

/// [`partition`] starting from a given cut tree, which the estimate-driven
/// re-splits then refine as usual. The cook uses it to cut a region further
/// once the measured drive gate has judged the tree.
pub fn partition_from(
    input: &PartitionInput,
    params: &PartitionParams,
    mut tree: CutTree,
) -> Partition {
    let run_speed = input.run_speed.unwrap_or(params.default_run_speed);
    let mut cache = account::AccountCache::new();
    let mut passes = 0;
    loop {
        let mut regions = account::account_regions(input, params, &tree, &mut cache);
        account::apply_texture_policy(input, params, &mut regions);
        let graph = graph::build_graph(input, params, &tree, &regions);
        let closure = closure::compute_closure(input, params, &regions, &graph);
        account::apply_pvs(params, &mut regions, &closure);
        let over = account::oversize_regions(params, &regions);
        if over.is_empty() || passes >= params.max_refine_passes {
            return finish(
                input, params, tree, regions, graph, closure, run_speed, passes,
            );
        }
        let changed = cuts::resplit(input, params, &mut tree, &over);
        if !changed {
            return finish(
                input, params, tree, regions, graph, closure, run_speed, passes,
            );
        }
        passes += 1;
    }
}

/// Map `f` over `inputs` on scoped threads, results in input order, so the
/// outcome never depends on scheduling.
pub(crate) fn par_map<T: Sync, R: Send>(inputs: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(8)
        .min(inputs.len().max(1));
    if workers <= 1 || inputs.len() < 4 {
        return inputs.iter().map(f).collect();
    }
    let chunk = inputs.len().div_ceil(workers);
    let mut out: Vec<Vec<R>> = Vec::new();
    std::thread::scope(|scope| {
        let handles: Vec<_> = inputs
            .chunks(chunk)
            .map(|part| scope.spawn(|| part.iter().map(&f).collect::<Vec<R>>()))
            .collect();
        for handle in handles {
            out.push(handle.join().expect("partition worker panicked"));
        }
    });
    out.into_iter().flatten().collect()
}

#[allow(clippy::too_many_arguments)]
fn finish(
    input: &PartitionInput,
    params: &PartitionParams,
    tree: CutTree,
    regions: Vec<Region>,
    graph: RegionGraph,
    closure: Closure,
    run_speed: f64,
    passes: u32,
) -> Partition {
    let layout = layout::compute_layout(params, &regions, &graph);
    let gate = gate::evaluate(input, params, &regions, &graph, &closure, run_speed, None);
    let start_region = input.player_start.map(|p| tree.locate(p));
    let total_input_faces = input.render.len() as u32;
    Partition {
        params: params.clone(),
        world_bounds: tree.root_bounds(),
        tree,
        regions,
        graph,
        closure,
        layout,
        gate,
        run_speed,
        start_region,
        refine_passes: passes,
        total_input_faces,
    }
}
