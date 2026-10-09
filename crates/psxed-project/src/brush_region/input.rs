//! Partition input: the shared brush front end plus the entities that matter
//! for residency (spawns, hooks, the player start).

use std::collections::HashMap;
use std::path::Path;

use crate::brush::Brush;
use crate::brush_collision_hulls::CollisionHullBounds;
use crate::brush_compile::CompiledSurface;
use crate::playtest::{component_character_controller, component_children};
use crate::{
    InteractableKind, NodeKind, ProjectDocument, ResourceData, ResourceId, Scene, SceneNode,
};

use super::geometry::{Aabb, ConvexSolid, SolidIndex, V3};
use super::PartitionError;

/// One texture-bearing material used by the world.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterialInfo {
    pub id: Option<ResourceId>,
    pub name: String,
    /// Size of the cooked texture file, bytes. Zero for the built-in flat
    /// fallback.
    pub texture_bytes: u32,
    /// Sky apertures reveal the scene sky and never draw their own texture;
    /// they count toward payload but are not sight targets.
    pub sky_aperture: bool,
}

/// One enemy type: a Character resource placed as a non-player controller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchetypeInfo {
    pub id: ResourceId,
    pub name: String,
}

/// A placed entity that lives in a region payload.
#[derive(Clone, Debug, PartialEq)]
pub struct Spawn {
    pub name: String,
    pub position: V3,
    /// Index into [`PartitionInput::archetypes`]; `None` for the player and
    /// for entities with no Character.
    pub archetype: Option<usize>,
    pub is_player: bool,
}

/// Everything the partitioner reads, in engine units.
pub struct PartitionInput {
    /// Unsplit CSG surfaces (the surface BSP input).
    pub topology: Vec<CompiledSurface>,
    /// Render surfaces before any region cut.
    pub render: Vec<CompiledSurface>,
    /// Static world brushes, for collision costing.
    pub brushes: Vec<Brush>,
    /// Bounds of `render`, `topology` and `brushes`, index for index.
    pub render_bounds: Vec<Aabb>,
    pub topology_bounds: Vec<Aabb>,
    pub brush_bounds: Vec<Aabb>,
    pub hull_bounds: [CollisionHullBounds; 3],
    pub materials: Vec<MaterialInfo>,
    pub archetypes: Vec<ArchetypeInfo>,
    pub spawns: Vec<Spawn>,
    pub hooks: Vec<V3>,
    pub checkpoints: Vec<V3>,
    pub player_start: Option<V3>,
    /// Player top run speed, engine units per second.
    pub run_speed: Option<f64>,
    pub bounds: Aabb,
    pub solids: SolidIndex,
    pub(crate) material_of: HashMap<Option<ResourceId>, usize>,
    /// Surfaces the u8 texel window split during the front end.
    pub uv_split_surfaces: usize,
    /// Surfaces dropped because the player cannot reach the air they face.
    pub culled_unreachable: usize,
    /// Floor points the player can walk to, when the project has a player
    /// start; `None` treats every upward surface as standable.
    pub walk: Option<WalkIndex>,
}

impl PartitionInput {
    /// Build the input from an authored project. `project` is cloned and
    /// scaled to engine units here, so callers pass the authored document.
    pub fn from_project(
        project: &ProjectDocument,
        project_root: &Path,
    ) -> Result<Self, PartitionError> {
        let mut scaled = project.clone();
        crate::units::scale_project_to_engine_units(&mut scaled);
        let front = crate::brush_world::partition_front_end(&scaled, project_root)
            .map_err(PartitionError::FrontEnd)?;
        if front.render.is_empty() {
            return Err(PartitionError::NoGeometry);
        }

        let mut materials: Vec<MaterialInfo> = Vec::new();
        let mut material_of: HashMap<Option<ResourceId>, usize> = HashMap::new();
        let mut register = |id: Option<ResourceId>| -> usize {
            if let Some(&index) = material_of.get(&id) {
                return index;
            }
            let (name, texture_bytes, sky) = match id {
                None => ("(flat)".to_string(), 0, false),
                Some(id) => {
                    let name = scaled
                        .resource(id)
                        .map(|r| r.name.clone())
                        .unwrap_or_else(|| format!("#{}", id.raw()));
                    // Sky apertures reveal the scene sky and never draw their
                    // own texture, so they carry no texture bytes.
                    let sky_aperture = matches!(
                        scaled.resource(id).map(|r| &r.data),
                        Some(ResourceData::Material(m)) if m.sky_aperture
                    );
                    let bytes = if sky_aperture {
                        0
                    } else {
                        crate::resolve_material_texture_psxt(&scaled, id, project_root)
                            .ok()
                            .flatten()
                            .map_or(0, |(_, bytes)| bytes.len() as u32)
                    };
                    (name, bytes, sky_aperture)
                }
            };
            let index = materials.len();
            materials.push(MaterialInfo {
                id,
                name,
                texture_bytes,
                sky_aperture: sky,
            });
            material_of.insert(id, index);
            index
        };
        // Deterministic order: first use along the render surface list.
        for surface in &front.render {
            register(surface.material);
        }

        let scene = scaled.active_scene();
        let mut archetypes: Vec<ArchetypeInfo> = Vec::new();
        let mut spawns = Vec::new();
        let mut hooks = Vec::new();
        let mut checkpoints = Vec::new();
        let mut player_start = None;
        let mut run_speed = None;
        for node in scene.nodes() {
            let position = world_position(scene, node);
            match &node.kind {
                NodeKind::HookPoint => hooks.push(position),
                NodeKind::SpawnPoint { player: true, .. } => {
                    player_start.get_or_insert(position);
                }
                NodeKind::Entity => {
                    let controller = component_character_controller(scene, node);
                    let is_checkpoint = component_children(scene, node).any(|child| {
                        matches!(
                            child.kind,
                            NodeKind::Interactable {
                                kind: InteractableKind::Checkpoint { .. },
                                ..
                            }
                        )
                    });
                    if is_checkpoint {
                        checkpoints.push(position);
                    }
                    let Some(controller) = controller else {
                        continue;
                    };
                    if controller.player {
                        player_start.get_or_insert(position);
                        let speed_q8 = controller
                            .settings
                            .map(|s| s.run_speed)
                            .or_else(|| {
                                controller
                                    .character
                                    .and_then(|id| scaled.resource(id))
                                    .and_then(|r| match &r.data {
                                        ResourceData::Character(c) => Some(c.run_speed),
                                        _ => None,
                                    })
                            })
                            .filter(|&q8| q8 > 0);
                        // run_speed is Q8 engine units per 60 Hz tick.
                        run_speed = run_speed.or(speed_q8.map(|q8| f64::from(q8) / 256.0 * 60.0));
                        spawns.push(Spawn {
                            name: node.name.clone(),
                            position,
                            archetype: None,
                            is_player: true,
                        });
                    } else {
                        let archetype = controller.character.map(|id| {
                            archetypes
                                .iter()
                                .position(|a| a.id == id)
                                .unwrap_or_else(|| {
                                    let name = scaled
                                        .resource(id)
                                        .map(|r| r.name.clone())
                                        .unwrap_or_else(|| format!("#{}", id.raw()));
                                    archetypes.push(ArchetypeInfo { id, name });
                                    archetypes.len() - 1
                                })
                        });
                        spawns.push(Spawn {
                            name: node.name.clone(),
                            position,
                            archetype,
                            is_player: false,
                        });
                    }
                }
                _ => {}
            }
        }

        let solid_list: Vec<ConvexSolid> = front
            .brushes
            .iter()
            .filter(|b| b.contents.is_solid())
            .filter_map(ConvexSolid::from_brush)
            .collect();
        let solids = SolidIndex::new(solid_list, 256.0);
        let mut bounds = Aabb::EMPTY;
        for surface in &front.render {
            for v in &surface.vertices {
                bounds.grow(*v);
            }
        }
        for solid in &solids.solids {
            bounds = bounds.union(&solid.bounds);
        }

        // Quake-style outside fill: the cooker keeps only what the player can
        // reach, so surfaces facing unreachable or exterior air are dropped
        // before any accounting. Without a player start the filter is off.
        let mut topology = front.topology;
        let mut render = front.render;
        let mut culled_unreachable = 0usize;
        if let Some(start) = player_start {
            if let Some(reach) = ReachGrid::flood(&solids, bounds, start) {
                let before = render.len() + topology.len();
                render.retain(|s| reach.sees_surface(s));
                topology.retain(|s| reach.sees_surface(s));
                culled_unreachable = before - render.len() - topology.len();
            }
        }
        if render.is_empty() {
            return Err(PartitionError::NoGeometry);
        }
        let walk = player_start
            .and_then(|start| walk_flood(&render, &materials, &material_of, &solids, start));
        let surface_bounds = |list: &[CompiledSurface]| -> Vec<Aabb> {
            list.iter()
                .map(|s| Aabb::from_points(&s.vertices))
                .collect()
        };
        let render_bounds = surface_bounds(&render);
        let topology_bounds = surface_bounds(&topology);
        let brush_bounds = front
            .brushes
            .iter()
            .map(|b| {
                let solved = b.solve();
                Aabb {
                    min: solved.min,
                    max: solved.max,
                }
            })
            .collect();
        Ok(Self {
            render_bounds,
            topology_bounds,
            brush_bounds,
            topology,
            render,
            brushes: front.brushes,
            hull_bounds: front.hull_bounds,
            materials,
            archetypes,
            spawns,
            hooks,
            checkpoints,
            player_start,
            run_speed,
            bounds,
            solids,
            material_of,
            uv_split_surfaces: front.uv_window.split_surfaces,
            culled_unreachable,
            walk,
        })
    }

    pub(crate) fn material_index(&self, material: Option<ResourceId>) -> usize {
        self.material_of.get(&material).copied().unwrap_or(0)
    }
}

/// Translation accumulated up the parent chain (rotation and scale of group
/// parents are ignored; authored levels parent entities to groups at
/// identity).
fn world_position(scene: &Scene, node: &SceneNode) -> V3 {
    let mut position = node.transform.translation.map(f64::from);
    let mut parent = node.parent;
    while let Some(id) = parent {
        let Some(p) = scene.node(id) else { break };
        for (value, delta) in position.iter_mut().zip(p.transform.translation) {
            *value += f64::from(delta);
        }
        parent = p.parent;
    }
    position
}

/// Reachable-air flood over a voxel grid, from the player start.
///
/// Moving between adjacent voxel centres is blocked when the segment crosses
/// any solid, so walls thinner than a voxel still seal. Passages narrower
/// than a voxel can close and under-reach, so the step stays at or below half
/// the narrowest passage the player can use.
struct ReachGrid {
    origin: V3,
    step: f64,
    dims: [usize; 3],
    reached: Vec<u64>,
}

impl ReachGrid {
    fn flood(solids: &SolidIndex, bounds: Aabb, start: V3) -> Option<Self> {
        let pad = 2.0;
        let origin = bounds.min.map(|v| v - pad * 16.0);
        let extent = [
            bounds.extent(0) + 2.0 * pad * 16.0,
            bounds.extent(1) + 2.0 * pad * 16.0,
            bounds.extent(2) + 2.0 * pad * 16.0,
        ];
        let count = |step: f64| -> usize {
            extent
                .iter()
                .map(|e| (e / step).ceil() as usize + 1)
                .product()
        };
        let step = if count(16.0) <= 40_000_000 {
            16.0
        } else {
            32.0
        };
        let dims = [
            (extent[0] / step).ceil() as usize + 1,
            (extent[1] / step).ceil() as usize + 1,
            (extent[2] / step).ceil() as usize + 1,
        ];
        let total = dims[0] * dims[1] * dims[2];
        let mut grid = Self {
            origin,
            step,
            dims,
            reached: vec![0; total.div_ceil(64)],
        };
        let mut tested = vec![0u64; total.div_ceil(64)];
        let index = |c: [usize; 3]| (c[2] * dims[1] + c[1]) * dims[0] + c[0];
        let center = |c: [usize; 3]| -> V3 {
            [
                origin[0] + (c[0] as f64 + 0.5) * step,
                origin[1] + (c[1] as f64 + 0.5) * step,
                origin[2] + (c[2] as f64 + 0.5) * step,
            ]
        };
        let start_cell = grid.cell_of(start)?;
        // The start may sit exactly on a floor; look a few voxels up for air.
        let mut seed = None;
        for lift in 0..4 {
            let mut c = start_cell;
            c[1] += lift;
            if c[1] < dims[1] && !solids.point_in_solid(center(c)) {
                seed = Some(c);
                break;
            }
        }
        let seed = seed?;
        let mut stack = vec![seed];
        grid.reached[index(seed) / 64] |= 1 << (index(seed) % 64);
        // `tested` caches only "centre is inside a solid"; an air voxel can
        // be retried from another side until some edge into it is clear.
        while let Some(c) = stack.pop() {
            let from = center(c);
            for axis in 0..3 {
                for dir in [-1i64, 1] {
                    let v = c[axis] as i64 + dir;
                    if v < 0 || v >= dims[axis] as i64 {
                        continue;
                    }
                    let mut n = c;
                    n[axis] = v as usize;
                    let i = index(n);
                    if grid.reached[i / 64] >> (i % 64) & 1 == 1
                        || tested[i / 64] >> (i % 64) & 1 == 1
                    {
                        continue;
                    }
                    let to = center(n);
                    if solids.point_in_solid(to) {
                        tested[i / 64] |= 1 << (i % 64);
                        continue;
                    }
                    // The segment test sees walls thinner than a voxel.
                    if !solids.segment_blocked(from, to) {
                        grid.reached[i / 64] |= 1 << (i % 64);
                        stack.push(n);
                    }
                }
            }
        }
        Some(grid)
    }

    fn cell_of(&self, point: V3) -> Option<[usize; 3]> {
        let mut cell = [0usize; 3];
        for axis in 0..3 {
            let v = ((point[axis] - self.origin[axis]) / self.step).floor();
            if v < 0.0 || v >= self.dims[axis] as f64 {
                return None;
            }
            cell[axis] = v as usize;
        }
        Some(cell)
    }

    fn reached_at(&self, point: V3) -> bool {
        self.cell_of(point).is_some_and(|c| {
            let i = (c[2] * self.dims[1] + c[1]) * self.dims[0] + c[0];
            self.reached[i / 64] >> (i % 64) & 1 == 1
        })
    }

    /// Whether the air in front of the surface is reachable.
    fn sees_surface(&self, surface: &CompiledSurface) -> bool {
        let n = surface.plane.normal.map(|v| v as f64);
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2])
            .sqrt()
            .max(1.0e-12);
        let c = super::geometry::polygon_centroid(&surface.vertices);
        let reach = self.step * 0.75;
        // Two probes along the normal: a face on a voxel boundary lands the
        // first probe in the next voxel, the second in the one after it.
        (0..2).any(|k| {
            let d = reach + k as f64 * self.step * 0.5;
            self.reached_at([
                c[0] + n[0] / len * d,
                c[1] + n[1] / len * d,
                c[2] + n[2] / len * d,
            ])
        })
    }
}

/// Standable floor points reachable on foot from the player start.
///
/// Every upward-facing, non-sky render surface contributes one point just
/// above its centroid. Two points connect when they are within
/// [`WALK_LINK_XZ`] horizontally and [`WALK_LINK_Y`] vertically with no
/// solid between points lifted a body height off the floor, so a wall between
/// two floors separates them and a stair of 28-unit steps does not. [E]
#[derive(Clone, Debug)]
pub struct WalkIndex {
    pub points: Vec<V3>,
    buckets: HashMap<(i32, i32), Vec<u32>>,
}

const WALK_CELL: f64 = 128.0;
/// Largest horizontal gap between linked floor points, engine units. [E]
const WALK_LINK_XZ: f64 = 400.0;
/// Largest step or slope rise between linked floor points. [E]
const WALK_LINK_Y: f64 = 64.0;
/// Body lift for the wall test between floor points. [E]
const WALK_LIFT: f64 = 24.0;

impl WalkIndex {
    fn key(p: V3) -> (i32, i32) {
        (
            (p[0] / WALK_CELL).floor() as i32,
            (p[2] / WALK_CELL).floor() as i32,
        )
    }

    fn candidates(&self, p: V3, xz: f64) -> impl Iterator<Item = u32> + '_ {
        let (cx0, cz0) = Self::key([p[0] - xz, 0.0, p[2] - xz]);
        let (cx1, cz1) = Self::key([p[0] + xz, 0.0, p[2] + xz]);
        (cx0..=cx1).flat_map(move |cx| {
            (cz0..=cz1)
                .flat_map(move |cz| self.buckets.get(&(cx, cz)).into_iter().flatten().copied())
        })
    }

    /// Whether a walkable floor lies within `xz` horizontally and between
    /// `below` under and `above` over the point.
    pub fn near(&self, p: V3, xz: f64, below: f64, above: f64) -> bool {
        self.candidates(p, xz).any(|i| {
            let f = self.points[i as usize];
            let (dx, dz) = (f[0] - p[0], f[2] - p[2]);
            dx * dx + dz * dz <= xz * xz && f[1] >= p[1] - below && f[1] <= p[1] + above
        })
    }
}

fn walk_flood(
    render: &[CompiledSurface],
    materials: &[MaterialInfo],
    material_of: &HashMap<Option<ResourceId>, usize>,
    solids: &SolidIndex,
    start: V3,
) -> Option<WalkIndex> {
    let mut points: Vec<V3> = Vec::new();
    for surface in render {
        if materials[material_of.get(&surface.material).copied().unwrap_or(0)].sky_aperture {
            continue;
        }
        let n = surface.plane.normal.map(|v| v as f64);
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2])
            .sqrt()
            .max(1.0e-12);
        if n[1] / len < 0.5 {
            continue;
        }
        let c = super::geometry::polygon_centroid(&surface.vertices);
        points.push([c[0], c[1] + 1.0, c[2]]);
    }
    if points.is_empty() {
        return None;
    }
    // Deterministic order for the flood.
    points.sort_by(|a, b| {
        a[0].total_cmp(&b[0])
            .then(a[2].total_cmp(&b[2]))
            .then(a[1].total_cmp(&b[1]))
    });
    let mut index = WalkIndex {
        points: Vec::new(),
        buckets: HashMap::new(),
    };
    for (i, p) in points.iter().enumerate() {
        index
            .buckets
            .entry(WalkIndex::key(*p))
            .or_default()
            .push(i as u32);
    }
    index.points = points;
    let all = &index.points;
    // Seed: the floor point nearest the start.
    let seed = (0..all.len() as u32)
        .filter(|&i| {
            let f = all[i as usize];
            let (dx, dz) = (f[0] - start[0], f[2] - start[2]);
            dx * dx + dz * dz <= 160.0 * 160.0
                && f[1] >= start[1] - 160.0
                && f[1] <= start[1] + 64.0
        })
        .min_by(|&a, &b| {
            super::geometry::distance(all[a as usize], start)
                .total_cmp(&super::geometry::distance(all[b as usize], start))
                .then(a.cmp(&b))
        })?;
    let mut seen = vec![false; all.len()];
    seen[seed as usize] = true;
    let mut stack = vec![seed];
    while let Some(i) = stack.pop() {
        let a = all[i as usize];
        let lifted_a = [a[0], a[1] + WALK_LIFT, a[2]];
        for j in index.candidates(a, WALK_LINK_XZ).collect::<Vec<_>>() {
            if seen[j as usize] {
                continue;
            }
            let b = all[j as usize];
            let (dx, dz) = (b[0] - a[0], b[2] - a[2]);
            if dx * dx + dz * dz > WALK_LINK_XZ * WALK_LINK_XZ || (b[1] - a[1]).abs() > WALK_LINK_Y
            {
                continue;
            }
            if solids.segment_blocked(lifted_a, [b[0], b[1] + WALK_LIFT, b[2]]) {
                continue;
            }
            seen[j as usize] = true;
            stack.push(j);
        }
    }
    // Keep only the reached floor points.
    let kept: Vec<V3> = all
        .iter()
        .zip(&seen)
        .filter(|(_, &s)| s)
        .map(|(p, _)| *p)
        .collect();
    let mut walk = WalkIndex {
        points: Vec::new(),
        buckets: HashMap::new(),
    };
    for (i, p) in kept.iter().enumerate() {
        walk.buckets
            .entry(WalkIndex::key(*p))
            .or_default()
            .push(i as u32);
    }
    walk.points = kept;
    Some(walk)
}
