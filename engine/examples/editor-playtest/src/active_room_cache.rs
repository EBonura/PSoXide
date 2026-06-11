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
    let (materials, material_count, _all_resolved) = build_runtime_room_material_table(record);
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
) -> ([WorldRenderMaterial; MAX_ROOM_MATERIALS], usize, bool) {
    let mut resolved_materials = [const { None }; MAX_ROOM_MATERIALS];
    let (material_count, all_resolved) = build_room_materials(record, &mut resolved_materials);
    let mut materials = [room_material_fallback(); MAX_ROOM_MATERIALS];
    for i in 0..material_count {
        if let Some(material) = resolved_materials[i] {
            materials[i] = material;
        }
    }
    (materials, material_count, all_resolved)
}

#[cfg(feature = "cd-stream-bench")]
pub(super) fn room_material_textures_ready(record: &LevelRoomRecord) -> bool {
    let mut resolved_materials = [const { None }; MAX_ROOM_MATERIALS];
    // `build_room_materials` reports whether every textured material resolved to a
    // ready VRAM slot; that is exactly the readiness condition.
    let (_, all_resolved) = build_room_materials(record, &mut resolved_materials);
    all_resolved
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

/// Prebuilt-quad pool slice + fill flag for `room`, claiming a slot
/// round-robin on first use. `fill == true` means the caller's draw
/// must write the packet skeletons this frame (the slot was just
/// claimed or stolen from another room). With 8 slots and at most
/// `visible_chunk_limit` (6) rooms drawn per frame, a slot claimed
/// this frame cannot be re-stolen before its draw consumes the fill.
pub(super) fn prebuilt_room_quads_for(
    room: RoomIndex,
) -> (&'static mut [QuadTexturedGouraud], bool) {
    unsafe {
        let mut i = 0usize;
        while i < PREBUILT_ROOM_QUAD_SLOTS {
            if PREBUILT_ROOM_QUAD_ROOMS[i] == room {
                return (&mut PREBUILT_ROOM_QUADS[i][..], false);
            }
            i += 1;
        }
        let slot = (PREBUILT_ROOM_QUAD_NEXT as usize) % PREBUILT_ROOM_QUAD_SLOTS;
        PREBUILT_ROOM_QUAD_NEXT = PREBUILT_ROOM_QUAD_NEXT.wrapping_add(1);
        PREBUILT_ROOM_QUAD_ROOMS[slot] = room;
        (&mut PREBUILT_ROOM_QUADS[slot][..], true)
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
