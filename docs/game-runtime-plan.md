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
2. **Grid world, not BSP.** Rooms are Tomb-Raider-style sector grids
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
