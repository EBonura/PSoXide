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
