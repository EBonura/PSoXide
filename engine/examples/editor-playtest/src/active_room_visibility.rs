//! Glue over `psx_game_runtime::room_visibility`: threads the cooked
//! manifest tables, the projection/schedule consts, and cross-module
//! scene state (the active-room window, the player motor) into the
//! crate-owned [`RoomVisibility`] instance held by
//! `Playtest::visibility`. The counter emission at the bottom stays
//! with the example's debug overlays.

use super::*;
use psx_game_runtime::{room_cache, room_visibility};

impl Playtest {
    pub(super) fn chunked_level(&self) -> bool {
        room_visibility::chunked_level(ROOM_CHUNKS)
    }

    pub(super) fn active_room_selection_view(&self) -> ActiveRoomView {
        ActiveRoomView::from_camera(self.render_camera)
    }

    /// Portal entry anchor expressed in the player's current-room space.
    ///
    /// [`RoomVisibility::portal_entry_anchor`] resolves portal records in
    /// global level coordinates. The visible-cell cache and active-room
    /// offsets use the current room as their shared origin, so rebase exactly
    /// once here before the caller subtracts the target room's offset.
    pub(super) fn portal_entry_anchor(
        &self,
        room: RoomIndex,
        sector_size: i32,
    ) -> Option<RoomPoint> {
        self.visibility
            .portal_entry_anchor(ROOM_PORTALS, room, sector_size)
            .map(|global| global_to_local_room_point(self.room_index, global))
    }

    pub(super) fn rebuild_portal_visibility(
        &mut self,
        current_index: RoomIndex,
        current_record: &LevelRoomRecord,
        view: ActiveRoomView,
        camera_global: RoomPoint,
    ) {
        let camera = self.visibility.rebuild(
            ROOMS,
            ROOM_PORTALS,
            current_index,
            current_record,
            view,
            camera_global,
            PROJECTION,
            FAR_Z,
            RUNTIME_SCHEDULE.portal_min_width_q12,
            RUNTIME_SCHEDULE.portal_max_depth,
            collect_portal_room_bounds,
        );
        if PORTAL_VIS_DEBUG_LOGS
            && self.portal_debug_log_cooldown == 0
            && should_debug_log_portal_visibility(current_record, &self.visibility.result)
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
                &self.visibility.result,
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
        self.visibility.view_sin_key = view_sin_key;
        self.visibility.view_cos_key = view_cos_key;
        self.visibility.view_pitch_sin_key = view_pitch_sin_key;
        self.visibility.view_pitch_cos_key = view_pitch_cos_key;
        self.visibility.view_anchor = view.position;
        self.rebuild_portal_visibility(
            visibility_index,
            visibility_record,
            visibility_space.view,
            visibility_space.camera_global,
        );
        self.visibility.candidates = self.visibility.result.stats.portals_tested.min(u16::MAX);
        self.visibility
            .include_overlapped_rooms(ROOMS, ROOM_OVERLAPPED_ROOMS);
        self.visibility.visible_missing_resident = 0;
        self.visibility.visible_missing_mask = RuntimeDebugMask::EMPTY;
        self.visibility.visible_build_failed = 0;
        self.visibility.visible_build_failed_mask = RuntimeDebugMask::EMPTY;
        self.active_window_dirty = true;
    }

    pub(super) fn portal_visible_room_limit(&self, current_record: &LevelRoomRecord) -> usize {
        self.visibility
            .visible_room_limit(room_active_chunk_limit(current_record))
    }

    pub(super) fn portal_visible_rooms_are_active(&self, current_record: &LevelRoomRecord) -> bool {
        self.window.visible_rooms_are_active(
            &self.visibility,
            self.room_index,
            room_active_chunk_limit(current_record),
        )
    }

    pub(super) fn active_room_mask(&self) -> RuntimeDebugMask {
        room_cache::active_room_mask(&self.window.rooms)
    }

    pub(super) fn active_room_drawable_mask(&self) -> RuntimeDebugMask {
        room_cache::active_room_drawable_mask(&self.window.rooms, self.room_index)
    }

    pub(super) fn portal_visibility_draws_room(&self, index: RoomIndex) -> bool {
        self.visibility.draws_room(index)
    }

    /// Conservative component-wise union of every clipped portal path that
    /// reaches `room`. Never return the first aperture alone: rooms may be
    /// visible through multiple disjoint paths.
    pub(super) fn portal_cell_window(&self, room: RoomIndex) -> Option<PortalCellWindow> {
        if room == self.visibility.root {
            return None;
        }
        let mut union: Option<PortalCellWindow> = None;
        for frustum in self.visibility.result.frustums[..self
            .visibility
            .result
            .frustum_count
            .min(self.visibility.result.frustums.len())]
            .iter()
            .copied()
        {
            if frustum.room != room {
                continue;
            }
            let path = PortalCellWindow::new(
                frustum.left_tan_q12,
                frustum.right_tan_q12,
                frustum.min_y_tan_q12,
                frustum.max_y_tan_q12,
            );
            union = Some(match union {
                None => path,
                Some(window) => window.union(path),
            });
        }
        union
    }

    pub(super) fn emit_portal_visibility_counters(&self) {
        let stats = self.visibility.result.stats;
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_CURRENT_ROOM,
            self.visibility.root.raw() as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_VISIBLE_ROOMS,
            self.visibility.result.room_count as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_FRONTIER_ROOMS,
            self.visibility.result.frontier_count as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_FRUSTUMS,
            self.visibility.result.frustum_count as u32,
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
            self.visibility.visible_missing_resident as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_VISIBLE_BUILD_FAILED,
            self.visibility.visible_build_failed as u32,
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
            self.visibility.result.visible_room_mask(),
        );
        emit_room_chunk_mask(
            telemetry::counter::PORTAL_VIS_FRONTIER_MASK_LO,
            telemetry::counter::PORTAL_VIS_FRONTIER_MASK_HI,
            self.visibility.result.frontier_room_mask(),
        );
        emit_room_chunk_mask(
            telemetry::counter::PORTAL_VIS_MISSING_MASK_LO,
            telemetry::counter::PORTAL_VIS_MISSING_MASK_HI,
            self.visibility.visible_missing_mask,
        );
        emit_room_chunk_mask(
            telemetry::counter::PORTAL_VIS_BUILD_FAILED_MASK_LO,
            telemetry::counter::PORTAL_VIS_BUILD_FAILED_MASK_HI,
            self.visibility.visible_build_failed_mask,
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
}
