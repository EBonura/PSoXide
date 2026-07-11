//! Glue over `psx_game_runtime::room_cache`: re-exports the window
//! vocabulary, threads the cooked manifest tables into the crate, and
//! keeps the build orchestration (residency, streaming, lighting)
//! whose inputs span the runtime arenas. Streamed-slot resolution
//! lives on `StreamedRoomSlots` since the vram_runtime carve; the
//! crate pool structs are arena-owned since phase 1.5 (see
//! `runtime_arenas`).

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
        active.materials(room_materials_arena())
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
pub(super) fn store_room_materials(
    stream_slot: u16,
    materials: [WorldRenderMaterial; MAX_ROOM_MATERIALS],
    count: usize,
) {
    room_materials_arena_mut().store(stream_slot, materials, count);
}

pub(super) fn build_active_room(
    slot: usize,
    index: RoomIndex,
    record: &LevelRoomRecord,
    current_record: &LevelRoomRecord,
) -> Option<ActiveRuntimeRoom> {
    if let Some(residency) = ROOM_RESIDENCY.iter().find(|r| r.room == index) {
        ensure_room_resident(residency);
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
    if let Some(previous) =
        room_cache::reuse_active_room(previous_rooms, index, stream_slot, record, current_record)
    {
        return Some(previous);
    }
    build_active_room(slot, index, record, current_record)
}

pub(super) fn active_room_stream_slot(index: RoomIndex) -> u16 {
    #[cfg(feature = "cd-stream-bench")]
    {
        room_streams_arena().resident_stream_slot(index)
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
    streamed_slots_arena()
        .surface_cache_for::<MAX_CACHED_ROOM_VERTICES, _>(room_streams_arena(), index)
}

/// Prebuilt-quad pool slices for `room` from the arena-owned pool
/// instance; the claim policy lives in
/// [`room_cache::PrebuiltRoomQuads::claim`].
pub(super) fn prebuilt_room_quads_for(
    room: RoomIndex,
) -> (&'static mut [QuadTexturedGouraud], &'static mut [u8]) {
    prebuilt_quads_arena().claim(room)
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
/// offset against the cache snapshot first. The `'static` on the
/// result comes from borrowing the arena-owned slot-buffer instance
/// (the `StreamedRoomSlots` staleness contract): consume it within the
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
    streamed_slots_arena().surface_cache_slices::<MAX_CACHED_ROOM_VERTICES, _>(
        room_streams_arena(),
        index,
        cache,
    )
}
