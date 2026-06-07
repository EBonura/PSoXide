use super::*;

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

    pub(super) fn portal_visibility_draws_room(&self, _index: RoomIndex) -> bool {
        // Reachability draw: callers pass rooms from the active camera-ring
        // window, so every active room is drawable. The room renderer still
        // runs projection, screen, near-plane, and backface checks per surface.
        true
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
}
