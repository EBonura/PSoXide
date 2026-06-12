# Streaming system audit, phase 1 (2026-06-12)

Read-only audit of the room-streaming path, done as the opener of the
engine-solidity track. Every claim below was verified directly in
source; an earlier agent-generated sweep contained fabrications
(a HashMap on the guest, a u64 pin bitmask, a room-blind quad pool)
and was discarded after spot-checks. File references are to the
state at commit 6a4d46ef.

## System map (verified)

- `RoomStreamScheduler<N>` (active_room_streaming.rs:49): N slots
  (`STREAMED_ROOM_SLOT_COUNT`, currently 6), each
  `StreamedRoomSlot { room, byte_count, last_used, state }` with
  state Empty / Loading / Resident / Failed. `room_slots` is a fixed
  array map room-index -> slot; `pinned_rooms` is a `[bool]` per room
  index. One `cd_stream::WorldRoomSlotsReadJob<N>` at a time.
- Per sim tick, the residency owner calls `reconcile_residency`
  (line 119): bump epoch, pin the desired window, `plan_window_loads`,
  `start_load_plan`. `pump` advances the CD job and commits completed
  entries (`commit_ready_job_entries` promotes slots to Resident as
  each chunk's bytes and checksum complete).
- Surface caches: baked rooms slice into the `ROOM_CACHE_*` static
  pools; STREAMED rooms slice directly into the slot byte buffers
  (`streamed_room_surface_cache_slices`, active_room_cache.rs:533).
- Materials for streamed rooms are COPIED into
  `ROOM_MATERIAL_POOL[stream_slot]` at build (active_room_cache.rs:256).
- Prebuilt room-quad pool is keyed by room index with per-surface
  valid bytes zeroed on slot claim (prebuilt_room_quads_for), so slot
  turnover cannot leak packets across rooms.

## Properties verified SAFE

1. **Stale map entries are harmless.** `mapped_slot_for`
   (active_room_streaming.rs:175) validates `slots[slot].room == room
   && state == wanted` on every lookup, so `room_slots` entries left
   behind by slot reuse cannot resolve.
2. **Active rooms cannot be evicted mid-use.** The whole desired
   window is pinned before planning (`set_resident_window` in
   `reconcile_residency`), and `choose_slot` (line 545) skips pinned,
   requested, and plan-reserved slots; eviction is LRU over the
   remainder only.
3. **The streamed render path re-validates per call.**
   `streamed_room_surface_cache_slices` re-resolves the resident slot
   and cross-checks all eight chunk-view offsets/counts against the
   cache snapshot (active_room_cache.rs:550-558) before returning
   slices; a reused or mismatched slot returns None and the caller
   falls back. Same per-call re-resolution in
   `parse_streamed_compact_collision_room`.
4. **Abort path for urgent loads exists.** A current-room request
   that cannot wait aborts the in-flight job and returns its Loading
   slots to Empty (`abort_active_load`, line 298).
5. **Departing rooms degrade, not corrupt.** A room that leaves the
   pinned window while still in the active array fails slice
   re-resolution after its slot is reused, so it stops drawing (a
   transient pop) rather than drawing another room's bytes; its stale
   material-pool entry is unreachable without geometry.

## Findings (ordered by priority)

1. **STALE DESIGN DOC.** `docs/level-residency.md` describes the
   embedded `include_bytes!` era and declares CD streaming "out of
   scope"; the shipped system streams chunks from CD with slots,
   prefetch, eviction, and failure states. The doc no longer
   describes the system and should be rewritten from this audit (or
   replaced by it) before the game-states work builds on streaming.
2. **NO FAILURE LATCH: failed chunks retry forever.** `Failed` slots
   are treated as free (`choose_slot` first loop) and a failed room
   is neither resident nor loading, so `plan_window_loads` reschedules
   it every frame. A permanently bad chunk (TOC mismatch, scratched
   disc) causes perpetual CD seek churn, and each retry occupies the
   single job pipeline, delaying other loads. Recommend: per-room
   failure counter with a cooldown/backoff and a latched telemetry
   signal once a chunk fails N times.
3. **'static LAUNDERING IS DISCIPLINE-ENFORCED.**
   `streamed_record_slice` casts slot-buffer bytes to
   `&'static [T]` (active_room_cache.rs:606). Soundness currently
   holds because every consumer re-resolves per call and nothing
   holds the slices across a pump; the type system does not enforce
   this, and a future caller caching one of these slices across ticks
   would be a silent use-after-overwrite. Recommend: a wrapper handle
   carrying the (slot, room, byte_count) it was resolved from, plus a
   debug-build revalidation hook, or at minimum a loud doc contract
   on the three laundering functions.
4. **WINDOW-CHANGE POP ORDERING.** Eviction (reconcile, early in the
   tick) can reuse a departing room's slot before the incremental
   active-window job removes that room from the active array, so the
   room pops out for the interim frames. Cosmetic, by design today;
   if pops become visible at window boundaries, update the active
   array before unpinning rather than after.
5. **LRU epoch wrap is theoretical.** `epoch` is a u32 bumped per
   window; after a wrap, `last_used` comparisons invert for one
   window. Sessions cannot realistically reach 2^32 windows; noting
   it so nobody rediscovers it.

## Not yet audited (phase 2 candidates)

- `cd_stream.rs` internals: checksum algorithm strength, the
  group/abort state machine across frames, timeout handling.
- The VRAM seam: `evict_unreferenced_vram` cadence vs streaming
  window changes, slot exhaustion behavior at
  `MAX_RESIDENT_VRAM_ASSETS`, fragmentation as content grows.
- The incremental active-window job (`ActiveRoomWindowJob`) rebuild
  ordering and its interaction with the portal-visibility mask.
- Surface-cache Overflow handling (does an Overflow room ever retry
  after the pool frees up?).

## Phase 2 (same day): cd_stream internals, VRAM seam, Overflow, window job

Verified directly in source, continuing the phase-1 method.

### cd_stream.rs

- **Checksum is FNV-1a 32-bit** (FNV_OFFSET/FNV_PRIME), accumulated
  per entry as sectors arrive and checked both at early commit
  (`completed_entries`) and at job finish. Proper mixing, not a naive
  sum; combined with CIRC this is adequate for room chunks.
- **Multi-frame group state is sound.** `processed[]` persists across
  pumps; `group_entries[]` clears only when a group completes; a
  partial group resumes from `sector_offset` with the ReadN stream
  still rolling. The phase-1 agent claim of per-frame state reset was
  false.
- **FIXED: late `fail_all` demoted verified rooms.** A group error
  (timeout, CD error) fails ALL job entries, including chunks that
  already completed, checksum-verified, and were early-committed
  Resident by `commit_ready_job_entries`. `commit_window_loads` then
  unconditionally demoted those healthy slots to Failed and charged
  their retry backoff. Now guarded: an entry whose slot is already
  Resident for the same room is kept (it holds verified bytes) and
  its failure counter is not charged.
- `start()` validates TOC presence and slot byte capacity per entry
  up front; `abort()` pauses the drive and resets cleanly; the
  begin-group pump intentionally defers the first sector read to the
  next tick (seek pacing).

### VRAM seam

- Slot-table exhaustion is already observable:
  `VRAM_SLOT_TABLE_FULL` counts the otherwise-silent drop
  (vram_runtime.rs `next_vram_slot`).
- `evict_unreferenced_vram` only touches READY room-texture-class
  slots (Opaque/TransparentZero) not referenced by the desired
  window; model atlas and sky modes are scoped elsewhere, and the
  ready guard honors `free_vram_slot`'s pending-upload contract.
- Remaining growth risk is allocator fragmentation of the texture
  band as content diversifies; revisit when a level approaches the
  64-asset table or the band fills.

### Surface-cache Overflow

There is no build to retry: `active_room_surface_cache_for` derives
the descriptor FRESH per refresh from cooked data (baked table or
streamed chunk-view header). `Overflow` therefore means the room's
COOKED vertex count exceeds `MAX_CACHED_ROOM_VERTICES` permanently;
such a room renders forever through the slower uncached fallback,
visible as persistent `room_cache_fallback_draws`. Recommendation
(cooker-side, future): warn at cook time when a room exceeds the
runtime cache cap instead of silently shipping a slow room.

### Window-job ordering

The incremental active-window job lags the pin set by design;
departing rooms pop out via slice re-validation rather than drawing
stale bytes, and the camera collision cache now keys on the resident
mask (phase-1 hardening), which closed the only stale-parse window
found. No further change needed at current scale.
