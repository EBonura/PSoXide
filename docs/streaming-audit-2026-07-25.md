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

**Status as of 2026-07-25, end of the implementation pass.** Findings 1, 2 and 3
are closed; 4, 5 and 6 remain and each is blocked on the same missing thing.

| # | Finding | Status |
|---:|---|---|
| 1 | Per-room asset residency cooked and ignored | **closed** — pool can reclaim, residency is neighbourhood-scoped, verified |
| 2 | Pump budget unanchored | **closed** — derived from the drive with a floor assertion |
| 3 | Gap discard unbounded | **closed** — capped at the seek break-even |
| 4 | One load job in flight | open — blocked on content, see finding 9 |
| 5 | Pool sized from whole-level figures | open — blocked on content, see finding 9 |
| 9 | Larger projects exceed the 32 KiB room chunk cap | **open — gates 4 and 5** |
| 6 | Miss policy is emergent | open — a design decision, not a defect |

Findings 4 and 5 share a blocker: **cortex_v1 cannot validate either.** Its eight
rooms are small enough that the worst-case neighbourhood is the whole level, so
scoping residency correctly still leaves every persistent asset resident
(measured: 314,952 bytes, the full packed set, with nothing evicted). Shrinking
the cooked budget against that number would change nothing, and a two-slot job
queue has no seek to overlap when the whole world is 54 KB and disc-adjacent.

Both need a synthetic level with rooms deliberately scattered on disc and an
asset set larger than a neighbourhood. Building that generator is the next piece
of work, and it gates the two remaining performance findings rather than being
optional groundwork.

The problems are around it. In priority order:

1. **Per-room asset residency is cooked and then ignored.** Every asset in the
   level is loaded and pinned; the manifest that would allow streaming the
   minimum sits unused.
2. **The pump budget is fiction.** It asks for 1.6x more sectors per vblank than
   a 2x drive can deliver, and the number is not derived from the drive at all.
3. **Seek time is absent from the scheduler's model**, though one seek costs as
   much as eight sectors.
4. **One load job in flight**, so a seek can never overlap a transfer.
5. **Pool capacity is sized from whole-level figures**, which does not
   generalise to a big world.

## 1. Per-room asset residency is cooked and then ignored — critical

**Corrected 2026-07-25 after review.** An earlier draft of this section named
blocking texture reads as the critical hole. That was wrong on two counts and
both corrections matter, so they are recorded rather than quietly edited out.

First, the blocking reads in [`vram.rs:303`](../engine/crates/psx-game-runtime/src/vram.rs#L303)
and [`vram.rs:1524`](../engine/crates/psx-game-runtime/src/vram.rs#L1524) are
the UI image pack and the sky panorama. Both are menu/transition loads, not
per-room gameplay streaming. `read_one_sector_blocking` measuring 2.01% of guest
PC samples on the 900-frame route is consistent with that: the route spends most
of its length in menu and story screens. It is still worth moving off the frame,
but it is not the seamless-world hole.

Second, and more importantly: **room textures are not streamed per room at all.**

### What already exists

[`RoomResidencyRecord`](../engine/crates/psx-level/src/lib.rs#L1344) carries a
complete per-room dependency set:

| Field | Meaning | Cooked? |
|---|---|---|
| `required_ram` | assets that must be RAM-resident to render the room | yes, populated |
| `required_vram` | assets that must be uploaded to VRAM | yes, populated |
| `warm_ram` | neighbour-room RAM hints across open portals | yes, populated |
| `warm_vram` | neighbour-room VRAM hints | yes, populated |

The type's doc comment still claims the warm sets are "Empty in this pass"; the
cooker has outgrown it. cortex_v1 room 0 warms `[14, 17]` in RAM and
`[15, 16, 25, 18]` in VRAM.

The VRAM side already behaves the way a delta streamer should. Uploads are keyed
by asset id, so `find_vram_slot` ([`vram.rs:909`](../engine/crates/psx-game-runtime/src/vram.rs#L909))
skips anything already resident, and `evict_unreferenced_vram`
([`vram.rs:686`](../engine/crates/psx-game-runtime/src/vram.rs#L686)) frees only
what no room in the desired set needs, via `vram_asset_required`. Walking into a
neighbour that shares textures with the current room already costs nothing.

### What is missing

`required_ram`, `warm_ram` and `warm_vram` have **no consumers anywhere in the
engine**. A repo-wide search outside the type definition returns nothing.

Instead, [`model_rendering.rs:55`](../engine/examples/editor-playtest/src/model_rendering.rs#L55)
calls `assets.begin(UI_PACK_START_LBA, UI_PACK_TOC, ASSETS)` — every asset in
the level, unconditionally — and the cooker sizes the pool to match at
[`manifest.rs:161`](../editor/crates/psxed-project/src/playtest/manifest.rs#L161):
`persistent_asset_slot_count = package.assets.len()`.

So the RAM asset pool is whole-level residency by construction. That is the
`PersistentAssetStreamer<154, 56>` at 318,364 bytes the RAM survey found holding
48% of all arena memory, and it is why room textures never need streaming: they
are already there, all of them, always.

### Why this is the blocker for a big world

This is the one finding in this audit that does not scale at all. Room geometry
pages properly; assets do not page. A world ten times the size of cortex_v1
needs roughly 3 MB of asset payload permanently resident in a 2 MB console.

It is also the finding with the least work behind it, because the hard part —
knowing exactly what each room needs, and what its neighbours will need next —
is already cooked and sitting unused.

### The structural blocker: asset storage cannot evict

One more correction to the picture above. `begin` does not load *every* asset: it
filters on `asset_flags::STREAMED_GAMEPLAY_PERSISTENT`, which in cortex_v1 is 22
of 56 assets (26 are static, 7 UI, 1 transient). So the problem is whole-level
residency *for the gameplay-persistent class* — which is precisely the class that
grows with world size, so the conclusion is unchanged.

The reason that class cannot page is its storage. `PersistentAssetStorage`
([`asset_streaming.rs:21`](../engine/crates/psx-game-runtime/src/asset_streaming.rs#L21))
is a **monotonic bump allocator**: `offsets`, `lengths`, `used_bytes` and
`prepare_slot`, with no release path. Once an asset is placed it can never be
freed, so no amount of manifest plumbing will make assets stream until the
allocator can reclaim.

The template already exists in the same crate. `StreamedRoomPages` has
`release_slot`, `free_page_count`, and fragmented-pool compaction with live-byte
preservation, all covered by four passing paged-room tests. The asset streamer
needs the same treatment.

That reorders the work: the allocator is the prerequisite, not the plumbing.

### Hazard for the wiring: `Model<'static>` borrows the pool

Found while wiring step 2, and it constrains the design rather than being a
detail. `RuntimeModelAsset` retains `model: Model<'static>`
([`model_rendering.rs:132`](../engine/crates/psx-game-runtime/src/model_rendering.rs#L132)),
a parsed view over the asset bytes. Its face, part and vertex pools are owned
copies, so those are safe, but the `Model` itself is a borrow.

Releasing an asset never moves bytes, so eviction alone is safe. **Compaction
is not.** It runs inside `prepare_slot` whenever appending no longer fits, which
is exactly what a portal crossing that needs new assets will trigger, and it
would leave every `Model<'static>` pointing at moved bytes. The failure would be
silent corruption of mesh data far from its cause, which is the worst kind of bug
to inherit.

`layout_generation` exists for this. The wiring must compare it across
`request_rooms` and force a model rebuild when it changes, via the existing
`runtime_models_loaded` flag that already gates `load_runtime_models`. Note that
the rebuild is synchronous and also re-uploads atlases, so it needs a frame-time
budget of its own; dropping it into a gameplay tick unmeasured is not acceptable.

The cheaper alternative worth measuring first: keep model assets pinned for the
level and scope only textures and room payloads to the neighbourhood. Models are
a small share of the persistent class and pinning them removes the compaction
hazard entirely.

### Shape of the work

1. **Give `PersistentAssetStorage` a page pool with release and compaction**,
   modelled on `StreamedRoomPages`. Nothing downstream is possible without it.
2. Replace the `begin(..., ASSETS)` call with a residency request driven by the
   active-room window: the union of `required_ram` over resident rooms plus
   `warm_ram` over their portal neighbours. Keep `begin`'s one-shot semantics for
   the initial neighbourhood so the boot loading screen's `progress_q12` and
   `ready` keep working, and add an incremental top-up for later crossings.
3. Give the persistent streamer a RAM-side twin of `evict_unreferenced_vram`,
   using the same union as the keep-set. `vram_asset_required` is the template;
   it needs a `ram_asset_required` sibling reading `required_ram`/`warm_ram`.
4. Consult `warm_vram` in the VRAM path too. Today only `required_vram` is read,
   so neighbour textures upload on arrival rather than ahead of it.
5. Only then size the pool from the worst-case neighbourhood rather than
   `package.assets.len()`. Shrinking it before eviction works would make rooms
   late in a large level unloadable.

Steps 1 and 2 give delta streaming for free: an asset already resident is
already in the union and is never re-requested. That is precisely the
"stream the bare minimum" behaviour the goal needs, and the manifest to do it
has been cooked into every build for some time.

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

If alternate rooms land (finding 1 of the the reference engine architecture review), a room with
a flip variant has two payloads. Pinning both would double the working set for
exactly the rooms most likely to be large set pieces.

Cook them as separate chunks keyed by `(room, flip_state)` and pin only the live
variant. The residency reconcile already keys by `RoomIndex`, so the change is
in the key rather than the mechanism. The neighbourhood sizing rule in finding 5
must then take the maximum over flip states as well.

## 9. The larger projects cannot stream at all — critical for the goal

Found while trying to validate findings 4 and 5 against real content rather than
a synthetic level. cortex_v2 carries 658 material references against cortex_v1's
22, which is exactly the asset scale needed to prove neighbourhood residency
evicts anything. It does not cook:

```text
[cook-playtest] write failed: room 0 stream chunk is 37576 bytes; the runtime
room slot is 32768 bytes (psx_level::MAX_STREAMED_ROOM_CHUNK_BYTES) -- split the
room with more portal seams or reduce its geometry
```

The cooker is right to refuse, and the check is well placed: a chunk larger than
the runtime slot could never be made resident. But the consequence is that
**cortex_v1 is currently the only streamable project**, and it is far too small
to exercise the streaming system it is the sole test case for.

### Root cause: the split rule ignores the byte budget

The error message asks the author to add portal seams, which misdirects. Chunking
is already automatic: cortex_v1's eight chunks come from ONE authored room. The
partition happens in [`playtest.rs:315`](../editor/crates/psxed-project/src/playtest.rs#L315),
which walks `plan.rooms` from the portal-room plan and cooks one chunk per portal
room.

That plan is derived from portal seams alone. Nothing in it consults
`MAX_STREAMED_ROOM_CHUNK_BYTES`, so a portal room with no internal seams may cook
to any size at all, and the violation is only caught later at manifest write.

For a seamless world this is the wrong place to put the constraint. An author
building a large hall should not have to reverse-engineer a 32 KiB cooked-byte
budget by inserting seams until the cook stops failing. The cooker knows the
cooked size of every candidate chunk, so it should subdivide a portal room on the
grid until each piece fits, generating the interior seams itself.

That is the fix, and it is not small: subdividing a portal room means synthesising
portals at each cut and rebuilding the neighbour links, visibility sets and portal
records that `plan.portals` and `portal_room.neighbours` carry. It belongs in the
portal-plan stage, not as a post-pass over cooked bytes.

**Note for whoever picks this up:** `cook-playtest` overwrites `generated/` and
leaves it incomplete when it fails, so a failed cook of one project destroys the
previously cooked manifest of another. Recook the working project immediately, or
make the writer stage to a temporary directory and swap on success.

This reframes findings 4 and 5. They are not blocked on a tool nobody has
written; they are blocked on authored content that fits the runtime contract.
Splitting cortex_v2's room 0 along more portal seams would produce, in one step,
both the scattered-room disc layout finding 4 needs and the larger-than-a-
neighbourhood asset set finding 5 needs, out of real content rather than a
synthetic fixture.

It is also a live constraint on the seamless-world goal in its own right. A large
continuous world means many rooms, and every one of them must stay under 32 KiB
of cooked chunk. That is an authoring budget the editor should surface while
building a room, not a cook-time failure discovered later.

## 10. Unblocking the scale measurement: add one portal seam

The chunk-overflow finding above led me toward a cooker change. That would have
been wrong, and the reason is worth recording.

`plan_portal_rooms` ([`portal_rooms.rs:207`](../editor/crates/psxed-project/src/portal_rooms.rs#L207))
splits a room on authored portal seams only, deliberately. `PortalRoomConfig`
already carries `max_width`, `max_depth`, `max_triangles` and `max_bytes`, and
`over_budget()` already flags every violating room, with `over_budget_count()` on
the plan. The budget check exists, runs, and marks the offenders. The design is
that the AUTHOR owns room boundaries and the tool reports violations.

So auto-subdividing in the cooker would override a deliberate decision, the same
class of mistake as changing the failed-cook manifest contract that
`failed_cook_removes_stale_cooked_manifest` pins. The engineering gap is only
that `over_budget_count()` is not surfaced while authoring, so a violation
appears at cook time instead of while building the room.

cortex_v2's room 0 is 4,808 bytes over. It needs one seam.

### Portal marker convention (established by two failed attempts)

Adding a seam by hand has three requirements, and missing any one makes the
marker silently vanish with the chunk byte-identical -- no warning, no counter.

1. **The marker must be in the Room node's `children` array.**
   `collect_portal_seams` filters on `scene.is_descendant_of(node.id, room_node)`
   ([`portal_rooms.rs:347`](../editor/crates/psxed-project/src/portal_rooms.rs#L347)).
   Setting only the child's `parent: Some((<room>))` is NOT enough. This was the
   first failed attempt.

2. **The translation is in editor units, not world cells.**
   `portal_edge_key_for_node` routes it through
   `WorldGrid::editor_to_world_cells`, which is
   `editor + grid_center_cells()` ([`world_types.rs:1716`](../editor/crates/psxed-project/src/world_types.rs#L1716)).
   So `world_cells = translation.xz + grid_centre`, then the array cell is
   `world_cells - grid.origin`, floored, with the fractional part choosing the
   edge. Treating translation as world cells was the second failed attempt.

3. **Both cells across the chosen edge must be populated.**
   The edge candidate list requires `populated(grid, x, z) && populated(grid, nx, nz)`
   ([`portal_rooms.rs:381`](../editor/crates/psxed-project/src/portal_rooms.rs#L381)),
   and the winning direction is whichever edge the fractional position is nearest:
   `local_z` for South, `1 - local_x` for East, `1 - local_z` for North,
   `local_x` for West. A marker in an empty cell, or on the outer boundary,
   yields no edge at all.

So placing a seam programmatically means reading the sector grid, finding a
populated cell pair spanning the intended cut, and solving 2 backwards for the
translation. Doing it in the editor instead is far cheaper: drop a Portal marker
on the wall you want to divide and it satisfies all three by construction.

### Recipe for a throwaway streaming test fixture

The author's own level should not be reshaped to serve a measurement. Copy it:

1. `cp -R editor/projects/cortex_v2 editor/projects/cortex_v2_streamtest`
2. In its `project.ron`, add a `Portal` node parented to the Room node, following
   the shape cortex_v1 uses:
   `kind: Portal(target_room: None, target_entry: "", entry_name: "portal_<x>_<z>_<dir>")`
   with a fresh unique node id, a `transform.translation` on the seam cell, and the
   Room node's id in `parent`. cortex_v1 carries seven of these; copy one and
   change the cell, direction and id.
3. Place the seam so neither resulting chunk exceeds
   `MAX_STREAMED_ROOM_CHUNK_BYTES` (32,768). Room 0 is 37,576 bytes, so a roughly
   central cut is sufficient.
4. `cook-playtest projects/cortex_v2_streamtest/project.ron`, then
   `make build-editor-playtest` and the mkisopsx step.

That yields 658 material references across multiple chunks: an asset set larger
than one neighbourhood and a disc layout with real gaps. Findings 4 and 5 both
become measurable from it in a single cook, via
`persistent asset resident bytes` and `persistent asset load failures`.

**Back up `generated/` first.** A failed cook still discards it (finding 1's note).

## Recommended order

1. **Consume the residency manifest** (finding 1). Nothing else here matters if
   assets cannot page, and the cooked data already exists.
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
