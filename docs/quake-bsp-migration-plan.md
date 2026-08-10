# Quake BSP world migration plan

Status: plan drafted 2026-08-10, branch `quake-bsp-world`. No code yet.

## Decision

Replace the sector-grid ("Tomb Raider style") world with a brush/BSP world across
the whole stack, modelled directly on quake-psx:

- The editor's level workflow becomes brush-based authoring inside psxed
  (convex brushes, face/edge/vertex manipulation, texture alignment,
  in-editor compile), following the Quake `.map` authoring model. No
  external tools.
- The engine adopts the Quake world architecture underneath: BSP tree + PVS
  visibility, classic-affine surface rendering, clipnode hull collision,
  whole-map-resident loading. Implemented in Rust; the quake-psx C core is the
  reference oracle, never linked.
- We KEEP the existing entity system and skeletal animation pipeline
  (psx-asset models, psxanim v3, model_rendering, combat/FSM). They are more
  mature than Quake's progs/MDL equivalents.
- The one net-new capability is level streaming, which Quake does not have and
  we need regardless.

Licensing: PSoXide and quake-psx are both GPL-2, so porting/adapting the Quake
tree and id-derived tool code is clean.

## Why this wins (measured, not vibes)

Same hardware model, same emulator:

- cortex_ignition_v1 gameplay: render vblank ~843k cycles (150% of one vblank),
  30 fps slot ~102% full, open sightlines up to 1.7M cycles/vblank because the
  grid world has no PVS and no distance cut (docs/perf-30fps.md,
  stress-map findings).
- quake-psx Episode 1 full route: update+render average 747k cycles/frame,
  p95 1,051k against the 1,128,960-cycle two-vblank budget, whole-route
  33.4 fps average on the hardware-timing build, with more complex geometry,
  nine monster families live, and baked lighting (quake-psx VALIDATION.md).

The difference is architecture: PVS bounds the visible set no matter the map,
faces are precompiled render-ready, lighting is baked at cook, and the loop
free-runs with variable dt so overload degrades smoothly instead of missing
fixed deadlines.

## What quake-psx actually is (verified 2026-08-10)

- Retained C core: `src/*.c` 7,247 lines (engine) + `src/progs/*.c` 7,928 lines
  (Quake gameplay, which we replace with our entity system). Key files:
  `world.c` 575 (hull trace, edict links, area nodes), `pmove.c` 232,
  `r_main.c` 798 (`R_MarkLeaves`, `R_RecursiveWorldNode`), `r_surf.c` 728,
  `model.c` 518 (XBSP load). Loop: free-run, dt = elapsed vblanks as x16 fixed
  (`system.c:42`).
- Rust/C ABI (`src/psoxide.h`): ~60 functions. The heavy render paths are
  ALREADY Rust SDK code: `psoxide_render_classic_affine_{fan,batch,packed_fan}`
  and `psoxide_render_classic_alias_{model,view_model}`, plus GTE projection,
  AVSZ, AABB clip classify.
- Map format XBSP (`src/bspfile.h`): one file per map, 15 lumps: TEXDATA (VRAM
  pages), SNDDATA (SPU, preassigned addresses), MDLDATA, VERTS (s16 pos, u8 UV,
  baked light or packet RGB), PLANES, TEXINFO, FACES (XFACE_BAKED_UV/LIGHT),
  MARKSURF, VISILIST (PVS), LEAFS, NODES, CLIPNODES (6-byte records, 4 hulls),
  MODELS (brush submodels), STRINGS, ENTITIES (binary records, u8 classname).
- Cooker: `tools/bspconvpsx` ~3k lines C, input is stock .bsp29 + assets.
- Memory: mark-arena; the Rust side budgets `MAX_RESIDENT_MAP_BYTES = 1.1 MB`
  per resident map; world textures stream to VRAM x=320 w=640.
- An all-Rust rewrite has STARTED in quake-psx (branch `codex/all-rust-quake`,
  uncommitted, parallel session): Rust runtime root (`game/src/quake.rs`) plus
  `crates/quake-formats`, an 819-line no_std XBSP parser with typed lump
  accessors. Boot + resident map load work; sim/render/UI not yet. Treat that
  working tree as read-only from here; copy what we need, reconcile later.

## Head start already inside PSoXide

- `engine/crates/psx-engine/src/classic_affine.rs` (1,857 lines, on main): the
  classic Quake surface renderer in Rust: camera-space midpoint reprojection,
  two-level near lattice, GP0(3Ch) quad pairing, crack underdraw. Pixel-matched
  against the historical renderer (quake-psx RENDERING.md).
- `psx_gte::scene`: projection, signed-area cull, cached-depth AVSZ3/4 with the
  MTC2 commit gap handled. `OrderingTable::submit_async` double-buffer support.
  `TextureMaterial::with_dither`.
- Emulator: GPU command-cost model + hardware-timing route measurements, the
  route/screenshot/pc-sample CLI harness quake-psx's regression uses.
- Editor interaction layer (survey 2026-08-10): face/edge/vertex selection
  modes with welded/detached semantics, gizmos with axis/plane/face handles,
  snapping, marquee, snapshot undo, per-face material paint + eyedropper,
  per-face UV transforms, prefab stamping, headless pick-test harness
  (ViewportHarness). Roughly the whole interaction half of a brush editor.
- `BoxProp` + `PlaytestBoxPropSurface`: an editable 8-vertex hexahedron with
  per-face materials/UVs and a cooked free-quad runtime record that already
  renders on hardware. The nearest existing ancestor of a brush.

## What gets replaced or retired

- Sector grid world: `WorldGrid`/`GridSector` family, `.psxw` dense sector
  records, `floor_sample.rs` corner interpolation, motor wall probes
  (`sector_probe_wall` path in `character_motor.rs`), `plan_portal_rooms`,
  `cook_visibility.rs` cell PVS, room chunk streaming
  (`room_streaming.rs` 1,690 + `asset_streaming.rs` 896 + room_cache /
  room_window / room_visibility / world_cells / world_visibility).
- Editor grid tools: paint floor/wall/ceiling, sector inspector's grid-only
  parts, grid prefab internals (the prefab CONCEPT survives as brush groups).
- Retirement is the LAST phase, not the first: the grid path keeps compiling
  until the new world carries a full game slice.

## What is kept (and its one coupling problem)

- Entity system `psx-game-runtime/src/entities.rs` (1,951), combat, logic,
  character.rs, skeletal `model_rendering.rs` (1,862), materials/psxt, UI,
  SPU/CD-DA, memcard, telemetry/profiling, editor Play harness (tapes,
  profiler, headless launch flags).
- Coupling problem: entities live in ROOM-LOCAL coordinates with `RoomIndex`
  everywhere (positions, spawn records, active-room AI gating, hl-psx-style).
  The BSP world wants world-space positions with leaf/PVS-derived activation.
  Decision below.

## Design decisions taken now

1. **Format: PXBSP, an XBSP-derived format, not raw XBSP.** Same lump
   architecture and runtime-ready philosophy, with PSoXide-specific lumps:
   material table referencing psxt materials instead of Quake texinfo,
   entity records referencing our `LevelGameEntityRecord` shapes (u16 class
   ids, skeletal model refs) instead of `xbspent_t`, a streaming index lump
   (phase S), and the existing UI/asset pack integration. Start by copying
   `quake-formats` into a new `psx-bsp` crate and evolving it.
   `psx-bsp` lives in `engine/crates/` (decided 2026-08-10) and is written
   to be shared: no_std core so the guest runtime, the editor cook and
   quake-psx can all depend on it, quake-psx by path exactly like it already
   depends on the sdk crates. Expect this shared-crate pattern to recur as
   the migration proceeds.
2. **Entities move to world-space i32 positions.** `RoomIndex` activation is
   replaced by leaf/cluster tests against the PVS (Quake's FindTouchedLeafs
   pattern; quake-psx already caches AI eye leaves). A thin "area" concept
   (group of clusters) replaces rooms for streaming and coarse gating so the
   entity system's activation logic keeps its shape.
3. **Collision: port Quake hulls exactly.** 6-byte clipnodes, 4 hulls,
   `RecursiveHullCheck` semantics, box hulls for entities. character_motor
   keeps its POLICY layer (step heights, acceleration feel, third-person
   camera contract) but its world queries (floor height, wall block) are
   reimplemented on hull traces. Golden tests: record traces from the C
   oracle via quake-psx's regression harness, assert bit-equal results.
4. **Timing: the 60 Hz fixed sim / 30 Hz visual scheduler stays. Permanent
   (owner decision 2026-08-10).** The perf win comes from PVS + precompiled
   surfaces + baked light, not the clock, and the kept entity/motor code
   assumes SimTick. No free-run variable-dt experiment.
5. **Cooker in Rust, inside the editor cook.** bspconvpsx is only ~3k lines
   and we need the compile chain (CSG/BSP/vis/light) natively anyway for
   in-editor iteration. Draft compiles skip vis (everything visible);
   release cooks run full vis.
6. **Editor brushes are world-space plane-defined convex solids** (Quake
   `.map` semantics, Valve-220-style UV axes per face). Editor-side compile
   math may use host floats; cooked output stays integer (the runtime
   numeric guard applies to the guest only).

## Phases

Each phase has a gate; nothing merges without its gate green. Perf changes
follow the existing multi-frame visual-gate rule.

- **P0 psx-bsp crate.** Copy/adapt quake-formats into `engine/crates/psx-bsp`
  (or sdk if cleaner): XBSP parse + validate, host-side tests against a real
  E1M1.xbsp cooked by quake-psx tools. Gate: parse+walk every E1 map.
- **P1 world render.** BSP world module in psx-engine: leaf find, PVS mark,
  recursive front-to-back walk with node sphere/frustum culling, faces into
  `classic_affine`, double-buffered OT via submit_async. Example scene flies a
  camera through E1M1 headless. Gate: recognizable captures + profiled
  cycles/frame recorded against quake-psx's numbers on the same route.
- **P2 collision.** Hull tracer + point contents + box hulls in Rust;
  character_motor queries backed by traces; golden-trace parity vs the C
  oracle. Gate: player capsule walks E1M1 start-to-exit under tape input with
  no fall-throughs, steps and slopes feeling correct.
- **P3 entities on BSP.** World-space migration of entity records + spawn +
  activation via clusters; skeletal models render inside the BSP OT (AVSZ
  depth per model); materials on world faces via the PXBSP material lump.
  Gate: a cooked map with the cortex player + one enemy family fighting.
- **P4 editor brush core.** Tool-FSM refactor of the viewport dispatch (the
  current if/else cannot host brush/clip/vertex tools); `Brush` data model +
  plane-polygon solver; create/resize/move on existing gizmos + face drag;
  texture alignment (per-face UV axes, offset/rot/scale, eyedropper reuse);
  brush preview through frontend editor_preview (direct triangulation,
  pre-BSP). Ortho: extend the existing top-down pane, add front/side panes
  after (docking is a later nicety, not a blocker). Gate: author a two-room
  map with a doorway entirely with brushes in psxed.
- **P5 compile in cook.** CSG face clipping, BSP build, portal generation,
  PVS (draft mode skips it), light bake to vertex RGB (reuse editor lighting
  direction), PXBSP pack, Play-in-editor wiring. Gate: authored map compiles
  and plays in embedded Play with PVS verified by overdraw counters.
- **P6 streaming.** The new design. Two candidates, decided by measurement
  after P5 on a real oversized map:
  (a) Quake model + fast swaps: whole map resident within the ~1.1 MB
  envelope, CD-loads between maps, hub structure for continuity. Always
  works; baseline.
  (b) Region paging: cook partitions the map at portal chokepoints into
  region groups; tree/clipnodes/PVS/entities stay resident (small), face,
  vertex and texture payloads page per region over CD with the existing
  cd_stream machinery. Prototype behind a flag; adopt only if (a)'s limits
  actually bite on cortex-scale content.
  Gate: chosen design walks a segmented oversized map with emulator CD
  timing, no stalls, no pops in the PVS-visible set.
- **P7 cutover.** Existing projects are frozen on the grid, not ported (owner
  decision 2026-08-10): the next game starts fresh on the brush world. Delete
  the grid world, grid cook, room streaming machinery and grid editor tools;
  brush flow becomes the default new-project path. Gate: full suites green
  with the grid gone; hardware burn of a brush-world demo passes the standard
  console battery.

## Risks and mitigations

- **Rust vs LTO'd C on traversal hot paths.** The C core needed whole-program
  LTO to reach parity; Rust in one crate gets cross-inlining for free, and
  classic_affine already proved the hot-path pattern. Still: profile in P1,
  not at the end.
- **vis cost in-editor.** Full PVS on big maps is slow everywhere. Draft mode
  (no vis) for iteration; full vis only on release cooks; incremental vis is
  future work.
- **Coordinate scale/precision.** XBSP verts are s16 map-local; engine units
  and streaming segmentation need one written convention (per-region origins
  if P6(b) wins). Decide in P0 and encode in psx-bsp types.
- **Editor scaling.** Snapshot undo clones the whole document and picking is a
  linear scan; both sized for 32x32 rooms. Fine for first brush maps; plan
  per-op undo and a face BVH when brush counts grow. Do not build them first.
- **Two worlds during transition.** Grid + BSP both alive until P7 is real
  maintenance drag; keep the window short and resist improving the grid path
  meanwhile.
- **Parallel quake-psx rewrite.** `codex/all-rust-quake` is live uncommitted
  work in another session. We copy quake-formats, we do not touch that tree,
  and we reconcile the two Rust XBSP readers when that branch lands.

## Resolved questions (owner, 2026-08-10)

1. Existing projects are ignored by the migration: frozen on the grid, no
   port. The next game starts fresh on the brush world.
2. The 60 Hz fixed sim stays, permanently.
3. `psx-bsp` lives in engine/, built to be shared by the guest runtime, the
   editor cook and quake-psx alike; shared crates are the expected pattern
   for further migration work.
