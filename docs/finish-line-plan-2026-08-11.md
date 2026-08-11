# PSoXide, Quake-PSX, and demo-disc finish-line plan

Date: 2026-08-11

Status: authoritative execution plan after the architecture correction at
PSoXide commit `04d53bae`.

This plan replaces any continuation plan that treats PSoXide Editor as a
Quake level editor. The detailed evidence history remains in
`docs/quake-psoxide-convergence-handoff.md`; this document defines what must
happen next and what "finished" means.

## 1. Outcome

The campaign is finished only when all of these products are true at the same
time:

1. PSoXide Editor can create, edit, save, cook, rebuild, and play a new BSP
   level for the owner's souls-like game.
2. That level can author and run the real PSoXide gameplay vocabulary: player,
   enemies, equipment, weapon sockets, hit and hurt volumes, doors, triggers,
   props, liquids, checkpoints, combat, death, and reset.
3. Quake-PSX plays the complete Quake shareware Episode 1 single-player route
   through its own Quake content cooker and gameplay runtime.
4. Both games use the same canonical reusable BSP foundation for traversal,
   PVS, contents, transformed movers, and collision behavior.
5. The opt-in local/test demo disc contains separate PSoXide souls-like and
   Quake-PSX entries, is built from exact clean revisions, and chain-loads both.
6. Host tests, real MIPS builds, deterministic image-free emulator gates, and
   the original-PlayStation battery support the claims made for each product.

The near-term priority is item 1 and item 2. The user should receive a real
level-building and playtesting workflow before the longer Quake Episode 1 tail
is allowed to dominate the schedule.

## 2. Architectural boundary

The shared part is an engine, not a game or authoring format.

```text
PSoXide Editor
  -> PXBSP geometry and PSoXide gameplay records
  -> PSoXide souls-like runtime
  -> shared psx-bsp engine

Quake BSP29 + shareware assets
  -> Quake-specific cook to PSB/WORLD.PAK
  -> Quake-PSX gameplay runtime
  -> Quake Z-up adapter
  -> shared psx-bsp engine

Final demo disc
  -> separate PSoXide souls-like entry
  -> separate Quake-PSX entry
  -> separate hardware-test entry
```

The canonical shared layer owns:

- validated resident BSP structures;
- allocation-free iterative BSP and clip-hull traversal;
- PVS decompression and leaf/cluster queries;
- Quake contents codes and contents sampling;
- transformed brush-model collision semantics;
- fixed-point failure, capacity, and determinism contracts.

The PSoXide side owns:

- PXBSP authoring and cooking;
- PSoXide materials, models, lights, props, and gameplay records;
- Y-up editor and runtime policy;
- souls-like combat, equipment, checkpoints, and game state;
- editor workflow and PS1 budget diagnostics.

The Quake side owns:

- BSP29/shareware ingestion and Quake PSB/WORLD.PAK cooking;
- Quake Z-up conversion at a narrow adapter boundary;
- Quake entities, movement, monsters, weapons, targets, inventory, and maps;
- Quake menus, HUD, audio, presentation, and Episode 1 progression.

The demo-disc side owns:

- opt-in inclusion, image relocation, TOC, chain-loading, and receipts;
- final source and artifact provenance;
- preservation of every existing non-Quake pressing variant.

### Explicit non-goals

- PSoXide Editor will not load or author Quake gameplay levels.
- Quake-PSX will not consume PSoXide editor projects or PSoXide gameplay
  records.
- The projects do not need one interchangeable on-disc level format.
- No Quake-to-PXBSP gameplay importer is required.
- TrenchBroom is an interaction reference, not a file-format or game-design
  requirement.
- Quake multiplayer, a general `progs.dat` virtual machine, and mod support are
  outside this finish line unless the owner expands scope.
- This plan finishes the PSoXide game toolchain and a representative
  souls-like vertical slice. It does not manufacture the full game's future
  content library.
- Engineering an opt-in local/test Quake pressing does not grant permission to
  redistribute Quake data.

## 3. Frozen starting point

| Lane | Worktree | Baseline | State |
|---|---|---:|---|
| PSoXide integration | `/Users/ebonura/Desktop/repos/PSoXide-convergence` | `04d53bae` | One preserved user change in `editor/projects/brush-first-playable/project.ron` |
| Quake integration | `/Users/ebonura/Desktop/repos/quake-psx-convergence` | `09ff502` | Clean |
| Quake E1M1 route | `/Users/ebonura/Desktop/repos/quake-psx-target-graph-start` | committed base `d7e521e` | Dirty, old-base implementation and temporary diagnostics; never merge as-is |
| Demo disc | `/Users/ebonura/Desktop/repos/psx-demo-disc-quake-shareware` | `ba250ef` | Clean, proven old-pin chain-load |

The PSoXide user change is a saved orbit camera and final-newline difference.
It must not be reset, staged, formatted, or committed without an owner choice.

The two exploratory editor-to-Quake worktrees are abandoned. They are not a
source of requirements or implementation.

### What is already real

PSoXide already has:

- PXBSP brush cook, PVS, world rendering, collision hulls, movers, liquids,
  BSP-native entity activation, and authored prop collision;
- a visible Move/Resize/Edge/Vertex workflow, 2D and 3D selection, numeric
  Origin/Size, face materials, clip and Hollow, safe file watching, Draft and
  Release paths, Play freshness, and a large roofless starter courtyard;
- a deterministic blank-project edit/save/reopen/cook/MIPS/disc/replay gate;
- a controlled BSP combat fixture with real equipment, sockets, weapon pose,
  authored player capsules, hurt volumes, stagger, death, and door occlusion.

Quake already has:

- Start and E1M1 through E1M8 cooked from verified shareware inputs;
- the full standard eight-weapon set and persistent inventory/HUD state;
- deterministic BSP movement and mover collision through the shared tracer;
- bounded Soldier and Dog runtime, player damage and death, sounds, water and
  sky rendering, map changes, and several deterministic regression builds;
- reproducible shipping artifacts and a fail-closed PSoXide source contract.

The demo-disc branch already proves a separate Quake whole-image chain-load,
but only against stale PSoXide and Quake pins.

## 4. Definition of done

### 4.1 Shared BSP foundation

Shared BSP is done when:

- shipping PSoXide BSP levels and shipping Quake maps reach the same
  `psx-bsp` traversal, PVS, contents, mover-transform, and collision kernels;
- adapters own axis, scale, and format policy, with no game-specific entity
  logic in `psx-bsp`;
- PSoXide BSP projects cannot silently fall back to `WorldGrid`, room-local
  coordinates, or `RoomIndex` for spatial authority;
- exact epsilon, zero-length, solid, liquid, axial, non-axial, translated,
  rotated, malformed, cyclic, 64-entry, and 65-entry behavior is pinned;
- failure leaves the caller-owned trace output unchanged and scratch is
  reusable;
- no new heap allocation, recursion, float, or 64-bit arithmetic enters guest
  traversal, movement, visibility, or combat hot paths;
- a source audit finds no second shipping BSP or collision authority.

### 4.2 PSoXide Editor and souls-like runtime

This lane is done when a fresh project, not a cloned combat fixture, can be
used to:

1. create and reshape a roofless multi-room BSP level in 2D and 3D;
2. assign materials and liquid/solid contents;
3. add a door or other mover, static props, collidable props, and lights;
4. place and configure a player start, checkpoint, enemy, and trigger;
5. select real character, equipment, weapon, socket, animation, hit capsule,
   and hurt-volume records through editor UI;
6. save, close, reopen, edit, cook in Draft and Release, rebuild, and Play;
7. run the production PSoXide gameplay runtime with BSP movement, camera
   collision, door collision, prop collision, PVS activation, and liquids;
8. render an attached weapon from the same retained simulation pose used for
   its active hit volume;
9. damage one enemy once per attack token, enter poise stagger, kill it, and
   cross the cleared route;
10. receive a genuine authored enemy attack when valid source content exists,
    die, reset at a checkpoint, and restore the documented souls-like world
    state;
11. report invalid references, invalid brushes, fixed-capacity overflow, RAM,
    VRAM, PVS, texture, and packet budget failures at the authored source;
12. reproduce the project and cooked output from a clean checkout without
    relying on ignored generated files.

If there is still no genuine Mantis attack clip/volume, do not relabel its
legacy fallback as authored content. Complete the authoring/runtime feature
with an honestly sourced enemy attack or record enemy reciprocal damage as a
content limitation. The editor feature itself must still allow the owner to
author that data.

The user performs the final native-window acceptance. Automated agents may
prove behavior through real egui input and headless replays, but may not claim
manual usability on the user's behalf.

### 4.3 Quake-PSX shareware Episode 1

Quake is done when:

- Start and every E1M1 through E1M8 map can be entered, played, exited, and
  revisited through the authored route graph where applicable;
- all Episode 1 classnames are classified as implemented, intentionally inert,
  or unsupported, and no required gameplay classname is silently inert;
- doors, buttons, counters, lifts, trains, teleports, secrets, changelevels,
  hazards, water, keys, items, armor, ammo, weapons, and powerups behave;
- Quake movement covers ground, air, stairs, ledges, water, friction,
  acceleration, damage, death, and restart with dynamic body blocking;
- Soldier, Dog, Demon, Enforcer, Fish, Hell Knight, Knight, Ogre, Shalrath,
  Shambler, Tarbaby, Wizard, Zombie, Chthon, and the Episode 1 end behavior run
  with their required projectiles, attacks, pain, death, gib, movement, and
  special-state logic;
- monster/player and monster/monster collision is bounded and deterministic;
- the secret-map route and normal episode route both work;
- the Chthon finale, intermission, and return to Start work;
- view models, HUD, menus, pause/options required for the demo, entity
  animation, sprites, particles, explosions, flashes, and spatial audio are
  coherent enough that no gameplay event is represented only by telemetry;
- a deterministic per-map gate and a complete episode gate exercise authored
  behavior rather than diagnostic teleports;
- the complete workload meets the agreed frame budget on original hardware.

Single-session Episode 1 completion is required. Save/load is not required by
the current demo-disc scope; if the owner wants durable saves, add it as a
separate explicit gate rather than allowing it to appear accidentally at the
end.

### 4.4 Demo disc

The local/test demo disc is done when:

- the normal public disc target remains unchanged;
- the explicit Quake target consumes the complete relocated Quake disc image,
  not a bare EXE;
- the disc has separate PSoXide souls-like, Quake-PSX, and hardware-test
  entries;
- receipts bind clean exact PSoXide and Quake revisions, the verified PAK,
  guest recipe, toolchain, EXE, BIN, CUE, relocated image, TOC, and final disc;
- missing, dirty, stale, drifted, or mismatched inputs fail closed;
- Quake boots twice from the carousel and reaches the same authored checkpoint
  as its standalone image without writing screenshots;
- the PSoXide souls-like entry also chain-loads and runs its final vertical
  slice;
- all existing default and special pressing variants still pass their checks;
- the generated PAK, EXE, BIN, CUE, and combined image remain uncommitted;
- public/web/pressed distribution remains a separate legal owner decision.

### 4.5 Evidence levels

Every completion claim must state its evidence level:

1. Source and host tests.
2. Real `mipsel-sony-psx` build.
3. Two deterministic, image-free headless replays from the exact source-built
   frontend and exact guest.
4. Original PlayStation run.

Level 3 is not Level 4. Emulator agreement cannot prove GPU pacing, DMA, CD
seek/settle, SPU envelopes, CD-DA, controller timing, or real 30 FPS.

## 5. Critical path and merge train

The work should run as two implementation lanes and one release lane, with a
small shared-engine gate in front of them.

```text
F0 Baseline and contract freeze
  -> S1 shared BSP authority audit
     -> P1 PSoXide authored souls vertical slice
        -> P2 editor production hardening
        -> P3 PSoXide final pin
     -> Q1 salvage E1M1 target route
        -> Q2 Quake common gameplay systems
        -> Q3 Quake monster/content batches
        -> Q4 full Episode 1 and performance
  -> I1 exact-pin convergence matrix
     -> D1 final opt-in demo disc
        -> H1 original-hardware acceptance
           -> R1 release documentation and owner decision
```

P1 and Q1 can proceed in parallel after S1. Quake work may use a provisional
clean PSoXide pin while the shared API is frozen, but its final provenance build
must use P3. The demo disc must not be repinned between overlapping PSoXide or
Quake runtime merges.

## 6. Work packages

### F0. Freeze one trustworthy baseline

Tasks:

- preserve the user-owned camera edit;
- record every active worktree, branch, head, dirty file, and generated output;
- mark the two editor-to-Quake experiments abandoned;
- capture the dirty E1M1 route diff and separate production work from temporary
  diagnostic examples;
- create one current requirements matrix linking each completion claim to its
  owner, source path, test, MIPS gate, replay gate, and hardware gate;
- mark old grid-only architecture documents historical where they contradict
  the current BSP direction;
- state that the editor is a PSoXide souls-like editor at the top of all active
  authoring documents.

Exit gate:

- no unknown worktree or unowned dirty file;
- no active plan asks for a Quake editor/importer;
- baseline revisions and limitations agree across the handoff, README files,
  and validation documents.

### S1. Freeze and challenge the shared BSP contract

Tasks:

- trace PSoXide and Quake shipping call sites into `psx-bsp`;
- inventory any remaining local traversal, PVS, contents, or mover transform;
- assert PSoXide Y-up and Quake Z-up conversions at one boundary each;
- compare static and transformed collision, contents, epsilon, overflow, and
  malformed-data behavior against independent golden cases;
- verify touched-leaf/PVS entity activation beyond the smallest fixture and
  define bounded overflow behavior;
- profile the caller-owned scratch and dynamic blocker composition with
  worst-case authored capacities;
- run the numeric-policy audit over modified guest hot paths.

Exit gate:

- one canonical shared implementation per behavior;
- adapters contain policy only;
- all shared and parity tests pass;
- any retained divergence is named, justified, and tested.

### P1. Join editor authoring to the real souls-like runtime

This is the first user-value milestone and should be finished before broad UI
polish or the long Quake monster tail.

Tasks:

1. Create a new tracked `souls-bsp-vertical-slice` project from the production
   New Project path, not by cloning the combat fixture.
2. Author its geometry through ordinary editor records: two combat spaces,
   one doorway/mover, one liquid or hazard region, and several representative
   collidable props.
3. Complete the BSP-space Add/Place/Inspector paths for player start,
   checkpoint, enemy, trigger, equipment, weapon, socket, hit shape, hurt
   shape, door state, prop state, and light records.
4. Remove fixture-only defaults and hidden generator knowledge from the cook.
5. Cook world-space records directly into the production runtime package.
6. Ensure PVS/leaf activation, actor bodies, movers, props, liquids, camera,
   and melee traces all compose in one movement tick.
7. Use the retained simulation pose as the sole source for rendering, sockets,
   hit capsules, hurt volumes, and attack tokens.
8. Implement and test checkpoint activation, player death delay, respawn,
   enemy reset, transient door/trigger reset, and explicitly persistent world
   state.
9. Add `make editor-souls-bsp-check`, an image-free gate that creates or
   regenerates the project, edits/saves/reopens it, cooks, builds a real MIPS
   executable and disc, and replays twice.
10. Pin counters for door use, weapon attachment, attack attempts, accepted
    hits, duplicate-hit rejection, stagger, enemy death, player damage, player
    death, checkpoint reset, final route crossing, PVS activation, and liquid
    damage where used.

Exit gate:

- the blank authoring loop and combat fixture are no longer separate proofs;
- a fresh editor-authored level executes the real souls-like runtime;
- changing geometry or gameplay data changes only the intended cooked/runtime
  evidence;
- no BSP path reads grid spatial authority;
- the user can open the built editor and follow a short acceptance script to
  build and play a level without command-line generated-file intervention.

### P2. Finish the practical editor workflow

Verify existing features first; implement only the missing or falsified parts.

Tasks:

- create brushes from Top, Front, Side, and 3D where useful;
- complete direct face, edge, vertex, arbitrary-plane, multi-brush, duplicate,
  delete, group, snap, numeric, and texture-lock behavior;
- keep selection synchronized across all views and expose every active
  transform mode without hidden modifier grammar;
- make each drag one undo transaction and verify redo across save/reopen;
- persist view framing, orientation, tool, selection where appropriate, and
  editor project state without polluting runtime content;
- complete face material/UV controls and content assignment;
- make entity placement and record references searchable and inspectable;
- make invalid brush, missing reference, capacity, cook, and budget errors
  select and focus the offending authored object;
- prove Play and Rebuild cannot use stale saved, cooked, guest, or disc state;
- prove external replace/delete/recreate and conflict/reload/save behavior;
- keep the large roofless neutral-material starter as the New Project default;
- keep all automated gates image-free;
- give the owner a native acceptance checklist and wait for the owner's result
  before claiming ergonomics complete.

Exit gate:

- the user can block out, texture, populate, cook, and play a small souls-like
  level from scratch;
- every failure points back to an actionable authored source;
- clean-checkout reproduction and exact recook pass;
- owner acceptance has no blocker in the core loop.

### P3. Close the legacy grid boundary and freeze a final PSoXide pin

Recommended decision:

- new projects are BSP-only;
- old grid projects remain an explicit compatibility mode while they still
  matter;
- grid code is not allowed to act as fallback authority for BSP projects;
- destructive grid removal is deferred until compatibility users are known.

Tasks:

- add or verify an explicit project-format discriminator;
- classify every grid authoring, cook, runtime, collision, navigation,
  streaming, and data path as BSP replacement, compatibility-only, or retire;
- test that old grid projects still load during the compatibility window;
- fail closed if a BSP project attempts to instantiate grid spatial state;
- update active runtime/editor architecture docs;
- run the complete PSoXide test, MIPS, deterministic replay, budget, and
  performance matrix;
- create one clean PSoXide pin for Quake and the demo disc.

Exit gate:

- PSoXide has one spatial authority for new projects;
- compatibility is isolated and named rather than silently mixed;
- the final pin is clean, reproducible, and contains no user-owned camera edit
  unless the owner explicitly chooses it.

### Q1. Salvage and integrate the E1M1 target route

Tasks:

- review the dirty old-base route diff file by file;
- finish buttons 211 and 212, the counter, final door, message, and crossing;
- preserve the exact i32 mover wait conversion and boundary tests;
- delete temporary `examples/` diagnostics and any diagnostic-only runtime
  behavior;
- replay the route twice from a real MIPS build;
- port the reviewed route changes onto a fresh branch from current Quake head
  rather than merging the dirty worktree wholesale;
- resolve `entity.rs`, `quake.rs`, `main.rs`, and builder changes without
  losing weapons, Soldier/Dog, or provenance gates;
- rerun Start, map, combat, arsenal, monster, audio, ambient, and disc gates.

Exit gate:

- one clean route integration commit or small reviewable series;
- no diagnostic coordinates or teleport shortcuts in the shipping proof;
- authored E1M1 target logic reaches the final crossing deterministically.

### Q2. Implement common Quake gameplay systems before more one-off content

Tasks:

- complete player movement parity needed by Episode 1: stairs, ledges, water,
  ground/air acceleration, friction, fall/hazard damage, death, and restart;
- add bounded dynamic body collision for player/monster and monster/monster;
- implement reusable ground, flying, swimming, leap, projectile, hitscan,
  melee, pain, death, corpse, and gib state machinery;
- complete target graph behaviors: doors, buttons, counters, lifts, trains,
  teleports, relays, secrets, messages, changelevels, and delays;
- complete item and powerup families used by Episode 1;
- define deterministic fixed pools and explicit overflow policy for actors,
  projectiles, particles, sounds, and target events;
- make water, slime, lava, sky, and mover semantics agree with the shared
  engine where they overlap while retaining Quake policy in Quake code;
- add per-system host tests and authored-map runtime probes.

Exit gate:

- remaining monsters can be expressed as data plus bounded special behavior,
  not as duplicated mini-engines;
- all Episode 1 non-monster classnames have an honest implementation status;
- no required common system is represented only by a diagnostic.

### Q3. Complete Episode 1 monsters in dependency batches

Implement in batches so each batch reuses Q2 machinery:

1. Ground melee/hitscan: Knight and Enforcer, then harden Soldier/Dog body and
   leap behavior.
2. Ground projectile/heavy: Ogre, Hell Knight, and Demon.
3. Flying/swimming: Wizard, Shalrath, and Fish.
4. Special survivability/death: Zombie, Tarbaby, and Shambler.
5. Episode bosses and end state: Chthon and the Episode 1 finale behavior.

Each batch must include:

- exact authored model frames and bounds;
- sight, idle, attack, pain, death, corpse, and gib behavior as applicable;
- projectile/sound/effect dependencies;
- world, mover, and actor collision;
- difficulty filtering and fixed-capacity counts from real maps;
- unit tests plus a deterministic authored-map MIPS replay;
- a negative test showing the gate fails when the new behavior is disabled.

Exit gate:

- every required Episode 1 monster classname has gameplay behavior;
- the all-map matrix has no required inert monster;
- resource and fixed-pool budgets cover worst authored map populations.

### Q4. Complete the episode, presentation, and performance

Tasks:

- build per-map canonical routes for Start and E1M1 through E1M8;
- build a normal full-episode route and a secret-map route;
- prove keys, secrets, hazards, water, trains, teleports, damage, death,
  restart, Chthon, intermission, and return to Start;
- replace bounded diagnostic checks with authored gameplay wherever an authored
  route exists;
- complete required sprites, particles, explosions, flashes, entity
  animation, view-model feedback, HUD, menu, pause/options, and spatial audio;
- implement `episode1-regress` as two deterministic image-free real-MIPS runs
  plus smaller per-map regressions for failure localization;
- profile worst PVS, entity, particle, projectile, audio, and target workloads;
- optimize without reducing intended visual quality;
- keep historical guest 64-bit paths visible and remove them when profiling or
  policy requires, never by falsely claiming they are already absent.

Exit gate:

- a player can complete shareware Episode 1 in one session;
- all required authored behavior is exercised;
- deterministic emulator gates pass;
- original hardware meets the agreed frame and correctness target.

### I1. Run the exact-pin convergence matrix

Tasks:

- build the frontend from the exact final PSoXide pin;
- hydrate Quake from that same clean pin and verify lockfile, stamp, revision,
  dirty-state, and source-kind truth;
- build Quake in two different clean checkout paths and compare EXE, BIN, CUE,
  sidecar, and cooked package bytes;
- run all PSoXide and Quake host suites and all image-free replay gates;
- make one controlled authored change in each product and prove the expected
  artifact and behavior change;
- return to the exact final revisions and reproduce the pinned hashes;
- run source audits for duplicate BSP, collision, pose, combat, grid, and
  generated-manifest authorities;
- write a final discrepancy report with confirmed, contradicted, incomplete,
  and untested claims.

Exit gate:

- exact source, input, output, and replay provenance is reproducible;
- no stale frontend, stale hydration, ignored generated file, dirty tree, or
  copied fixture can make a false-green gate;
- every remaining limitation is explicitly outside the agreed finish line.

### D1. Repin and prove the final demo disc

Tasks:

- repin only after P3 and Q4 are final;
- update Quake source, PSoXide source, shareware input, guest recipe, artifact,
  relocation, TOC, and final-disc receipts together;
- run `make check`, `make relocation-check`, `make quake-verify`, and
  `make quake-headless-check`;
- add a headless gate for the final PSoXide souls-like entry if one is not
  already covered at the combined-disc layer;
- verify two Quake chain-loads and two PSoXide chain-loads without images;
- verify hardware tests remain present and all old pressing variants remain
  buildable;
- audit the final tree to ensure no PAK or generated disc artifact is tracked.

Exit gate:

- one reproducible opt-in local/test pressing contains both separate games;
- standalone and relocated behavior agree where disc timing does not require a
  documented exception;
- distribution is still off until the owner makes the legal decision.

### H1. Original PlayStation acceptance

Run the existing hardware ladder in order:

1. exact-source headless replay;
2. interactive emulator only if a problem needs inspection;
3. hardware-test battery in the combined disc;
4. owner-authorized burn, CRT capture, and diagnostic decoding.

The console pass must cover:

- cold boot and repeated chain-loads;
- controller input and pause/resume;
- PSoXide level editing artifacts running as the souls-like demo;
- Quake Start, representative map changes, worst combat, water/hazards,
  Chthon, intermission, and return;
- GPU pacing/tearing/corruption, DMA, CD seeks and streamed WORLD.PAK reads;
- SPU one-shots, spatial loops, voice lifetime, and optional CD-DA behavior;
- long-run stability and measured frame cadence at worst workloads;
- the on-disc hardware-test suite and decoded results.

Do not change game behavior to match an emulator when silicon disagrees.
Record confirmed emulator gaps in the established hardware documentation.

Exit gate:

- no console correctness blocker;
- the performance claim is backed by console measurement;
- any remaining silicon limitation is explicit and accepted by the owner.

### R1. Final documentation and release decision

Tasks:

- update the handoff, READMEs, validation logs, architecture, editor workflow,
  Quake coverage matrix, demo-disc receipt documentation, and hardware results;
- mark stale grid-only and interim RC documents historical;
- provide exact user instructions for creating a PSoXide souls-like BSP
  project and for building the opt-in Quake demo disc;
- record clean heads, artifact hashes, test counts, replay counters, and
  hardware evidence without copying stale values;
- leave all worktrees clean except explicitly accepted owner changes;
- ask the owner separately whether to push/publish and whether Quake data may
  be included in a distributable pressing.

Exit gate:

- a completely different model or engineer can reproduce every final claim;
- the documentation describes the architecture that actually ships;
- engineering completion and publication permission remain distinct.

## 7. Validation command matrix

Commands are run from the named worktree and workspace. Exact counts and
hashes must be refreshed at final heads.

### PSoXide host and runtime

```sh
cd /Users/ebonura/Desktop/repos/PSoXide-convergence/editor
cargo test -p psxed-project --lib
cargo test -p psxed-ui --lib

cd /Users/ebonura/Desktop/repos/PSoXide-convergence/engine
cargo test -p psx-bsp --lib
cargo test -p psx-engine --lib render
cargo test -p psx-game-runtime --lib

cd /Users/ebonura/Desktop/repos/PSoXide-convergence
make editor-blank-playtest-check
make editor-bsp-liquid-check
make combat-checkpoint
make editor-souls-bsp-check       # planned in P1
```

### Quake host, cook, MIPS, and replay

Use a clean worktree at the final PSoXide pin for every command until the
published dependency and `PSOXIDE_REV` are identical.

```sh
cd /Users/ebonura/Desktop/repos/quake-psx-convergence
cargo test
(cd crates/quake-cook && cargo test)
(cd crates/quake-core && cargo test)
(cd crates/quake-formats && cargo test)
cargo run --release -- check --psoxide /path/to/final-clean-PSoXide-pin
cargo run --release -- compile --psoxide /path/to/final-clean-PSoXide-pin
cargo run --release -- map-regress --psoxide /path/to/final-clean-PSoXide-pin
cargo run --release -- combat-regress --psoxide /path/to/final-clean-PSoXide-pin
cargo run --release -- monster-regress --psoxide /path/to/final-clean-PSoXide-pin
cargo run --release -- arsenal-regress --psoxide /path/to/final-clean-PSoXide-pin
cargo run --release -- audio-regress --psoxide /path/to/final-clean-PSoXide-pin
cargo run --release -- ambient-regress --psoxide /path/to/final-clean-PSoXide-pin
cargo run --release -- episode1-regress --psoxide /path/to/final-clean-PSoXide-pin  # planned in Q4
cargo run --release -- disc --psoxide /path/to/final-clean-PSoXide-pin
```

### Demo disc

```sh
cd /Users/ebonura/Desktop/repos/psx-demo-disc-quake-shareware
make check
make relocation-check
make quake-verify
make quake-headless-check
```

### Rules for every replay gate

- Build the frontend from the exact PSoXide source under test.
- Build the guest for the real MIPS target.
- Run twice and compare all deterministic telemetry and hashes.
- Write no screenshots, framebuffer dumps, or image artifacts.
- Assert controller polls and guest frames so a boot/menu false positive fails.
- Assert authored gameplay counters, not only final coordinates.
- Assert a negative mutation makes the gate fail.
- Preserve exact commands, revisions, toolchain identity, input digests, and
  output digests.

## 8. Adversarial challenge protocol

An independent review occurs before P3, after Q4, and before D1. It must try to
disprove the milestone instead of confirming the implementation narrative.

For every major claim ask:

- Who owns the canonical data?
- At which simulation tick is it produced?
- Which consumers read it?
- Is a second path recomputing or mutating a competing value?
- What are the axis, units, fixed-point scale, rounding, and saturation rules?
- What happens at fixed-capacity overflow or malformed input?
- Can the test pass without executing the shipping guest path?
- Could a stale frontend, hydration, generated file, fixture clone, or dirty
  checkout produce the result?
- Does the negative test fail when the claimed feature is disabled?
- Is an emulator result being described as hardware evidence?

Mandatory falsification targets:

- PSoXide BSP projects accidentally instantiating grid spatial state;
- render and combat sampling different animation poses;
- both legacy arc and capsule damage applying to one attack;
- mover, prop, actor, or camera traces using divergent transforms;
- editor Play using stale project, cook, guest, or disc output;
- Quake resources cooking successfully while gameplay classnames stay inert;
- diagnostics or teleports replacing authored map behavior;
- all-map gates that do not prove death, hazards, secrets, or boss behavior;
- demo receipts describing artifacts that were rebuilt after receipt creation;
- original-hardware claims inferred from deterministic emulator hashes.

Each audit produces a durable discrepancy table:

| Claim | Evidence checked | Outcome | Impact | Correction | Milestone change |
|---|---|---|---|---|---|

Allowed outcomes are `confirmed`, `contradicted`, `incomplete`, and `untested`.

## 9. Risk register

| Risk | Consequence | Control |
|---|---|---|
| Editor/Quake scope conflation returns | Wrong importer and shared gameplay schema consume time | Enforce section 2 in code review and active docs |
| Generic editor loop and combat fixture stay separate | False claim that users can author the game | P1 fresh production-path vertical slice |
| Legacy grid remains hidden authority | Two worlds and divergent collision/activation | P3 discriminator, fail-closed BSP path, compatibility boundary |
| Dirty old-base E1M1 route is merged wholesale | Weapons, monsters, or provenance regress | Q1 reviewed port onto current head |
| Quake system work becomes per-monster duplication | Long tail and capacity drift | Q2 common state machinery before Q3 batches |
| Cooked classname mistaken for implemented behavior | Maps load but do not play | Coverage matrix plus negative runtime gates |
| Stale PSoXide hydration or frontend | Reproducible-looking false green | Exact clean pin, lock/stamp check, source-built frontend |
| Fixed-capacity success only on tiny fixtures | Real maps overflow or drop behavior | Worst-map capacity tests and explicit overflow policy |
| Guest 64-bit math enters hot paths | PS1 performance regression | Numeric guard plus focused profiling |
| Screenshot-based testing obscures state | Slow, fragile, and weak evidence | Image-free telemetry and deterministic hashes |
| Emulator-only completion | Console GPU/CD/SPU failure | H1 hardware battery before final claim |
| Quake data is accidentally committed/distributed | Legal and repository problem | Opt-in local input, ignore audit, separate owner gate |
| User camera edit is overwritten | Loss of user work | Preserve until explicit owner choice |

## 10. Progress reporting

Report status by exit gate, not by elapsed hours or number of commits.

Every report should contain:

- current PSoXide, Quake, and demo heads;
- dirty worktree inventory;
- last exit gate reached;
- current work package and its remaining stop conditions;
- exact tests run since the previous report;
- newly discovered contradiction or risk;
- the next user-testable outcome;
- whether any evidence is host-only, MIPS, emulator, or hardware.

Use these milestone names only after their exit gates pass:

1. `PSoXide Authored Souls Slice`
2. `PSoXide Editor Production Candidate`
3. `Quake Episode 1 Gameplay Complete`
4. `Exact-Pin Convergence Candidate`
5. `Combined Demo Disc Candidate`
6. `Original Hardware Accepted`

## 11. Immediate next actions

1. Execute F0 and S1 as short audits, correcting active documentation and
   freezing the shared API without expanding scope.
2. Start P1 immediately. It is the shortest path to letting the user build and
   test actual game levels.
3. In parallel, complete Q1 by salvaging the authored E1M1 route onto current
   Quake without its diagnostics.
4. After P1, hand the user the exact current editor binary and a concise native
   acceptance script. Fix blockers before broadening editor polish.
5. Continue P2/P3 and Q2/Q3 in parallel behind their common S1 boundary.
6. Do not repin the demo disc until both final product heads are stable.
7. Run I1, D1, H1, and R1 in that order. Any failure returns to the owning work
   package, not to a workaround in the release layer.

The fastest honest route over the finish line is therefore: join the PSoXide
editor and souls runtime first, finish Quake by reusable systems and authored
map gates second, then perform one exact-pin demo-disc and hardware closure.
