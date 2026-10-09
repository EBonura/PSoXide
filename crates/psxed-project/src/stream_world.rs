//! Deterministic stress-world generator for world streaming (design
//! 2026-10-08, milestone M8, generator half).
//!
//! A grid of 768-unit modules (rooms, serpentine corridors, partitioned interiors,
//! open courtyards, rooms with hook platforms, terrain patches) joined by a
//! randomised spanning tree plus loop edges, sealed under a sky slab, with
//! the two existing enemy archetypes placed at spawn points, a player start,
//! and a route set (shortest paths, a sprint, random walks, a backtrack
//! exercise, hook edges) written as data for later tape generation.
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
const CEIL_T: i32 = 32;
const SKY_Y: i32 = 1024;
const SKY_T: i32 = 32;
const PILLAR: i32 = 48;
const PLATFORM: i32 = 256;
const PLATFORM_H: i32 = 112;
const TERRAIN_CELLS: usize = 8;
const TERRAIN_AMPLITUDE: i32 = 96;

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
    /// `bsp_patch_extent` written into the World node, authored units.
    pub patch_extent_authored: i32,
    /// Module mix, percent. Rooms take the remainder.
    pub corridor_pct: u32,
    pub interior_pct: u32,
    pub courtyard_pct: u32,
    pub terrain_pct: u32,
    /// Extra door edges beyond the spanning tree, percent of candidates.
    pub loop_pct: u32,
    /// Chance a room, interior or courtyard holds an enemy, percent.
    pub enemy_pct: u32,
    /// Rooms that get a hook platform and Hook Point; the level cap is 32
    /// hook points. [M, docs/cortex/hook-points.md]
    pub max_hooks: u32,
    /// Minimum Chebyshev distance between hook rooms, in modules, so
    /// that no position has several hook landings in range.
    pub hook_spacing: u32,
    /// The deliberately dense variant: no interior detail, wide doors on
    /// every edge, so almost everything sees almost everything.
    pub dense: bool,
    /// Page pool the stress run is sized for, bytes. Written to
    /// `stream-cook.ron` for `stream-report --params`. The design's own
    /// P_world estimate is 213,124 B; this world is a stress test of the
    /// scheduler, so it names the larger pool it assumes (a share of the
    /// 643,072 B of session clips reclaimed by per-archetype residency). [E]
    pub cook_pool_bytes: u32,
    pub shortest_routes: u32,
    pub random_walks: u32,
    pub walk_steps: u32,
}

impl Default for StreamWorldConfig {
    /// The full stress world.
    fn default() -> Self {
        Self {
            seed: 31,
            grid: [15, 15],
            module_units: 768,
            patch_extent_authored: 2048,
            corridor_pct: 25,
            interior_pct: 20,
            courtyard_pct: 6,
            terrain_pct: 3,
            loop_pct: 3,
            enemy_pct: 35,
            max_hooks: 12,
            hook_spacing: 4,
            dense: false,
            cook_pool_bytes: 524_288,
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
}

#[derive(Debug)]
pub struct StreamWorld {
    pub cook_pool_bytes: u32,
    pub project: ProjectDocument,
    pub routes: RouteSet,
    pub stats: StreamWorldStats,
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

/// A cuboid in engine units, written in authored units.
fn cuboid(min: [i32; 3], max: [i32; 3], material: ResourceId) -> Brush {
    let mut brush = Brush::cuboid(min.map(|v| v * A), max.map(|v| v * A));
    for face in &mut brush.faces {
        face.material = Some(material);
    }
    brush
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
    // Randomised Prim over the grid for the spanning tree.
    let mut rng = Rng(config.seed ^ 0xA5A5_5A5A);
    let mut in_tree = vec![false; modules.len()];
    let mut chosen = vec![false; candidates.len()];
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
        let k = frontier[rng.below(frontier.len() as u64) as usize];
        chosen[k] = true;
        in_tree[candidates[k].0] = true;
        in_tree[candidates[k].1] = true;
    }
    for (k, take) in chosen.iter_mut().enumerate() {
        let _ = k;
        if !*take && (config.dense || rng.pct(config.loop_pct)) {
            *take = true;
        }
    }
    let wide = m - 2 * WALL - 64;
    let door_w = if config.dense { wide } else { DOOR_W };
    let mut edges = Vec::new();
    for (k, &(a, b, along_x)) in candidates.iter().enumerate() {
        if !chosen[k] {
            continue;
        }
        let mid = if along_x {
            modules[a].z + m / 2
        } else {
            modules[a].x + m / 2
        };
        let slack = (m / 2 - WALL - door_w / 2 - 32).max(0);
        let off = if config.dense || slack == 0 {
            0
        } else {
            (mix(config.seed, a as u64, b as u64) % (2 * slack as u64 + 1)) as i32 - slack
        };
        edges.push(Edge {
            a,
            b,
            along_x,
            centre: mid + off,
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
    }
}

/// One wall strip along an axis with an optional door, as brushes.
#[allow(clippy::too_many_arguments)]
fn wall(
    out: &mut Vec<Brush>,
    material: ResourceId,
    alongx: bool,
    fixed: (i32, i32),
    span: (i32, i32),
    top: i32,
    door: Option<(i32, i32)>,
) {
    let mk = |lo: i32, hi: i32, y0: i32, y1: i32| -> Brush {
        if alongx {
            cuboid([lo, y0, fixed.0], [hi, y1, fixed.1], material)
        } else {
            cuboid([fixed.0, y0, lo], [fixed.1, y1, hi], material)
        }
    };
    match door {
        None => out.push(mk(span.0, span.1, 0, top)),
        Some((d0, d1)) => {
            if d0 > span.0 {
                out.push(mk(span.0, d0, 0, top));
            }
            if span.1 > d1 {
                out.push(mk(d1, span.1, 0, top));
            }
            out.push(mk(d0, d1, DOOR_H, top));
        }
    }
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

    let mut brushes: Vec<Brush> = Vec::new();
    let mut stats = StreamWorldStats {
        modules: layout.modules.len(),
        edges: layout.edges.len(),
        ..StreamWorldStats::default()
    };
    // Doors per module side: (east, west, south, north) as optional spans.
    let door_span = |e: &Edge| {
        let w = if config.dense {
            m - 2 * WALL - 64
        } else {
            DOOR_W
        };
        (e.centre - w / 2, e.centre + w / 2)
    };
    // Hook platforms go to rooms with the lowest hash, spaced apart, up to
    // the cap.
    let hook_modules: Vec<usize> = {
        let mut courtyards: Vec<usize> = layout
            .modules
            .iter()
            .enumerate()
            .filter(|(_, mo)| mo.kind == ModuleKind::Room)
            .map(|(k, _)| k)
            .collect();
        courtyards.sort_by_key(|&k| (mix(config.seed, 9000 + k as u64, 1), k));
        let mut chosen: Vec<usize> = Vec::new();
        for k in courtyards {
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

    for (index, module) in layout.modules.iter().enumerate() {
        let (i, j) = (index % cols, index / cols);
        let (x0, z0) = (module.x, module.z);
        let (x1, z1) = (x0 + m, z0 + m);
        let h = mix(config.seed, 1000 + index as u64, 7);
        let roofed = matches!(
            module.kind,
            ModuleKind::Room | ModuleKind::Corridor | ModuleKind::Interior
        );
        // The slab runs under terrain too: the walls start at 0, so a lower
        // slab top would leave a gap at the module edge.
        brushes.push(cuboid([x0, -SLAB, z0], [x1, 0, z1], mats.floor));

        let door_for = |neighbour: Option<usize>, ex: bool, a_side: bool| -> Option<(i32, i32)> {
            let n = neighbour?;
            let (lo, hi) = if a_side { (index, n) } else { (n, index) };
            layout
                .edges
                .iter()
                .find(|e| e.a == lo && e.b == hi && e.along_x == ex)
                .map(door_span)
        };
        let east = (i + 1 < cols).then(|| index + 1);
        let west = (i > 0).then(|| index - 1);
        let south = (j + 1 < rows).then(|| index + cols);
        let north = (j > 0).then(|| index - cols);
        let outer_top = SKY_Y + SKY_T;
        // An outer wall runs the whole side, so the seam between two border
        // modules is not left open above the inner walls' height.
        let border_span = |neighbour: Option<usize>, lo: i32, hi: i32| {
            if neighbour.is_none() {
                (lo, hi)
            } else {
                (lo + WALL, hi - WALL)
            }
        };
        let top_of = |neighbour: Option<usize>| {
            if neighbour.is_none() {
                outer_top
            } else {
                WALL_H
            }
        };
        // West and east walls span the full z range; north and south fit
        // between them so the corners are not duplicated.
        wall(
            &mut brushes,
            mats.wall,
            false,
            (x0, x0 + WALL),
            (z0, z1),
            top_of(west),
            door_for(west, true, false),
        );
        wall(
            &mut brushes,
            mats.wall,
            false,
            (x1 - WALL, x1),
            (z0, z1),
            top_of(east),
            door_for(east, true, true),
        );
        wall(
            &mut brushes,
            mats.wall,
            true,
            (z0, z0 + WALL),
            border_span(north, x0, x1),
            top_of(north),
            door_for(north, false, false),
        );
        wall(
            &mut brushes,
            mats.wall,
            true,
            (z1 - WALL, z1),
            border_span(south, x0, x1),
            top_of(south),
            door_for(south, false, true),
        );
        if roofed {
            brushes.push(cuboid(
                [x0, WALL_H, z0],
                [x1, WALL_H + CEIL_T, z1],
                mats.wall,
            ));
        }

        let cx = x0 + m / 2;
        let cz = z0 + m / 2;
        let detail = !config.dense;
        match module.kind {
            ModuleKind::Room => {
                stats.rooms += 1;
                if detail {
                    for (fx, fz) in [(1, 1), (3, 1), (1, 3), (3, 3)] {
                        let px = x0 + m * fx / 4;
                        let pz = z0 + m * fz / 4;
                        brushes.push(cuboid(
                            [px - PILLAR / 2, 0, pz - PILLAR / 2],
                            [px + PILLAR / 2, WALL_H, pz + PILLAR / 2],
                            mats.wall,
                        ));
                    }
                }
            }
            ModuleKind::Corridor => {
                stats.corridors += 1;
                // A serpentine: three baffles with alternating gaps, so no
                // sightline crosses the module end to end.
                if detail {
                    let gap = DOOR_W;
                    for k in 1..4 {
                        let bx = x0 + m * k / 4;
                        if k % 2 == 1 {
                            brushes.push(cuboid(
                                [bx - 8, 0, z0 + WALL + gap],
                                [bx + 8, WALL_H, z1 - WALL],
                                mats.cliff,
                            ));
                        } else {
                            brushes.push(cuboid(
                                [bx - 8, 0, z0 + WALL],
                                [bx + 8, WALL_H, z1 - WALL - gap],
                                mats.cliff,
                            ));
                        }
                    }
                }
            }
            ModuleKind::Interior => {
                stats.interiors += 1;
                if detail {
                    let gap = 224;
                    let a = x0 + m / 3;
                    let b = x0 + 2 * m / 3;
                    brushes.push(cuboid(
                        [a - 8, 0, z0 + WALL],
                        [a + 8, WALL_H, z1 - WALL - gap],
                        mats.wall,
                    ));
                    brushes.push(cuboid(
                        [b - 8, 0, z0 + WALL + gap],
                        [b + 8, WALL_H, z1 - WALL],
                        mats.wall,
                    ));
                }
            }
            ModuleKind::Courtyard => {
                stats.courtyards += 1;
                if detail {
                    for (bx, bz) in [(-200, 180), (230, -160), (0, -40)] {
                        let (qx, qz) = (cx + bx, cz + bz);
                        brushes.push(cuboid(
                            [qx - 64, 0, qz - 64],
                            [qx + 64, 96 + (h % 3) as i32 * 32, qz + 64],
                            mats.cliff,
                        ));
                    }
                }
            }
            ModuleKind::Terrain => {
                stats.terrains += 1;
                let span = m - 2 * WALL;
                debug_assert_eq!(span % TERRAIN_CELLS as i32, 0);
                let spacing = (span / TERRAIN_CELLS as i32) * A;
                let terrain = Terrain::generate(
                    [TERRAIN_CELLS, TERRAIN_CELLS],
                    [spacing, spacing],
                    [(x0 + WALL) * A, 0, (z0 + WALL) * A],
                    TERRAIN_AMPLITUDE * A,
                    (h & 0xFFFF_FFFF) as u32,
                    TerrainShape::Hills,
                    0.5,
                )
                .map_err(|e| format!("terrain at module {index}: {e}"))?;
                brushes.extend(terrain.brushes(Some(mats.cliff))?);
            }
        }
        if detail && hook_modules.contains(&index) {
            // A raised platform with stairs and the Hook Point on top. It
            // sits in a roofed room, so the landing's own view stays small.
            let sx = (h % 3) as i32 - 1;
            let sz = ((h >> 8) % 3) as i32 - 1;
            let px = cx + sx * 64 - PLATFORM / 2;
            let pz = cz + sz * 64 - PLATFORM / 2;
            brushes.push(cuboid(
                [px, 0, pz],
                [px + PLATFORM, PLATFORM_H, pz + PLATFORM],
                mats.cliff,
            ));
            for k in 0..4 {
                let sx0 = px + PLATFORM + 48 * (3 - k);
                brushes.push(cuboid(
                    [sx0, 0, pz + 64],
                    [sx0 + 48, 28 * (k + 1), pz + PLATFORM - 64],
                    mats.cliff,
                ));
            }
            hooks.push([px + PLATFORM / 2, PLATFORM_H, pz + PLATFORM / 2]);
        }
        if !matches!(module.kind, ModuleKind::Corridor | ModuleKind::Terrain)
            && mix(config.seed, 5000 + index as u64, 3) % 100 < u64::from(config.enemy_pct)
        {
            let heavy = mix(config.seed, 6000 + index as u64, 9).is_multiple_of(3);
            let offset = if module.kind == ModuleKind::Courtyard {
                (-224, -224)
            } else {
                (80, 80)
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
    // The sky slab seals every open module.
    brushes.push(cuboid(
        [world_min[0], SKY_Y, world_min[1]],
        [world_max[0], SKY_Y + SKY_T, world_max[1]],
        mats.sky,
    ));
    stats.brushes = brushes.len();
    stats.enemies = enemies.len();
    stats.hooks = hooks.len();

    // Player start: a serpentine corridor (a quiet, low-exposure spot for the
    // home pin) else the first room, else module 0.
    let start_index = layout
        .modules
        .iter()
        .position(|mo| mo.kind == ModuleKind::Corridor)
        .or_else(|| {
            layout
                .modules
                .iter()
                .position(|mo| mo.kind == ModuleKind::Room)
        })
        .unwrap_or(0);
    let start = layout.modules[start_index];
    let player_start = [start.x + m / 2 - 160, 16, start.z + m / 2 - 160];

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
        cook_pool_bytes: config.cook_pool_bytes,
        project,
        routes,
        stats,
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

/// Door centre between two adjacent modules.
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
        points.push(module_centre(layout, config, pair[0]));
        points.push(door_point(layout, config, edge));
    }
    points.push(module_centre(layout, config, *path.last().unwrap()));
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
    if let Some(&(n, e)) = layout.adjacency[start].first() {
        let door = door_point(layout, config, e);
        let here = module_centre(layout, config, start);
        let there = module_centre(layout, config, n);
        let mut points = vec![player_start];
        for _ in 0..4 {
            points.push(door);
            points.push([(door[0] + there[0]) / 2, 0, (door[2] + there[2]) / 2]);
            points.push(door);
            points.push([(door[0] + here[0]) / 2, 0, (door[2] + here[2]) / 2]);
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
    let cook = crate::brush_region::CookOverrides {
        pool_bytes: Some(world.cook_pool_bytes),
        rho_batched: Some(true),
        ..Default::default()
    };
    std::fs::write(
        out_dir.join("stream-cook.ron"),
        ron::ser::to_string_pretty(&cook, ron::ser::PrettyConfig::default())
            .map_err(std::io::Error::other)?
            + "\n",
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
        assert!(b.stats.brushes < a.stats.brushes, "no interior detail");
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
