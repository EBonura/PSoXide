//! Glue over `psx_game_runtime::room_window`: threads the cooked room
//! table, the schedule knobs, and the streaming/residency-coupled room
//! builders into the crate-owned [`RoomWindow`] instance held by
//! `Playtest::window`. The residency owner, the full-rebuild and
//! streamed-preload orchestration (both interleave current-room field
//! application with VRAM/residency-coupled builders), the floor-link
//! switching, and the room-crossing logic stay here until those
//! modules move.
//!
//! [`RoomWindow`]: psx_game_runtime::room_window::RoomWindow

use super::*;

/// Count a visible room whose build failed into the visibility
/// diagnostics: resident-but-unparseable rooms are build failures,
/// rooms still on the CD are missing residents (a loading room is
/// neither -- it resolves on its own). Free function (not a `Playtest`
/// method) so the window-rebuild closures can capture the visibility
/// state alone while `RoomWindow` methods hold the window borrow.
fn mark_visible_room_unbuilt(visibility: &mut RuntimeRoomVisibility, index: RoomIndex) {
    #[cfg(feature = "cd-stream-bench")]
    {
        if streamed_room_is_resident(index) {
            visibility.visible_build_failed = visibility.visible_build_failed.saturating_add(1);
            visibility.visible_build_failed_mask |= room_index_debug_mask(index);
        } else if !streamed_room_is_loading(index) {
            visibility.visible_missing_resident =
                visibility.visible_missing_resident.saturating_add(1);
            visibility.visible_missing_mask |= room_index_debug_mask(index);
        }
    }
    #[cfg(not(feature = "cd-stream-bench"))]
    {
        visibility.visible_build_failed = visibility.visible_build_failed.saturating_add(1);
        visibility.visible_build_failed_mask |= room_index_debug_mask(index);
    }
}

impl Playtest {
    pub(super) fn retain_previous_active_rooms(
        &mut self,
        previous_active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
        current_record: &LevelRoomRecord,
        active_limit: usize,
        next_slot: &mut usize,
    ) {
        self.window.retain_previous_rooms(
            ROOMS,
            previous_active_rooms,
            current_record,
            RUNTIME_SCHEDULE.retained_inactive_rooms,
            active_limit,
            next_slot,
            active_room_stream_slot,
        );
    }

    pub(super) fn load_active_room_window(&mut self) {
        self.window.job = ActiveRoomWindowJob::EMPTY;
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
        self.window
            .rebase_to_current_room(ROOMS, current_record, active_room_stream_slot);
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

        let mut requested_rooms = [INVALID_ROOM_INDEX; MAX_ACTIVE_ROOMS];
        requested_rooms[0] = current_index;
        let mut requested_count = 1usize;
        // Pruned draw: the drawn set is the frustum-clipped portal traversal
        // (visibility.result, refreshed just above), which honours the
        // per-room draw_distance far clamp and portal narrowing. Bounded by
        // what the camera can actually see, so long open sightlines cost
        // rooms-in-view, not the whole reachability ring.
        #[cfg(feature = "vis-draw-pruned")]
        {
            let visible = self.visibility.result.room_count.min(MAX_ACTIVE_ROOMS);
            let mut i = 0usize;
            while i < visible && requested_count < MAX_ACTIVE_ROOMS {
                let room = self.visibility.result.rooms[i].room;
                if room != current_index && room != INVALID_ROOM_INDEX {
                    requested_rooms[requested_count] = room;
                    requested_count += 1;
                }
                i += 1;
            }
        }
        // Reachability draw (default): the active/drawn set is the unpruned
        // portal-graph ring around the camera's room (the visibility root),
        // not the frustum-clipped visible set. Side and behind-the-player
        // rooms stay drawn (no pop-in when a portal goes edge-on); per-polygon
        // backface + screen culling still removes the off-screen geometry
        // cheaply.
        #[cfg(not(feature = "vis-draw-pruned"))]
        {
            let mut ring = [INVALID_ROOM_INDEX; MAX_ACTIVE_ROOMS];
            let ring_count = room_graph_ring(
                self.visibility.root,
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
        }

        // Stacked overlaps are visibility additions, not portal-graph edges.
        // Reachability mode above therefore cannot discover them through
        // `room_graph_ring`. Append every remaining visible room explicitly so
        // the active-window job can build the lower geometry that translucent
        // upper floors blend against. Portal rooms already present in the ring
        // are deduplicated here.
        let visible = self.visibility.result.room_count.min(MAX_ACTIVE_ROOMS);
        let mut visible_index = 0usize;
        while visible_index < visible && requested_count < MAX_ACTIVE_ROOMS {
            let room = self.visibility.result.rooms[visible_index].room;
            if room != current_index
                && room != INVALID_ROOM_INDEX
                && !requested_rooms[..requested_count].contains(&room)
            {
                requested_rooms[requested_count] = room;
                requested_count += 1;
            }
            visible_index += 1;
        }

        self.window.begin_job(
            current_index,
            requested_rooms,
            requested_count,
            update_streaming,
            self.motor.position(),
        );
    }

    pub(super) fn step_active_room_window_job(&mut self) {
        if !self.window.job.active {
            return;
        }
        let current_room = self.window.job.current_room;
        if current_room != self.room_index {
            self.window.job = ActiveRoomWindowJob::EMPTY;
            return;
        }
        let Some(current_record) = ROOMS.get(current_room.to_usize()) else {
            self.window.job = ActiveRoomWindowJob::EMPTY;
            return;
        };

        // Residency is owned by update_room_residency now; the build job no
        // longer requests streaming itself, it only builds from resident rooms.

        #[cfg(feature = "cd-stream-bench")]
        let build_blocked =
            |index: RoomIndex| streamed_room_is_loading(index) || !streamed_room_is_resident(index);
        #[cfg(not(feature = "cd-stream-bench"))]
        let build_blocked = |_: RoomIndex| false;

        telemetry::stage_begin(telemetry::stage::ACTIVE_ROOM_WINDOW);
        let step = self.window.step_job(
            ROOMS,
            RUNTIME_SCHEDULE.active_job_builds_per_tick,
            |slot, index, record, previous_rooms| {
                reuse_or_build_active_room(slot, index, record, current_record, previous_rooms)
            },
            build_blocked,
        );
        if step.unbuilt_room != INVALID_ROOM_INDEX {
            self.mark_visible_room_unbuilt(step.unbuilt_room);
        }
        if let Some(active) = step.current_active {
            self.apply_current_active_room(active);
        }

        telemetry::counter(
            telemetry::counter::ROOM_WINDOW_BUILT_CHUNKS,
            step.built as u32,
        );
        telemetry::stage_end(telemetry::stage::ACTIVE_ROOM_WINDOW);

        if self.window.finish_job(
            ROOMS,
            current_record,
            RUNTIME_SCHEDULE.retained_inactive_rooms,
            room_active_chunk_limit(current_record),
            active_room_stream_slot,
        ) {
            self.apply_current_active_room_fields();
            self.room_materials_unresolved = true;
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
            if let Some(active) = self.window.rooms[slot] {
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
        let mats = active_room_materials(active);
        self.material_count = mats.len();
        self.materials[..mats.len()].copy_from_slice(mats);
    }

    /// Rebuild every active room's material table, re-queuing any texture that
    /// was previously dropped (upload queue full) now that the queue may have
    /// drained. Returns `true` while any active room still has an unresolved
    /// (pending or dropped) texture, so the caller keeps pumping until all
    /// resolve instead of stalling once in-flight uploads finish.
    #[inline(never)]
    pub(super) fn refresh_active_room_materials(&mut self) -> bool {
        let mut unresolved = false;
        let mut slot = 0usize;
        while slot < MAX_ACTIVE_ROOMS {
            if let Some(active) = self.window.rooms[slot] {
                if let Some(record) = ROOMS.get(active.index.to_usize()) {
                    // Same room, so its stored table is the right seed: an
                    // unresolved slot keeps the material it last resolved to.
                    let previous = active_room_materials(&active);
                    let (materials, material_count, all_resolved) =
                        build_runtime_room_material_table(record, previous);
                    if !all_resolved {
                        unresolved = true;
                    }
                    let materials_changed =
                        active_room_materials(&active) != &materials[..material_count];
                    if materials_changed {
                        prewarm_active_room_quads(
                            active.index,
                            active.surface_cache,
                            &materials[..material_count],
                        );
                    }
                    #[cfg(feature = "cd-stream-bench")]
                    store_room_materials(active.stream_slot, materials, material_count);
                    #[cfg(not(feature = "cd-stream-bench"))]
                    {
                        let mut active = active;
                        active.materials = materials;
                        active.material_count = material_count;
                        self.window.rooms[slot] = Some(active);
                    }
                }
            }
            slot += 1;
        }
        self.apply_current_active_room_fields();
        unresolved
    }

    pub(super) fn mark_visible_room_unbuilt(&mut self, index: RoomIndex) {
        mark_visible_room_unbuilt(&mut self.visibility, index);
    }

    pub(super) fn rebuild_active_room_window(&mut self, update_streaming: bool) {
        #[cfg(not(feature = "cd-stream-bench"))]
        let _ = update_streaming;

        telemetry::stage_begin(telemetry::stage::ACTIVE_ROOM_WINDOW);
        telemetry::counter(telemetry::counter::ROOM_WINDOW_REBUILDS, 1);
        let previous_active_rooms = self.window.rooms;
        self.room = None;
        self.current_collision_room = None;
        self.current_ambient_rgb = [0x80, 0x80, 0x80];
        self.materials = [room_material_fallback(); MAX_ROOM_MATERIALS];
        self.material_count = 0;
        self.window.rooms = [const { None }; MAX_ACTIVE_ROOMS];
        self.visibility.candidates = 0;
        self.window.cache_skips = 0;
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
        let (visible_rooms, visible_count) = self.visible_room_list(desired_visible_count);
        let visibility = &mut self.visibility;
        let rebuild = self.window.rebuild_from_visible(
            ROOMS,
            &visible_rooms[..visible_count],
            current_index,
            current_record,
            player,
            &previous_active_rooms,
            |slot, index, record, previous_rooms| {
                reuse_or_build_active_room(slot, index, record, current_record, previous_rooms)
            },
            |index| mark_visible_room_unbuilt(visibility, index),
        );
        let mut next_slot = rebuild.next_slot;
        if let Some(active) = rebuild.current_active {
            self.apply_current_active_room(active);
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
                    self.apply_current_active_room(active);
                    self.window.rooms[0] = Some(active);
                    next_slot = 1;
                }
            }
        }

        self.room_materials_unresolved = true;
        self.retain_previous_active_rooms(
            &previous_active_rooms,
            current_record,
            active_limit,
            &mut next_slot,
        );

        if self.visibility.result.room_count == 0 {
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
        if self.visibility.result.room_count == 0 {
            self.visibility.visible_missing_resident = 0;
            self.visibility.visible_missing_mask = RuntimeDebugMask::EMPTY;
            self.visibility.visible_build_failed = 0;
            self.visibility.visible_build_failed_mask = RuntimeDebugMask::EMPTY;
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
        current_record: &'static LevelRoomRecord,
    ) {
        // Residency is owned by update_room_residency now; this path only
        // builds the active window from whatever the owner made resident.
        let visible_limit = desired_visible_count
            .min(self.visibility.result.room_count)
            .min(room_active_chunk_limit(current_record));

        let previous_active_rooms = self.window.rooms;
        let active_limit = room_active_chunk_limit(current_record).min(MAX_ACTIVE_ROOMS);
        self.visibility.visible_missing_resident = 0;
        self.visibility.visible_missing_mask = RuntimeDebugMask::EMPTY;
        self.visibility.visible_build_failed = 0;
        self.visibility.visible_build_failed_mask = RuntimeDebugMask::EMPTY;
        let (visible_rooms, visible_count) = self.visible_room_list(visible_limit);
        let visibility = &mut self.visibility;
        let mut next_slot = self.window.preload_from_visible(
            ROOMS,
            &visible_rooms[..visible_count],
            self.room_index,
            current_record,
            active_limit,
            &previous_active_rooms,
            |slot, index, record, previous_rooms| {
                reuse_or_build_active_room(slot, index, record, current_record, previous_rooms)
            },
            |index| mark_visible_room_unbuilt(visibility, index),
        );
        self.retain_previous_active_rooms(
            &previous_active_rooms,
            current_record,
            active_limit,
            &mut next_slot,
        );
        self.apply_current_active_room_fields();
    }

    /// Copy the portal-visible room list (up to `limit` entries) out of
    /// the visibility result, so window rebuild methods can borrow the
    /// window and the visibility diagnostics disjointly.
    fn visible_room_list(&self, limit: usize) -> ([RoomIndex; MAX_ACTIVE_ROOMS], usize) {
        let mut rooms = [INVALID_ROOM_INDEX; MAX_ACTIVE_ROOMS];
        let count = limit.min(MAX_ACTIVE_ROOMS);
        let mut i = 0usize;
        while i < count {
            rooms[i] = self.visibility.result.rooms[i].room;
            i += 1;
        }
        (rooms, count)
    }

    #[cfg(feature = "cd-stream-bench")]
    #[inline(never)]
    pub(super) fn pump_room_stream(&mut self, max_sectors: usize) -> bool {
        // Behind the loading screen the room read may wait for the sector
        // already on its way; in play it must not, because the frame is due.
        let loading = !self.initial_world_ready();
        room_streams_arena().set_wait_for_sectors(loading);
        room_streams_arena().pump(
            cd_arena(),
            streamed_slots_arena_mut(),
            max_sectors,
            debug_log_stream_entry,
        )
    }

    /// The residency owner: computes the single desired resident set -- the
    /// whole level when it fits the budget, otherwise the current room plus its
    /// visible neighbourhood -- and hands it to the scheduler to pin + load.
    /// This is the one place residency is declared; the build paths read
    /// residency from what this makes resident.
    #[cfg(feature = "cd-stream-bench")]
    #[inline(never)]
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
        let visible = self.visibility.result.room_count.min(MAX_ACTIVE_ROOMS);
        let mut i = 0usize;
        while i < visible && count < STREAMED_ROOM_SLOT_COUNT {
            let room = self.visibility.result.rooms[i].room;
            if room != INVALID_ROOM_INDEX && !room_requested(room, &desired, count) {
                desired[count] = room;
                count += 1;
            }
            i += 1;
        }
        // Everything past the visible prefix is the prefetch ring; the
        // scheduler counts and protects the two classes differently.
        let active_count = count;
        // First look-ahead tier: rooms just beyond the accepted portal walk.
        // These portals already survived camera-facing and clipped-frustum
        // tests, so they are a stronger prediction of where the player/view is
        // heading than an undirected graph neighbour.
        let frontier_count = self
            .visibility
            .result
            .frontier_count
            .min(MAX_PORTAL_FRONTIER_ROOMS);
        let mut frontier_index = 0usize;
        while frontier_index < frontier_count && count < STREAMED_ROOM_SLOT_COUNT {
            let room = self.visibility.result.frontier_rooms[frontier_index].room;
            if room != INVALID_ROOM_INDEX && !room_requested(room, &desired, count) {
                desired[count] = room;
                count += 1;
            }
            frontier_index += 1;
        }
        // Prefetch ring rooted at the camera's room (the visibility root), not
        // the player's. Breadth-first, so the closest hops fill the rest of the
        // budget. Radius = traversal depth + a small margin that also absorbs the
        // one-frame residency lag.
        let resident_radius = RESIDENT_DRAW_DEPTH.saturating_add(RESIDENT_PREFETCH_HOPS);
        let mut ring = [INVALID_ROOM_INDEX; STREAMED_ROOM_SLOT_COUNT];
        let ring_count = room_graph_ring(
            self.visibility.root,
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
        let storage_relocated = room_streams_arena().reconcile_residency(
            cd_arena(),
            streamed_slots_arena_mut(),
            &desired,
            count,
            active_count,
            RUNTIME_SCHEDULE.stream_load_batch_count,
            WORLD_PACK_START_LBA,
            WORLD_PACK_TOC,
            debug_log_stream_plan,
            debug_log_stream_entry,
        );
        if storage_relocated {
            // The page allocator only compacts during this residency phase.
            // Drop every parsed view containing direct pointers before any
            // gameplay/camera work resumes, then synchronously rebuild the
            // current collision room from its new address.
            self.window.rooms = [const { None }; MAX_ACTIVE_ROOMS];
            self.window.job = ActiveRoomWindowJob::EMPTY;
            self.room = None;
            self.current_collision_room = None;
            self.camera_collision_room_count = 0;
            self.camera_rooms_key = (INVALID_ROOM_INDEX, i32::MIN, i32::MIN, 0, 0);
            self.load_active_room_window();
        }
        // The ring only moves when the camera changes room, so the desired set is
        // stable between crossings; debounce eviction on the camera room (not the
        // player) and let the scheduler LRU absorb visible-set jitter.
        evict_unreferenced_vram(self.visibility.root, &desired, count);
        // Same desired set, same residency phase: scope the persistent RAM
        // assets to it so textures page with the window instead of the whole
        // level staying pinned for the run.
        #[cfg(feature = "cd-stream-bench")]
        request_persistent_assets(&desired, count);
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
        self.lock_invalid_ticks = 0;
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
        let loading_mask = room_streams_arena().loading_room_mask();
        #[cfg(not(feature = "cd-stream-bench"))]
        let loading_mask = RuntimeDebugMask::EMPTY;
        let stats = self.visibility.result.stats;
        debug_log_room_window_after_cross(
            next_room,
            self.visibility.result.room_count,
            self.visibility.result.frontier_count,
            self.visibility.result.visible_room_mask(),
            self.active_room_mask(),
            self.active_room_drawable_mask(),
            loading_mask,
            self.visibility.visible_missing_mask,
            self.visibility.visible_build_failed_mask,
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
        let moved_far = point_xz_axis_moved_at_least(player, self.window.anchor, threshold);
        let camera_moved_far = point_xyz_axis_moved_at_least(
            view.position,
            self.visibility.view_anchor,
            view_threshold,
        );
        let view_changed = view_sin_key != self.visibility.view_sin_key
            || view_cos_key != self.visibility.view_cos_key
            || view_pitch_sin_key != self.visibility.view_pitch_sin_key
            || view_pitch_cos_key != self.visibility.view_pitch_cos_key;
        if moved_far {
            self.begin_active_room_window_job(true);
            return;
        }
        if !camera_moved_far && !view_changed {
            return;
        }
        self.refresh_portal_visibility_for_view(self.room_index, record, view);
        if !self.window.job.active && !self.portal_visible_rooms_are_active(record) {
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
        if !self.window.job.active && !self.portal_visible_rooms_are_active(record) {
            self.begin_active_room_window_job(true);
        }
    }
}
