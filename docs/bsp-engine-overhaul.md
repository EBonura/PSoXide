# BSP engine overhaul: the complete runtime architecture

Status: drafted 2026-08-10 on `quake-bsp-world`. This expands the migration
plan's P1-P3/P6 into the full engine picture. It is a complete overhaul of
the runtime world model, not a renderer swap: rendering, collision,
movement, entity residency, activation and streaming all change together.
New levels are authored entirely with brushes (owner directive); the grid
world survives only until P7 retirement.

## Frame anatomy of the new runtime

Per the landed quake-psx Rust runtime (our lift source), one frame is:

1. Fixed 60 Hz sim ticks, kept exactly as today (scheduler untouched;
   30 Hz visual pacing, pipelined present, phase-1 flip).
2. Camera leaf: descend the BSP from the root by signed plane side.
3. PVS: decompress the camera leaf's run-length visibility row and stamp
   the visible leaves for this frame.
4. World: recursive front-to-back node walk with node-bounds culling;
   marked leaves emit their marksurfaces (plane-side backface test);
   faces materialize into classic-affine vertex batches (baked UVs and
   vertex light, per-face texture windows, animated/special textures),
   inserted into the OT by depth and submitted double-buffered.
5. Entities: every entity is leaf-linked; entities whose leaves are not
   in the camera PVS are skipped for render and (behavior permitting)
   for AI. Skeletal characters render exactly as today, depth-sorted
   into the same OT via AVSZ.
6. Brush submodels (doors, platforms, movers): brush entities with their
   own BSP subtrees, transformed for render and collision both.

## Collision and movement: complete replacement

- All world collision is clipnode hulls (point, player, big; the
  `MAX_MAP_HULLS` model), served by `psx-bsp::collision` (already lifted
  and green). Point contents, segment traces, box hulls for entities.
- `character_motor` keeps its POLICY layer verbatim: acceleration, step
  up/down heights, jump, slope feel, the tuned third-person contract.
  Its world queries (floor height under point, wall blocking, ground
  probing) are reimplemented as hull traces. `third_person_camera` floor
  and occlusion probes become traces too.
- Triggers and touch use leaf/box links (the areanode pattern).
- Gates: trace parity against quake-psx on recorded segments, then feel
  parity on existing input tapes.

## Characters, animations, entities (how the mature systems fit)

- The skeletal pipeline is untouched: psx-asset models, psxanim v3
  clips, the model_rendering path, combat and FSM logic keep their
  semantics. This is the migration's core promise.
- The one structural change is coordinates and residency: room-local
  positions + `RoomIndex` become world-space i32 + leaf links, and the
  hl-psx-style `active_rooms` AI gate becomes PVS-based activation
  (entity awake when its cluster is visible from the player's cluster,
  or when behavior forces it). Same gating semantics, new substrate.
- Spawning: the PXBSP entity lump carries world-space records with our
  entity class ids and skeletal model references. The editor's Place
  flow becomes world-space point-entity placement inside brush maps.

## Shared with quake-psx

- `psx-bsp` is the shared home: XBSP/PXBSP formats and collision are in;
  the render walk and ResidentMap loader are the next lift (waiting on
  that session's stable-commit ping); the alias-model table APIs fold in
  at the same time.
- Already shared at SDK level: classic-affine renderer, GTE scene
  helpers, OT async submit, input/aim curves, psx-pack containers, CD
  machinery.
- Not shared: gameplay (Quake progs vs our entity systems), skeletal
  animation (ours), UI.
- Adoption terms (agreed): quake-psx swaps to psx-bsp only after
  wire-parity and headless frame-hash tests pass, with the release
  dependency revision-pinned through the hydrated `.psoxide` SDK.

## Controls and camera

- Pad conventions and the 60 Hz sim clock are unchanged (owner
  decision: fixed sim is permanent).
- Third person stays: the camera rig and motor feel carry over; only
  their world probes change to traces. The shared first-person aim
  curve precedent already shows where input curves live (SDK).
- A first-person debug fly/walk mode falls out of the quake-psx player
  code nearly free and is worth having for map inspection.

## BSP and level streaming (the P6 design)

Split the map into an always-resident core and paged payloads:

- Resident always: nodes, planes, leaves, clipnodes, PVS, entity
  records, strings. This is bounded and small (order 100-200 KB at
  episode-map scale; the cook emits a budget report per map). Collision
  and visibility therefore NEVER stall on the CD; gameplay is always
  correct even mid-stream.
- Paged: face/vertex/marksurface payloads and textures, grouped into
  region chunks by leaf clusters at compile time (portal chokepoints or
  spatial buckets), stored as psx-pack chunks and streamed by the
  existing cd_stream machinery.
- Prefetch: the cook writes a cluster-to-region map. When the player's
  cluster PVS (extended one portal depth out) references a non-resident
  region, its chunk is enqueued; eviction by PVS absence with
  hysteresis. A face whose region is not yet resident is skipped, so
  the failure mode is visual pop-in, never a stall; the prefetch
  horizon is sized against the emulator's silicon-calibrated CD model.
- Baseline first: whole-map resident (the Quake model) whenever the map
  fits; quake-psx proves ~1.1 MB per episode map. Streaming engages
  only for maps that outgrow that, measured, not assumed.

## Editor compile chain (P5), brush-native

- New-project template: a brush world with zero grid rooms.
- Compile: CSG face clipping, BSP build, portal generation, leaf
  clustering, PVS (draft mode: all-visible), light bake to vertex RGB
  from editor lights, clipnode hull generation (bevel expansion per
  hull size), PXBSP pack with region grouping.
- Draft cooks (Play) skip vis and light for iteration speed; release
  cooks run everything.

## Sequencing

1. (Blocked on the quake-psx stable ping) Lift renderer + ResidentMap
   into psx-bsp; headless E1M1 camera-flight gate with cycle numbers.
2. Hull-backed character_motor + third-person camera walking E1M1 under
   tape input; trace-parity and feel gates.
3. Minimal brush compiler (CSG + BSP + clipnodes + draft PVS) feeding
   editor Play with the real BSP runtime and the player character. This
   is the milestone: the first authored brush map you can walk.
4. World-space entities: spawn from the lump, PVS activation, a combat
   slice with one enemy family as the gate.
5. Light bake, full vis, budget reports; region streaming only after
   the resident baseline is measured.
6. Movers/submodels; polish; grid retirement (P7) last.
