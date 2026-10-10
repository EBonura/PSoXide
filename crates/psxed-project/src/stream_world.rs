//! Deterministic stress-world generator for world streaming (design
//! 2026-10-08, milestone M8, generator half).
//!
//! A grid of square modules (rooms, hallways, partitioned interiors, walled
//! courtyards, terrain patches) joined by a randomised spanning tree plus a
//! few loop edges, sealed under a roof, with the two existing enemy archetypes
//! placed at spawn points, a player start, and a route set (shortest paths, a
//! sprint, random walks, a backtrack exercise, hook edges) written as data for
//! later tape generation.
//!
//! # Density is a construction rule, not a seed lottery
//!
//! The streaming gates ask three things of a world: a region's payload fits
//! the region budget, what one position needs resident (its closure, the lead
//! ring and the home pin) fits the RAM pool, and the bytes entering that set
//! per unit of travel fit the drive. All three follow from how many regions a
//! region sees and how many bytes a region holds, so the generator bounds both
//! by construction:
//!
//! * **Door degree.** No module has more than `max_door_degree` doors. The
//!   spanning tree and the loop edges both respect it, so a region has at most
//!   that many neighbours.
//! * **Vestibules.** Every door is screened by a baffle a few units inside
//!   it. A sight line through a door ends on the baffle, so a region sees its
//!   door neighbours (their vestibules) and nobody behind them. That bounds
//!   the closure to `1 + max_door_degree` regions, which is what the pool is
//!   sized for ([`PoolPlan`]).
//! * **Roof.** Every module is closed overhead (a ceiling, or the sky material
//!   on a courtyard or terrain patch), and the walls reach it. There is no
//!   open air layer for sight lines to run through, and no cell of sky.
//! * **Hook rooms are dead ends.** A hook pulls the closure of its landing
//!   room into the requirement of every region that sees it and is in hook
//!   range. A room with one door is seen only from its one neighbour, so a
//!   hook adds one room's worth of bytes, not a junction's.
//! * **Budget.** Each module is built to stay under `region_target_bytes` in
//!   the cut search's own byte model, so the cook never cuts through a module:
//!   a cell is a module, and its closure is its door neighbours. The shell,
//!   the vestibules and the hook pedestal are mandatory; the interior detail
//!   of a kind is added in priority order while it fits, and terrain patches
//!   pick the finest resolution that fits. A module whose mandatory shell
//!   already exceeds the budget is reported in the stats, not hidden.
//! * **Patch size.** `patch_extent_authored` is the cooker's own surface
//!   subdivision; the default is its coarsest (4096 authored, 256 units).
//!
//! What the world asks of the RAM pool, and what the RAM can give, is stated
//! by [`PoolPlan`] from [`RAM_BUDGET`]; see `StreamWorldConfig::pool_plan`.
//!
//! It looks like Graybox Reach on purpose: the donor project supplies every
//! material, character, model and the world settings. Output is reproducible
//! from a small config, so the project is regenerated on demand and not
//! tracked; see `editor/projects/stream-world.config.ron`.
//!
//! Units: modules are laid out in engine units (the Quake-scale units the
//! cooker uses) and written to the project in authored units (x16).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::brush::Brush;
use crate::brush_region::cuts::{surface_bytes, BRUSH_BYTES, TOPOLOGY_BYTES};
use crate::brush_region::{CookOverrides, PoolPlan, RamBudget, RAM_BUDGET};
use crate::terrain::{Terrain, TerrainShape};
use crate::units::WORLD_UNIT_DIVISOR;
use crate::{NodeId, NodeKind, ProjectDocument, ResourceData, ResourceId, Scene, Transform3};

/// Authored units per engine unit.
const A: i32 = WORLD_UNIT_DIVISOR;

/// Wall thickness each module contributes at its own boundary, engine units.
const WALL: i32 = 16;
const WALL_H: i32 = 384;
const DOOR_W: i32 = 192;
const DOOR_H: i32 = 224;
const SLAB: i32 = 64;
const ROOF_T: i32 = 32;
/// Distance from the inside face of a door's wall to its vestibule baffle,
/// the baffle's extra width past the door on each side, and its thickness.
const VESTIBULE_DEPTH: i32 = 64;
const BAFFLE_OVERHANG: i32 = 96;
const BAFFLE_T: i32 = 16;
const PILLAR: i32 = 48;
const COVER: i32 = 96;
const COVER_H: i32 = 64;
/// A hook pedestal fits the free corner between two vestibules.
const PEDESTAL: i32 = 96;
const PEDESTAL_H: i32 = 112;
const TERRAIN_AMPLITUDE: i32 = 96;

/// The cut search's model of a module came out up to this many times the sum
/// of its visible sides (door pieces, patch pieces and the BSP's own splits
/// add surfaces). Measured as the worst of 1.44 over three seeds of every
/// module kind; `module_estimates_bound_the_cut_search_model` fails if a
/// change moves it past this margin. [M]
const MODEL_MARGIN: f64 = 1.5;

/// Small tracked config; everything else is derived. Missing keys take the
/// default.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamWorldConfig {
    pub seed: u64,
    /// Modules along x and z.
    pub grid: [u32; 2],
    /// Module side, engine units. Multiple of 8.
    pub module_units: i32,
    /// `bsp_patch_extent` written into the World node, authored units. The
    /// cooker clamps it to 1024..=4096 (64 to 256 units); the default is the
    /// coarsest subdivision.
    pub patch_extent_authored: i32,
    /// Texture scale on every generated face, Q8 (512 = the texture appears
    /// twice as big). The cooker splits a face whose UV span passes 255
    /// texels, which at 256 would cut every 256-unit piece in two; stretching
    /// the texture lets the patch size, not the UV window, set the density.
    pub uv_scale_q8: i16,
    /// Module mix, percent. Rooms take the remainder.
    pub corridor_pct: u32,
    pub interior_pct: u32,
    pub courtyard_pct: u32,
    pub terrain_pct: u32,
    /// Extra door edges beyond the spanning tree, percent of candidates.
    pub loop_pct: u32,
    /// Chance a room, interior or courtyard holds an enemy, percent.
    pub enemy_pct: u32,
    /// Rooms that get a hook pedestal and Hook Point; the level cap is 32
    /// hook points. [M, docs/cortex/hook-points.md]
    pub max_hooks: u32,
    /// Minimum Chebyshev distance between hook rooms, in modules, so
    /// that no position has several hook landings in range.
    pub hook_spacing: u32,
    /// The deliberately dense variant: no interior detail, no vestibules, wide
    /// doors on every edge, so almost everything sees almost everything.
    pub dense: bool,
    /// Doors one module may have. Bounds how many regions a region sees.
    pub max_door_degree: u32,
    /// Terrain patch resolution at most, cells per module side.
    pub terrain_cells: u32,
    /// The cut target the world is cooked with, bytes in the cut search's
    /// model. Every module is built under it, so a cell is a module (a module
    /// whose bare shell is over it is cut in two and reported). Seven sectors
    /// holds a module with its door walls, vestibules and a little detail (the
    /// cut model counts more than the region encodes). The drive gate prices a
    /// crossing at the median region's sector count, so this is also what
    /// keeps the bytes a crossing brings in under the gate: at nine sectors
    /// 17 of 50 seeds failed it, at seven none do (see the sweep table in
    /// docs/streaming-world-sweep-2026-10-09.md).
    pub region_target_bytes: u32,
    /// Interior detail (pillars, cover) a module may add to its shell, in the
    /// same bytes. Zero builds bare shells. The sum with the shell never
    /// passes `region_target_bytes`.
    pub detail_bytes: u32,
    /// RAM the model and animation pass is assumed to hand the world, on top
    /// of what the guest has today. Zero is the RAM of today; any other value
    /// is a scenario, labelled as one in the pool plan.
    pub arena_reclaim_bytes: u32,
    pub shortest_routes: u32,
    pub random_walks: u32,
    pub walk_steps: u32,
}

impl Default for StreamWorldConfig {
    /// The full stress world.
    fn default() -> Self {
        Self {
            seed: 31,
            grid: [20, 20],
            module_units: 768,
            patch_extent_authored: 4096,
            uv_scale_q8: 512,
            corridor_pct: 20,
            interior_pct: 20,
            courtyard_pct: 8,
            terrain_pct: 6,
            loop_pct: 3,
            enemy_pct: 35,
            max_hooks: 12,
            hook_spacing: 4,
            dense: false,
            max_door_degree: 3,
            terrain_cells: 3,
            region_target_bytes: 14_336,
            detail_bytes: 1_024,
            arena_reclaim_bytes: 0,
            shortest_routes: 6,
            random_walks: 8,
            walk_steps: 120,
        }
    }
}

impl StreamWorldConfig {
    /// A small world for tests: same module mix, fewer modules.
    pub fn small(grid: [u32; 2]) -> Self {
        Self {
            grid,
            shortest_routes: 2,
            random_walks: 2,
            walk_steps: 24,
            ..Self::default()
        }
    }

    pub fn dense_variant(&self) -> Self {
        Self {
            dense: true,
            ..self.clone()
        }
    }

    /// The RAM the world is judged against: the guest's today, plus the
    /// scenario reclaim if the config names one.
    pub fn ram(&self) -> RamBudget {
        RamBudget {
            arena_reclaim_bytes: self.arena_reclaim_bytes,
            ..RAM_BUDGET
        }
    }

    /// What this world asks of the pool and what the RAM gives.
    pub fn pool_plan(&self) -> PoolPlan {
        let modules = (self.grid[0] * self.grid[1]) as usize;
        PoolPlan::derive(&self.ram(), modules, self.max_door_degree)
    }

    /// The cook parameters this world is made for: its cut target and the
    /// design's 2x hard cap, and the pool the RAM scenario gives.
    pub fn cook_overrides(&self) -> CookOverrides {
        CookOverrides {
            pool_bytes: Some(self.ram().world_pool_bytes()),
            region_target_bytes: Some(self.region_target_bytes),
            region_hard_cap_bytes: Some(2 * self.region_target_bytes),
            ..CookOverrides::default()
        }
    }

    pub fn from_ron_str(source: &str) -> Result<Self, String> {
        ron::from_str(source).map_err(|e| format!("stream-world config: {e}"))
    }

    pub fn to_ron_string(&self) -> String {
        ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default()).expect("config ron")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModuleKind {
    Room,
    Corridor,
    Interior,
    Courtyard,
    Terrain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RouteKind {
    Shortest,
    Sprint,
    RandomWalk,
    Backtrack,
    HookEdge,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookEdge {
    /// Where the player stands when the hook is chosen.
    pub from: [i32; 3],
    /// The hook point (landing).
    pub hook: [i32; 3],
}

/// One route as data. Points are engine units; the player runs the polyline
/// at `speed_pct` of its run speed (125 is the design's `v_max`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    pub name: String,
    pub kind: RouteKind,
    pub speed_pct: u32,
    pub points: Vec<[i32; 3]>,
    pub hook: Option<HookEdge>,
}

/// One module of the world, for tools that want the layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleInfo {
    pub kind: ModuleKind,
    /// Minimum corner, engine units.
    pub x: i32,
    pub z: i32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteSet {
    pub version: u32,
    pub seed: u64,
    pub grid: [u32; 2],
    pub module_units: i32,
    pub player_start: [i32; 3],
    /// Row-major modules and the door edges joining them (indices).
    pub modules: Vec<ModuleInfo>,
    pub doors: Vec<[u32; 2]>,
    pub routes: Vec<Route>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamWorldStats {
    pub modules: usize,
    pub rooms: usize,
    pub corridors: usize,
    pub interiors: usize,
    pub courtyards: usize,
    pub terrains: usize,
    pub edges: usize,
    pub brushes: usize,
    pub enemies: usize,
    pub hooks: usize,
    /// Largest door count of any module.
    pub max_door_degree: usize,
    /// Doors the spanning tree had to add past the degree cap to stay
    /// connected (zero when the cap holds everywhere).
    pub degree_overflows: usize,
    /// Largest estimated cut-model bytes of one module, and the sum over the
    /// world.
    pub max_module_bytes: u32,
    pub min_module_bytes: u32,
    pub estimated_bytes: u64,
    /// Modules whose mandatory shell is already over the budget.
    pub over_budget_modules: usize,
    /// Modules that lost interior detail to the budget.
    pub trimmed_modules: usize,
}

#[derive(Debug)]
pub struct StreamWorld {
    /// What the world asks of the pool and what the RAM gives.
    pub plan: PoolPlan,
    /// The cook parameters the world is made for.
    pub overrides: CookOverrides,
    pub project: ProjectDocument,
    pub routes: RouteSet,
    pub stats: StreamWorldStats,
    /// Upper bound of every module's bytes in the cut search's model, in
    /// module order.
    pub module_bytes: Vec<u32>,
}

/// SplitMix64: tiny, fixed, portable.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn pct(&mut self, percent: u32) -> bool {
        self.below(100) < u64::from(percent)
    }
}

/// Hash of a module or edge, independent of generation order.
fn mix(seed: u64, a: u64, b: u64) -> u64 {
    let mut r = Rng(seed ^ a.wrapping_mul(0x1000_0000_01B3) ^ b.wrapping_mul(0x9E37_79B9));
    r.next();
    r.next()
}

struct Materials {
    floor: ResourceId,
    wall: ResourceId,
    cliff: ResourceId,
    sky: ResourceId,
}

fn find_material(project: &ProjectDocument, name: &str) -> Option<ResourceId> {
    project.resources.iter().find_map(|r| {
        (r.name == name && matches!(r.data, ResourceData::Material(_))).then_some(r.id)
    })
}

fn pick_materials(donor: &ProjectDocument) -> Result<Materials, String> {
    let sky = donor
        .resources
        .iter()
        .find_map(|r| match &r.data {
            ResourceData::Material(m) if m.sky_aperture => Some(r.id),
            _ => None,
        })
        .ok_or("donor has no sky-aperture material")?;
    let need = |name: &str| {
        find_material(donor, name).ok_or_else(|| format!("donor has no material '{name}'"))
    };
    Ok(Materials {
        floor: need("Graybox / Ground 64")?,
        wall: need("Graybox / Original 64")?,
        cliff: need("Graybox / Cliff 64")?,
        sky,
    })
}

/// A cuboid in engine units. Built before it becomes a [`Brush`] so a module's
/// face count can be estimated against its budget.
#[derive(Clone, Copy)]
struct Cube {
    min: [i32; 3],
    max: [i32; 3],
    material: ResourceId,
    /// Sides that stay a surface after the solids are merged and the
    /// unreachable ones culled, one bit each: -x, +x, -y, +y, -z, +z.
    sides: u8,
}

/// Side bits of [`Cube::sides`].
const NX: u8 = 1;
const PX: u8 = 2;
const NY: u8 = 4;
const PY: u8 = 8;
const NZ: u8 = 16;
const PZ: u8 = 32;
const ALL_SIDES: u8 = 63;

impl Cube {
    fn new(min: [i32; 3], max: [i32; 3], material: ResourceId) -> Self {
        Self {
            min,
            max,
            material,
            sides: ALL_SIDES,
        }
    }

    /// A free-standing block: its four vertical sides, and its top unless it
    /// reaches the ceiling.
    fn solid(min: [i32; 3], max: [i32; 3], material: ResourceId) -> Self {
        let top = if max[1] < WALL_H { PY } else { 0 };
        Self::new(min, max, material).visible(NX | PX | NZ | PZ | top)
    }

    /// Only these sides count as surfaces in the byte estimate.
    fn visible(mut self, sides: u8) -> Self {
        self.sides = sides;
        self
    }

    fn brush(&self, uv_scale_q8: i16) -> Brush {
        let mut brush = Brush::cuboid(self.min.map(|v| v * A), self.max.map(|v| v * A));
        for face in &mut brush.faces {
            face.material = Some(self.material);
            face.uv.scale_q8 = [uv_scale_q8, uv_scale_q8];
        }
        brush
    }

    /// Render faces the cooker will cut the visible sides of this cuboid
    /// into at `patch` units.
    fn estimated_faces(&self, patch: i32) -> u32 {
        let d = [0, 1, 2].map(|a| self.max[a] - self.min[a]);
        let cells = |len: i32| ((len + patch - 1) / patch).max(1) as u32;
        let (x, y, z) = (cells(d[0]), cells(d[1]), cells(d[2]));
        let per_side = [y * z, y * z, x * z, x * z, x * y, x * y];
        (0..6)
            .filter(|bit| self.sides & (1 << bit) != 0)
            .map(|bit| per_side[bit])
            .sum()
    }

    /// Bytes the cut search's model gives this cuboid at most: its render
    /// faces as quads, one topology surface per visible side, and the brush
    /// itself.
    fn model_bytes(&self, patch: i32) -> f64 {
        MODEL_MARGIN
            * (f64::from(self.estimated_faces(patch)) * surface_bytes(4)
                + f64::from(self.sides.count_ones()) * TOPOLOGY_BYTES
                + BRUSH_BYTES)
    }
}

/// What one module is built from, before it is turned into brushes.
struct ModuleParts {
    cubes: Vec<Cube>,
    terrain: Option<Terrain>,
    /// Upper bound of the module's bytes in the cut search's model.
    model_bytes: f64,
}

#[derive(Clone, Copy)]
struct Module {
    kind: ModuleKind,
    /// Minimum corner, engine units.
    x: i32,
    z: i32,
}

/// Door between module `a` and its +x (east) or +z (south) neighbour.
#[derive(Clone, Copy, Debug)]
struct Edge {
    a: usize,
    b: usize,
    /// True when the shared wall is perpendicular to x (b is at +x).
    along_x: bool,
    /// Door centre along the shared wall, engine units.
    centre: i32,
}

struct Layout {
    cols: usize,
    rows: usize,
    modules: Vec<Module>,
    edges: Vec<Edge>,
    adjacency: Vec<Vec<(usize, usize)>>, // (neighbour, edge index)
    degree_overflows: usize,
}

fn build_layout(config: &StreamWorldConfig) -> Layout {
    let (cols, rows) = (config.grid[0] as usize, config.grid[1] as usize);
    let m = config.module_units;
    let x0 = -(cols as i32 * m) / 2;
    let z0 = -(rows as i32 * m) / 2;
    let mut modules = Vec::with_capacity(cols * rows);
    for j in 0..rows {
        for i in 0..cols {
            let h = mix(config.seed, i as u64, j as u64) % 100;
            let mut edge = config.corridor_pct as u64;
            let kind = if h < edge {
                ModuleKind::Corridor
            } else {
                edge += u64::from(config.interior_pct);
                if h < edge {
                    ModuleKind::Interior
                } else {
                    edge += u64::from(config.courtyard_pct);
                    if h < edge {
                        ModuleKind::Courtyard
                    } else {
                        edge += u64::from(config.terrain_pct);
                        if h < edge {
                            ModuleKind::Terrain
                        } else {
                            ModuleKind::Room
                        }
                    }
                }
            };
            modules.push(Module {
                kind,
                x: x0 + i as i32 * m,
                z: z0 + j as i32 * m,
            });
        }
    }
    // Candidate edges in a fixed order.
    let mut candidates: Vec<(usize, usize, bool)> = Vec::new();
    for j in 0..rows {
        for i in 0..cols {
            let a = j * cols + i;
            if i + 1 < cols {
                candidates.push((a, a + 1, true));
            }
            if j + 1 < rows {
                candidates.push((a, a + cols, false));
            }
        }
    }
    // Randomised Prim over the grid for the spanning tree, never taking a
    // module past the door cap while an admissible edge exists.
    let cap = config.max_door_degree.max(1) as usize;
    let mut rng = Rng(config.seed ^ 0xA5A5_5A5A);
    let mut degree = vec![0usize; modules.len()];
    let mut in_tree = vec![false; modules.len()];
    let mut chosen = vec![false; candidates.len()];
    let mut degree_overflows = 0;
    in_tree[0] = true;
    loop {
        let frontier: Vec<usize> = candidates
            .iter()
            .enumerate()
            .filter(|(_, (a, b, _))| in_tree[*a] != in_tree[*b])
            .map(|(k, _)| k)
            .collect();
        if frontier.is_empty() {
            break;
        }
        let room = |k: usize| {
            let (a, b, _) = candidates[k];
            degree[a] < cap && degree[b] < cap
        };
        let open: Vec<usize> = frontier.iter().copied().filter(|&k| room(k)).collect();
        let pool = if open.is_empty() {
            // Every way forward is at the cap: connectivity wins, and the
            // overflow is counted.
            degree_overflows += 1;
            &frontier
        } else {
            &open
        };
        let k = pool[rng.below(pool.len() as u64) as usize];
        let (a, b, _) = candidates[k];
        chosen[k] = true;
        degree[a] += 1;
        degree[b] += 1;
        in_tree[a] = true;
        in_tree[b] = true;
    }
    for (k, take) in chosen.iter_mut().enumerate() {
        let (a, b, _) = candidates[k];
        let wanted = rng.pct(config.loop_pct);
        if !*take
            && (config.dense || wanted)
            && (config.dense || (degree[a] < cap && degree[b] < cap))
        {
            *take = true;
            degree[a] += 1;
            degree[b] += 1;
        }
    }
    let mut edges = Vec::new();
    for (k, &(a, b, along_x)) in candidates.iter().enumerate() {
        if !chosen[k] {
            continue;
        }
        // Doors sit at the middle of the shared wall: with a vestibule behind
        // each, no sight line runs from one door to another.
        let centre = if along_x {
            modules[a].z + m / 2
        } else {
            modules[a].x + m / 2
        };
        edges.push(Edge {
            a,
            b,
            along_x,
            centre,
        });
    }
    let mut adjacency = vec![Vec::new(); modules.len()];
    for (k, e) in edges.iter().enumerate() {
        adjacency[e.a].push((e.b, k));
        adjacency[e.b].push((e.a, k));
    }
    Layout {
        cols,
        rows,
        modules,
        edges,
        adjacency,
        degree_overflows,
    }
}

/// One wall strip along an axis with an optional door. `inner` is the side
/// facing into the module; the strips next to a door show their end at it, and
/// the lintel its underside.
#[allow(clippy::too_many_arguments)]
fn wall(
    out: &mut Vec<Cube>,
    material: ResourceId,
    alongx: bool,
    fixed: (i32, i32),
    span: (i32, i32),
    top: i32,
    door: Option<(i32, i32)>,
    inner: u8,
) {
    // Ends along the wall: low end, high end.
    let (low_end, high_end) = if alongx { (NX, PX) } else { (NZ, PZ) };
    let mk = |lo: i32, hi: i32, y0: i32, y1: i32, sides: u8| -> Cube {
        if alongx {
            Cube::new([lo, y0, fixed.0], [hi, y1, fixed.1], material).visible(sides)
        } else {
            Cube::new([fixed.0, y0, lo], [fixed.1, y1, hi], material).visible(sides)
        }
    };
    match door {
        None => out.push(mk(span.0, span.1, 0, top, inner)),
        Some((d0, d1)) => {
            if d0 > span.0 {
                out.push(mk(span.0, d0, 0, top, inner | high_end));
            }
            if span.1 > d1 {
                out.push(mk(d1, span.1, 0, top, inner | low_end));
            }
            out.push(mk(d0, d1, DOOR_H, top, inner | NY));
        }
    }
}

/// Door of a module on each side: west, east, north, south.
type Doors = [Option<(i32, i32)>; 4];

const WEST: usize = 0;
const EAST: usize = 1;
const NORTH: usize = 2;
const SOUTH: usize = 3;

fn door_width(config: &StreamWorldConfig) -> i32 {
    if config.dense {
        config.module_units - 2 * WALL - 64
    } else {
        DOOR_W
    }
}

fn module_doors(layout: &Layout, config: &StreamWorldConfig, index: usize) -> Doors {
    let w = door_width(config);
    let mut doors: Doors = [None; 4];
    for e in &layout.edges {
        let span = (e.centre - w / 2, e.centre + w / 2);
        match (e.along_x, e.a == index, e.b == index) {
            (true, true, _) => doors[EAST] = Some(span),
            (true, _, true) => doors[WEST] = Some(span),
            (false, true, _) => doors[SOUTH] = Some(span),
            (false, _, true) => doors[NORTH] = Some(span),
            _ => {}
        }
    }
    doors
}

/// Terrain resolutions a module side can be cut into, finest first, capped at
/// `max_cells` (the span must divide evenly). One cell is two wedges: the
/// coarsest patch, taken when the shell leaves no room for four cells, so
/// that a terrain module is not cut in two (a cut module is seen by more
/// regions than a whole one, and brings more bytes in per crossing). A shell
/// with no room even for that stays flat.
fn terrain_resolutions(span: i32, max_cells: u32) -> Vec<usize> {
    (1..=max_cells.min(16) as usize)
        .rev()
        .filter(|&c| span % c as i32 == 0)
        .collect()
}

struct Budgets {
    /// Cut-model bytes one module may hold.
    bytes: f64,
    /// Cut-model bytes of interior detail it may add to its shell.
    detail: f64,
    patch: i32,
}

#[derive(Default)]
struct ModuleOutcome {
    over_budget: bool,
    trimmed: bool,
}

/// Everything one module is made of, inside its budget.
#[allow(clippy::too_many_arguments)]
fn build_module(
    config: &StreamWorldConfig,
    layout: &Layout,
    mats: &Materials,
    budgets: &Budgets,
    index: usize,
    hook_corner: Option<usize>,
    hash: u64,
) -> Result<(ModuleParts, ModuleOutcome, Option<[i32; 3]>), String> {
    let module = layout.modules[index];
    let m = config.module_units;
    let (x0, z0) = (module.x, module.z);
    let (x1, z1) = (x0 + m, z0 + m);
    let (cx, cz) = (x0 + m / 2, z0 + m / 2);
    let doors = module_doors(layout, config, index);
    let detail = !config.dense;
    let open_top = matches!(module.kind, ModuleKind::Courtyard | ModuleKind::Terrain);
    let mut cubes: Vec<Cube> = Vec::new();

    // Shell. The slab runs under terrain too: the walls start at 0, so a lower
    // slab top would leave a gap at the module edge.
    cubes.push(Cube::new([x0, -SLAB, z0], [x1, 0, z1], mats.floor).visible(PY));
    wall(
        &mut cubes,
        mats.wall,
        false,
        (x0, x0 + WALL),
        (z0, z1),
        WALL_H,
        doors[WEST],
        PX,
    );
    wall(
        &mut cubes,
        mats.wall,
        false,
        (x1 - WALL, x1),
        (z0, z1),
        WALL_H,
        doors[EAST],
        NX,
    );
    wall(
        &mut cubes,
        mats.wall,
        true,
        (z0, z0 + WALL),
        (x0 + WALL, x1 - WALL),
        WALL_H,
        doors[NORTH],
        PZ,
    );
    wall(
        &mut cubes,
        mats.wall,
        true,
        (z1 - WALL, z1),
        (x0 + WALL, x1 - WALL),
        WALL_H,
        doors[SOUTH],
        NZ,
    );
    // The roof closes the module: a ceiling, or the sky on an open one.
    cubes.push(
        Cube::new(
            [x0, WALL_H, z0],
            [x1, WALL_H + ROOF_T, z1],
            if open_top { mats.sky } else { mats.wall },
        )
        .visible(NY),
    );
    // Vestibules: one baffle inside each door.
    if detail {
        let half = DOOR_W / 2 + BAFFLE_OVERHANG;
        let depth = WALL + VESTIBULE_DEPTH;
        if let Some((d0, d1)) = doors[WEST] {
            let c = (d0 + d1) / 2;
            cubes.push(Cube::solid(
                [x0 + depth, 0, c - half],
                [x0 + depth + BAFFLE_T, WALL_H, c + half],
                mats.wall,
            ));
        }
        if let Some((d0, d1)) = doors[EAST] {
            let c = (d0 + d1) / 2;
            cubes.push(Cube::solid(
                [x1 - depth - BAFFLE_T, 0, c - half],
                [x1 - depth, WALL_H, c + half],
                mats.wall,
            ));
        }
        if let Some((d0, d1)) = doors[NORTH] {
            let c = (d0 + d1) / 2;
            cubes.push(Cube::solid(
                [c - half, 0, z0 + depth],
                [c + half, WALL_H, z0 + depth + BAFFLE_T],
                mats.wall,
            ));
        }
        if let Some((d0, d1)) = doors[SOUTH] {
            let c = (d0 + d1) / 2;
            cubes.push(Cube::solid(
                [c - half, 0, z1 - depth - BAFFLE_T],
                [c + half, WALL_H, z1 - depth],
                mats.wall,
            ));
        }
    }
    // Hook pedestal in the corner between two vestibules.
    let mut hook = None;
    if detail {
        if let Some(corner) = hook_corner {
            let px = if corner & 1 == 0 {
                x0 + WALL + 8
            } else {
                x1 - WALL - 8 - PEDESTAL
            };
            let pz = if corner & 2 == 0 {
                z0 + WALL + 8
            } else {
                z1 - WALL - 8 - PEDESTAL
            };
            cubes.push(Cube::solid(
                [px, 0, pz],
                [px + PEDESTAL, PEDESTAL_H, pz + PEDESTAL],
                mats.cliff,
            ));
            hook = Some([px + PEDESTAL / 2, PEDESTAL_H, pz + PEDESTAL / 2]);
        }
    }

    let model_of =
        |cubes: &[Cube]| -> f64 { cubes.iter().map(|c| c.model_bytes(budgets.patch)).sum() };
    let mut outcome = ModuleOutcome::default();
    let mut terrain = None;
    let mut model = model_of(&cubes);
    outcome.over_budget = model > budgets.bytes;

    // Terrain takes the finest resolution that still fits. A shell with no
    // room for even one patch stays a flat open court (trimmed, not over
    // budget, unless the shell alone is over).
    if module.kind == ModuleKind::Terrain {
        let span = m - 2 * WALL;
        let options = terrain_resolutions(span, config.terrain_cells);
        if options.is_empty() {
            return Err("module_units leave no terrain resolution".into());
        }
        // A wedge is a render face or two (counted as quads), its sides and a brush.
        let wedge_bytes =
            MODEL_MARGIN * (2.0 * surface_bytes(4) + 6.0 * TOPOLOGY_BYTES + BRUSH_BYTES);
        let cost = |c: usize| (2 * c * c) as f64 * wedge_bytes;
        let pick = options
            .iter()
            .copied()
            .find(|&c| model + cost(c) <= budgets.bytes);
        if pick != options.first().copied() {
            outcome.trimmed = true;
        }
        if let Some(pick) = pick {
            let spacing = (span / pick as i32) * A;
            let patch = Terrain::generate(
                [pick, pick],
                [spacing, spacing],
                [(x0 + WALL) * A, 0, (z0 + WALL) * A],
                TERRAIN_AMPLITUDE * A,
                (hash & 0xFFFF_FFFF) as u32,
                TerrainShape::Hills,
                0.5,
            )
            .map_err(|e| format!("terrain at module {index}: {e}"))?;
            let mut patch = patch;
            // Level rim: the doors open onto the patch's edge, and a walker
            // must be able to step in from the flat floor of the next module.
            let stride = pick + 1;
            for z in 0..stride {
                for x in 0..stride {
                    if x == 0 || z == 0 || x == pick || z == pick {
                        patch.heights[z * stride + x] = 0.0;
                    }
                }
            }
            model += cost(pick);
            terrain = Some(patch);
        }
    }

    // Interior detail, most valuable first, while it fits. Terrain counts as
    // part of the shell.
    let shell_model = model;
    if detail {
        let at = |dx: i32, dz: i32, size: i32, h: i32, mat: ResourceId| {
            Cube::solid(
                [cx + dx - size / 2, 0, cz + dz - size / 2],
                [cx + dx + size / 2, h, cz + dz + size / 2],
                mat,
            )
        };
        let features: Vec<Cube> = match module.kind {
            ModuleKind::Room => [(-64, -64), (64, 64), (64, -64), (-64, 64)]
                .into_iter()
                .map(|(dx, dz)| at(dx, dz, PILLAR, WALL_H, mats.wall))
                .collect(),
            ModuleKind::Corridor => [(-96, 0), (96, 0)]
                .into_iter()
                .map(|(dx, dz)| at(dx, dz, COVER, COVER_H, mats.cliff))
                .collect(),
            ModuleKind::Interior => [(-64, -64), (64, 64), (64, -64), (-64, 64)]
                .into_iter()
                .map(|(dx, dz)| at(dx, dz, PILLAR, WALL_H, mats.wall))
                .chain(
                    [(-144, -144), (144, 144), (144, -144), (-144, 144)]
                        .into_iter()
                        .map(|(dx, dz)| at(dx, dz, COVER, COVER_H, mats.cliff)),
                )
                .collect(),
            ModuleKind::Courtyard => [(-112, 64), (112, -64), (0, -128)]
                .into_iter()
                .map(|(dx, dz)| at(dx, dz, COVER, COVER_H + 32 * (hash % 3) as i32, mats.cliff))
                .collect(),
            ModuleKind::Terrain => Vec::new(),
        };
        let ceiling = (shell_model + budgets.detail).min(budgets.bytes);
        for feature in features {
            let cost = feature.model_bytes(budgets.patch);
            if model + cost > ceiling {
                outcome.trimmed = true;
                break;
            }
            model += cost;
            cubes.push(feature);
        }
    }
    Ok((
        ModuleParts {
            cubes,
            terrain,
            model_bytes: model,
        },
        outcome,
        hook,
    ))
}

fn host_components(
    project: &ProjectDocument,
    name: &str,
) -> Result<Vec<(String, NodeKind)>, String> {
    let scene = project.active_scene();
    let host = scene
        .nodes()
        .iter()
        .find(|n| n.name == name && matches!(n.kind, NodeKind::Entity))
        .ok_or_else(|| format!("donor has no '{name}' entity"))?;
    Ok(host
        .children
        .iter()
        .filter_map(|id| scene.node(*id))
        .filter(|n| n.kind.is_component())
        .map(|n| (n.name.clone(), n.kind.clone()))
        .collect())
}

fn character_id(project: &ProjectDocument, name: &str) -> Result<ResourceId, String> {
    project
        .resources
        .iter()
        .find_map(|r| {
            (r.name == name && matches!(r.data, ResourceData::Character(_))).then_some(r.id)
        })
        .ok_or_else(|| format!("donor has no Character '{name}'"))
}

fn add_host(
    scene: &mut Scene,
    name: &str,
    translation: [f32; 3],
    yaw: f32,
    components: &[(String, NodeKind)],
) {
    let host = scene.add_node(NodeId::ROOT, name, NodeKind::Entity);
    scene.node_mut(host).expect("host").transform = Transform3 {
        translation,
        rotation_degrees: [0.0, yaw, 0.0],
        ..Transform3::default()
    };
    for (cname, kind) in components {
        scene.add_node(host, cname, kind.clone());
    }
}

/// Retarget a cloned enemy component set to another Character.
fn enemy_components(
    base: &[(String, NodeKind)],
    donor: &ProjectDocument,
    character: ResourceId,
) -> Vec<(String, NodeKind)> {
    let (model, material) = match donor.resource(character).map(|r| &r.data) {
        Some(ResourceData::Character(c)) => (c.model, c.material),
        _ => (None, None),
    };
    base.iter()
        .map(|(name, kind)| {
            let kind = match kind.clone() {
                NodeKind::CharacterController { player, .. } => NodeKind::CharacterController {
                    character: Some(character),
                    loadout: None,
                    settings: None,
                    player,
                },
                NodeKind::ModelRenderer {
                    visual_offset,
                    visual_scale_q8,
                    ..
                } => NodeKind::ModelRenderer {
                    model,
                    material,
                    visual_offset,
                    visual_scale_q8,
                },
                other => other,
            };
            (name.clone(), kind)
        })
        .collect()
}

pub fn generate(
    config: &StreamWorldConfig,
    donor: &ProjectDocument,
) -> Result<StreamWorld, String> {
    let m = config.module_units;
    if m % 8 != 0 || m < 512 {
        return Err("module_units must be a multiple of 8 and at least 512".into());
    }
    if config.grid[0] == 0 || config.grid[1] == 0 {
        return Err("grid must be at least 1 x 1".into());
    }
    let mats = pick_materials(donor)?;
    let layout = build_layout(config);
    let (cols, rows) = (layout.cols, layout.rows);
    let world_min = [layout.modules[0].x, layout.modules[0].z];
    let world_max = [
        world_min[0] + cols as i32 * m,
        world_min[1] + rows as i32 * m,
    ];
    if [world_min[0], world_min[1], world_max[0], world_max[1]]
        .iter()
        .any(|v| v.abs() * A > 131_072)
    {
        return Err("world exceeds the brush edit extent of 131072 authored units".into());
    }
    // The cooker cuts a face at the patch size or where its UV span passes 255
    // texels, whichever is finer.
    let uv_window = 255 * i32::from(config.uv_scale_q8.max(1)) / 256;
    let patch = (config.patch_extent_authored / A)
        .clamp(64, 256)
        .min(uv_window);
    let budgets = Budgets {
        bytes: f64::from(config.region_target_bytes),
        detail: f64::from(config.detail_bytes),
        patch,
    };

    let mut brushes: Vec<Brush> = Vec::new();
    let mut stats = StreamWorldStats {
        modules: layout.modules.len(),
        edges: layout.edges.len(),
        degree_overflows: layout.degree_overflows,
        max_door_degree: layout.adjacency.iter().map(Vec::len).max().unwrap_or(0),
        ..StreamWorldStats::default()
    };
    // The module the player starts in: a quiet dead-end hallway else a dead-end
    // room. No enemy and no hook there.
    // A dead end keeps the home pin to two regions.
    let start_index = {
        let dead_end = |k: &usize| layout.adjacency[*k].len() == 1;
        let of_kind = |kind: ModuleKind| {
            (0..layout.modules.len()).find(|k| layout.modules[*k].kind == kind && dead_end(k))
        };
        of_kind(ModuleKind::Corridor)
            .or_else(|| of_kind(ModuleKind::Room))
            .or_else(|| {
                layout
                    .modules
                    .iter()
                    .position(|mo| mo.kind == ModuleKind::Corridor)
            })
            .unwrap_or(0)
    };
    // Hook pedestals go to rooms with the lowest hash, spaced apart, up to
    // the cap.
    let hook_modules: Vec<usize> = {
        let mut rooms: Vec<usize> = layout
            .modules
            .iter()
            .enumerate()
            .filter(|(k, mo)| {
                mo.kind == ModuleKind::Room && *k != start_index && layout.adjacency[*k].len() == 1
            })
            .map(|(k, _)| k)
            .collect();
        rooms.sort_by_key(|&k| (mix(config.seed, 9000 + k as u64, 1), k));
        let mut chosen: Vec<usize> = Vec::new();
        for k in rooms {
            if chosen.len() >= config.max_hooks.min(32) as usize {
                break;
            }
            let (ki, kj) = ((k % cols) as i64, (k / cols) as i64);
            let clear = chosen.iter().all(|&c| {
                let (ci, cj) = ((c % cols) as i64, (c / cols) as i64);
                (ki - ci).abs().max((kj - cj).abs()) >= i64::from(config.hook_spacing)
            });
            if clear {
                chosen.push(k);
            }
        }
        chosen
    };
    let mut enemies: Vec<(String, [i32; 3], bool)> = Vec::new();
    let mut hooks: Vec<[i32; 3]> = Vec::new();
    let mut module_bytes: Vec<u32> = Vec::with_capacity(layout.modules.len());

    for (index, module) in layout.modules.iter().enumerate() {
        let (i, j) = (index % cols, index / cols);
        let h = mix(config.seed, 1000 + index as u64, 7);
        let hook_corner =
            (!config.dense && hook_modules.contains(&index)).then_some((h >> 12) as usize % 4);
        let (parts, outcome, hook) =
            build_module(config, &layout, &mats, &budgets, index, hook_corner, h)?;
        stats.max_module_bytes = stats.max_module_bytes.max(parts.model_bytes as u32);
        stats.min_module_bytes = if index == 0 {
            parts.model_bytes as u32
        } else {
            stats.min_module_bytes.min(parts.model_bytes as u32)
        };
        stats.estimated_bytes += parts.model_bytes as u64;
        module_bytes.push(parts.model_bytes.ceil() as u32);
        stats.over_budget_modules += usize::from(outcome.over_budget);
        stats.trimmed_modules += usize::from(outcome.trimmed);
        match module.kind {
            ModuleKind::Room => stats.rooms += 1,
            ModuleKind::Corridor => stats.corridors += 1,
            ModuleKind::Interior => stats.interiors += 1,
            ModuleKind::Courtyard => stats.courtyards += 1,
            ModuleKind::Terrain => stats.terrains += 1,
        }
        brushes.extend(parts.cubes.iter().map(|c| c.brush(config.uv_scale_q8)));
        if let Some(terrain) = &parts.terrain {
            brushes.extend(terrain.brushes(Some(mats.cliff))?);
        }
        hooks.extend(hook);
        if index != start_index
            && !matches!(module.kind, ModuleKind::Corridor | ModuleKind::Terrain)
            && mix(config.seed, 5000 + index as u64, 3) % 100 < u64::from(config.enemy_pct)
        {
            let heavy = mix(config.seed, 6000 + index as u64, 9).is_multiple_of(3);
            let (cx, cz) = (module.x + m / 2, module.z + m / 2);
            let offset = if module.kind == ModuleKind::Courtyard {
                (-160, -160)
            } else {
                (0, 0)
            };
            enemies.push((
                format!(
                    "Enemy {} {}-{}",
                    if heavy { "Heavy" } else { "Light" },
                    i,
                    j
                ),
                [cx + offset.0, 4, cz + offset.1],
                heavy,
            ));
        }
    }
    stats.brushes = brushes.len();
    stats.enemies = enemies.len();
    stats.hooks = hooks.len();

    let start = layout.modules[start_index];
    let player_start = [start.x + m / 2, 16, start.z + m / 2];

    // ---- project ---------------------------------------------------------
    let player_components = host_components(donor, "Aletha (Player)")?;
    let light_base = host_components(donor, "Single Enemy / Behaviour Study")?;
    let light = character_id(donor, "Light Enemy")?;
    let heavy = character_id(donor, "Heavy Enemy")?;
    let light_components = enemy_components(&light_base, donor, light);
    let heavy_components = enemy_components(&light_base, donor, heavy);

    let world_kind = {
        let scene = donor.active_scene();
        let mut kind = scene
            .node(NodeId::ROOT)
            .map(|n| n.kind.clone())
            .ok_or("donor scene has no world root")?;
        if let NodeKind::World { culling, .. } = &mut kind {
            culling.bsp_patch_extent = config.patch_extent_authored;
        }
        kind
    };
    let mut project = donor.clone();
    project.name = if config.dense {
        "Stream World (dense)".to_string()
    } else {
        "Stream World".to_string()
    };
    project.boot = crate::BootTarget::Gameplay;
    let blank = ProjectDocument::new("blank scene");
    project.scenes = blank.scenes;
    project.ui_scenes = blank.ui_scenes;
    project.scene_states = blank.scene_states;
    project.options.clear();
    {
        let scene = project.active_scene_mut();
        scene.node_mut(NodeId::ROOT).expect("root").kind = world_kind;
        scene.brushes = brushes;
        add_host(
            scene,
            "Aletha (Player)",
            player_start.map(|v| (v * A) as f32),
            0.0,
            &player_components,
        );
        for (name, at, is_heavy) in &enemies {
            add_host(
                scene,
                name,
                at.map(|v| (v * A) as f32),
                0.0,
                if *is_heavy {
                    &heavy_components
                } else {
                    &light_components
                },
            );
        }
        for (k, at) in hooks.iter().enumerate() {
            let id = scene.add_node(NodeId::ROOT, format!("Hook {k}"), NodeKind::HookPoint);
            scene.node_mut(id).expect("hook").transform = Transform3 {
                translation: at.map(|v| (v * A) as f32),
                ..Transform3::default()
            };
        }
    }

    let routes = build_routes(config, &layout, start_index, player_start, &hooks);
    Ok(StreamWorld {
        plan: config.pool_plan(),
        overrides: config.cook_overrides(),
        project,
        routes,
        stats,
        module_bytes,
    })
}

fn module_centre(layout: &Layout, config: &StreamWorldConfig, index: usize) -> [i32; 3] {
    let mo = layout.modules[index];
    [
        mo.x + config.module_units / 2,
        0,
        mo.z + config.module_units / 2,
    ]
}

/// Door centre between two adjacent modules, on the shared boundary.
fn door_point(layout: &Layout, config: &StreamWorldConfig, edge: usize) -> [i32; 3] {
    let e = layout.edges[edge];
    let a = layout.modules[e.a];
    let m = config.module_units;
    if e.along_x {
        [a.x + m, 0, e.centre]
    } else {
        [e.centre, 0, a.z + m]
    }
}

/// The waypoints that carry a walker from the middle of `from` through the
/// door of `edge` to the middle of `to`: out past the sides of the vestibule
/// baffle on each end, so the polyline never crosses a baffle.
fn door_crossing(
    layout: &Layout,
    config: &StreamWorldConfig,
    from: usize,
    edge: usize,
) -> Vec<[i32; 3]> {
    let e = layout.edges[edge];
    let to = if e.a == from { e.b } else { e.a };
    let door = door_point(layout, config, edge);
    // Unit direction of the wall (lateral) and of the walk from `from` to `to`.
    let (lateral, forward): ([i32; 3], i32) = if e.along_x {
        ([0, 0, 1], if e.a == from { 1 } else { -1 })
    } else {
        ([1, 0, 0], if e.a == from { 1 } else { -1 })
    };
    let normal: [i32; 3] = if e.along_x { [1, 0, 0] } else { [0, 0, 1] };
    let side = DOOR_W / 2 + BAFFLE_OVERHANG + 24;
    let near = WALL + 40;
    let clear = WALL + VESTIBULE_DEPTH + BAFFLE_T + 40;
    // `inward` runs from the door back into `from` (negative: into `to`);
    // `offset` is the lateral shift, `side` clear of the baffle or 0 in line
    // with the door.
    let at = |inward: i32, offset: i32| -> [i32; 3] {
        [0, 1, 2].map(|a| door[a] - forward * normal[a] * inward + lateral[a] * offset)
    };
    let mut points = vec![module_centre(layout, config, from)];
    if config.dense {
        points.push(door);
    } else {
        // Round the baffle on the side, then in between the wall and the baffle
        // to the door: a straight line from beside the baffle to the door would
        // cut the door jamb.
        points.extend([
            at(clear, side),
            at(near, side),
            at(near, 0),
            door,
            at(-near, 0),
            at(-near, side),
            at(-clear, side),
        ]);
    }
    points.push(module_centre(layout, config, to));
    points
}

fn path_points(
    layout: &Layout,
    config: &StreamWorldConfig,
    path: &[usize],
    start: [i32; 3],
) -> Vec<[i32; 3]> {
    let mut points = vec![start];
    for pair in path.windows(2) {
        let edge = layout.adjacency[pair[0]]
            .iter()
            .find(|(n, _)| *n == pair[1])
            .map(|(_, e)| *e)
            .expect("path steps follow edges");
        points.extend(door_crossing(layout, config, pair[0], edge));
    }
    if path.len() == 1 {
        points.push(module_centre(layout, config, path[0]));
    }
    points.dedup();
    points
}

fn bfs(layout: &Layout, from: usize) -> (Vec<u32>, Vec<usize>) {
    let mut depth = vec![u32::MAX; layout.modules.len()];
    let mut parent = vec![usize::MAX; layout.modules.len()];
    depth[from] = 0;
    let mut queue = std::collections::VecDeque::from([from]);
    while let Some(v) = queue.pop_front() {
        for &(n, _) in &layout.adjacency[v] {
            if depth[n] == u32::MAX {
                depth[n] = depth[v] + 1;
                parent[n] = v;
                queue.push_back(n);
            }
        }
    }
    (depth, parent)
}

fn path_to(parent: &[usize], from: usize, to: usize) -> Vec<usize> {
    let mut path = vec![to];
    let mut at = to;
    while at != from {
        at = parent[at];
        path.push(at);
    }
    path.reverse();
    path
}

fn build_routes(
    config: &StreamWorldConfig,
    layout: &Layout,
    start: usize,
    player_start: [i32; 3],
    hooks: &[[i32; 3]],
) -> RouteSet {
    let (depth, parent) = bfs(layout, start);
    let mut order: Vec<usize> = (0..layout.modules.len()).collect();
    order.sort_by(|&a, &b| depth[b].cmp(&depth[a]).then(a.cmp(&b)));
    let mut routes = Vec::new();

    // Shortest paths to targets spread over the depth range, farthest first.
    let count = (config.shortest_routes as usize).min(order.len().saturating_sub(1));
    for k in 0..count {
        let target = order[k * order.len() / count.max(1)];
        if target == start {
            continue;
        }
        let path = path_to(&parent, start, target);
        routes.push(Route {
            name: format!("shortest-{k}"),
            kind: RouteKind::Shortest,
            speed_pct: 100,
            points: path_points(layout, config, &path, player_start),
            hook: None,
        });
    }
    // Sprint along the longest shortest path at v_max.
    if let Some(&far) = order.first() {
        if far != start {
            let path = path_to(&parent, start, far);
            routes.push(Route {
                name: "sprint-longest".into(),
                kind: RouteKind::Sprint,
                speed_pct: 125,
                points: path_points(layout, config, &path, player_start),
                hook: None,
            });
        }
    }
    // Random walks, a few at sprint speed. A walk may step back.
    let mut rng = Rng(config.seed ^ 0x0BAD_CAFE);
    for w in 0..config.random_walks {
        let mut at = start;
        let mut path = vec![at];
        let mut previous = usize::MAX;
        for _ in 0..config.walk_steps {
            let options: Vec<usize> = layout.adjacency[at].iter().map(|(n, _)| *n).collect();
            if options.is_empty() {
                break;
            }
            let forward: Vec<usize> = options.iter().copied().filter(|&n| n != previous).collect();
            let pick = if forward.is_empty() || rng.pct(15) {
                options[rng.below(options.len() as u64) as usize]
            } else {
                forward[rng.below(forward.len() as u64) as usize]
            };
            previous = at;
            at = pick;
            path.push(at);
        }
        routes.push(Route {
            name: format!("walk-{w}"),
            kind: RouteKind::RandomWalk,
            speed_pct: if w % 3 == 2 { 125 } else { 100 },
            points: path_points(layout, config, &path, player_start),
            hook: None,
        });
    }
    // Boundary dither: out through a door, half a step back, repeatedly.
    if let Some(&(_, e)) = layout.adjacency[start].first() {
        let crossing = door_crossing(layout, config, start, e);
        // Out to just past the door (the door itself when there is no
        // vestibule), then back.
        let out = &crossing[1..if config.dense { 2 } else { 5 }];
        let mut points = vec![player_start];
        for _ in 0..4 {
            points.extend(out);
            points.extend(out[..out.len() - 1].iter().rev());
        }
        routes.push(Route {
            name: "backtrack-at-boundary".into(),
            kind: RouteKind::Backtrack,
            speed_pct: 100,
            points,
            hook: None,
        });
    }
    // Hook edges: from every module within hook reach of a hook.
    for (h, &hook) in hooks.iter().enumerate() {
        let mut sources: Vec<usize> = (0..layout.modules.len())
            .filter(|&k| {
                let c = module_centre(layout, config, k);
                let (dx, dz) = (i64::from(c[0] - hook[0]), i64::from(c[2] - hook[2]));
                dx * dx + dz * dz <= 1600i64 * 1600 && depth[k] != u32::MAX
            })
            .collect();
        sources.sort_unstable();
        sources.truncate(4);
        for k in sources {
            let from = module_centre(layout, config, k);
            routes.push(Route {
                name: format!("hook-{h}-from-{k}"),
                kind: RouteKind::HookEdge,
                speed_pct: 100,
                points: vec![from, hook],
                hook: Some(HookEdge { from, hook }),
            });
        }
    }
    RouteSet {
        version: 1,
        seed: config.seed,
        grid: config.grid,
        module_units: config.module_units,
        player_start,
        modules: layout
            .modules
            .iter()
            .map(|m| ModuleInfo {
                kind: m.kind,
                x: m.x,
                z: m.z,
            })
            .collect(),
        doors: layout
            .edges
            .iter()
            .map(|e| [e.a as u32, e.b as u32])
            .collect(),
        routes,
    }
}

/// Load the donor (Graybox Reach) from an `editor/projects` directory.
pub fn load_donor(projects_dir: &Path) -> Result<ProjectDocument, String> {
    let path = projects_dir.join("graybox-reach").join("project.ron");
    ProjectDocument::load_from_path(&path).map_err(|e| format!("{}: {e:?}", path.display()))
}

/// Write `project.ron`, `routes.ron` and an `assets` link into `out_dir`.
pub fn write_world(world: &StreamWorld, out_dir: &Path, donor_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(out_dir)?;
    let mut source = world
        .project
        .to_ron_string()
        .map_err(|e| std::io::Error::other(format!("{e:?}")))?;
    source.push('\n');
    std::fs::write(out_dir.join("project.ron"), source)?;
    let routes = ron::ser::to_string_pretty(&world.routes, ron::ser::PrettyConfig::default())
        .map_err(std::io::Error::other)?;
    std::fs::write(out_dir.join("routes.ron"), routes + "\n")?;
    // The cut target and the pool the world is cooked for.
    std::fs::write(
        out_dir.join("stream-cook.ron"),
        ron::ser::to_string_pretty(&world.overrides, ron::ser::PrettyConfig::default())
            .map_err(std::io::Error::other)?
            + "\n",
    )?;
    let region = world.overrides.region_target_bytes.unwrap_or(0);
    std::fs::write(
        out_dir.join("pool-plan.txt"),
        world.plan.describe(region) + "\n",
    )?;
    let link = out_dir.join("assets");
    if !link.exists() {
        let target = donor_dir.join("assets");
        #[cfg(unix)]
        std::os::unix::fs::symlink(std::fs::canonicalize(target)?, &link)?;
        #[cfg(not(unix))]
        copy_dir(&target, &link)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn projects() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../projects")
    }

    fn donor() -> ProjectDocument {
        load_donor(&projects()).expect("donor")
    }

    #[test]
    fn generation_is_deterministic() {
        let d = donor();
        let config = StreamWorldConfig::small([5, 4]);
        let a = generate(&config, &d).unwrap();
        let b = generate(&config, &d).unwrap();
        assert_eq!(
            a.project.to_ron_string().unwrap(),
            b.project.to_ron_string().unwrap()
        );
        assert_eq!(a.routes, b.routes);
        assert_eq!(a.stats, b.stats);
        // A different seed is a different world.
        let other = generate(
            &StreamWorldConfig {
                seed: config.seed + 1,
                ..config.clone()
            },
            &d,
        )
        .unwrap();
        assert_ne!(
            a.project.to_ron_string().unwrap(),
            other.project.to_ron_string().unwrap()
        );
    }

    #[test]
    fn project_round_trips_through_the_editor_model() {
        let world = generate(&StreamWorldConfig::small([4, 4]), &donor()).unwrap();
        let first = world.project.to_ron_string().unwrap();
        let reloaded = ProjectDocument::from_ron_str(&first).expect("reload");
        assert_eq!(reloaded.to_ron_string().unwrap(), first);
        assert_eq!(reloaded.active_scene().brushes.len(), world.stats.brushes);
    }

    #[test]
    fn config_round_trips_and_defaults_missing_keys() {
        let config = StreamWorldConfig::small([7, 3]);
        assert_eq!(
            StreamWorldConfig::from_ron_str(&config.to_ron_string()).unwrap(),
            config
        );
        let partial = StreamWorldConfig::from_ron_str("(seed: 5)").unwrap();
        assert_eq!(partial.seed, 5);
        assert_eq!(partial.grid, StreamWorldConfig::default().grid);
        // The tracked config parses.
        let tracked = std::fs::read_to_string(projects().join("stream-world.config.ron")).unwrap();
        let parsed = StreamWorldConfig::from_ron_str(&tracked).unwrap();
        assert!(parsed.hook_spacing >= 1 && parsed.max_hooks <= 32);
        assert_eq!(
            parsed,
            StreamWorldConfig::default(),
            "tracked config and built-in default agree"
        );
    }

    #[test]
    fn world_has_the_requested_content() {
        let d = donor();
        let config = StreamWorldConfig {
            enemy_pct: 100,
            ..StreamWorldConfig::small([6, 6])
        };
        let world = generate(&config, &d).unwrap();
        let scene = world.project.active_scene();
        assert_eq!(world.stats.modules, 36);
        assert!(scene.brushes.iter().all(|b| b.solve().is_valid()));
        // Player, both enemy archetypes, and at most 32 hooks.
        let controllers: Vec<_> = scene
            .nodes()
            .iter()
            .filter_map(|n| match &n.kind {
                NodeKind::CharacterController {
                    character, player, ..
                } => Some((*character, *player)),
                _ => None,
            })
            .collect();
        assert_eq!(controllers.iter().filter(|(_, player)| *player).count(), 1);
        let mut types: Vec<_> = controllers
            .iter()
            .filter(|(_, p)| !p)
            .map(|(c, _)| *c)
            .collect();
        types.sort_by_key(|c| c.map(|id| id.raw()));
        types.dedup();
        assert_eq!(types.len(), 2, "light and heavy enemies both appear");
        assert!(world.stats.hooks <= 32);
        // Every authored brush stays inside the brush edit extent.
        for b in &scene.brushes {
            let s = b.solve();
            assert!(s.within_extent(crate::brush::BRUSH_EDIT_EXTENT_LIMIT));
        }
    }

    #[test]
    fn routes_are_well_formed_data() {
        let world = generate(&StreamWorldConfig::small([6, 6]), &donor()).unwrap();
        let r = &world.routes;
        let kinds = |k: RouteKind| r.routes.iter().filter(|x| x.kind == k).count();
        assert!(kinds(RouteKind::Shortest) >= 1);
        assert_eq!(kinds(RouteKind::Sprint), 1);
        assert!(kinds(RouteKind::RandomWalk) >= 1);
        assert_eq!(kinds(RouteKind::Backtrack), 1);
        assert_eq!(kinds(RouteKind::HookEdge) > 0, world.stats.hooks > 0);
        let half = |g: u32| i32::try_from(g).unwrap() * r.module_units / 2;
        for route in &r.routes {
            assert!(route.points.len() >= 2, "{}", route.name);
            assert!(route.speed_pct == 100 || route.speed_pct == 125);
            for p in &route.points {
                assert!(p[0].abs() <= half(r.grid[0]) && p[2].abs() <= half(r.grid[1]));
            }
        }
        // Serialises and reads back.
        let text = ron::ser::to_string_pretty(r, ron::ser::PrettyConfig::default()).unwrap();
        assert_eq!(&ron::from_str::<RouteSet>(&text).unwrap(), r);
        assert_eq!(r.doors.len(), world.stats.edges);
    }

    #[test]
    fn door_graph_is_connected() {
        let world = generate(&StreamWorldConfig::small([7, 5]), &donor()).unwrap();
        let n = world.routes.modules.len();
        let mut parent: Vec<usize> = (0..n).collect();
        fn find(p: &mut [usize], x: usize) -> usize {
            if p[x] != x {
                let r = find(p, p[x]);
                p[x] = r;
            }
            p[x]
        }
        for [a, b] in &world.routes.doors {
            let (ra, rb) = (
                find(&mut parent, *a as usize),
                find(&mut parent, *b as usize),
            );
            parent[ra] = rb;
        }
        let root = find(&mut parent, 0);
        assert!((0..n).all(|i| find(&mut parent, i) == root));
    }

    #[test]
    fn dense_variant_differs_and_has_more_doors() {
        let d = donor();
        let base = StreamWorldConfig::small([5, 5]);
        let a = generate(&base, &d).unwrap();
        let b = generate(&base.dense_variant(), &d).unwrap();
        assert!(b.stats.edges > a.stats.edges);
        assert!(
            b.stats.max_door_degree > a.stats.max_door_degree,
            "the dense variant lifts the door cap"
        );
    }

    #[test]
    fn bad_config_is_refused() {
        let d = donor();
        let mut c = StreamWorldConfig::small([3, 3]);
        c.module_units = 770;
        assert!(generate(&c, &d).is_err());
        c.module_units = 768;
        c.grid = [0, 3];
        assert!(generate(&c, &d).is_err());
        c.grid = [40, 40];
        assert!(generate(&c, &d).unwrap_err().contains("edit extent"));
    }

    #[test]
    fn door_degree_is_capped_and_hooks_sit_in_dead_ends() {
        let d = donor();
        let config = StreamWorldConfig::small([9, 9]);
        let world = generate(&config, &d).unwrap();
        assert!(world.stats.max_door_degree <= config.max_door_degree as usize);
        assert_eq!(world.stats.degree_overflows, 0);
        let mut degree = vec![0usize; world.routes.modules.len()];
        for [a, b] in &world.routes.doors {
            degree[*a as usize] += 1;
            degree[*b as usize] += 1;
        }
        // A hook pedestal is a brush 96 units square and 112 high; its
        // module is a dead end, so the hook adds one room to the requirement.
        let scene = world.project.active_scene();
        let m = config.module_units;
        let mut hook_modules = 0;
        for node in scene.nodes() {
            if matches!(node.kind, NodeKind::HookPoint) {
                let t = node.transform.translation;
                let (x, z) = ((t[0] / A as f32) as i32, (t[2] / A as f32) as i32);
                let col = (x - world.routes.modules[0].x).div_euclid(m) as usize;
                let row = (z - world.routes.modules[0].z).div_euclid(m) as usize;
                let index = row * config.grid[0] as usize + col;
                assert_eq!(degree[index], 1, "hook in module {index}");
                hook_modules += 1;
            }
        }
        assert_eq!(hook_modules, world.stats.hooks);
        // The player starts in a dead end too: the home pin stays two regions.
        let start = world.routes.player_start;
        let col = (start[0] - world.routes.modules[0].x).div_euclid(m) as usize;
        let row = (start[2] - world.routes.modules[0].z).div_euclid(m) as usize;
        assert_eq!(degree[row * config.grid[0] as usize + col], 1);
    }

    #[test]
    fn modules_are_built_under_the_budget_and_report_when_they_cannot() {
        let d = donor();
        let roomy = generate(&StreamWorldConfig::small([6, 6]), &d).unwrap();
        assert_eq!(roomy.stats.over_budget_modules, 0);
        assert!(roomy.stats.max_module_bytes <= StreamWorldConfig::default().region_target_bytes);
        // A tighter target trims the interiors (and the terrain resolution)
        // first, and the world gets smaller, never bigger.
        let tight_config = StreamWorldConfig {
            region_target_bytes: 12_288,
            ..StreamWorldConfig::small([6, 6])
        };
        let tight = generate(&tight_config, &d).unwrap();
        assert!(tight.stats.estimated_bytes <= roomy.stats.estimated_bytes);
        assert!(tight.stats.trimmed_modules >= roomy.stats.trimmed_modules);
        // A target under the bare shell cannot be met, and says so.
        let starved = generate(
            &StreamWorldConfig {
                region_target_bytes: 2048,
                ..StreamWorldConfig::small([4, 4])
            },
            &d,
        )
        .unwrap();
        assert_eq!(starved.stats.over_budget_modules, 16);
        // Bare shells when no detail is allowed.
        let bare = generate(
            &StreamWorldConfig {
                detail_bytes: 0,
                ..StreamWorldConfig::small([6, 6])
            },
            &d,
        )
        .unwrap();
        assert!(bare.stats.brushes < roomy.stats.brushes);
    }

    #[test]
    fn terrain_modules_fit_the_default_target_by_giving_up_patches() {
        // A terrain module with several doors has a shell that leaves no room
        // for four cells, or for any. It is trimmed to fewer cells or to a
        // flat court, never left over budget (an over-budget module is cut in
        // two by the cooker, and the halves see more regions).
        let config = StreamWorldConfig {
            terrain_pct: 60,
            corridor_pct: 10,
            interior_pct: 10,
            courtyard_pct: 10,
            ..StreamWorldConfig::small([8, 8])
        };
        for seed in 1..=3 {
            let world = generate(
                &StreamWorldConfig {
                    seed,
                    ..config.clone()
                },
                &donor(),
            )
            .unwrap();
            assert!(world.stats.terrains >= 8, "seed {seed}: terrain modules");
            assert_eq!(world.stats.over_budget_modules, 0, "seed {seed}");
            assert!(world.stats.trimmed_modules > 0, "seed {seed}");
        }
    }

    #[test]
    fn pool_and_cook_parameters_come_from_the_ram_budget() {
        let config = StreamWorldConfig::default();
        let plan = config.pool_plan();
        assert_eq!(plan.pool_bytes, RAM_BUDGET.world_pool_bytes());
        assert_eq!(plan.regions, (config.grid[0] * config.grid[1]) as usize);
        let overrides = config.cook_overrides();
        assert_eq!(overrides.pool_bytes, Some(plan.pool_bytes));
        assert_eq!(
            overrides.region_target_bytes,
            Some(config.region_target_bytes)
        );
        assert_eq!(
            overrides.region_hard_cap_bytes,
            Some(2 * config.region_target_bytes)
        );
        // A scenario adds exactly what the arena hands back.
        let scenario = StreamWorldConfig {
            arena_reclaim_bytes: 100_000,
            ..config.clone()
        };
        assert_eq!(
            scenario.cook_overrides().pool_bytes,
            Some(plan.pool_bytes + 100_000)
        );
        // The world is far bigger than the pool it is cooked for: at least
        // ten times, as the design asks of the stress world.
        let world = generate(&config, &donor()).unwrap();
        assert!(
            world.stats.estimated_bytes >= 10 * u64::from(plan.pool_bytes),
            "{} B of world against a {} B pool",
            world.stats.estimated_bytes,
            plan.pool_bytes
        );
    }

    #[test]
    fn world_is_sealed() {
        // The cooker's own leak check: the player's air must not reach the
        // void, or the outside fill (and the partitioner's reachability
        // filter) would keep exterior faces.
        for grid in [[2, 2], [3, 3]] {
            let world = generate(&StreamWorldConfig::small(grid), &donor()).unwrap();
            let leak =
                crate::brush_world::diagnose_brush_world_leak(world.project.clone()).unwrap();
            assert!(leak.is_empty(), "{grid:?} leaks: {:?}", leak.path.first());
        }
    }
}
