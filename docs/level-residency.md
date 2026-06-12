# Level residency and room streaming

How rooms, materials, and texture assets reach RAM and VRAM at
runtime. Rewritten 2026-06-12 from the phase-1 streaming audit
(`docs/streaming-audit-2026-06-12.md`); the previous revision
described the embedded `include_bytes!` era and called CD streaming
out of scope, which has not been true since the world-pack loader
shipped.

## Two backing stores

```text
baked     LevelAssetRecord { bytes: include_bytes!(...) }   pinned in the EXE
streamed  world pack on CD: TOC + chunked room data         paged into slots
```

Baked assets (textures, models, UI, small rooms) resolve through the
master asset table (`ASSETS`, keyed by `AssetId`) exactly as before.
Streamed ROOM chunks live in a world pack on disc
(`--world-pack-rooms-dir` / `world_pack_order.txt` at mkiso time) and
are paged into a fixed pool of RAM slots at runtime. The
`cd-stream-bench` feature (a default feature of the playtest) selects
the streamed path.

## The streaming scheduler

`RoomStreamScheduler<N>` (`active_room_streaming.rs`) owns
`N = STREAMED_ROOM_SLOT_COUNT` (currently 6) byte buffers of
`STREAMED_ROOM_SLOT_BYTES` each. Per slot:

```text
StreamedRoomSlot { room, byte_count, last_used, state }
state: Empty | Loading | Resident | Failed
```

`room_slots` is a fixed array map (room index -> slot) whose entries
are validated against the slot's current room and state on every
lookup, so stale entries after slot reuse cannot resolve.

Once per sim tick the residency owner calls `reconcile_residency`
with the desired room window:

1. `begin_window` bumps the epoch and clears the window counters.
2. `set_resident_window` pins exactly the desired set. Pinned rooms
   are never eviction candidates.
3. `plan_window_loads` walks the requests: already-resident rooms
   refresh `last_used`; missing rooms get slots via `choose_slot`
   (free or Failed slots first, then LRU among unpinned, unrequested,
   unreserved residents). Only the active prefix of the request list
   may evict (`protected_count`); prefetch requests never push out
   resident rooms.
4. `start_load_plan` hands the plan to the single
   `WorldRoomSlotsReadJob`, which streams sectors from the world pack
   (`pump`, budgeted by `max_sectors` per tick) and validates a
   per-chunk checksum. Slots promote to Resident per entry as each
   chunk completes; a current-room request that cannot wait aborts
   the in-flight job (`abort_active_load`) and reclaims its Loading
   slots.

### Failure policy

A failed chunk (checksum mismatch, TOC miss, timeout, oversized)
marks the slot Failed and enters the room into a retry backoff:
first retry after 16 reconcile windows (~0.27 s), doubling per
consecutive failure to a cap of 512 windows (~8.5 s), reset by any
successful load. Failed slots count as free for other rooms. The
room is never abandoned; the backoff only bounds how often a
permanently bad chunk can churn the CD and occupy the job pipeline.
`ROOM_STREAM_FAILED_LOADS` counts new failures per window.

## Surface caches: baked pools vs streamed in-place slices

Baked rooms copy parsed geometry into the `ROOM_CACHE_*` static
pools; their cache records index those pools.

Streamed rooms render DIRECTLY out of their slot bytes:
`streamed_room_surface_cache_slices` re-resolves the resident slot
and cross-checks all chunk-view offsets/counts against the cache
snapshot on EVERY call before returning slices, and
`parse_streamed_compact_collision_room` does the same for collision.
A room whose slot was reused fails resolution and the caller falls
back (the room pops for a frame instead of drawing foreign bytes).

**Lifetime contract:** the returned slices are typed `'static` but
point into slot buffers the scheduler overwrites on reuse. They are
valid only until the next streaming step. Never store them across
ticks; re-resolve per use. Holding them longer is sound only for
active-window rooms (pinned against eviction) -- the camera collision
cache relies on that and additionally keys on the streaming RESIDENT
mask, so residency turnover forces a re-gather even while the
incremental active-window job still lags the pin set.

## Materials and VRAM

- Streamed room materials are parsed once at room build and COPIED
  into `ROOM_MATERIAL_POOL[stream_slot]`; they are only consumed
  alongside successfully resolved geometry, so a reused slot's stale
  pool entry is unreachable.
- VRAM is a 64-slot allocator (`vram_runtime.rs`) with per-use CLUT
  modes (opaque, transparent-zero, model atlas, sky panorama). Room
  textures upload on demand and `evict_unreferenced_vram` reclaims
  slots no longer referenced by the streaming window. Menu UI images
  are scoped per UI scene; the sky panorama is gameplay-scoped
  (loaded on gameplay entry, freed on exit). The VRAM seam is a
  phase-2 audit item (slot exhaustion and fragmentation behavior as
  content grows).

## Invariants the render side relies on (verified in the audit)

1. Slot lookups validate room identity and state; stale map entries
   cannot resolve.
2. The pinned window (the active room set) cannot be evicted mid-use.
3. Streamed geometry/collision access re-validates per call; eviction
   degrades to a fallback, never to foreign bytes.
4. Departing rooms (left the window) may pop for the frames between
   unpinning and the active-window job catching up; they cannot draw
   another room's data.

## Related

- `docs/streaming-audit-2026-06-12.md` -- the audit this doc is built
  from, including phase-2 candidates (cd_stream internals, VRAM seam,
  window-job ordering, surface-cache Overflow retry).
- `docs/world-grid-architecture.md` -- the cooked chunk format the
  slots hold.
