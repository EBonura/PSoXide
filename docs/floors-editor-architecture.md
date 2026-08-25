# Floors in the editor: floor-awareness architecture

> RETIRED (2026-08-25): this document describes the deleted grid-world floor
> authoring system. BSP projects author vertical space directly as brushes.
> This file is retained only as historical design context.

Status: design proposal (no code yet). Written after a string of "the
entity shows on the wrong floor / selection hits the floor below"
regressions, each fixed by bolting a floor check onto one more code path.
This doc maps the current handling, names the failure pattern, and offers
three designs with tradeoffs so we can pick one before refactoring.

## The concept

A room can have stacked floors (`WorldGrid.floors_above`). Every placed
node (entity, prop, character, enemy, light, spawn) belongs to exactly
ONE floor, recorded on `SceneNode.floor` (usize, 0 = ground). The editor
shows one "active floor" (Sims-style: active floor is the working plane at
Y=0, floors below render descending, floors above hidden). Three
subsystems must agree on a node's floor and its world Y:

1. RENDER (editor preview) - draw each node once, on its floor, at that
   floor's elevation offset.
2. PICK / SELECTION / MOVE (psxed-ui viewport) - only the active floor's
   geometry and the nodes on it are selectable, at the drawn Y.
3. COOK (psxed-project) - bind each node to its floor's runtime room.

## Current handling (the evidence)

### Where `node.floor` is WRITTEN (4 sites, psxed-ui)
- `create_model_entity_at_room_hit` (~8423)
- `create_character_entity_at_room_hit` (~8465)
- placement helper (~8511)
- `run_paint_action` Place arm (~9176)
All do `node.floor = self.active_floor`. (Floating-geometry duplicate /
rotate / paste paths do NOT set it - gap.)

### Where floor is READ for RENDER (editor_preview.rs)
`node_enclosing_floor(scene, id)` walks ancestors to the room, max of
`node.floor`. Called as a filter `!= floor_index` in SIX separate walks:
`walk_entities` (2129), `walk_image_props` (2445), `walk_box_props`
(2559), `walk_model_instances` (2971), `walk_player_spawn_preview` (3156),
`walk_light_gizmos` (3730). Each ALSO independently applies `y_offset` to
its origin. `PreviewFloor { floor_index, y_offset }` is the per-floor
context threaded into all six.

### Where floor is READ for COOK (playtest.rs)
`chunk_for_node` binds a node to the chunk whose `floor_idx == node.floor`
(clamped). One site. Correct.

### Where floor is IGNORED - the bugs
- `collect_entity_bounds` (psxed-ui): filters by room + visibility, NOT
  by floor, and computes bounds at the node's own transform Y with NO
  active-floor offset. So object selection/move (a) shows handles for
  nodes on every floor, and (b) on an upper floor the bounds sit at the
  wrong Y vs the Sims-drawn position. This is the user's "object selection
  and movement not tied to the floor" report.
- `resolve_viewport_3d_pointer_target` / `pick_entity_bound` consume those
  unfiltered bounds, so entity picking inherits the same bug.
- node gizmo bounds (`node_gizmo_bounds_3d`) likewise read raw transform.

## The failure pattern

"Which floor a node is on, and what Y it draws at" is ONE fact, but it is
re-derived at ~9 call sites (6 render walks + cook + bounds + gizmo), and
adding a new render/pick path silently misses it. Every floors bug this
week (entity drawn twice, player on every floor, player floating, light
gizmo, selection-below) was one of these sites missing the check. Classic
shotgun surgery: the data model is fine (`node.floor` exists), the
dispatch is scattered.

## Designs

### Option A - Floor-resolved scene view (single source of truth)
One function, given `(scene, active_room, active_floor)`, produces the set
of (node, floor_index, y_offset, visible) the editor should act on this
frame - the SAME resolution for render AND pick/bounds. Render walks
iterate it; `collect_entity_bounds` / pick iterate it. Nobody re-derives
floor; selection becomes floor-tied for free because it reads the same
resolved list the renderer drew.
- Pros: true single chokepoint; impossible to add a path that forgets
  floor; render and pick can't disagree (the class of bug we keep hitting).
- Cons: medium refactor; the six walks currently take `&WorldGrid` +
  offset, would take a resolved-node iterator instead; need to keep the
  per-walk kind-matching. ~1 focused day.
- Data change: none.

### Option B - Floor as a first-class filter in pick + bounds only
Leave the six render walks as-is (they already work post-fixes). Just make
`collect_entity_bounds` + the gizmo bounds take `active_floor`, filter by
`node_enclosing_floor`, and apply the active-floor `y_offset` so handles
sit on the drawn node.
- Pros: smallest change; fixes the reported selection/move bug directly;
  low risk to the render side that now works.
- Cons: does NOT unify; the render walks still each re-derive floor, so the
  next new render/pick path can still miss it. Treats the symptom, not the
  pattern.
- Data change: none.

### Option C - Resolved-floor cache on the workspace
Compute once per frame a `HashMap<NodeId, ResolvedFloor { floor_index,
y_offset, visible }>` on `EditorWorkspace`, and have every consumer
(render via a passed reference, pick, bounds, gizmo, cook-preview) look up
that map instead of calling `node_enclosing_floor` + recomputing offset.
- Pros: single source like A, but consumers stay in place (less
  restructuring of the six walks); cheap lookups.
- Cons: a per-frame cache that must be invalidated on edits (staleness
  risk - the exact kind of bug that's hard to see); render is in the
  frontend crate, pick in psxed-ui, so the map must cross the crate
  boundary (it already passes active_floor across it, so feasible).
- Data change: none.

## Recommendation

Option A. The whole point is that render and pick stop disagreeing; a
shared resolution is the only design that makes that structurally true,
and the bugs we keep paying for are exactly render/pick disagreements. B
is the fast tactical fix if we want the selection bug gone today and defer
the unification; C trades restructuring for cache-invalidation risk, which
historically bites harder.

Suggested sequence if we pick A:
1. Define `ResolvedFloorNode` + a `resolve_floor_nodes(scene, active_room,
   active_floor)` producing the active floor + floors-below set with
   per-node floor_index/y_offset/visible.
2. Port the six render walks to consume it (behavior-preserving; verify by
   the existing demo11 dumps + editor_preview tests staying identical for
   floor 0).
3. Port `collect_entity_bounds` + gizmo bounds + pick to consume it -
   this is what newly makes selection/move floor-tied.
4. Regression: per-floor demo11 dumps (entity once, on its floor, at the
   right Y) + a psxed-ui test that selection on an upper floor returns a
   node on that floor, not the one below.

## Out of scope (already done / separate)
- `node.floor` data model + placement writes (done).
- Cook floor binding + hole-gated vertical portals + runtime edge-on
  admission (done; see floors-plan.md).
- Floating-geometry duplicate not setting `node.floor` (small gap; fold
  into whichever option as a placement-write fix).
