# World Streaming Engine Plan

Status: active implementation plan.

This plan keeps the editor workflow flexible while making the PS1
runtime data small, deterministic, and streamable.

## Core Decision

Editor rooms may be arbitrarily large. Runtime rooms should not be.

The editor owns authored `WorldGrid` data. The cooker owns subdivision.
The runtime owns only cooked chunks, shared assets, and transient caches.

```text
Editor authoring
  Authored Room / WorldGrid

Cooker output
  WorldChunk[] + global AssetTable + residency graph

Runtime
  active chunks + warm chunks + shared assets + frame scratch
```

The runtime should not contain a second rich copy of the world in a
cache. Cooked chunk data is the canonical geometry store. Runtime caches
exist only to accelerate the current frame or active set.

## CD / ISO Timing

Do not make production CD streaming the next blocking task.

The current priority is to fix the logical data model:

1. chunk identity,
2. asset identity,
3. residency,
4. shared texture/model ownership,
5. visibility,
6. geometry packing.

CD reading should be a bounded proof-of-life, not the main engine
rewrite yet. The useful early spike is:

```text
build tiny ISO
  -> put one stream pack on it
  -> boot executable
  -> read one asset by sector/offset into RAM
  -> parse the same bytes through the normal AssetStorage API
```

That proves the transport layer without letting ISO details leak into
the world architecture. Until then, `include_bytes!` remains a valid
backing store as long as the runtime talks to an asset-storage
abstraction instead of directly depending on embedded byte slices.

Production CD streaming should start after the chunk/residency model can
run from embedded data. At that point the CD backend becomes a storage
swap, not an architecture rewrite.

## Ownership Model

### Authored Data

Authored data is for the editor:

- scene tree,
- large `WorldGrid` rooms,
- author-facing materials,
- light nodes,
- entity nodes,
- portals,
- editor-only metadata.

This data can be comfortable and redundant. It is not the runtime
format.

### Cooked Chunk Data

Cooked chunk data is the runtime world truth:

- packed vertices,
- packed surfaces,
- baked vertex lighting,
- cell surface ranges,
- cell bounds,
- blocker and portal masks,
- collision records,
- static entity placements,
- chunk bounds,
- neighbour links,
- portal links,
- asset references.

Each chunk owns its geometry and collision. It does not own shared
texture, model, animation, or audio payloads.

### Shared Assets

Shared assets are global and deduplicated:

- textures,
- models,
- animations,
- audio banks,
- fonts,
- common effects.

Chunks reference shared assets by compact IDs.

```text
chunk_12 -> textures [3, 4, 9]
chunk_13 -> textures [4, 9, 10]
chunk_14 -> textures [3, 10]

resident VRAM
  texture 3  refs 2
  texture 4  refs 2
  texture 9  refs 2
  texture 10 refs 2
```

When one chunk unloads, a shared asset remains resident if another
active or warm chunk still references it.

## Target Runtime Memory Shape

Runtime memory should look like this:

```text
required current chunk geometry/collision
+ warm neighbour chunk geometry/collision
+ globally deduped shared assets
+ active dynamic entities
+ transient render scratch
```

It should not look like this:

```text
all chunk geometry
+ copied textures per chunk
+ duplicated material payloads
+ all-cells-per-anchor visibility tables
+ reconstructed rich surface cache
+ persistent duplicate projected geometry
```

## Phase 0: Correctness Baseline

Goal: stop optimizing against a moving or broken target.

Tasks:

- Add cached vs uncached surface equivalence tests.
- Compare editor preview lighting against runtime lighting for a tiny
  known scene.
- Verify cached sample center, baked RGB, kind, ordinal, wall direction,
  and material.
- Add demo3 screenshot and telemetry baseline.
- Add counters for room packets, split packets, visible cells, active
  chunks, projected vertices, dropped packets, and packet queue pressure.
- Add build report entries for chunk bytes, visibility bytes, texture
  refs, estimated packets, and stack margin.

Exit criteria:

- Demo3 lighting is correct.
- Cached and uncached surface paths are equivalent.
- The build report shows where memory and packet pressure come from.

## Phase 1: Make Chunks The Runtime Unit

Goal: authored rooms stay arbitrary; runtime sees only cooked chunks.

Tasks:

- Keep `WorldGrid` as editor authoring data.
- Emit stable `WorldChunk` records from the cooker.
- Give each chunk stable identity from authored room ID and world-grid
  coordinates.
- Split by runtime cost, not only by maximum dimensions.
- Keep hard caps for width, depth, triangles, byte size, and coordinate
  safety.
- Add chunk manifest records for bounds, origin, size, neighbours,
  portals, and asset references.

Initial split policy:

- Target `8x8`, `12x12`, or `16x16` sectors.
- Prefer cost-based split when surface count, packet estimate, light
  overlap, or byte size is high.
- Avoid splitting purely empty space.
- Report over-budget reasons per chunk.

Exit criteria:

- Demo3 cooks into performance-sized chunks.
- The runtime no longer treats large authored rooms as runtime rooms.
- Chunk boundaries are visible in editor diagnostics.

Current implementation notes:

- The playtest manifest now emits one `ROOM_CHUNKS` record per cooked
  chunk, including authored room id, chunk index, bounds, and cardinal
  neighbours.
- The runtime active window now uses cooked chunk metadata as a compact
  spatial candidate set and activates the closest chunks that fit the
  shared cache budget. Older manifests without chunk records still use
  the old touching fallback.
- The selector keeps the active geometry window stable around the
  player/current authored room, then lets per-frame frustum culling
  decide what to submit. Other authored rooms must be non-overlapping
  with the current authored footprint, so copied/stacked room
  experiments do not leak into the draw set just because they share X/Z
  coordinates.
- The active window is refreshed when the player moves several sectors
  inside the current runtime chunk. Camera rotation never rebuilds room
  caches, which avoids stutter while turning.
- Runtime visible-cell BFS is anchored from the camera X/Z position,
  then filtered by cell global range. The cached room draw path still
  runs the exact camera frustum test every frame, so rotating in place
  cannot reuse a stale direction-filtered cell list.
- The cached vertex-lit path now only gathers per-vertex fog depths when
  lighting actually consumes them. No-fog scenes keep the same baked
  lighting result without doing depth preparation for every surface.
- `make profile-demo3` is the canonical headless baseline command. The
  current measured demo3 run loads 4 active chunks, uses 4 cached room
  draws, uses 0 uncached room draws, and spends about 2.97M cycles/frame
  in room rendering while drawing about 755 camera-visible room
  surfaces/frame.

## Phase 2A: Visibility-Driven Chunk Expansion

Goal: show enough of the level without returning to the old broad
touching-chunk overdraw.

Current problem:

- Cardinal chunk neighbours made demo3 smooth, but they show too small
  a portion of the level.
- Some geometry appears outside the intended player/camera visibility
  circle. Do not assume the cause yet: it may be far-vista geometry,
  neighbour chunks using clamped local visibility anchors, generated
  transition walls, cached-cell bounds, or stale chunk activation.

Decision:

Do not go directly to a full world-space visibility rewrite. Proceeded
in three contained passes:

1. classify the artifact,
2. add one global range/frustum rejection layer,
3. expand chunks with a cache-budgeted BFS.

### 2A.1 Debug Geometry Classification

Add temporary or toggleable diagnostics that make each source visible in
screenshots and telemetry:

- current chunk room surfaces,
- neighbour chunk room surfaces,
- generated floor-transition walls,
- far-vista ring,
- cached room cells,
- uncached fallback draws.

Counters should report:

- chunks considered,
- chunks activated,
- chunks drawn,
- cells rejected by global range,
- cells rejected by frustum,
- far-vista packets,
- transition-wall surfaces.

Exit criteria:

- The out-of-circle geometry is identified by source.
- `make profile-demo3` produces a screenshot and profile that explain
  the artifact without guessing.

Current status:

- Telemetry reports chunks considered, active chunks, cache skips,
  global-range rejected cells, frustum-culled cells, cached draws, and
  uncached fallbacks.
- Far-vista remains a separate background draw and is not considered
  room geometry.
- Demo3 has authored room copies occupying nearly the same X/Z range;
  the runtime now rejects overlapping unrelated authored-room chunks so
  copied rooms do not accidentally behave like visible neighbours.

### 2A.2 Global Visibility Rejection

Reject visible cells against one coherent player/camera visibility
volume before submitting their surfaces.

Runtime shape:

```text
global player/camera visibility volume
  -> active chunk
    -> chunk-local visible cells
      -> reject by cell global bounds + camera frustum
      -> draw surviving surfaces
```

Important constraints:

- Do not clamp the player anchor into neighbour chunks as the only
  visibility rule.
- Convert each cell's bounds to world/chunk-global space before range
  tests.
- Preserve the cached surface path. Rejection should choose cells, not
  rebuild surfaces.
- Far-vista remains a separate always/conditionally drawn background
  system, not part of room-cell visibility.

Exit criteria:

- Geometry outside the intended visibility volume is gone, unless it is
  explicitly classified as far-vista/background.
- Demo3 keeps 0 uncached room draws.
- Room render stays comfortably below the old broad-window baseline.

Current status:

- Chunk-local visible cells are filtered by global cell bounds before
  they reach the cached room draw path.
- The cached room cell table is compact and stores only populated cells,
  so larger draw windows do not duplicate empty grid headers.
- The old flattened `VISIBLE_CELLS` table is gone. Runtime visibility
  now traverses compact cell records directly through their open-edge
  masks, which cuts demo3 visibility metadata from 29,088 bytes to
  24,240 bytes and removes the dead per-anchor table from the manifest.
- Active visibility uses one shared 1024-cell scratch arena plus
  per-active-slot ranges. This avoids rebuilding visibility BFS for
  every active chunk every frame without allocating an 8x512 cell cache.
- The visible-cell cache is keyed by camera-derived anchor cells rather
  than only by the player cell. It does not cache camera direction; the
  exact draw frustum remains the final authority each frame.

### 2A.3 Cache-Budgeted Chunk Expansion

Replace "current plus cardinal neighbours" with bounded spatial
selection over `ROOM_CHUNKS`.

Traversal policy:

```text
scan cooked chunks in the global activation radius
score chunks by authored-room priority and distance
reject overlapping unrelated authored-room chunks
reject chunks that would overflow the shared active cache
activate draw chunks until the draw cap or cache budget is reached
```

Final demo3 budget:

- Draw chunks: 4 current-authored chunks.
- Runtime visibility radius: 64 grid cells.
- Global visibility radius: 64 sectors.
- Warm chunks: current frontier neighbours that are resident but not
  drawn.
- Cached room draws: all active draw chunks.
- Uncached room draws: 0 in the normal path.
- Room render: about 2.97M cycles/frame on the current 60-frame demo3
  profile.
- Primitive queue: about 1327 packets/frame, with about 1946 slots still
  free.

Budget rules:

- Stop adding draw chunks before exceeding cached cell, vertex, or
  surface capacity.
- Prefer chunks from the current authored room.
- Reject unrelated authored rooms that overlap the current authored
  footprint in X/Z. Without portal/zone metadata, overlapping authored
  rooms are separate spaces, not neighbours.
- Prefer nearer chunks inside the same priority class.
- Never rebuild the active draw window on camera yaw/pitch changes.
  Camera changes only affect per-frame frustum culling.
- Warm residency may include more chunks than draw activation, but warm
  chunks must not allocate render cache.

Exit criteria:

- Demo3 shows a materially larger portion of the level.
- Active draw chunk count is bounded by telemetry, not a fixed broad
  touching window.
- Primitive queue pressure remains safe.
- Cache fallback counters stay at zero in the normal path.

Measured alternatives:

- 3 draw chunks, radius 4: ~0.68M room cycles/frame, too little level
  visible.
- 7 draw chunks, radius 4: ~1.49M room cycles/frame, good compromise.
- 8 draw chunks, radius 4 before visible-cache reuse: ~1.75M room
  cycles/frame and ~308 room surfaces/frame.
- 8 draw chunks, radius 4 after visible-cache reuse: ~1.44M room
  cycles/frame with identical output.
- 8 draw chunks, radius 5: ~2.00M room cycles/frame and ~452 room
  surfaces/frame.
- 8 draw chunks, radius 6: ~2.50M room cycles/frame and ~578 room
  surfaces/frame.
- 8 draw chunks, radius 7: ~3.05M room cycles/frame and ~725 room
  surfaces/frame.
- 8 draw chunks, radius 8: ~3.47M room cycles/frame and ~837 room
  surfaces/frame, but it includes overlapping authored-room copies in
  demo3 and is rejected.
- 8 draw chunks, radius 9: ~4.00M room cycles/frame, ~937 room
  surfaces/frame, and only 57 frames in the fixed-step profile;
  rejected.
- 9 draw chunks, radius 4 after visible-cache reuse: ~1.65M room
  cycles/frame but only 57 frames in the fixed-step profile, with the
  active cache almost full; rejected.
- 4 current-authored draw chunks, player-anchored visible cells, radius
  16: ~2.75M room cycles/frame and ~725 room surfaces/frame.
- 4 current-authored draw chunks, camera-anchored visible cells, radius
  16: ~2.67M room cycles/frame and ~699 room surfaces/frame.
- Increasing the pre-frustum visible-cell screen margin from 96 to 512
  submitted no additional surfaces or packets, but raised walked cells
  and room cost; rejected.
- 4 current-authored draw chunks, camera-anchored visibility, direction
  cached in the visible-cell list: smooth default profile but missing
  geometry when turning unless the room window was rebuilt; rejected.
- 4 current-authored draw chunks, camera-position visibility only,
  radius 64: ~2.97M room cycles/frame, ~755 room surfaces/frame,
  stable camera cost, and the opposite-facing test covers the same 403
  visible cells with no active-window rebuild.

Final measured demo3 pass:

- 4 draw chunks.
- 4 cached room draws.
- 0 uncached room draws.
- Visibility radius 64.
- Global visibility radius 64 sectors.
- ~2.97M room cycles/frame.
- ~1327 primitive packets/frame, with ~1946 primitive slots free.
- Active cache: ~405 cells, ~882 vertices, ~913 surfaces.
- Room visible cells: ~403/frame.
- Room surfaces: ~755/frame.
- Camera stage: ~3,819 cycles/frame.
- Playtest executable payload: 743,424 bytes.
- Display hash: `0xe50443fcf79947f3`.

### Deferred Options

Full world-space visibility, near/mid/far proxy geometry, and portal or
zone compression are valid later steps. They should wait until the
debug classification, global rejection, and budgeted chunk BFS prove the
basic chunk visibility model.

## Phase 2: Replace All-Cells Visibility

Goal: remove the O(n squared) generated visible-cell table.

Current problem:

- Each visibility anchor currently references every generated cell.
- This bloats generated manifests and does not provide meaningful
  culling.

New model:

- Store one compact record per populated cell.
- Store bounds, blocker mask, and portal/open-edge mask.
- At runtime, gather visible cells from the camera/player cell with a
  fixed-size queue.
- Traverse through open edges only.
- Apply a radius/depth cap.
- Frustum-test cell bounds before walking surfaces.

Optional later step:

- Cook compressed visibility zones or portal sets for expensive rooms.

Exit criteria:

- No per-anchor all-cell table is required.
- Visibility memory drops sharply.
- Room rendering walks fewer cells and surfaces per frame.

## Phase 3: One Cooked Geometry Store

Goal: eliminate duplicated world data.

Cooked chunk owns:

- vertex stream,
- surface stream,
- cell-to-surface ranges,
- material slot references,
- baked RGB,
- collision records.

Runtime cache owns:

- active chunk handles,
- decoded lightweight chunk views if needed,
- projected vertices for this frame,
- packet output.

The hot path should be:

```text
active chunks
  -> visible cells
    -> surface ranges
      -> packed vertices + baked RGB + material ID
        -> projected vertex cache
          -> packets
```

Rich `WorldSurfaceSample` reconstruction should remain only for tests,
debug tools, or transitional compatibility.

Exit criteria:

- Geometry is stored once in cooked chunk data.
- Runtime caches do not duplicate the world model.
- Cached rendering and uncached debug rendering agree visually.

## Phase 4: Cook-Time PS1 Geometry Preparation

Goal: stop doing expensive geometry repair at runtime.

Tasks:

- Pre-split oversized static surfaces during cook.
- Validate PS1 coordinate and primitive extent limits.
- Emit packet-friendly final surfaces.
- Preserve cell ownership after subdivision.
- Estimate final packet count per chunk.
- Fail or warn in the build report when a chunk cannot be made legal.

Exit criteria:

- Runtime triangle splitting is rare or removed.
- `MAX_TEXTURED_TRIS` pressure comes from real visible content, not
  avoidable runtime subdivision.

## Phase 5: Cross-Chunk Lighting

Goal: remove lighting seams at chunk boundaries.

New rule:

Lights are world-space influence volumes. They are not owned only by the
chunk containing their origin.

Tasks:

- Bake each chunk using every static light whose radius intersects the
  chunk bounds.
- Emit dynamic lights into every affected chunk, or into a separate
  spatial light grid.
- Sample actor lights from active nearby lights, not only current chunk
  lights.
- Add seam tests where a light near a boundary affects both sides.

Exit criteria:

- Lights crossing chunk boundaries bake correctly.
- Dynamic actors near chunk boundaries receive consistent lighting.

## Phase 6: Runtime Chunk Streamer

Goal: replace bounding-box-touching room loading with explicit chunk
streaming.

Runtime state:

```text
current chunk
visible chunks
warm chunks
resident RAM assets
resident VRAM assets
pending loads
pending evictions
```

Policy:

- Current chunk is required.
- Portal/neighbour chunks are warm.
- Visible-linked chunks are active.
- Distant chunks are evictable.
- Shared assets are retained by active/warm reference count.
- Texture pages can be pinned when they are common enough.

Initial backing store:

- Embedded bytes through the same storage API.

Later backing store:

- Stream pack records with pack ID, offset, compressed size if any, and
  decoded size.
- CD reader fills RAM pages behind the same storage API.

Exit criteria:

- Runtime can swap active chunk sets without rebuilding the world.
- Shared textures survive chunk transitions when still referenced.
- The same code path can use embedded bytes or streamed bytes.

## Phase 7: Collision And Entity Migration

Goal: chunked levels behave like normal levels.

Tasks:

- Use current plus neighbour chunk collision for player and camera.
- Restore camera collision in chunked levels.
- Keep static entities chunk-owned.
- Store dynamic entities in world space.
- Migrate dynamic entities between chunk ownership sets.
- Avoid full reload when the player crosses a chunk boundary.

Exit criteria:

- Chunked demo3 supports movement, camera collision, entities, and
  chunk crossing without special disabling.

## Phase 8: Stream Pack And ISO Backend

Goal: add real disc-backed loading after the logical model is clean.

Prerequisites:

- Chunk manifest exists.
- Asset table is global.
- Residency manager can request RAM and VRAM assets by ID.
- Embedded backing store and streamed backing store share an API.

Tasks:

- Define stream pack index records.
- Pack nearby chunks and first-use assets together.
- Keep asset residency independent of physical pack layout.
- Build a tiny ISO proof first.
- Read one asset from CD into a RAM page.
- Parse that asset through the existing asset loader.
- Add emulator and hardware-oriented diagnostics for read latency.

Exit criteria:

- Disc I/O can feed the same asset storage API.
- Stream packs are a disk layout optimization, not a gameplay data
  ownership model.

## Phase 9: Editor Tooling

Goal: make PS1 constraints visible without taking away freeform room
authoring.

Tasks:

- Add chunk overlay.
- Show per-chunk bytes, surface count, packet estimate, texture refs,
  static lights, dynamic light influences, and visibility cell count.
- Show active and warm chunk preview from a chosen camera/player point.
- Warn about over-budget chunks.
- Warn about likely lighting seams.
- Show shared asset residency impact.

Exit criteria:

- A designer can author large rooms and see exactly how they cook.
- Performance and memory problems are visible before launching.

## Recommended Implementation Order

1. Fix lighting correctness and cached-surface equivalence.
2. Add telemetry and build-report counters.
3. Change chunk target policy from hard-cap chunks to performance chunks.
4. Replace all-cells visibility with runtime cell traversal.
5. Add explicit chunk manifest records.
6. Add global shared asset table and refcount-style residency.
7. Move to one cooked geometry store.
8. Pre-split PS1-unsafe geometry at cook time.
9. Fix cross-chunk static and dynamic lighting.
10. Replace active room window with active chunk streamer.
11. Restore collision and entity migration across chunks.
12. Do the tiny ISO/CD proof-of-life.
13. Build production stream packs after the embedded path is clean.

## Non-Negotiables

- Runtime chunks are canonical.
- Shared assets are global.
- Cooked geometry is stored once.
- Runtime caches are transient.
- Visibility data must not be O(n squared) by default.
- CD streaming must use the same asset API as embedded data.
- Editor authoring must remain flexible.
