# Project finalisation log

Living backlog for turning PSoXide from a working research/demo stack
into a cohesive project. This is deliberately broader than
`docs/milestones.md`: the milestone ladder tracks emulator canaries,
while this log tracks editor/runtime/SDK/product polish.

Last updated: 2026-05-04.

## Current active thread

### Entity facing reaches playtest builds

Status: in progress.

Problem: editor Y rotation must survive cook/build so player starts,
placed actors, and future enemies face the authored direction.

Scope:
- editor entity/character-controller authoring;
- playtest cook records for spawns, model instances, equipment, and
  future NPC records;
- runtime draw/camera/controller consumption of cooked yaw.

Done when:
- rotated player spawns initialise player yaw correctly;
- rotated non-player character entities render facing the authored
  direction;
- cooked manifest tests pin editor degrees to PSX angle units;
- editor-playtest builds after a fresh cook.

## World and level authoring

### Diagonal world geometry

Status: design needed.

Goal: reintroduce the diagonal world support that existed in Bonnie32,
but carry it through the whole PSoXide pipeline instead of only
authoring it.

Likely scope:
- editor grid authoring and picking for diagonal walls/sector cuts;
- validation UI that distinguishes supported diagonals from invalid
  geometry;
- `psxed-project::world_cook` support instead of rejecting diagonal
  walls;
- `.psxw` schema/runtime parser compatibility;
- `psx-engine` collision, visibility, and world rendering;
- playtest cook/build tests for diagonal walls and traversal.

Done when:
- a diagonal wall can be authored, cooked, rendered, picked, and
  collided against consistently;
- diagonal floor/ceiling splits keep matching editor and runtime
  triangulation;
- malformed diagonal data fails loudly in parser/cooker tests.

### Three stacked wall segments

Status: design needed.

Goal: walls should support up to three vertical segments on the same
edge so rooms can express ledges, trim, half-height blockers, and
decorative stacked materials.

Likely scope:
- editor controls for adding/removing/reordering stacked wall segments;
- clear per-segment material, solidity, and UV controls;
- cook/runtime limits set to a hard cap of three segments;
- budget UI that reports segment counts and triangle impact;
- `psx-engine` wall collision/rendering for stacked segments.

Done when:
- one edge can carry 1, 2, or 3 stacked wall segments;
- over-cap stacks are blocked in editor validation and cooker tests;
- runtime render/collision matches the authored segment heights.

### Background and skybox

Status: design needed.

Goal: give playtest scenes an intentional background. Start with a
simple gradient, then explore distant sprite cards inspired by PS1-era
games such as Spyro.

Likely scope:
- first pass: configurable sky/background gradient in editor project
  data and playtest manifest;
- runtime render pass that draws background before world geometry;
- later pass: far-distance sprite/card layers with camera-relative
  placement, constrained parallax, and stable ordering-table behavior;
- editor preview parity with runtime framing.

Open design questions:
- whether distant sprites are authored per room, per world, or as a
  shared scene background;
- whether sprites should follow camera translation partially or only
  camera yaw/pitch;
- how to avoid near/far clipping and OT depth fights on PS1 hardware.

Done when:
- every playtest build has a non-black, stable background;
- camera movement cannot reveal edges or make the background intersect
  authored room geometry;
- at least one screenshot/hash-style regression guards the gradient or
  background pass.

## Player and gameplay feel

### Z-targeting movement and animation polish

Status: needs investigation.

Problem: lock-on/Z-targeting movement is target-relative, but selected
animations do not match the movement direction.

Likely scope:
- define movement states for locked-on forward, backpedal, strafe left,
  strafe right, turn-in-place, and run;
- extend `CharacterResource` / animation role maps if the current
  idle/walk/run/turn set is too small;
- make `CharacterMotor` report animation intent rich enough for the
  runtime player path;
- keep camera-relative and target-relative movement rules explicit.

Done when:
- locked-on movement chooses clips that visually match direction;
- missing optional clips fall back predictably rather than popping;
- unit tests pin motor intent for each lock-on input quadrant.

### Enemy AI

Status: probably later.

Goal: introduce enough enemy behavior to prove the editor/component
model can drive non-player characters.

Suggested first slice:
- deterministic idle/patrol/chase state machine;
- one target source: the player;
- simple attack range and cooldown;
- no pathfinding beyond direct movement/collision avoidance until room
  traversal and diagonals settle.

Done when:
- an enemy authored in the editor cooks into runtime data;
- it can face, move toward, and stop near the player;
- behavior is deterministic enough to test without visual inspection.

## Editor and project UX

### Project menu names

Status: small, independent bug.

Problem: the frontend Projects menu can show repeated
`untitled_ps1_project` entries. It should display the project name from
the project's own metadata, not just a directory/default stem.

Likely scope:
- project scanning in frontend/settings/project listing;
- reading `project.ron` cheaply enough for menu display;
- fallback naming for missing or malformed project files;
- collision display, such as showing the directory as secondary text
  when names repeat.

Done when:
- the Projects menu displays each project's authored name;
- duplicate names are disambiguated without changing project files;
- tests cover multiple project directories with identical/default
  folder names.

### Editor transform gizmos

Status: design needed.

Goal: add viewport gizmos for moving, rotating, and scaling authored
objects without relying only on inspector numeric fields and hotkeys.

Likely scope:
- selected-node transform handles in the 3D editor viewport;
- explicit move/rotate/scale modes with snapping options;
- entity-safe constraints, such as yaw-only rotation for gameplay
  entities unless the node type supports full Euler rotation;
- undo/redo integration and dirty-state updates for every drag;
- visual parity between gizmo edits, inspector values, and playtest
  cooked transforms.

Done when:
- movement, rotation, and scale can be edited directly in the viewport;
- transforms are committed to project data and survive save/cook/build;
- gizmo drag tests or interaction probes cover snapping, cancel/commit,
  and undo behavior.

## SDK examples and demos

### `hello-gte`

Status: investigate, then repair or remove.

Problem: `hello-gte` currently appears not to do anything useful. It
should either be restored as a meaningful GTE canary/demo or removed
from the public example set.

Decision paths:
- repair: make it visibly render and include it in example/run docs;
- remove: delete the example and update README, Makefile targets,
  parity notes, and any probe tools that assume it exists.

Done when:
- `make hello-gte` and any documented run path produce a useful
  visible result, or the target is gone everywhere;
- README and `docs/redux-oracle.md` no longer contradict reality.

## Architecture hygiene

### Consolidate repeated functionality

Status: ongoing.

Goal: keep the four fundamental pieces -- emulator, SDK, engine, and
editor -- aligned instead of letting parallel implementations drift.

Audit areas:
- angle/yaw conversion between editor degrees and PSX Q12 units;
- world-coordinate transforms across editor preview, cooker, and
  runtime;
- model/animation clip resolution and fallback rules;
- asset path resolution and validation;
- lighting/material conventions between editor preview and runtime;
- generated manifest schema versus `psx-level` records.

Done when:
- each shared rule has one obvious owner or helper API;
- duplicated conversions have focused tests on both producer and
  consumer sides;
- `make check`, `make test`, and the editor-playtest cook/build path
  stay green after refactors.
