//! Streamed-room residency scheduling, carved out of
//! `editor-playtest`'s `active_room_streaming` module (phase 1, slice 2
//! of docs/game-runtime-plan.md). [`RoomStreamScheduler`] owns the
//! room-to-slot residency map, the pin-and-load reconcile, and the
//! retry-backoff policy over the crate's `cd_stream` read job; cooked
//! pack tables arrive as psx-level record params, capacities as
//! `const N` generic parameters, the game's slot word buffers as `&mut`
//! parameters, and its debug logging as closures. Since the
//! vram_runtime slice, the slot byte buffers live here too as
//! [`StreamedRoomSlots`], whose resolvers replace the game's unsafe
//! `&'static`-lying readers with borrows tied to the buffer.

#[cfg(feature = "cd-stream-bench")]
use crate::cd_stream;
use crate::room_cache::INVALID_ROOM_INDEX;
#[cfg(feature = "cd-stream-bench")]
use crate::room_cache::{ActiveRoomCacheStatus, ActiveRoomSurfaceCache};
use psx_engine::telemetry;
#[cfg(feature = "cd-stream-bench")]
use psx_engine::CompactCollisionRoom;
#[cfg(feature = "cd-stream-bench")]
use psx_engine::{
    cached_room_cells_from_level_records, cached_room_surfaces_from_level_records,
    cached_room_vertices_from_level_records, CachedRoomCell, CachedRoomSurface, WorldVertex,
};
#[cfg(feature = "cd-stream-bench")]
use psx_level::{
    streamed_room_chunk_header, LevelCachedRoomCellRecord, LevelCachedRoomSurfaceRecord,
    LevelCachedRoomVertexRecord, LevelWorldPackEntryRecord,
    STREAMED_ROOM_CHUNK_FLAG_COLLISION_COMPACT, STREAMED_ROOM_CHUNK_HEADER_BYTES,
    STREAMED_ROOM_CHUNK_MAGIC, STREAMED_ROOM_CHUNK_VERSION,
};
use psx_level::{LevelRoomPortalRecord, LevelRoomRecord, RoomIndex, RuntimeDebugMask};

/// Sentinel for "no slot" in room-to-stream-slot maps.
pub const STREAMED_ROOM_SLOT_NONE: u16 = u16::MAX;

/// Emit a 64-bit room/portal debug mask as a lo/hi counter pair.
pub fn emit_room_chunk_mask(counter_lo: u16, counter_hi: u16, mask: RuntimeDebugMask) {
    telemetry::counter(counter_lo, mask.lo());
    telemetry::counter(counter_hi, mask.hi());
}

#[cfg(feature = "cd-stream-bench")]
#[derive(Copy, Clone)]
struct StreamedRoomSlot {
    room: RoomIndex,
    byte_count: usize,
    last_used: u32,
    state: RoomStreamSlotState,
}

#[cfg(feature = "cd-stream-bench")]
impl StreamedRoomSlot {
    const EMPTY: Self = Self {
        room: INVALID_ROOM_INDEX,
        byte_count: 0,
        last_used: 0,
        state: RoomStreamSlotState::Empty,
    };

    /// All-zero-bytes placeholder for [`RoomStreamScheduler::zeroed`];
    /// differs from [`Self::EMPTY`] only in the `room` sentinel, which
    /// `state: Empty` keeps unread.
    const ZEROED: Self = Self {
        room: RoomIndex(0),
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

/// Rooms and target slots of one scheduled batch of streamed loads.
#[cfg(feature = "cd-stream-bench")]
#[derive(Copy, Clone)]
pub struct RoomStreamLoadPlan<const N: usize> {
    /// Rooms to load, in schedule order.
    pub rooms: [RoomIndex; N],
    /// Target slot per `rooms` entry.
    pub slots: [usize; N],
    /// In-use length of `rooms`/`slots`.
    pub count: usize,
}

#[cfg(feature = "cd-stream-bench")]
impl<const N: usize> RoomStreamLoadPlan<N> {
    /// Empty plan.
    pub const EMPTY: Self = Self {
        rooms: [INVALID_ROOM_INDEX; N],
        slots: [usize::MAX; N],
        count: 0,
    };

    /// All-zero-bytes placeholder for [`RoomStreamScheduler::zeroed`];
    /// differs from [`Self::EMPTY`] only in sentinel values that
    /// `count: 0` keeps unread.
    const ZEROED: Self = Self {
        rooms: [RoomIndex(0); N],
        slots: [0; N],
        count: 0,
    };
}

/// Streamed-room residency scheduler over `N` slot buffers: maps rooms
/// to slots, pins the residency owner's desired set, batches CD loads
/// through the single `cd_stream` job with per-room failure backoff,
/// and evicts unpinned residents by LRU.
///
/// The game supplies its generated budget consts as the generic
/// parameters and keeps one instance in its own static storage.
#[cfg(feature = "cd-stream-bench")]
pub struct RoomStreamScheduler<const N: usize, const MAX_STREAMED_ROOM_INDEX_COUNT: usize> {
    slots: [StreamedRoomSlot; N],
    room_slots: [u16; MAX_STREAMED_ROOM_INDEX_COUNT],
    /// Rooms declared part of the resident window via `set_resident_window`.
    /// Pinned rooms are never chosen for eviction regardless of LRU age, so the
    /// residency owner can keep them resident without re-requesting them. This
    /// is the primitive both policies build on: full-residency pins every room,
    /// a sliding window pins the current room plus its near neighbours.
    pinned_rooms: [bool; MAX_STREAMED_ROOM_INDEX_COUNT],
    /// The single in-flight multi-room CD read job.
    pub job: cd_stream::WorldRoomSlotsReadJob<N>,
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
impl<const N: usize, const MAX_STREAMED_ROOM_INDEX_COUNT: usize>
    RoomStreamScheduler<N, MAX_STREAMED_ROOM_INDEX_COUNT>
{
    /// All-zero-bytes placeholder so a game can hold this scheduler inside
    /// a link-time-zero (`.bss`) arena static instead of storing `new`'s
    /// non-zero image (room-to-slot sentinels, the slot limit) in the flat
    /// PSX-EXE. The value is NOT ready for use: assign `Self::new()` over
    /// it (once, before first use) to stamp the real initial state. Built
    /// from honest zero-value literals; the sentinel-bearing fields are
    /// unread while their gating fields (`state`, `count`) are zero.
    pub const fn zeroed() -> Self {
        Self {
            slots: [StreamedRoomSlot::ZEROED; N],
            room_slots: [0; MAX_STREAMED_ROOM_INDEX_COUNT],
            pinned_rooms: [false; MAX_STREAMED_ROOM_INDEX_COUNT],
            job: cd_stream::WorldRoomSlotsReadJob::zeroed(),
            job_plan: RoomStreamLoadPlan::ZEROED,
            slot_limit: 0,
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

    /// Empty scheduler; `const` so the game can keep it in link-time
    /// zero-initialized storage.
    pub const fn new() -> Self {
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

    fn effective_slot_limit(&self) -> usize {
        self.slot_limit.clamp(1, N)
    }

    fn is_room_pinned(&self, room: RoomIndex) -> bool {
        let index = room.to_usize();
        index < MAX_STREAMED_ROOM_INDEX_COUNT && self.pinned_rooms[index]
    }

    /// Declare the rooms that must stay resident. They are pinned (never
    /// evicted) so they survive without being re-requested; rooms no longer in
    /// the set are unpinned and become evictable again.
    fn set_resident_window(&mut self, rooms: &[RoomIndex], count: usize) {
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
    pub fn reconcile_residency<const STREAMED_ROOM_SLOT_BYTES: usize>(
        &mut self,
        desired: &[RoomIndex; N],
        count: usize,
        stream_load_batch_count: usize,
        world_pack_start_lba: u32,
        world_pack_toc: &[LevelWorldPackEntryRecord],
        log_plan: impl Fn(&str, &RoomStreamLoadPlan<N>),
        log_entry: impl Fn(&str, RoomIndex, usize, usize, u32),
    ) {
        self.begin_window();
        self.set_resident_window(desired, count);
        let plan =
            self.plan_window_loads(desired, count, count, stream_load_batch_count, &log_plan);
        self.start_load_plan::<STREAMED_ROOM_SLOT_BYTES>(
            plan,
            world_pack_start_lba,
            world_pack_toc,
            log_plan,
            log_entry,
        );
        self.emit_counters();
    }

    fn begin_window(&mut self) {
        self.epoch = self.epoch.wrapping_add(1).max(1);
        self.window_requests = 0;
        self.window_misses = 0;
        self.window_prefetch_requests = 0;
        self.window_evictions = 0;
        self.window_failed_loads = 0;
        self.window_pending_loads = 0;
        self.window_protected_full = 0;
    }

    /// Resident slot holding `room`, refreshing its LRU age.
    pub fn resident_slot_for(&mut self, room: RoomIndex) -> Option<usize> {
        if let Some(slot) = self.mapped_slot_for(room, RoomStreamSlotState::Resident) {
            self.slots[slot].last_used = self.epoch;
            return Some(slot);
        }
        None
    }

    /// Resident slot for `room` as the u16 stream-slot id the material
    /// pool keys on ([`STREAMED_ROOM_SLOT_NONE`] when not resident),
    /// refreshing the slot's LRU age.
    #[inline]
    pub fn resident_stream_slot(&mut self, room: RoomIndex) -> u16 {
        self.resident_slot_for(room)
            .and_then(|slot| u16::try_from(slot).ok())
            .unwrap_or(STREAMED_ROOM_SLOT_NONE)
    }

    /// Whether `room` is resident in some slot.
    pub fn is_resident(&self, room: RoomIndex) -> bool {
        self.mapped_slot_for(room, RoomStreamSlotState::Resident)
            .is_some()
    }

    /// Resident payload byte count of `slot`, when it holds one.
    pub fn resident_byte_count(&self, slot: usize) -> Option<usize> {
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

    fn loading_slot_for(&self, room: RoomIndex) -> Option<usize> {
        self.mapped_slot_for(room, RoomStreamSlotState::Loading)
    }

    /// Whether `room` has a load in flight.
    pub fn is_loading(&self, room: RoomIndex) -> bool {
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
        self.failure_hold_until[index] =
            self.epoch.wrapping_add(stream_retry_backoff_windows(count));
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

    fn plan_window_loads(
        &mut self,
        requested_rooms: &[RoomIndex; N],
        requested_count: usize,
        active_count: usize,
        stream_load_batch_count: usize,
        log_plan: impl Fn(&str, &RoomStreamLoadPlan<N>),
    ) -> RoomStreamLoadPlan<N> {
        let mut plan = RoomStreamLoadPlan::EMPTY;
        if requested_count > 0 && !self.current_room_request_can_wait(requested_rooms[0]) {
            self.abort_active_load(log_plan);
        }
        let can_schedule_new_loads = !self.job.is_active();
        let protected_count = active_count
            .min(requested_count)
            .min(self.effective_slot_limit())
            .min(N);
        let limit = requested_count.min(self.effective_slot_limit()).min(N);
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
            if plan.count >= stream_load_batch_count {
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

    fn current_room_request_can_wait(&self, room: RoomIndex) -> bool {
        room == INVALID_ROOM_INDEX
            || self.is_resident(room)
            || self.is_loading(room)
            || !self.job.is_active()
    }

    fn abort_active_load(&mut self, log_plan: impl Fn(&str, &RoomStreamLoadPlan<N>)) {
        if !self.job.is_active() {
            return;
        }
        log_plan("stream abort", &self.job_plan);
        self.job.abort();
        let plan = self.job_plan;
        let mut i = 0usize;
        while i < plan.count.min(N) {
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

    fn start_load_plan<const STREAMED_ROOM_SLOT_BYTES: usize>(
        &mut self,
        plan: RoomStreamLoadPlan<N>,
        world_pack_start_lba: u32,
        world_pack_toc: &[LevelWorldPackEntryRecord],
        log_plan: impl Fn(&str, &RoomStreamLoadPlan<N>),
        log_entry: impl Fn(&str, RoomIndex, usize, usize, u32),
    ) {
        if plan.count == 0 || self.job.is_active() {
            return;
        }
        log_plan("stream start", &plan);
        let mut room_ids = [u16::MAX; N];
        let mut i = 0usize;
        while i < plan.count.min(N) {
            room_ids[i] = plan.rooms[i].raw();
            i += 1;
        }
        self.job.start::<STREAMED_ROOM_SLOT_BYTES>(
            world_pack_start_lba,
            world_pack_toc,
            &room_ids[..plan.count],
            &plan.slots[..plan.count],
        );
        self.job_plan = plan;
        if self.job.is_done() {
            self.commit_completed_job(log_entry);
        }
    }

    /// Advance the in-flight load by up to `max_sectors` CD sectors into
    /// the game's slot word buffers, committing rooms as they complete.
    /// Returns whether any room became resident this call.
    pub fn pump<const STREAMED_ROOM_SLOT_WORDS: usize>(
        &mut self,
        dst: &mut [[u32; STREAMED_ROOM_SLOT_WORDS]; N],
        max_sectors: usize,
        log_entry: impl Fn(&str, RoomIndex, usize, usize, u32),
    ) -> bool {
        if !self.job.is_active() {
            return false;
        }
        self.job
            .poll_words::<STREAMED_ROOM_SLOT_WORDS>(dst, max_sectors);
        let committed = self.commit_ready_job_entries();
        if self.job.is_done() {
            self.commit_completed_job(log_entry);
            true
        } else {
            committed
        }
    }

    fn commit_ready_job_entries(&mut self) -> bool {
        let completed = self.job.completed_entries();
        let byte_counts = *self.job.byte_counts();
        let plan = self.job_plan;
        let mut committed = false;
        let mut i = 0usize;
        while i < plan.count.min(N) {
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

    fn commit_completed_job(&mut self, log_entry: impl Fn(&str, RoomIndex, usize, usize, u32)) {
        let byte_counts = *self.job.byte_counts();
        let statuses = *self.job.statuses();
        let plan = self.job_plan;
        self.commit_window_loads(&plan, &byte_counts, &statuses, log_entry);
        self.job = cd_stream::WorldRoomSlotsReadJob::new();
        self.job_plan = RoomStreamLoadPlan::EMPTY;
    }

    fn commit_window_loads(
        &mut self,
        plan: &RoomStreamLoadPlan<N>,
        byte_counts: &[usize; N],
        statuses: &[u32; N],
        log_entry: impl Fn(&str, RoomIndex, usize, usize, u32),
    ) {
        let mut loaded = 0usize;
        while loaded < plan.count.min(N) {
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
                log_entry(
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
                log_entry(
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
                log_entry(
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

    fn emit_counters(&self) {
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

    /// Slots currently holding a resident room.
    pub fn resident_slot_count(&self) -> usize {
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

    /// Debug mask of every resident room.
    pub fn resident_room_mask(&self) -> RuntimeDebugMask {
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

    /// Debug mask of every room with a load in flight.
    pub fn loading_room_mask(&self) -> RuntimeDebugMask {
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

    fn choose_slot(
        &self,
        requested_rooms: &[RoomIndex; N],
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

/// Layout of one streamed room chunk's payload inside its slot bytes.
#[cfg(feature = "cd-stream-bench")]
#[derive(Copy, Clone)]
pub struct StreamedRoomChunkView {
    /// Total payload bytes, header included.
    pub total_bytes: usize,
    /// Byte offset of the compact collision payload.
    pub collision_offset: usize,
    /// Compact collision payload byte count.
    pub collision_bytes: usize,
    /// Byte offset of the cached cell records.
    pub cells_offset: usize,
    /// Cached cell record count.
    pub cell_count: usize,
    /// Byte offset of the per-cell vertex indices.
    pub cell_vertices_offset: usize,
    /// Per-cell vertex index count.
    pub cell_vertex_count: usize,
    /// Byte offset of the cached vertex records.
    pub vertices_offset: usize,
    /// Cached vertex record count.
    pub vertex_count: usize,
    /// Byte offset of the cached surface records.
    pub surfaces_offset: usize,
    /// Cached surface record count.
    pub surface_count: usize,
    /// Cooked chunk flags.
    pub flags: u32,
}

/// Parse and bounds-check a streamed room chunk header, verifying it
/// belongs to `expected_room`.
#[cfg(feature = "cd-stream-bench")]
pub fn streamed_room_chunk_view(
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

/// Whether `count` records of `T` at `offset` sit aligned and in-bounds
/// within a `total_bytes` chunk payload.
#[cfg(feature = "cd-stream-bench")]
pub fn streamed_chunk_range_valid<T>(total_bytes: usize, offset: usize, count: usize) -> bool {
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

/// Streamed-room slot word buffers: the RAM the scheduler loads CD
/// chunks into, owned as one struct over the game's `(SLOT_WORDS, N)`
/// budget (replacing the example's `STREAMED_ROOM_WORDS` static).
/// Zero-initialized `const` construction keeps the game's static
/// instance in `.bss` (NOLOAD) instead of storing ~200 KB of zeros in
/// the flat PSX-EXE image.
///
/// The resolvers are lifetime-honest: every returned slice borrows
/// `self`, so it cannot outlive the buffer it points into. The
/// STALENESS caveat from the streaming audit (finding 3) still applies
/// across calls: a resolved slice describes the slot's contents only
/// until the next `RoomStreamScheduler::pump` / `reconcile_residency`
/// overwrites that slot, so re-resolve per use; holding one longer is
/// sound only for ACTIVE-WINDOW rooms, which are pinned against
/// eviction (the camera/motor collision caches rely on exactly that,
/// plus cache keys that include the active-room mask so a room leaving
/// the window forces a re-gather before its slot can be reused).
#[cfg(feature = "cd-stream-bench")]
pub struct StreamedRoomSlots<const WORDS: usize, const N: usize> {
    words: [[u32; WORDS]; N],
}

#[cfg(feature = "cd-stream-bench")]
impl<const WORDS: usize, const N: usize> StreamedRoomSlots<WORDS, N> {
    /// Zero-initialized slot buffers; `const` so the game's static
    /// instance stays in `.bss`.
    pub const fn new() -> Self {
        Self {
            words: [[0; WORDS]; N],
        }
    }

    /// Raw per-slot word buffers for [`RoomStreamScheduler::pump`].
    pub fn words_mut(&mut self) -> &mut [[u32; WORDS]; N] {
        &mut self.words
    }

    /// Byte view of `slot`'s first `byte_count` bytes.
    #[inline]
    pub fn slot_bytes(&self, slot: usize, byte_count: usize) -> Option<&[u8]> {
        if slot >= N || byte_count > WORDS * 4 {
            return None;
        }
        // SAFETY: in-bounds u32 -> u8 reinterpretation of one slot row
        // (plain old data, alignment only loosens). Moved from the
        // example's `streamed_room_slot_bytes`, which returned the same
        // view with a lied `'static` lifetime; the borrow of `self` now
        // carries the real one.
        Some(unsafe {
            core::slice::from_raw_parts(self.words[slot].as_ptr().cast::<u8>(), byte_count)
        })
    }

    /// Resident chunk bytes for `index`, re-validating residency (and
    /// refreshing the room's LRU age) through the scheduler.
    #[inline]
    pub fn resident_chunk_bytes<const M: usize>(
        &self,
        scheduler: &mut RoomStreamScheduler<N, M>,
        index: RoomIndex,
    ) -> Option<&[u8]> {
        let resident_slot = scheduler.resident_slot_for(index)?;
        let byte_count = scheduler.resident_byte_count(resident_slot)?;
        self.slot_bytes(resident_slot, byte_count)
    }

    /// Parse a streamed room's collision view out of its slot byte
    /// buffer, re-validating residency first. The result is only good
    /// until the next streaming step (see the type-level staleness
    /// caveat).
    #[inline]
    pub fn compact_collision_room<const M: usize>(
        &self,
        scheduler: &mut RoomStreamScheduler<N, M>,
        index: RoomIndex,
    ) -> Option<CompactCollisionRoom<'_>> {
        let bytes = self.resident_chunk_bytes(scheduler, index)?;
        let view = streamed_room_chunk_view(bytes, index)?;
        if view.flags & STREAMED_ROOM_CHUNK_FLAG_COLLISION_COMPACT == 0 {
            return None;
        }
        let collision =
            bytes.get(view.collision_offset..view.collision_offset + view.collision_bytes)?;
        telemetry::counter(telemetry::counter::CD_ROOM_CHUNK_HITS, 1);
        CompactCollisionRoom::from_bytes(collision).ok()
    }

    /// Surface-cache descriptor for a streamed room, read out of its
    /// resident chunk header.
    #[inline]
    pub fn surface_cache_for<const MAX_VERTICES: usize, const M: usize>(
        &self,
        scheduler: &mut RoomStreamScheduler<N, M>,
        index: RoomIndex,
    ) -> Option<ActiveRoomSurfaceCache> {
        let bytes = self.resident_chunk_bytes(scheduler, index)?;
        let view = streamed_room_chunk_view(bytes, index)?;
        if view.vertex_count > MAX_VERTICES {
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

    /// Resolve a streamed room's surface-cache slices DIRECTLY INTO its
    /// slot byte buffer, re-validating residency and every chunk-view
    /// offset against the cache snapshot first. The result inherits the
    /// type-level staleness caveat: consume it within the current
    /// render/update step and re-resolve next time; never store the
    /// slices.
    #[allow(clippy::type_complexity)]
    #[inline]
    pub fn surface_cache_slices<const MAX_VERTICES: usize, const M: usize>(
        &self,
        scheduler: &mut RoomStreamScheduler<N, M>,
        index: RoomIndex,
        cache: ActiveRoomSurfaceCache,
    ) -> Option<(
        &[CachedRoomCell],
        &[u16],
        &[WorldVertex],
        &[CachedRoomSurface],
    )> {
        if !cache.ready || cache.vertex_count > MAX_VERTICES {
            return None;
        }
        let bytes = self.resident_chunk_bytes(scheduler, index)?;
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

/// Typed record slice out of a chunk's validated byte payload. The
/// returned slice borrows `bytes` (lifetime-honest since the
/// vram_runtime carve; the old example version lied `'static`). Entry
/// points re-validate slot residency and the chunk-view offsets per
/// call, which is what keeps this cast sound.
#[cfg(feature = "cd-stream-bench")]
#[inline]
fn streamed_record_slice<T>(
    bytes: &[u8],
    total_bytes: usize,
    offset: usize,
    count: usize,
) -> Option<&[T]> {
    if !streamed_chunk_range_valid::<T>(total_bytes, offset, count) {
        return None;
    }
    let byte_count = count.checked_mul(core::mem::size_of::<T>())?;
    let slice = bytes.get(offset..offset + byte_count)?;
    // SAFETY: `streamed_chunk_range_valid` checked alignment and bounds
    // for `count` records of `T` at `offset`; the records are plain old
    // cooked data.
    Some(unsafe { core::slice::from_raw_parts(slice.as_ptr().cast::<T>(), count) })
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
/// `room_portals[r.portal_first .. r.portal_first + r.portal_count]`. Invalid
/// indices and indices outside `rooms` are skipped.
pub fn room_graph_ring<const MAX_STREAMED_ROOM_INDEX_COUNT: usize>(
    rooms: &'static [LevelRoomRecord],
    room_portals: &'static [LevelRoomPortalRecord],
    start: RoomIndex,
    max_depth: u16,
    out: &mut [RoomIndex],
    out_cap: usize,
) -> usize {
    let mut count = 0usize;
    if start == INVALID_ROOM_INDEX
        || start.to_usize() >= rooms.len()
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

        let Some(record) = rooms.get(room.to_usize()) else {
            continue;
        };
        let portal_first = record.portal_first as usize;
        let portal_end = portal_first.saturating_add(record.portal_count as usize);
        let mut portal_index = portal_first;
        while portal_index < portal_end.min(room_portals.len()) {
            let portal = room_portals[portal_index];
            portal_index += 1;
            if portal.source_room != room {
                continue;
            }
            let neighbour = portal.destination_room;
            if neighbour == INVALID_ROOM_INDEX {
                continue;
            }
            let neighbour_idx = neighbour.to_usize();
            if neighbour_idx >= rooms.len() || neighbour_idx >= MAX_STREAMED_ROOM_INDEX_COUNT {
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

/// Whether `room` is already in `requested_rooms[..requested_count]`.
#[cfg(feature = "cd-stream-bench")]
pub fn room_requested<const N: usize>(
    room: RoomIndex,
    requested_rooms: &[RoomIndex; N],
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
