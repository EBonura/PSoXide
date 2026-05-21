//! Portal-room planning for the playtest cooker and editor overlays.
//!
//! The authored editor still presents one contiguous [`WorldGrid`], but the
//! runtime wants small room payloads with explicit connectivity. Walls are
//! sealed seams. `Portal` scene nodes placed on grid edges create open seams
//! that keep the rooms separate while allowing visibility/residency traversal.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};

use crate::{
    GridDirection, GridSector, GridVerticalFace, NodeId, NodeKind, Scene, SceneNode, WorldGrid,
    WorldGridBudget, MAX_ROOM_BYTES, MAX_ROOM_DEPTH, MAX_ROOM_TRIANGLES, MAX_ROOM_WIDTH,
};

/// Default maximum runtime portal-room span, in authored sectors.
pub const DEFAULT_PORTAL_ROOM_MAX_SECTORS: u16 = 5;

/// Portal-room planning limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortalRoomConfig {
    /// Maximum runtime room width in sectors.
    pub max_width: u16,
    /// Maximum runtime room depth in sectors.
    pub max_depth: u16,
    /// Absolute maximum triangle estimate accepted by the current cooker.
    pub max_triangles: usize,
    /// Absolute maximum static-lit `.psxw` room asset size accepted by Embedded Play.
    pub max_bytes: usize,
}

impl Default for PortalRoomConfig {
    fn default() -> Self {
        Self {
            max_width: DEFAULT_PORTAL_ROOM_MAX_SECTORS.min(MAX_ROOM_WIDTH),
            max_depth: DEFAULT_PORTAL_ROOM_MAX_SECTORS.min(MAX_ROOM_DEPTH),
            max_triangles: MAX_ROOM_TRIANGLES,
            max_bytes: MAX_ROOM_BYTES,
        }
    }
}

impl PortalRoomConfig {
    fn normalized(self) -> Self {
        Self {
            max_width: self.max_width.clamp(1, MAX_ROOM_WIDTH),
            max_depth: self.max_depth.clamp(1, MAX_ROOM_DEPTH),
            max_triangles: self.max_triangles.max(1),
            max_bytes: self.max_bytes.max(1),
        }
    }

    fn over_budget(self, budget: &WorldGridBudget) -> bool {
        budget.width > self.max_width
            || budget.depth > self.max_depth
            || budget.triangles > self.max_triangles
            || budget.psxw_static_lit_bytes > self.max_bytes
    }
}

/// One runtime room derived from an authored grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalRoom {
    /// Stable order inside the authored grid.
    pub index: usize,
    /// Top-left array-sector coordinate of the room bounding rectangle.
    pub array_origin: [u16; 2],
    /// Top-left world-cell coordinate of the room bounding rectangle.
    pub world_origin: [i32; 2],
    /// Bounding rectangle size in sectors.
    pub size: [u16; 2],
    /// Authored cells that belong to this runtime room.
    pub cells: Vec<[u16; 2]>,
    /// Cardinal open-seam neighbours `[north, east, south, west]`.
    pub neighbours: [Option<usize>; 4],
    /// Geometry/byte estimate for the extracted runtime room.
    pub budget: WorldGridBudget,
    /// True if the derived room still violates a hard cap.
    pub over_budget: bool,
}

/// Deterministic portal-room plan for one authored grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalRoomPlan {
    /// Config used to create the plan.
    pub config: PortalRoomConfig,
    /// Authored grid origin in world-cell coordinates.
    pub source_origin: [i32; 2],
    /// Authored grid size in sectors.
    pub source_size: [u16; 2],
    /// Runtime rooms in deterministic array order.
    pub rooms: Vec<PortalRoom>,
    /// Number of authored portal markers that snapped to a valid grid edge.
    pub portal_count: usize,
}

impl PortalRoomPlan {
    /// Number of derived runtime rooms.
    pub fn room_count(&self) -> usize {
        self.rooms.len()
    }

    /// Number of derived rooms still over hard limits.
    pub fn over_budget_count(&self) -> usize {
        self.rooms.iter().filter(|room| room.over_budget).count()
    }

    /// Room with the largest static-lit `.psxw` estimate.
    pub fn largest_room_asset(&self) -> Option<&PortalRoom> {
        self.rooms
            .iter()
            .max_by_key(|room| room.budget.psxw_static_lit_bytes)
    }

    /// Room with the largest base geometry estimate.
    pub fn largest_geometry(&self) -> Option<&PortalRoom> {
        self.rooms.iter().max_by_key(|room| room.budget.psxw_bytes)
    }

    /// Room with the largest triangle estimate.
    pub fn largest_triangle_room(&self) -> Option<&PortalRoom> {
        self.rooms.iter().max_by_key(|room| room.budget.triangles)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Cell {
    x: u16,
    z: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EdgeKey {
    x: u16,
    z: u16,
    direction: GridDirection,
}

impl Hash for EdgeKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.x.hash(state);
        self.z.hash(state);
        direction_slot(self.direction).hash(state);
    }
}

/// Plan small runtime rooms from one contiguous authored world grid.
pub fn plan_portal_rooms(
    scene: &Scene,
    room_node: NodeId,
    grid: &WorldGrid,
    config: PortalRoomConfig,
) -> PortalRoomPlan {
    let config = config.normalized();
    let portal_edges = collect_portal_edges(scene, room_node, grid);
    let mut raw_regions = flood_regions(grid, &portal_edges);
    let mut room_cells = Vec::new();
    for region in raw_regions.drain(..) {
        split_region_to_budget(region, config, &mut room_cells);
    }
    room_cells.sort_by_key(|cells| region_sort_key(cells));

    let mut cell_to_room = HashMap::new();
    for (index, cells) in room_cells.iter().enumerate() {
        for cell in cells {
            cell_to_room.insert(*cell, index);
        }
    }

    let mut rooms = Vec::with_capacity(room_cells.len());
    for (index, cells) in room_cells.into_iter().enumerate() {
        let (origin, size) = cells_bounds(&cells).unwrap_or(([0, 0], [0, 0]));
        let extracted = extract_portal_room_grid_from_cells(grid, origin, &cells);
        let budget = extracted.budget();
        let neighbours = room_neighbours(index, &cells, grid, &cell_to_room);
        rooms.push(PortalRoom {
            index,
            array_origin: origin,
            world_origin: [
                grid.origin[0].saturating_add(origin[0] as i32),
                grid.origin[1].saturating_add(origin[1] as i32),
            ],
            size,
            cells: cells.iter().map(|cell| [cell.x, cell.z]).collect(),
            neighbours,
            over_budget: config.over_budget(&budget),
            budget,
        });
    }

    PortalRoomPlan {
        config,
        source_origin: grid.origin,
        source_size: [grid.width, grid.depth],
        rooms,
        portal_count: portal_edges.len(),
    }
}

/// Extract one planned runtime room into a standalone [`WorldGrid`].
pub fn extract_portal_room_grid(grid: &WorldGrid, room: &PortalRoom) -> WorldGrid {
    let cells: Vec<Cell> = room
        .cells
        .iter()
        .map(|cell| Cell {
            x: cell[0],
            z: cell[1],
        })
        .collect();
    extract_portal_room_grid_from_cells(grid, room.array_origin, &cells)
}

fn extract_portal_room_grid_from_cells(
    grid: &WorldGrid,
    origin: [u16; 2],
    cells: &[Cell],
) -> WorldGrid {
    let (_, size) = cells_bounds(cells).unwrap_or((origin, [0, 0]));
    let mut out = WorldGrid::empty(size[0], size[1], grid.sector_size);
    out.origin = [
        grid.origin[0].saturating_add(origin[0] as i32),
        grid.origin[1].saturating_add(origin[1] as i32),
    ];
    out.ambient_color = grid.ambient_color;
    out.fog_enabled = grid.fog_enabled;
    out.fog_color = grid.fog_color;
    out.fog_near = grid.fog_near;
    out.fog_far = grid.fog_far;
    out.atmosphere_enabled = grid.atmosphere_enabled;
    out.atmosphere_color = grid.atmosphere_color;
    out.atmosphere_density = grid.atmosphere_density;
    out.atmosphere_fall_speed_q4 = grid.atmosphere_fall_speed_q4;
    out.atmosphere_wind_speed_q4 = grid.atmosphere_wind_speed_q4;

    let included: HashSet<Cell> = cells.iter().copied().collect();
    for cell in cells {
        let Some(src) = grid.sector_index(cell.x, cell.z) else {
            continue;
        };
        let lx = cell.x.saturating_sub(origin[0]);
        let lz = cell.z.saturating_sub(origin[1]);
        let Some(dst) = out.sector_index(lx, lz) else {
            continue;
        };
        out.sectors[dst] = grid.sectors[src].clone();
    }

    mirror_external_seam_walls(grid, &mut out, origin, &included);
    out
}

fn collect_portal_edges(scene: &Scene, room_node: NodeId, grid: &WorldGrid) -> HashSet<EdgeKey> {
    scene
        .nodes()
        .iter()
        .filter(|node| matches!(node.kind, NodeKind::Portal { .. }))
        .filter(|node| scene.is_descendant_of(node.id, room_node))
        .filter_map(|node| portal_edge_for_node(grid, node))
        .collect()
}

fn portal_edge_for_node(grid: &WorldGrid, node: &SceneNode) -> Option<EdgeKey> {
    let world =
        grid.editor_to_world_cells([node.transform.translation[0], node.transform.translation[2]]);
    let ax = world[0] - grid.origin[0] as f32;
    let az = world[1] - grid.origin[1] as f32;
    let sx = ax.floor().clamp(0.0, (grid.width.saturating_sub(1)) as f32) as u16;
    let sz = az.floor().clamp(0.0, (grid.depth.saturating_sub(1)) as f32) as u16;
    let local_x = (ax - sx as f32).clamp(0.0, 1.0);
    let local_z = (az - sz as f32).clamp(0.0, 1.0);

    let candidates = [
        (local_z, sx, sz, GridDirection::South),
        (1.0 - local_x, sx, sz, GridDirection::East),
        (1.0 - local_z, sx, sz, GridDirection::North),
        (local_x, sx, sz, GridDirection::West),
    ];
    candidates
        .into_iter()
        .filter_map(|(distance, x, z, direction)| {
            let (nx, nz, _) = neighbour_across(grid, x, z, direction)?;
            if populated(grid, x, z) && populated(grid, nx, nz) {
                canonical_edge(x, z, direction).map(|edge| (distance, edge))
            } else {
                None
            }
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, edge)| edge)
}

fn flood_regions(grid: &WorldGrid, portal_edges: &HashSet<EdgeKey>) -> Vec<Vec<Cell>> {
    let mut visited = HashSet::new();
    let mut regions = Vec::new();
    for x in 0..grid.width {
        for z in 0..grid.depth {
            let start = Cell { x, z };
            if visited.contains(&start) || !populated(grid, x, z) {
                continue;
            }
            let mut region = Vec::new();
            let mut queue = VecDeque::new();
            visited.insert(start);
            queue.push_back(start);
            while let Some(cell) = queue.pop_front() {
                region.push(cell);
                for direction in GridDirection::CARDINAL {
                    let Some((nx, nz, _)) = neighbour_across(grid, cell.x, cell.z, direction)
                    else {
                        continue;
                    };
                    let next = Cell { x: nx, z: nz };
                    if visited.contains(&next) || !populated(grid, nx, nz) {
                        continue;
                    }
                    let Some(edge) = canonical_edge(cell.x, cell.z, direction) else {
                        continue;
                    };
                    if portal_edges.contains(&edge) || edge_has_wall(grid, edge) {
                        continue;
                    }
                    visited.insert(next);
                    queue.push_back(next);
                }
            }
            regions.push(region);
        }
    }
    regions
}

fn split_region_to_budget(cells: Vec<Cell>, config: PortalRoomConfig, out: &mut Vec<Vec<Cell>>) {
    let Some((origin, size)) = cells_bounds(&cells) else {
        return;
    };
    if size[0] <= config.max_width && size[1] <= config.max_depth {
        out.push(cells);
        return;
    }

    let split_x = size[0] >= size[1] && size[0] > config.max_width;
    let split_z = !split_x && size[1] > config.max_depth;
    if !split_x && !split_z {
        out.push(cells);
        return;
    }

    let cut = if split_x {
        origin[0].saturating_add(size[0] / 2)
    } else {
        origin[1].saturating_add(size[1] / 2)
    };
    let mut low = Vec::new();
    let mut high = Vec::new();
    for cell in cells {
        if (split_x && cell.x < cut) || (split_z && cell.z < cut) {
            low.push(cell);
        } else {
            high.push(cell);
        }
    }

    if low.is_empty() || high.is_empty() {
        let mut sorted = if low.is_empty() { high } else { low };
        sorted.sort_by_key(|cell| (cell.x, cell.z));
        for chunk in sorted.chunks(config.max_width as usize * config.max_depth as usize) {
            out.push(chunk.to_vec());
        }
        return;
    }

    split_region_to_budget(low, config, out);
    split_region_to_budget(high, config, out);
}

fn room_neighbours(
    room_index: usize,
    cells: &[Cell],
    grid: &WorldGrid,
    cell_to_room: &HashMap<Cell, usize>,
) -> [Option<usize>; 4] {
    let mut neighbours = [None; 4];
    let mut scores = [0u16; 4];
    for cell in cells {
        for direction in GridDirection::CARDINAL {
            let Some((nx, nz, _)) = neighbour_across(grid, cell.x, cell.z, direction) else {
                continue;
            };
            let Some(&other) = cell_to_room.get(&Cell { x: nx, z: nz }) else {
                continue;
            };
            if other == room_index {
                continue;
            }
            let Some(edge) = canonical_edge(cell.x, cell.z, direction) else {
                continue;
            };
            if edge_has_wall(grid, edge) {
                continue;
            }
            let slot = direction_slot(direction);
            let score = scores[slot].saturating_add(1);
            if score > scores[slot] {
                scores[slot] = score;
                neighbours[slot] = Some(other);
            }
        }
    }
    neighbours
}

fn mirror_external_seam_walls(
    source: &WorldGrid,
    out: &mut WorldGrid,
    origin: [u16; 2],
    included: &HashSet<Cell>,
) {
    for cell in included {
        for direction in GridDirection::CARDINAL {
            let Some((nx, nz, opposite)) = neighbour_across(source, cell.x, cell.z, direction)
            else {
                continue;
            };
            if included.contains(&Cell { x: nx, z: nz }) {
                continue;
            }
            let current_has_wall = source
                .sector(cell.x, cell.z)
                .is_some_and(|sector| !sector.walls.get(direction).is_empty());
            if current_has_wall {
                continue;
            }
            let lx = cell.x.saturating_sub(origin[0]);
            let lz = cell.z.saturating_sub(origin[1]);
            let mirror = source
                .sector(nx, nz)
                .map(|sector| sector.walls.get(opposite))
                .filter(|walls| !walls.is_empty());
            if let Some(mirror) = mirror {
                if let Some(sector) = out.ensure_sector(lx, lz) {
                    sector
                        .walls
                        .get_mut(direction)
                        .extend(mirror.iter().map(mirrored_wall));
                }
            } else if direction == GridDirection::North || direction == GridDirection::East {
                let Some(wall) = source.floor_transition_wall_for_edge(cell.x, cell.z, direction)
                else {
                    continue;
                };
                let Some(sector) = out.ensure_sector(lx, lz) else {
                    continue;
                };
                sector.walls.get_mut(direction).push(wall);
            }
        }
    }
}

fn mirrored_wall(wall: &GridVerticalFace) -> GridVerticalFace {
    let mut out = wall.clone();
    out.heights = [
        wall.heights[1],
        wall.heights[0],
        wall.heights[3],
        wall.heights[2],
    ];
    out
}

fn populated(grid: &WorldGrid, x: u16, z: u16) -> bool {
    grid.sector(x, z).is_some_and(GridSector::has_geometry)
}

fn edge_has_wall(grid: &WorldGrid, edge: EdgeKey) -> bool {
    let Some((nx, nz, opposite)) = neighbour_across(grid, edge.x, edge.z, edge.direction) else {
        return true;
    };
    grid.sector(edge.x, edge.z)
        .is_some_and(|sector| !sector.walls.get(edge.direction).is_empty())
        || grid
            .sector(nx, nz)
            .is_some_and(|sector| !sector.walls.get(opposite).is_empty())
}

fn canonical_edge(x: u16, z: u16, direction: GridDirection) -> Option<EdgeKey> {
    match direction {
        GridDirection::North => Some(EdgeKey {
            x,
            z,
            direction: GridDirection::North,
        }),
        GridDirection::East => Some(EdgeKey {
            x,
            z,
            direction: GridDirection::East,
        }),
        GridDirection::South => Some(EdgeKey {
            x,
            z: z.checked_sub(1)?,
            direction: GridDirection::North,
        }),
        GridDirection::West => Some(EdgeKey {
            x: x.checked_sub(1)?,
            z,
            direction: GridDirection::East,
        }),
        GridDirection::NorthWestSouthEast | GridDirection::NorthEastSouthWest => None,
    }
}

fn neighbour_across(
    grid: &WorldGrid,
    x: u16,
    z: u16,
    direction: GridDirection,
) -> Option<(u16, u16, GridDirection)> {
    let opposite = direction.opposite_cardinal()?;
    let (nx, nz) = match direction {
        GridDirection::North => (x, z.checked_add(1)?),
        GridDirection::East => (x.checked_add(1)?, z),
        GridDirection::South => (x, z.checked_sub(1)?),
        GridDirection::West => (x.checked_sub(1)?, z),
        GridDirection::NorthWestSouthEast | GridDirection::NorthEastSouthWest => return None,
    };
    (nx < grid.width && nz < grid.depth).then_some((nx, nz, opposite))
}

fn direction_slot(direction: GridDirection) -> usize {
    match direction {
        GridDirection::North => 0,
        GridDirection::East => 1,
        GridDirection::South => 2,
        GridDirection::West => 3,
        GridDirection::NorthWestSouthEast | GridDirection::NorthEastSouthWest => 0,
    }
}

fn cells_bounds(cells: &[Cell]) -> Option<([u16; 2], [u16; 2])> {
    let first = *cells.first()?;
    let mut min_x = first.x;
    let mut max_x = first.x;
    let mut min_z = first.z;
    let mut max_z = first.z;
    for cell in cells {
        min_x = min_x.min(cell.x);
        max_x = max_x.max(cell.x);
        min_z = min_z.min(cell.z);
        max_z = max_z.max(cell.z);
    }
    Some((
        [min_x, min_z],
        [
            max_x.saturating_sub(min_x).saturating_add(1),
            max_z.saturating_sub(min_z).saturating_add(1),
        ],
    ))
}

fn region_sort_key(cells: &[Cell]) -> (u16, u16, u16, u16) {
    let (origin, size) = cells_bounds(cells).unwrap_or(([0, 0], [0, 0]));
    (origin[0], origin[1], size[0], size[1])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GridVerticalFace, MaterialResource, ProjectDocument, PsxBlendMode, ResourceData};

    fn material(project: &mut ProjectDocument) -> crate::ResourceId {
        let texture = project.add_resource(
            "Texture",
            ResourceData::Texture {
                psxt_path: "texture.psxt".to_string(),
            },
        );
        project.add_resource(
            "Material",
            ResourceData::Material(MaterialResource {
                texture: Some(texture),
                blend_mode: PsxBlendMode::Opaque,
                tint: [128, 128, 128],
                face_sidedness: crate::MaterialFaceSidedness::Both,
                double_sided: true,
            }),
        )
    }

    #[test]
    fn wall_splits_portal_rooms() {
        let mut project = ProjectDocument::new("test");
        let mat = material(&mut project);
        let mut grid = WorldGrid::stone_room(2, 1, 1024, Some(mat), None);
        grid.ensure_sector(0, 0)
            .unwrap()
            .walls
            .east
            .push(GridVerticalFace::flat(0, 1024, Some(mat)));
        let room = project.active_scene_mut().add_node(
            crate::NodeId::ROOT,
            "Room",
            NodeKind::Room { grid },
        );
        let NodeKind::Room { grid } = &project.active_scene().node(room).unwrap().kind else {
            panic!("expected room");
        };
        let plan = plan_portal_rooms(
            project.active_scene(),
            room,
            grid,
            PortalRoomConfig::default(),
        );
        assert_eq!(plan.room_count(), 2);
        assert_eq!(plan.rooms[0].neighbours, [None; 4]);
        assert_eq!(plan.rooms[1].neighbours, [None; 4]);
    }

    #[test]
    fn portal_marker_splits_open_edge_and_links_rooms() {
        let mut project = ProjectDocument::new("test");
        let mat = material(&mut project);
        let grid = WorldGrid::stone_room(2, 1, 1024, Some(mat), None);
        let room = project.active_scene_mut().add_node(
            crate::NodeId::ROOT,
            "Room",
            NodeKind::Room { grid },
        );
        let portal = project.active_scene_mut().add_node(
            room,
            "Portal",
            NodeKind::Portal {
                target_room: None,
                target_entry: String::new(),
                entry_name: String::new(),
            },
        );
        project
            .active_scene_mut()
            .node_mut(portal)
            .unwrap()
            .transform
            .translation = [0.0, 0.0, 0.0];
        let NodeKind::Room { grid } = &project.active_scene().node(room).unwrap().kind else {
            panic!("expected room");
        };
        let plan = plan_portal_rooms(
            project.active_scene(),
            room,
            grid,
            PortalRoomConfig::default(),
        );
        assert_eq!(plan.room_count(), 2);
        assert_eq!(plan.portal_count, 1);
        assert_eq!(plan.rooms[0].neighbours[1], Some(1));
        assert_eq!(plan.rooms[1].neighbours[3], Some(0));
    }

    #[test]
    fn oversized_open_region_gets_safety_split() {
        let mut project = ProjectDocument::new("test");
        let mat = material(&mut project);
        let grid = WorldGrid::stone_room(1, 8, 1024, Some(mat), None);
        let room = project.active_scene_mut().add_node(
            crate::NodeId::ROOT,
            "Room",
            NodeKind::Room { grid },
        );
        let NodeKind::Room { grid } = &project.active_scene().node(room).unwrap().kind else {
            panic!("expected room");
        };
        let plan = plan_portal_rooms(
            project.active_scene(),
            room,
            grid,
            PortalRoomConfig::default(),
        );
        assert_eq!(plan.room_count(), 2);
        assert!(plan.rooms.iter().all(|room| room.size[1] <= 5));
        assert!(plan.rooms.iter().any(|room| room.neighbours[0].is_some()));
    }

    #[test]
    fn safety_split_keeps_canonical_floor_transition_wall() {
        let mut project = ProjectDocument::new("test");
        let mat = material(&mut project);
        let mut grid = WorldGrid::empty(6, 1, 1024);
        for x in 0..grid.width {
            let height = if x < 3 { 0 } else { 512 };
            grid.set_floor(x, 0, height, Some(mat));
        }
        let room = project.active_scene_mut().add_node(
            crate::NodeId::ROOT,
            "Room",
            NodeKind::Room { grid },
        );
        let NodeKind::Room { grid } = &project.active_scene().node(room).unwrap().kind else {
            panic!("expected room");
        };
        let plan = plan_portal_rooms(
            project.active_scene(),
            room,
            grid,
            PortalRoomConfig::default(),
        );
        assert_eq!(plan.room_count(), 2);
        let west = plan
            .rooms
            .iter()
            .find(|room| room.array_origin == [0, 0])
            .expect("west roomlet");
        let extracted = extract_portal_room_grid(grid, west);
        assert_eq!(
            extracted
                .sector(2, 0)
                .expect("canonical sector")
                .walls
                .east
                .len(),
            1
        );
    }
}
