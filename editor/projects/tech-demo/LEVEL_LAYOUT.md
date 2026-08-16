# Ashen Sanctum tech demo

This file is the level's design and acceptance contract. `project.ron` is the
authoritative implementation; keep this document aligned whenever the route,
entity names, or supported mechanics change.

## Original-inspiration boundary

Ashen Sanctum studies the *shape of an experience* found in compact
Souls-like opening areas: confinement, a first glimpse of an intimidating
arena, an escape into a looping upper route, a checkpoint, a shortcut, and a
return to the arena with better knowledge.

It is not a reproduction of Dark Souls' Undead Asylum. Do not copy its room
dimensions, floor plan, encounter positions, names, textures, models, audio,
lighting, set dressing, or recognizable architectural silhouettes. All
geometry and presentation here must remain original PSoXide content. If a
space reads as a one-to-one comparison rather than the same broad design
grammar, redesign it.

## Playable route

The intended first pass is a short, legible loop through one reused arena:

```text
Cinder Cell
    |
    v
Intake Passage
    |
    v
Flooded Junction / Water
    |
    v
Four-riser Ascent
    |
    v
Courtyard Relay / Roofless Arrival Court
    |
    v
First Warden Gate
    |
    v
Roofless Warden Court / Warden Prototype (first approach)
    |
    v
Side Escape
    |
    v
Sanctum Relay / Equipment Hall -- Gallery Custodian
    |                         |
    |                         +--> Sanctum Shortcut --> Arrival Court
    v
Five-riser Upper Rampart
    |
    v
Four Descending Ledges
    |
    v
Roofless Warden Court / Warden Prototype (same arena, upper return)
    |
    v
Far Warden Gate
    |
    v
Rising Cliff Vista
```

Critical beats:

1. **Cinder Cell:** safe movement/camera tutorial and a confined starting view.
2. **Intake Passage:** a long compressed approach containing the `Intake
   Custodian` before the first large reveal.
3. **Flooded Junction:** a distinct Water contents volume marks the route's
   environmental hazard before the climb. It is not lava and does not stand in
   for a lava-damage test.
4. **Four-riser Ascent:** four broad steps rise 448 units apiece, below the
   proven 640-unit motor step envelope, and replace any need for a ladder, jump
   or precision movement.
5. **Courtyard Relay and arrival court:** `Courtyard Memory Field` targets the
   `Courtyard Relay` checkpoint as the route reaches its first roofless open
   court.
6. **First Warden Gate:** the first moving gate admits the player into the
   central roofless Warden Court for the intimidating ground-level approach.
7. **Roofless Warden Court:** this single physical arena contains the scaled
   Rust Mantis entity `Warden Prototype`. The first approach and upper return
   must never be documented or rebuilt as separate arenas. The prototype is a
   combat and scale proxy, not a claim of finished boss behavior.
8. **Side escape:** a lower side route leaves the Warden Court and drops within
   the supported motor envelope into the relay loop.
9. **Sanctum Relay and equipment hall:** `Relay Approach` targets the second
   checkpoint, `Sanctum Relay`, before the `Gallery Custodian` and the long
   upper approach. The four characters already carry authored equipment;
   there is no collectible equipment pickup in this first version.
10. **Sanctum Shortcut:** the second moving gate reconnects the relay loop to
    the arrival court. It is an optional spatial shortcut, not the main route
    to the final vista.
11. **Five-riser Upper Rampart:** five broad 512-unit risers create the long
    vertical return without a ladder, key or scripted obstacle. Crossing the
    `Upper Return Threshold` activates the `Upper Route Cleared` master.
12. **Four descending ledges:** four 512-unit drops lead back into the same
    Warden Court. They create a plunge-like composition without requiring a
    plunge attack or fall-damage mechanic.
13. **Far Warden Gate and rising cliff vista:** the far arena gate is
    master-gated by `Upper Route Cleared`, so it cannot open until the player
    has crossed `Upper Return Threshold` on the upper route. It then opens onto
    four broad rising cliff stages and the authored end vista. No cutscene or
    level transition is implied.

The critical route must never require a precision jump, hidden switch, or
unimplemented script. Optional overlooks and dead ends may reward exploration
without blocking completion.

## Systems used now

- PXBSP solid brushes for all static floors, walls, stairs and boundaries.
- PXBSP materials; texture lock should be enabled while moving structural
  brushes.
- One player character (Aletha) with the light sword equipment attachment.
- Three Rust Mantis-profile enemies named `Intake Custodian`, `Gallery
  Custodian` and `Warden Prototype`. The two Custodians share the same visual
  scale, while the Warden is an explicitly labelled larger proxy. All three
  use the current enemy/runtime path and heavy sword equipment.
- Character collision, authored hurt/attack capsules, stamina, roll, stagger,
  damage and death already supplied by the shared souls runtime.
- Three moving gates, `First Warden Gate`, `Sanctum Shortcut` and `Far Warden
  Gate`. Each has a `Door` logic node plus a brush whose **Model owner** is that
  exact door. `Far Warden Gate` additionally names `Upper Route Cleared` as its
  master; the `Upper Return Threshold` touch trigger supplies the one input to
  that multisource.
- Two checkpoints: `Courtyard Relay`, targeted by `Courtyard Memory Field`, and
  `Sanctum Relay`, targeted by `Relay Approach`. Each owns an `Interactable`
  component of kind `Checkpoint`.
- One authored Water contents volume in the Flooded Junction. There is no lava
  brush in this project.
- BSP PVS and the project sky/panorama path; visibility is automatic, not
  authored with legacy grid portals.

### Current geometry and cook budget

The generated checkpoint measured on 2026-08-13 cooks to a 173,840-byte PXBSP
with 1,473 visible surfaces and a 51-byte PVS row. Its 2,946 authored triangle
packets derive a 3,008-packet arena. Relative to the runtime's hard
4,096-packet ceiling, that leaves 1,150 authored packets, or 28.1 percent, of
content headroom.

This is a content-budget checkpoint, not a performance or visibility proof.
The static PXBSP is still wholly resident and its walkable space remains one
portal-connected visibility component; there are no streamed BSP micro-sections
yet. The warning-only, camera-independent TR planning envelope is 7,365 packets
because it pessimistically charges every visible world face before frustum,
back-face and runtime hardware-extent behavior. That planning number is not an
emitted-packet bound. The cooked player-hull route, recorded runtime peak packet
count and eventual console pass remain authoritative acceptance gates after any
geometry change.

### Automated checkpoint

The generated project is rebuilt through the editor's production commands and
then loaded through the final cooked `PxbspResidentMap`. The host route proof
uses Aletha's authored body hull plus all three transformed door hulls. It
proves every closed gate blocks, every fully opened gate clears, both stair
flights and all landings fit, the Warden Court is traversed on both elevations,
and the cliff vista is reachable without placement or teleporting.

The 2026-08-13 real-MIPS smoke ran 600 simulation frames from the authored
spawn, uploaded seven room textures and three model atlases, rendered Aletha
and both weapon types, and completed without an asset-load failure, panic or
allocator trap. Its deterministic terminal hashes were VRAM
`0x3018415c8718e80d` and display `0xc6d1334a8a429eac` at 320x240. This is
emulator evidence, not original-hardware acceptance.

The same smoke also records an honest performance warning: the current spawn
view misses the two-VBlank visual budget, with room rendering dominating the
profile. The project is a playable level-design blockout, not yet a stable
30-fps content candidate. Reduce connected-world draw cost and validate a
recorded traversal before adding architectural detail.

Playtest controls:

- Left stick or D-pad: camera-relative movement.
- Right stick: orbit camera; L1 recentres it.
- Circle tap: directional roll; Circle held while moving: run.
- R3: lock/unlock the most central live target; right-stick flick switches.
- R1: light attack; R2: heavy attack; L2: combo attack.
- Cross: interact with doors and dismiss the checkpoint overlay.
- Select: debug free-orbit camera, for diagnosis rather than normal play.

## Authoring conventions

- This is a BSP project. Never add `Room`, `Section`, manual `Portal`, or
  legacy-grid streaming geometry.
- PXBSP is Y-up. Keep walkable floors, stairs, doors and spawn elevations on
  the editor grid; use grid 16 for blockout and reduce it only for intentional
  detail.
- Use outward architectural shells built from solid brushes, not one enormous
  overlapping solid. Avoid coplanar duplicates, paper-thin blockers, accidental
  sealed player volumes, and decorative brushes that alter the player route.
- Give stable gameplay nodes unique role-first names. Trigger targets must
  match entity names exactly; do not casually rename the bindings documented
  above.
- Maintain exactly one player source. Character placement must not leave the
  template Player Spawn alongside Aletha.
- Doors require both halves of the contract: a Door logic node and a mover
  brush bound through **Model owner**. Verify both after duplicating anything.
- Treat the Courtyard Relay, First Warden Gate, Sanctum Relay, upper rampart,
  Warden Court, Far Warden Gate and cliff vista as named landmarks. From each
  decision point, at least one should be visible or strongly framed.
- Use original neutral stone/metal/ash materials. Do not use character-derived
  textures or recreate FromSoftware motifs. Keep face scale consistent; use
  face U/V/rotation controls for deliberate alignment rather than compensating
  by moving the brush.
- Keep the first shipping version compact. The entire static PXBSP world is
  currently resident; micro-section BSP streaming is future work.
- Generated `cooked/`, `baked/`, logs and captures are never source assets.

## Deferred mechanics

These are explicitly outside the first playable and must not be simulated by
mislabelled stand-ins:

- bespoke Warden boss AI, phases, boss health UI, arena fog gate and rewards;
- keys, inventory, save/continue, bonfires beyond the current checkpoint loop;
- scripted ambushes, cinematics, dialogue, item pickups and quest state;
- ladders, mantling, jumping, breakable walls and moving platforms;
- multiple enemy archetypes, ranged combat and encounter scripting;
- streamed BSP micro-sections, seamless level transitions and background music;
- final particles, audio mix, authored light bake and console performance pass.

The blockout should leave room for those additions without depending on them.

## Native acceptance checklist

### Editor

- [ ] Open the project in the native editor without warnings or an automatic
      rewrite; confirm it is reported as BSP/Draft.
- [ ] Top, Front and Side views show every structural brush at useful scale.
- [ ] Select, move and resize representative floor, wall, stair and door
      brushes; Undo restores each operation exactly.
- [ ] Move a texture-locked wall: its mapping remains visually anchored.
- [ ] Select a face and change U%, V%, rotation and offsets independently;
      scaling pivots without unintended sliding.
- [ ] Confirm Aletha is the only player source. Confirm Aletha, `Intake
      Custodian`, `Gallery Custodian` and `Warden Prototype` each have the
      intended weapon equipment attachment.
- [ ] Confirm mover ownership for `First Warden Gate`, `Sanctum Shortcut` and
      `Far Warden Gate`. Confirm `Upper Return Threshold` targets `Upper Route
      Cleared` and `Far Warden Gate` names that exact master.
- [ ] Confirm `Courtyard Memory Field` -> `Courtyard Relay` and `Relay
      Approach` -> `Sanctum Relay`, including both checkpoint components.
- [ ] Confirm all three enemy profiles/hurtboxes, the Flooded Junction Water
      contents, four lower risers, five upper risers and four descending
      ledges.
- [ ] Save, close, reopen, and confirm brush selection and all bindings remain.

### Playtest

- [ ] Play and Rebuild & Play both reach gameplay in the embedded viewport.
- [ ] Spawn is clear of solids; the complete critical route is walkable without
      debug camera, teleporting or precision movement.
- [ ] The Intake Passage, Flooded Junction, Courtyard Relay, first arena
      approach, side escape, relay hall, upper rampart and cliff vista are
      readable without prior knowledge of the layout.
- [ ] Enter the Water volume and confirm its current supported immersion
      behavior. It must not behave or report as lava.
- [ ] `Courtyard Relay` and `Sanctum Relay` each activate through their authored
      trigger, and each overlay dismisses with Cross.
- [ ] `First Warden Gate` and `Sanctum Shortcut` open only through intended
      interaction. Confirm `Far Warden Gate` refuses interaction before the
      upper return, then opens after crossing `Upper Return Threshold`; confirm
      collision clears every aperture when fully open.
- [ ] Lock-on, circling, running, rolling and R1/R2/L2 attacks work in the
      Intake Passage, equipment hall and Warden Court; both sword types follow
      the correct hand in motion.
- [ ] `Intake Custodian`, `Gallery Custodian` and `Warden Prototype` can each be
      staggered and killed; walls and closed gates prevent melee damage through
      geometry.
- [ ] Deliberate death respawns at the most recently activated checkpoint and
      resets enemy/gate state.
- [ ] The four-riser and five-riser climbs are continuous, and the four ledges
      descend into the Warden Court without jumping, teleporting or becoming
      stuck.
- [ ] No visible void, accidental ceiling, stuck corner, camera trap, severe
      texture swimming, stale frame, or unexplained hazard appears on-route.
- [ ] Enter the Roofless Warden Court through `First Warden Gate`, leave by the
      side escape, and return to that same arena from the upper ledges.
- [ ] Fight `Warden Prototype` at a playable frame cadence, open `Far Warden
      Gate`, and reach the rising cliff vista. Record any slow vista for later
      profiling; do not hide it by removing landmarks.

For the proven generic workflow and current runtime behavior, see
[`fresh-project-workflow-checklist.md`](../../../docs/fresh-project-workflow-checklist.md)
and [`souls-slice-acceptance.md`](../../../docs/souls-slice-acceptance.md).
