//! Active-room window state machine, carved out of `editor-playtest`'s
//! `active_rooms` module (phase 1, slice 2 of
//! docs/game-runtime-plan.md). [`RoomWindow`] owns the resident draw
//! window, the incremental rebuild job staged against it, and the
//! request anchor the example previously spread across its scene
//! struct; cooked tables arrive as `&'static` psx-level records,
//! capacities as `const N` generic parameters, and the
//! streaming/residency-coupled room builders stay with the game as
//! closures until those modules move.

use crate::room_cache::{
    active_room_contains_drawable, ActiveRoomWindowJob, ActiveRuntimeRoom, INVALID_ROOM_INDEX,
};
use crate::room_visibility::RoomVisibility;
use psx_engine::{telemetry, RoomPoint};
use psx_level::{LevelRoomRecord, RoomIndex};

/// Outcome of one [`RoomWindow::step_job`] build slice.
pub struct RoomWindowStep {
    /// Rooms built (or reused) into the staged window this slice.
    pub built: usize,
    /// Requested room that failed to build, or [`INVALID_ROOM_INDEX`].
    pub unbuilt_room: RoomIndex,
    /// The freshly built current room, when this slice produced it.
    pub current_active: Option<ActiveRuntimeRoom>,
}

/// Outcome of one [`RoomWindow::reconcile`] pass.
pub struct RoomWindowReconcile {
    /// Rooms built into the window this pass.
    pub built: usize,
    /// Entries freed (left the desired set, or stale stream slot).
    pub freed: usize,
    /// Whether every desired room is now present in the window.
    pub converged: bool,
    /// The freshly built current room, when this pass produced it.
    pub current_active: Option<ActiveRuntimeRoom>,
    /// Last room freed for a stale stream slot (diagnostics).
    pub freed_stale_room: RoomIndex,
    /// Last room whose build returned nothing (diagnostics).
    pub failed_room: RoomIndex,
    /// Last room whose build was skipped as not drawable (diagnostics).
    pub skipped_room: RoomIndex,
}

/// Outcome of a synchronous [`RoomWindow::rebuild_from_visible`].
pub struct RoomWindowRebuild {
    /// Staged room count (the next free window slot).
    pub next_slot: usize,
    /// The freshly built current room, when the rebuild produced it.
    pub current_active: Option<ActiveRuntimeRoom>,
}

/// Whether a staged room is drawable enough to occupy a window slot.
/// The shared accept policy of [`RoomWindow::step_job`] and the
/// synchronous rebuild/preload paths: the first requested room (the
/// current room) is always accepted; any other room must carry a
/// render payload or a ready surface cache.
fn staged_room_accepted(active: &ActiveRuntimeRoom, first_requested: bool) -> bool {
    first_requested || active.render_room.is_some() || active.surface_cache.ready
}

/// Owned active-room window state: the resident draw window, the
/// incremental rebuild job staged against it, the player anchor the
/// latest window request ran for, and the skipped-build diagnostics.
///
/// The game supplies its generated budget consts as the generic
/// parameter and keeps one instance wherever it keeps scene state.
pub struct RoomWindow<const MAX_ACTIVE_ROOMS: usize> {
    /// Cache-budgeted draw chunks, all expressed relative to the game's
    /// current room.
    pub rooms: [Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    /// Incremental active-room cache rebuild in progress. The old
    /// `rooms` remain drawable until the staged replacement is ready.
    pub job: ActiveRoomWindowJob<MAX_ACTIVE_ROOMS>,
    /// Player position the latest window request was anchored to.
    pub anchor: RoomPoint,
    /// Rooms whose staged build was skipped as not yet drawable.
    pub cache_skips: u16,
    /// Full-width identity for caches derived from the live room window.
    generation: u32,
}

impl<const MAX_ACTIVE_ROOMS: usize> RoomWindow<MAX_ACTIVE_ROOMS> {
    /// Empty boot state. NOT all-zero bytes: `job.requested_rooms` holds
    /// `INVALID_ROOM_INDEX` sentinels, and the `Option` room slots niche
    /// their `None` into a payload byte, so a game keeping this state in
    /// link-time-zero (`.bss`) storage must stamp it at boot instead of
    /// storing this `const` directly (every field is `pub`).
    pub const EMPTY: Self = Self {
        rooms: [const { None }; MAX_ACTIVE_ROOMS],
        job: ActiveRoomWindowJob::EMPTY,
        anchor: RoomPoint::ZERO,
        cache_skips: 0,
        generation: 1,
    };

    /// Identity that changes whenever live window contents may change.
    pub const fn generation(&self) -> u32 {
        self.generation
    }

    fn bump_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1).max(1);
    }

    /// Whether the window holds `index` in any slot.
    pub fn contains(&self, index: RoomIndex) -> bool {
        let mut slot = 0usize;
        while slot < MAX_ACTIVE_ROOMS {
            if self.rooms[slot].is_some_and(|active| active.index == index) {
                return true;
            }
            slot += 1;
        }
        false
    }

    /// Re-express every windowed room's chunk offsets relative to
    /// `current_record`, dropping rooms whose record or resident stream
    /// slot (per `stream_slot_for`) is gone.
    pub fn rebase_to_current_room(
        &mut self,
        rooms: &'static [LevelRoomRecord],
        current_record: &LevelRoomRecord,
        stream_slot_for: impl Fn(RoomIndex) -> u16,
    ) {
        self.bump_generation();
        let mut slot = 0usize;
        while slot < MAX_ACTIVE_ROOMS {
            let Some(active) = self.rooms[slot] else {
                slot += 1;
                continue;
            };
            let Some(record) = rooms.get(active.index.to_usize()) else {
                self.rooms[slot] = None;
                slot += 1;
                continue;
            };
            if active.stream_slot != stream_slot_for(active.index) {
                self.rooms[slot] = None;
                slot += 1;
                continue;
            }
            self.rooms[slot] = Some(active.with_current_room_offsets(record, current_record));
            slot += 1;
        }
    }

    /// Carry still-valid rooms from `previous_rooms` into the free
    /// window slots after a rebuild, up to `retained_inactive_rooms`
    /// beyond `*next_slot` (capped by `active_limit`).
    pub fn retain_previous_rooms(
        &mut self,
        rooms: &'static [LevelRoomRecord],
        previous_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
        current_record: &LevelRoomRecord,
        retained_inactive_rooms: usize,
        active_limit: usize,
        next_slot: &mut usize,
        stream_slot_for: impl Fn(RoomIndex) -> u16,
    ) {
        self.bump_generation();
        let retained_limit = next_slot
            .saturating_add(retained_inactive_rooms)
            .min(active_limit)
            .min(MAX_ACTIVE_ROOMS);
        let mut previous_slot = 0usize;
        while *next_slot < retained_limit && previous_slot < MAX_ACTIVE_ROOMS {
            let Some(previous) = previous_rooms[previous_slot] else {
                previous_slot += 1;
                continue;
            };
            previous_slot += 1;
            if previous.stream_slot != stream_slot_for(previous.index)
                || self.contains(previous.index)
            {
                continue;
            }
            let Some(record) = rooms.get(previous.index.to_usize()) else {
                continue;
            };
            self.rooms[*next_slot] =
                Some(previous.with_current_room_offsets(record, current_record));
            *next_slot += 1;
        }
    }

    /// Per-tick desired/actual convergence over the live window: the
    /// replacement for the event-triggered rebuild paths.
    ///
    /// Invariants (see docs/cortex-v3-visuals-30fps.md, informed by
    /// REFERENCE's per-frame `GetRoomBounds` walk):
    /// - The goal is read-only. A room that fails to build stays in
    ///   `desired` and simply retries on a later pass; failures never
    ///   prune the goal to match.
    /// - Build-then-swap. An existing entry is freed only when its room
    ///   left the desired set or its resident stream slot moved (its
    ///   parsed pointers would dangle). A still-desired, still-valid
    ///   entry is never dropped, so a visible room cannot blink while
    ///   the window converges.
    /// - `desired` is priority-ordered with the current room first, so
    ///   a mandatory free of the current room rebuilds in this same
    ///   pass while `builds_per_tick >= 1`.
    ///
    /// The pass is cheap when converged: one mask-style sweep and no
    /// builds. Callers run it every tick instead of gating on camera
    /// movement.
    pub fn reconcile(
        &mut self,
        rooms: &'static [LevelRoomRecord],
        desired: &[RoomIndex],
        current_room: RoomIndex,
        builds_per_tick: usize,
        stream_slot_for: impl Fn(RoomIndex) -> u16,
        mut build: impl FnMut(
            usize,
            RoomIndex,
            &'static LevelRoomRecord,
            &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
        ) -> Option<ActiveRuntimeRoom>,
    ) -> RoomWindowReconcile {
        // Snapshot before freeing so a same-pass rebuild can still
        // reuse a valid previous entry.
        let previous_rooms = self.rooms;

        let mut freed = 0usize;
        let mut freed_stale_room = INVALID_ROOM_INDEX;
        let mut slot = 0usize;
        while slot < MAX_ACTIVE_ROOMS {
            if let Some(active) = self.rooms[slot] {
                let stale = active.stream_slot != stream_slot_for(active.index);
                let wanted = desired.contains(&active.index);
                if stale || !wanted {
                    if stale {
                        freed_stale_room = active.index;
                    }
                    self.rooms[slot] = None;
                    freed += 1;
                }
            }
            slot += 1;
        }

        let mut built = 0usize;
        let mut converged = true;
        let mut current_active = None;
        let mut failed_room = INVALID_ROOM_INDEX;
        let mut skipped_room = INVALID_ROOM_INDEX;
        let mut desired_slot = 0usize;
        while desired_slot < desired.len() {
            let index = desired[desired_slot];
            desired_slot += 1;
            if index == INVALID_ROOM_INDEX || self.contains(index) {
                continue;
            }
            if built >= builds_per_tick {
                converged = false;
                continue;
            }
            let Some(record) = rooms.get(index.to_usize()) else {
                continue;
            };
            let Some(free_slot) = self.rooms.iter().position(|entry| entry.is_none()) else {
                converged = false;
                break;
            };
            match build(free_slot, index, record, &previous_rooms) {
                Some(active) if staged_room_accepted(&active, index == current_room) => {
                    if active.index == current_room {
                        current_active = Some(active);
                    }
                    self.rooms[free_slot] = Some(active);
                    built += 1;
                }
                Some(_) => {
                    self.cache_skips = self.cache_skips.saturating_add(1);
                    skipped_room = index;
                    converged = false;
                }
                None => {
                    failed_room = index;
                    converged = false;
                }
            }
        }

        if freed > 0 || built > 0 {
            self.bump_generation();
        }
        RoomWindowReconcile {
            built,
            freed,
            converged,
            current_active,
            freed_stale_room,
            failed_room,
            skipped_room,
        }
    }

    /// Stage an incremental window rebuild for `current_room` over the
    /// caller-assembled request list, anchored at `anchor`.
    pub fn begin_job(
        &mut self,
        current_room: RoomIndex,
        requested_rooms: [RoomIndex; MAX_ACTIVE_ROOMS],
        requested_count: usize,
        update_streaming: bool,
        anchor: RoomPoint,
    ) {
        self.anchor = anchor;
        self.cache_skips = 0;
        self.job = ActiveRoomWindowJob {
            active: true,
            update_streaming,
            current_room,
            requested_rooms,
            requested_count,
            cursor: 0,
            next_slot: 0,
            rooms: [const { None }; MAX_ACTIVE_ROOMS],
            previous_rooms: self.rooms,
        };
        telemetry::counter(telemetry::counter::ROOM_WINDOW_REBUILDS, 1);
    }

    /// Run up to `active_job_builds_per_tick` staged builds of the
    /// pending job. `build` is the game's (streaming/residency-coupled)
    /// room builder over `(slot, index, record, previous_rooms)`;
    /// `build_blocked` reports whether a failed room is still loading
    /// (the slice then stops instead of skipping it).
    pub fn step_job(
        &mut self,
        rooms: &'static [LevelRoomRecord],
        active_job_builds_per_tick: usize,
        mut build: impl FnMut(
            usize,
            RoomIndex,
            &'static LevelRoomRecord,
            &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
        ) -> Option<ActiveRuntimeRoom>,
        build_blocked: impl Fn(RoomIndex) -> bool,
    ) -> RoomWindowStep {
        let current_room = self.job.current_room;
        let mut built_this_tick = 0usize;
        let mut skipped = 0u16;
        let mut unbuilt_room = INVALID_ROOM_INDEX;
        let mut current_active = None;
        {
            let job = &mut self.job;
            while job.cursor < job.requested_count
                && job.next_slot < MAX_ACTIVE_ROOMS
                && built_this_tick < active_job_builds_per_tick
            {
                let index = job.requested_rooms[job.cursor];
                if index == INVALID_ROOM_INDEX {
                    job.cursor += 1;
                    continue;
                }
                let Some(record) = rooms.get(index.to_usize()) else {
                    job.cursor += 1;
                    continue;
                };
                match build(job.next_slot, index, record, &job.previous_rooms) {
                    Some(active) if staged_room_accepted(&active, job.cursor == 0) => {
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
                        if build_blocked(index) {
                            break;
                        }
                        job.cursor += 1;
                    }
                }
            }
        }
        self.cache_skips = self.cache_skips.saturating_add(skipped);
        RoomWindowStep {
            built: built_this_tick,
            unbuilt_room,
            current_active,
        }
    }

    /// Build `visible_rooms` into the live window with the shared
    /// accept/skip/block policy (see [`staged_room_accepted`]): a
    /// skipped room counts a cache skip, a failed build reports through
    /// `mark_unbuilt`, and a failed FIRST visible room stops the pass
    /// (nothing closer can unblock it). Returns the freshly built
    /// current room, if this pass produced it.
    fn stage_visible_rooms(
        &mut self,
        rooms: &'static [LevelRoomRecord],
        visible_rooms: &[RoomIndex],
        current_room: RoomIndex,
        skip_room: Option<RoomIndex>,
        slot_cap: usize,
        next_slot: &mut usize,
        previous_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
        build: &mut impl FnMut(
            usize,
            RoomIndex,
            &'static LevelRoomRecord,
            &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
        ) -> Option<ActiveRuntimeRoom>,
        mark_unbuilt: &mut impl FnMut(RoomIndex),
    ) -> Option<ActiveRuntimeRoom> {
        let mut current_active = None;
        let mut visible_slot = 0usize;
        while visible_slot < visible_rooms.len() && *next_slot < slot_cap.min(MAX_ACTIVE_ROOMS) {
            let index = visible_rooms[visible_slot];
            if skip_room == Some(index) {
                visible_slot += 1;
                continue;
            }
            let Some(record) = rooms.get(index.to_usize()) else {
                visible_slot += 1;
                continue;
            };
            match build(*next_slot, index, record, previous_rooms) {
                Some(active) if staged_room_accepted(&active, visible_slot == 0) => {
                    if active.index == current_room {
                        current_active = Some(active);
                    }
                    self.rooms[*next_slot] = Some(active);
                    *next_slot += 1;
                }
                Some(_) => {
                    self.cache_skips = self.cache_skips.saturating_add(1);
                }
                None => {
                    mark_unbuilt(index);
                    if visible_slot == 0 {
                        break;
                    }
                }
            }
            visible_slot += 1;
        }
        current_active
    }

    /// Synchronous full-window rebuild over the frustum-visible room
    /// list: reset the window, stage every drawable visible room
    /// eagerly, and fall back to building the current room alone when
    /// the visible pass did not produce it (collision must never go
    /// missing). The caller applies `current_active`'s fields, retains
    /// previous rooms, and owns its diagnostics through `mark_unbuilt`.
    pub fn rebuild_from_visible(
        &mut self,
        rooms: &'static [LevelRoomRecord],
        visible_rooms: &[RoomIndex],
        current_room: RoomIndex,
        current_record: &'static LevelRoomRecord,
        anchor: RoomPoint,
        previous_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
        mut build: impl FnMut(
            usize,
            RoomIndex,
            &'static LevelRoomRecord,
            &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
        ) -> Option<ActiveRuntimeRoom>,
        mut mark_unbuilt: impl FnMut(RoomIndex),
    ) -> RoomWindowRebuild {
        self.bump_generation();
        self.rooms = [const { None }; MAX_ACTIVE_ROOMS];
        self.cache_skips = 0;
        self.anchor = anchor;
        let mut next_slot = 0usize;
        let mut current_active = self.stage_visible_rooms(
            rooms,
            visible_rooms,
            current_room,
            None,
            MAX_ACTIVE_ROOMS,
            &mut next_slot,
            previous_rooms,
            &mut build,
            &mut mark_unbuilt,
        );
        // The visible pass did not build the current room (not listed,
        // or its build failed): try it alone so collision stays live.
        if current_active.is_none() && next_slot < MAX_ACTIVE_ROOMS {
            if let Some(active) = build(next_slot, current_room, current_record, previous_rooms) {
                current_active = Some(active);
                self.rooms[next_slot] = Some(active);
                next_slot += 1;
            }
        }
        RoomWindowRebuild {
            next_slot,
            current_active,
        }
    }

    /// Synchronous window rebuild for the streamed preload path: the
    /// current room builds first (accepted regardless of drawability),
    /// then the visible list fills the remaining slots up to
    /// `active_limit`, skipping the current room. Returns the next free
    /// window slot; the caller retains previous rooms and re-applies its
    /// current-room fields from the landed window.
    pub fn preload_from_visible(
        &mut self,
        rooms: &'static [LevelRoomRecord],
        visible_rooms: &[RoomIndex],
        current_room: RoomIndex,
        current_record: &'static LevelRoomRecord,
        active_limit: usize,
        previous_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
        mut build: impl FnMut(
            usize,
            RoomIndex,
            &'static LevelRoomRecord,
            &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
        ) -> Option<ActiveRuntimeRoom>,
        mut mark_unbuilt: impl FnMut(RoomIndex),
    ) -> usize {
        self.bump_generation();
        self.rooms = [const { None }; MAX_ACTIVE_ROOMS];
        let active_limit = active_limit.min(MAX_ACTIVE_ROOMS);
        let mut next_slot = 0usize;
        if next_slot < active_limit {
            match build(next_slot, current_room, current_record, previous_rooms) {
                Some(active) => {
                    self.rooms[next_slot] = Some(active);
                    next_slot += 1;
                }
                None => mark_unbuilt(current_room),
            }
        }
        let _ = self.stage_visible_rooms(
            rooms,
            visible_rooms,
            current_room,
            Some(current_room),
            active_limit,
            &mut next_slot,
            previous_rooms,
            &mut build,
            &mut mark_unbuilt,
        );
        next_slot
    }

    /// When the pending job has consumed its request list (or filled the
    /// window), promote the staged rooms to the live window, retain
    /// still-valid previous rooms, and clear the job. Returns whether
    /// the job landed, so the caller can refresh its current-room state.
    pub fn finish_job(
        &mut self,
        rooms: &'static [LevelRoomRecord],
        current_record: &LevelRoomRecord,
        retained_inactive_rooms: usize,
        active_limit: usize,
        stream_slot_for: impl Fn(RoomIndex) -> u16,
    ) -> bool {
        if self.job.cursor < self.job.requested_count && self.job.next_slot < MAX_ACTIVE_ROOMS {
            return false;
        }
        self.rooms = self.job.rooms;
        self.bump_generation();
        let previous_rooms = self.job.previous_rooms;
        let mut next_slot = self.job.next_slot;
        self.retain_previous_rooms(
            rooms,
            &previous_rooms,
            current_record,
            retained_inactive_rooms,
            active_limit,
            &mut next_slot,
            stream_slot_for,
        );
        self.job = ActiveRoomWindowJob::EMPTY;
        true
    }

    /// Whether the current room and every frustum-visible room (up to
    /// `active_chunk_limit`) is present and drawable in the active window.
    pub fn visible_rooms_are_active<
        const MAX_PORTAL_FRUSTUMS: usize,
        const MAX_PORTAL_FRONTIER_ROOMS: usize,
        const MAX_PORTAL_ROOM_BOUNDS: usize,
    >(
        &self,
        visibility: &RoomVisibility<
            MAX_ACTIVE_ROOMS,
            MAX_PORTAL_FRUSTUMS,
            MAX_PORTAL_FRONTIER_ROOMS,
            MAX_PORTAL_ROOM_BOUNDS,
        >,
        current_room: RoomIndex,
        active_chunk_limit: usize,
    ) -> bool {
        if !active_room_contains_drawable(&self.rooms, current_room, current_room) {
            return false;
        }
        let visible_limit = visibility.visible_room_limit(active_chunk_limit);
        let mut i = 0usize;
        while i < visible_limit {
            if !active_room_contains_drawable(
                &self.rooms,
                current_room,
                visibility.result.rooms[i].room,
            ) {
                return false;
            }
            i += 1;
        }
        true
    }
}
