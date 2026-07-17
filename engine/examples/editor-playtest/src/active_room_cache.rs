//! Glue over `psx_game_runtime::room_cache`: re-exports the window
//! vocabulary, threads the cooked manifest tables into the crate, and
//! keeps the build orchestration (residency, streaming, lighting)
//! whose inputs span the runtime arenas. Streamed-slot resolution
//! lives on `StreamedRoomPages` since the vram_runtime carve; the
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

/// Both remaining callers sit on `not(cd-stream-bench)` branches (the
/// crate's grid queries parse rooms themselves since the phase-2
/// `world_visibility` carve).
#[cfg(not(feature = "cd-stream-bench"))]
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
    let _ = room_reflection_probe_ready(index);
    let payload = parse_active_room_payload(slot, index, record)?;
    let (materials, material_count, _all_resolved) = build_runtime_room_material_table(record);
    let stream_slot = active_room_stream_slot(index);
    #[cfg(feature = "cd-stream-bench")]
    store_room_materials(stream_slot, materials, material_count);
    let surface_cache = active_room_surface_cache_for(index);
    prewarm_active_room_quads(index, surface_cache, &materials[..material_count]);
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

/// Prepare immutable baked-quad packet payloads while a room is being built or
/// its streamed texture bindings change. The render loop then only patches
/// positions and links the packet into the OT.
pub(super) fn prewarm_active_room_quads(
    index: RoomIndex,
    cache: ActiveRoomSurfaceCache,
    materials: &[WorldRenderMaterial],
) {
    let Some((_, _, _, surfaces)) = room_surface_cache_slices(index, cache) else {
        return;
    };
    let (quads, valid) = prebuilt_room_quads_for(index);
    prewarm_indexed_cached_room_quads(surfaces, materials, quads, valid);
}

impl Playtest {
    /// Refill every active room's static packet payload after the menu/gameplay
    /// overlay handoff resets the shared prebuilt-packet claims.
    pub(super) fn prewarm_active_room_window_quads(&mut self) {
        let mut slot = 0usize;
        while slot < MAX_ACTIVE_ROOMS {
            if let Some(active) = self.window.rooms[slot] {
                prewarm_active_room_quads(
                    active.index,
                    active.surface_cache,
                    active_room_materials(&active),
                );
            }
            slot += 1;
        }
    }
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
/// (the `StreamedRoomPages` staleness contract): consume it within the
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
