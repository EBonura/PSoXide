use super::*;

#[derive(Copy, Clone)]
pub(super) struct ActiveRoomSurfaceCache {
    pub(super) cell_first: usize,
    pub(super) cell_count: usize,
    pub(super) cell_vertex_first: usize,
    pub(super) cell_vertex_count: usize,
    pub(super) vertex_first: usize,
    pub(super) vertex_count: usize,
    pub(super) surface_first: usize,
    pub(super) surface_count: usize,
    pub(super) status: ActiveRoomCacheStatus,
    pub(super) ready: bool,
}

impl ActiveRoomSurfaceCache {
    pub(super) const EMPTY: Self = Self {
        cell_first: 0,
        cell_count: 0,
        cell_vertex_first: 0,
        cell_vertex_count: 0,
        vertex_first: 0,
        vertex_count: 0,
        surface_first: 0,
        surface_count: 0,
        status: ActiveRoomCacheStatus::NotBuilt,
        ready: false,
    };
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub(super) enum ActiveRoomCacheStatus {
    Ready,
    NotBuilt,
    Overflow,
    Empty,
}

#[derive(Copy, Clone)]
pub(super) struct ActiveRuntimeRoom {
    pub(super) index: RoomIndex,
    pub(super) stream_slot: u16,
    pub(super) render_room: Option<RuntimeRoom<'static>>,
    pub(super) collision_room: RuntimeCollisionRoom<'static>,
    pub(super) width: u16,
    pub(super) depth: u16,
    pub(super) sector_size: i32,
    pub(super) ambient_rgb: [u8; 3],
    /// Non-streamed builds keep materials inline; streamed builds pool them by
    /// `stream_slot` (see [`ROOM_MATERIAL_POOL`]) to keep this struct small.
    #[cfg(not(feature = "cd-stream-bench"))]
    pub(super) materials: [WorldRenderMaterial; MAX_ROOM_MATERIALS],
    #[cfg(not(feature = "cd-stream-bench"))]
    pub(super) material_count: usize,
    /// Offset from the current chunk's origin to this chunk's
    /// origin, in engine units.
    pub(super) offset_x: i32,
    pub(super) offset_z: i32,
    /// Vertical offset from the current room's elevation to this room's,
    /// in engine units. Stacked floors cook to distinct `origin_y`; this
    /// places the room's geometry at its real height relative to the
    /// camera so an upper floor renders a storey up, not on top of the
    /// current one at Y=0.
    pub(super) offset_y: i32,
    pub(super) surface_cache: ActiveRoomSurfaceCache,
}

impl ActiveRuntimeRoom {
    pub(super) fn render(&self) -> Option<RoomRender<'static, '_>> {
        self.render_room.as_ref().map(|room| room.render())
    }

    /// In-use room-surface materials. Streamed builds read the `stream_slot`
    /// pool; non-streamed builds read the inline array.
    pub(super) fn materials(&self) -> &[WorldRenderMaterial] {
        #[cfg(feature = "cd-stream-bench")]
        {
            let slot = self.stream_slot as usize;
            if slot < STREAMED_ROOM_SLOT_COUNT {
                let pool = unsafe { &*core::ptr::addr_of!(ROOM_MATERIAL_POOL) };
                return &pool[slot].materials[..pool[slot].count];
            }
            &[]
        }
        #[cfg(not(feature = "cd-stream-bench"))]
        {
            &self.materials[..self.material_count]
        }
    }

    pub(super) fn with_current_room_offsets(
        mut self,
        record: &LevelRoomRecord,
        current_record: &LevelRoomRecord,
    ) -> Self {
        self.offset_x = room_origin_x(record).saturating_sub(room_origin_x(current_record));
        self.offset_z = room_origin_z(record).saturating_sub(room_origin_z(current_record));
        // `origin_y` is absolute engine units (not sector-scaled like
        // x/z), so the vertical offset is a plain record difference.
        self.offset_y = record.origin_y.saturating_sub(current_record.origin_y);
        self
    }
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
#[derive(Copy, Clone)]
pub(super) struct ActiveVisibleCellCache {
    pub(super) room: RoomIndex,
    pub(super) anchor_x: i32,
    pub(super) anchor_z: i32,
    pub(super) view_sin_key: i16,
    pub(super) view_cos_key: i16,
    pub(super) camera_independent: bool,
    pub(super) rejected_global: u16,
    pub(super) first: u16,
    pub(super) count: u16,
    pub(super) ready: bool,
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
impl ActiveVisibleCellCache {
    pub(super) const EMPTY: Self = Self {
        room: RoomIndex::ZERO,
        anchor_x: 0,
        anchor_z: 0,
        view_sin_key: 0,
        view_cos_key: 0,
        camera_independent: false,
        rejected_global: 0,
        first: 0,
        count: 0,
        ready: false,
    };
}

#[derive(Copy, Clone)]
pub(super) struct ActiveRoomWindowJob {
    pub(super) active: bool,
    pub(super) update_streaming: bool,
    pub(super) current_room: RoomIndex,
    pub(super) requested_rooms: [RoomIndex; MAX_ACTIVE_ROOMS],
    pub(super) requested_count: usize,
    pub(super) cursor: usize,
    pub(super) next_slot: usize,
    pub(super) rooms: [Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    pub(super) previous_rooms: [Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
}

impl ActiveRoomWindowJob {
    pub(super) const EMPTY: Self = Self {
        active: false,
        update_streaming: false,
        current_room: RoomIndex::ZERO,
        requested_rooms: [INVALID_ROOM_INDEX; MAX_ACTIVE_ROOMS],
        requested_count: 0,
        cursor: 0,
        next_slot: 0,
        rooms: [const { None }; MAX_ACTIVE_ROOMS],
        previous_rooms: [const { None }; MAX_ACTIVE_ROOMS],
    };
}

pub(super) fn parse_runtime_room(record: &LevelRoomRecord) -> Option<RuntimeRoom<'static>> {
    let asset = find_asset_of_kind(ASSETS, record.world_asset, AssetKind::RoomWorld)?;
    RuntimeRoom::from_bytes(asset.bytes).ok()
}

pub(super) fn parse_collision_room_for_index(
    index: RoomIndex,
    record: &LevelRoomRecord,
) -> Option<RuntimeCollisionRoom<'static>> {
    #[cfg(feature = "cd-stream-bench")]
    {
        let _ = record;
        parse_streamed_compact_collision_room(0, index).map(RuntimeCollisionRoom::Compact)
    }
    #[cfg(not(feature = "cd-stream-bench"))]
    {
        let _ = index;
        parse_runtime_room(record).map(RuntimeCollisionRoom::Runtime)
    }
}

#[derive(Copy, Clone)]
struct ParsedActiveRoomPayload {
    render_room: Option<RuntimeRoom<'static>>,
    collision_room: RuntimeCollisionRoom<'static>,
    width: u16,
    depth: u16,
    sector_size: i32,
    ambient_rgb: [u8; 3],
}

fn parse_active_room_payload(
    slot: usize,
    index: RoomIndex,
    record: &LevelRoomRecord,
) -> Option<ParsedActiveRoomPayload> {
    #[cfg(feature = "cd-stream-bench")]
    if let Some(room) = parse_streamed_compact_collision_room(slot, index) {
        return Some(ParsedActiveRoomPayload {
            render_room: None,
            collision_room: RuntimeCollisionRoom::Compact(room),
            width: room.width(),
            depth: room.depth(),
            sector_size: room.sector_size(),
            ambient_rgb: room.ambient_color(),
        });
    }
    #[cfg(not(feature = "cd-stream-bench"))]
    {
        let _ = (slot, index);
        let room = parse_runtime_room(record)?;
        Some(ParsedActiveRoomPayload {
            render_room: Some(room),
            collision_room: RuntimeCollisionRoom::Runtime(room),
            width: room.width(),
            depth: room.depth(),
            sector_size: room.sector_size(),
            ambient_rgb: room.render().ambient_color(),
        })
    }
    #[cfg(feature = "cd-stream-bench")]
    {
        let _ = record;
        None
    }
}

// Retained after the BFS-ring residency rewrite (the desired-set is now copied
// from the cached stream ring); kept for other build paths / future reuse.
pub(super) const fn room_material_fallback() -> WorldRenderMaterial {
    WorldRenderMaterial::both(TextureMaterial::opaque(0, TPAGE_WORD, (0x80, 0x80, 0x80)))
}

/// Refactor B: room-surface materials live in a pool keyed by the resident
/// `stream_slot` rather than inline in `ActiveRuntimeRoom`, so the per-crossing
/// copy of the `[ActiveRuntimeRoom; MAX_ACTIVE_ROOMS]` window stays small. An
/// entry is (re)built whenever a room becomes active in its slot and read at
/// render through `ActiveRuntimeRoom::materials()`.
#[cfg(feature = "cd-stream-bench")]
#[derive(Copy, Clone)]
struct ResidentRoomMaterials {
    materials: [WorldRenderMaterial; MAX_ROOM_MATERIALS],
    count: usize,
}

#[cfg(feature = "cd-stream-bench")]
static mut ROOM_MATERIAL_POOL: [ResidentRoomMaterials; STREAMED_ROOM_SLOT_COUNT] =
    [ResidentRoomMaterials {
        materials: [room_material_fallback(); MAX_ROOM_MATERIALS],
        count: 0,
    }; STREAMED_ROOM_SLOT_COUNT];

#[cfg(feature = "cd-stream-bench")]
pub(super) fn store_room_materials(
    stream_slot: u16,
    materials: [WorldRenderMaterial; MAX_ROOM_MATERIALS],
    count: usize,
) {
    let slot = stream_slot as usize;
    if slot < STREAMED_ROOM_SLOT_COUNT {
        let pool = unsafe { &mut *core::ptr::addr_of_mut!(ROOM_MATERIAL_POOL) };
        pool[slot] = ResidentRoomMaterials { materials, count };
    }
}

pub(super) fn build_active_room(
    slot: usize,
    index: RoomIndex,
    record: &LevelRoomRecord,
    current_record: &LevelRoomRecord,
) -> Option<ActiveRuntimeRoom> {
    if let Some(residency) = ROOM_RESIDENCY.iter().find(|r| r.room == index) {
        let _ = unsafe { RESIDENCY.ensure_room_resident(residency) };
    }
    let payload = parse_active_room_payload(slot, index, record)?;
    let (materials, material_count) = build_runtime_room_material_table(record);
    let stream_slot = active_room_stream_slot(index);
    #[cfg(feature = "cd-stream-bench")]
    store_room_materials(stream_slot, materials, material_count);
    let surface_cache = active_room_surface_cache_for(index);
    Some(ActiveRuntimeRoom {
        index,
        stream_slot,
        render_room: payload.render_room,
        collision_room: payload.collision_room,
        width: payload.width,
        depth: payload.depth,
        sector_size: payload.sector_size,
        ambient_rgb: payload.ambient_rgb,
        #[cfg(not(feature = "cd-stream-bench"))]
        materials,
        #[cfg(not(feature = "cd-stream-bench"))]
        material_count,
        offset_x: room_origin_x(record).saturating_sub(room_origin_x(current_record)),
        offset_z: room_origin_z(record).saturating_sub(room_origin_z(current_record)),
        offset_y: record.origin_y.saturating_sub(current_record.origin_y),
        surface_cache,
    })
}

pub(super) fn reuse_or_build_active_room(
    slot: usize,
    index: RoomIndex,
    record: &LevelRoomRecord,
    current_record: &LevelRoomRecord,
    previous_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
) -> Option<ActiveRuntimeRoom> {
    let stream_slot = active_room_stream_slot(index);
    for previous in previous_rooms.iter().flatten().copied() {
        if previous.index != index || previous.stream_slot != stream_slot {
            continue;
        }
        return Some(previous.with_current_room_offsets(record, current_record));
    }
    build_active_room(slot, index, record, current_record)
}

pub(super) fn active_room_stream_slot(index: RoomIndex) -> u16 {
    #[cfg(feature = "cd-stream-bench")]
    unsafe {
        ROOM_STREAM_SCHEDULER
            .resident_slot_for(index)
            .and_then(|slot| u16::try_from(slot).ok())
            .unwrap_or(STREAMED_ROOM_SLOT_NONE)
    }
    #[cfg(not(feature = "cd-stream-bench"))]
    {
        let _ = index;
        u16::MAX
    }
}

pub(super) fn build_runtime_room_material_table(
    record: &LevelRoomRecord,
) -> ([WorldRenderMaterial; MAX_ROOM_MATERIALS], usize) {
    let mut resolved_materials = [const { None }; MAX_ROOM_MATERIALS];
    let material_count = build_room_materials(record, &mut resolved_materials);
    let mut materials = [room_material_fallback(); MAX_ROOM_MATERIALS];
    for i in 0..material_count {
        if let Some(material) = resolved_materials[i] {
            materials[i] = material;
        }
    }
    (materials, material_count)
}

#[cfg(feature = "cd-stream-bench")]
pub(super) fn room_material_textures_ready(record: &LevelRoomRecord) -> bool {
    let mut resolved_materials = [const { None }; MAX_ROOM_MATERIALS];
    let _ = build_room_materials(record, &mut resolved_materials);
    let first = record.material_first.to_usize();
    let count = record.material_count as usize;
    let slice: &[LevelMaterialRecord] = &MATERIALS[first..first + count];
    let mut ready = true;

    for material in slice {
        let slot = material.local_slot.to_usize();
        if slot >= MAX_ROOM_MATERIALS {
            continue;
        }
        if find_asset_of_kind(ASSETS, material.texture_asset, AssetKind::Texture).is_some()
            && resolved_materials[slot].is_none()
        {
            ready = false;
        }
    }

    ready
}

pub(super) fn active_room_surface_cache_for(index: RoomIndex) -> ActiveRoomSurfaceCache {
    #[cfg(feature = "cd-stream-bench")]
    if let Some(cache) = streamed_active_room_surface_cache_for(index) {
        return cache;
    }

    let Some(cache) = ROOM_SURFACE_CACHES.iter().find(|cache| cache.room == index) else {
        return ActiveRoomSurfaceCache::EMPTY;
    };
    let cell_first = cache.cell_first as usize;
    let cell_count = cache.cell_count as usize;
    let cell_vertex_first = cache.cell_vertex_first as usize;
    let cell_vertex_count = cache.cell_vertex_count as usize;
    let vertex_first = cache.vertex_first as usize;
    let vertex_count = cache.vertex_count as usize;
    let surface_first = cache.surface_first as usize;
    let surface_count = cache.surface_count as usize;
    if vertex_count > MAX_CACHED_ROOM_VERTICES
        || cell_first.saturating_add(cell_count) > ROOM_CACHE_CELLS.len()
        || cell_vertex_first.saturating_add(cell_vertex_count) > ROOM_CACHE_CELL_VERTICES.len()
        || vertex_first.saturating_add(vertex_count) > ROOM_CACHE_VERTICES.len()
        || surface_first.saturating_add(surface_count) > ROOM_CACHE_SURFACES.len()
    {
        return ActiveRoomSurfaceCache {
            status: ActiveRoomCacheStatus::Overflow,
            ..ActiveRoomSurfaceCache::EMPTY
        };
    }
    if cell_count == 0 || vertex_count == 0 || surface_count == 0 {
        return ActiveRoomSurfaceCache {
            status: ActiveRoomCacheStatus::Empty,
            ..ActiveRoomSurfaceCache::EMPTY
        };
    }
    ActiveRoomSurfaceCache {
        cell_first,
        cell_count,
        cell_vertex_first,
        cell_vertex_count,
        vertex_first,
        vertex_count,
        surface_first,
        surface_count,
        status: ActiveRoomCacheStatus::Ready,
        ready: true,
    }
}

#[cfg(feature = "cd-stream-bench")]
fn streamed_active_room_surface_cache_for(index: RoomIndex) -> Option<ActiveRoomSurfaceCache> {
    unsafe {
        let resident_slot = ROOM_STREAM_SCHEDULER.resident_slot_for(index)?;
        let byte_count = ROOM_STREAM_SCHEDULER.resident_byte_count(resident_slot)?;
        let bytes = streamed_room_slot_bytes(resident_slot, byte_count)?;
        let view = streamed_room_chunk_view(bytes, index)?;
        if view.vertex_count > MAX_CACHED_ROOM_VERTICES {
            return Some(ActiveRoomSurfaceCache {
                status: ActiveRoomCacheStatus::Overflow,
                ..ActiveRoomSurfaceCache::EMPTY
            });
        }
        if view.cell_count == 0 || view.vertex_count == 0 || view.surface_count == 0 {
            return Some(ActiveRoomSurfaceCache {
                status: ActiveRoomCacheStatus::Empty,
                ..ActiveRoomSurfaceCache::EMPTY
            });
        }
        Some(ActiveRoomSurfaceCache {
            cell_first: view.cells_offset,
            cell_count: view.cell_count,
            cell_vertex_first: view.cell_vertices_offset,
            cell_vertex_count: view.cell_vertex_count,
            vertex_first: view.vertices_offset,
            vertex_count: view.vertex_count,
            surface_first: view.surfaces_offset,
            surface_count: view.surface_count,
            status: ActiveRoomCacheStatus::Ready,
            ready: true,
        })
    }
}

pub(super) fn room_surface_cache_slices(
    index: RoomIndex,
    cache: ActiveRoomSurfaceCache,
) -> Option<(
    &'static [CachedRoomCell],
    &'static [u16],
    &'static [WorldVertex],
    &'static [CachedRoomSurface],
)> {
    #[cfg(feature = "cd-stream-bench")]
    if let Some(slices) = streamed_room_surface_cache_slices(index, cache) {
        return Some(slices);
    }
    #[cfg(not(feature = "cd-stream-bench"))]
    let _ = index;

    generated_room_surface_cache_slices(cache)
}

fn generated_room_surface_cache_slices(
    cache: ActiveRoomSurfaceCache,
) -> Option<(
    &'static [CachedRoomCell],
    &'static [u16],
    &'static [WorldVertex],
    &'static [CachedRoomSurface],
)> {
    if !cache.ready || cache.vertex_count > MAX_CACHED_ROOM_VERTICES {
        return None;
    }
    let cell_end = cache.cell_first.checked_add(cache.cell_count)?;
    let cell_vertex_end = cache
        .cell_vertex_first
        .checked_add(cache.cell_vertex_count)?;
    let vertex_end = cache.vertex_first.checked_add(cache.vertex_count)?;
    let surface_end = cache.surface_first.checked_add(cache.surface_count)?;
    let cells = ROOM_CACHE_CELLS.get(cache.cell_first..cell_end)?;
    let cell_vertices = ROOM_CACHE_CELL_VERTICES.get(cache.cell_vertex_first..cell_vertex_end)?;
    let vertices = ROOM_CACHE_VERTICES.get(cache.vertex_first..vertex_end)?;
    let surfaces = ROOM_CACHE_SURFACES.get(cache.surface_first..surface_end)?;
    Some((
        cached_room_cells_from_level_records(cells),
        cell_vertices,
        cached_room_vertices_from_level_records(vertices),
        cached_room_surfaces_from_level_records(surfaces),
    ))
}

#[cfg(feature = "cd-stream-bench")]
fn streamed_room_surface_cache_slices(
    index: RoomIndex,
    cache: ActiveRoomSurfaceCache,
) -> Option<(
    &'static [CachedRoomCell],
    &'static [u16],
    &'static [WorldVertex],
    &'static [CachedRoomSurface],
)> {
    if !cache.ready || cache.vertex_count > MAX_CACHED_ROOM_VERTICES {
        return None;
    }
    unsafe {
        let resident_slot = ROOM_STREAM_SCHEDULER.resident_slot_for(index)?;
        let byte_count = ROOM_STREAM_SCHEDULER.resident_byte_count(resident_slot)?;
        let bytes = streamed_room_slot_bytes(resident_slot, byte_count)?;
        let view = streamed_room_chunk_view(bytes, index)?;
        if cache.cell_first != view.cells_offset
            || cache.cell_count != view.cell_count
            || cache.cell_vertex_first != view.cell_vertices_offset
            || cache.cell_vertex_count != view.cell_vertex_count
            || cache.vertex_first != view.vertices_offset
            || cache.vertex_count != view.vertex_count
            || cache.surface_first != view.surfaces_offset
            || cache.surface_count != view.surface_count
        {
            return None;
        }
        let cells = streamed_record_slice::<LevelCachedRoomCellRecord>(
            bytes,
            view.total_bytes,
            view.cells_offset,
            view.cell_count,
        )?;
        let cell_vertices = streamed_record_slice::<u16>(
            bytes,
            view.total_bytes,
            view.cell_vertices_offset,
            view.cell_vertex_count,
        )?;
        let vertices = streamed_record_slice::<LevelCachedRoomVertexRecord>(
            bytes,
            view.total_bytes,
            view.vertices_offset,
            view.vertex_count,
        )?;
        let surfaces = streamed_record_slice::<LevelCachedRoomSurfaceRecord>(
            bytes,
            view.total_bytes,
            view.surfaces_offset,
            view.surface_count,
        )?;
        Some((
            cached_room_cells_from_level_records(cells),
            cell_vertices,
            cached_room_vertices_from_level_records(vertices),
            cached_room_surfaces_from_level_records(surfaces),
        ))
    }
}

#[cfg(feature = "cd-stream-bench")]
fn streamed_record_slice<T>(
    bytes: &'static [u8],
    total_bytes: usize,
    offset: usize,
    count: usize,
) -> Option<&'static [T]> {
    if !streamed_chunk_range_valid::<T>(total_bytes, offset, count) {
        return None;
    }
    let byte_count = count.checked_mul(core::mem::size_of::<T>())?;
    let slice = bytes.get(offset..offset + byte_count)?;
    Some(unsafe { core::slice::from_raw_parts(slice.as_ptr().cast::<T>(), count) })
}

pub(super) fn active_surface_cache_failed(cache: ActiveRoomSurfaceCache) -> bool {
    !cache.ready && cache.status != ActiveRoomCacheStatus::Empty
}

impl Playtest {
    pub(super) fn chunked_level(&self) -> bool {
        !ROOM_CHUNKS.is_empty()
    }

    pub(super) fn active_room_selection_view(&self) -> ActiveRoomView {
        ActiveRoomView::from_camera(self.render_camera)
    }

    pub(super) fn rebuild_portal_visibility(
        &mut self,
        current_index: RoomIndex,
        current_record: &LevelRoomRecord,
        view: ActiveRoomView,
        camera_global: RoomPoint,
    ) {
        let half_fov_x_tan_q12 = ((SCREEN_CX as i32).saturating_mul(4096) / FOCAL.max(1)).max(1);
        let half_fov_y_tan_q12 = ((SCREEN_CY as i32).saturating_mul(4096) / FOCAL.max(1)).max(1);
        let far_z = current_record.draw_distance.clamp(NEAR_Z, FAR_Z);
        self.portal_visibility_root = current_index;
        self.portal_visibility_camera_global = camera_global;
        telemetry::stage_begin(telemetry::stage::PORTAL_VISIBILITY);
        let camera = PortalVisibilityCamera::new(
            camera_global.x,
            camera_global.y,
            camera_global.z,
            view.sin_yaw,
            view.cos_yaw,
            view.sin_pitch,
            view.cos_pitch,
            PROJECTION.near_z,
            far_z,
            half_fov_x_tan_q12,
            half_fov_y_tan_q12,
            RUNTIME_SCHEDULE.portal_min_width_q12,
        );
        // The room bounds are a pure function of the static cooked geometry, so
        // collect them once and reuse the cached length on every later refresh.
        let bounds_count = match self.portal_room_bounds_count {
            Some(count) => count,
            None => {
                let count = collect_portal_room_bounds(&mut self.portal_room_bounds);
                self.portal_room_bounds_count = Some(count);
                count
            }
        };
        build_portal_visibility_with_room_bounds(
            ROOMS,
            ROOM_PORTALS,
            &self.portal_room_bounds[..bounds_count],
            current_index,
            camera,
            RUNTIME_SCHEDULE.portal_max_depth,
            &mut self.portal_visibility,
        );
        telemetry::stage_end(telemetry::stage::PORTAL_VISIBILITY);
        if PORTAL_VIS_DEBUG_LOGS
            && self.portal_debug_log_cooldown == 0
            && should_debug_log_portal_visibility(current_record, &self.portal_visibility)
        {
            let player_local = self.motor.position();
            let player_global = local_to_global_room_point(self.room_index, player_local);
            debug_log_portal_visibility_snapshot(
                current_index,
                current_record,
                self.room_index,
                player_local,
                player_global,
                view,
                camera,
                &self.portal_visibility,
            );
            self.portal_debug_log_cooldown = PORTAL_VIS_DEBUG_LOG_COOLDOWN_TICKS;
        }
    }

    pub(super) fn refresh_portal_visibility_for_view(
        &mut self,
        current_index: RoomIndex,
        current_record: &LevelRoomRecord,
        view: ActiveRoomView,
    ) {
        let visibility_space = portal_visibility_space_for_view(current_index, view);
        let visibility_index = visibility_space.room;
        let visibility_record = ROOMS
            .get(visibility_index.to_usize())
            .unwrap_or(current_record);
        let (view_sin_key, view_cos_key, view_pitch_sin_key, view_pitch_cos_key) =
            portal_visibility_view_keys(view);
        self.active_room_view_sin_key = view_sin_key;
        self.active_room_view_cos_key = view_cos_key;
        self.active_room_view_pitch_sin_key = view_pitch_sin_key;
        self.active_room_view_pitch_cos_key = view_pitch_cos_key;
        self.active_room_view_anchor = view.position;
        self.rebuild_portal_visibility(
            visibility_index,
            visibility_record,
            visibility_space.view,
            visibility_space.camera_global,
        );
        self.active_room_candidates = self.portal_visibility.stats.portals_tested.min(u16::MAX);
        self.portal_visible_missing_resident = 0;
        self.portal_visible_missing_mask = RuntimeDebugMask::EMPTY;
        self.portal_visible_build_failed = 0;
        self.portal_visible_build_failed_mask = RuntimeDebugMask::EMPTY;
    }

    pub(super) fn portal_visible_room_limit(&self, current_record: &LevelRoomRecord) -> usize {
        self.portal_visibility
            .room_count
            .min(room_active_chunk_limit(current_record))
            .min(MAX_ACTIVE_ROOMS)
    }

    pub(super) fn portal_visible_rooms_are_active(&self, current_record: &LevelRoomRecord) -> bool {
        if !self.active_room_contains_drawable(self.room_index) {
            return false;
        }
        let visible_limit = self.portal_visible_room_limit(current_record);
        let mut i = 0usize;
        while i < visible_limit {
            if !self.active_room_contains_drawable(self.portal_visibility.rooms[i].room) {
                return false;
            }
            i += 1;
        }
        true
    }

    pub(super) fn active_room_contains_drawable(&self, index: RoomIndex) -> bool {
        let mut slot = 0usize;
        while slot < MAX_ACTIVE_ROOMS {
            if let Some(active) = self.active_rooms[slot] {
                if active.index == index
                    && (index == self.room_index
                        || active.render_room.is_some()
                        || active.surface_cache.ready)
                {
                    return true;
                }
            }
            slot += 1;
        }
        false
    }

    pub(super) fn retain_previous_active_rooms(
        &mut self,
        previous_active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
        current_record: &LevelRoomRecord,
        active_limit: usize,
        next_slot: &mut usize,
    ) {
        let retained_limit = next_slot
            .saturating_add(RUNTIME_SCHEDULE.retained_inactive_rooms)
            .min(active_limit)
            .min(MAX_ACTIVE_ROOMS);
        let mut previous_slot = 0usize;
        while *next_slot < retained_limit && previous_slot < MAX_ACTIVE_ROOMS {
            let Some(previous) = previous_active_rooms[previous_slot] else {
                previous_slot += 1;
                continue;
            };
            previous_slot += 1;
            if previous.stream_slot != active_room_stream_slot(previous.index)
                || self.active_room_contains(previous.index)
            {
                continue;
            }
            let Some(record) = ROOMS.get(previous.index.to_usize()) else {
                continue;
            };
            self.active_rooms[*next_slot] =
                Some(previous.with_current_room_offsets(record, current_record));
            *next_slot += 1;
        }
    }

    pub(super) fn active_room_contains(&self, index: RoomIndex) -> bool {
        let mut slot = 0usize;
        while slot < MAX_ACTIVE_ROOMS {
            if self.active_rooms[slot].is_some_and(|active| active.index == index) {
                return true;
            }
            slot += 1;
        }
        false
    }

    pub(super) fn active_room_mask(&self) -> RuntimeDebugMask {
        let mut mask = RuntimeDebugMask::EMPTY;
        let mut slot = 0usize;
        while slot < MAX_ACTIVE_ROOMS {
            if let Some(active) = self.active_rooms[slot] {
                mask.insert_room(active.index);
            }
            slot += 1;
        }
        mask
    }

    pub(super) fn active_room_drawable_mask(&self) -> RuntimeDebugMask {
        let mut mask = RuntimeDebugMask::EMPTY;
        let mut slot = 0usize;
        while slot < MAX_ACTIVE_ROOMS {
            if let Some(active) = self.active_rooms[slot] {
                if active.index == self.room_index
                    || active.render_room.is_some()
                    || active.surface_cache.ready
                {
                    mask.insert_room(active.index);
                }
            }
            slot += 1;
        }
        mask
    }

    pub(super) fn portal_visibility_draws_room(&self, index: RoomIndex) -> bool {
        // Node-traversal draw gate: draw a room only if the portal walk reached
        // it through a frustum-facing portal. This prunes rooms whose connecting
        // portal is behind the camera (resident in the ring but never visible).
        // Visible rooms then rasterize ALL their cells -- per-polygon backface +
        // screen culling does the rest, there is no per-cell PVS. The camera's
        // own room always draws even if the walk has not repopulated this frame.
        index == self.portal_visibility_root || self.portal_visibility.contains_room(index)
    }

    pub(super) fn emit_portal_visibility_counters(&self) {
        let stats = self.portal_visibility.stats;
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_CURRENT_ROOM,
            self.portal_visibility_root.raw() as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_VISIBLE_ROOMS,
            self.portal_visibility.room_count as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_FRONTIER_ROOMS,
            self.portal_visibility.frontier_count as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_FRUSTUMS,
            self.portal_visibility.frustum_count as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_PORTALS_TESTED,
            stats.portals_tested as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_PORTALS_ACCEPTED,
            stats.portals_accepted as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_REJECT_BACKFACE,
            stats.reject_backface as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_REJECT_FRUSTUM,
            stats.reject_frustum as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_REJECT_TINY,
            stats.reject_tiny as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_BOUNDS_FALLBACKS,
            stats.bounds_fallbacks as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_CAP_ROOM,
            stats.cap_room as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_CAP_FRUSTUM,
            stats.cap_frustum as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_CAP_DEPTH,
            stats.cap_depth as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_VISIBLE_MISSING_RESIDENT,
            self.portal_visible_missing_resident as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_VISIBLE_BUILD_FAILED,
            self.portal_visible_build_failed as u32,
        );
        telemetry::counter(
            telemetry::counter::ROOM_STREAM_PRIORITY_CURRENT,
            self.portal_stream_priority_current as u32,
        );
        telemetry::counter(
            telemetry::counter::ROOM_STREAM_PRIORITY_VISIBLE,
            self.portal_stream_priority_visible as u32,
        );
        telemetry::counter(
            telemetry::counter::ROOM_STREAM_PRIORITY_FRONTIER,
            self.portal_stream_priority_frontier as u32,
        );
        emit_room_chunk_mask(
            telemetry::counter::PORTAL_VIS_VISIBLE_MASK_LO,
            telemetry::counter::PORTAL_VIS_VISIBLE_MASK_HI,
            self.portal_visibility.visible_room_mask(),
        );
        emit_room_chunk_mask(
            telemetry::counter::PORTAL_VIS_FRONTIER_MASK_LO,
            telemetry::counter::PORTAL_VIS_FRONTIER_MASK_HI,
            self.portal_visibility.frontier_room_mask(),
        );
        emit_room_chunk_mask(
            telemetry::counter::PORTAL_VIS_MISSING_MASK_LO,
            telemetry::counter::PORTAL_VIS_MISSING_MASK_HI,
            self.portal_visible_missing_mask,
        );
        emit_room_chunk_mask(
            telemetry::counter::PORTAL_VIS_BUILD_FAILED_MASK_LO,
            telemetry::counter::PORTAL_VIS_BUILD_FAILED_MASK_HI,
            self.portal_visible_build_failed_mask,
        );
        emit_room_chunk_mask(
            telemetry::counter::PORTAL_VIS_TESTED_MASK_LO,
            telemetry::counter::PORTAL_VIS_TESTED_MASK_HI,
            stats.tested_room_mask,
        );
        emit_room_chunk_mask(
            telemetry::counter::PORTAL_VIS_ACCEPTED_MASK_LO,
            telemetry::counter::PORTAL_VIS_ACCEPTED_MASK_HI,
            stats.accepted_room_mask,
        );
        emit_room_chunk_mask(
            telemetry::counter::PORTAL_VIS_REJECT_FRUSTUM_MASK_LO,
            telemetry::counter::PORTAL_VIS_REJECT_FRUSTUM_MASK_HI,
            stats.reject_frustum_room_mask,
        );
        emit_room_chunk_mask(
            telemetry::counter::PORTAL_VIS_BOUNDS_FALLBACK_MASK_LO,
            telemetry::counter::PORTAL_VIS_BOUNDS_FALLBACK_MASK_HI,
            stats.bounds_fallback_room_mask,
        );
        emit_room_chunk_mask(
            telemetry::counter::PORTAL_VIS_TESTED_PORTAL_MASK_LO,
            telemetry::counter::PORTAL_VIS_TESTED_PORTAL_MASK_HI,
            stats.tested_portal_mask,
        );
        emit_room_chunk_mask(
            telemetry::counter::PORTAL_VIS_ACCEPTED_PORTAL_MASK_LO,
            telemetry::counter::PORTAL_VIS_ACCEPTED_PORTAL_MASK_HI,
            stats.accepted_portal_mask,
        );
        emit_room_chunk_mask(
            telemetry::counter::PORTAL_VIS_REJECT_FRUSTUM_PORTAL_MASK_LO,
            telemetry::counter::PORTAL_VIS_REJECT_FRUSTUM_PORTAL_MASK_HI,
            stats.reject_frustum_portal_mask,
        );
        emit_room_chunk_mask(
            telemetry::counter::PORTAL_VIS_BOUNDS_FALLBACK_PORTAL_MASK_LO,
            telemetry::counter::PORTAL_VIS_BOUNDS_FALLBACK_PORTAL_MASK_HI,
            stats.bounds_fallback_portal_mask,
        );
    }

    pub(super) fn load_active_room_window(&mut self) {
        self.active_room_job = ActiveRoomWindowJob::EMPTY;
        if !self.chunked_level() {
            self.rebuild_active_room_window(true);
            return;
        }
        self.rebase_active_rooms_to_current_room();
        #[cfg(all(
            feature = "world-grid-visible",
            not(feature = "vis-full-active-chunks")
        ))]
        {
            self.clear_visible_cell_caches();
        }
        self.apply_current_active_room_fields();
        self.begin_active_room_window_job(true);
        if self.current_collision_room.is_none() {
            self.step_active_room_window_job();
        }
    }

    pub(super) fn rebase_active_rooms_to_current_room(&mut self) {
        let Some(current_record) = ROOMS.get(self.room_index.to_usize()) else {
            return;
        };
        let mut slot = 0usize;
        while slot < MAX_ACTIVE_ROOMS {
            let Some(active) = self.active_rooms[slot] else {
                slot += 1;
                continue;
            };
            let Some(record) = ROOMS.get(active.index.to_usize()) else {
                self.active_rooms[slot] = None;
                slot += 1;
                continue;
            };
            if active.stream_slot != active_room_stream_slot(active.index) {
                self.active_rooms[slot] = None;
                slot += 1;
                continue;
            }
            self.active_rooms[slot] =
                Some(active.with_current_room_offsets(record, current_record));
            slot += 1;
        }
    }

    pub(super) fn begin_active_room_window_job(&mut self, update_streaming: bool) {
        if !self.chunked_level() {
            return;
        }
        let current_index = self.room_index;
        let Some(current_record) = ROOMS.get(current_index.to_usize()) else {
            return;
        };
        let view = self.active_room_selection_view();
        self.refresh_portal_visibility_for_view(current_index, current_record, view);

        // Reachability draw: the active/drawn set is the unpruned portal-graph
        // ring around the camera's room (the visibility root), not the
        // frustum-clipped visible set. Side and behind-the-player rooms stay
        // drawn (no pop-in when a portal goes edge-on); per-polygon backface +
        // screen culling still removes the off-screen geometry cheaply.
        let mut requested_rooms = [INVALID_ROOM_INDEX; MAX_ACTIVE_ROOMS];
        requested_rooms[0] = current_index;
        let mut requested_count = 1usize;
        let mut ring = [INVALID_ROOM_INDEX; MAX_ACTIVE_ROOMS];
        let ring_count = room_graph_ring(
            self.portal_visibility_root,
            RESIDENT_DRAW_DEPTH,
            &mut ring,
            MAX_ACTIVE_ROOMS,
        );
        let mut i = 0usize;
        while i < ring_count && requested_count < MAX_ACTIVE_ROOMS {
            let room = ring[i];
            if room != current_index && room != INVALID_ROOM_INDEX {
                requested_rooms[requested_count] = room;
                requested_count += 1;
            }
            i += 1;
        }

        self.active_room_anchor = self.motor.position();
        self.active_room_cache_skips = 0;
        self.active_room_job = ActiveRoomWindowJob {
            active: true,
            update_streaming,
            current_room: current_index,
            requested_rooms,
            requested_count,
            cursor: 0,
            next_slot: 0,
            rooms: [const { None }; MAX_ACTIVE_ROOMS],
            previous_rooms: self.active_rooms,
        };
        telemetry::counter(telemetry::counter::ROOM_WINDOW_REBUILDS, 1);
    }

    pub(super) fn step_active_room_window_job(&mut self) {
        if !self.active_room_job.active {
            return;
        }
        let current_room = self.active_room_job.current_room;
        if current_room != self.room_index {
            self.active_room_job = ActiveRoomWindowJob::EMPTY;
            return;
        }
        let Some(current_record) = ROOMS.get(current_room.to_usize()) else {
            self.active_room_job = ActiveRoomWindowJob::EMPTY;
            return;
        };

        // Residency is owned by update_room_residency now; the build job no
        // longer requests streaming itself, it only builds from resident rooms.

        telemetry::stage_begin(telemetry::stage::ACTIVE_ROOM_WINDOW);
        let mut built_this_tick = 0usize;
        let mut skipped = 0u16;
        let mut unbuilt_room = INVALID_ROOM_INDEX;
        let mut current_active = None;
        {
            let job = &mut self.active_room_job;
            while job.cursor < job.requested_count
                && job.next_slot < MAX_ACTIVE_ROOMS
                && built_this_tick < RUNTIME_SCHEDULE.active_job_builds_per_tick
            {
                let index = job.requested_rooms[job.cursor];
                if index == INVALID_ROOM_INDEX {
                    job.cursor += 1;
                    continue;
                }
                let Some(record) = ROOMS.get(index.to_usize()) else {
                    job.cursor += 1;
                    continue;
                };
                match reuse_or_build_active_room(
                    job.next_slot,
                    index,
                    record,
                    current_record,
                    &job.previous_rooms,
                ) {
                    Some(active)
                        if job.cursor == 0
                            || active.render_room.is_some()
                            || active.surface_cache.ready =>
                    {
                        job.rooms[job.next_slot] = Some(active);
                        if active.index == current_room {
                            current_active = Some(active);
                        }
                        job.next_slot += 1;
                        job.cursor += 1;
                        built_this_tick += 1;
                    }
                    Some(_) => {
                        skipped = skipped.saturating_add(1);
                        job.cursor += 1;
                    }
                    None => {
                        unbuilt_room = index;
                        #[cfg(feature = "cd-stream-bench")]
                        {
                            if streamed_room_is_loading(index) || !streamed_room_is_resident(index)
                            {
                                break;
                            }
                            job.cursor += 1;
                        }
                        #[cfg(not(feature = "cd-stream-bench"))]
                        {
                            job.cursor += 1;
                        }
                    }
                }
            }
        }
        self.active_room_cache_skips = self.active_room_cache_skips.saturating_add(skipped);
        if unbuilt_room != INVALID_ROOM_INDEX {
            self.mark_visible_room_unbuilt(unbuilt_room);
        }
        if let Some(active) = current_active {
            self.apply_current_active_room(active);
        }

        telemetry::counter(
            telemetry::counter::ROOM_WINDOW_BUILT_CHUNKS,
            built_this_tick as u32,
        );
        telemetry::stage_end(telemetry::stage::ACTIVE_ROOM_WINDOW);

        if self.active_room_job.cursor >= self.active_room_job.requested_count
            || self.active_room_job.next_slot >= MAX_ACTIVE_ROOMS
        {
            self.active_rooms = self.active_room_job.rooms;
            let previous_rooms = self.active_room_job.previous_rooms;
            let mut next_slot = self.active_room_job.next_slot;
            self.retain_previous_active_rooms(
                &previous_rooms,
                current_record,
                room_active_chunk_limit(current_record),
                &mut next_slot,
            );
            self.apply_current_active_room_fields();
            self.active_room_job = ActiveRoomWindowJob::EMPTY;
        }
    }

    pub(super) fn apply_current_active_room_fields(&mut self) {
        self.room = None;
        self.current_collision_room = None;
        self.current_ambient_rgb = [0x80, 0x80, 0x80];
        self.materials = [room_material_fallback(); MAX_ROOM_MATERIALS];
        self.material_count = 0;
        let mut slot = 0usize;
        while slot < MAX_ACTIVE_ROOMS {
            if let Some(active) = self.active_rooms[slot] {
                if active.index == self.room_index {
                    self.apply_current_active_room(active);
                    return;
                }
            }
            slot += 1;
        }
    }

    pub(super) fn apply_current_active_room(&mut self, active: ActiveRuntimeRoom) {
        self.room = active.render_room;
        self.current_collision_room = Some(active.collision_room);
        self.current_ambient_rgb = active.ambient_rgb;
        self.set_current_materials(&active);
    }

    /// Copy a room's in-use materials into the current-room slot the renderer
    /// reads. Source is the `stream_slot` pool (streamed) or inline (non-stream).
    pub(super) fn set_current_materials(&mut self, active: &ActiveRuntimeRoom) {
        let mats = active.materials();
        self.material_count = mats.len();
        self.materials[..mats.len()].copy_from_slice(mats);
    }

    pub(super) fn refresh_active_room_materials(&mut self) {
        let mut slot = 0usize;
        while slot < MAX_ACTIVE_ROOMS {
            if let Some(active) = self.active_rooms[slot] {
                if let Some(record) = ROOMS.get(active.index.to_usize()) {
                    let (materials, material_count) = build_runtime_room_material_table(record);
                    #[cfg(feature = "cd-stream-bench")]
                    store_room_materials(active.stream_slot, materials, material_count);
                    #[cfg(not(feature = "cd-stream-bench"))]
                    {
                        let mut active = active;
                        active.materials = materials;
                        active.material_count = material_count;
                        self.active_rooms[slot] = Some(active);
                    }
                }
            }
            slot += 1;
        }
        self.apply_current_active_room_fields();
    }

    pub(super) fn mark_visible_room_unbuilt(&mut self, index: RoomIndex) {
        #[cfg(feature = "cd-stream-bench")]
        {
            if streamed_room_is_resident(index) {
                self.portal_visible_build_failed =
                    self.portal_visible_build_failed.saturating_add(1);
                self.portal_visible_build_failed_mask |= room_index_debug_mask(index);
            } else if !streamed_room_is_loading(index) {
                self.portal_visible_missing_resident =
                    self.portal_visible_missing_resident.saturating_add(1);
                self.portal_visible_missing_mask |= room_index_debug_mask(index);
            }
        }
        #[cfg(not(feature = "cd-stream-bench"))]
        {
            self.portal_visible_build_failed = self.portal_visible_build_failed.saturating_add(1);
            self.portal_visible_build_failed_mask |= room_index_debug_mask(index);
        }
    }

    pub(super) fn rebuild_active_room_window(&mut self, update_streaming: bool) {
        #[cfg(not(feature = "cd-stream-bench"))]
        let _ = update_streaming;

        telemetry::stage_begin(telemetry::stage::ACTIVE_ROOM_WINDOW);
        telemetry::counter(telemetry::counter::ROOM_WINDOW_REBUILDS, 1);
        let previous_active_rooms = self.active_rooms;
        self.room = None;
        self.current_collision_room = None;
        self.current_ambient_rgb = [0x80, 0x80, 0x80];
        self.materials = [room_material_fallback(); MAX_ROOM_MATERIALS];
        self.material_count = 0;
        self.active_rooms = [const { None }; MAX_ACTIVE_ROOMS];
        self.active_room_candidates = 0;
        self.active_room_cache_skips = 0;
        #[cfg(all(
            feature = "world-grid-visible",
            not(feature = "vis-full-active-chunks")
        ))]
        {
            self.clear_visible_cell_caches();
        }

        let current_index = self.room_index;
        let Some(current_record) = ROOMS.get(current_index.to_usize()) else {
            telemetry::stage_end(telemetry::stage::ACTIVE_ROOM_WINDOW);
            return;
        };
        let player = self.motor.position();
        let view = self.active_room_selection_view();
        let active_limit = room_active_chunk_limit(current_record);
        self.refresh_portal_visibility_for_view(current_index, current_record, view);

        let desired_visible_count = self.portal_visible_room_limit(current_record);
        let mut next_slot = 0usize;
        let mut visible_slot = 0usize;
        self.active_room_anchor = player;

        while visible_slot < desired_visible_count && next_slot < MAX_ACTIVE_ROOMS {
            let index = self.portal_visibility.rooms[visible_slot].room;
            let Some(record) = ROOMS.get(index.to_usize()) else {
                visible_slot += 1;
                continue;
            };
            match reuse_or_build_active_room(
                next_slot,
                index,
                record,
                current_record,
                &previous_active_rooms,
            ) {
                Some(active)
                    if visible_slot == 0
                        || active.render_room.is_some()
                        || active.surface_cache.ready =>
                {
                    if index == current_index {
                        self.room = active.render_room;
                        self.current_collision_room = Some(active.collision_room);
                        self.current_ambient_rgb = active.ambient_rgb;
                        self.set_current_materials(&active);
                    }
                    self.active_rooms[next_slot] = Some(active);
                    next_slot += 1;
                }
                Some(_) => {
                    self.active_room_cache_skips = self.active_room_cache_skips.saturating_add(1);
                }
                None => {
                    self.mark_visible_room_unbuilt(index);
                    if visible_slot == 0 {
                        break;
                    }
                }
            }
            visible_slot += 1;
        }

        if self.current_collision_room.is_none() && next_slot < MAX_ACTIVE_ROOMS {
            if let Some(active) = reuse_or_build_active_room(
                next_slot,
                current_index,
                current_record,
                current_record,
                &previous_active_rooms,
            ) {
                self.room = active.render_room;
                self.current_collision_room = Some(active.collision_room);
                self.current_ambient_rgb = active.ambient_rgb;
                self.set_current_materials(&active);
                self.active_rooms[next_slot] = Some(active);
                next_slot += 1;
            }
        }

        if next_slot == 0 {
            #[cfg(not(feature = "cd-stream-bench"))]
            {
                if let Some(active) = reuse_or_build_active_room(
                    0,
                    current_index,
                    current_record,
                    current_record,
                    &previous_active_rooms,
                ) {
                    self.room = active.render_room;
                    self.current_collision_room = Some(active.collision_room);
                    self.current_ambient_rgb = active.ambient_rgb;
                    self.set_current_materials(&active);
                    self.active_rooms[0] = Some(active);
                    next_slot = 1;
                }
            }
        }

        self.retain_previous_active_rooms(
            &previous_active_rooms,
            current_record,
            active_limit,
            &mut next_slot,
        );

        if self.portal_visibility.room_count == 0 {
            let visibility_space = portal_visibility_space_for_view(current_index, view);
            let visibility_record = ROOMS
                .get(visibility_space.room.to_usize())
                .unwrap_or(current_record);
            self.rebuild_portal_visibility(
                visibility_space.room,
                visibility_record,
                visibility_space.view,
                visibility_space.camera_global,
            );
        }
        if self.portal_visibility.room_count == 0 {
            self.portal_visible_missing_resident = 0;
            self.portal_visible_missing_mask = RuntimeDebugMask::EMPTY;
            self.portal_visible_build_failed = 0;
            self.portal_visible_build_failed_mask = RuntimeDebugMask::EMPTY;
        }
        telemetry::counter(
            telemetry::counter::ROOM_WINDOW_BUILT_CHUNKS,
            next_slot as u32,
        );
        #[cfg(feature = "cd-stream-bench")]
        if update_streaming {
            self.preload_streamed_active_room_window(desired_visible_count, current_record);
        }
        telemetry::stage_end(telemetry::stage::ACTIVE_ROOM_WINDOW);
    }

    #[cfg(feature = "cd-stream-bench")]
    pub(super) fn preload_streamed_active_room_window(
        &mut self,
        desired_visible_count: usize,
        current_record: &LevelRoomRecord,
    ) {
        // Residency is owned by update_room_residency now; this path only
        // builds the active window from whatever the owner made resident.
        let visible_limit = desired_visible_count
            .min(self.portal_visibility.room_count)
            .min(room_active_chunk_limit(current_record));

        let previous_active_rooms = self.active_rooms;
        let mut rebuilt = [const { None }; MAX_ACTIVE_ROOMS];
        let mut next_slot = 0usize;
        let active_limit = room_active_chunk_limit(current_record).min(MAX_ACTIVE_ROOMS);
        let mut visible_slot = 0usize;
        self.portal_visible_missing_resident = 0;
        self.portal_visible_missing_mask = RuntimeDebugMask::EMPTY;
        self.portal_visible_build_failed = 0;
        self.portal_visible_build_failed_mask = RuntimeDebugMask::EMPTY;
        if next_slot < active_limit {
            match reuse_or_build_active_room(
                next_slot,
                self.room_index,
                current_record,
                current_record,
                &previous_active_rooms,
            ) {
                Some(active) => {
                    rebuilt[next_slot] = Some(active);
                    next_slot += 1;
                }
                None => self.mark_visible_room_unbuilt(self.room_index),
            }
        }
        while visible_slot < visible_limit && next_slot < active_limit {
            let index = self.portal_visibility.rooms[visible_slot].room;
            if index == self.room_index {
                visible_slot += 1;
                continue;
            }
            if let Some(record) = ROOMS.get(index.to_usize()) {
                match reuse_or_build_active_room(
                    next_slot,
                    index,
                    record,
                    current_record,
                    &previous_active_rooms,
                ) {
                    Some(active)
                        if visible_slot == 0
                            || active.render_room.is_some()
                            || active.surface_cache.ready =>
                    {
                        rebuilt[next_slot] = Some(active);
                        next_slot += 1;
                    }
                    Some(_) => {
                        self.active_room_cache_skips =
                            self.active_room_cache_skips.saturating_add(1);
                    }
                    None => {
                        self.mark_visible_room_unbuilt(index);
                        if visible_slot == 0 {
                            break;
                        }
                    }
                }
            }
            visible_slot += 1;
        }
        self.active_rooms = rebuilt;
        self.retain_previous_active_rooms(
            &previous_active_rooms,
            current_record,
            active_limit,
            &mut next_slot,
        );
        self.apply_current_active_room_fields();
    }

    #[cfg(feature = "cd-stream-bench")]
    pub(super) fn pump_room_stream(&mut self, max_sectors: usize) -> bool {
        unsafe { ROOM_STREAM_SCHEDULER.pump(&mut STREAMED_ROOM_WORDS, max_sectors) }
    }

    /// The residency owner: computes the single desired resident set -- the
    /// whole level when it fits the budget, otherwise the current room plus its
    /// visible neighbourhood -- and hands it to the scheduler to pin + load.
    /// This is the one place residency is declared; the build paths read
    /// residency from what this makes resident.
    #[cfg(feature = "cd-stream-bench")]
    pub(super) fn update_room_residency(&mut self) {
        // One source of truth: the camera-rooted portal traversal. The resident
        // desired-set is its frustum-visible rooms first (correctness -- anything
        // drawn must be resident), then an unpruned BFS ring from the SAME root
        // (prefetch). The ring radius covers the traversal depth, so
        // resident is a superset of visible by construction; visible-first keeps
        // that true even when the budget cannot hold the whole prefetch ring.
        //
        let mut desired = [INVALID_ROOM_INDEX; STREAMED_ROOM_SLOT_COUNT];
        let mut count = 0usize;
        let visible = self.portal_visibility.room_count.min(MAX_ACTIVE_ROOMS);
        let mut i = 0usize;
        while i < visible && count < STREAMED_ROOM_SLOT_COUNT {
            let room = self.portal_visibility.rooms[i].room;
            if room != INVALID_ROOM_INDEX && !room_requested(room, &desired, count) {
                desired[count] = room;
                count += 1;
            }
            i += 1;
        }
        // Prefetch ring rooted at the camera's room (the visibility root), not
        // the player's. Breadth-first, so the closest hops fill the rest of the
        // budget. Radius = traversal depth + a small margin that also absorbs the
        // one-frame residency lag.
        let resident_radius = RESIDENT_DRAW_DEPTH.saturating_add(RESIDENT_PREFETCH_HOPS);
        let mut ring = [INVALID_ROOM_INDEX; STREAMED_ROOM_SLOT_COUNT];
        let ring_count = room_graph_ring(
            self.portal_visibility_root,
            resident_radius,
            &mut ring,
            STREAMED_ROOM_SLOT_COUNT,
        );
        let mut j = 0usize;
        while j < ring_count && count < STREAMED_ROOM_SLOT_COUNT {
            let room = ring[j];
            if room != INVALID_ROOM_INDEX && !room_requested(room, &desired, count) {
                desired[count] = room;
                count += 1;
            }
            j += 1;
        }
        self.resident_desired = desired;
        self.resident_desired_count = count;
        unsafe { ROOM_STREAM_SCHEDULER.reconcile_residency(&desired, count) };
        // The ring only moves when the camera changes room, so the desired set is
        // stable between crossings; debounce eviction on the camera room (not the
        // player) and let the scheduler LRU absorb visible-set jitter.
        let current = self.portal_visibility_root;
        if unsafe { LAST_EVICT_ROOM } != current {
            evict_unreferenced_vram(&desired, count);
            unsafe { LAST_EVICT_ROOM = current };
        }
    }

    #[cfg(feature = "cd-stream-bench")]
    pub(super) fn bootstrap_streamed_room_window(&mut self) {
        self.update_room_residency();
        self.load_active_room_window();

        let mut steps = 0usize;
        while steps < RUNTIME_SCHEDULE.stream_bootstrap_pump_limit {
            let stream_progress = if streamed_room_stream_active() {
                self.pump_room_stream(RUNTIME_SCHEDULE.stream_pump_sectors_per_tick)
            } else {
                false
            };

            if stream_progress {
                if self.active_room_job.active {
                    self.active_room_job.update_streaming = true;
                } else {
                    self.begin_active_room_window_job(true);
                }
            }

            self.step_active_room_window_job();

            if self.current_collision_room.is_some() && !self.active_room_job.active {
                break;
            }

            if !streamed_room_stream_active() {
                self.update_room_residency();
            }

            steps += 1;
        }

        if self.current_collision_room.is_none() {
            self.load_active_room_window();
        }
    }

    pub(super) fn current_floor_link_sector(&self) -> Option<psx_engine::SectorCollision> {
        let room = self.current_collision_room.as_ref()?.collision();
        let sector_size = room.sector_size();
        if sector_size <= 0 {
            return None;
        }
        let player = self.motor.position();
        if player.x < 0 || player.z < 0 {
            return None;
        }
        let sx = player.x / sector_size;
        let sz = player.z / sector_size;
        if sx < 0 || sz < 0 || sx >= room.width() as i32 || sz >= room.depth() as i32 {
            return None;
        }
        room.sector(sx as u16, sz as u16)
    }

    pub(super) fn current_floor_link_switch_target(&self) -> Option<RoomIndex> {
        let sector = self.current_floor_link_sector()?;
        let player_y = self.motor.position().y;
        let current_origin_y = ROOMS
            .get(self.room_index.to_usize())
            .map(room_origin_y)
            .unwrap_or(0);
        // The motor's Y is current-room-local; lift to global so it can be
        // compared against another room's absolute elevation.
        let global_y = player_y.saturating_add(current_origin_y);

        // Switch floors using a hysteresis band around the boundary
        // between the two rooms (the higher of the two origins). Without
        // it the player thrashes between rooms at the seam: climbing up to
        // the boundary satisfies "below me is a hole" (down) and "I've
        // reached the upper floor" (up) on the same frame. Requiring the
        // player to clear the boundary by FLOOR_LINK_SWITCH_HYSTERESIS in
        // the travel direction makes the transition one-way and stable.
        if let Some(room) = sector.floor_above_room() {
            let boundary = ROOMS.get(room.to_usize()).map(room_origin_y).unwrap_or(0);
            // Climbed clearly up to / past the upper floor's elevation.
            if global_y >= boundary.saturating_sub(FLOOR_LINK_CROSS_EPSILON)
                && self.can_switch_to_floor_link_room(room)
            {
                return Some(room);
            }
        }

        if let Some(room) = sector.floor_below_room() {
            // Boundary is THIS room's own floor elevation; the lower room
            // sits below it. Only drop down when the player has descended
            // CLEARLY below the boundary (by the hysteresis margin). This
            // is what stops climb-thrash: arriving at the boundary from
            // below (global_y ~= boundary) does NOT re-trigger a drop, even
            // on the floorless hole cell you climbed through -- you must
            // actually fall to leave.
            let boundary = current_origin_y;
            let descended = global_y <= boundary.saturating_sub(FLOOR_LINK_SWITCH_HYSTERESIS);
            if descended && self.can_switch_to_floor_link_room(room) {
                return Some(room);
            }
        }

        None
    }

    pub(super) fn can_switch_to_floor_link_room(&self, room: RoomIndex) -> bool {
        if room == self.room_index || room == INVALID_ROOM_INDEX || room.to_usize() >= ROOMS.len() {
            return false;
        }
        #[cfg(feature = "cd-stream-bench")]
        if self.chunked_level() && !streamed_room_is_resident(room) {
            return false;
        }
        true
    }

    pub(super) fn update_current_room_from_player(&mut self) -> bool {
        if !self.chunked_level() {
            return false;
        }
        let global = local_to_global_room_point(self.room_index, self.motor.position());
        let Some(next_room) = self
            .current_floor_link_switch_target()
            .or_else(|| room_index_containing_global_from(self.room_index, global))
        else {
            return false;
        };
        if next_room == self.room_index {
            return false;
        }
        let previous_room = self.room_index;
        let previous_local = self.motor.position();
        let local = global_to_local_room_point(next_room, global);
        let camera_delta = RoomPoint::new(
            local.x.saturating_sub(previous_local.x),
            local.y.saturating_sub(previous_local.y),
            local.z.saturating_sub(previous_local.z),
        );
        let camera_before = RoomPoint::new(
            self.render_camera.position.x,
            self.render_camera.position.y,
            self.render_camera.position.z,
        );
        self.room_index = next_room;
        self.motor.relocate(local);
        self.camera.relocate_room_space(camera_delta);
        self.render_camera.position = WorldVertex::new(
            self.render_camera.position.x.saturating_add(camera_delta.x),
            self.render_camera.position.y.saturating_add(camera_delta.y),
            self.render_camera.position.z.saturating_add(camera_delta.z),
        );
        self.lock_target = None;
        self.lock_switch_stick_held = false;
        self.soft_lock_target = None;
        self.active_interactable = None;
        let camera_after = RoomPoint::new(
            self.render_camera.position.x,
            self.render_camera.position.y,
            self.render_camera.position.z,
        );
        debug_log_room_transition(
            previous_room,
            next_room,
            previous_local,
            local,
            global,
            camera_before,
            camera_after,
        );
        self.load_active_room_window();
        #[cfg(feature = "cd-stream-bench")]
        let loading_mask = unsafe { ROOM_STREAM_SCHEDULER.loading_room_mask() };
        #[cfg(not(feature = "cd-stream-bench"))]
        let loading_mask = RuntimeDebugMask::EMPTY;
        let stats = self.portal_visibility.stats;
        debug_log_room_window_after_cross(
            next_room,
            self.portal_visibility.room_count,
            self.portal_visibility.frontier_count,
            self.portal_visibility.visible_room_mask(),
            self.active_room_mask(),
            self.active_room_drawable_mask(),
            loading_mask,
            self.portal_visible_missing_mask,
            self.portal_visible_build_failed_mask,
            self.room.is_some(),
            self.current_collision_room.is_some(),
            stats.portals_tested,
            stats.portals_accepted,
        );
        self.post_cross_debug_frames = RUNTIME_SCHEDULE.post_cross_render_debug_frames;
        true
    }

    pub(super) fn refresh_active_room_window_if_needed(&mut self) {
        if !self.chunked_level() {
            return;
        }
        let Some(record) = ROOMS.get(self.room_index.to_usize()) else {
            return;
        };
        let sector_size = record.sector_size.max(1);
        let threshold = sector_size.saturating_mul(RUNTIME_SCHEDULE.active_refresh_sectors.max(1));
        let view_threshold = sector_size;
        let player = self.motor.position();
        let view = self.active_room_selection_view();
        let (view_sin_key, view_cos_key, view_pitch_sin_key, view_pitch_cos_key) =
            portal_visibility_view_keys(view);
        let moved_far = point_xz_axis_moved_at_least(player, self.active_room_anchor, threshold);
        let camera_moved_far = point_xyz_axis_moved_at_least(
            view.position,
            self.active_room_view_anchor,
            view_threshold,
        );
        let view_changed = view_sin_key != self.active_room_view_sin_key
            || view_cos_key != self.active_room_view_cos_key
            || view_pitch_sin_key != self.active_room_view_pitch_sin_key
            || view_pitch_cos_key != self.active_room_view_pitch_cos_key;
        if moved_far {
            self.begin_active_room_window_job(true);
            return;
        }
        if !camera_moved_far && !view_changed {
            return;
        }
        self.refresh_portal_visibility_for_view(self.room_index, record, view);
        if !self.active_room_job.active && !self.portal_visible_rooms_are_active(record) {
            self.begin_active_room_window_job(true);
        }
    }

    pub(super) fn force_refresh_active_room_window_view(&mut self) {
        if !self.chunked_level() {
            return;
        }
        let Some(record) = ROOMS.get(self.room_index.to_usize()) else {
            return;
        };
        let view = self.active_room_selection_view();
        self.refresh_portal_visibility_for_view(self.room_index, record, view);
        if !self.active_room_job.active && !self.portal_visible_rooms_are_active(record) {
            self.begin_active_room_window_job(true);
        }
    }
}
