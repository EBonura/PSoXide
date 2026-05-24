//! Portal-room planning for the playtest cooker and editor overlays.
//!
//! The authored editor presents one contiguous [`WorldGrid`]. Runtime portal
//! rooms are split only by authored `Portal` scene nodes placed on grid edges;
//! the planner must not invent chunk boundaries for size, walls, or streaming.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};

use crate::{
    GridDirection, GridSector, GridVerticalFace, NodeId, NodeKind, Scene, SceneNode, WorldGrid,
    WorldGridBudget, MAX_ROOM_BYTES, MAX_ROOM_DEPTH, MAX_ROOM_TRIANGLES, MAX_ROOM_WIDTH,
};

/// Default hard portal-room span limit, in authored sectors.
pub const DEFAULT_PORTAL_ROOM_MAX_SECTORS: u16 = MAX_ROOM_WIDTH;

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
            max_width: MAX_ROOM_WIDTH,
            max_depth: MAX_ROOM_DEPTH,
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
    /// First directed portal record sourced from this room.
    pub portal_first: usize,
    /// Number of directed portal records sourced from this room.
    pub portal_count: usize,
    /// Geometry/byte estimate for the extracted runtime room.
    pub budget: WorldGridBudget,
    /// True if the derived room still violates a hard cap.
    pub over_budget: bool,
}

/// One directed runtime portal between two derived rooms.
///
/// `normal_world` points back toward `source_room`, away from
/// `destination_room`. Runtime backface tests can therefore accept a
/// source-side view when `dot(normal, camera - vertex0) > 0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalRoomPortal {
    /// Stable index in [`PortalRoomPlan::portals`].
    pub index: usize,
    /// Source runtime room.
    pub source_room: usize,
    /// Destination runtime room.
    pub destination_room: usize,
    /// Canonical seam edge represented by this merged portal.
    pub source_edge: PortalEdge,
    /// Direction crossed when travelling from source to destination.
    pub direction: GridDirection,
    /// World-space portal rectangle vertices `[BL, BR, TR, TL]`.
    pub vertices_world: [[i32; 3]; 4],
    /// Source-facing world-space normal.
    pub normal_world: [i16; 3],
    /// True for floor/ceiling portals. Demo7 currently emits wall portals.
    pub vertical: bool,
    /// Authored Portal marker that opened this seam, when one exists.
    pub source_marker: Option<NodeId>,
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
    /// Directed runtime portal graph records.
    pub portals: Vec<PortalRoomPortal>,
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

struct PortalSeams {
    cuts: HashSet<EdgeKey>,
    source_marker_for_edge: HashMap<EdgeKey, NodeId>,
    marker_count: usize,
}

/// Canonical grid edge selected by an authored portal marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortalEdge {
    /// Array-sector X coordinate of the canonical edge owner.
    pub x: u16,
    /// Array-sector Z coordinate of the canonical edge owner.
    pub z: u16,
    /// Canonical edge direction. Only north/east are emitted.
    pub direction: GridDirection,
}

impl Hash for EdgeKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.x.hash(state);
        self.z.hash(state);
        direction_slot(self.direction).hash(state);
    }
}

impl From<EdgeKey> for PortalEdge {
    fn from(edge: EdgeKey) -> Self {
        Self {
            x: edge.x,
            z: edge.z,
            direction: edge.direction,
        }
    }
}

/// Plan manual portal rooms from one contiguous authored world grid.
pub fn plan_portal_rooms(
    scene: &Scene,
    room_node: NodeId,
    grid: &WorldGrid,
    config: PortalRoomConfig,
) -> PortalRoomPlan {
    let config = config.normalized();
    let portal_seams = collect_portal_seams(scene, room_node, grid);
    let mut room_cells = portal_regions(grid, &portal_seams.cuts);
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
        rooms.push(PortalRoom {
            index,
            array_origin: origin,
            world_origin: [
                grid.origin[0].saturating_add(origin[0] as i32),
                grid.origin[1].saturating_add(origin[1] as i32),
            ],
            size,
            cells: cells.iter().map(|cell| [cell.x, cell.z]).collect(),
            neighbours: [None; 4],
            portal_first: 0,
            portal_count: 0,
            over_budget: config.over_budget(&budget),
            budget,
        });
    }
    let portals = build_room_portals(grid, &cell_to_room, &portal_seams);
    apply_portal_slices_and_neighbours(&mut rooms, &portals);

    PortalRoomPlan {
        config,
        source_origin: grid.origin,
        source_size: [grid.width, grid.depth],
        rooms,
        portals,
        portal_count: portal_seams.marker_count,
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

/// Snap an authored Portal node to the grid edge it opens.
pub fn portal_edge_for_node(grid: &WorldGrid, node: &SceneNode) -> Option<PortalEdge> {
    portal_edge_key_for_node(grid, node).map(Into::into)
}

/// Expand an authored Portal node to every connected edge on the seam it cuts.
pub fn portal_seam_edges_for_node(grid: &WorldGrid, node: &SceneNode) -> Vec<PortalEdge> {
    let Some(edge) = portal_edge_key_for_node(grid, node) else {
        return Vec::new();
    };
    sorted_portal_edges(expand_portal_seam(grid, edge))
}

/// Expand a canonical portal edge to every connected edge on the seam it cuts.
pub fn portal_seam_edges_for_edge(grid: &WorldGrid, edge: PortalEdge) -> Vec<PortalEdge> {
    if !matches!(edge.direction, GridDirection::North | GridDirection::East) {
        return Vec::new();
    }
    sorted_portal_edges(expand_portal_seam(
        grid,
        EdgeKey {
            x: edge.x,
            z: edge.z,
            direction: edge.direction,
        },
    ))
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

fn collect_portal_seams(scene: &Scene, room_node: NodeId, grid: &WorldGrid) -> PortalSeams {
    let mut cuts = HashSet::new();
    let mut source_marker_for_edge = HashMap::new();
    let mut marker_count = 0;
    for node in scene
        .nodes()
        .iter()
        .filter(|node| matches!(node.kind, NodeKind::Portal { .. }))
        .filter(|node| scene.is_descendant_of(node.id, room_node))
    {
        let Some(edge) = portal_edge_key_for_node(grid, node) else {
            continue;
        };
        marker_count += 1;
        for seam_edge in expand_portal_seam(grid, edge) {
            source_marker_for_edge
                .entry(seam_edge)
                .and_modify(|existing: &mut NodeId| {
                    if node.id.raw() < existing.raw() {
                        *existing = node.id;
                    }
                })
                .or_insert(node.id);
            cuts.insert(seam_edge);
        }
    }
    PortalSeams {
        cuts,
        source_marker_for_edge,
        marker_count,
    }
}

fn portal_edge_key_for_node(grid: &WorldGrid, node: &SceneNode) -> Option<EdgeKey> {
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

fn portal_regions(grid: &WorldGrid, portal_edges: &HashSet<EdgeKey>) -> Vec<Vec<Cell>> {
    let populated_cells = all_populated_cells(grid);
    if populated_cells.is_empty() {
        return Vec::new();
    }
    if portal_edges.is_empty() {
        return vec![populated_cells];
    }

    flood_regions(grid, portal_edges)
}

fn all_populated_cells(grid: &WorldGrid) -> Vec<Cell> {
    let mut cells = Vec::new();
    for x in 0..grid.width {
        for z in 0..grid.depth {
            if populated(grid, x, z) {
                cells.push(Cell { x, z });
            }
        }
    }
    cells
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
                    if portal_edges.contains(&edge) {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OpenPortalEdge {
    edge: EdgeKey,
    source_room: usize,
    destination_room: usize,
    source_marker: Option<NodeId>,
}

fn build_room_portals(
    grid: &WorldGrid,
    cell_to_room: &HashMap<Cell, usize>,
    portal_seams: &PortalSeams,
) -> Vec<PortalRoomPortal> {
    let mut edges = Vec::new();
    for cell in cell_to_room.keys() {
        for direction in [GridDirection::North, GridDirection::East] {
            let Some((nx, nz, _)) = neighbour_across(grid, cell.x, cell.z, direction) else {
                continue;
            };
            let Some(&source_room) = cell_to_room.get(cell) else {
                continue;
            };
            let Some(&destination_room) = cell_to_room.get(&Cell { x: nx, z: nz }) else {
                continue;
            };
            if source_room == destination_room {
                continue;
            }
            let Some(edge) = canonical_edge(cell.x, cell.z, direction) else {
                continue;
            };
            let Some(&source_marker) = portal_seams.source_marker_for_edge.get(&edge) else {
                continue;
            };
            if edge_has_wall(grid, edge) {
                continue;
            }
            edges.push(OpenPortalEdge {
                edge,
                source_room,
                destination_room,
                source_marker: Some(source_marker),
            });
        }
    }

    edges.sort_by_key(open_portal_edge_sort_key);

    let mut portals = Vec::new();
    let mut i = 0usize;
    while i < edges.len() {
        let first = edges[i];
        let mut span_edges = vec![first];
        i += 1;
        while i < edges.len() && portal_edges_can_merge(span_edges[span_edges.len() - 1], edges[i])
        {
            span_edges.push(edges[i]);
            i += 1;
        }
        append_directed_portal_pair(grid, &span_edges, &mut portals);
    }

    portals.sort_by_key(|portal| {
        (
            portal.source_room,
            direction_slot(portal.direction),
            portal.destination_room,
            portal.source_edge.z,
            portal.source_edge.x,
        )
    });
    for (index, portal) in portals.iter_mut().enumerate() {
        portal.index = index;
    }
    portals
}

fn portal_edges_can_merge(previous: OpenPortalEdge, next: OpenPortalEdge) -> bool {
    if previous.source_room != next.source_room
        || previous.destination_room != next.destination_room
        || previous.edge.direction != next.edge.direction
    {
        return false;
    }
    match previous.edge.direction {
        GridDirection::North => {
            previous.edge.z == next.edge.z && previous.edge.x.saturating_add(1) == next.edge.x
        }
        GridDirection::East => {
            previous.edge.x == next.edge.x && previous.edge.z.saturating_add(1) == next.edge.z
        }
        GridDirection::South
        | GridDirection::West
        | GridDirection::NorthWestSouthEast
        | GridDirection::NorthEastSouthWest => false,
    }
}

fn open_portal_edge_sort_key(open: &OpenPortalEdge) -> (usize, usize, usize, u16, u16) {
    let (line, span) = match open.edge.direction {
        GridDirection::North => (open.edge.z, open.edge.x),
        GridDirection::East => (open.edge.x, open.edge.z),
        GridDirection::South
        | GridDirection::West
        | GridDirection::NorthWestSouthEast
        | GridDirection::NorthEastSouthWest => (open.edge.z, open.edge.x),
    };
    (
        open.source_room,
        open.destination_room,
        direction_slot(open.edge.direction),
        line,
        span,
    )
}

fn append_directed_portal_pair(
    grid: &WorldGrid,
    span_edges: &[OpenPortalEdge],
    portals: &mut Vec<PortalRoomPortal>,
) {
    let Some(first) = span_edges.first().copied() else {
        return;
    };
    let Some((vertices, source_edge)) = merged_portal_vertices(grid, span_edges) else {
        return;
    };
    let source_marker = span_edges
        .iter()
        .filter_map(|edge| edge.source_marker)
        .min_by_key(|id| id.raw());
    portals.push(PortalRoomPortal {
        index: portals.len(),
        source_room: first.source_room,
        destination_room: first.destination_room,
        source_edge,
        direction: first.edge.direction,
        vertices_world: vertices,
        normal_world: portal_source_facing_normal(first.edge.direction),
        vertical: false,
        source_marker,
    });
    if let Some(reverse) = first.edge.direction.opposite_cardinal() {
        portals.push(PortalRoomPortal {
            index: portals.len(),
            source_room: first.destination_room,
            destination_room: first.source_room,
            source_edge,
            direction: reverse,
            vertices_world: vertices,
            normal_world: portal_source_facing_normal(reverse),
            vertical: false,
            source_marker,
        });
    }
}

fn merged_portal_vertices(
    grid: &WorldGrid,
    span_edges: &[OpenPortalEdge],
) -> Option<([[i32; 3]; 4], PortalEdge)> {
    let first = span_edges.first()?.edge;
    let last = span_edges.last()?.edge;
    let (bottom, top) = portal_height_bounds(grid, span_edges)?;
    let s = grid.sector_size;
    if s <= 0 || top <= bottom {
        return None;
    }
    let (a, b) = match first.direction {
        GridDirection::North => {
            let x0 = grid.origin[0]
                .saturating_add(i32::from(first.x))
                .saturating_mul(s);
            let x1 = grid.origin[0]
                .saturating_add(i32::from(last.x).saturating_add(1))
                .saturating_mul(s);
            let z = grid.origin[1]
                .saturating_add(i32::from(first.z).saturating_add(1))
                .saturating_mul(s);
            ([x0, z], [x1, z])
        }
        GridDirection::East => {
            let x = grid.origin[0]
                .saturating_add(i32::from(first.x).saturating_add(1))
                .saturating_mul(s);
            let z0 = grid.origin[1]
                .saturating_add(i32::from(first.z))
                .saturating_mul(s);
            let z1 = grid.origin[1]
                .saturating_add(i32::from(last.z).saturating_add(1))
                .saturating_mul(s);
            ([x, z1], [x, z0])
        }
        GridDirection::South
        | GridDirection::West
        | GridDirection::NorthWestSouthEast
        | GridDirection::NorthEastSouthWest => return None,
    };
    Some((
        [
            [a[0], bottom, a[1]],
            [b[0], bottom, b[1]],
            [b[0], top, b[1]],
            [a[0], top, a[1]],
        ],
        PortalEdge {
            x: first.x,
            z: first.z,
            direction: first.direction,
        },
    ))
}

fn portal_height_bounds(grid: &WorldGrid, span_edges: &[OpenPortalEdge]) -> Option<(i32, i32)> {
    let mut bottom = i32::MAX;
    let mut top = i32::MIN;
    let mut found = false;
    for open in span_edges {
        let heights =
            grid.wall_heights_aligned_to_surfaces(open.edge.x, open.edge.z, open.edge.direction);
        bottom = bottom.min(heights[0]).min(heights[1]);
        top = top.max(heights[2]).max(heights[3]);
        found = true;
    }
    found.then_some((bottom, top))
}

fn portal_source_facing_normal(direction: GridDirection) -> [i16; 3] {
    match direction {
        GridDirection::North => [0, 0, -1],
        GridDirection::East => [-1, 0, 0],
        GridDirection::South => [0, 0, 1],
        GridDirection::West => [1, 0, 0],
        GridDirection::NorthWestSouthEast | GridDirection::NorthEastSouthWest => [0, 0, 0],
    }
}

fn apply_portal_slices_and_neighbours(rooms: &mut [PortalRoom], portals: &[PortalRoomPortal]) {
    for room in rooms.iter_mut() {
        room.neighbours = [None; 4];
        room.portal_first = 0;
        room.portal_count = 0;
    }
    for (index, portal) in portals.iter().enumerate() {
        let Some(room) = rooms.get_mut(portal.source_room) else {
            continue;
        };
        if room.portal_count == 0 {
            room.portal_first = index;
        }
        room.portal_count = room.portal_count.saturating_add(1);
        if portal.direction.is_cardinal() {
            room.neighbours[direction_slot(portal.direction)] = Some(portal.destination_room);
        }
    }
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

fn expand_portal_seam(grid: &WorldGrid, edge: EdgeKey) -> HashSet<EdgeKey> {
    let mut out = HashSet::new();
    if !edge_between_populated(grid, edge) {
        return out;
    }
    out.insert(edge);
    match edge.direction {
        GridDirection::North => {
            expand_portal_seam_axis(grid, edge, -1, 0, &mut out);
            expand_portal_seam_axis(grid, edge, 1, 0, &mut out);
        }
        GridDirection::East => {
            expand_portal_seam_axis(grid, edge, 0, -1, &mut out);
            expand_portal_seam_axis(grid, edge, 0, 1, &mut out);
        }
        GridDirection::South
        | GridDirection::West
        | GridDirection::NorthWestSouthEast
        | GridDirection::NorthEastSouthWest => {}
    }
    out
}

fn expand_portal_seam_axis(
    grid: &WorldGrid,
    start: EdgeKey,
    step_x: i32,
    step_z: i32,
    out: &mut HashSet<EdgeKey>,
) {
    let mut x = start.x as i32 + step_x;
    let mut z = start.z as i32 + step_z;
    while x >= 0 && z >= 0 && x < grid.width as i32 && z < grid.depth as i32 {
        let edge = EdgeKey {
            x: x as u16,
            z: z as u16,
            direction: start.direction,
        };
        if !edge_between_populated(grid, edge) {
            break;
        }
        out.insert(edge);
        x += step_x;
        z += step_z;
    }
}

fn edge_between_populated(grid: &WorldGrid, edge: EdgeKey) -> bool {
    let Some((nx, nz, _)) = neighbour_across(grid, edge.x, edge.z, edge.direction) else {
        return false;
    };
    populated(grid, edge.x, edge.z) && populated(grid, nx, nz)
}

fn sorted_portal_edges(edges: HashSet<EdgeKey>) -> Vec<PortalEdge> {
    let mut out: Vec<PortalEdge> = edges.into_iter().map(Into::into).collect();
    out.sort_by_key(|edge| (edge.z, edge.x, direction_slot(edge.direction)));
    out
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
    fn walls_do_not_split_manual_portal_rooms() {
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
        assert_eq!(plan.room_count(), 1);
        assert_eq!(plan.rooms[0].cells.len(), 2);
        assert!(plan.portals.is_empty());
        assert_eq!(plan.rooms[0].neighbours, [None; 4]);
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
                geometry: None,
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
        assert_eq!(plan.portals.len(), 2);
        assert_eq!(plan.rooms[0].portal_first, 0);
        assert_eq!(plan.rooms[0].portal_count, 1);
        assert_eq!(plan.rooms[1].portal_first, 1);
        assert_eq!(plan.rooms[1].portal_count, 1);
        assert_eq!(plan.rooms[0].neighbours[1], Some(1));
        assert_eq!(plan.rooms[1].neighbours[3], Some(0));
        let east = &plan.portals[plan.rooms[0].portal_first];
        assert_eq!(east.source_room, 0);
        assert_eq!(east.destination_room, 1);
        assert_eq!(east.direction, GridDirection::East);
        assert_eq!(east.normal_world, [-1, 0, 0]);
        assert_eq!(east.source_marker, Some(portal));
        assert_eq!(
            east.vertices_world,
            [
                [1024, 0, 1024],
                [1024, 0, 0],
                [1024, 2048, 0],
                [1024, 2048, 1024],
            ]
        );
        let west = &plan.portals[plan.rooms[1].portal_first];
        assert_eq!(west.source_room, 1);
        assert_eq!(west.destination_room, 0);
        assert_eq!(west.direction, GridDirection::West);
        assert_eq!(west.normal_world, [1, 0, 0]);
    }

    #[test]
    fn portal_marker_cuts_full_connected_seam_with_one_opening() {
        let mut project = ProjectDocument::new("test");
        let mat = material(&mut project);
        let grid = WorldGrid::stone_room(3, 2, 1024, Some(mat), None);
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
                geometry: None,
            },
        );
        let NodeKind::Room { grid } = &project.active_scene().node(room).unwrap().kind else {
            panic!("expected room");
        };
        let editor =
            grid.world_cells_to_editor([grid.origin[0] as f32 + 1.5, grid.origin[1] as f32 + 1.0]);
        project
            .active_scene_mut()
            .node_mut(portal)
            .unwrap()
            .transform
            .translation = [editor[0], 0.0, editor[1]];
        let NodeKind::Room { grid } = &project.active_scene().node(room).unwrap().kind else {
            panic!("expected room");
        };

        let seam = portal_seam_edges_for_node(grid, project.active_scene().node(portal).unwrap());
        assert_eq!(
            seam,
            vec![
                PortalEdge {
                    x: 0,
                    z: 0,
                    direction: GridDirection::North,
                },
                PortalEdge {
                    x: 1,
                    z: 0,
                    direction: GridDirection::North,
                },
                PortalEdge {
                    x: 2,
                    z: 0,
                    direction: GridDirection::North,
                },
            ]
        );

        let plan = plan_portal_rooms(
            project.active_scene(),
            room,
            grid,
            PortalRoomConfig::default(),
        );
        assert_eq!(plan.portal_count, 1);
        assert_eq!(plan.room_count(), 2);
        assert_eq!(plan.portals.len(), 2);
        let south = plan
            .rooms
            .iter()
            .find(|room| room.array_origin == [0, 0])
            .expect("south room");
        let north = plan
            .rooms
            .iter()
            .find(|room| room.array_origin == [0, 1])
            .expect("north room");
        assert_eq!(south.cells.len(), 3);
        assert_eq!(north.cells.len(), 3);
        assert_eq!(south.neighbours[0], Some(north.index));
        assert_eq!(north.neighbours[2], Some(south.index));
        let northbound = plan
            .portals
            .iter()
            .find(|portal| {
                portal.source_room == south.index && portal.destination_room == north.index
            })
            .expect("south-to-north portal");
        assert_eq!(northbound.direction, GridDirection::North);
        assert_eq!(northbound.normal_world, [0, 0, -1]);
        assert_eq!(
            northbound.vertices_world,
            [
                [0, 0, 1024],
                [3072, 0, 1024],
                [3072, 2048, 1024],
                [0, 2048, 1024],
            ]
        );
        let southbound = plan
            .portals
            .iter()
            .find(|portal| {
                portal.source_room == north.index && portal.destination_room == south.index
            })
            .expect("north-to-south portal");
        assert_eq!(southbound.direction, GridDirection::South);
        assert_eq!(southbound.normal_world, [0, 0, 1]);
    }

    #[test]
    fn oversized_open_region_stays_one_manual_portal_room() {
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
        assert_eq!(plan.room_count(), 1);
        assert_eq!(plan.rooms[0].size, [1, 8]);
        assert!(plan.portals.is_empty());
    }

    #[test]
    fn manual_portal_split_keeps_canonical_floor_transition_wall() {
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
        let portal = project.active_scene_mut().add_node(
            room,
            "Portal",
            NodeKind::Portal {
                target_room: None,
                target_entry: String::new(),
                entry_name: String::new(),
                geometry: None,
            },
        );
        let NodeKind::Room { grid } = &project.active_scene().node(room).unwrap().kind else {
            panic!("expected room");
        };
        let editor =
            grid.world_cells_to_editor([grid.origin[0] as f32 + 3.0, grid.origin[1] as f32 + 0.5]);
        project
            .active_scene_mut()
            .node_mut(portal)
            .unwrap()
            .transform
            .translation = [editor[0], 0.0, editor[1]];
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
            .expect("west room");
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
