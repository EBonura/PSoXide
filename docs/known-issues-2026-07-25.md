# Known issues carried forward from the 30 fps branch

Date: 2026-07-25
Branch of origin: `perf/engine-30fps`

Three items are deliberately merged unfixed. Each has a measured cost and a
diagnosed cause, so none of them needs rediscovering.

## 1. Cardinal walls draw both faces, costing render time

**State:** shipped as a correctness fix, not the final form.

`wall_material_for_direction` forces `SurfaceSidedness::Both` for every cardinal
wall ([`world_render.rs:241`](../engine/crates/psx-engine/src/world_render.rs#L241)).
That fixed the cortex_v1 report where a wall vanished as soon as the player
entered the cell that owns it: the winding made the owning cell's interior the
back face, and for a wall bounding the playable area that is the only side a
player can stand on.

**Cost, measured on the fixed 900-frame route:** room surface draw +9.09%,
render +4.34%, visual frames 532 -> 518, deadline misses 65 -> 87. Emitted
primitives rise 302 -> 325 per gameplay render, and those 23 are the wall faces
that were previously dropped, so much of the cost is legitimate work.

**The proper fix, and why it is not hard.** The cooker has the grid, so for any
wall it knows which neighbouring cell is open and which is solid. Emitting each
wall single-sided facing its open side is correct *and* free. Only a wall with
walkable cells on both sides needs both faces.

### Design, worked out 2026-07-25

Sidedness currently comes from the MATERIAL, and walls share materials, so per-
wall orientation cannot be expressed there. It needs a per-SURFACE bit.

`CachedRoomSurface::kind_flags` has room: bits 0-1 are the kind, bit 6 is
`CACHED_SURFACE_HORIZONTAL_NON_FLAT`, bit 7 is `CACHED_SURFACE_HAS_BAKED_RGB`.
**Bits 2-5 are free** ([`world_render.rs:720`](../engine/crates/psx-engine/src/world_render.rs#L720)).

The rule follows from `cardinal_wall_backs_face_their_owning_cell`: a wall's back
faces the cell that owns it, so by default it is visible only from the
neighbouring cell. Therefore:

- Neighbour in the wall's direction absent or non-walkable -> the owning cell is
  the only side a player can occupy, so **flip the wall** and keep it
  single-sided. This is the cortex_v1 case: the corridor's west boundary wall had
  no neighbour west of it.
- Neighbour walkable on both sides -> genuinely needs `Both`.

Touch points:

1. Cooker: `blocker_mask_for_sector` in
   [`cook_world.rs:58`](../editor/crates/psxed-project/src/playtest/cook_world.rs#L58)
   already tests per-direction wall solidity, and the sector grid gives neighbour
   presence, so both inputs are in hand.
2. Cooked format: set the new bit in the surface's `kind_flags`.
3. Runtime: `wall_material_for_direction`
   ([`world_render.rs:241`](../engine/crates/psx-engine/src/world_render.rs#L241))
   stops forcing `Both` and honours the bit.

Verify by replaying the recorded tape: walls must stay visible everywhere they
are today, and the render stage should give back most of the +9.09% room-surface
cost. Do NOT judge it on a single position -- the earlier global-flip attempt
looked plausible on one frame and had inverted every already-correct wall.

**Do not repeat this mistake.** An attempt to take the cheap path removed the
sidedness swap *globally*, which flipped every wall including those already
facing correctly, so more walls disappeared and the experiment was misread as
"the data needs both faces". Orientation must be decided per wall from grid
adjacency, never by a global flip.

## 2. Box props reserve 8 pre-baked break shards each

**State:** capacity is now cooked from the authored prop count
(`BOX_PROP_STATE_COUNT`), which recovered 172,900 bytes. The per-slot cost is
untouched.

Each `BoxPropRuntime` slot is about 1,406 bytes
([`box_props.rs:186`](../engine/crates/psx-game-runtime/src/box_props.rs#L186)):

| Field | Count | Contents |
|---|---:|---|
| `faces` | 6 | four `WorldVertex`, a centre and an `[i32; 3]` normal each |
| `break_shards` | **8** | pre-baked destruction fragments |
| bounds | — | cull sphere, `floor_y`, `ground_y`, debris bounds, AABB min/max |

Six faces of derived geometry is only a few hundred bytes. **The eight break
shards are the bulk, and every box carries a full set whether or not it can
break.**

All of it is derived data, rebuilt from `LevelBoxPropRecord`, which already
holds the eight authored vertices, tints and baked vertex RGB. It is a cache,
not source, so it can be shrunk or streamed. Cheapest first:

1. **Allocate shards only for breakable boxes.** A static crate needs none.
2. **Scope box-prop runtime to the active room window.** The machinery now
   exists: `required_ram`/`warm_ram`, a reclaiming asset pool, and delta
   residency. Box props are per-room and fit the same pattern as textures.
3. Rebuild faces on demand rather than caching, if the CPU cost measures
   acceptable.

This is also the best candidate for explaining an unresolved regression: cooking
the capacity cost 10 delivered frames and +13 deadline misses, and padding the
arena back did not recover them, which points at code/data layout rather than
size. Removing the cache that should not exist is more likely to come out net
positive than resizing it was.

## 3. Floor tiles vanish near the player in cortex_v1 rooms 6/7 -- FIXED

**Resolved 2026-07-25.** `submit_tomb_raider_cached_room_quad` discarded its
submit result with `let _` and returned `true` unconditionally. The caller reads
`true` as "this surface is drawn" and skips its own whole-quad submit, so any
subdivision that emitted nothing became a silent hole. It now returns whether
geometry actually reached the sink, and a failed subdivision falls back to the
authored quad. Verified on the recorded tape: every hole gone, subdivision still
enabled, so near-floor affine correction is retained.

The history below is kept because six hypotheses were falsified before this one,
all of them assuming something REJECTED the surface. Nothing did; a failure was
swallowed. The lesson is that a function returning `bool` while discarding a
richer result is worth suspecting when geometry vanishes with no counter moving.

### Original investigation

**State:** open, but **localised to floor subdivision, and pre-existing.**

Two experiments closed this down on 2026-07-25.

*Provenance.* Built at `e78b44fe`, the last commit before any render change on
`perf/engine-30fps`, and replayed the same tape. The holes are identical. None
of the three render-path changes on that branch (double-sided walls, material
stand-in, texture eviction) caused it. Getting there needed a workaround: the
older cooker hard-errors on cortex_v1's empty `.wav` files while the current one
tolerates them, so valid 16-bit stubs were written into a throwaway copy. That
cook produced identical geometry (8 rooms, 234 cells, 889 tris), which is what
makes the comparison meaningful.

*Cause.* Setting `ROOM_TOMB_RAIDER_SUBDIVISION_KINDS` to `WALL` only, disabling
floor subdivision, **removes every hole**. The floor renders textured and
continuous across the whole route. So the loss is in the Tomb Raider subdivision
path for floor surfaces: the generated leaf quads, not the authored surface,
which is consistent with everything else already ruled out.

The original report said "only when tessellating them" and was correct. It was
argued down on a `split_tris` reading of 0, but that counter measures the
HARDWARE splitter, not TR subdivision, so it was never evidence either way.

*Also ruled out: the leaf depth key.* Floors take a prepared whole-quad depth
while walls take a different branch, and a floor recedes steeply, so its leaves
span a far wider depth range than a wall's. Plausible, and wrong: forcing
`CachedRoomDepthMode::PerTriangle`, so every leaf computes its own depth,
leaves the holes unchanged.

*The `tessellation-debug` capture, and what it changes.* Run over the same tape,
leaves colour cyan, underdraw yellow and un-subdivided roots red. Walls show
cyan and yellow. **The affected floor shows none of the three.**

That rules out leaf emission, which was the previous suspect. A leaf failing to
emit would leave its siblings drawn, so the floor would appear as cyan patches
with gaps. Contributing no debug-coloured geometry at all -- not even red, the
un-subdivided root colour -- means those floor surfaces never reach the
colouring path.

So the failure is UPSTREAM of leaf emission, at the point a surface is routed
into the subdivision path rather than inside it. That is consistent with
disabling floor subdivision restoring the floor: the subdivision *decision* is
implicated, not the leaves it produces.

*Also ruled out: the warmed path's backface early return.* That function returns
`true` ("handled") without drawing, which matches the no-debug-colour signature
exactly. Disabling it entirely leaves the holes unchanged, so it is not the
silent drop.

*What reading the path actually shows.* The floor branch was read end to end
rather than probed. Two things follow.

The divergence the subdivision toggle causes is visible: the warmed whole-quad
early return is gated on `!tomb_raider_subdivision`, so with floor subdivision
ON a floor skips it and falls through to the dynamic path.

But that dynamic path does NOT drop anything:

```
if tomb_raider_subdivision && submit_tomb_raider_cached_room_quad(...) { return 1; }
// otherwise falls through to the ordinary quad submit
```

A `false` return still draws the floor as a whole quad. So nothing in this branch
rejects the surface, which contradicts every hypothesis tested so far -- all six
assumed something was rejecting it.

**The surface is submitted, and the debug capture shows no coloured geometry
reaches the screen. Those reconcile only if `submit_tomb_raider_cached_room_quad`
returns `true` while emitting nothing visible** -- claiming success so the caller
skips the fallback.

**That is exactly what it does.** At
[`indexed_cache.rs:2045`](../engine/crates/psx-engine/src/world_render/indexed_cache.rs#L2045):

```rust
let _ = world.submit_tomb_raider_textured_gouraud_view_quad_uv_words(...);
true
```

The submit's result is discarded and `true` is returned unconditionally. The
caller reads that as success and skips its whole-quad fallback, so a submit that
emitted nothing leaves a hole with no counter recording it. This fits every piece
of evidence rather than contradicting some of it, and it explains why six
hypotheses hunting a REJECTION all missed: nothing rejects the surface, a failure
is simply swallowed.

**Before fixing, confirm the discarded value can signal failure.** If it is a
stats struct with no failure indicator, this is a red herring and the emission
itself is at fault. If it carries an overflow flag or a dropped-primitive count,
the fix is to propagate it so a failed subdivision falls through to the ordinary
quad submit, which the caller already has.

A second silent path exists nearby -- `count_lighting_reject()` returns `0` when
`indexed_vertex_lighting_colors` yields `None` -- but walls share it and render
correctly, so it is the weaker candidate.

The earlier candidate, now superseded: `tomb_raider_warmed_quad_requires_dynamic_submit` forces
a subdividing quad off the warmed fast path
([`indexed_cache.rs`](../engine/crates/psx-engine/src/world_render/indexed_cache.rs)).
If the dynamic fallback then declines it too, the surface is dropped by both
paths with no counter recording it. Check that hand-off first.

Disabling floor subdivision is not the fix -- it exists for affine correction on
near floors.

**Original state, for the record:** open, and provenance unknown. Reported against a build from this
branch; never checked against `main`. Three changes here touch the room render
path (double-sided walls, the material stand-in, live texture eviction), so this
work cannot rule itself out. **Establish whether it reproduces on `main` first.**

The symptom follows the player: tiles disappear in the area *around* them as they
walk. Cooked data is static, so this is a per-frame decision keyed on camera
proximity, not missing data.

**Ruled out, each by experiment:**

- Surface submission. With `room-surface-profile`, `room_surf_profiled` equals
  `room_surfaces_considered` in rooms 6/7, with backface culls ~0.1, zero
  primitive overflows and zero submit fallbacks.
- Cell selection. Forcing `cell_aabb_visible` to accept every cell changed 21 of
  35 route frames but left the floor unchanged.
- Material resolution. Seeding unresolved slots from a resolved material in the
  same room left 31 of 35 frames byte-identical.

**Methodology warning.** The three results above were read as "the geometry was
never cooked", which the follows-the-player symptom contradicts. The error was
aggregating: means over 253 renders cannot see a handful of tiles vanishing per
frame, and `considered == profiled` only proves everything *considered* was
drawn, saying nothing about what was considered. A `split_tris` reading of 0 was
also used to dismiss tessellation, but that counter measures the hardware
splitter, not Tomb Raider subdivision.

**Correct approach:** capture *every* route tick through rooms 6/7, find the tick
a tile blinks out, and diff that frame against tick-1 where the floor is whole.
Then run `tessellation-debug` over the same span; leaves colour cyan, underdraw
yellow, un-subdivided roots red. Proximity narrows the suspects to code that only
runs near the camera: the TR subdivision band,
`draw_near_clipped_cached_room_surface`, and near-plane extent clamping.

## RESOLVED: menu music played an octave up (2026-07-25)

Not a conversion bug. `CRT Lab Loop.wav` is a clean 44100 Hz / 16-bit / stereo
source, 67.42 s, and the cook is a passthrough at unity speed.

cortex_v1's Menu Music node carried `playback_speed_q12: 8192` in all three menu
scenes. That is 2.0x, the exact maximum of the editor's Playback speed slider
(`UI_MUSIC_PLAYBACK_SPEED_UNITY_Q12` is 4096). `cook_cdda_track_from_wav_at_speed`
honoured it, resampling the track to 33.72 s, which plays back at 44.1 kHz as
double speed, one octave up.

Nothing in the codebase writes that field except the slider itself, and new
music nodes are created at unity, so this was authored data rather than a
regression: the slider had been dragged to its end.

Set back to 4096 and re-cooked. The track is 67.43 s (the extra 1176 bytes is
CD-DA sector padding) and its first 800,000 bytes are byte-identical to the
source WAV's PCM, which at matching sample rates is what unity speed should
produce.

The project's `assets/audio/cdda/crt_lab_loop.track02.cdda` was deleted along
with it: 23.7 MB, referenced nowhere, and at 8 bytes per source frame it was
twice the correct size, so an artifact of some older path. The cooker reads the
WAV, never that file.

## Unrelated, also open

`cortex_v3` does not cook: `sector 1,13 East wall has invalid heights
[0, 0, -64, 320]`. A negative height in a wall quad; one sector to fix in the
editor.

## MEASURED: wall orientation from grid adjacency

With headless gameplay working again, the wall-orientation change (`fb884080`)
was A/B'd by reverting it, rebuilding the guest, and replaying the identical
scripted route. Both sides produced exactly 1360 gameplay rows, so the routes
match frame for frame.

| stage | before | after | delta |
| --- | --- | --- | --- |
| `render` | 555,806 | 521,716 | **-6.13%** |
| `room` | 166,152 | 126,170 | **-24.06%** |
| `frame_cycles` | 986,629 | 984,089 | -0.26% |

The room stage, which is what the change actually touches, drops by a quarter.
`frame_cycles` barely moves because `present` busy-waits to the vblank boundary
and absorbs the saving; that is expected and is why `render` is the honest
number here (see the chart caveat in `CLAUDE.md`).

This beats the +4.34% estimate it was designed against. It is still a cycle
measurement, NOT a correctness gate: walls must be checked visually across a
whole route in the editor, because the earlier global-flip attempt looked right
on a single frame while having inverted every wall that was already correct.

### VISUAL A/B: the change is not safe to merge (2026-07-25, evening)

Three guest builds over the same cooked cortex_v1 data and the same scripted
route (`--hold-forward` plus the menu `--press` set), all reaching 3069 route
ticks, screenshots every 100 ticks:

| build | what it is |
| --- | --- |
| before | `a4814df2`, walls forced to both faces |
| after | `4fd44d5b`, the optimization |
| control | `4fd44d5b` clean, without the streaming changes in this commit |

- The SIMULATION is identical between before and after: `cam_x/y/z`,
  `current_room` and `drawn_mask` match on all 3038 counter-log frames. Only
  what is drawn changed.
- **27 of 30 captured frames differ.** This is a route, not a pose.
- From about tick 400 onward the PLAYER IS NOT DRAWN in the after build. She is
  present in the simulation, the camera tracks her, and she renders normally in
  the both-faces build at the same tick.
- Per frame, roughly 36k pixels shift slightly across the walls and about 2.8k
  shift strongly, in a band wider than her silhouette.
- The control is PIXEL-IDENTICAL to the after build on all 30 frames, so the
  streaming changes committed alongside this note are not involved and cost
  nothing visually.

### FIXED: the quad packet's corner order must not depend on sidedness

The wall rule was never the problem. The bisect isolated it:

| experiment | result |
| --- | --- |
| new rule + flag, sidedness forced back to `Both` | pixel-identical to before, 0/30 |
| single face, warmed packets disabled | pixel-identical to broken, 0/30 |
| owner-facing walls skipped entirely | player visible, wall gone: it IS the occluder |
| per-triangle wall depth | player returns in FRAGMENTS: a depth tie |

The drawn set can only SHRINK when a face goes from `Both` to `Back`, so extra
occlusion was impossible and the difference had to be ordering. The diff map
agreed: outside her silhouette the wall differed only at dither level.

`submit_sided_projected_gouraud_quad_cached_uv_words` reversed the four corners
for a `Back` face before reordering them into the hardware quad. That yields the
SAME two triangles with reversed winding, which is why the wall itself looked
right, but it also swaps WHICH TRIANGLE IS SUBMITTED FIRST. That matters because
`submit_textured_gouraud_quad_prescreened_uv_words_prepared_depth` abandons the
single prepared depth and emits the two triangles separately, each computing its
own depth, whenever the quad needs splitting. A corridor wall seen at a grazing
angle spans a huge depth range, so its two triangle depths straddle anything
standing in front of it. Swapping their order flipped the tie and the wall
painted over the player.

Fix: `quad_packet_order` builds the packet from the authored corners and never
consults sidedness. The winding is not needed there anyway, since the software
backface cull has already run and the GPU does not cull. The warmed packet path
gets its own `WARMED_ROOM_QUAD_REVERSE_FRONT` bit so ceiling `reverse_front`
ordering is untouched while the cull keeps using the sidedness-folded bit.

Verified on the identical 3069-tick route, 3001 render hits, 1360 room hits:

- **29 of 30 route frames are pixel-identical to the both-faces build.** The
  30th is tick 400, a one-frame phase shift, with the player visible in both.
- `room surface draw` per hit 121,298 against the both-faces 129,921, so
  **-6.6%** of the room stage is kept. The broken build's -10.6% was partly
  paid for by not drawing the player.
- 267 psx-engine tests pass.

Coverage caveat: `--hold-forward` walks one corridor, so this still wants an
editor pass over a wider route.

## RESOLVED: the "loading stall" was a disc built without the UI pack

**There is no engine stall. This was my build error, and the whole entry below
was wrong.** Recorded in full because the way it failed is a real reliability
problem even though the cause was not.

The iso template in `CLAUDE.md` omits `--ui-pack-dir` and `--ui-pack-order-file`.
The Makefile's `profile-demo3-disc-stream-forward` target passes both. Persistent
gameplay assets live in that UI pack, so a disc built from the template has
nothing at `UI_PACK_START_LBA`: the asset arena waits forever for sectors that
were never written. Add both flags and cortex_v1 boots to gameplay -- verified,
1360 gameplay rows out of 3030, first at row 288, with `--press` clearing the
menu.

What the misdiagnosis cost, and the two genuine bugs it exposed:

1. **A missing pack is indistinguishable from a hung drive.** Every symptom I
   chased -- zero sectors, zero completed entries, no failure reported, readers
   entered every frame -- is equally consistent with "the data is not on the
   disc". Nothing in the guest says which. A pack whose TOC is absent or empty
   should be a loud failure at `begin`, not an infinite wait.
2. **The stall detector did not fire. RESOLVED: its budget was 136 seconds.**
   Nothing upstream stopped the job, and the `begin_next_group` reset was not
   the cause either. The counter's UNIT was wrong.

   `try_read_stream_sector` checks the IRQ flag once and returns; on `Ok(false)`
   the caller `break`s out of its sector loop. So `data_wait_polls` could
   increment at most **once per `poll_into` call**, never once per spin, despite
   the name. The pump runs on alternate sim ticks
   (`ROOM_WINDOW_BACKGROUND_TICK_MASK == 1`), so 4096 increments needed 8194
   ticks. The run was 8117. It came within about 80 ticks of firing and simply
   ran out of road, which is also why `failed()` was never set: the job stayed
   in `Reading` the whole time and `finish_if_done` had nothing to judge.

   The reset in `begin_next_group` is fine on its merits and stays: that state
   is only reached at job start or after `mark_group_processed` retired a group,
   both of which are real progress, never a re-arm of the same work.

   **Fixed** by naming the unit and setting a budget in it: `empty_pumps` with
   `EMPTY_PUMP_STALL_LIMIT = 256`, about 8.5 s at 30 pumps/second.

   Measured both ways on cortex_v1, same guest binary:

   | disc | route ticks | `persistent asset load failures` | gameplay |
   |---|---:|---:|---|
   | built without `--ui-pack-dir` | 3117 | **1** | none, as expected |
   | built with the UI pack flags | 3069 | 0 | 2749 frames, first at 290 |

   The failing run reports inside 3117 ticks where the old limit needed 8194, and
   the tighter limit does not false-positive on a healthy load.
3. **A sticky `failed` stops the pump silently.** One bad persistent asset sets
   `AssetArena::failed`, `pump` then returns early forever and nothing surfaces
   it.

   **Partly fixed.** Every path in `PersistentAssetStreamer` that gives up now
   routes through one `fail()` that counts `PERSISTENT_ASSET_LOAD_FAILURES`
   once, so a failure is always visible in telemetry rather than only in
   `request_rooms` (which was the sole counted path, and not the one the missing
   pack took). That is what makes the table above readable.

   **Now names the asset.** A failure count alone leaves every streamed asset in
   the level as the search space. `fail()` takes the asset id and a reason, and
   both go out as counters (`PERSISTENT_ASSET_FAILED_ID`,
   `PERSISTENT_ASSET_FAILED_REASON`). Reasons 0..11 are `cd_stream` chunk
   statuses; 100+ are `asset_streaming` codes (`ASSET_FAIL_BAD_RECORD`,
   `ASSET_FAIL_NO_SPACE`, `ASSET_FAIL_SHORT_READ`).

   On the missing-pack disc this reports asset **26** (`Rust Mantis` mesh, 6800
   RAM bytes) with reason **4**, `STATUS_DATA_TIMEOUT`. Read that pairing
   carefully: reason 4 means the drive delivered nothing, so 26 is simply the
   first asset in read order, not a faulty asset. The reason code is the
   diagnosis and the id only localises it. `ASSET_FAIL_NO_SPACE` or a
   per-chunk status is where the id genuinely identifies the culprit.

   **On a correctly built cortex_v1 disc no asset fails at all** (0 failures,
   gameplay from frame 292). The hang is only reachable from a bad build.

   **The loading screen no longer pretends.** `loading_progress_q12` returns 0
   once the arena has failed, so the bar empties and stays empty instead of
   parking part-full and reading as slow progress, and the guest prints
   `PERSISTENT ASSET LOAD FAILED: asset <id> reason <n>` to TTY once,
   unconditionally rather than under `boot-trace`, because this state never
   resolves.

   **Still a design call:** the game stays on that screen. It does not offer an
   error scene or a way back to the menu, and doing either needs authored UI or
   a new `Scene` hook rather than a runtime change.

Ruled out along the way, so nobody re-walks them: the confirm press
(`just_pressed` is edge-triggered, but pressing every 30 ticks changed nothing),
the `poll_into` group-start break (`begin_next_group` always leaves state
`Reading`), and CD-DA contention (a disc built with no tracks at all stalls
identically).

**Fix the template.** `CLAUDE.md`'s replay recipe should carry the UI pack flags,
or the next person loses the same day.

## Headless gameplay is unreachable, which blocks all render A/B (2026-07-25)

No headless run can reach gameplay any more, so no render change can be measured
before it ships. This is worse than any single bug in this file: it removes the
evidence step from the perf workflow.

Two paths existed and both are closed:

- **`--hold-forward`.** It only holds the left stick and never presses CROSS.
  That was fine when the demo projects booted straight into play. They no longer
  do: demo_05 boots to the menu exactly like cortex_v1, so the run sits on the
  menu for its whole length. There is no CLI flag that presses a button
  (`launch --help` offers `--hold-forward` and `--hold-run`, nothing else).
- **`--input-tape`.** Replaying `cortex_v1.pxtape` headless never leaves the
  intro. Measured, not assumed: of 2097 telemetry rows, **0** have
  `room_cells_drawn > 0`, and route screenshots at ticks 300..2100 all show the
  same archive-fragment text screen. This is the desync recorded for demo10 in
  `CLAUDE.md`, still unfixed and now the only remaining path.

**FIXED.** `--press "tick:button[:hold]"` feeds scheduled button presses on the
route clock and combines with `--hold-forward`, so a headless run can work a
menu. Together with a disc built with the UI pack flags (see above), cortex_v1
reaches gameplay headless again and render changes can be measured.

## The cooker overwrites `generated/` when it fails (2026-07-25)

A failed cook prints "Nothing was written; the previously generated output is
intact" and that is not true. Cooking `demo_03` (which fails on a 89208-byte
room, over the 32 KiB chunk limit) left `engine/examples/editor-playtest/generated/`
in a state where the next `build-editor-playtest` failed with four unresolved
imports: `COMBAT_CAPSULES`, `ROOM_REFLECTION_PROBES`, `GAMEPLAY_PACK_MAX_CHUNK_BYTES`,
the `UI_PACK_*` set and `LOADING_UI_SCENE`. Re-cooking a project that succeeds
repairs it.

This contradicts the failed-cook manifest contract restored in 67d8cf92, so
either the contract regressed or it does not cover every output the manifest
needs. The symptom is nasty because the compile errors point at the guest source
rather than at the cook that broke it.
