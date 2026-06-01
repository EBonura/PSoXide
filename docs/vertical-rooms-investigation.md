# Vertical Rooms Investigation

Status: investigation findings, not a committed plan. Captures whether the engine supports
Tomb-Raider-style vertically stacked rooms and what it would take. To be tackled separately
from the UI/game-states work, and after (or folded into) the in-flight streaming refactor.
Authored 2026-05-31.

## Question

The engine is modelled on Tomb Raider, which supports cleanly vertically stacked rooms
(separate rooms at the same X/Z footprint but different heights, connected by floor/ceiling
portals). Do we do the same? How does our portal system differ? And why can the UI not author
portals between vertically stacked rooms?

## Headline

Within a single room we already have full TR-style vertical relief. Across rooms (separate
stacked floors) we do not. But the runtime is the part that is already built: both the
rendering-visibility path and the gameplay-traversal path exist and are wired. The missing
work is concentrated in room placement, authoring, and cook, not in the hard 3D math, and our
portal math/format is not actually different from TR's.

## The two TR vertical mechanisms

Tomb Raider does vertical via two separate systems. The engine has scaffolding for both, split
cleanly into a visibility axis and a traversal axis.

| Axis | Purpose | TR mechanism | Our status |
| --- | --- | --- | --- |
| Visibility | see into the room above/below through a hole | floor/ceiling portals (3D quads) | Runtime ready, auth/cook missing |
| Traversal | fall through / climb to the room above/below | floor-data room-above/below links | Runtime consumes it, but it is never fed |

### Visibility axis (floor/ceiling portals)

- Runtime ready. `LevelRoomPortalRecord` (`engine/crates/psx-level/src/lib.rs:688`) is a full
  3D quad: `vertex_x/y/z[4]` (`[BL, BR, TR, TL]`) plus a 3D `normal_x/y/z`, directed
  source->destination, with a `kind: u8` documented as "`0` wall, `1` vertical placeholder".
- The visibility traversal is a proper 3D recursive portal-frustum clipper, not a 2D top-down
  test. `portal_visibility.rs` clips the portal polygon against near + left/right/bottom/top
  planes (`clip_portal_polygon_against_plane` x5, `:791`-`:831`), and `PortalFrustum` carries
  vertical tangents (`min_y_tan_q12`/`max_y_tan_q12`), not just horizontal. The module comment
  (`:6`) states reachability is based on the projected portal rectangle, "not because the
  room's top-down bounds". A Y-dominant portal normal should already clip correctly.
- Auth/cook missing. The cook hardcodes `vertical: false` ("Demo7 currently emits wall
  portals", `editor/crates/psxed-project/src/portal_rooms.rs:107`; also `:602`, `:614`).
  `RoomConnectionKind { Wall, FloorCeiling }` exists (`room_connections.rs:29`-`:30`), but
  `classify_edge` (`:260`) only ever returns `Wall`/`Unknown` for cardinal/diagonal seams, and
  `FloorCeiling` is detected only from an imported TR `PortalGeometry.normal` (`classify_geometry`
  `:271`), never from an authored edge. `kind: 1` is never emitted from real authoring.

### Traversal axis (room-above/below links)

- Plumbed across all three layers. `GridSector` has `floor_above`/`floor_below`
  `GridFloorLink`s (`editor/crates/psxed-project/src/lib.rs:2386`, `:2389`; `GridFloorLink`
  `:2426`; setters `set_floor_above`/`set_floor_below` `:5252`/`:5259`). The cooked compact
  sector reserves bytes 44/46, and the runtime reads `floor_above_room`/`floor_below_room`
  (`engine/crates/psx-engine/src/world.rs:544`-`:545`, `:643`-`:644`, `:1186`-`:1197`).
- Runtime already consumes it. `update_current_room_from_player` switches the player's current
  room when a sector carries a `floor_below_room`/`floor_above_room` link
  (`engine/examples/editor-playtest/src/main.rs:6235`, `:6244`).
- But it is never fed. The only caller that sets a link is a test
  (`editor/crates/psxed-project/src/playtest.rs:7214`), and the world-cook encoders inspected
  (`world_cook.rs`, `world_cook/encode.rs`) do not reference the floor links, so authored links
  are not carried into the cooked bytes. The runtime consumer therefore never fires in real
  projects. (Confirm the exact compact-sector encoder when implementing.)

## Our portal system is not really different from TR's

The record format (3D quad + 3D normal + directed + adjoining room) and the recursive 3D
frustum clip are TR's algorithm. The difference is entirely upstream: the authoring and cook
only ever produce horizontal wall portals derived from cardinal grid seams.

## Why rooms cannot stack today

Three blockers, all in placement/authoring, none in the runtime:

1. Rooms have no Y. `WorldGrid.origin` is `[i32; 2]` (X/Z only,
   `editor/crates/psxed-project/src/lib.rs:5128`); the cooked `LevelRoomRecord` has
   `origin_x`/`origin_z` and no `origin_y` (`psx-level/src/lib.rs:618`); room placement pins a
   placed room's Y to `0.0` (`placement_translation_for_room_hit`, psxed-ui around `:8203`).
   Vertical relief lives only inside a room (per-sector floor/ceiling heights). Two rooms
   cannot occupy the same X/Z at different heights.
2. Adjacency is horizontal-only. `GridDirection` is North/East/South/West plus two diagonals,
   no Up/Down (`lib.rs:1656`-`:1703`; `CARDINAL`/`DIAGONAL`/`ALL`). `LevelChunkNeighbours` is
   the four compass directions (`psx-level/src/lib.rs:713`). The room graph, and the streaming
   ring built on it, has no vertical edge. `overlapped_rooms` (which would detect
   X/Z-overlapping stacked rooms) is stubbed to empty (`playtest.rs:202`, `:487`-`:488`).
3. No authoring path. Portals are `NodeKind::Portal` nodes that snap to a cardinal grid seam
   between adjacent cells (`portal_edge_for_node` `portal_rooms.rs:273`); a floor/ceiling portal
   (two rooms sharing a horizontal plane) has no seam to snap to. There is no UI to raise a room
   to a level or to mark a floor hole as a vertical portal.

This is why the UI cannot author vertical portals: a top-down grid plus cardinal-seam portal
snapping plus Y-pinned-to-0 placement makes vertical portals inexpressible by construction,
even though the data slots and the runtime exist.

## What is already supported: within-room vertical relief

A single room has full vertical detail. `GridHorizontalFace` stores floor/ceiling with four
corner heights (sloped faces, pits, ledges), `GridVerticalFace` stores walls with bottom/top
heights, and `GridSector` carries optional floor + ceiling
(`engine/crates/psx-engine/src/world.rs:66`-`:224`). So multi-level relief inside one room
(ramps, pits, raised platforms) works today. What is missing is two separate rooms stacked in
Y, which is the TR notion of "floors".

## What it would take

Concentrated in placement, cook, and authoring. Runtime visibility and traversal-consumption
are already done.

1. Room vertical placement: widen `WorldGrid.origin` to carry Y (or add a room elevation /
   level field), stop pinning a placed room's Y to 0, and thread `origin_y` through the cook
   into `LevelRoomRecord`.
2. Vertical adjacency: add up/down to the room graph (or populate the reserved
   `overlapped_rooms` from X/Z overlap) so the streaming ring and traversal can cross levels.
3. Cook the two link types from authored data: emit `vertical: true` / `kind: 1` floor/ceiling
   portals (the `floor_above`/`floor_below` `GridFloorLink`s are the natural anchor), and write
   bytes 44/46 from authored sector links so the existing runtime consumer fires. Entry points:
   `plan_portal_rooms` (`portal_rooms.rs:207`), `build_room_portals` (`portal_rooms.rs:478`),
   `derive_room_connections` (`room_connections.rs:114`), and the compact-sector encoder.
4. Authoring UI: raise a room to a level, and mark a floor/ceiling region as a vertical portal
   to the room above/below (could auto-derive from a floor hole plus a room placed beneath it).
5. Runtime: mostly verification. The 3D clipper should already handle a Y-dominant portal
   normal; the room-handoff consumer already exists. Collision/traversal (falling, climbing)
   between stacked rooms is a further gameplay concern beyond rendering visibility.

## Coordination with the streaming refactor

Step 2 (vertical adjacency) lands squarely in the room streaming graph, which is cardinal-only
today and is exactly what the in-flight streaming refactor is reworking. Unlike the UI track,
vertical rooms is not independent of that work: it should build on whatever the new streaming
graph looks like, and "up/down neighbours" is arguably something to fold into the refactor's
graph design rather than bolt on afterward. Raise this with the streaming effort before either
side hardens its adjacency model.

## TR import note

The repo can import TR levels (`editor/crates/psxed-project/src/bin/import_tr_level.rs`,
`tr_level.rs`), and an imported `NodeKind::Portal` carries a `PortalGeometry` with a 3D normal
that `classify_geometry` will tag `FloorCeiling`. But because placement pins Y to 0, adjacency
is cardinal-only, and the cook emits `vertical: false`, an imported vertical portal does not
survive the pipeline into a working runtime portal today.
