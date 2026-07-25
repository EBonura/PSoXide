# Room streaming audit against the seamless-world goal

Date: 2026-07-25
Branch: `perf/engine-30fps`
Goal under audit: a large continuous world where the current room plus its
neighbours stay resident and further rooms load from CD as the player walks.

## Method

Code read at `perf/engine-30fps`. Runtime figures come from the cortex_v1
recorded tape and the fixed 900-frame route. Drive figures come from the
emulator's CD timing model in
[`cdrom/timing.rs`](../emu/crates/emulator-core/src/cdrom/timing.rs), which is
transcribed from hardware-oriented references rather than invented, so the
budget arithmetic below is meaningful rather than decorative.

Reference quantities:

| Quantity | Value | Derivation |
|---|---:|---|
| `CD_READ_TIME` (one sector, single speed) | 451,584 cyc | `33,868,800 / 75` |
| One sector, double speed | 225,792 cyc | `CD_READ_TIME / 2` |
| Seek (`SEEK_SECOND_RESPONSE_CYCLES`) | 1,806,336 cyc | `CD_READ_TIME * 4` |
| One NTSC vblank | 564,480 cyc | |
| **Sectors deliverable per vblank at 2x** | **2.50** | |
| **Seek cost in vblanks** | **3.20** (~53 ms) | |

## Verdict

The room-geometry streamer is well built and is not the problem. It is
non-blocking, budgeted, LRU-evicted, and it degrades by not drawing rather than
by stalling. Four paged-room tests and three asset-streamer tests cover it.

The problems are around it. In priority order:

1. **Texture loading blocks the frame.** Room geometry streams asynchronously;
   the textures those rooms need do not.
2. **The pump budget is fiction.** It asks for 1.6x more sectors per vblank than
   a 2x drive can deliver, and the number is not derived from the drive at all.
3. **Seek time is absent from the scheduler's model**, though one seek costs as
   much as eight sectors.
4. **One load job in flight**, so a seek can never overlap a transfer.
5. **Pool capacity is sized from whole-level figures**, which does not
   generalise to a big world.

## 1. Texture loading blocks the frame — critical

Room chunks are read through the non-blocking job path:
[`cd_stream.rs:405`](../engine/crates/psx-game-runtime/src/cd_stream.rs#L405)
calls `try_read_stream_sector`, which returns `Ok(false)` when no sector has
landed. The scheduler's `pump` advances the job by at most `max_sectors` and
returns. That is the correct shape and it is what makes room streaming
seamless today.

Texture and VRAM assets do not use it. [`vram.rs:303`](../engine/crates/psx-game-runtime/src/vram.rs#L303)
calls `read_chunks_contiguous` and [`vram.rs:1524`](../engine/crates/psx-game-runtime/src/vram.rs#L1524)
calls `read_chunk_blocking`. Both spin inside `read_one_sector_blocking`
([`cd_stream/hw.rs:240`](../engine/crates/psx-game-runtime/src/cd_stream/hw.rs#L240)),
which waits on `IRQ_DATA_READY` with a poll limit before DMAing the sector.

Measured: `read_one_sector_blocking` was **2.01% of all guest PC samples** on the
900-frame route, ranking 13th of 123 symbols. Each blocked sector costs
225,792 cycles at 2x — **0.4 of a vblank per sector**. A texture that needs four
sectors stalls the frame for more than a full vblank before any seek.

This is the hole in the seamless-world story. Walk into a room whose geometry
prefetched correctly but whose textures did not, and the frame stops.

It also explains a finding from the RAM/VRAM survey that looked contradictory
there: 28 room-material texture drops and 128 upload-queue-full events against
only 21 successful uploads, while a quarter of VRAM sat untouched. The address
space was never the constraint. The upload path was, and behind it a blocking
CD read that the frame budget cannot absorb.

**Fix.** Move VRAM asset loading onto the same non-blocking job the room
streamer uses, and let the material table keep the previous texture until the
new one lands. The second half of that already shipped on this branch
(`19663b55`); the first half has not.

## 2. The pump budget is not derived from the drive

[`runtime_schedule.rs:19`](../engine/examples/editor-playtest/src/runtime_schedule.rs#L19)
sets `stream_pump_sectors_per_tick: 8`, and [`main.rs:589`](../engine/examples/editor-playtest/src/main.rs#L589)
pumps only on background ticks, which are odd sim ticks. That is 8 sectors per
two vblanks, or **4.0 sectors per vblank requested against 2.50 deliverable**.

This is not fatal, because the job simply makes less progress per pump than
asked. But it means the number encodes no real budget: it cannot be used to
reason about how far ahead the prefetch must run, and it will silently mislead
anyone tuning it. Derive it from `CD_READ_TIME` and the configured drive speed
instead, and it becomes a statement about the hardware rather than a guess.

## 3. Seek time is missing from the scheduler's model

The load plan is expressed in sectors
([`RoomStreamLoadPlan`](../engine/crates/psx-game-runtime/src/room_streaming.rs#L100)).
Nothing in it accounts for seeks, yet one seek costs 1,806,336 cycles — **the
same as eight sectors at 2x, or 3.2 vblanks**.

For cortex_v1 this is invisible: the whole world is 54,220 bytes and the largest
room is 9 sectors. For a big world it dominates. Loading one 9-sector room from
a cold position costs 3.2 vblanks of seek plus 3.6 vblanks of transfer, about
113 ms, or roughly three and a half frames at 30 fps.

Two mitigations already exist in part. `world_pack_order.txt` means disc layout
is controllable at cook time, so chunks can be ordered along likely traversal.
And `read_chunks_contiguous` deliberately reads and discards gap sectors
([`cd_stream.rs:912`](../engine/crates/psx-game-runtime/src/cd_stream.rs#L912))
to keep one continuous `ReadN` rather than reseeking, which is the right trade.

What is missing is the threshold. Discarding gap sectors beats a reseek only
while the gap is under about eight sectors at 2x; past that, seeking is cheaper.
The discard loop is currently unbounded. Cap it at the computed break-even and
the trade becomes principled instead of incidental.

## 4. One load job in flight

`pump` returns immediately unless a job is active
([`room_streaming.rs:602`](../engine/crates/psx-game-runtime/src/room_streaming.rs#L602)),
and a completed job resets to `WorldRoomSlotsReadJob::new()`. There is exactly
one job at a time.

The consequence is that the seek for the next room can never overlap the
transfer of the current one. With seek at 3.2 vblanks, a level whose rooms are
not disc-adjacent pays that serially for every load. A two-slot job queue, where
the next seek is issued as the current transfer drains, would hide most of it.

This matters much more once the world is big enough that consecutive rooms are
not adjacent on disc.

## 5. Pool capacity is sized from the whole level

`STREAMED_ROOM_SLOT_COUNT` and `STREAMED_ROOM_PAGE_COUNT` come from cooked
figures `WORLD_STREAM_SLOT_COUNT` (8) and `WORLD_RESIDENT_PAGE_COUNT` (30).
That gives `StreamedRoomPages<30, 8>` at 61,492 bytes.

cortex_v1's entire room payload is 54,220 bytes. The paging pool is **larger
than the world it pages**, which the RAM/VRAM survey already flagged.

For the seamless-world goal the sizing rule is wrong in shape, not just in
value. What the pool must hold is the worst-case *neighbourhood* — the current
room, its shoulder rooms, and whatever is in flight — not a fraction of the
level. The cooker can compute that directly: walk the room graph, take the
maximum over all rooms of (room + its neighbours + one in-flight room), and emit
that. It is then independent of level size, which is exactly the property a big
world needs.

## 6. Miss policy: rooms silently do not draw

A room that is not resident is skipped and recorded in `visible_missing_mask`
([`active_rooms.rs:29`](../engine/examples/editor-playtest/src/active_rooms.rs#L29)).
There is no stall and no placeholder.

For seamlessness this is the right default: never freeze the game for the disc.
But it means fast traversal produces visible holes. The recorded route logged
**138 misses across 2,450 room requests**, about 5.6%, in a world small enough
to fit in RAM twice over.

This needs an explicit policy rather than an emergent one. The options are a
prefetch distance guaranteed to cover maximum traversal speed, a soft gate that
slows the player at a chunk boundary, or an authored fade. Whichever is chosen,
`visible_missing_mask` should drive it instead of being telemetry only.

## 7. CD-DA contention is a real hardware risk

The emulator explicitly models extra command latency while Red Book audio plays,
and the comment in [`timing.rs`](../emu/crates/emulator-core/src/cdrom/timing.rs)
is blunt about why: emulators that acknowledge every command in a flat ~2048
cycles hide a stall that is real on silicon, and a missed poll can reseek and
kill the audio.

A seamless world with streaming music therefore contends for the same drive on
every room load. This has bitten the project before and was fixed at the engine
level, but the fix predates the current streaming shape and should be re-tested
once loads become frequent.

## 8. Interaction with alternate rooms

If alternate rooms land (finding 1 of the TR5 architecture review), a room with
a flip variant has two payloads. Pinning both would double the working set for
exactly the rooms most likely to be large set pieces.

Cook them as separate chunks keyed by `(room, flip_state)` and pin only the live
variant. The residency reconcile already keys by `RoomIndex`, so the change is
in the key rather than the mechanism. The neighbourhood sizing rule in finding 5
must then take the maximum over flip states as well.

## Recommended order

1. **Move VRAM asset loading onto the non-blocking job** (finding 1). It is the
   only item here that stalls a frame today, and it is measurable immediately
   through `read_one_sector_blocking` disappearing from the PC profile.
2. **Size the page pool from the room graph's worst neighbourhood** (finding 5).
   Cheap, cook-time, and it is what makes the design independent of world size.
3. **Derive the pump budget from `CD_READ_TIME`** (finding 2), then cap the
   gap-sector discard at the computed break-even (finding 3).
4. **Two-slot job queue** so a seek overlaps a transfer (finding 4).
5. **Decide the miss policy** (finding 6) before the world gets big enough for
   5.6% to be visible.

Items 1 to 3 are small and independent. Item 4 is the one that determines
whether a genuinely large world streams without hitching, and it should be
measured with a synthetic level whose rooms are deliberately scattered on disc,
because cortex_v1 is far too small to expose it.
