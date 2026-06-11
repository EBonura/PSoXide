# Vertical Rooms: VR2 Implementation Plan

> **Superseded by [`floors-plan.md`](floors-plan.md).** This doc describes the
> older stacked-room-node approach; the agreed model is floors within a single
> room. Kept for the investigation context it references.

Builds the actual room-stacking feature on top of the VR1 groundwork (room
elevation plumbed to `LevelRoomRecord.origin_y`, committed in `ae8a5f6b`). Companion
to `docs/vertical-rooms-investigation.md`, which establishes that the runtime is
already capable (full 3D portal clip + per-sector `floor_above_room`/`floor_below_room`
consumption) and the gap is authoring + cook + the streaming-graph edge.

Three sub-steps. VR2c is gated on a decision that is yours, because it lands in the
room streaming graph your streaming/emu work is reshaping.

## Decision 1: room elevation becomes an explicit, editable field

Today VR1 derives a room's Y from the room node's `transform.translation[1]`, set
implicitly from the hit-point Y at placement. There is no control to set it and no
stable notion of a "level", which is why you cannot place a room on top of another.

Plan: give a Room node an explicit elevation (in sectors), editable in the editor via
(a) a numeric "Elevation" field when a Room node is selected, and (b) a vertical-move
gizmo (the Select tool already does vertical drag for faces; extend it to a whole Room
node). Make the cook read this explicit elevation as the source of truth, threading to
`LevelRoomRecord.origin_y` (the VR1 plumbing already exists), and drop VR1's
read-from-transform TODO. Snap to whole sectors.

Files: `psxed-ui` (room inspector field + gizmo), `psxed-project` (`WorldGrid.elevation`
exists; add the editor accessors), the cook (read explicit elevation). Editor-heavy, so
do this INLINE in the warm tree (agents stall on cold editor builds).

## Decision 2: vertical (floor/ceiling) portals for rendering visibility

Today the cook emits only wall portals (`vertical: false` / `kind 0`). The cooked
`LevelRoomPortalRecord` is already a full 3D quad with a 3D normal, and the visibility
clipper is full 3D, so the runtime needs nothing new here.

Plan: in the cook, detect where one room's ceiling plane and the room-above's floor
plane share a horizontal opening over matching X/Z (a "hole", authored as a missing
floor/ceiling face with a stacked room beyond), and emit a portal with a vertical normal
(`normal_y` dominant) and `vertical: true` / `kind: 1`.

Files: `psxed-project` (`portal_rooms.rs` `build_room_portals`/`plan_portal_rooms`,
`world_cook`), `psx-level` (record already supports it). Cook/runtime, so agent-friendly
(no egui), after VR2a.

## Decision 3 (YOURS): up/down adjacency and streaming

The investigation splits vertical into two axes:
- Traversal (fall through / climb): per-sector `floor_above_room`/`floor_below_room`,
  already parsed and consumed at `editor-playtest/src/main.rs:6235`. Populate the
  `GridFloorLink`s from authored stacking and cook them into the compact sector bytes
  44/46 (VR1 left this as a TODO; the runtime consumer already exists). Bounded.
- Streaming/visibility: the room adjacency graph is cardinal-only (N/E/S/W). Stacked
  rooms need an up/down edge so the streaming ring keeps the room above/below resident
  and the portal traversal recurses into it.

The streaming/visibility edge is the part that touches the graph your streaming work is
reshaping. The decision to lock with that effort BEFORE coding VR2c:

- Option A: add explicit `Up`/`Down` to the neighbour model, parallel to N/E/S/W.
- Option B: populate the reserved-but-empty `overlapped_rooms` from X/Z overlap and
  treat overlaps as streaming neighbours.
- Option C: reuse the `floor_above_room`/`floor_below_room` sector links as streaming
  edges too, one source of truth for both traversal and streaming.

Recommendation: Option C if the new streaming graph can derive edges from the sector
links (single source of truth), otherwise Option A. Either way this is the seam the
investigation flagged for coordination, so it should be agreed against the streaming
refactor's graph design before implementation, not invented by an agent.

Files: `psx-level` (graph/adjacency), the cook (graph build), the streaming ring
(`psx-engine` / `editor-playtest`). This overlaps the streaming work.

## Sequencing

1. VR2a, elevation authoring: INLINE (editor-heavy), after Step 3a integrates. No
   streaming-graph touch.
2. VR2b, vertical portals: AGENT (cook/runtime), after VR2a. No streaming-graph touch.
3. VR2c, adjacency + streaming: only after Decision 3 is locked with you. Highest risk;
   coordinate with the streaming refactor.

Runtime is already done (3D portal clip + floor-above/below consumption), so VR2 is
concentrated in authoring + cook + the one streaming-graph edge.
