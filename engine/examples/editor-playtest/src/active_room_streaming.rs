use super::*;

#[cfg(feature = "cd-stream-bench")]
#[derive(Copy, Clone)]
struct StreamedRoomSlot {
    pub(super) room: RoomIndex,
    byte_count: usize,
    last_used: u32,
    state: RoomStreamSlotState,
}

#[cfg(feature = "cd-stream-bench")]
impl StreamedRoomSlot {
    pub(super) const EMPTY: Self = Self {
        room: INVALID_ROOM_INDEX,
        byte_count: 0,
        last_used: 0,
        state: RoomStreamSlotState::Empty,
    };
}

#[cfg(feature = "cd-stream-bench")]
#[derive(Copy, Clone, PartialEq, Eq)]
enum RoomStreamSlotState {
    Empty,
    Resident,
    Loading,
    Failed,
}

/// First retry comes after 16 reconcile windows (~0.27s at 60Hz);
/// each consecutive failure doubles the hold up to 16 << 5 = 512
/// windows (~8.5s). A success resets the count, so transient CD
/// hiccups recover fast while a permanently bad chunk settles into
/// one retry every few seconds instead of one per frame.
#[cfg(feature = "cd-stream-bench")]
const STREAM_RETRY_BACKOFF_BASE_WINDOWS: u32 = 16;
#[cfg(feature = "cd-stream-bench")]
const STREAM_RETRY_BACKOFF_MAX_SHIFT: u32 = 5;

#[cfg(feature = "cd-stream-bench")]
fn stream_retry_backoff_windows(count: u8) -> u32 {
    let shift = (count.saturating_sub(1) as u32).min(STREAM_RETRY_BACKOFF_MAX_SHIFT);
    STREAM_RETRY_BACKOFF_BASE_WINDOWS << shift
}

#[cfg(feature = "cd-stream-bench")]
#[derive(Copy, Clone)]
pub(super) struct RoomStreamLoadPlan<const N: usize> {
    pub(super) rooms: [RoomIndex; N],
    pub(super) slots: [usize; N],
    pub(super) count: usize,
}

#[cfg(feature = "cd-stream-bench")]
impl<const N: usize> RoomStreamLoadPlan<N> {
    pub(super) const EMPTY: Self = Self {
        rooms: [INVALID_ROOM_INDEX; N],
        slots: [usize::MAX; N],
        count: 0,
    };
}

#[cfg(feature = "cd-stream-bench")]
pub(super) struct RoomStreamScheduler<const N: usize> {
    slots: [StreamedRoomSlot; N],
    room_slots: [u16; MAX_STREAMED_ROOM_INDEX_COUNT],
    /// Rooms declared part of the resident window via `set_resident_window`.
    /// Pinned rooms are never chosen for eviction regardless of LRU age, so the
    /// residency owner can keep them resident without re-requesting them. This
    /// is the primitive both policies build on: full-residency pins every room,
    /// a sliding window pins the current room plus its near neighbours.
    pinned_rooms: [bool; MAX_STREAMED_ROOM_INDEX_COUNT],
    pub(super) job: cd_stream::WorldRoomSlotsReadJob<N>,
    job_plan: RoomStreamLoadPlan<N>,
    slot_limit: usize,
    epoch: u32,
    /// Consecutive failed loads per room index, saturating; reset by a
    /// successful load. Drives the retry backoff so a permanently bad
    /// chunk (TOC mismatch, unreadable sector) cannot churn the CD and
    /// the single job pipeline every frame (streaming audit finding 2).
    failure_counts: [u8; MAX_STREAMED_ROOM_INDEX_COUNT],
    /// Epoch until which a previously failed room is not rescheduled
    /// (exclusive, wrap-safe compare). The room is never abandoned:
    /// the hold doubles per consecutive failure up to a cap, then
    /// retries keep coming at the capped interval.
    failure_hold_until: [u32; MAX_STREAMED_ROOM_INDEX_COUNT],
    window_requests: u16,
    window_misses: u16,
    window_prefetch_requests: u16,
    window_evictions: u16,
    window_failed_loads: u16,
    window_pending_loads: u16,
    window_protected_full: u16,
}

#[cfg(feature = "cd-stream-bench")]
impl<const N: usize> RoomStreamScheduler<N> {
    pub(super) const fn new() -> Self {
        Self {
            slots: [StreamedRoomSlot::EMPTY; N],
            room_slots: [STREAMED_ROOM_SLOT_NONE; MAX_STREAMED_ROOM_INDEX_COUNT],
            pinned_rooms: [false; MAX_STREAMED_ROOM_INDEX_COUNT],
            job: cd_stream::WorldRoomSlotsReadJob::new(),
            job_plan: RoomStreamLoadPlan::EMPTY,
            slot_limit: N,
            epoch: 0,
            failure_counts: [0; MAX_STREAMED_ROOM_INDEX_COUNT],
            failure_hold_until: [0; MAX_STREAMED_ROOM_INDEX_COUNT],
            window_requests: 0,
            window_misses: 0,
            window_prefetch_requests: 0,
            window_evictions: 0,
            window_failed_loads: 0,
            window_pending_loads: 0,
            window_protected_full: 0,
        }
    }

    pub(super) fn effective_slot_limit(&self) -> usize {
        self.slot_limit.clamp(1, N)
    }

    pub(super) fn is_room_pinned(&self, room: RoomIndex) -> bool {
        let index = room.to_usize();
        index < MAX_STREAMED_ROOM_INDEX_COUNT && self.pinned_rooms[index]
    }

    /// Declare the rooms that must stay resident. They are pinned (never
    /// evicted) so they survive without being re-requested; rooms no longer in
    /// the set are unpinned and become evictable again.
    pub(super) fn set_resident_window(&mut self, rooms: &[RoomIndex], count: usize) {
        self.pinned_rooms = [false; MAX_STREAMED_ROOM_INDEX_COUNT];
        let mut i = 0usize;
        while i < count {
            let index = rooms[i].to_usize();
            if index < MAX_STREAMED_ROOM_INDEX_COUNT {
                self.pinned_rooms[index] = true;
            }
            i += 1;
        }
    }

    /// Single residency entry point: pin the desired set and load whatever is
    /// missing. Called once per frame by the residency owner so residency is
    /// no longer requested ad-hoc from the build paths.
    pub(super) fn reconcile_residency(
        &mut self,
        desired: &[RoomIndex; STREAMED_ROOM_SLOT_COUNT],
        count: usize,
    ) {
        self.begin_window();
        self.set_resident_window(desired, count);
        let plan = self.plan_window_loads(desired, count, count);
        self.start_load_plan(plan);
        self.emit_counters();
    }

    pub(super) fn begin_window(&mut self) {
        self.epoch = self.epoch.wrapping_add(1).max(1);
        self.window_requests = 0;
        self.window_misses = 0;
        self.window_prefetch_requests = 0;
        self.window_evictions = 0;
        self.window_failed_loads = 0;
        self.window_pending_loads = 0;
        self.window_protected_full = 0;
    }

    pub(super) fn resident_slot_for(&mut self, room: RoomIndex) -> Option<usize> {
        if let Some(slot) = self.mapped_slot_for(room, RoomStreamSlotState::Resident) {
            self.slots[slot].last_used = self.epoch;
            return Some(slot);
        }
        None
    }

    pub(super) fn is_resident(&self, room: RoomIndex) -> bool {
        self.mapped_slot_for(room, RoomStreamSlotState::Resident)
            .is_some()
    }

    pub(super) fn resident_byte_count(&self, slot: usize) -> Option<usize> {
        if slot >= self.effective_slot_limit() {
            return None;
        }
        let meta = *self.slots.get(slot)?;
        if meta.state == RoomStreamSlotState::Resident && meta.byte_count > 0 {
            Some(meta.byte_count)
        } else {
            None
        }
    }

    pub(super) fn loading_slot_for(&self, room: RoomIndex) -> Option<usize> {
        self.mapped_slot_for(room, RoomStreamSlotState::Loading)
    }

    pub(super) fn is_loading(&self, room: RoomIndex) -> bool {
        self.loading_slot_for(room).is_some()
    }

    /// True while `room` is inside its failure-retry hold window.
    /// Wrap-safe: holds shorter than 2^31 epochs compare correctly
    /// across the u32 epoch wrap.
    fn room_failure_hold_active(&self, room: RoomIndex) -> bool {
        let index = room.to_usize();
        if index >= MAX_STREAMED_ROOM_INDEX_COUNT || self.failure_counts[index] == 0 {
            return false;
        }
        self.failure_hold_until[index].wrapping_sub(self.epoch) as i32 > 0
    }

    fn note_room_load_success(&mut self, room: RoomIndex) {
        let index = room.to_usize();
        if index < MAX_STREAMED_ROOM_INDEX_COUNT {
            self.failure_counts[index] = 0;
            self.failure_hold_until[index] = self.epoch;
        }
    }

    fn note_room_load_failure(&mut self, room: RoomIndex) {
        let index = room.to_usize();
        if index >= MAX_STREAMED_ROOM_INDEX_COUNT {
            return;
        }
        let count = self.failure_counts[index].saturating_add(1);
        self.failure_counts[index] = count;
        self.failure_hold_until[index] = self
            .epoch
            .wrapping_add(stream_retry_backoff_windows(count));
    }

    fn mapped_slot_for(&self, room: RoomIndex, state: RoomStreamSlotState) -> Option<usize> {
        let room_index = room.to_usize();
        if room_index >= MAX_STREAMED_ROOM_INDEX_COUNT {
            return None;
        }
        let slot = self.room_slots[room_index] as usize;
        if slot >= self.effective_slot_limit() {
            return None;
        }
        let meta = self.slots[slot];
        if meta.room == room && meta.state == state {
            Some(slot)
        } else {
            None
        }
    }

    fn set_slot(&mut self, slot: usize, meta: StreamedRoomSlot) {
        if slot >= N {
            return;
        }
        let old_room = self.slots[slot].room.to_usize();
        if old_room < MAX_STREAMED_ROOM_INDEX_COUNT && self.room_slots[old_room] as usize == slot {
            self.room_slots[old_room] = STREAMED_ROOM_SLOT_NONE;
        }
        self.slots[slot] = meta;
        let new_room = meta.room.to_usize();
        if meta.state != RoomStreamSlotState::Empty && new_room < MAX_STREAMED_ROOM_INDEX_COUNT {
            self.room_slots[new_room] = slot as u16;
        }
    }

    pub(super) fn plan_window_loads(
        &mut self,
        requested_rooms: &[RoomIndex; STREAMED_ROOM_SLOT_COUNT],
        requested_count: usize,
        active_count: usize,
    ) -> RoomStreamLoadPlan<N> {
        let mut plan = RoomStreamLoadPlan::EMPTY;
        if requested_count > 0 && !self.current_room_request_can_wait(requested_rooms[0]) {
            self.abort_active_load();
        }
        let can_schedule_new_loads = !self.job.is_active();
        let protected_count = active_count
            .min(requested_count)
            .min(self.effective_slot_limit())
            .min(N)
            .min(STREAMED_ROOM_SLOT_COUNT);
        let limit = requested_count
            .min(self.effective_slot_limit())
            .min(N)
            .min(STREAMED_ROOM_SLOT_COUNT);
        let mut i = 0usize;
        while i < limit {
            let room = requested_rooms[i];
            if room == INVALID_ROOM_INDEX {
                i += 1;
                continue;
            }
            self.window_requests = self.window_requests.saturating_add(1);
            if i >= active_count {
                self.window_prefetch_requests = self.window_prefetch_requests.saturating_add(1);
            }
            if self.resident_slot_for(room).is_some() {
                i += 1;
                continue;
            }
            if self.loading_slot_for(room).is_some() {
                self.window_misses = self.window_misses.saturating_add(1);
                self.window_pending_loads = self.window_pending_loads.saturating_add(1);
                i += 1;
                continue;
            }

            self.window_misses = self.window_misses.saturating_add(1);
            if self.room_failure_hold_active(room) {
                i += 1;
                continue;
            }
            if !can_schedule_new_loads {
                i += 1;
                continue;
            }
            if plan.count >= RUNTIME_SCHEDULE.stream_load_batch_count {
                i += 1;
                continue;
            }
            let allow_eviction = i < protected_count;
            let Some(target) = self.choose_slot(
                requested_rooms,
                protected_count,
                &plan.slots,
                plan.count,
                allow_eviction,
            ) else {
                self.window_protected_full = self.window_protected_full.saturating_add(1);
                i += 1;
                continue;
            };
            if self.slots[target].state == RoomStreamSlotState::Resident {
                self.window_evictions = self.window_evictions.saturating_add(1);
            }
            self.set_slot(
                target,
                StreamedRoomSlot {
                    room,
                    byte_count: 0,
                    last_used: self.epoch,
                    state: RoomStreamSlotState::Loading,
                },
            );
            plan.rooms[plan.count] = room;
            plan.slots[plan.count] = target;
            plan.count += 1;
            self.window_pending_loads = self.window_pending_loads.saturating_add(1);
            i += 1;
        }
        plan
    }

    pub(super) fn current_room_request_can_wait(&self, room: RoomIndex) -> bool {
        room == INVALID_ROOM_INDEX
            || self.is_resident(room)
            || self.is_loading(room)
            || !self.job.is_active()
    }

    pub(super) fn abort_active_load(&mut self) {
        if !self.job.is_active() {
            return;
        }
        debug_log_stream_plan("stream abort", &self.job_plan);
        self.job.abort();
        let plan = self.job_plan;
        let mut i = 0usize;
        while i < plan.count.min(N).min(STREAMED_ROOM_SLOT_COUNT) {
            let slot = plan.slots[i];
            if slot < N
                && self.slots[slot].state == RoomStreamSlotState::Loading
                && self.slots[slot].room == plan.rooms[i]
            {
                self.set_slot(slot, StreamedRoomSlot::EMPTY);
            }
            i += 1;
        }
        self.job_plan = RoomStreamLoadPlan::EMPTY;
    }

    pub(super) fn start_load_plan(&mut self, plan: RoomStreamLoadPlan<N>) {
        if plan.count == 0 || self.job.is_active() {
            return;
        }
        debug_log_stream_plan("stream start", &plan);
        let mut room_ids = [u16::MAX; N];
        let mut i = 0usize;
        while i < plan.count.min(N) {
            room_ids[i] = plan.rooms[i].raw();
            i += 1;
        }
        self.job.start::<STREAMED_ROOM_SLOT_BYTES>(
            WORLD_PACK_START_LBA,
            WORLD_PACK_TOC,
            &room_ids[..plan.count],
            &plan.slots[..plan.count],
        );
        self.job_plan = plan;
        if self.job.is_done() {
            self.commit_completed_job();
        }
    }

    pub(super) fn pump(
        &mut self,
        dst: &mut [[u32; STREAMED_ROOM_SLOT_WORDS]; N],
        max_sectors: usize,
    ) -> bool {
        if !self.job.is_active() {
            return false;
        }
        self.job
            .poll_words::<STREAMED_ROOM_SLOT_WORDS>(dst, max_sectors);
        let committed = self.commit_ready_job_entries();
        if self.job.is_done() {
            self.commit_completed_job();
            true
        } else {
            committed
        }
    }

    pub(super) fn commit_ready_job_entries(&mut self) -> bool {
        let completed = self.job.completed_entries();
        let byte_counts = *self.job.byte_counts();
        let plan = self.job_plan;
        let mut committed = false;
        let mut i = 0usize;
        while i < plan.count.min(N).min(STREAMED_ROOM_SLOT_COUNT) {
            if !completed[i] {
                i += 1;
                continue;
            }
            let target = plan.slots[i];
            if target < N
                && self.slots[target].state == RoomStreamSlotState::Loading
                && self.slots[target].room == plan.rooms[i]
            {
                self.set_slot(
                    target,
                    StreamedRoomSlot {
                        room: plan.rooms[i],
                        byte_count: byte_counts[i],
                        last_used: self.epoch,
                        state: RoomStreamSlotState::Resident,
                    },
                );
                committed = true;
            }
            i += 1;
        }
        committed
    }

    pub(super) fn commit_completed_job(&mut self) {
        let byte_counts = *self.job.byte_counts();
        let statuses = *self.job.statuses();
        let plan = self.job_plan;
        self.commit_window_loads(&plan, &byte_counts, &statuses);
        self.job = cd_stream::WorldRoomSlotsReadJob::new();
        self.job_plan = RoomStreamLoadPlan::EMPTY;
    }

    pub(super) fn commit_window_loads(
        &mut self,
        plan: &RoomStreamLoadPlan<N>,
        byte_counts: &[usize; N],
        statuses: &[u32; N],
    ) {
        let mut loaded = 0usize;
        while loaded < plan.count.min(N).min(STREAMED_ROOM_SLOT_COUNT) {
            let target = plan.slots[loaded];
            if target >= N {
                loaded += 1;
                continue;
            }
            if statuses[loaded] == cd_stream::ROOM_CHUNK_STATUS_OK && byte_counts[loaded] > 0 {
                self.set_slot(
                    target,
                    StreamedRoomSlot {
                        room: plan.rooms[loaded],
                        byte_count: byte_counts[loaded],
                        last_used: self.epoch,
                        state: RoomStreamSlotState::Resident,
                    },
                );
                self.note_room_load_success(plan.rooms[loaded]);
                debug_log_stream_entry(
                    "stream loaded",
                    plan.rooms[loaded],
                    target,
                    byte_counts[loaded],
                    statuses[loaded],
                );
            } else if self.slots[target].state == RoomStreamSlotState::Resident
                && self.slots[target].room == plan.rooms[loaded]
            {
                // The chunk completed and was early-committed by
                // `commit_ready_job_entries` before a LATER group's
                // error ran `fail_all`, which clobbers every entry's
                // status including already-verified ones. The slot
                // holds checksum-verified bytes; do not demote it or
                // charge its failure backoff for another chunk's
                // error (streaming audit phase 2).
                debug_log_stream_entry(
                    "stream kept (late fail_all)",
                    plan.rooms[loaded],
                    target,
                    self.slots[target].byte_count,
                    statuses[loaded],
                );
            } else {
                self.set_slot(
                    target,
                    StreamedRoomSlot {
                        room: plan.rooms[loaded],
                        byte_count: 0,
                        last_used: self.epoch,
                        state: RoomStreamSlotState::Failed,
                    },
                );
                self.window_failed_loads = self.window_failed_loads.saturating_add(1);
                self.note_room_load_failure(plan.rooms[loaded]);
                debug_log_stream_entry(
                    "stream failed",
                    plan.rooms[loaded],
                    target,
                    byte_counts[loaded],
                    statuses[loaded],
                );
            }
            loaded += 1;
        }
    }

    pub(super) fn emit_counters(&self) {
        telemetry::counter(
            telemetry::counter::ROOM_STREAM_REQUESTS,
            self.window_requests as u32,
        );
        telemetry::counter(
            telemetry::counter::ROOM_STREAM_MISSES,
            self.window_misses as u32,
        );
        telemetry::counter(
            telemetry::counter::ROOM_STREAM_PREFETCH_REQUESTS,
            self.window_prefetch_requests as u32,
        );
        telemetry::counter(
            telemetry::counter::ROOM_STREAM_RESIDENT_SLOTS,
            self.resident_slot_count() as u32,
        );
        telemetry::counter(
            telemetry::counter::ROOM_STREAM_SLOT_LIMIT,
            self.effective_slot_limit() as u32,
        );
        emit_room_chunk_mask(
            telemetry::counter::ROOM_STREAM_LOADING_MASK_LO,
            telemetry::counter::ROOM_STREAM_LOADING_MASK_HI,
            self.loading_room_mask(),
        );
        emit_room_chunk_mask(
            telemetry::counter::ROOM_STREAM_RESIDENT_MASK_LO,
            telemetry::counter::ROOM_STREAM_RESIDENT_MASK_HI,
            self.resident_room_mask(),
        );
        telemetry::counter(
            telemetry::counter::ROOM_STREAM_EVICTIONS,
            self.window_evictions as u32,
        );
        telemetry::counter(
            telemetry::counter::ROOM_STREAM_FAILED_LOADS,
            self.window_failed_loads as u32,
        );
        telemetry::counter(
            telemetry::counter::ROOM_STREAM_PENDING_LOADS,
            self.window_pending_loads as u32,
        );
        telemetry::counter(
            telemetry::counter::ROOM_STREAM_PROTECTED_FULL,
            self.window_protected_full as u32,
        );
    }

    pub(super) fn resident_slot_count(&self) -> usize {
        let mut count = 0usize;
        let mut slot = 0usize;
        let limit = self.effective_slot_limit();
        while slot < limit {
            if self.slots[slot].state == RoomStreamSlotState::Resident {
                count += 1;
            }
            slot += 1;
        }
        count
    }

    pub(super) fn resident_room_mask(&self) -> RuntimeDebugMask {
        let mut mask = RuntimeDebugMask::EMPTY;
        let mut slot = 0usize;
        let limit = self.effective_slot_limit();
        while slot < limit {
            let meta = self.slots[slot];
            if meta.state == RoomStreamSlotState::Resident {
                mask.insert_room(meta.room);
            }
            slot += 1;
        }
        mask
    }

    pub(super) fn loading_room_mask(&self) -> RuntimeDebugMask {
        let mut mask = RuntimeDebugMask::EMPTY;
        let mut slot = 0usize;
        let limit = self.effective_slot_limit();
        while slot < limit {
            let meta = self.slots[slot];
            if meta.state == RoomStreamSlotState::Loading {
                mask.insert_room(meta.room);
            }
            slot += 1;
        }
        mask
    }

    pub(super) fn choose_slot(
        &self,
        requested_rooms: &[RoomIndex; STREAMED_ROOM_SLOT_COUNT],
        requested_count: usize,
        reserved_slots: &[usize; N],
        reserved_count: usize,
        allow_eviction: bool,
    ) -> Option<usize> {
        let mut slot = 0usize;
        let slot_limit = self.effective_slot_limit();
        while slot < slot_limit {
            let state = self.slots[slot].state;
            if (state == RoomStreamSlotState::Empty || state == RoomStreamSlotState::Failed)
                && !streamed_slot_reserved(slot, reserved_slots, reserved_count)
            {
                return Some(slot);
            }
            slot += 1;
        }
        if !allow_eviction {
            return None;
        }

        let mut best_slot = None;
        let mut best_age = u32::MAX;
        let mut candidate = 0usize;
        while candidate < slot_limit {
            let meta = self.slots[candidate];
            if meta.state != RoomStreamSlotState::Resident
                || streamed_slot_reserved(candidate, reserved_slots, reserved_count)
                || room_requested(meta.room, requested_rooms, requested_count)
                || self.is_room_pinned(meta.room)
            {
                candidate += 1;
                continue;
            }
            if best_slot.is_none() || meta.last_used < best_age {
                best_slot = Some(candidate);
                best_age = meta.last_used;
            }
            candidate += 1;
        }
        best_slot
    }
}

/// Parse a streamed room's collision view out of its slot byte
/// buffer, re-validating residency first. The `'static` lifetime on
/// the result is a lie (see the contract on
/// `streamed_record_slice` in active_room_cache.rs): the slices point
/// into a slot the scheduler can overwrite, so the value is only good
/// until the next streaming step. Holding it longer is sound only for
/// ACTIVE-WINDOW rooms, which are pinned against eviction; the
/// camera/motor collision caches rely on exactly that, plus cache
/// keys that include the active-room mask so a room leaving the
/// window forces a re-gather before its slot can be reused.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn parse_streamed_compact_collision_room(
    slot: usize,
    index: RoomIndex,
) -> Option<CompactCollisionRoom<'static>> {
    let _ = slot;
    unsafe {
        let resident_slot = ROOM_STREAM_SCHEDULER.resident_slot_for(index)?;
        let byte_count = ROOM_STREAM_SCHEDULER.resident_byte_count(resident_slot)?;
        let bytes = streamed_room_slot_bytes(resident_slot, byte_count)?;
        let view = streamed_room_chunk_view(bytes, index)?;
        if view.flags & STREAMED_ROOM_CHUNK_FLAG_COLLISION_COMPACT == 0 {
            return None;
        }
        let collision =
            bytes.get(view.collision_offset..view.collision_offset + view.collision_bytes)?;
        telemetry::counter(telemetry::counter::CD_ROOM_CHUNK_HITS, 1);
        CompactCollisionRoom::from_bytes(collision).ok()
    }
}

#[cfg(feature = "cd-stream-bench")]
#[derive(Copy, Clone)]
pub(super) struct StreamedRoomChunkView {
    pub(super) total_bytes: usize,
    pub(super) collision_offset: usize,
    pub(super) collision_bytes: usize,
    pub(super) cells_offset: usize,
    pub(super) cell_count: usize,
    pub(super) cell_vertices_offset: usize,
    pub(super) cell_vertex_count: usize,
    pub(super) vertices_offset: usize,
    pub(super) vertex_count: usize,
    pub(super) surfaces_offset: usize,
    pub(super) surface_count: usize,
    pub(super) flags: u32,
}

#[cfg(feature = "cd-stream-bench")]
pub(super) fn streamed_room_slot_bytes(slot: usize, byte_count: usize) -> Option<&'static [u8]> {
    if slot >= STREAMED_ROOM_SLOT_COUNT || byte_count > STREAMED_ROOM_SLOT_BYTES {
        return None;
    }
    unsafe {
        let ptr = core::ptr::addr_of!(STREAMED_ROOM_WORDS[slot])
            .cast::<u32>()
            .cast::<u8>();
        Some(core::slice::from_raw_parts(ptr, byte_count))
    }
}

#[cfg(feature = "cd-stream-bench")]
pub(super) fn streamed_room_chunk_view(
    bytes: &[u8],
    expected_room: RoomIndex,
) -> Option<StreamedRoomChunkView> {
    if bytes.len() < STREAMED_ROOM_CHUNK_HEADER_BYTES {
        return None;
    }
    if bytes.get(0..8)? != STREAMED_ROOM_CHUNK_MAGIC.as_slice() {
        return None;
    }
    if read_streamed_chunk_u32(bytes, streamed_room_chunk_header::VERSION)?
        != STREAMED_ROOM_CHUNK_VERSION
    {
        return None;
    }
    if read_streamed_chunk_u32(bytes, streamed_room_chunk_header::ROOM)?
        != u32::from(expected_room.raw())
    {
        return None;
    }
    let total_bytes =
        read_streamed_chunk_u32(bytes, streamed_room_chunk_header::TOTAL_BYTES)? as usize;
    if total_bytes < STREAMED_ROOM_CHUNK_HEADER_BYTES || total_bytes > bytes.len() {
        return None;
    }
    let collision_offset =
        read_streamed_chunk_u32(bytes, streamed_room_chunk_header::COLLISION_OFFSET)? as usize;
    let collision_bytes =
        read_streamed_chunk_u32(bytes, streamed_room_chunk_header::COLLISION_BYTES)? as usize;
    let cells_offset =
        read_streamed_chunk_u32(bytes, streamed_room_chunk_header::CELLS_OFFSET)? as usize;
    let cell_count =
        read_streamed_chunk_u32(bytes, streamed_room_chunk_header::CELL_COUNT)? as usize;
    let cell_vertices_offset =
        read_streamed_chunk_u32(bytes, streamed_room_chunk_header::CELL_VERTICES_OFFSET)? as usize;
    let cell_vertex_count =
        read_streamed_chunk_u32(bytes, streamed_room_chunk_header::CELL_VERTEX_COUNT)? as usize;
    let vertices_offset =
        read_streamed_chunk_u32(bytes, streamed_room_chunk_header::VERTICES_OFFSET)? as usize;
    let vertex_count =
        read_streamed_chunk_u32(bytes, streamed_room_chunk_header::VERTEX_COUNT)? as usize;
    let surfaces_offset =
        read_streamed_chunk_u32(bytes, streamed_room_chunk_header::SURFACES_OFFSET)? as usize;
    let surface_count =
        read_streamed_chunk_u32(bytes, streamed_room_chunk_header::SURFACE_COUNT)? as usize;
    let flags = read_streamed_chunk_u32(bytes, streamed_room_chunk_header::FLAGS)?;
    if !streamed_chunk_range_valid::<u8>(total_bytes, collision_offset, collision_bytes)
        || !streamed_chunk_range_valid::<LevelCachedRoomCellRecord>(
            total_bytes,
            cells_offset,
            cell_count,
        )
        || !streamed_chunk_range_valid::<u16>(total_bytes, cell_vertices_offset, cell_vertex_count)
        || !streamed_chunk_range_valid::<LevelCachedRoomVertexRecord>(
            total_bytes,
            vertices_offset,
            vertex_count,
        )
        || !streamed_chunk_range_valid::<LevelCachedRoomSurfaceRecord>(
            total_bytes,
            surfaces_offset,
            surface_count,
        )
    {
        return None;
    }
    Some(StreamedRoomChunkView {
        total_bytes,
        collision_offset,
        collision_bytes,
        cells_offset,
        cell_count,
        cell_vertices_offset,
        cell_vertex_count,
        vertices_offset,
        vertex_count,
        surfaces_offset,
        surface_count,
        flags,
    })
}

#[cfg(feature = "cd-stream-bench")]
fn read_streamed_chunk_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let raw = bytes.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

#[cfg(feature = "cd-stream-bench")]
pub(super) fn streamed_chunk_range_valid<T>(
    total_bytes: usize,
    offset: usize,
    count: usize,
) -> bool {
    if count == 0 {
        return offset <= total_bytes;
    }
    if offset % core::mem::align_of::<T>() != 0 {
        return false;
    }
    let Some(byte_count) = count.checked_mul(core::mem::size_of::<T>()) else {
        return false;
    };
    offset
        .checked_add(byte_count)
        .is_some_and(|end| end <= total_bytes)
}

#[cfg(feature = "cd-stream-bench")]
pub(super) fn streamed_room_is_resident(index: RoomIndex) -> bool {
    unsafe { ROOM_STREAM_SCHEDULER.is_resident(index) }
}

#[cfg(feature = "cd-stream-bench")]
pub(super) fn streamed_room_is_loading(index: RoomIndex) -> bool {
    unsafe { ROOM_STREAM_SCHEDULER.is_loading(index) }
}

#[cfg(feature = "cd-stream-bench")]
pub(super) fn streamed_room_stream_active() -> bool {
    unsafe { ROOM_STREAM_SCHEDULER.job.is_active() }
}

#[cfg(feature = "cd-stream-bench")]
fn streamed_slot_reserved(slot: usize, reserved_slots: &[usize], reserved_count: usize) -> bool {
    let mut i = 0usize;
    while i < reserved_count.min(reserved_slots.len()) {
        if reserved_slots[i] == slot {
            return true;
        }
        i += 1;
    }
    false
}

/// Breadth-first room-graph ring around `start`.
///
/// Walks the portal connectivity graph (portals are edges, rooms are nodes) in
/// distance order and writes the rooms reachable within `max_depth` portal hops
/// into `out`, stopping once `out_cap` rooms are written. Because expansion is
/// distance-ordered, capping keeps the NEAREST rooms. Returns the number of
/// rooms written.
///
/// Neighbours of room `r` are the `destination_room`s of the portals in
/// `ROOM_PORTALS[r.portal_first .. r.portal_first + r.portal_count]`. Invalid
/// indices and indices outside `ROOMS` are skipped.
pub(super) fn room_graph_ring(
    start: RoomIndex,
    max_depth: u16,
    out: &mut [RoomIndex],
    out_cap: usize,
) -> usize {
    let mut count = 0usize;
    if start == INVALID_ROOM_INDEX
        || start.to_usize() >= ROOMS.len()
        || start.to_usize() >= MAX_STREAMED_ROOM_INDEX_COUNT
        || out_cap == 0
    {
        return count;
    }

    let mut visited = [false; MAX_STREAMED_ROOM_INDEX_COUNT];
    let mut queue = [(INVALID_ROOM_INDEX, 0u16); MAX_STREAMED_ROOM_INDEX_COUNT];
    let mut head = 0usize;
    let mut tail = 0usize;

    visited[start.to_usize()] = true;
    queue[tail] = (start, 0u16);
    tail += 1;

    while head < tail {
        let (room, depth) = queue[head];
        head += 1;

        if count < out_cap {
            out[count] = room;
            count += 1;
        } else {
            break;
        }

        if depth >= max_depth {
            continue;
        }

        let Some(record) = ROOMS.get(room.to_usize()) else {
            continue;
        };
        let portal_first = record.portal_first as usize;
        let portal_end = portal_first.saturating_add(record.portal_count as usize);
        let mut portal_index = portal_first;
        while portal_index < portal_end.min(ROOM_PORTALS.len()) {
            let portal = ROOM_PORTALS[portal_index];
            portal_index += 1;
            if portal.source_room != room {
                continue;
            }
            let neighbour = portal.destination_room;
            if neighbour == INVALID_ROOM_INDEX {
                continue;
            }
            let neighbour_idx = neighbour.to_usize();
            if neighbour_idx >= ROOMS.len() || neighbour_idx >= MAX_STREAMED_ROOM_INDEX_COUNT {
                continue;
            }
            if visited[neighbour_idx] {
                continue;
            }
            if tail >= MAX_STREAMED_ROOM_INDEX_COUNT {
                continue;
            }
            visited[neighbour_idx] = true;
            queue[tail] = (neighbour, depth + 1);
            tail += 1;
        }
    }

    count
}

#[cfg(feature = "cd-stream-bench")]
pub(super) fn room_requested(
    room: RoomIndex,
    requested_rooms: &[RoomIndex; STREAMED_ROOM_SLOT_COUNT],
    requested_count: usize,
) -> bool {
    let mut i = 0usize;
    while i < requested_count {
        if requested_rooms[i] == room {
            return true;
        }
        i += 1;
    }
    false
}
