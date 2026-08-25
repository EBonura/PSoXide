# Floors: vertical levels within a room

> RETIRED (2026-08-25): this plan describes the deleted grid-world floor
> authoring system. BSP projects author vertical space directly with brushes;
> no stacked-floor editor workflow remains.

Supersedes the earlier VR2 "stack separate room nodes + manual vertical
portals" framing (that plan doc has been removed; see git history). A project authors a single Room (one
grid plus manually placed portals that the cook splits into streaming regions), so
the right vertical primitive is FLOORS (levels) within that one room, navigated by an
up/down button, not stacked room nodes.

## Model (agreed with the user)

- A Room is a base floor (today's floor + ceiling grid = floor 0) plus a stack of
  floors above it.
- Each floor is its own free grid: any footprint, any floor/ceiling heights.
- Auto-stack: floor N's base sits just above floor N-1's ceiling, so floors never
  overlap in Y.
- Authoring: an up/down button switches the active floor; the paint/edit tools target
  the active floor's plane. Clicking "up" moves you to the plane above.
- Ghosting: while editing floor N, floor N-1 is drawn faintly underneath for alignment
  (lining up stairs / holes).

## Data

`WorldGrid` gains `floors_above: Vec<WorldGrid>` (`#[serde(default)]`, empty). Floor 0 is
the base grid itself; floor `i` is `floors_above[i - 1]`. Floors live on the grid, NOT on
the `NodeKind::Room` variant, on purpose: the grid's cells are public fields (`sectors`,
`width`, `depth`) read directly at ~168 `NodeKind::Room { grid }` sites, so a Room-variant
field (or a multi-floor restructure of those public fields) would ripple through all of
them. With floors on `WorldGrid`, those 168 sites keep operating on floor 0 (the base
grid) untouched; only the editor's active-floor access (paint, hit-test, render) routes to
`grid.floor(i)` / `grid.floor_mut(i)`.

Accessors on `WorldGrid`: `floor_count()`, `floor(i)`, `floor_mut(i)`, and `push_floor()`
(adds an empty floor auto-stacked just above the top floor's ceiling). The editor holds
the active-floor index as view state (like the active UI scene index), not in grid data.

## Slices

- Slice 1 (authoring, INLINE in the editor): add `floors_above` to `WorldGrid` + floor
  accessors; an active-floor index plus up/down buttons in the Room workspace; paint/edit
  target the active floor; the viewport ghosts the floor below. The cook still emits only
  floor 0 (upper floors are author-only until Slice 2). This delivers the edit-floor-0,
  click-up, edit-floor-1 loop.
- Slice 2 (playable, cook + runtime): cook every floor at its auto elevation
  (`origin_y`); auto-wire vertical connections from holes (a missing floor face becomes
  a floor/ceiling portal for seeing down + the `floor_above`/`floor_below` links for
  walking, both already runtime-supported); add the streaming edge between vertically
  overlapping regions (the old Decision 3, now derived rather than hand-wired).

## Reuses

VR1 `origin_y` (per-floor elevation), the 3D portal record + clipper (floor/ceiling
portals), and the runtime `floor_above_room`/`floor_below_room` consumption
(`editor-playtest/src/main.rs:6235`). The new work is the floors dimension, the up/down
editor with ghosting, and the cook auto-wiring.

## RUNTIME: can't cross to the upper floor (round 6) - DIAGNOSED via user tape

Replayed the user's recorded tape (`<config>/editor/playtest_tapes/demo11.pxtape`, PXITAPE1)
headless via `--input-tape` + `--counter-log`. Result: active room mask stays 1 (room 0)
the whole climb - the room switch into room 1 NEVER fires. Player ends visually atop the
stairs but still "in" room 0, blocked from entering room 1.

Cooked links are fine: ALL 36 room-0 cells have `above=Some(1)`. So the bug is the SWITCH,
two coordinated problems in editor-playtest/main.rs:

1. `current_floor_link_switch_target` up-branch requires `sector.has_ceiling() && player_y >
   ceiling_top + EPS`. Climbing stairs tops out AT the upper floor's elevation (3584), resting
   ON the step, never strictly above the room-0 ceiling; and an open stairwell sector has no
   ceiling at all, so `has_ceiling()` is false and the branch never fires. Wrong model for
   stairs. Correct: switch up to `above_room` when `player_y >= ROOMS[above_room].origin_y -
   EPS` over a linked cell (you've climbed to the upper floor's level).
2. `local_to_global_room_point` / `global_to_local_room_point` translate X/Z only, NOT Y. So
   even if the switch fired, the player's Y isn't rebased into the new room's local frame
   (room geometry is floor-local; origin_y is applied as a render/collision offset). The
   player would land origin_y units above room 1's floor and fall. Y rebasing must subtract
   `origin_y` on enter (and the reciprocal on the global mapping).

Both are runtime changes in the shared main.rs (co-mingled with the other lane's HUD work).
The Y-rebase touches ALL room transitions (horizontal portals too), so it needs care +
regression on the existing flat-seam crossing.

## RUNTIME: upper room vanishes mid-climb (round 5) - FIXED

After the offset_y + stair-step fixes, climbing demo11's stairs works and the upper room
shows - but it DISAPPEARS as the eye nears/crosses the portal plane on the top steps.
Cause: `portal_front_faces_camera` for the up-portal (normal [0,-1,0], plane Y=elev)
back-faces once `camera.y >= plane`, so crossing the portal Y while climbing culls the
upper room from the visibility BFS (which roots at the still-current lower room). FIX
(portal_visibility.rs): `camera_in_vertical_portal_footprint` - when the camera's XZ is
inside the hole's rectangle, a vertical portal stays admitted across the plane regardless
of front-face (you're physically in the opening). Outside the footprint the front-face test
still applies, so the sealed-slab reject (`vertical_portal_backface_still_rejected`, camera
above plane but off the hole) is preserved. Red→green test
`vertical_portal_stays_visible_in_hole_across_plane`. psx-level 39 green.

## RUNTIME: upper room drawn but at Y=0 (round 4) - ROOT CAUSE FOUND

demo11 overlay showed `vis 2` and counter-log `drawn=3` (both rooms resident AND drawn),
yet the upper room is invisible in-game. NOT a streaming/visibility/draw-gate bug - those
are all correct now. ROOT CAUSE: the runtime never applies a room's `origin_y` at render
time. `ActiveRuntimeRoom` has `offset_x`/`offset_z` but NO `offset_y`;
`local_to_global_room_point` (main.rs ~9547) passes `point.y` through unchanged;
`camera_for_room` (~9569) subtracts `offset_x`/`offset_z` from the camera but not Y. So the
cooked `origin_y=3584` is ignored and the upper room renders stacked ON TOP of floor 0 at
Y=0 (overlapping/z-fighting), instead of a storey up. The whole vertical separation
collapses at draw.

FIX (editor-playtest, the runtime - shared with the streaming lane): add `offset_y` to
`ActiveRuntimeRoom`, compute it in `with_current_room_offsets` as
`record.origin_y - current_record.origin_y` (origin_y is ABSOLUTE, floors don't shift with
the current room, unlike x/z which are relative), and subtract it in `camera_for_room` so
the room's geometry lands at its real elevation relative to the camera. Mirror x/z exactly
at the ~15 offset_x/_z consumer sites. Cook + visibility already correct; this is purely
the render transform.

## SIMS-STYLE EDITOR FLOOR VIEW + selection fix (user issues round 3)

User on floor 2/3 still saw the player duplicated AND selection landing on the floor
below. Directive: "Sims style, when you select a floor it visualises ALL floors below."

ROOT CAUSE of the selection bug: render moved the active floor UP to its real elevation
(`y_offset = elevation - base`), but ALL pick paths test at Y=0 (`pick_3d_world` ->
`pick_3d_world_on_room_plane(plane_y=0)`, and `pick_face_with_hit` ray-tests the active
floor's floor-LOCAL faces ~Y=0). So a click on the visually-high active floor resolved at
Y=0 where lower geometry sat -> "selects the one below". Render and pick disagreed on Y.

FIX (`preview_room_grids`): Sims model. The ACTIVE floor is the working plane drawn at
`y_offset = 0`; floors BELOW render descending (`y_offset = floor.elevation -
active.elevation`, negative); floors ABOVE are hidden (loop `0..=active`). Anchoring the
active floor at Y=0 makes it coincide with where every pick tests, so selection/paint hit
the floor you're editing. Entities/models/lights already filter by `node_enclosing_floor ==
floor_index` and offset by `y_offset`, so each renders once on its own floor at the right
height. Test `preview_room_grids_shows_active_floor_and_below_only` (active@0, below
negative, above hidden). Tests: frontend editor_preview 39, psxed-ui 195.

NOTE on the user's persistent duplication: with the entity floor-filter committed, headless
dumps show the player once (on floor 0 only). If the running editor still shows duplicates
it's a STALE BUILD -- rebuild the frontend (`cargo build -p frontend --release`) and
relaunch.

## AUTOPORTAL / vertical visibility (user issues round 2)

Two issues from playtest screenshots:
1. EDITOR: player drawn twice (once per floor). CAUSE: render-all-floors made the
   room-scoped entity/model/light/prop walks run once per floor entry. FIX: `PreviewFloor`
   gained `floor_index`; `node_enclosing_floor()` filters each walk to nodes on that floor,
   and `y_offset` lifts them to the floor's elevation. Each entity now renders once, on its
   own floor. Verified by per-floor dumps (player appears only on floor 0). Also closes the
   upper-floor-entity render gap.
2. RUNTIME: upper room never visible. The user's model: a vertical link is an "autoportal"
   you can see through ONLY where there's a real gap (no floor above AND no ceiling below).
   THREE bugs found, all fixed:
   a. Portals emitted at EVERY shared cell. FIX: `auto_wire_floor_stack_portals` now gates
      on an actual hole (upper floor face absent AND lower ceiling absent). demo11: 72 -> 22
      portals (11 real openings x2 reciprocal). Sealed cells get none.
   b. Vertical (kind=1) horizontal portal quads go edge-on under a level camera and were
      frustum-rejected, so the linked room never entered the visibility BFS. FIX
      (`portal_visibility.rs`): an edge-on vertical portal that front-faces the camera is
      admitted by INHERITING the parent frustum (still gated by front-face + depth + the
      hole), so it acts as an implicit neighbour through the opening. Tests
      `vertical_portal_admits_room_when_edge_on` / `vertical_portal_backface_still_rejected`.
   c. THE ACTUAL BLOCKER (only found by in-engine repro): vertical portals were appended to
      the global table AFTER the cook loop set per-room portal_first/count, so rooms'
      portal ranges didn't include them (room 0 had portal_count=0) and the BFS never
      scanned them. FIX: `regroup_room_portals` sorts the portal table by source_room and
      rebuilds every room's [portal_first, portal_count) over BOTH horizontal + vertical
      portals. demo11: room 0 -> [0,11), room 1 -> [11,22).
   RESULT (in-engine, counter-log): before, visible mask = 1 (room 0 only) for all frames;
   after, mask = 3 (rooms 0+1) every frame -- the upper room streams in and renders through
   the hole. Lesson: the unit test passed because it hand-set portal_count; only the real
   cook+run exposed bug (c).

## demo11 IN-ENGINE PLAYTEST (verification milestone)

Full pipeline run: `make cook-playtest PROJECT=projects/demo11/project.ron` →
`make build-editor-playtest` → mkisopsx pack → `frontend launch --embedded-playtest`.
Cooked manifest: ROOMS[0] origin_y=0, ROOMS[1] origin_y=3584; PLAYER_SPAWN room=0
x=5184 y=0 z=5056; ROOM_PORTALS = 72 vertical (kind=1, ±Y normals). Headless HW dump
(`/tmp/demo11_spawn_hw.ppm`): player (Crimson Cross Knight) stands on the stone GROUND
floor with brick walls + HUD bars - bug 1 (wrong spawn floor) CONFIRMED FIXED in-engine,
scene renders with no crash. Hold-forward sweep + `--counter-log`: room mask stays `1`
(room 0) for all 458 post-boot frames; room 1 never streams; cam_y stays ground-level.

WHY YOU CAN'T REACH FLOOR 1 (not a bug): demo11 is authored SEALED. Floor 0 has 25
ceiling faces; floor 1 has 25 floor faces and 0 ceilings - a solid slab between the two,
no traversable hole. Runtime floor-switch (`current_floor_link_switch_target`,
main.rs:6231) crosses DOWN only when `!sector.has_floor()` (a hole) and the player falls
below it, UP only when above the ceiling. With a sealed slab neither can trigger, so the
player is correctly locked to floor 0. The cook/link/portal machinery is verified correct;
the MAP just has no stairs/opening. To actually walk between floors, author a hole (erase a
floor-1 floor cell AND the floor-0 ceiling cell beneath it) and/or stairs.

## demo11 reproduction (cook ground truth) - OPEN BUGS (historical; all fixed)

Loaded `editor/projects/demo11/project.ron` and cooked it via `build_package` in a
diagnostic test (`diag_demo11_cook`, `#[ignore]`, run with `--ignored --nocapture`).
Structure: ONE Room node "Demo7 Map" (id 18), sector_size 1792, 2 floors (floor 0
elevation 0, floor 1 elevation 3584 = 2 sectors), each 36 populated cells. Player Entity
(id 28) at translation.y = 2.892857 sectors (= 5184 engine units). Cook output:
- rooms: room[0] origin_y=0, room[1] origin_y=3584. Good.
- room_portals: **0**. Floor links exist (72) but NO vertical portal quads.
- SPAWN room=1 (origin_y 3584) - WRONG, player authored on floor 0.

Three root causes, all from the floors design conflict (Slice 1 renders floors IN PLACE at
base Y; Slice 2 cook + entities use REAL elevation - the editor and cook disagree on where
a floor is):

1. PLAYER SPAWNS ON WRONG FLOOR. `chunk_for_node` "closest elevation" picks floor 1:
   |5184−3584|=1600 < |5184−0|=5184. But 2.89 sectors is a model-height offset standing
   ON floor 0 (visual_offset.y=640 on top), not a floor selector. Even a half-open band
   rule [elev_N, elev_{N+1}) misassigns: 5184 ≥ 3584 → floor 1. The real problem: the
   player Y was authored against the in-place (Y=0) floor-0 render, so its absolute Y is
   meaningless as a floor key. My elevation-binding (the just-"DONE" item below) BROKE this
   existing project. Likely correct fix: bind by the floor the node was authored on
   (explicit), not by inferring from a Y that the in-place render made unreliable.

2. NO VERTICAL PORTALS. `room_portals` only come from `plan.portals` (manual seams within
   one grid, playtest.rs:259). The auto floor-stack produces `room_floor_links` but no
   `PlaytestRoomPortal` quads → portal view empty, no visual/render connection between
   floors. Slice 2 wired links but never generated the horizontal portal quads at the
   shared floor/ceiling boundary.

3. "GOING UP MOVES THE PLAYER MODEL." Editor renders the active floor's GEOMETRY in place
   at base Y (`preview_room_grids`), but `walk_entities` draws entities at their true
   `translation.y` (`node_room_local_origin`, raw transform). Switching floors swaps the
   geometry under a fixed-Y entity, so the player appears to shift relative to the floor.

Fix direction (NEEDS USER CALL - reverses Slice 1's "render in place"): make the editor
render each floor at its REAL elevation (stack visibly), so authored Y, cook elevation, and
render agree. Then: stamp/track the floor a node belongs to explicitly; generate vertical
portal quads from the floor links; entities move with their floor because the floor is
where its elevation says.

### FIX PROGRESS (full-fix path, user-approved)

- BUG 1 (wrong spawn floor): FIXED + verified. Y is a placement DEFAULT (2.892857 sectors,
  identical across demo7/demo10/default/demo11), so a node's floor can't be inferred from
  Y. Added explicit `SceneNode::floor: usize` (#[serde(default)]=0=ground); placement sets
  it = active_floor; `chunk_for_node` binds by it. REVERTED the unsound Y-elevation
  placement stamp and the "closest elevation" binding from the prior turn. demo11 now cooks
  SPAWN room=0 (origin_y=0). Test: `entities_bind_to_their_explicit_floor` (two markers at
  identical transforms, only `floor` differs).
- BUG 2 (no vertical portals): FIXED + verified. `auto_wire_floor_stack_portals` emits a
  reciprocal up(-Y)/down(+Y) `PlaytestRoomPortal` (kind=1) per shared cell at the boundary
  elevation, merged into `room_portals`. demo11: 72 portals (36 cells × 2). Test extended
  `floors_cook_to_stacked_rooms_with_auto_links` (vertical, reciprocal, planar-in-Y).
  FOLLOW-UP: per-cell quads (36 pairs); could merge contiguous cells into one quad like the
  wall-portal path for perf / cleaner portal view.
- BUG 3 (editor render: player marker floats / "moves" on floor switch): FIXED + visually
  verified (headless `dump-editor-preview`). Two changes in editor_preview.rs: (a)
  `walk_entities` now uses `floor_anchored_node_room_local_origin` so the marker sits on the
  floor surface like the model (was raw translation.y=5184, floating); (b) `preview_room_grids`
  now returns `PreviewFloor { room, grid, y_offset, active }` and emits EVERY floor of the
  active room at its real elevation (`grid.elevation - base.elevation`), threaded into
  `walk_room` via off3/off4 height offsets - the whole stack renders at true Y, so switching
  the active floor no longer shifts the world. Edit overlays (selection/hover/paint) gate on
  the active floor (`active || y_offset==0`) so they stay aligned to floor-local geometry.
  Removed the now-dead floor-below ghost (`walk_room_ghost`/`GHOST_FLOOR_SHADE`) - the real
  floor below renders instead. Added a `--active-floor` flag to the dump CLI for inspection.
  Verified: demo11 dumps show floor 0 and floor 1 stacked with a real vertical gap.
  REMAINING (not exercised by demo11, all entities on floor 0): entities/props/lights on
  UPPER floors still render at floor-local Y (not +y_offset) - the walks position via the
  per-floor grid but don't add the elevation. Thread y_offset through walk_entities/
  image/box props/model_instances/light_gizmos when stacked-floor entities are authored.
- KNOWN EDGE: cook's `floor_anchored_node_chunk_local_position` samples the BASE grid's
  surface for all entities; an entity bound to floor N>0 would get floor 0's surface Y.
  Fine for demo11 (player on floor 0). Fix when stacked-floor entities are exercised.

## Status

- Per-floor entities/lights (post-selection-fix): DONE (but see demo11 bug 1 - the
  elevation-inference binding is unreliable for nodes authored against the in-place render;
  revisit alongside the render-at-real-elevation fix). Entity/light chunk binding was
  XZ-only, so on stacked floors a node bound to floor 0's chunk. Root cause: entities
  carried no floor, the editor renders the active floor in place at base Y, so a floor-1
  placement stored `translation[1] ≈ 0`, identical to floor 0. Three coordinated parts:
  (1) placement stamps the active floor's elevation into `translation[1]`
  (`placement_translation_for_room_hit` + new `active_floor_elevation_sectors`), so a node
  records its floor (and cooks at the right height for free); (2) `chunk_for_node` picks
  the candidate chunk whose `floor_idx` elevation is closest to the node's Y (entities AND
  light source-binding route through it); (3) `expand_lights_across_chunks` gates the
  cross-chunk spill by Y (`CookedRoomBakeInput` gained `origin_y`) so a light stays on its
  level. Red→green test `entities_bind_to_the_floor_matching_their_y` (two markers, one per
  floor; XZ-only binding yields `[0,0]`, Y-aware binds each to its own `origin_y`). Tests:
  psxed-project 258, psxed-ui 195; frontend builds. KNOWN LIMIT: floating-geometry
  duplicate placement (`floating_origin_from_3d_hover`) is XZ-cell based and doesn't carry
  floor Y; only the main click-placement path stamps elevation.
- Selection fix (post-Slice-2): DONE. The 3D ray-pick and every selection / inspector
  reader destructured `NodeKind::Room { grid }` directly (always floor 0), so on an upper
  floor you saw floor N but selected floor 0's faces. Routed all of them through
  `room_grid_view` (active floor): `pick_face_with_hit`, `face_world_corners`,
  `triangle_world_corners`, `horizontal_triangle_ref_at_hit`, `horizontal_face_split_and_drop`,
  `face_material`, `triangle_material`, `triangle_parent_values`, `all_faces_in_room`,
  `existing_horizontal_rect_faces`, `existing_wall_span_faces`, `world_to_sector`,
  `ensure_cell_in_grid`, `apply_paint`, `sector_bounds_2d`, `selection_bounds_3d`, and the
  2D box-select (`select_sectors_in_screen_rect`, per-floor via `grid.floor(idx)`).
  Red→green test `face_corner_reads_address_the_active_floor` (distinct per-floor geometry:
  floor 0 has a floor face, floor 1 a wall) covers `face_world_corners` + `all_faces_in_room`.
  195 psxed-ui tests pass; frontend builds.
- Slice 1 (authoring + visual): DONE. `floors_above` + accessors on `WorldGrid`
  (`psxed-project/src/lib.rs`); `push_floor` auto-stacks one storey above the top floor
  and inherits the base's fog/ambient/atmosphere. Editor: `active_floor` view-state +
  Up/Down stepper in the viewport toolbar; `room_grid_view` (reads) and a new
  `room_floor_grid_mut` (writes) route EVERY floor-level edit to the active floor (paint,
  vertex/edge heights, materials, autotile, face/triangle/sector/vertex delete, sector
  inspector, auto-grow `extend`, `resize`, drag-move, rotate, duplicate/paste). The
  `extend`/`resize`/`draw_sector_inspector`/`remove_primitive_faces_from_project` free fns
  gained an `active_floor` param. Render: `emu/.../editor_preview.rs::preview_room_grids`
  routes the active room to its active floor (in place); `walk_room_ghost` draws the floor
  below dropped one storey, flat-dim, for alignment. Tests green: psxed-project 256,
  psxed-ui 194, frontend editor_preview 38.
- Slice 2 (playable cook): DONE (cook-side). The per-room cook loop now wraps a
  `for floor_idx in 0..base_grid.floor_count()` so each floor cooks its own streaming
  chunks at `room_origin_y_base + (floor.elevation - base.elevation)`; `AuthoredRoomChunk`
  carries `floor_idx`; `auto_wire_floor_stack_links` links consecutive floors by chunk
  `room_index` (bypassing the `(node id, cell)` resolver, which can't disambiguate floors)
  and is merged into `room_floor_links`. 256 psxed-project tests pass; psxed-ui builds.
  Playtest to confirm the runtime renders/walks the stack. Design below.

## Slice 2 implementation plan (cook)

Cook is `psxed-project/src/playtest.rs`, pass 1 = `for room_node in &room_nodes` (~204).
Per room node today: destructure base `grid`; `room_origin_y` from
`transform.translation[1] * sector_size` (~226); `plan_portal_rooms(scene, node.id, grid)`
(~232); `for portal_room in plan.rooms` cooks each chunk (`extract_portal_room_grid` ->
`cook_world_grid` -> push `rooms`/`room_meta`/`room_origins` with `origin_y`, materials,
`collect_room_lights`, `collect_pending_floor_links`); then per-node entities; then
post-loop `resolve_room_floor_links(&pending, &room_chunks_by_node)` (~1208) keyed by
`(target NodeId, world_cell)`.

Changes:

1. Wrap ONLY the chunk-cook (streaming + `plan_portal_rooms` + the `for portal_room`
   loop, roughly the `let streaming` .. close-of-`for portal_room`) in
   `for floor_idx in 0..base_grid.floor_count()`, binding `let grid =
   base_grid.floor(floor_idx).unwrap()` and `room_origin_y = base_origin_y +
   (grid.elevation - base_grid.elevation)`. Keep per-node entities OUTSIDE the floor loop
   (entities belong to the room once). Destructure as `base_grid`, shadow `grid` inside.
2. Add `floor_idx: usize` to `AuthoredRoomChunk` and set it, so chunks know their floor.
3. After the floor loop (per node), auto-wire vertical links: for consecutive floors
   (N-1, N), for each XZ cell present in both, emit a `ResolvedRoomFloorLink` directly by
   chunk `room_index` (floor N's sector floor_below -> floor N-1 chunk; floor N-1
   floor_above -> floor N chunk). Do NOT route through `resolve_floor_link_target` /
   `(NodeId, cell)` resolution: that CANNOT disambiguate two floors of the same room node
   (same node id, same cell). Build a per-floor `cell -> room_index` map from the
   `AuthoredRoomChunk`s (now carrying `floor_idx`) and link directly.
4. Vertical portals between floors follow from the links (runtime already consumes
   `origin_y` + floor links + vertical portal records). No runtime change expected.

Known gotchas / unverified:
- Lights (`collect_room_lights`) and entities (`resolve_entity_room`) resolve by XZ, so on
  stacked floors they may bind to the wrong floor's chunk. Acceptable for a first pass
  (entities/lights effectively floor-0); refine by elevation later.
- Whether the runtime cleanly renders one room node -> several stacked chunks is only
  verifiable by playtest; that path is adjacent to the streaming refactor. Cook-side
  changes are isolated to `psxed-project`.

DONE as described above. Remaining follow-ups: (a) a dedicated cook test asserting N
floors -> N stacked rooms at distinct `origin_y` with auto-wired links; (b) lights
(`collect_room_lights`) and entities (`resolve_entity_room`) still resolve by XZ, so on
stacked floors they bind to whichever chunk matches in plan order (effectively floor 0),
refine by elevation later; (c) playtest one room with two floors to confirm the runtime
renders/walks the stack.
