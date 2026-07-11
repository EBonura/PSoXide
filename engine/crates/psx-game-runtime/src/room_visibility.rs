//! Portal-expanded room visibility policy, carved out of
//! `editor-playtest`'s `active_room_visibility` module (phase 1,
//! slice 2 of docs/game-runtime-plan.md). [`RoomVisibility`] owns the
//! traversal state the example previously spread across its scene
//! struct; cooked tables (`ROOMS`, `ROOM_PORTALS`, `ROOM_CHUNKS`)
//! arrive as `&'static` psx-level records, capacities as `const N`
//! generic parameters, and example-side policy knobs as plain values.

use crate::room_cache::{active_room_contains_drawable, ActiveRuntimeRoom};
use psx_engine::{telemetry, RoomPoint, WorldCamera, WorldProjection};
use psx_level::portal_visibility::{
    build_portal_visibility_with_room_bounds, PortalRoomBounds, PortalVisibilityCamera,
    PortalVisibilityResult,
};
use psx_level::{
    LevelChunkRecord, LevelRoomPortalRecord, LevelRoomRecord, RoomIndex, RuntimeDebugMask,
};

/// A cooked level is chunked when the manifest carries room chunks.
pub fn chunked_level(room_chunks: &[LevelChunkRecord]) -> bool {
    !room_chunks.is_empty()
}

/// Camera view snapshot that active-room selection and portal
/// visibility run against.
#[derive(Copy, Clone)]
pub struct ActiveRoomView {
    /// Camera position, current-room-local engine units.
    pub position: RoomPoint,
    /// Camera yaw sine, Q12 raw.
    pub sin_yaw: i32,
    /// Camera yaw cosine, Q12 raw.
    pub cos_yaw: i32,
    /// Camera pitch sine, Q12 raw.
    pub sin_pitch: i32,
    /// Camera pitch cosine, Q12 raw.
    pub cos_pitch: i32,
}

impl ActiveRoomView {
    /// Snapshot the selection view from a world camera.
    pub fn from_camera(camera: WorldCamera) -> Self {
        Self {
            position: RoomPoint::new(camera.position.x, camera.position.y, camera.position.z),
            sin_yaw: camera.sin_yaw.raw(),
            cos_yaw: camera.cos_yaw.raw(),
            sin_pitch: camera.sin_pitch.raw(),
            cos_pitch: camera.cos_pitch.raw(),
        }
    }
}

/// Owned portal-visibility runtime state: the latest traversal result
/// plus its root/anchor bookkeeping, the cached static room bounds,
/// and the per-refresh visible-room diagnostics.
///
/// The game supplies its generated budget consts as the generic
/// parameters and keeps one instance wherever it keeps scene state.
pub struct RoomVisibility<
    const MAX_ACTIVE_ROOMS: usize,
    const MAX_PORTAL_FRUSTUMS: usize,
    const MAX_PORTAL_FRONTIER_ROOMS: usize,
    const MAX_PORTAL_ROOM_BOUNDS: usize,
> {
    /// Portal traversal result for the current player/camera room.
    pub result:
        PortalVisibilityResult<MAX_ACTIVE_ROOMS, MAX_PORTAL_FRUSTUMS, MAX_PORTAL_FRONTIER_ROOMS>,
    /// Runtime room used as the root for the latest portal traversal.
    pub root: RoomIndex,
    /// Absolute level-space render camera used by the latest portal traversal.
    pub camera_global: RoomPoint,
    /// Global chunk bounds retained for portal diagnostics and streaming.
    room_bounds: [PortalRoomBounds; MAX_PORTAL_ROOM_BOUNDS],
    /// Cached `room_bounds` length. The bounds are a pure function of the
    /// static cooked geometry (ROOM_VISIBILITY / VISIBILITY_CELLS / ROOMS), so
    /// they are computed once and reused; recomputing them per portal-visibility
    /// refresh was ~74% of the portal-visibility cost.
    room_bounds_count: Option<usize>,
    /// Frustum-visible rooms whose streamed payload was not resident.
    pub visible_missing_resident: u16,
    /// Room mask matching `visible_missing_resident`.
    pub visible_missing_mask: RuntimeDebugMask,
    /// Frustum-visible rooms whose active-room build failed.
    pub visible_build_failed: u16,
    /// Room mask matching `visible_build_failed`.
    pub visible_build_failed_mask: RuntimeDebugMask,
    /// Camera position the latest visibility refresh ran for.
    pub view_anchor: RoomPoint,
    /// Quantised yaw sine of the latest refreshed view.
    pub view_sin_key: i16,
    /// Quantised yaw cosine of the latest refreshed view.
    pub view_cos_key: i16,
    /// Quantised pitch sine of the latest refreshed view.
    pub view_pitch_sin_key: i16,
    /// Quantised pitch cosine of the latest refreshed view.
    pub view_pitch_cos_key: i16,
    /// Portals tested by the latest traversal (selection diagnostics).
    pub candidates: u16,
}

impl<
        const MAX_ACTIVE_ROOMS: usize,
        const MAX_PORTAL_FRUSTUMS: usize,
        const MAX_PORTAL_FRONTIER_ROOMS: usize,
        const MAX_PORTAL_ROOM_BOUNDS: usize,
    >
    RoomVisibility<
        MAX_ACTIVE_ROOMS,
        MAX_PORTAL_FRUSTUMS,
        MAX_PORTAL_FRONTIER_ROOMS,
        MAX_PORTAL_ROOM_BOUNDS,
    >
{
    /// Zeroed state; `const` so the game can keep it in link-time
    /// zero-initialized storage.
    pub const EMPTY: Self = Self {
        result: PortalVisibilityResult::EMPTY,
        root: RoomIndex::ZERO,
        camera_global: RoomPoint::ZERO,
        room_bounds: [PortalRoomBounds::EMPTY; MAX_PORTAL_ROOM_BOUNDS],
        room_bounds_count: None,
        visible_missing_resident: 0,
        visible_missing_mask: RuntimeDebugMask::EMPTY,
        visible_build_failed: 0,
        visible_build_failed_mask: RuntimeDebugMask::EMPTY,
        view_anchor: RoomPoint::ZERO,
        view_sin_key: 0,
        view_cos_key: 0,
        view_pitch_sin_key: 0,
        view_pitch_cos_key: 0,
        candidates: 0,
    };

    /// Global-space visibility anchor for a portal-admitted far room: the
    /// center of the portal that admitted it, nudged half a sector INTO
    /// the room so the grid lookup lands on an interior doorway cell. The
    /// cooked PVS of that cell is, by construction, what is visible of
    /// the room from its doorway -- the user-facing contract: rooms stay
    /// drawn N portal hops down the line as long as the connecting
    /// portals survive the frustum-clipped portal walk.
    ///
    /// Returns `None` when the room has no recorded entry frustum (the
    /// caller then draws every cell through the cached path instead).
    pub fn portal_entry_anchor(
        &self,
        room_portals: &[LevelRoomPortalRecord],
        room: RoomIndex,
        sector_size: i32,
    ) -> Option<RoomPoint> {
        let position = self.result.room_position(room)?;
        let visible = self.result.rooms.get(position)?;
        let frustum = self.result.frustums.get(visible.frustum_first as usize)?;
        if frustum.room != room {
            return None;
        }
        let record = room_portals.get(frustum.source_portal as usize)?;
        let center_x =
            (record.vertex_x[0] + record.vertex_x[1] + record.vertex_x[2] + record.vertex_x[3]) / 4;
        let center_y =
            (record.vertex_y[0] + record.vertex_y[1] + record.vertex_y[2] + record.vertex_y[3]) / 4;
        let center_z =
            (record.vertex_z[0] + record.vertex_z[1] + record.vertex_z[2] + record.vertex_z[3]) / 4;
        // The cooked normal faces the record's SOURCE room. Nudge toward
        // whichever side `room` is on; the walk traverses records in
        // both directions, so check rather than assume orientation.
        let nudge = (sector_size / 2).max(1);
        let sign = if record.destination_room == room {
            -1
        } else if record.source_room == room {
            1
        } else {
            return None;
        };
        Some(RoomPoint::new(
            center_x + sign * (record.normal_x as i32) * nudge,
            center_y + sign * (record.normal_y as i32) * nudge,
            center_z + sign * (record.normal_z as i32) * nudge,
        ))
    }

    /// Rebuild the portal traversal rooted at `current_index`,
    /// refreshing the cached static room bounds on first use.
    ///
    /// `projection` supplies the screen centre/focal/near-plane the
    /// half-FOV tangents derive from, `far_z_limit` caps the record's
    /// authored draw distance, and the portal knobs come from the
    /// game's schedule config. Returns the traversal camera so the
    /// caller's debug snapshot path can log it.
    #[allow(clippy::too_many_arguments)]
    pub fn rebuild(
        &mut self,
        rooms: &'static [LevelRoomRecord],
        room_portals: &'static [LevelRoomPortalRecord],
        current_index: RoomIndex,
        current_record: &LevelRoomRecord,
        view: ActiveRoomView,
        camera_global: RoomPoint,
        projection: WorldProjection,
        far_z_limit: i32,
        portal_min_width_q12: i32,
        portal_max_depth: u8,
        collect_room_bounds: impl FnOnce(&mut [PortalRoomBounds; MAX_PORTAL_ROOM_BOUNDS]) -> usize,
    ) -> PortalVisibilityCamera {
        let half_fov_x_tan_q12 = ((projection.screen_x as i32).saturating_mul(4096)
            / projection.focal_length.max(1))
        .max(1);
        let half_fov_y_tan_q12 = ((projection.screen_y as i32).saturating_mul(4096)
            / projection.focal_length.max(1))
        .max(1);
        let far_z = current_record
            .draw_distance
            .clamp(projection.near_z, far_z_limit);
        self.root = current_index;
        self.camera_global = camera_global;
        telemetry::stage_begin(telemetry::stage::PORTAL_VISIBILITY);
        let camera = PortalVisibilityCamera::new(
            camera_global.x,
            camera_global.y,
            camera_global.z,
            view.sin_yaw,
            view.cos_yaw,
            view.sin_pitch,
            view.cos_pitch,
            projection.near_z,
            far_z,
            half_fov_x_tan_q12,
            half_fov_y_tan_q12,
            portal_min_width_q12,
        );
        // The room bounds are a pure function of the static cooked geometry, so
        // collect them once and reuse the cached length on every later refresh.
        let bounds_count = match self.room_bounds_count {
            Some(count) => count,
            None => {
                let count = collect_room_bounds(&mut self.room_bounds);
                self.room_bounds_count = Some(count);
                count
            }
        };
        build_portal_visibility_with_room_bounds(
            rooms,
            room_portals,
            &self.room_bounds[..bounds_count],
            current_index,
            camera,
            portal_max_depth,
            &mut self.result,
        );
        telemetry::stage_end(telemetry::stage::PORTAL_VISIBILITY);
        camera
    }

    /// Rooms drawable from the latest traversal, capped by the current
    /// room's active-chunk limit and the active-room window capacity.
    pub fn visible_room_limit(&self, active_chunk_limit: usize) -> usize {
        self.result
            .room_count
            .min(active_chunk_limit)
            .min(MAX_ACTIVE_ROOMS)
    }

    /// Whether the current room and every frustum-visible room (up to
    /// `active_chunk_limit`) is present and drawable in the active window.
    pub fn visible_rooms_are_active(
        &self,
        active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
        current_room: RoomIndex,
        active_chunk_limit: usize,
    ) -> bool {
        if !active_room_contains_drawable(active_rooms, current_room, current_room) {
            return false;
        }
        let visible_limit = self.visible_room_limit(active_chunk_limit);
        let mut i = 0usize;
        while i < visible_limit {
            if !active_room_contains_drawable(active_rooms, current_room, self.result.rooms[i].room)
            {
                return false;
            }
            i += 1;
        }
        true
    }

    /// Whether the latest traversal draws `_index`.
    pub fn draws_room(&self, _index: RoomIndex) -> bool {
        // Reachability draw: callers pass rooms from the active camera-ring
        // window, so every active room is drawable. The room renderer still
        // runs projection, screen, near-plane, and backface checks per surface.
        true
    }
}
