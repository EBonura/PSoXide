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

**Resolved 2026-07-25.** `submit_adaptive_cached_room_quad` discarded its
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

*Cause.* Setting `ROOM_ADAPTIVE_SUBDIVISION_KINDS` to `WALL` only, disabling
floor subdivision, **removes every hole**. The floor renders textured and
continuous across the whole route. So the loss is in the adaptive subdivision
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
early return is gated on `!adaptive_subdivision`, so with floor subdivision
ON a floor skips it and falls through to the dynamic path.

But that dynamic path does NOT drop anything:

```
if adaptive_subdivision && submit_adaptive_cached_room_quad(...) { return 1; }
// otherwise falls through to the ordinary quad submit
```

A `false` return still draws the floor as a whole quad. So nothing in this branch
rejects the surface, which contradicts every hypothesis tested so far -- all six
assumed something was rejecting it.

**The surface is submitted, and the debug capture shows no coloured geometry
reaches the screen. Those reconcile only if `submit_adaptive_cached_room_quad`
returns `true` while emitting nothing visible** -- claiming success so the caller
skips the fallback.

**That is exactly what it does.** At
[`indexed_cache.rs:2045`](../engine/crates/psx-engine/src/world_render/indexed_cache.rs#L2045):

```rust
let _ = world.submit_adaptive_textured_gouraud_view_quad_uv_words(...);
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

The earlier candidate, now superseded: `adaptive_warmed_quad_requires_dynamic_submit` forces
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
splitter, not adaptive subdivision.

**Correct approach:** capture *every* route tick through rooms 6/7, find the tick
a tile blinks out, and diff that frame against tick-1 where the floor is whole.
Then run `tessellation-debug` over the same span; leaves colour cyan, underdraw
yellow, un-subdivided roots red. Proximity narrows the suspects to code that only
runs near the camera: the TR subdivision band,
`draw_near_clipped_cached_room_surface`, and near-plane extent clamping.

## Unrelated, also open

`cortex_v3` does not cook: `sector 1,13 East wall has invalid heights
[0, 0, -64, 320]`. A negative height in a wall quad; one sector to fix in the
editor.

## cortex_v1 never finishes loading headless (2026-07-25)

Root cause of the "headless gameplay is unreachable" entry below, found after
adding `--press`. It is NOT an input problem: the guest completes pad polls
normally (401 polls over 517 route ticks) and simply never satisfies its exit
condition.

`GameApp::update` leaves the loading scene only when
`loading_confirm_ready && confirmed`, where `loading_confirm_ready = world_ready
&& hold_done` (`engine/crates/psx-engine/src/game_app.rs:1189`). `confirmed` is a
fresh CROSS or START, which `--press` supplies. So the stall is `world_ready`,
i.e. `gameplay.loading_update` never returns true.

Evidence, from an 8117-route-tick run (~135 s of guest time):

- **22** `cd_room_chunk_loads` in total, all early, then nothing for the rest.
- `room_stream_requests`, `room_stream_pending_loads` and
  `room_stream_failed_loads` are **0** for the entire run, so the stall is not a
  reported failure. Nothing is even asking.
- PC sampling shows the guest busy in the loading UI:
  `FontAtlas::text_width` 27.6%, `draw_transformed_text_paint` 18.0%,
  `draw_label` 7.6%, `draw_quad_paint` 6.6% -- and, tellingly,
  `cd_stream::hw::read_one_sector_blocking` at **12.3%**.
- Route screenshots at ticks 1000..8000 animate (a two-state blink) but never
  advance.

**Narrowed to a single condition (2026-07-25, later).** Added `boot-trace`
reporting of the world-ready conditions to `initial_world_ready`, which prints
only on change so a 30 Hz loading loop does not flood the TTY. Guest TTY reaches
host stdout through the HLE BIOS putchar, so this needs no new telemetry counter.

Result, in order of elimination:

1. Only `runtime_models_loaded` is ever pending. The other six conditions are
   never even evaluated: it returns before them.
2. `persistent_assets_arena().failed()` is **false**, so this is not the sticky
   failure path in `AssetArena::finish_if_done`. (Worth knowing that path exists:
   one bad asset sets `failed`, `pump` then returns early forever, and nothing is
   reported. A different hang with the same symptom.)
3. `progress_q12()` never changes from 0 across the whole run. `begin` always
   sets `started`, and `count == 0` would set `ready`, so the arena is started
   with a non-empty asset list and **not one entry ever completes** -- while the
   guest spends 12.3% of its time reading sectors.

So the wedge is inside the grouped read job, `WorldRoomSlotsRead::poll_into`
(`engine/crates/psx-game-runtime/src/cd_stream.rs:394`), which reads sectors
without ever completing an entry.

**One hypothesis already ruled out.** In `poll_into`:

```rust
if self.state == WorldRoomSlotsReadState::Ready {
    if !self.begin_next_group(cd, &mut polls) { break; }
    if sectors_this_poll == 0 { break; }   // always true here
}
```

`sectors_this_poll` is zero-initialised per call, so the call that starts a group
always breaks immediately. This is NOT the bug: `begin_next_group`
(`cd_stream.rs:564`) sets `state = Reading` on every path that returns `true`, so
the break costs one pump tick per group and the next call proceeds to the read.

That leaves the `Reading` path itself. The state does advance, sectors are read
continuously, and yet `processed_count` never rises, so look at what turns read
sectors into a completed entry: whether `try_read_stream_sector` keeps returning
`Ok(false)` and burns the budget, whether `sector_offset` ever reaches
`group_end`, and whether `mark_group_processed` is reached but the entry is then
rejected. `data_wait_polls` and its timeout are worth watching, since a retry
that resets progress would look exactly like this.

**The confirm is not the blocker.** `confirmed` is edge-triggered
(`just_pressed`), so an early press is consumed before the world is ready and
never repeated -- a trap worth knowing when scripting a route. Ruled out by
pressing CROSS every 30 ticks for 6000 ticks: still zero gameplay rows. So
`initial_world_ready()` (`engine/examples/editor-playtest/src/main.rs:618`) is
genuinely false, and the next step is to find which of its seven conditions
never flips:

```text
runtime_models_loaded
current_collision_room.is_some()
!window.job.active
portal_visible_rooms_are_active(record)
initial_stream_ring_resident()
initial_stream_ring_textures_ready()
streaming_jobs.vram_uploads_idle()
!streamed_room_stream_active()
```

None of them is currently visible in telemetry, which is why this took a whole
session to corner. Emitting them as counters is the cheap fix and would pay for
itself the next time a load stalls.

**The contradiction worth chasing:** the guest spends 12% of its time inside
`read_one_sector_blocking` while completing zero further chunk loads. Sectors are
being read continuously and never assembled into a chunk, which reads as a retry
or restart loop in the CD stream layer rather than a slow load. Start at
`psx-game-runtime/src/cd_stream.rs` and ask what makes a sector read succeed
without advancing a chunk.

This matters beyond profiling: if it can happen headless it is a streaming
reliability bug, and the editor differing only hides it.

Reproduce:

```sh
cd emu && cargo run -p frontend --release -- launch \
  --path ../build/examples/mipsel-sony-psx/release/editor-playtest.cue \
  --embedded-playtest --press "100:cross:20" \
  --profile-log /tmp/long.csv --guest-visual-frames 4000 --steps 4000000000
```

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

**Half fixed.** `--press "tick:button[:hold]"` now feeds scheduled button presses
on the route clock and combines with `--hold-forward`, so a headless run can work
a menu. It is not enough on its own: cortex_v1 then stalls in its loading scene
for a different reason, recorded above.

Until that stall is fixed a render change can only be gated visually in the
editor, and any cycle figure quoted for one is an estimate.

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
