//! Glue over `psx_game_runtime::world_visibility`: re-exports the
//! draw-order and grid-topology vocabulary and threads the cooked
//! manifest tables (bundled as [`WorldTables`]) plus this example's
//! capacity/knob consts into the crate queries, keeping the old
//! call-site signatures.
//!
//! [`WorldTables`]: psx_game_runtime::world_visibility::WorldTables

use super::*;
use psx_game_runtime::world_visibility as wv;

pub(super) use psx_game_runtime::room_cache::{room_origin_x, room_origin_y, room_origin_z};
pub(super) use psx_game_runtime::room_visibility::ActiveRoomView;
pub(super) use psx_game_runtime::world_visibility::{
    active_room_overlaps_collision_window, camera_for_room, chunk_overlaps_collision_window,
    collision_room_collected, point_xyz_axis_moved_at_least, point_xz_axis_moved_at_least,
    portal_visibility_view_keys, CachedRoomDrawOrderMode, PortalVisibilitySpace,
    INVALID_ACTIVE_ROOM_SLOT,
};

/// The cooked world-topology tables bundled for the crate grid queries.
pub(super) fn world_tables() -> wv::WorldTables {
    wv::WorldTables {
        assets: ASSETS,
        rooms: ROOMS,
        room_chunks: ROOM_CHUNKS,
        room_visibility: ROOM_VISIBILITY,
        visibility_cells: VISIBILITY_CELLS,
    }
}

/// Order the active-room window's drawable slots for rendering.
pub(super) fn active_room_draw_order(
    active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    camera: WorldCamera,
    visibility: &RuntimePortalVisibility,
    current_room: RoomIndex,
    mode: CachedRoomDrawOrderMode,
) -> [u8; MAX_ACTIVE_ROOMS] {
    wv::active_room_draw_order(active_rooms, camera, visibility, current_room, mode)
}

/// Map a current-room-local view to the visibility root space.
pub(super) fn portal_visibility_space_for_view(
    current_index: RoomIndex,
    view: ActiveRoomView,
) -> PortalVisibilitySpace {
    wv::portal_visibility_space_for_view::<MAX_PORTAL_ROOM_BOUNDS>(
        world_tables(),
        current_index,
        view,
    )
}

/// Authored (editor) room id owning chunk `index`, if any.
pub(super) fn authored_room_for_chunk(index: RoomIndex) -> Option<u32> {
    wv::authored_room_for_chunk(ROOM_CHUNKS, index)
}

/// Chunk record for room `index`.
pub(super) fn chunk_record_for_room(index: RoomIndex) -> Option<&'static LevelChunkRecord> {
    wv::chunk_record_for_room(ROOM_CHUNKS, index)
}

/// Collect the static portal room bounds over the cooked tables.
pub(super) fn collect_portal_room_bounds(
    out: &mut [PortalRoomBounds; MAX_PORTAL_ROOM_BOUNDS],
) -> usize {
    wv::collect_portal_room_bounds(
        world_tables(),
        PORTAL_ROOM_BOUNDS_MIN_Y,
        PORTAL_ROOM_BOUNDS_MAX_Y,
        out,
    )
}

/// Room containing global `point`, searched outward from `current`.
pub(super) fn room_index_containing_global_from(
    current: RoomIndex,
    point: RoomPoint,
) -> Option<RoomIndex> {
    wv::room_index_containing_global_from::<MAX_PORTAL_ROOM_BOUNDS>(world_tables(), current, point)
}

/// Lift a `room`-local point into global level space.
pub(super) fn local_to_global_room_point(room: RoomIndex, point: RoomPoint) -> RoomPoint {
    wv::local_to_global_room_point(ROOMS, room, point)
}

/// Re-express a global point in `room`'s local frame.
pub(super) fn global_to_local_room_point(room: RoomIndex, point: RoomPoint) -> RoomPoint {
    wv::global_to_local_room_point(ROOMS, room, point)
}
