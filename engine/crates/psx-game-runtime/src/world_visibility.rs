//! Active-room draw ordering and grid-world topology, carved out of
//! `editor-playtest`'s `visibility_runtime` module (phase 2 of
//! docs/game-runtime-plan.md): which active room draws first, which
//! room contains a global point (cell-accurate for L-shaped manual
//! portal rooms), portal room-bounds collection, and the local/global
//! room-space mappings. Cooked tables arrive bundled as `&'static`
//! psx-level records in [`WorldTables`]; capacities arrive as `const N`
//! generic parameters.

use psx_engine::{RoomPoint, RuntimeRoom, WorldCamera, WorldVertex};
use psx_level::portal_visibility::{PortalRoomBounds, PortalVisibilityResult};
use psx_level::{
    visibility_cell_flags, LevelChunkRecord, LevelRoomRecord, LevelRoomVisibilityRecord,
    LevelVisibilityCellRecord, RoomIndex,
};

use crate::room_cache::{
    parse_runtime_room, room_origin_x, room_origin_y, room_origin_z, ActiveRuntimeRoom,
    INVALID_ROOM_INDEX,
};
use crate::room_visibility::ActiveRoomView;

/// The cooked world-topology tables the grid queries walk, bundled so
/// call sites thread one value instead of five slices.
#[derive(Copy, Clone)]
pub struct WorldTables {
    /// Master cooked asset table (room payload parses resolve here).
    pub assets: &'static [psx_level::LevelAssetRecord],
    /// Cooked room records.
    pub rooms: &'static [LevelRoomRecord],
    /// Cooked room-chunk records (streamed grid chunks + neighbours).
    pub room_chunks: &'static [LevelChunkRecord],
    /// Per-room visibility-cell directory.
    pub room_visibility: &'static [LevelRoomVisibilityRecord],
    /// Flat visibility-cell pool the directory indexes.
    pub visibility_cells: &'static [LevelVisibilityCellRecord],
}

/// Slot sentinel for "no active room" in a draw-order array.
pub const INVALID_ACTIVE_ROOM_SLOT: u8 = u8::MAX;

/// Draw-order policy for the active-room window.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CachedRoomDrawOrderMode {
    /// Far-to-near by room-centre camera depth (painter's default).
    Distance,
    /// Portal-traversal order, then remaining drawable slots.
    Portal,
    /// Window slot order.
    Slot,
}

/// Order the active-room window's drawable slots for rendering under
/// `mode`; unused entries are [`INVALID_ACTIVE_ROOM_SLOT`].
#[inline]
pub fn active_room_draw_order<
    const MAX_ACTIVE_ROOMS: usize,
    const MAX_PORTAL_FRUSTUMS: usize,
    const MAX_PORTAL_FRONTIER_ROOMS: usize,
>(
    active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    camera: WorldCamera,
    visibility: &PortalVisibilityResult<
        MAX_ACTIVE_ROOMS,
        MAX_PORTAL_FRUSTUMS,
        MAX_PORTAL_FRONTIER_ROOMS,
    >,
    current_room: RoomIndex,
    mode: CachedRoomDrawOrderMode,
) -> [u8; MAX_ACTIVE_ROOMS] {
    match mode {
        CachedRoomDrawOrderMode::Distance => {
            active_room_draw_order_by_distance(active_rooms, camera, visibility, current_room)
        }
        CachedRoomDrawOrderMode::Portal => {
            active_room_draw_order_by_portal(active_rooms, visibility, current_room)
        }
        CachedRoomDrawOrderMode::Slot => {
            active_room_draw_order_by_slot(active_rooms, visibility, current_room)
        }
    }
}

fn active_room_draw_order_by_distance<
    const MAX_ACTIVE_ROOMS: usize,
    const MAX_PORTAL_FRUSTUMS: usize,
    const MAX_PORTAL_FRONTIER_ROOMS: usize,
>(
    active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    camera: WorldCamera,
    visibility: &PortalVisibilityResult<
        MAX_ACTIVE_ROOMS,
        MAX_PORTAL_FRUSTUMS,
        MAX_PORTAL_FRONTIER_ROOMS,
    >,
    current_room: RoomIndex,
) -> [u8; MAX_ACTIVE_ROOMS] {
    let mut order = [INVALID_ACTIVE_ROOM_SLOT; MAX_ACTIVE_ROOMS];
    let mut depths = [i32::MIN; MAX_ACTIVE_ROOMS];
    let mut count = 0usize;
    let mut slot = 0usize;
    while slot < MAX_ACTIVE_ROOMS {
        if let Some(active) = active_rooms[slot] {
            if !portal_visibility_result_draws_room(visibility, current_room, active.index) {
                slot += 1;
                continue;
            }
            let depth = active_room_sort_depth(active, camera);
            let mut insert = count;
            while insert > 0 && depth > depths[insert - 1] {
                depths[insert] = depths[insert - 1];
                order[insert] = order[insert - 1];
                insert -= 1;
            }
            depths[insert] = depth;
            order[insert] = slot as u8;
            count += 1;
        }
        slot += 1;
    }
    order
}

fn active_room_draw_order_by_portal<
    const MAX_ACTIVE_ROOMS: usize,
    const MAX_PORTAL_FRUSTUMS: usize,
    const MAX_PORTAL_FRONTIER_ROOMS: usize,
>(
    active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    visibility: &PortalVisibilityResult<
        MAX_ACTIVE_ROOMS,
        MAX_PORTAL_FRUSTUMS,
        MAX_PORTAL_FRONTIER_ROOMS,
    >,
    current_room: RoomIndex,
) -> [u8; MAX_ACTIVE_ROOMS] {
    let mut order = [INVALID_ACTIVE_ROOM_SLOT; MAX_ACTIVE_ROOMS];
    let mut count = 0usize;
    let mut visible_index = 0usize;
    while visible_index < visibility.room_count.min(MAX_ACTIVE_ROOMS) && count < MAX_ACTIVE_ROOMS {
        let room = visibility.rooms[visible_index].room;
        if portal_visibility_result_draws_room(visibility, current_room, room) {
            if let Some(slot) = active_room_slot_for_room(active_rooms, room) {
                order[count] = slot;
                count += 1;
            }
        }
        visible_index += 1;
    }
    if count == 0 {
        if let Some(slot) = active_room_slot_for_room(active_rooms, current_room) {
            order[count] = slot;
            count += 1;
        }
    }
    let mut slot = 0usize;
    while slot < MAX_ACTIVE_ROOMS && count < MAX_ACTIVE_ROOMS {
        if let Some(active) = active_rooms[slot] {
            if portal_visibility_result_draws_room(visibility, current_room, active.index)
                && !active_draw_order_contains(&order, count, slot as u8)
            {
                order[count] = slot as u8;
                count += 1;
            }
        }
        slot += 1;
    }
    order
}

fn active_room_draw_order_by_slot<
    const MAX_ACTIVE_ROOMS: usize,
    const MAX_PORTAL_FRUSTUMS: usize,
    const MAX_PORTAL_FRONTIER_ROOMS: usize,
>(
    active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    visibility: &PortalVisibilityResult<
        MAX_ACTIVE_ROOMS,
        MAX_PORTAL_FRUSTUMS,
        MAX_PORTAL_FRONTIER_ROOMS,
    >,
    current_room: RoomIndex,
) -> [u8; MAX_ACTIVE_ROOMS] {
    let mut order = [INVALID_ACTIVE_ROOM_SLOT; MAX_ACTIVE_ROOMS];
    let mut count = 0usize;
    let mut slot = 0usize;
    while slot < MAX_ACTIVE_ROOMS {
        if let Some(active) = active_rooms[slot] {
            if portal_visibility_result_draws_room(visibility, current_room, active.index) {
                order[count] = slot as u8;
                count += 1;
            }
        }
        slot += 1;
    }
    order
}

fn active_room_slot_for_room<const MAX_ACTIVE_ROOMS: usize>(
    active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    room: RoomIndex,
) -> Option<u8> {
    let mut slot = 0usize;
    while slot < MAX_ACTIVE_ROOMS {
        if active_rooms[slot].is_some_and(|active| active.index == room) {
            return Some(slot as u8);
        }
        slot += 1;
    }
    None
}

fn active_draw_order_contains<const MAX_ACTIVE_ROOMS: usize>(
    order: &[u8; MAX_ACTIVE_ROOMS],
    count: usize,
    slot: u8,
) -> bool {
    let mut i = 0usize;
    while i < count.min(MAX_ACTIVE_ROOMS) {
        if order[i] == slot {
            return true;
        }
        i += 1;
    }
    false
}

fn portal_visibility_result_draws_room<
    const MAX_ACTIVE_ROOMS: usize,
    const MAX_PORTAL_FRUSTUMS: usize,
    const MAX_PORTAL_FRONTIER_ROOMS: usize,
>(
    visibility: &PortalVisibilityResult<
        MAX_ACTIVE_ROOMS,
        MAX_PORTAL_FRUSTUMS,
        MAX_PORTAL_FRONTIER_ROOMS,
    >,
    current_room: RoomIndex,
    index: RoomIndex,
) -> bool {
    // Keep streamed residency broad but make the draw list match the latest
    // portal traversal. The current room remains a conservative fail-safe if a
    // visibility refresh has not populated its result yet.
    index == current_room || visibility.contains_drawable_room(index)
}

fn active_room_sort_depth(active: ActiveRuntimeRoom, camera: WorldCamera) -> i32 {
    let sector_size = active.sector_size.max(1);
    let center_x = active
        .offset_x
        .saturating_add((active.width as i32).saturating_mul(sector_size) >> 1);
    let center_z = active
        .offset_z
        .saturating_add((active.depth as i32).saturating_mul(sector_size) >> 1);
    camera
        .view_vertex(WorldVertex::new(center_x, 0, center_z))
        .z
}

/// The room + view a portal traversal should root at for `view`: the
/// camera can sit in a different room than the player, so the
/// traversal reroots at the room containing the camera's global
/// position, with the view re-expressed in that room's local frame.
#[derive(Copy, Clone)]
pub struct PortalVisibilitySpace {
    /// Room containing the camera (the traversal root).
    pub room: RoomIndex,
    /// View re-expressed in `room`'s local frame.
    pub view: ActiveRoomView,
    /// Camera position in global level space.
    pub camera_global: RoomPoint,
}

/// Map a current-room-local view to the visibility root space (see
/// [`PortalVisibilitySpace`]).
pub fn portal_visibility_space_for_view<const MAX_PORTAL_ROOM_BOUNDS: usize>(
    tables: WorldTables,
    current_index: RoomIndex,
    view: ActiveRoomView,
) -> PortalVisibilitySpace {
    let camera_global = local_to_global_room_point(tables.rooms, current_index, view.position);
    let visibility_index = room_index_containing_global_from::<MAX_PORTAL_ROOM_BOUNDS>(
        tables,
        current_index,
        camera_global,
    )
    .unwrap_or(current_index);
    let visibility_view = if visibility_index == current_index {
        view
    } else {
        ActiveRoomView {
            position: global_to_local_room_point(tables.rooms, visibility_index, camera_global),
            ..view
        }
    };
    PortalVisibilitySpace {
        room: visibility_index,
        view: visibility_view,
        camera_global,
    }
}

/// Quantised yaw/pitch keys for view-change detection.
pub fn portal_visibility_view_keys(view: ActiveRoomView) -> (i16, i16, i16, i16) {
    (
        (view.sin_yaw / 64) as i16,
        (view.cos_yaw / 64) as i16,
        (view.sin_pitch / 64) as i16,
        (view.cos_pitch / 64) as i16,
    )
}

/// Authored (editor) room id owning chunk `index`, if any.
pub fn authored_room_for_chunk(
    room_chunks: &'static [LevelChunkRecord],
    index: RoomIndex,
) -> Option<u32> {
    chunk_record_for_room(room_chunks, index).map(|chunk| chunk.authored_room)
}

/// Chunk record for room `index` (fast path: same-index slot).
pub fn chunk_record_for_room(
    room_chunks: &'static [LevelChunkRecord],
    index: RoomIndex,
) -> Option<&'static LevelChunkRecord> {
    if let Some(chunk) = room_chunks.get(index.to_usize()) {
        if chunk.room == index {
            return Some(chunk);
        }
    }
    room_chunks.iter().find(|chunk| chunk.room == index)
}

/// Whether `chunk` overlaps the collision window around `anchor`
/// (current-room-local), padded by `margin`.
pub fn chunk_overlaps_collision_window(
    chunk: LevelChunkRecord,
    current_record: &LevelRoomRecord,
    chunk_record: &LevelRoomRecord,
    anchor: RoomPoint,
    margin: i32,
) -> bool {
    let sector_size = chunk_record.sector_size.max(1);
    let x0 = room_origin_x(chunk_record).saturating_sub(room_origin_x(current_record));
    let z0 = room_origin_z(chunk_record).saturating_sub(room_origin_z(current_record));
    let x1 = x0.saturating_add((chunk.width as i32).saturating_mul(sector_size));
    let z1 = z0.saturating_add((chunk.depth as i32).saturating_mul(sector_size));
    let margin = margin.max(0);
    anchor.x.saturating_add(margin) >= x0
        && anchor.x.saturating_sub(margin) < x1
        && anchor.z.saturating_add(margin) >= z0
        && anchor.z.saturating_sub(margin) < z1
}

fn axis_moved_at_least(a: i32, b: i32, threshold: i32) -> bool {
    let threshold = threshold.max(0);
    if a >= b {
        a.saturating_sub(b) >= threshold
    } else {
        b.saturating_sub(a) >= threshold
    }
}

/// Whether `a` and `b` differ by at least `threshold` on X or Z.
pub fn point_xz_axis_moved_at_least(a: RoomPoint, b: RoomPoint, threshold: i32) -> bool {
    axis_moved_at_least(a.x, b.x, threshold) || axis_moved_at_least(a.z, b.z, threshold)
}

/// Whether `a` and `b` differ by at least `threshold` on any axis.
pub fn point_xyz_axis_moved_at_least(a: RoomPoint, b: RoomPoint, threshold: i32) -> bool {
    axis_moved_at_least(a.x, b.x, threshold)
        || axis_moved_at_least(a.y, b.y, threshold)
        || axis_moved_at_least(a.z, b.z, threshold)
}

fn room_bounds(record: &LevelRoomRecord, room: RuntimeRoom<'_>) -> (i32, i32, i32, i32) {
    let x0 = room_origin_x(record);
    let z0 = room_origin_z(record);
    let x1 = x0.saturating_add((room.width() as i32).saturating_mul(record.sector_size));
    let z1 = z0.saturating_add((room.depth() as i32).saturating_mul(record.sector_size));
    (x0, x1, z0, z1)
}

/// Collect the static portal room bounds (per geometry-bearing cell,
/// falling back to chunk then parsed-room bounds), clamped to the
/// caller's vertical band. Returns the filled count.
pub fn collect_portal_room_bounds<const MAX_PORTAL_ROOM_BOUNDS: usize>(
    tables: WorldTables,
    bounds_min_y: i32,
    bounds_max_y: i32,
    out: &mut [PortalRoomBounds; MAX_PORTAL_ROOM_BOUNDS],
) -> usize {
    let mut count = 0usize;
    for visibility in tables.room_visibility {
        let Some(record) = tables.rooms.get(visibility.room.to_usize()) else {
            continue;
        };
        let first = visibility.cell_first.to_usize();
        let end = first.saturating_add(visibility.cell_count as usize);
        let Some(cells) = tables.visibility_cells.get(first..end) else {
            continue;
        };
        let sector_size = record.sector_size.max(1);
        let room_x0 = room_origin_x(record);
        let room_z0 = room_origin_z(record);
        for cell in cells {
            if cell.flags & visibility_cell_flags::HAS_GEOMETRY == 0 {
                continue;
            }
            let x0 = room_x0.saturating_add((cell.x as i32).saturating_mul(sector_size));
            let z0 = room_z0.saturating_add((cell.z as i32).saturating_mul(sector_size));
            count = push_portal_room_bounds(
                out,
                count,
                visibility.room,
                x0,
                x0.saturating_add(sector_size),
                z0,
                z0.saturating_add(sector_size),
                bounds_min_y,
                bounds_max_y,
            );
        }
    }
    if count > 0 {
        return count;
    }

    if !tables.room_chunks.is_empty() {
        for chunk in tables.room_chunks {
            let Some(record) = tables.rooms.get(chunk.room.to_usize()) else {
                continue;
            };
            let (x0, x1, z0, z1) = chunk_global_bounds(*chunk, record);
            count = push_portal_room_bounds(
                out,
                count,
                chunk.room,
                x0,
                x1,
                z0,
                z1,
                bounds_min_y,
                bounds_max_y,
            );
        }
        return count;
    }

    for (raw_index, record) in tables.rooms.iter().enumerate() {
        if raw_index >= u16::MAX as usize {
            break;
        }
        let Some(room) = parse_runtime_room(tables.assets, record) else {
            continue;
        };
        let (x0, x1, z0, z1) = room_bounds(record, room);
        count = push_portal_room_bounds(
            out,
            count,
            RoomIndex::new(raw_index as u16),
            x0,
            x1,
            z0,
            z1,
            bounds_min_y,
            bounds_max_y,
        );
    }
    count
}

fn push_portal_room_bounds<const MAX_PORTAL_ROOM_BOUNDS: usize>(
    out: &mut [PortalRoomBounds; MAX_PORTAL_ROOM_BOUNDS],
    count: usize,
    room: RoomIndex,
    min_x: i32,
    max_x: i32,
    min_z: i32,
    max_z: i32,
    bounds_min_y: i32,
    bounds_max_y: i32,
) -> usize {
    if count >= out.len() || min_x >= max_x || min_z >= max_z {
        return count;
    }
    out[count] = PortalRoomBounds {
        room,
        min_x,
        max_x,
        min_y: bounds_min_y,
        max_y: bounds_max_y,
        min_z,
        max_z,
    };
    count + 1
}

/// Whether `index` is already in the first `count` collected rooms.
pub fn collision_room_collected<const MAX_COLLISION_ROOMS: usize>(
    collected_rooms: &[RoomIndex; MAX_COLLISION_ROOMS],
    count: usize,
    index: RoomIndex,
) -> bool {
    let mut i = 0usize;
    while i < count.min(collected_rooms.len()) {
        if collected_rooms[i] == index {
            return true;
        }
        i += 1;
    }
    false
}

fn room_index_containing_global(tables: WorldTables, point: RoomPoint) -> Option<RoomIndex> {
    if !tables.room_chunks.is_empty() {
        for chunk in tables.room_chunks {
            let Some(record) = tables.rooms.get(chunk.room.to_usize()) else {
                continue;
            };
            if chunk_contains_global_point(tables, *chunk, record, point) {
                return Some(chunk.room);
            }
        }
        return None;
    }
    for (raw_index, record) in tables.rooms.iter().enumerate() {
        let Some(room) = parse_runtime_room(tables.assets, record) else {
            continue;
        };
        let (x0, x1, z0, z1) = room_bounds(record, room);
        if point.x >= x0 && point.x < x1 && point.z >= z0 && point.z < z1 {
            return Some(RoomIndex::new(raw_index as u16));
        }
    }
    None
}

/// Room containing global `point`, searched outward from `current`
/// (neighbour BFS first, then the authored-room siblings, then the
/// whole level for non-chunked worlds).
pub fn room_index_containing_global_from<const MAX_PORTAL_ROOM_BOUNDS: usize>(
    tables: WorldTables,
    current: RoomIndex,
    point: RoomPoint,
) -> Option<RoomIndex> {
    if !tables.room_chunks.is_empty() {
        let current_authored = authored_room_for_chunk(tables.room_chunks, current);
        return room_index_containing_global_by_neighbours::<MAX_PORTAL_ROOM_BOUNDS>(
            tables, current, point,
        )
        .or_else(|| {
            room_index_containing_global_in_authored(tables, point, current_authored).or_else(
                || {
                    if current_authored.is_none() {
                        room_index_containing_global(tables, point)
                    } else {
                        None
                    }
                },
            )
        });
    }
    room_index_containing_global(tables, point)
}

fn room_index_containing_global_by_neighbours<const MAX_PORTAL_ROOM_BOUNDS: usize>(
    tables: WorldTables,
    current: RoomIndex,
    point: RoomPoint,
) -> Option<RoomIndex> {
    let current_authored = authored_room_for_chunk(tables.room_chunks, current);
    // Manual portal rooms can be L-shaped; topology comes from cells, not bboxes.
    let mut queue = [INVALID_ROOM_INDEX; MAX_PORTAL_ROOM_BOUNDS];
    let mut visited = [INVALID_ROOM_INDEX; MAX_PORTAL_ROOM_BOUNDS];
    let mut head = 0usize;
    let mut tail = 0usize;
    let mut visited_count = 0usize;
    push_room_search(
        current,
        &mut queue,
        &mut tail,
        &mut visited,
        &mut visited_count,
    );

    while head < tail {
        let index = queue[head];
        head += 1;
        if current_authored.is_some()
            && authored_room_for_chunk(tables.room_chunks, index) != current_authored
        {
            continue;
        }
        let Some(chunk) = chunk_record_for_room(tables.room_chunks, index) else {
            continue;
        };
        let Some(record) = tables.rooms.get(index.to_usize()) else {
            continue;
        };
        if chunk_contains_global_point(tables, *chunk, record, point) {
            return Some(index);
        }
        for neighbour in chunk_neighbours(*chunk) {
            push_room_search(
                neighbour,
                &mut queue,
                &mut tail,
                &mut visited,
                &mut visited_count,
            );
        }
    }
    None
}

fn room_index_containing_global_in_authored(
    tables: WorldTables,
    point: RoomPoint,
    authored_room: Option<u32>,
) -> Option<RoomIndex> {
    for chunk in tables.room_chunks {
        if authored_room.is_some() && Some(chunk.authored_room) != authored_room {
            continue;
        }
        let Some(record) = tables.rooms.get(chunk.room.to_usize()) else {
            continue;
        };
        if chunk_contains_global_point(tables, *chunk, record, point) {
            return Some(chunk.room);
        }
    }
    None
}

fn push_room_search<const MAX_PORTAL_ROOM_BOUNDS: usize>(
    room: RoomIndex,
    queue: &mut [RoomIndex; MAX_PORTAL_ROOM_BOUNDS],
    tail: &mut usize,
    visited: &mut [RoomIndex; MAX_PORTAL_ROOM_BOUNDS],
    visited_count: &mut usize,
) {
    if room == INVALID_ROOM_INDEX || *tail >= queue.len() || *visited_count >= visited.len() {
        return;
    }
    let mut i = 0usize;
    while i < *visited_count {
        if visited[i] == room {
            return;
        }
        i += 1;
    }
    visited[*visited_count] = room;
    *visited_count += 1;
    queue[*tail] = room;
    *tail += 1;
}

fn chunk_neighbours(chunk: LevelChunkRecord) -> [RoomIndex; 4] {
    [
        chunk.neighbours.north,
        chunk.neighbours.east,
        chunk.neighbours.south,
        chunk.neighbours.west,
    ]
}

fn chunk_contains_global_point(
    tables: WorldTables,
    chunk: LevelChunkRecord,
    record: &LevelRoomRecord,
    point: RoomPoint,
) -> bool {
    if chunk.room.to_usize() >= tables.rooms.len() {
        return false;
    }
    match room_visibility_contains_global_point(tables, chunk.room, record, point) {
        Some(contains) => contains,
        None => chunk_bounds_contains_global_point(chunk, record, point),
    }
}

fn chunk_bounds_contains_global_point(
    chunk: LevelChunkRecord,
    record: &LevelRoomRecord,
    point: RoomPoint,
) -> bool {
    let (x0, x1, z0, z1) = chunk_global_bounds(chunk, record);
    point.x >= x0 && point.x < x1 && point.z >= z0 && point.z < z1
}

fn room_visibility_contains_global_point(
    tables: WorldTables,
    room: RoomIndex,
    record: &LevelRoomRecord,
    point: RoomPoint,
) -> Option<bool> {
    let sector_size = record.sector_size.max(1);
    let x0 = room_origin_x(record);
    let z0 = room_origin_z(record);
    let local_x = point.x.checked_sub(x0)?;
    let local_z = point.z.checked_sub(z0)?;
    if local_x < 0 || local_z < 0 {
        return Some(false);
    }
    let sx_raw = local_x / sector_size;
    let sz_raw = local_z / sector_size;
    if sx_raw > u16::MAX as i32 || sz_raw > u16::MAX as i32 {
        return Some(false);
    }
    let sx = sx_raw as u16;
    let sz = sz_raw as u16;
    room_visibility_contains_cell(tables, room, sx, sz)
}

fn room_visibility_contains_cell(
    tables: WorldTables,
    room: RoomIndex,
    sx: u16,
    sz: u16,
) -> Option<bool> {
    let visibility = tables
        .room_visibility
        .iter()
        .find(|visibility| visibility.room == room)?;
    let first = visibility.cell_first.to_usize();
    let count = visibility.cell_count as usize;
    let cells = tables
        .visibility_cells
        .get(first..first.checked_add(count)?)?;
    let mut i = 0usize;
    while i < cells.len() {
        let cell = cells[i];
        if cell.room == room && cell.x == sx && cell.z == sz {
            return Some(cell.flags & visibility_cell_flags::HAS_GEOMETRY != 0);
        }
        i += 1;
    }
    Some(false)
}

fn chunk_global_bounds(chunk: LevelChunkRecord, record: &LevelRoomRecord) -> (i32, i32, i32, i32) {
    let sector_size = record.sector_size.max(1);
    let x0 = room_origin_x(record);
    let z0 = room_origin_z(record);
    let x1 = x0.saturating_add((chunk.width as i32).saturating_mul(sector_size));
    let z1 = z0.saturating_add((chunk.depth as i32).saturating_mul(sector_size));
    (x0, x1, z0, z1)
}

/// Lift a `room`-local point into global level space.
pub fn local_to_global_room_point(
    rooms: &'static [LevelRoomRecord],
    room: RoomIndex,
    point: RoomPoint,
) -> RoomPoint {
    let Some(record) = rooms.get(room.to_usize()) else {
        return point;
    };
    RoomPoint::new(
        point.x.saturating_add(room_origin_x(record)),
        point.y.saturating_add(room_origin_y(record)),
        point.z.saturating_add(room_origin_z(record)),
    )
}

/// Re-express a global point in `room`'s local frame.
pub fn global_to_local_room_point(
    rooms: &'static [LevelRoomRecord],
    room: RoomIndex,
    point: RoomPoint,
) -> RoomPoint {
    let Some(record) = rooms.get(room.to_usize()) else {
        return point;
    };
    RoomPoint::new(
        point.x.saturating_sub(room_origin_x(record)),
        point.y.saturating_sub(room_origin_y(record)),
        point.z.saturating_sub(room_origin_z(record)),
    )
}

/// The camera re-expressed in `active`'s room-local frame.
pub fn camera_for_room(camera: WorldCamera, active: ActiveRuntimeRoom) -> WorldCamera {
    WorldCamera::from_basis(
        camera.projection,
        WorldVertex::new(
            camera.position.x.saturating_sub(active.offset_x),
            camera.position.y.saturating_sub(active.offset_y),
            camera.position.z.saturating_sub(active.offset_z),
        ),
        camera.sin_yaw,
        camera.cos_yaw,
        camera.sin_pitch,
        camera.cos_pitch,
    )
}

/// Whether the active room overlaps the collision window around
/// `anchor` (current-room-local), padded by `margin`.
pub fn active_room_overlaps_collision_window(
    active: ActiveRuntimeRoom,
    anchor: RoomPoint,
    margin: i32,
) -> bool {
    let sector_size = active.sector_size.max(1);
    let x0 = active.offset_x;
    let z0 = active.offset_z;
    let x1 = x0.saturating_add((active.width as i32).saturating_mul(sector_size));
    let z1 = z0.saturating_add((active.depth as i32).saturating_mul(sector_size));
    let margin = margin.max(0);
    anchor.x.saturating_add(margin) >= x0
        && anchor.x.saturating_sub(margin) < x1
        && anchor.z.saturating_add(margin) >= z0
        && anchor.z.saturating_sub(margin) < z1
}
