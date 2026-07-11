# psx-game-runtime: the game layer plan

Status: phase 1 in progress (2026-07-11). This document is the design
brief for the layer between `psx-engine` (reusable PS1 primitives) and
a game. It exists because two audits agreed on the same finding from
opposite directions:

- PSoXide's `editor-playtest` example contains ~12k lines of
  engine-shaped runtime (CD streaming, room residency, VRAM
  management, model-instance rendering, visibility, props) that every
  downstream game would otherwise re-implement, and no gameplay layer
  at all (zero enemy/combat/trigger/save code; the cooked schema has
  stubs waiting).
- hl-psx (the most advanced downstream port) contains a complete,
  campaign-proven gameplay layer -- and its own plan document names
  PSoXide's residency stack as the streaming model it wants.

The runtime crate is the convergence point: the home for both halves.

## The four constraints (from the project owner)

1. **Souls-like, not FPS.** Cortex Ignition is the reference game.
   Melee-first combat: lock-on (the camera already has it), stamina
   (the motor already meters it), windup/commit/punish attack grammar,
   dodge-roll i-frames, deliberate enemy archetypes over crowds,
   checkpoint-respawn death loops. hl-psx's hitscan-vs-point-targets
   is the wrong default here; melee arcs vs capsule/box hurtboxes are
   the first-class case (the cooked `WeaponHitboxRecord` schema
   already models this -- it needs a runtime, not a redesign).
2. **Grid world, not BSP.** Rooms are adaptive-style sector grids
   with portals. The grid is the optimization substrate: visibility is
   portal-expanded cell sets (already precomputed at cook), collision
   is O(1) sector lookup (already true in the motor), streaming wants
   grid-aligned room pages (already true). Where hl-psx leans on BSP
   traversal order and PVS bitsets, we lean on portal frontiers and
   cell lists. Do not port BSP-isms.
3. **Editor-first.** Everything the runtime executes must be
   authorable in psxed -- the editor overhaul is the next big chunk
   after this, so every schema this plan adds gets designed as
   editor-authored data + cook output + runtime consumer, in that
   order. No runtime feature lands without its cooked-record story.
   hl-psx compiles Hammer's entity soup at cook time; we get the same
   discipline for free by *authoring* typed records in the editor.
4. **Clean architecture at 30 fps.** hl-psx accepts 20 Hz; Cortex
   targets 30 fps -- a 33% smaller frame budget than the port, on the
   same silicon, with the playtest currently measuring ~26 fps with
   ~34% deadline misses on the gameplay tape. The runtime carve must
   be cost-neutral (same code, new home), and every gameplay system
   added afterwards must fit inside a budget line that is written
   down BEFORE the system is built.

## Architecture

```
psx-engine        primitives: scheduler, render passes, GTE kit,
                  motor, camera, transitions, UI flow, fixed-point
psx-game-runtime  the game layer (this plan):
                    streaming/residency/VRAM (phase 1)
                    model instances + props + visibility (phase 2)
                    entity/logic runtime + combat + AI (phase 3)
                    saves (phase 4)
editor-playtest   thin Scene impl + cooked-manifest glue + debug
                  overlays; the reference consumer
cortex ignition   the game, authored in psxed
```

Crate placement: `engine/crates/psx-game-runtime`, no_std, same
workspace and lint set as psx-engine. It may depend on psx-engine,
psx-level, and SDK crates; nothing may depend back on it except games
and examples.

### Boundary rules

- The runtime owns POLICY (what to stream, what to evict, what an
  entity does per tick). psx-engine owns MECHANISM (how to draw, how
  to schedule, how to trace collision).
- All capacities arrive through one `RuntimeBudgets` struct generated
  at cook/build time, hl-psx-style: budgets are derived from the
  project's actual cooked content by the build script, not guessed
  constants sprinkled through source. (`runtime_config.rs`'s ~40
  consts become fields with provenance.)
- Cooked data reaches the runtime as `&'static` typed records (the
  psx-level pattern), never parsed strings. Editor-authored names are
  interned to u16 ids at cook (adopted from hl-psx's LogicEnt).
- No `static mut` in new code: state lives in arena structs owned by
  the game's Scene and passed `&mut` (the SoA layout is kept; the
  unsafety is not).

## Adopted from hl-psx (and what is deliberately not)

Adopt:
- Build-time RAM auto-budgeting (`build.rs` scans cooked assets,
  emits budget consts + a headroom report).
- Stack watermark canary probe behind the telemetry feature; RAM
  gates argued from measured peaks, not vibes.
- Cook-time entity compilation: strings die on the host; the runtime
  sees u16 ids and fixed-size records.
- SoA entity state with hot-candidate index lists; per-archetype tick
  dispatch over small explicit state machines (no behavior-tree VM).
- Delay-queued logic events with depth-limited fan-out and
  master/AND gating -- the entity-I/O core.
- PVS/visibility-gated AI thinking (ours gates on the portal-expanded
  cell set instead of BSP PVS).
- Resident-SFX / streamed-dialogue SPU split on a reserved voice.
- Merged asset chunks: one CD handshake per asset group (hl-psx
  measured -12% load time).
- The milestone-log documentation style.

Deliberately not:
- Whole-map residency (our room streaming is strictly finer).
- Hitscan-first combat (melee arcs first; hitscan is the special
  case for the odd ranged enemy).
- Landmark transitions, sentences.txt-style voice, chapter-select
  persistence (port-specific).
- The 13k-line main.rs shape. The crate is small modules with owned
  state; that is half the reason it exists.

## Phases

Phase 1 -- streaming + residency + VRAM (this branch):
  move `cd_stream(+hw)`, `active_room_streaming`, `active_rooms`,
  `active_room_cache`, `active_room_visibility`, `vram_runtime`,
  `vram_upload*` out of the example behind a `RuntimeBudgets` +
  `StreamSources` boundary. Example keeps its Scene impl and debug
  overlays. Gate: cortex boots bit-identical (validate suite +
  checkpoint hashes), MIPS build, zero new cost on the perf tape.

### Phase 1, slice 2: the active_room family (design notes, 2026-07-11)

Coupling facts (measured, not guessed):
- Capacities live in `runtime_config.rs` consts (`MAX_ACTIVE_ROOMS`,
  `STREAMED_ROOM_SLOT_COUNT`, `RESIDENT_DRAW_DEPTH`, ...) and are
  already consumed const-generically in places
  (`RoomStreamScheduler<STREAMED_ROOM_SLOT_COUNT>`): the carve keeps
  that pattern -- runtime types take `const N: usize` parameters and
  the game supplies its generated budget consts. `RuntimeBudgets` as
  a value struct is phase-1.5 (when build.rs generation lands);
  const generics are the phase-1 seam.
- State lives in `main.rs` `static mut`s (`ROOM_STREAM_SCHEDULER`,
  `ACTIVE_ROOM_WINDOW`, caches). The carve turns each into an owned
  runtime struct (`RoomStreaming<const SLOTS: usize>` etc.) with
  `&mut self` methods; the example keeps ONE instance wherever it
  keeps its scene state today. New crate code holds no statics.
- Cooked data (`ROOMS`, visibility cells, materials) comes from the
  example's `generated::` module as `&'static` psx-level records:
  runtime methods take these as parameters -- the crate must never
  name `generated::`.
- Module order for the move (least to most coupled):
  `active_room_visibility` -> `active_room_cache` -> `active_rooms`
  -> `active_room_streaming`; `runtime_schedule.rs`'s
  `RuntimeScheduleConfig` moves alongside as the shared knob struct.
- Gate per move: identical to slice 1 (validate bit-identical, MIPS
  build, engine tests) plus a perf-tape spot check after the last
  move since this family is on the hot path.

Phase 2 -- world runtime: model instances/equipment rendering, image
  props, box props, sky, particles, room lighting runtime. Same gate.

Phase 3 -- the gameplay layer (the new code): entity/logic runtime
  (editor-authored records: triggers, doors, switches, spawners),
  combat resolution (melee arcs vs hurtboxes, damage, poise/stagger,
  death), enemy archetypes (patrol/aggro/windup/punish state
  machines), nav (grid-native flood-fill/portal-graph paths -- we do
  not need hl-psx's node graph; the sector grid IS the nav mesh).
  Editor work lands alongside: each record type ships with its
  inspector. Budget line written before code: combat+AI get a
  per-frame cycle envelope measured against the 30fps target on the
  gameplay tape before content scales up.

### Phase 3 budget (written before code, per constraint 4)

Measured 2026-07-11 on the phase-2 tip (467ea54b + this slice's
baseline), cortex_ignition_v1, telemetry disc (`cd-stream-bench
emulator-telemetry`), temporary `boot: Gameplay` flip (reverted
byte-identical, the phase-1/2 tape recipe). Two runs, 2,400 guest
frames each, headless:

- Controlled idle (no input, standing at spawn -- the phase-2
  idle A/B shape; byte-deterministic, every per-frame stage count
  is constant): update band 37,941 cycles per 30 fps frame, frame
  total 873,778 of the 1,128,960-cycle slot (2 vblanks at
  564,480), 0 deadline misses, 33.0 cells drawn per frame at
  4,952 room cycles/drawn-cell. Slot headroom at idle: ~255k.
- Loaded route (`--hold-forward` corridor): update band avg
  157,959 / p50 142,407 / p95 196,124 per frame; frame total avg
  981,967 / p95 1,025,082; misses 4.88%; 12.6 cells per frame at
  6,733 room cycles/drawn-cell. Slot headroom: avg ~147k, p95
  ~104k. (The heavier committed bench tape,
  `benchmarks/cortex_ignition_v1-bench-2026-06-11.pxtape`, does
  not replay headless past the splash -- known menu-desync gotcha
  -- so its 12.59% miss figure from the phase-2 gate stands as
  the worst-route reference; these idle/forward runs are the
  reproducible A/B anchors.)

The envelope: the phase-3 gameplay layer (entity ticks + AI +
logic events + combat resolution) gets **60k cycles per 30 fps
frame** (~30k per 60 Hz tick) inside the update band -- p95 route
headroom is ~104k, so the grant leaves ~44k p95 margin for the
render-side cost of visible enemies (which rides the existing
`model_instances` band and gets re-measured at the first live
archetype). Sub-split: entity/AI ticks 40k, logic queue +
triggers 10k, combat resolution 10k. Per-thinker target: with
visibility-gated thinking and <=8 concurrently awake entities,
5k per entity per frame. An EMPTY gameplay layer (zero cooked
records -- cortex today) must cost <1k cycles per frame; the
idle run is byte-deterministic, so the A/B delta is exact.

Capacity budgets (SoA, all-zero constructors, sized against
cortex's 8-room scale with growth margin; contract caps live in
psx-level like `MAX_ROOM_MATERIALS` so the cook rejects over-cap
content loudly):

- `MAX_GAME_ENTITY_RECORDS` = 64: 8 rooms x up to 8 placed
  enemies (souls density is 2-4 deliberate enemies per room; 64
  is ~2x that at today's scale). As built: SoA 22 B/entity = 1.4
  KB .bss; cooked `LevelGameEntityRecord` = 48 B rodata each.
- `MAX_LOGIC_RECORDS` = 64: interactables + trigger/door/relay
  graphs, ~8 nodes per room average. As built: runtime state 7
  B/record ~= 450 B .bss; cooked `LevelLogicRecord` = 64 B rodata
  each.
- `MAX_LOGIC_EVENTS` = 32 in-flight delayed events (hl-psx ships
  64 for full HL campaign maps; our graphs are one 8-room level;
  overflow increments a saturating drop counter instead of
  silently vanishing). As built: 12 B/event = 384 B.
- `LOGIC_FIRE_DEPTH_MAX` = 8 (hl-psx parity; bounds one tick's
  fan-out recursion).

Slice-1 result (2026-07-11, the foundation slice): with cortex
cooking zero records, the identical-work A/B (same idle and
forward runs, before vs after) measured EXACTLY +0 cycles on the
update band, frame totals, and miss counts, with byte-identical
display hashes -- LTO folds the empty-table guard away, so the
record-free gameplay layer beats the <1k rule at literally zero.

Seams for the next phase-3 slice (first live archetype +
trigger-door in cortex content): enemy authoring exists
(non-player Character Controller + `EnemyBehaviorSettings`
opt-in; archetype tag = Character resource name) but needs its
inspector UI and real content in cortex (user authoring);
trigger/relay/multisource/door logic kinds run in the runtime
(host-tested) but have no authoring node yet -- the door's `link`
targets a `BOX_PROPS` index (toggle draw + collision); the entity
skeleton's placeholder constants (`GAME_ENTITY_ATTACK_RANGE`,
`GAME_ENTITY_WALK_STEP`) must be replaced by Character-bound
speeds and motor-integrated movement; `LogicRuntime::take_fired`
marks await example-side effect dispatch (message overlay /
checkpoint / door visual), at which point the interactable UI
path migrates onto LOGIC records; enemy RENDER cost rides the
existing `model_instances` band and must be re-measured against
the 60k envelope at first live content.

Phase 4 -- persistence: memory-card saves via psx-mc (checkpoint
  model designed for souls-like respawn loops), settings, session
  flags. Greenfield; nothing to port.

## Verification protocol (every phase)

1. `make validate` (cortex checkpoints) green before and after.
2. Cortex boot + gameplay-tape hashes unchanged for pure moves.
3. MIPS guest builds for editor-playtest; engine+runtime host tests.
4. The perf tape's deadline-miss rate does not regress (phase 1/2
   must be cost-neutral; phase 3 owns a written budget).
5. Frontend/editor builds with the `editor` feature; the emulator
   suites stay green (the runtime is upstream of nothing in emu/,
   but the playtest disc it produces is validated there).
