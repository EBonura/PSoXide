# TR5 engine architecture review: the non-render half

Date: 2026-07-25
TR5 source pinned to [`6abe6b2`](https://github.com/TOMB5/TOMB5/tree/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667).
PSoXide compared at `perf/engine-30fps`.

## Scope

[`tr5-performance-architecture-survey-2026-07-23.md`](tr5-performance-architecture-survey-2026-07-23.md)
already compared the two renderers and ranked 21 performance techniques, every
one of which was then implemented and measured in
[`tr5-performance-experiment-results-2026-07-23.md`](tr5-performance-experiment-results-2026-07-23.md).
That work is not repeated here.

This review covers everything else: world state, AI, triggers, entity
management, cameras, and the object model. These are the systems a Souls-like
leans on hardest, and they are where TR5 is furthest ahead.

Findings are architectural. No claim here has been runtime-measured, and none
should be treated as a performance result.

## Ranked findings

| # | TR5 capability | PSoXide today | Verdict |
|---:|---|---|---|
| 1 | Alternate rooms (flipmaps) | absent; importer parses, cooker drops | **Adopt** — largest gameplay gap |
| 2 | Zone/box reachability baked into floor data | no navigation data at all | **Adopt** — O(1), cook-time, cheap |
| 3 | Object descriptor table with behaviour pointers | enum dispatch in engine code | **Adopt the seam**, not the pointers |
| 4 | Heavy triggers (object-activated) | player-activated logic only | **Adopt** — small change, real authoring gain |
| 5 | Scripted spline cameras (SPOTCAM) | third-person follow camera only | **Adopt when set pieces exist** |
| 6 | Active/free/room intrusive item lists | fixed-pool scans | **Deferred** — already rejected at current scale |
| 7 | Hair/cloth chain simulation | absent | **Conditional** — only if the player wears cloth |
| 8 | Cutscene control system | absent | **Not now** — needs a cutscene pipeline first |

## 1. Alternate rooms (flipmaps)

**TR5.** A level can author a second version of a room and swap the whole thing
at runtime. `FlipMap` in [`CONTROL.C:2417`](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/GAME/CONTROL.C#L2417)
performs the swap, `flip_status` records which version is live, and per-map
`flip_stats` track each flip independently. Crucially the flip state is not
cosmetic: it indexes AI zone data (see finding 2), so creature reachability
changes with the world.

This is how the series does flooding, collapsing floors, opened shortcuts, and
before/after set pieces, with no runtime geometry generation.

**PSoXide.** No equivalent. The only "flip" in the codebase is the room-material
flipbook at [`psx-level/src/lib.rs:1276`](../engine/crates/psx-level/src/lib.rs#L1276),
which animates textures, not geometry. The TR level importer already parses the
field, at [`tr_level.rs:116`](../editor/crates/psxed-project/src/tr_level.rs#L116)
and [`:520`](../editor/crates/psxed-project/src/tr_level.rs#L520), and the cooker
then discards it: no `alternate_room` reaches `psx-level` or the manifest.

**Why it matters here.** A Souls-like is built on persistent world-state change
— the shortcut you unlock, the gate you open, the collapsed bridge. Today the
only tool for that is `logic_kind::DOOR`, which hides a single box prop. A
room-level swap is a categorically bigger authoring primitive.

**Shape of the work.** The streaming layer already keys rooms by index and pages
them by chunk, so an alternate room is a second chunk for the same footprint and
a residency rule that keeps only the live one pinned. The cooker knows both
versions at bake time, so the collision cache and the render cache can both be
emitted per version. The runtime cost is a swap of the active chunk handle.

## 2. Zone/box reachability baked into floor data

**TR5.** Every sector carries a box index, and `ground_zone[zone_type][flip_state]`
maps a box to a zone id ([`BOX.C:38`](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/GAME/BOX.C#L38)).
Answering "can this creature reach that target" is then two sector lookups and
one integer compare of the two zone ids
([`BOX.C:129-137`](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/GAME/BOX.C#L129-L137)).
No search, no per-frame cost that scales with distance. Separate zone arrays
exist per creature capability (ground, water, flying), so a flying enemy sees a
different connectivity graph over the same geometry.

**PSoXide.** There is no navigation data. Entities move by direct motor steering
toward the player (`GameEntityIntent::Approach` / `Orbit`, [`entities.rs:82`](../engine/crates/psx-game-runtime/src/entities.rs#L82)).
The 2026-07-23 survey's finding 9 correctly recorded that pathfinding is not
applicable "until navigation exists" — this is the missing piece it was waiting
on.

**Why it matters here.** Direct steering walks enemies into walls and lets them
aggro through geometry they cannot reach. Zones fix the second problem outright
and at O(1), which is the cheapest possible answer, and they are a prerequisite
for any later path search. They are also purely cook-time: the flood fill runs
in the editor, and the runtime only ever compares two integers.

**Shape of the work.** The cooker already walks the sector grid to build
collision and visibility. A connected-component flood over walkable sectors,
respecting the existing `walkable` flag and step-height limits, produces the box
and zone tables. Emit one zone array per alternate-room state once finding 1
lands, matching TR5's indexing.

## 3. Object descriptor table with behaviour pointers

**TR5.** `objects[]` carries per-type behaviour: `initialise`, `control`,
`collision`, `draw_routine`, `draw_routine_extra`, plus data such as
`object_mip` ([`SETUP.C:623-741`](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/GAME/SETUP.C#L623-L741)).
Adding an enemy is a table entry plus a control function. The main loop knows
nothing about specific enemies.

**PSoXide.** Behaviour lives in a shared state machine over
`GameEntityState`/`GameEntityIntent` enums ([`entities.rs:40`](../engine/crates/psx-game-runtime/src/entities.rs#L40)),
tuned per record by data such as `windup_ticks` and `recovery_ticks`. Every
entity runs the same code path.

**Assessment, with a caveat.** This is genuinely a fork in the road rather than
a defect. The data-driven state machine is more uniform, easier to cook, and
avoids the relocation machinery TR5 needs on PSX. It is also strictly less
expressive: a boss with a bespoke multi-phase pattern cannot be written without
editing engine code.

**Recommendation.** Do not import function pointers. Do add the seam: a small
per-archetype behaviour selector so an entity record can name a specialised
controller, with the shared state machine as the default. That keeps the
uniform path for the common enemy and gives bosses an escape hatch.

## 4. Heavy triggers

**TR5.** `TestTriggers(data, heavy, HeavyFlags)`
([`CONTROL.C:1494`](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/GAME/CONTROL.C#L1494))
distinguishes a trigger fired by the player from one fired by an object standing
on it. That single distinction is what lets a pushable block hold a pressure
plate, or a dropped body open a gate.

**PSoXide.** `logic_kind` covers `MESSAGE`, `CHECKPOINT`, `TRIGGER_VOLUME`,
`RELAY`, `MULTISOURCE`, `DOOR` ([`psx-level/src/lib.rs:1511`](../engine/crates/psx-level/src/lib.rs#L1511)).
The composition primitives (`RELAY`, `MULTISOURCE`) are arguably cleaner than
TR5's flat floor-data trigger stream. What is missing is the activator concept:
everything is implicitly player-activated.

**Shape of the work.** Add an activator mask to `TRIGGER_VOLUME` and test box
props and entities against volumes, not just the player. The existing
`MULTISOURCE` gate then composes object-activated and player-activated sources
for free.

## 5. Scripted spline cameras

**TR5.** `SPOTCAM.C` is 1,142 lines of scripted camera paths with their own
trigger integration, used for reveals, boss introductions and fixed-angle
sections.

**PSoXide.** [`third_person_camera.rs`](../engine/crates/psx-engine/src/third_person_camera.rs)
is a follow camera with collision. There is no scripted camera, no camera
volume, no fixed-angle region.

**Assessment.** Fixed and rail cameras are a defining tool of the genre this
engine targets, and the existing camera already solves the hard part (wall
collision and clearance). Worth adopting once there is content to frame, not
before.

## 6. Active/free/room intrusive item lists

**TR5.** Items are threaded through three intrusive lists: `next_item_active`
for the update walk, `next_item_free` for allocation, and `next_item_room` so a
room draw touches only its own items ([`ITEMS.C:22-212`](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/GAME/ITEMS.C#L22-L212)).
Cost scales with live entities, not pool capacity.

**PSoXide.** Fixed pools, scanned. This was measured on 2026-07-23 (finding 8)
and rejected: cortex_v1 runs 258 entity thoughts over a whole route, and the
30 Hz think cadence already halves the passes.

**Verdict unchanged: deferred.** Revisit when a scene holds many entities. Note
that finding 3 above and the box-prop capacity work already landed on this
branch both push in the same direction: size pools from cooked counts first, and
the pool scan stops being the problem.

## 7. Hair and cloth

`HAIR.C` plus `CALCHAIR.MIP` (1,074 lines of PSX assembly) run a
budgeted chain simulation for Lara's braid. Nothing equivalent exists in
PSoXide.

Only worth doing if the player character wears cloth. The technique is a short
verlet chain with sphere collision against the character's own joints, which is
affordable in the current per-frame budget, but it is content-driven and
shouldn't be built speculatively.

## 8. Cutscene control

`DELTAPAK.C` is 4,310 lines, the third-largest file in the engine: per-cutscene
`init` / `control` / `end` hooks that drive items, meshswaps and cameras
frame-by-frame. PSoXide has no cutscene concept at all.

This is a large system with no value until there is a cutscene pipeline and a
scripted camera (finding 5). Listed for completeness, not recommended now.

## What PSoXide already does better

Stated for balance, and all of it verified in the 2026-07-23 experiment ledger
rather than asserted:

- **Streaming and residency.** Paged room/asset streaming with prefetch and
  eviction against TR5's whole-level residency (finding 20).
- **Portal visibility.** Recursive clipped frustum traversal, 27 focused tests
  (finding 10).
- **Fixed arenas with overflow telemetry.** Predictable capacity and counters
  for every overflow class (finding 14).
- **Instrumentation.** Per-stage cycle telemetry, per-vblank profiling, GP0
  command census, guest PC symbolisation. TR5 has no counterpart, and this
  review's own conclusions rest on it.
- **Editor and cooker pipeline.** Authoring, cooking and playtesting in one
  tool, with the cooker sizing runtime budgets from authored content.

## Recommended order

1. **Zone/box reachability** (finding 2). Pure cook-time, O(1) at runtime, no
   render-budget cost, and it unblocks every later AI improvement.
2. **Alternate rooms** (finding 1). The biggest gameplay capability gap, and the
   streaming layer is already shaped for it.
3. **Heavy triggers** (finding 4). Small change, composes with existing logic
   kinds.
4. **Per-archetype behaviour seam** (finding 3). Needed before bosses.
5. Scripted cameras, hair, cutscenes: content-driven, defer.

Findings 1 and 2 should land together, because TR5 indexes zone data by flip
state and splitting them would mean building the zone tables twice.
