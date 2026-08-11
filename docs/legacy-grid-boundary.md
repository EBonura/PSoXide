# Legacy grid boundary decision artifact

Status: the boundary is now enforced in code; the retire/freeze decision is
still NOT made here. The owner accepts (or amends) the classification after
review, per `docs/quake-psoxide-convergence-handoff.md` section 15 ("remove
fallback paths only after the owner accepts the migration boundary"). No grid
code is deleted.

> **P3 execution update (2026-08-11).** Sections 0 and 10 are the current
> state. Sections 1 through 9 are the original 2026-08-10 audit, kept as the
> evidence they were written as; where they disagree with section 0, section 0
> wins. The two changes since that audit are `fcb99eeb` (the synthetic grid
> scaffold described in section 2.8 is gone) and the P3 work described below.

## 0. What changed in P3

**The discriminator is explicit.** `ProjectDocument::world_format` is a
persisted `ProjectWorldFormat` (`Bsp` | `LegacyGrid`)
(`editor/crates/psxed-project/src/document_types.rs`). The cook branches on it
instead of on brush presence. A document saved before the field existed
resolves exactly once on load, from the same presence rule it was cooked with,
and is persisted from then on; an already-stored value is never re-derived.
New documents are `Bsp`.

**Contradictions are named, not silently preferred.** Three hard cook errors
replace three silent behaviours (`playtest.rs::build_package`):

| Authored state | Old behaviour | New behaviour |
| --- | --- | --- |
| `LegacyGrid` + brushes | brushes silently won | error naming the brush count, focus on brush 0 |
| `Bsp` + no brushes | fell through to the grid pipeline | error: a BSP project has no other world source |
| `Bsp` + grid Sections | Sections silently discarded | error naming the first Section, focus on its node |

**The cook re-checks its own outputs.** After the walk, a BSP package is
verified to carry zero room chunks, room visibility rows, visibility cells,
PVS rows, surface caches, room portals, floor links, water cells and PSXW
world assets, exactly one room record with no world asset, and a `Pxbsp`
world-geometry variant. A grid package is verified not to carry a PXBSP
payload. This is the leak detector: the guards above make the leak impossible
today, but "impossible today" decays silently, and the failure it decays into
is a BSP level that streams rooms nobody authored.

**The guest asserts the same boundary.** `USES_PXBSP`
(`engine/examples/editor-playtest/src/main.rs`) is a build-time constant, so
`load_active_room_window` and `build_active_room` assert against it at zero
cost. A grid guest is byte-identical with and without the asserts; a BSP guest
drops the now-provably-dead grid window and room-build code (933,888 ->
919,552 bytes).

**Deliberate exceptions, per handoff section 0.10.** Both are documented
inline at the check site so a future audit does not read them as leaks:

- the singleton `PlaytestRoom` ("PXBSP World"), non-spatial metadata carrying
  gravity, camera, sky and fog, with `world_asset_index: None` and the
  `AssetId(65535)` sentinel in the manifest;
- the header-only `WORLD.PAK`, written from a deliberately empty world-pack
  order so the disc layout and loader contract stay stable.

**Reserved streaming seams, explicitly NOT removed.** Full PXBSP region
streaming is not implemented and is not on the critical path, but nothing here
forecloses it:

- `PxbspLumpKind::StreamingIndex` (`engine/crates/psx-bsp/src/pxbsp.rs`) is
  still a first-class lump, still written by `brush_pxbsp.rs` (currently
  empty) and still readable via `PxbspResidentMap::streaming_index()`;
- the `ReadAt` abstraction (`engine/crates/psx-bsp/src/lib.rs`) is untouched;
  both resident maps still load through it, so a pageable reader can replace
  `SliceReader` without touching call sites;
- the pack boundaries (WORLD.PAK / UI.PAK, their TOCs and start LBAs) and the
  residency budget checks in `playtest/manifest.rs` are untouched.

One seam did move: the guest assert in `build_active_room` makes the GRID
residency path (`ROOM_RESIDENCY` -> `ensure_room_resident`) unreachable from a
BSP build. The record and the machinery still exist and still compile. A
future pageable PXBSP must add its own residency entry point rather than
routing through the grid room window, and the assert says so on the first
attempt instead of silently building an empty window.

Audited tree: `/Users/ebonura/Desktop/repos/PSoXide-convergence`, branch
`codex/quake-psoxide-convergence`, HEAD `7c714194` (clean worktree at audit
time, 2026-08-10). All paths below are repo-relative. Line numbers are from
this HEAD and will drift.

Historical context: `docs/game-runtime-plan.md` (constraint 2, "Grid world,
not BSP") is the stale mandate this artifact supersedes for new projects.
`docs/quake-bsp-migration-plan.md` ("What gets replaced or retired") is the
plan this artifact verifies against the code as merged. `docs/world-grid-architecture.md`
and `docs/level-residency.md` document the grid architecture being bounded.

Classification vocabulary used throughout:

- **migrate**: the capability must exist in the BSP world; the grid code is
  the seed or the policy layer that carries over (possibly rewritten).
- **compatibility-only**: keep compiling and passing tests, frozen, for
  existing grid projects until the owner closes the window; no new features.
- **retire**: no BSP successor needed; delete when the compatibility window
  closes.

Nothing below is deleted or gated by this document.

---

## 1. The discriminator: how grid vs BSP was selected before P3

Superseded by section 0: the selection is now the persisted
`ProjectDocument::world_format`. What follows is the presence-based chain it
replaced, kept because it is the map of every layer the choice reaches.

There was no explicit "format" field. Selection was presence-based at every
layer, in one direction (brushes win):

**Authoring data.** A grid world is one or more scene nodes of
`NodeKind::Section { grid: WorldGrid }`
(`editor/crates/psxed-project/src/scene_types.rs:58-62`; serde aliases `Room`
and `Map` keep every historical save loading). A BSP world is the parallel
collection `Scene.brushes: Vec<Brush>`
(`editor/crates/psxed-project/src/scene_types.rs:684-696`;
`editor/crates/psxed-project/src/brush.rs:73-108` for `BrushFace`/`Brush`,
faces carry `material: Option<ResourceId>`). `WorldGrid` itself lives at
`editor/crates/psxed-project/src/world_types.rs:757`.

**Cook.** `build_package` computes
`let uses_pxbsp = !scene.brushes.is_empty();`
(`editor/crates/psxed-project/src/playtest.rs:261`). If any brush exists,
Section nodes are dropped before cooking
(`room_nodes.clear()`, `playtest.rs:267-271`, comment: "The PXBSP payload is
authoritative when brushes are present"). `world_geometry` starts as
`PlaytestWorldGeometry::Grid` (`playtest.rs:314`) and becomes
`PlaytestWorldGeometry::Pxbsp(...)` after `compile_brush_world` succeeds
(`playtest.rs:948-1001`). The enum is
`editor/crates/psxed-project/src/playtest/schema.rs:112-123`; its doc comment
records the design intent: "Gameplay tables remain common to both variants.
The distinction only chooses how the static world, visibility, and collision
are supplied."

**Generated manifest.** `render_manifest_source` emits a compile-time
constant into the guest:
`pub const PLAYTEST_USES_PXBSP: bool = false|true;` plus
`PXBSP_WORLD`/`PXBSP_MOVER_NODE_IDS`/`PXBSP_MOVER_MODEL_INDICES` statics
(`editor/crates/psxed-project/src/playtest/manifest.rs:507-540`). The cooked
`brush_world.pxbsp` file is written (Pxbsp) or removed (Grid) at
`manifest.rs:88-96`; the filename constant is
`editor/crates/psxed-project/src/brush_playtest.rs:3`. The PXBSP payload is
embedded into guest `.data` via `include_bytes!`
(`manifest.rs:177-186`, `write_aligned_asset_bytes_static`), so the BSP world
is fully resident; grid rooms stream from disc instead (section 2.7).

**Guest runtime.** One switch at gameplay init:
`self.bsp = if generated::PLAYTEST_USES_PXBSP { Some(BspRuntime::load_manifest()...) } else { None }`
(`engine/examples/editor-playtest/src/playtest_update.rs:157-164`). Every
downstream fork dispatches on `Option<BspRuntime>` (`self.bsp`), not on a
feature flag: player movement (`playtest_update.rs:458-520`), player body
dimensions (`playtest_runtime.rs:217-222`), camera
(`playtest_runtime.rs:680-686` vs the grid collision-room path below it),
NPC movement (`game_logic_runtime.rs:138-174`), static world render
(`playtest_scene.rs:277-286` BSP draw, `playtest_scene.rs:419` grid draw
gated on `self.bsp.is_none()`), doors
(`game_logic_runtime.rs:461,512`).

**What the discriminator is not.** `ProjectDocument.bsp_cook_mode`
(`editor/crates/psxed-project/src/document_types.rs:335-338`) selects BSP
Draft/Release cook quality only
(`editor/crates/psxed-project/src/brush_world.rs:44-66,480-482`); it does not
select grid vs BSP. Nothing prevents a project from holding both Sections and
brushes; the cook silently prefers brushes. The editor UI has zero
project-type gating today (the only `brushes.is_empty` checks in `psxed-ui`
are in tests: `editor/crates/psxed-ui/src/tests/project_workspace.rs:1127,1183`).

---

## 2. Grid-only paths, BSP equivalents, and per-path classification

### 2.1 Authoring (editor UI and tools)

| # | Grid-only path | BSP equivalent | Classification |
|---|---|---|---|
| A1 | `NodeKind::Section { grid: WorldGrid }` node type plus `Room`/`Map` serde aliases (`editor/crates/psxed-project/src/scene_types.rs:58-62`) | `Scene.brushes` (`scene_types.rs:684-696`) | compatibility-only: the aliases are what keep every saved grid project loading; freeze, do not extend |
| A2 | Section creation in the scene-tree "Add Child" menu: `scene_graph_addable_kinds()` offers "Section" with a fresh 3x3 `WorldGrid`, ungated (`editor/crates/psxed-ui/src/scene_tree.rs:1463-1475`) | Brush tool drag-create (`editor/crates/psxed-ui/src/workspace/tools.rs:168-230`, `BrushTool`) | retire (from the menu) at the freeze point: this is the door through which NEW grid authoring still enters any project, including BSP ones |
| A3 | Grid paint tools `ViewTool::PaintFloor/PaintWall/PaintCeiling` (`editor/crates/psxed-ui/src/lib.rs:2052-2076`; toolbar wiring `workspace/toolbars.rs:1049-1066`, enabled only when a Section is active; implementation `workspace/painting.rs`) | Brush create/face-drag/clip/hollow (`workspace/tools.rs:46-56` tool dispatch, `BrushTool` arm; `hollow_selected_brush` `tools.rs:228`) | compatibility-only: already self-gating (dead without a Section); freeze |
| A4 | `ViewTool::PaintMaterial` and `ViewTool::Erase` on grid faces (`editor/crates/psxed-ui/src/lib.rs:2064-2070`; `toolbars.rs:1030-1046`) | Brush faces carry `material: Option<ResourceId>` (`editor/crates/psxed-project/src/brush.rs:71-89`) edited via the brush inspector (`workspace/panels.rs:1700-1705`, `draw_brush_inspector`); full face-level texture controls are still open editor work (handoff section 13 item 13) | compatibility-only for the grid side; the brush face-material UX gap is migrate work tracked by the handoff, not by grid code |
| A5 | `ViewTool::Water` cell painting on a Room floor, `NodeKind::Water` (`editor/crates/psxed-ui/src/lib.rs:2065`; `editor/crates/psxed-project/src/scene_types.rs:63-68`) | closed BSP brushes expose **BSP contents** = Water/Slime/Lava in the Brush Inspector; the field persists on `Brush`, is visible in 2D/3D outlines, and cooks exact Quake contents codes | migrated for BSP authoring; retain the grid tool only for compatibility projects |
| A6 | Grid face/edge/vertex select-and-drag primitives and their inspectors (`editor/crates/psxed-ui/src/workspace/panels.rs:1706-1721`, `draw_face_inspector` etc.; move gestures tested in `editor/crates/psxed-ui/src/tests/world_editing.rs`) | Brush inspector (`panels.rs:1702-1704`); explicit brush edge/vertex editing is still open (handoff section 13 items 3-5) | compatibility-only (grid); the brush equivalents are migrate work already on the handoff list |
| A7 | Sector inspector panel (`editor/crates/psxed-ui/src/sector_inspector.rs:93` `draw_sector_inspector`, mounted at `workspace/panels.rs:2049`; edits `WorldGrid` sectors/floors directly) | brush inspector (see A4) | compatibility-only |
| A8 | Stacked-floor authoring: active-floor resolution (`editor/crates/psxed-project/src/floor_view.rs`), floor links (`psxed-ui/src/workspace/painting.rs:2` `GridFloorLink`), layer tests (`psxed-ui/src/tests/layer_authoring.rs`) | none needed as a concept: BSP brushes are 3D; vertical structure is just geometry | retire with the grid (no successor required) |
| A9 | Portal nodes on grid edges driving portal-room splits (`editor/crates/psxed-project/src/portal_rooms.rs`, header: "Runtime portal rooms are split only by authored Portal scene nodes"; paired-connection view `room_connections.rs`) | BSP portal generation is automatic from brush CSG (`editor/crates/psxed-project/src/brush_portal.rs`, "Portal generation and leaf classification for compiled brush BSPs") | compatibility-only: authored portals are a grid concept; BSP derives portals |
| A10 | Prefab stamping, grid-cell clipboard (`editor/crates/psxed-project/src/prefab.rs:1,30-52`, `PrefabCell`/`PrefabFloor`; UI in `psxed-ui/src/resource_browser.rs`) | none yet; the migration plan says the prefab CONCEPT survives as brush groups (`docs/quake-bsp-migration-plan.md`, "Editor grid tools" bullet) | migrate (concept), retire (grid-cell implementation) |
| A11 | World-node grid settings only meaningful for grid cooking: `sector_size`, `streaming`, `culling` sector radii (`editor/crates/psxed-project/src/scene_types.rs:21-44`) | BSP budgets/diagnostics come from the Draft/Release cook report (`editor/crates/psxed-project/src/playtest/budget.rs:133,216,232`) | compatibility-only; note the BSP cook still reads `world_sector_size_for_node` for its synthetic region (`playtest.rs:784-787`), so removal is sequenced after 2.8 |
| A12 | Grid-only viewport overlays and debug: ortho overlay grid readout (`editor/crates/psxed-ui/src/viewport2d.rs:1-40`), chunk/PVS-cell visualisations behind `world-grid-visible` (`engine/examples/editor-playtest/Cargo.toml:26,52`) | BSP draw-stat proof is the shared GPU counters (`engine/examples/editor-playtest/src/playtest_scene.rs:322-324` comment) | compatibility-only |
| A13 | New Project flow: BSP-first already shipped. `create_and_open_project` copies the brush template and opens with the Brush tool in the top ortho view (`editor/crates/psxed-ui/src/lib.rs:3016-3053`; template dir `editor/crates/psxed-project/src/lib.rs:315-323`) | n/a (this IS the BSP path) | done; listed so the artifact records that no New Project route creates grid content anymore |

### 2.2 Cook (psxed-project)

| # | Grid-only path | BSP equivalent | Classification |
|---|---|---|---|
| C1 | Grid world validation/cook: `world_cook.rs` (`cook_world_grid`, `encode_world_grid_psxw`, `CookedWorldGrid`, exports at `editor/crates/psxed-project/src/lib.rs:347-351`) producing `.psxw` blobs | `compile_brush_world` (`editor/crates/psxed-project/src/brush_world.rs:249`) producing PXBSP via `brush_compile.rs` (CSG/BSP), `brush_portal.rs` (portals/leaf classification), `brush_light.rs` (Release baked lighting), `brush_collision_hulls.rs`, `brush_pack.rs`, `brush_pxbsp.rs` | compatibility-only, with the 2.8 caveat: the BSP cook itself still calls `cook_world_grid` for its synthetic region (`playtest.rs:788-795`), so C1 cannot be deleted before that scaffold is removed |
| C2 | Portal-room planning, splitting one authored Section into runtime rooms (`editor/crates/psxed-project/src/portal_rooms.rs`, `plan_portal_rooms` called at `playtest.rs:353`) | BSP leaf/portal generation (`brush_portal.rs`) | compatibility-only |
| C3 | Grid PVS cook: visibility cells and portal-expanded PVS (`editor/crates/psxed-project/src/playtest/cook_visibility.rs`, `append_room_visibility`) | conservative brush PVS cooked into PXBSP (commit `f93a4908`, `brush_portal.rs` components; consumed by `engine/crates/psx-bsp/src/render.rs:198-245` leaf-visibility bitset) | compatibility-only; note the BSP cook also calls `append_room_visibility` once for the synthetic region (`playtest.rs:822-830`) |
| C4 | Cached-room surface cook (`editor/crates/psxed-project/src/playtest/cook_world.rs`; `PlaytestRoomSurfaceCache`/`PlaytestCachedRoom*` rows built in `playtest.rs:306-310`) | PXBSP precompiled faces/lightmap-free vertex lighting (`brush_world.rs:480-482`) | compatibility-only |
| C5 | Water-cell cook from grid floors (`editor/crates/psxed-project/src/playtest.rs:1114-1181`; requires `grid.world_cell_to_array`, sector floors) | brush CSG, collision-hull classification and PXBSP packing preserve Water/Slime/Lava terminals; runtime samples the static point hull at feet/torso/head and applies nonblocking slowdown plus hazard cadence | migrated for BSP; retire the cell mechanism only after the compatibility window |
| C6 | Streamed room chunk emission: `.psxc` stream chunks + world pack order (`editor/crates/psxed-project/src/playtest/manifest.rs:64-96,145,154`; layout `manifest.rs:384-396`) | none needed: PXBSP is resident via `include_bytes!` (`manifest.rs:514-520`); BSP asset streaming for models/textures rides the shared UI.PAK path unchanged | compatibility-only until a future PXBSP streaming lump exists (`docs/quake-bsp-migration-plan.md` design decision 1 mentions a phase-S streaming index lump; nothing in code today) |
| C7 | Grid budget estimation (`editor/crates/psxed-project/src/world_types.rs:10` `WorldGridBudget`, `WorldGridFootprint` `scene_grid_types.rs:2538`; over-budget warnings `playtest.rs:354-360`) | BSP authored estimate + exact cooked budget report (`editor/crates/psxed-project/src/playtest/budget.rs:133` `estimate_playtest_budgets`, `:216` `cooked_playtest_budgets`, `:232` PXBSP branch) | compatibility-only |
| C8 | Grid-only generator/tool bins: `gen_stress_map.rs`, `gen_prefab_gallery.rs`, `gen_prefab_kit.rs`, `prefab_sheet.rs` (all under `editor/crates/psxed-project/src/bin/`, headers confirm grid/sector content) | `gen_brush_first_playable.rs` (same dir) generates the tracked BSP acceptance map; `cook_playtest.rs` and `miniaturise_project.rs` are format-agnostic (`cook_playtest.rs:73,103` handles both variants) | compatibility-only (stress/prefab tooling still backs perf campaigns on grid projects); regenerate equivalents on BSP when needed |

### 2.3 Project data (ProjectDocument)

| # | Grid-only path | BSP equivalent | Classification |
|---|---|---|---|
| P1 | `NodeKind::Section`/`Water`/`Portal` node kinds and `WorldGrid` payload inside `scenes` (`scene_types.rs`, `world_types.rs:757`) | `Scene.brushes` | compatibility-only (load forever within the window; never write new ones after freeze) |
| P2 | Grid-only World-node settings (A11) and `runtime_*` grid render knobs (`document_types.rs:339-350`: `runtime_depth_sort_mode`, `runtime_texture_split_mode`, `runtime_room_draw_order_mode`, `runtime_texture_split_max_edge`, all documented against cooked rooms) | `bsp_cook_mode` (`document_types.rs:335-338`) | compatibility-only |
| P3 | `ProjectDocument::starter()` deserializes the embedded GRID default project (`document_types.rs:412-424`; `DEFAULT_PROJECT_RON` = `editor/projects/default/project.ron`, `lib.rs:64`); used by the starter character catalogue sync (`editor/crates/psxed-ui/src/starter_catalogue.rs:112`) and as the fixture for most cook tests (section 6) | New Project uses `new_project_template_dir()` = `editor/projects/brush-first-playable` (`lib.rs:315-323`) | compatibility-only; `default_project_dir` doc comment already states the intended status: "retained as the compatibility/fallback project while existing grid content remains supported" (`lib.rs:308-313`). If grid retires fully, the starter-catalogue resource source must move off the grid project |

### 2.4 Runtime: static world render (guest)

| # | Grid-only path | BSP equivalent | Classification |
|---|---|---|---|
| R1 | Grid room drawing in psx-engine: `world_render.rs` + `world_render/{room_draw,cache_build,indexed_cache}.rs` (header: "Drawing helpers for cooked grid worlds"); `RuntimeRoom::render()` surface (`engine/crates/psx-engine/src/world.rs:268-...`) | `engine/crates/psx-bsp/src/render.rs` (XBSP world rendering through the shared classic-affine path, leaf-PVS `mark_visible_faces` `render.rs:244`, visibility bitset `render.rs:198-201,827`) | compatibility-only |
| R2 | Example render loop: grid active-room draw order + surface caches + per-room lighting/fog, gated `if self.bsp.is_none()` (`engine/examples/editor-playtest/src/playtest_scene.rs:356-431`; glue modules `active_room_cache.rs`, `room_lighting_runtime.rs`) | `bsp.draw(...)` into the same OT/arena (`playtest_scene.rs:277-286`; `bsp_runtime.rs:355-...`) | compatibility-only; note the room-window loop still RUNS in BSP mode for actors/instances over the synthetic room (see 2.8), only the static-surface draw is skipped |
| R3 | Grid PVS cell selection + portal visibility runtime (`engine/crates/psx-game-runtime/src/world_cells.rs` `VisibleCellSelector`, `world_visibility.rs`, `room_visibility.rs`; example glue `visible_cell_runtime.rs`, `visibility_runtime.rs`, `active_room_visibility.rs`) | BSP leaf PVS inside `psx-bsp::render` (R1) | compatibility-only |
| R4 | `world-grid-visible` cargo feature and grid visibility stats (`engine/examples/editor-playtest/Cargo.toml:26,52`; `playtest_scene.rs:311-335`) | shared GPU/draw counters | compatibility-only, retire with R2 |

### 2.5 Collision and movement

| # | Grid-only path | BSP equivalent | Classification |
|---|---|---|---|
| M1 | Grid collision data model: `.psxw` parse + `RoomCollision` + `RuntimeCollisionRoom`/`CompactCollisionRoom` (`engine/crates/psx-engine/src/world.rs:268,768-773`), corner-interpolated floor heights (`engine/crates/psx-engine/src/floor_sample.rs`) | caller-owned hull tracing: `psx-bsp/src/collision.rs` (`trace_into`, 64-entry `TraceScratch`, contents codes), `collision_provider.rs` (`PxbspCollisionProvider`), transformed movers `mover.rs` | compatibility-only |
| M2 | Character motor grid backend: `update_vblanks_with_collision` -> `GridCharacterCollision` ("grid collision queries are infallible", `engine/crates/psx-engine/src/character_motor.rs:713-725`), `CharacterCollision{room,rooms,blockers,aabb_blockers}` (`character_motor.rs:221-260`), grid `commit_body_step` (`character_motor.rs:1574`) | trace backend on the same motor: `update_vblanks_with_trace_provider` (`character_motor.rs:733-749`), `commit_body_step_with_trace_provider` (`character_motor.rs:1634`); shared world-agnostic contract `engine/crates/psx-engine/src/collision_query.rs:1-40` (deliberately keeps psx-engine independent of psx-bsp) | migrate DONE for the mechanism (one motor, two backends); grid backend itself compatibility-only |
| M3 | Player movement grid branch in the example: multi-room collision gather + box/arch prop AABBs (`engine/examples/editor-playtest/src/playtest_update.rs:476-520`) | BSP branch `bsp.update_motor(...)` with `CharacterBlockerTraceProvider` composition (`playtest_update.rs:458-475`; `bsp_runtime.rs:273-297`) | compatibility-only; PARITY GAP: the BSP branch composes only actor cylinders; BoxProp/ArchProp AABBs are NOT in the BSP provider (grid branch collects them at `playtest_update.rs:501-515`, BSP branch has no equivalent; the handoff logs this risk in section 7.5) |
| M4 | Third-person camera grid backend: `update_vblanks_with_collision_rooms` (`engine/crates/psx-engine/src/third_person_camera.rs:329`) and the example's cached blocking-room gather (`playtest_runtime.rs:687-onwards`) | `update_vblanks_with_trace_provider` (`third_person_camera.rs:360`), used via `bsp.update_camera` with the point hull (`bsp_runtime.rs:327-353`) | migrate DONE (mechanism), grid backend compatibility-only |
| M5 | Dynamic actor blockers | shared, not grid-only: `CharacterBlockerTraceProvider` (`character_motor.rs:119-152`) wraps any provider; grid path passes cylinders directly in `CharacterCollision.blockers` | done; listed to record that blockers are already format-neutral |
| M6 | Hull selection for bodies: grid uses per-model radii/heights (`game_logic_runtime.rs:101-120`) | BSP hard-codes `BSP_PLAYER_HULL_INDEX = 1` for player AND every NPC (`engine/examples/editor-playtest/src/bsp_runtime.rs:30-31,289,317`; acknowledged seam comment at `playtest_runtime.rs:217-222`) | migrate: authored/cooked body-size to hull mapping is required for parity (handoff section 14 item 10) |

### 2.6 Navigation / AI

| # | Grid-only path | BSP equivalent | Classification |
|---|---|---|---|
| N1 | Room-indexed entity activation: `LevelGameEntityRecord.room: RoomIndex`, active-room gate `room_is_active(record.room, input.active_rooms)` (`engine/crates/psx-game-runtime/src/entities.rs:34,739-741,814`; helper defined at `entities.rs:1383`); positions are room-local (`entities.rs:144-159,411`) | none yet: the migration plan's decision 2 (world-space positions, leaf/cluster PVS activation, "area" grouping) is unimplemented; BSP projects work because the synthetic single room 0 makes every entity always active (see 2.8) | migrate: this is the largest un-started parity item; grid rooms currently define the AI activation and coordinate model for BOTH worlds |
| N2 | NPC movement backend: grid branch resolves the entity's own active room + prop AABBs + `psx_engine::character_motor::commit_body_step` (`engine/examples/editor-playtest/src/game_logic_runtime.rs:145-174`; "a grid room whose collision is not resident refuses movement", header lines 5-10) | BSP branch: same cascade through `bsp.commit_body_step` (`game_logic_runtime.rs:138-143`; `bsp_runtime.rs:301-325`), merged in the dynamic-blockers checkpoint (`420e9deb`) | migrate DONE (both worlds feed one `GameEntityMover` trait, `entities.rs:148-176`); grid branch compatibility-only; same prop-AABB gap as M3 applies to the BSP branch |
| N3 | Anchor-based patrol (spawn anchor to patrol anchor, `entities.rs:427-428,45`) | shared; no waypoint/pathfinding graph exists in either world | done/neutral; listed because any future BSP navigation (plan decision 2) replaces the room gate, not the patrol logic |

### 2.7 Streaming and residency

| # | Grid-only path | BSP equivalent | Classification |
|---|---|---|---|
| S1 | Streamed-room scheduler + slot residency (`engine/crates/psx-game-runtime/src/room_streaming.rs` `RoomStreamScheduler`; example glue `active_room_streaming.rs`) | none needed today: the PXBSP world is baked resident (section 1); shared packet-arena words were reserved for BSP streams (`engine/crates/psx-engine/src/render.rs`, commit `7a09c1e9`) as forward provisioning | compatibility-only |
| S2 | WORLD.PAK CD read job + TOC (`engine/crates/psx-game-runtime/src/cd_stream.rs:1,84,254-275`; cook layout `manifest.rs:384-396`) | shared for UI.PAK/model assets; only the room-chunk consumer is grid-specific | compatibility-only (the crate also serves UI.PAK, which BSP projects still use) |
| S3 | Active-room window state machine (`engine/crates/psx-game-runtime/src/room_window.rs` `RoomWindow`; example `active_rooms.rs`) | none; BSP has no window (resident world), but the window still runs over the synthetic room in BSP mode (2.8) | compatibility-only, entangled with 2.8 |
| S4 | Disc layout flags for room packs: `--world-pack-rooms-dir`, `--world-pack-order-file` (`tools/mkisopsx/src/main.rs:43-61`) | same flags still consumed for BSP discs; the pack is just tiny (synthetic room is zero bytes, `playtest.rs:797-803`) | compatibility-only |
| S5 | Room asset residency records: `RoomResidencyRecord`, `LevelChunkRecord`, `LevelWorldPackEntryRecord` (`engine/crates/psx-level/src/lib.rs:873,901,1355`) | PXBSP resident map (`engine/crates/psx-bsp/src/pxbsp_resident.rs` `PxbspResidentMap`, `resident.rs` validated storage) | compatibility-only |

### 2.8 The synthetic-grid scaffold inside the BSP cook (important coupling)

The BSP cook does not bypass the grid pipeline; it feeds it a stub. When
`uses_pxbsp`, `build_package` cooks an EMPTY 1x1 `WorldGrid` through
`cook_world_grid`, pushes a zero-byte `room_000.psxw` asset, one synthetic
`AuthoredRoomChunk`, one synthetic `PlaytestRoom` named "PXBSP World", and one
`append_room_visibility` call
(`editor/crates/psxed-project/src/playtest.rs:779-830,855-939`; comment: "One
synthetic region preserves the existing room-indexed actor tables until real
PXBSP leaf/PVS residency replaces them; its tiny PSXW is metadata only and is
never the selected world renderer/collider").

Consequences for the boundary decision:

- every "room-indexed" gameplay table (entities, lights, water, props, model
  instances, spawn, camera/sky/fog per room) keeps working in BSP projects by
  collapsing to room 0;
- the grid cook (`cook_world_grid`), room-chunk plumbing, room-window runtime,
  and room-visibility records CANNOT be deleted while this scaffold exists,
  even if every project migrates to BSP;
- retiring the grid therefore has a hard prerequisite: implement the migration
  plan's decision 2 (world-space entities + leaf/area activation) or an
  equivalent, then remove the synthetic region.

This is the main "BSP silently rides on grid" discovery of the audit.

### 2.9 Formats and shared records (psx-level)

Grid-specific record families in `engine/crates/psx-level/src/lib.rs`:
`LevelRoomRecord` (:707), `LevelRoomPortalRecord` (:784),
`LevelWaterCellRecord` (:808), `LevelChunkRecord` (:873),
`LevelWorldPackEntryRecord` (:901), `LevelRoomVisibilityRecord` (:1070),
`LevelVisibilityPvsRecord` (:1093), `LevelVisibilityCellRecord` (:1126),
`LevelRoomSurfaceCacheRecord`/`LevelCachedRoom*` (:1157-1224),
`RoomResidencyRecord` (:1355). Everything else in the crate (assets, sky,
far vista, camera, materials, models, logic, game entities, weapons,
equipment, combat capsules) is world-agnostic and consumed identically by BSP
projects. Classification: grid families compatibility-only; keep the shared
families untouched. `RoomIndex` itself (:72) stays until N1 lands.

Stale doc note: `engine/crates/psx-engine/src/world.rs:242,265` still
reference a `GridRoom` type that no longer exists anywhere in the tree
(removed in the cleanup/dedup wave); fix the comments when the boundary is
executed.

---

## 3. Classification summary

Counts over the enumeration above (excluding "done/neutral" rows A13, M5, N3):

- **retire** (2): A2 (Section entry in the Add menu, at the freeze point),
  A8 (stacked-floor concept, no successor needed).
- **migrate** (6): A5/C5 (water), A10 (prefab concept as brush groups),
  M6 (authored hull selection), N1 (world-space entities + leaf/area
  activation), plus the two "mechanism migrations" already DONE and verified
  (M2/M4 motor and camera backends, N2 NPC cascade).
- **compatibility-only** (the rest, roughly 24 paths): the entire grid
  authoring/cook/render/collision/streaming stack stays frozen and compiling
  until the owner closes the window, because (a) two tracked grid projects
  depend on it (section 4) and (b) the BSP cook itself still leans on it
  (section 2.8).

Blockers to eventual retirement, in dependency order:

1. N1: entity/table de-room-ification (largest; unimplemented).
2. 2.8: remove the synthetic grid region once N1 lands.
3. A5/C5: water parity or explicit feature drop (owner call).
4. M3/N2 prop-AABB composition and M6 hull selection (BSP parity gaps that
   make grid the only fully-featured collision world today).
5. Section 4 projects migrated or explicitly parked.
6. Compatibility tests of section 6.3 in place BEFORE any deletion.

---

## 4. Tracked projects and samples that still require grid behavior

Checked by node-kind census of each tracked `project.ron` (`kind: Room` is the
serde alias of `Section`; `brushes:` presence checked separately):

- `editor/projects/default/project.ron`: 1 `Room` node, 0 brushes. GRID.
  Tracked, whitelisted in `editor/projects/.gitignore`. Triple duty: the
  embedded starter (`ProjectDocument::starter()`), the starter character
  catalogue source (`starter_catalogue.rs:112`), and the fixture root for
  most cook tests (section 6.1).
- `editor/samples/cortex_v1/project.ron`: 1 `Room` node, 0 brushes. GRID.
  This is the tracked Cortex combat sample that just received the migrated
  weapon content (commits `61deb273`, merge `7c714194`): sockets, swords,
  attack capsules, hurtboxes. The flagship combat authoring evidence
  therefore currently REQUIRES the grid runtime; the handoff's Level-B "BSP
  combat fixture" (section 14 item 2) does not exist yet.
- `editor/projects/brush-first-playable/project.ron`: brushes present. BSP.
  Tracked with its replay tape `walk-through-door.pxitape.csv`.
- Untracked but load-bearing for owner workflows: `Makefile` targets
  `profile-demo3*`/`profile-demo7-camera-sweep` cook `projects/demo_03` /
  `projects/demo_07` (`Makefile:722-782`), `WARP_PROBE_PROJECT ?=
  projects/cortex_v3` (`Makefile:347`); all grid projects that exist only in
  the owner's working checkouts. The perf-campaign tooling is therefore
  grid-dependent.
- `engine/examples/editor-playtest/generated/level_manifest.rs` (tracked
  placeholder) currently has `PLAYTEST_USES_PXBSP: bool = false` (line 39),
  i.e. the committed guest builds as a grid project until someone cooks.
- Other `engine/examples/*` (game-pong, showcase-*, hardware-tests etc.) use
  psx-engine app/render primitives, not the grid world API; they do not
  constrain the boundary.

## 5. Proposed freeze point for new grid authoring (proposal, not decision)

Current state: New Project is already BSP-only (A13), but grid authoring can
still start anywhere because (a) "Section" is offered ungated in the Add-Child
menu (A2), (b) grid paint tools activate the moment any Section exists (A3),
and (c) a project may hold both Sections and brushes with silent brush
priority at cook (section 1).

Proposed freeze, in three steps the owner can accept independently:

1. **Authoring freeze (cheap, immediate).** Remove/gate "Section" in
   `scene_graph_addable_kinds()` (`psxed-ui/src/scene_tree.rs:1463-1475`):
   offer it only when the scene already contains a Section (legacy project
   maintenance) and never when `!scene.brushes.is_empty()`. Surface a
   one-line "legacy grid project" banner in the workspace when Sections
   exist. Grid paint tools stay functional inside legacy projects.
2. **Cook freeze (guard, not removal).** Turn the silent brush-wins rule
   into an explicit validation: a project containing BOTH Sections with
   geometry and brushes gets a cook warning naming the ignored Sections
   (`playtest.rs:267-271` is the site). Grid-only projects keep cooking
   green exactly as today.
3. **Feature freeze (policy).** After 1 and 2, no new capabilities land in
   the grid stack (sections 2.1-2.7); grid changes are restricted to bug
   fixes that keep section 6 tests green. New gameplay features must be
   authored/tested on BSP fixtures.

Retirement (deleting compatibility-only rows) is explicitly OUT of this
freeze and waits on the section 3 blocker list plus owner acceptance.

## 6. Test evidence today, and what the compatibility window is missing

### 6.1 Existing tests that prove grid projects load/cook

- Load: `cortex_project_deserializes_authored_enemy_combat_profile`
  (`editor/crates/psxed-project/src/tests.rs:206-243`) parses the tracked
  grid Cortex sample. `ProjectDocument::starter()` parsing is guarded by the
  `embedded_default_project_ron_deserializes` invariant referenced at
  `document_types.rs:418-420`. Legacy-name loading is guaranteed by the
  `Section`/`Room`/`Map` alias chain (`scene_types.rs:56-58`).
- Cook: the bulk of `editor/crates/psxed-project` coverage cooks GRID
  fixtures. `playtest/tests.rs` helpers default to `ProjectDocument::starter()`
  with `starter_project_root() = default_project_dir()` (`playtest/tests.rs:7-9`).
  Representative grid-specific tests in
  `playtest/tests/rooms_visibility.rs` (14 tests):
  `floors_cook_to_stacked_rooms_with_auto_links`,
  `package_resolves_vertical_floor_links_to_runtime_rooms`,
  `layered_pvs_can_leave_a_room_and_reenter_behind_a_shallow_recess`,
  `room_vertical_placement_flows_from_transform_into_origin_y`,
  `oversized_authored_room_fails_without_manual_split`,
  `portal_room_cook_emits_directed_room_portals`,
  `manual_portal_rooms_emit_warm_residency_hints`,
  `generated_room_cache_counts_match_runtime_builder`. Plus
  `world_cook.rs` (30 tests), `portal_rooms.rs` (7),
  `playtest/tests/water.rs` (2), and the wider playtest suites
  (`assets_validation` 21, `models_entities` 24, `player_character` 16,
  `logic_entities` 12, `lights_components` 14, `ui_options` 13) which all run
  through the grid world path.
- Editor UI: `psxed-ui/src/tests/world_editing.rs` (47 tests, grid
  face/edge/vertex/gizmo editing), `placement_painting.rs` (35),
  `layer_authoring.rs` (5), `scene_tree_selection.rs` (40),
  `project_workspace.rs` (59) exercise grid authoring through the headless
  harness.
- Engine runtime: `psx-engine` `character_motor.rs` tests (54, grid rooms via
  `RuntimeRoom` fixtures, `character_motor.rs:2456-2458`),
  `third_person_camera.rs` (26, including grid collision-room cases),
  `world_render/tests.rs` (36). `psx-game-runtime`: `entities.rs` (24),
  `room_streaming.rs` (4), `water.rs` (2).
- BSP contrast (what the new world has): `new_project_template_is_a_buildable_pxbsp_level`
  (`psxed-project/src/lib.rs:233-252`),
  `first_playable_brush_world_uses_the_normal_gameplay_package`,
  `project_cook_choice_reaches_the_normal_package_and_compiler`,
  `first_playable_fixture_opens_a_player_hull_route_through_its_door`
  (`brush_playtest.rs:19,61,98`), `brush_world.rs` determinism tests
  (Draft==Draft, Release==Release, `brush_world.rs:1056-1059`),
  `brush_compile.rs` (16), `brush_pxbsp.rs` (6), psx-bsp crate tests
  (collision 16, provider 3, mover 6, pxbsp 11, resident map 9, render 9),
  psxed-ui `brush_tools.rs` (13) and `orthographic_brush.rs` (10), plus the
  tracked replay tape for the BSP first-playable.

### 6.2 What proves grid still PLAYS

Nothing tracked and automated. The grid guest runtime is exercised by:
`character_motor`/`camera`/`world_render`/`room_streaming` unit tests (host),
and by the owner's untracked tapes/projects (Makefile demo targets,
`~/Library/Application Support/com.psoxide.PSoXide/editor/playtest_tapes/`).
The only tracked end-to-end replay fixture in the repo is the BSP one
(`editor/projects/brush-first-playable/walk-through-door.pxitape.csv`). The
committed placeholder manifest is grid (`generated/level_manifest.rs:39`), so
plain `make build-editor-playtest` still builds a grid guest, but no CI-style
gate replays it.

### 6.3 Missing compatibility-window tests (recommended before any freeze is
enforced, mandatory before any retirement)

1. Tracked grid end-to-end replay: cook `editor/projects/default` (or the
   cortex_v1 sample), build the guest, replay a short tracked `.pxitape.csv`
   headless, assert route ticks + final position + display hash, mirroring
   the BSP `walk-through-door` regression. Today grid has zero replay
   fixtures in-tree.
2. Grid cook determinism test (byte-identical repeat cook), mirroring
   `brush_world.rs:1056-1059`; no grid equivalent exists.
3. Old-save compatibility corpus: minimal tracked RON fixtures exercising the
   `Map` and `Room` aliases and pre-`floors_above`/pre-`options` defaults, so
   serde-default drift is caught (aliases exist, but no test loads a
   deliberately old document).
4. Mixed-content guard test: Sections + brushes in one project cooks Pxbsp
   and (post freeze step 2) reports the ignored Sections.
5. Cortex grid sample full-package cook test: `build_package` over
   `editor/samples/cortex_v1` (the current test only deserializes it; the
   weapon-content agent reported cook evidence, but nothing in-tree cooks
   the tracked sample on every run).
6. A "grid project unaffected by BSP changes" hash gate: cook default project,
   assert `PLAYTEST_USES_PXBSP = false` and no `brush_world.pxbsp` emitted
   (`manifest.rs:88-96` removal branch is currently untested).

## 7. Known consumers of grid outside PSoXide

Checked every sibling game repo's own `Cargo.toml` (not their vendored
PSoXide copies) and their `game/src` imports:

- Depend on `engine/crates/psx-engine`: psxcel, gh-psx, hl-psx, nitroxide
  (e.g. `gh-psx/game/Cargo.toml:44`). Their imports are the App/Scene loop,
  input, primitives, OT and classic-affine helpers only (`psx_engine::{button,
  App, Config, Ctx, Scene}`, `PrimitiveArena`, `OtFrame`, ...). A grep for
  `RuntimeRoom|RoomCollision|WorldGrid|GridRoom|CharacterMotor` over all six
  game repos plus nitroxide and both quake worktrees returns zero hits.
- Quake convergence (`quake-psx-convergence`): `psx-bsp` + `psx-engine`
  (`crates/quake-core/Cargo.toml:16-17`); psx-engine usage is `div_q12_i32`
  and the classic-affine render API (`game/src/renderer.rs:7-14`). No grid.
- alttp-psx, voxide, pico8-psx, oot-psx: SDK crates only, no engine crates.

Conclusion: the grid world API surface has NO consumers outside this
repository. Its complete consumer set is: the editor (authoring + cook), the
`editor-playtest` example guest, `psx-game-runtime`, and the two tracked grid
projects. The boundary decision is therefore entirely intra-repo; no
downstream pin or migration wave is required to retire grid, whenever that is
accepted.

## 8. Surprises and risks surfaced by this audit

1. **BSP cooking rides on the grid cook** (section 2.8): synthetic 1x1
   `WorldGrid`, zero-byte `.psxw`, synthetic room/chunk/visibility rows. Grid
   cook code is a live dependency of every BSP build.
2. **The BSP combat sample is a grid project**: `editor/samples/cortex_v1`
   (freshly armed with weapon content) has one Room node and no brushes. The
   combat migration evidence currently depends on the compatibility path.
3. **Grid authoring is completely ungated**: "Section" is offered in every
   project's Add menu (`scene_tree.rs:1463-1475`), and a BSP project that
   gains a Section keeps cooking with the Section silently ignored.
4. **BSP parity gaps that block "grid is redundant" claims**: BoxProp/ArchProp
   AABBs absent from the BSP trace stack (M3/N2), hard-coded NPC hull index 1
   (M6), no water (A5/C5), room-indexed entity activation collapsed to one
   synthetic room, i.e. no real spatial AI gating or PVS-driven entity
   activation in BSP (N1).
5. **No tracked grid replay fixture** while BSP has one; the compatibility
   window currently rests on host unit tests plus untracked owner content
   (6.2, 6.3).
6. **Committed placeholder guest manifest is grid** (`generated/level_manifest.rs:39`),
   so the default `make build-editor-playtest` exercises the grid runtime; a
   grid freeze does not remove that build path until the placeholder flips.
7. Doc rot markers to fix at execution time: `docs/game-runtime-plan.md`
   constraint 2 (superseded, keep as history with a banner), stale `GridRoom`
   doc references (`psx-engine/src/world.rs:242,265`).

## 9. Decision status

This artifact enumerates, maps, and classifies; it decides nothing. The owner
accepts or amends: (a) the per-path classifications (section 2/3), (b) the
freeze point (section 5), (c) the required test additions (section 6.3), and
(d) the retirement prerequisites (section 3). Until then, per the handoff:
grid behavior stays unchanged, and the current dynamic-blocker-era rule holds
("grid projects retain their existing grid/AABB path for compatibility",
`engine/examples/editor-playtest/src/game_logic_runtime.rs:5-10`).

---

## 10. Current classification and BSP reachability (P3, 2026-08-11)

The section 2 tables stand as the per-path evidence. This is the same
enumeration re-stated against the tree as it is now, with the column the
original audit could not have: whether a BSP project can still reach the path.

Reachability vocabulary:

- **unreachable (guarded)**: a BSP project cannot enter it, and trying is a
  named cook error or a guest assert added in P3.
- **unreachable (empty input)**: no guard names it, but its only input is a
  table a BSP cook provably leaves empty; the cook's output check fails closed
  if that ever stops being true.
- **shared**: not grid-specific; both worlds use it.
- **reserved**: currently unused, deliberately kept for a future
  implementation (see the streaming seams in section 0).

### 10.1 Authoring (editor)

| Path | Class | BSP reachability |
| --- | --- | --- |
| `NodeKind::Section { grid: WorldGrid }` + `Room`/`Map` serde aliases | compatibility-only | authored Sections load, but a BSP cook refuses them by name |
| "Section" in the scene-tree Add menu (`scene_tree.rs`) | retire at the freeze point | still offered in every project; the cook is now the backstop that names the mistake |
| Grid paint tools (`PaintFloor/Wall/Ceiling`, `PaintMaterial`, `Erase`) | compatibility-only | self-gating; need an active Section |
| `ViewTool::Water` cell painting, `NodeKind::WaterVolume` on a Section floor | migrated (BSP uses brush contents) | unreachable (empty input): the water walk needs a Section ancestor |
| Grid face/edge/vertex inspectors, sector inspector | compatibility-only | need a Section |
| Stacked-floor authoring (`floor_view.rs`, `GridFloorLink`) | retire (no successor needed) | needs a Section |
| Authored Portal nodes on grid edges (`portal_rooms.rs`) | compatibility-only | BSP derives portals from CSG |
| Prefab stamping / grid-cell clipboard (`prefab.rs`) | migrate (concept), retire (grid-cell implementation) | grid-cell payloads only |
| World-node grid settings (`sector_size`, streaming, culling radii) | compatibility-only | `sector_size` still feeds the BSP metadata room; the rest is inert |
| Room Rings topology overlay (`build_debug_topology`) | compatibility-only | **unreachable (guarded)**: returns empty for a BSP project |
| New Project flow | done (BSP-only) | this is the BSP path |

### 10.2 Cook

| Path | Class | BSP reachability |
| --- | --- | --- |
| `cook_world_grid` / `CookedWorldGrid` / `encode_world_grid_psxw` (`world_cook.rs`) | compatibility-only | **unreachable (guarded)**: only called from the Section walk |
| `extract_portal_room_grid` -> `WorldGrid` construction | compatibility-only | unreachable (guarded) |
| `plan_portal_rooms` / portal-room splitting (`portal_rooms.rs`) | compatibility-only | unreachable (guarded) |
| `PlaytestAssetKind::RoomWorld` (`.psxw`) emission | compatibility-only | unreachable (guarded); asserted absent in the output check |
| `AuthoredRoomChunk` + `build_playtest_chunks` + `.psxc` stream chunks | compatibility-only | unreachable (guarded); explicitly `Vec::new()` for BSP; asserted absent |
| `append_room_visibility`, visibility cells, PVS rows (`cook_visibility.rs`) | compatibility-only | unreachable (guarded); asserted absent |
| `rebuild_portal_connected_visibility_pvs` | compatibility-only | unreachable (empty input); self-guards on empty cells/rows |
| `append_room_surface_cache` + cached-room tables | compatibility-only | unreachable (guarded); asserted absent |
| Floor links, floor-stack/terrace portals, cross-Section portals, adjacency portals | compatibility-only | unreachable (empty input): all keyed on the empty chunk map; asserted absent |
| Water-cell cook from grid floors | migrated (BSP liquids are brush contents) | unreachable (empty input); asserted absent |
| Room-local placement resolution (`world_cell_to_array`, `node_chunk_local_position`) | compatibility-only | explicitly branched: BSP places in world space against room 0 |
| Grid budget estimation (`WorldGridBudget`, `WorldGridFootprint`) | compatibility-only | grid packages only |
| Grid generator bins (`gen_stress_map`, `gen_prefab_gallery`, `gen_prefab_kit`, `prefab_sheet`) | compatibility-only | they author grid projects on purpose |
| `WORLD.PAK` header + empty world-pack order | **compatibility-only, deliberate exception** | reached by every BSP cook by design (handoff 0.10) |
| Singleton `PlaytestRoom` metadata record | **not grid state, deliberate exception** | reached by every BSP cook by design (handoff 0.10) |
| `ROOM_RESIDENCY` record for the metadata room | compatibility-only / reserved | still emitted; its consumer is unreachable in a BSP guest |

### 10.3 Runtime (guest)

| Path | Class | BSP reachability |
| --- | --- | --- |
| `load_active_room_window` (`active_rooms.rs`) | compatibility-only | **unreachable (guarded)**: asserts on `USES_PXBSP` |
| `build_active_room` -> `RuntimeRoom` / `RoomCollision` / `ActiveRuntimeRoom` / residency | compatibility-only | **unreachable (guarded)**: asserts on `USES_PXBSP` |
| Grid static-surface draw (`world_render.rs`, `playtest_scene.rs`) | compatibility-only | gated by `self.bsp.is_none()` |
| Grid PVS cell selection (`world_cells.rs`, `world_visibility.rs`, `room_visibility.rs`) | compatibility-only | unreachable (empty input): `chunked_level()` is false with no `ROOM_CHUNKS` |
| `RoomStreamScheduler`, `RoomMaterialPool`, room-window/visibility storage | compatibility-only / reserved | zero-initialised at boot in every build; never populated in BSP |
| Grid collision (`RuntimeCollisionRoom`, `floor_sample.rs`) | compatibility-only | only reachable through `build_active_room` |
| `CharacterMotor` grid backend (`update_vblanks_with_collision`) | compatibility-only | BSP uses the trace-provider backend on the same motor |
| Third-person camera grid backend | compatibility-only | BSP uses `update_camera` with the point hull |
| Room-indexed entity activation (`RoomIndex` gate) | superseded by leaf PVS | BSP uses `set_spatial_active_mask` from the PXBSP PVS row |
| `RoomIndex` type and shared `psx-level` record families | shared | `RoomIndex` still types the metadata room |
| `psx-bsp` `StreamingIndex` lump, `ReadAt`, pack TOCs | **reserved** | present, empty, intentionally kept |

### 10.4 What still blocks retirement

Unchanged from section 3, minus the item P3 and `fcb99eeb` closed:

1. ~~synthetic grid region inside the BSP cook~~ (closed by `fcb99eeb`).
2. ~~no explicit discriminator~~ (closed by P3).
3. Two tracked grid projects still depend on the grid stack:
   `editor/projects/default` (also the embedded starter and the fixture root
   for most cook tests) and `editor/samples/cortex_v1` (the combat authoring
   sample). Until the starter catalogue and the cook fixtures move off the
   grid project, deleting grid code breaks the editor's own defaults.
4. The owner's untracked perf-campaign projects and Makefile targets
   (`profile-demo3*`, `profile-demo7-camera-sweep`, the warp probe) are grid.
5. Still no tracked grid replay fixture: the compatibility window rests on
   host unit tests. Section 6.3's list is unchanged and unstarted.
