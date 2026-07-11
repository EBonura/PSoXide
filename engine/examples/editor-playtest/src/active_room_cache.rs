//! Glue over `psx_game_runtime::room_cache`: re-exports the window
//! vocabulary, threads the cooked manifest tables and VRAM-layout
//! consts into the crate, and keeps the streamed-slot resolution and
//! build orchestration (residency, streaming, lighting) here until
//! those modules move in the next slices. The example holds the crate
//! pool structs as its usual `static mut` instances (phase 1.5 cleans
//! that style up).

use super::*;
use psx_game_runtime::room_cache;

pub(super) use psx_game_runtime::room_cache::{
    active_surface_cache_failed, ActiveRoomCacheStatus, ActiveRoomSurfaceCache, ActiveRuntimeRoom,
};

/// The crate window-job record instantiated with this example's window
/// capacity.
pub(super) type ActiveRoomWindowJob = room_cache::ActiveRoomWindowJob<MAX_ACTIVE_ROOMS>;

/// In-use room-surface materials. Streamed builds read the
/// `stream_slot` pool; non-streamed builds read the inline array.
pub(super) fn active_room_materials(active: &ActiveRuntimeRoom) -> &[WorldRenderMaterial] {
    #[cfg(feature = "cd-stream-bench")]
    {
        active.materials(unsafe { &*core::ptr::addr_of!(ROOM_MATERIAL_POOL) })
    }
    #[cfg(not(feature = "cd-stream-bench"))]
    {
        active.materials()
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

pub(super) fn parse_runtime_room(record: &LevelRoomRecord) -> Option<RuntimeRoom<'static>> {
    room_cache::parse_runtime_room(ASSETS, record)
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

/// This example's untextured fallback, on its shared room tpage.
pub(super) const fn room_material_fallback() -> WorldRenderMaterial {
    room_cache::room_material_fallback(TPAGE_WORD)
}

#[cfg(feature = "cd-stream-bench")]
static mut ROOM_MATERIAL_POOL: room_cache::RoomMaterialPool<STREAMED_ROOM_SLOT_COUNT> =
    room_cache::RoomMaterialPool::new(room_material_fallback());

#[cfg(feature = "cd-stream-bench")]
pub(super) fn store_room_materials(
    stream_slot: u16,
    materials: [WorldRenderMaterial; MAX_ROOM_MATERIALS],
    count: usize,
) {
    let pool = unsafe { &mut *core::ptr::addr_of_mut!(ROOM_MATERIAL_POOL) };
    pool.store(stream_slot, materials, count);
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
    let streamed = streamed_active_room_surface_cache_for(index);
    #[cfg(not(feature = "cd-stream-bench"))]
    let streamed = None;
    room_cache::active_room_surface_cache_for::<MAX_CACHED_ROOM_VERTICES>(
        streamed,
        ROOM_SURFACE_CACHES,
        ROOM_CACHE_CELLS,
        ROOM_CACHE_CELL_VERTICES,
        ROOM_CACHE_VERTICES,
        ROOM_CACHE_SURFACES,
        index,
    )
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

/// Prebuilt-quad pool slices for `room` from the example's static pool
/// instance; the claim policy lives in
/// [`room_cache::PrebuiltRoomQuads::claim`].
pub(super) fn prebuilt_room_quads_for(
    room: RoomIndex,
) -> (&'static mut [QuadTexturedGouraud], &'static mut [u8]) {
    let pool = unsafe { &mut *core::ptr::addr_of_mut!(PREBUILT_ROOM_QUADS) };
    pool.claim(room)
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

    room_cache::generated_room_surface_cache_slices::<MAX_CACHED_ROOM_VERTICES>(
        ROOM_CACHE_CELLS,
        ROOM_CACHE_CELL_VERTICES,
        ROOM_CACHE_VERTICES,
        ROOM_CACHE_SURFACES,
        cache,
    )
}

/// Resolve a streamed room's surface-cache slices DIRECTLY INTO its
/// slot byte buffer, re-validating residency and every chunk-view
/// offset against the cache snapshot first. The result inherits the
/// [`streamed_record_slice`] lifetime contract: consume it within the
/// current render/update step and re-resolve next time; never store
/// the slices.
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

/// LIFETIME CONTRACT (streaming audit finding 3): the returned
/// `&'static [T]` is a lie. It points into a streamed room slot
/// buffer that the scheduler overwrites on eviction/reuse, so it is
/// only valid until the next `RoomStreamScheduler::pump` /
/// `reconcile_residency` call (the next streaming step of the next
/// sim tick). NEVER store it across ticks or cache it in a struct;
/// re-resolve through `streamed_room_surface_cache_slices` /
/// `parse_streamed_compact_collision_room` on every use. Those entry
/// points re-validate slot residency and the chunk-view offsets per
/// call, which is what keeps this cast sound today.
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
