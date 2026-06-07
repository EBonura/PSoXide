use super::*;

pub(super) const INVALID_ACTIVE_ROOM_SLOT: u8 = u8::MAX;

pub(super) fn active_room_draw_order(
    active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    camera: WorldCamera,
    visibility: &RuntimePortalVisibility,
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

fn active_room_draw_order_by_distance(
    active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    camera: WorldCamera,
    visibility: &RuntimePortalVisibility,
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

fn active_room_draw_order_by_portal(
    active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    visibility: &RuntimePortalVisibility,
    current_room: RoomIndex,
) -> [u8; MAX_ACTIVE_ROOMS] {
    let mut order = [INVALID_ACTIVE_ROOM_SLOT; MAX_ACTIVE_ROOMS];
    let mut count = 0usize;
    let mut visible_index = 0usize;
    while visible_index < visibility.room_count.min(MAX_ACTIVE_ROOMS) && count < MAX_ACTIVE_ROOMS {
        let room = visibility.rooms[visible_index].room;
        if let Some(slot) = active_room_slot_for_room(active_rooms, room) {
            order[count] = slot;
            count += 1;
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

fn active_room_draw_order_by_slot(
    active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    visibility: &RuntimePortalVisibility,
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

fn active_room_slot_for_room(
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

fn active_draw_order_contains(order: &[u8; MAX_ACTIVE_ROOMS], count: usize, slot: u8) -> bool {
    let mut i = 0usize;
    while i < count.min(MAX_ACTIVE_ROOMS) {
        if order[i] == slot {
            return true;
        }
        i += 1;
    }
    false
}

fn portal_visibility_result_draws_room(
    _visibility: &RuntimePortalVisibility,
    _current_room: RoomIndex,
    _index: RoomIndex,
) -> bool {
    // Reachability draw: the draw-order builders only pass rooms from the active
    // window (the camera ring), so every one is drawn -- no frustum/far-distance
    // cull gates room drawing here. Per-cell frustum + per-polygon backface and
    // screen culling still trim the off-screen geometry. This is the draw-order
    // twin of `portal_visibility_draws_room`; the reachability-draw rework
    // flipped that one but missed this, so a reachable-but-not-frustum-visible
    // room (e.g. the room behind the player) was dropped from the draw order.
    true
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

pub(super) fn room_origin_x(record: &LevelRoomRecord) -> i32 {
    record.origin_x.saturating_mul(record.sector_size)
}

pub(super) fn room_origin_z(record: &LevelRoomRecord) -> i32 {
    record.origin_z.saturating_mul(record.sector_size)
}

/// Vertical origin of a room in engine units. Unlike X/Z (`origin_*` in
/// sectors), `origin_y` is already stored in engine units, so it is used
/// directly. Drives Y rebasing across room transitions for stacked floors.
pub(super) fn room_origin_y(record: &LevelRoomRecord) -> i32 {
    record.origin_y
}

#[derive(Copy, Clone)]
pub(super) struct ActiveRoomView {
    pub(super) position: RoomPoint,
    pub(super) sin_yaw: i32,
    pub(super) cos_yaw: i32,
    pub(super) sin_pitch: i32,
    pub(super) cos_pitch: i32,
}

impl ActiveRoomView {
    pub(super) fn from_camera(camera: WorldCamera) -> Self {
        Self {
            position: RoomPoint::new(camera.position.x, camera.position.y, camera.position.z),
            sin_yaw: camera.sin_yaw.raw(),
            cos_yaw: camera.cos_yaw.raw(),
            sin_pitch: camera.sin_pitch.raw(),
            cos_pitch: camera.cos_pitch.raw(),
        }
    }
}

#[derive(Copy, Clone)]
pub(super) struct PortalVisibilitySpace {
    pub(super) room: RoomIndex,
    pub(super) view: ActiveRoomView,
    pub(super) camera_global: RoomPoint,
}

pub(super) fn portal_visibility_space_for_view(
    current_index: RoomIndex,
    view: ActiveRoomView,
) -> PortalVisibilitySpace {
    let camera_global = local_to_global_room_point(current_index, view.position);
    let visibility_index =
        room_index_containing_global_from(current_index, camera_global).unwrap_or(current_index);
    let visibility_view = if visibility_index == current_index {
        view
    } else {
        ActiveRoomView {
            position: global_to_local_room_point(visibility_index, camera_global),
            ..view
        }
    };
    PortalVisibilitySpace {
        room: visibility_index,
        view: visibility_view,
        camera_global,
    }
}

pub(super) fn portal_visibility_view_keys(view: ActiveRoomView) -> (i16, i16, i16, i16) {
    (
        (view.sin_yaw / 64) as i16,
        (view.cos_yaw / 64) as i16,
        (view.sin_pitch / 64) as i16,
        (view.cos_pitch / 64) as i16,
    )
}

pub(super) fn authored_room_for_chunk(index: RoomIndex) -> Option<u32> {
    chunk_record_for_room(index).map(|chunk| chunk.authored_room)
}

pub(super) fn chunk_record_for_room(index: RoomIndex) -> Option<&'static LevelChunkRecord> {
    if let Some(chunk) = ROOM_CHUNKS.get(index.to_usize()) {
        if chunk.room == index {
            return Some(chunk);
        }
    }
    ROOM_CHUNKS.iter().find(|chunk| chunk.room == index)
}

pub(super) fn chunk_overlaps_collision_window(
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

pub(super) fn point_xz_axis_moved_at_least(a: RoomPoint, b: RoomPoint, threshold: i32) -> bool {
    axis_moved_at_least(a.x, b.x, threshold) || axis_moved_at_least(a.z, b.z, threshold)
}

pub(super) fn point_xyz_axis_moved_at_least(a: RoomPoint, b: RoomPoint, threshold: i32) -> bool {
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

pub(super) fn collect_portal_room_bounds(
    out: &mut [PortalRoomBounds; MAX_PORTAL_ROOM_BOUNDS],
) -> usize {
    let mut count = 0usize;
    for visibility in ROOM_VISIBILITY {
        let Some(record) = ROOMS.get(visibility.room.to_usize()) else {
            continue;
        };
        let first = visibility.cell_first.to_usize();
        let end = first.saturating_add(visibility.cell_count as usize);
        let Some(cells) = VISIBILITY_CELLS.get(first..end) else {
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
            );
        }
    }
    if count > 0 {
        return count;
    }

    if !ROOM_CHUNKS.is_empty() {
        for chunk in ROOM_CHUNKS {
            let Some(record) = ROOMS.get(chunk.room.to_usize()) else {
                continue;
            };
            let (x0, x1, z0, z1) = chunk_global_bounds(*chunk, record);
            count = push_portal_room_bounds(out, count, chunk.room, x0, x1, z0, z1);
        }
        return count;
    }

    for (raw_index, record) in ROOMS.iter().enumerate() {
        if raw_index >= u16::MAX as usize {
            break;
        }
        let Some(room) = parse_runtime_room(record) else {
            continue;
        };
        let (x0, x1, z0, z1) = room_bounds(record, room);
        count =
            push_portal_room_bounds(out, count, RoomIndex::new(raw_index as u16), x0, x1, z0, z1);
    }
    count
}

fn push_portal_room_bounds(
    out: &mut [PortalRoomBounds; MAX_PORTAL_ROOM_BOUNDS],
    count: usize,
    room: RoomIndex,
    min_x: i32,
    max_x: i32,
    min_z: i32,
    max_z: i32,
) -> usize {
    if count >= out.len() || min_x >= max_x || min_z >= max_z {
        return count;
    }
    out[count] = PortalRoomBounds {
        room,
        min_x,
        max_x,
        min_y: PORTAL_ROOM_BOUNDS_MIN_Y,
        max_y: PORTAL_ROOM_BOUNDS_MAX_Y,
        min_z,
        max_z,
    };
    count + 1
}

pub(super) fn collision_room_collected(
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

fn room_index_containing_global(point: RoomPoint) -> Option<RoomIndex> {
    if !ROOM_CHUNKS.is_empty() {
        for chunk in ROOM_CHUNKS {
            let Some(record) = ROOMS.get(chunk.room.to_usize()) else {
                continue;
            };
            if chunk_contains_global_point(*chunk, record, point) {
                return Some(chunk.room);
            }
        }
        return None;
    }
    for (raw_index, record) in ROOMS.iter().enumerate() {
        let Some(room) = parse_runtime_room(record) else {
            continue;
        };
        let (x0, x1, z0, z1) = room_bounds(record, room);
        if point.x >= x0 && point.x < x1 && point.z >= z0 && point.z < z1 {
            return Some(RoomIndex::new(raw_index as u16));
        }
    }
    None
}

pub(super) fn room_index_containing_global_from(
    current: RoomIndex,
    point: RoomPoint,
) -> Option<RoomIndex> {
    if !ROOM_CHUNKS.is_empty() {
        let current_authored = authored_room_for_chunk(current);
        return room_index_containing_global_by_neighbours(current, point).or_else(|| {
            room_index_containing_global_in_authored(point, current_authored).or_else(|| {
                if current_authored.is_none() {
                    room_index_containing_global(point)
                } else {
                    None
                }
            })
        });
    }
    room_index_containing_global(point)
}

fn room_index_containing_global_by_neighbours(
    current: RoomIndex,
    point: RoomPoint,
) -> Option<RoomIndex> {
    let current_authored = authored_room_for_chunk(current);
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
        if current_authored.is_some() && authored_room_for_chunk(index) != current_authored {
            continue;
        }
        let Some(chunk) = chunk_record_for_room(index) else {
            continue;
        };
        let Some(record) = ROOMS.get(index.to_usize()) else {
            continue;
        };
        if chunk_contains_global_point(*chunk, record, point) {
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
    point: RoomPoint,
    authored_room: Option<u32>,
) -> Option<RoomIndex> {
    for chunk in ROOM_CHUNKS {
        if authored_room.is_some() && Some(chunk.authored_room) != authored_room {
            continue;
        }
        let Some(record) = ROOMS.get(chunk.room.to_usize()) else {
            continue;
        };
        if chunk_contains_global_point(*chunk, record, point) {
            return Some(chunk.room);
        }
    }
    None
}

fn push_room_search(
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
    chunk: LevelChunkRecord,
    record: &LevelRoomRecord,
    point: RoomPoint,
) -> bool {
    if chunk.room.to_usize() >= ROOMS.len() {
        return false;
    }
    match room_visibility_contains_global_point(chunk.room, record, point) {
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
    room_visibility_contains_cell(room, sx, sz)
}

fn room_visibility_contains_cell(room: RoomIndex, sx: u16, sz: u16) -> Option<bool> {
    let visibility = ROOM_VISIBILITY
        .iter()
        .find(|visibility| visibility.room == room)?;
    let first = visibility.cell_first.to_usize();
    let count = visibility.cell_count as usize;
    let cells = VISIBILITY_CELLS.get(first..first.checked_add(count)?)?;
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

pub(super) fn local_to_global_room_point(room: RoomIndex, point: RoomPoint) -> RoomPoint {
    let Some(record) = ROOMS.get(room.to_usize()) else {
        return point;
    };
    RoomPoint::new(
        point.x.saturating_add(room_origin_x(record)),
        point.y.saturating_add(room_origin_y(record)),
        point.z.saturating_add(room_origin_z(record)),
    )
}

pub(super) fn global_to_local_room_point(room: RoomIndex, point: RoomPoint) -> RoomPoint {
    let Some(record) = ROOMS.get(room.to_usize()) else {
        return point;
    };
    RoomPoint::new(
        point.x.saturating_sub(room_origin_x(record)),
        point.y.saturating_sub(room_origin_y(record)),
        point.z.saturating_sub(room_origin_z(record)),
    )
}

pub(super) fn camera_for_room(camera: WorldCamera, active: ActiveRuntimeRoom) -> WorldCamera {
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

pub(super) fn active_room_overlaps_collision_window(
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
