# Copy-Paste Prompt For Worker 2: Demo3 Tile Visibility And PVS

Copy everything in this file into Worker 2.

```text
You are Worker 2 on the PSoXide demo3 performance pass.

Your assignment is issue 2 only: improve camera/tile-aware visibility and PVS so demo3 renders a better set of in-view geometry with fewer wasted room cells/surfaces/packets.

You are not alone in the codebase. Worker 1 is working separately on moving room surface cache build work out of the frame path. Do not revert their edits. Avoid editing Worker 1-owned cache build/storage code unless a tiny call-site adaptation is unavoidable.

Repository:
/Users/ebonura/Desktop/repos/PSoXide

Before doing architecture/codebase work, read:
sed -n '1,180p' AGENTS.md
sed -n '1,180p' graphify-out/GRAPH_REPORT.md

Useful graph query:
graphify query "How do editor-playtest room surface caching, generated chunk visibility, and residency/streaming relate in this codebase?"

After modifying code files, run:
graphify update .

Use rg for search. Use apply_patch for manual edits. Do not use destructive git commands. Do not commit or push. Do not revert files you did not intentionally change.

Known dirty files at orchestration time, unrelated to this performance pass:
editor/crates/psxed-project/src/lib.rs
editor/crates/psxed-ui/src/lib.rs

Treat those as user/local work. Do not revert them. If your task genuinely requires touching either file, explain that in your final handoff and keep the edit narrowly scoped.

Baseline benchmark target:
make cook-playtest PROJECT=projects/demo3/project.ron
make build-editor-playtest
cd emu && ./target/release/frontend launch \
  --path ../build/examples/mipsel-sony-psx/release/editor-playtest.exe \
  --guest-frames 60 \
  --steps 120000000 \
  --dump-hw /tmp/psoxide-demo3-worker2-pvs-check.ppm \
  --dump-hash \
  --dump-guest-profile

Baseline demo3 hashes:
display_fnv1a_64=0x3c4e18c498e13da0
vram_fnv1a_64=0x7725d623d3394114
screenshot=/tmp/psoxide-demo3-baseline-restored-60.png

Baseline demo3 profile:
render total       3,746,709 cycles/frame
room               2,989,336 cycles/frame
player               568,672 cycles/frame
world flush/sort     108,828 cycles/frame
update               117,579 cycles/frame
room cache build     280,105 cycles/frame average, 1,956,017 max hit

tri prims            1,294/frame
world commands        1,294/frame
room cells              405/frame
room cells drawn        238/frame
room cells culled       167/frame
room surfaces           745/frame
room chunks               4/frame
room cache builds        12 total during 60-frame capture

Current memory map findings:
PS1 RAM total: 2 MiB
EXE payload: 845,824 B / 826 KiB / 40.3%
.text:       190,456 B / 186 KiB /  9.1%
.data:       655,360 B / 640 KiB / 31.3%
.bss:        363,040 B / 354 KiB / 17.3%
heap+stack runway: about 803 KiB / 39.2%

Actual VRAM pressure is currently low:
Framebuffer: about 300 KiB
Room textures: about 4.8 KiB actual uploaded
Model atlas: about 16.5 KiB actual uploaded
Font + shadow: about 6 KiB

Architecture summary:
Demo3 is a geometry-heavy playtest level cooked into engine/examples/editor-playtest/generated/.

Current playtest path:
1. Editor/cooker creates room chunks and generated Rust manifest.
2. Runtime includes room/world/model/texture bytes through include_bytes!.
3. Runtime picks active chunk rooms near the player/camera.
4. Runtime builds or reads per-room surface caches.
5. Runtime uses generated visibility cell lists plus runtime camera/frustum tests to draw cached room surfaces.
6. World commands are sorted/flushed into the PS1 ordering table.

Your problem:
The current room visibility is still too broad and sometimes unstable. It selects too much geometry, and previous screenshots showed missing/oddly selected chunks. The world is quantized into cells/chunks, so visibility should exploit current chunk, local cell, yaw sector, frustum, and neighbour graph.

Primary goal:
Use camera/tile-aware visibility or a compact generated PVS so demo3 draws fewer wasted room cells/surfaces while preserving visible geometry. Prefer conservative false positives over missing floors/walls.

Baseline:
room cycles/frame:       2,989,336
room cells/frame:              405
room cells drawn/frame:        238
room surfaces/frame:           745
tri prims/frame:             1,294
room chunks/frame:               4

Target for first useful pass:
room cycles/frame below 2.4M without visual regression
room cells drawn meaningfully below 238/frame, or same cells with fewer submitted surfaces
tri prims below 1,100/frame if possible
no missing wall/floor holes in default demo3 screenshot
no obvious pop/stutter in 240-frame hold-forward run

Aggressive target:
room cycles/frame near or below 2.0M
room surfaces below about 550/frame
tri prims below about 1,000/frame

Your write ownership:
engine/examples/editor-playtest/src/main.rs
editor/crates/psxed-project/src/world_cook.rs
editor/crates/psxed-project/src/playtest.rs
editor/crates/psxed-project/src/playtest/schema.rs
editor/crates/psxed-project/src/playtest/manifest.rs
engine/crates/psx-level/src/lib.rs

In engine/examples/editor-playtest/src/main.rs, focus on:
fill_precomputed_visible_cells
cached_precomputed_visible_cells
active_room_camera_key
load_active_room_window
refresh_active_room_window_if_needed
chunk_activation_score
best_spatial_chunk_candidate
chunk_camera_metrics
chunk_bounds_current_space
room/chunk frustum checks
render-loop choice of visible cells

Avoid redesigning these Worker 1 areas:
active_room_surface_cache_for
cache_active_room_surfaces
CACHED_ROOM_* storage ownership
room surface cache generation/loading

Key files and landmarks:
engine/examples/editor-playtest/src/main.rs
  fill_precomputed_visible_cells
  cached_precomputed_visible_cells
  active_room_camera_key
  load_active_room_window
  refresh_active_room_window_if_needed
  chunk_activation_score
  best_spatial_chunk_candidate
  chunk_camera_metrics
  chunk_bounds_current_space

engine/crates/psx-level/src/lib.rs
  LevelChunkRecord
  LevelRoomVisibilityRecord
  LevelVisibilityCellRecord
  add visibility/PVS records here if needed

editor/crates/psxed-project/src/playtest/manifest.rs
  emits ROOM_CHUNKS
  emits ROOM_VISIBILITY
  emits VISIBILITY_CELLS
  add generated PVS tables here if needed

Current generated visibility data:
ROOM_VISIBILITY: one record per generated room/chunk
VISIBILITY_CELLS: list of cells for each room
LevelVisibilityCellRecord { room, x, z, min_y, max_y, portal_mask, blocker_mask, flags }

The user-observed visibility issues:
1. Geometry missing when looking around.
2. It does not render far enough when turning.
3. Earlier builds had chunks/geometry outside the intended view region always rendered.
4. The engine should focus on what is actually in view.
5. A possible approach is a precomputed list of offsets for each quantized camera/player position.

Suggested design:
Use a conservative two-stage visibility system.

Stage A: Runtime frustum refinement over existing generated visibility.
1. Keep existing ROOM_VISIBILITY / VISIBILITY_CELLS.
2. For each candidate visibility cell, compute a conservative world-space cell AABB or bounding sphere:
   x/z from cell coordinates and chunk origin
   sector_size from room/chunk
   min_y/max_y from LevelVisibilityCellRecord
   extra margin for walls/ceilings and PS1 ordering safety
3. Reject cells clearly outside the camera frustum.
4. Use a safety margin so walls/floors do not disappear at screen edges.
5. Never cull the player/current cell or immediately adjacent cells.
6. Prefer false positives over false negatives.

Stage B: Cooked quantized PVS, if Stage A alone is insufficient.
Add a compact generated table keyed by:
room/chunk index
observer cell x/z
camera yaw sector, probably 8 or 16 sectors

Each key maps to a short list of cell refs or cell ranges. Runtime chooses:
current generated room/chunk
current camera/player local cell
quantized yaw sector

then draws only that PVS list after Stage A frustum refinement.

Suggested records:
pub struct LevelVisibilityViewRecord {
    pub room: RoomIndex,
    pub cell_x: u16,
    pub cell_z: u16,
    pub yaw_sector: u8,
    pub first_ref: VisibilityCellIndex,
    pub ref_count: u16,
    pub flags: u16,
}

pub struct LevelVisibilityCellRefRecord {
    pub room: RoomIndex,
    pub x: u16,
    pub z: u16,
}

or use indices into VISIBILITY_CELLS if that is smaller and simpler.

Keep the schema compact. Avoid heap allocations. Avoid duplicating large full LevelVisibilityCellRecord entries per view if index refs are enough.

PVS heuristics:
The first PVS does not need perfect occlusion. It needs a conservative, stable win.

Good first heuristic:
1. Include current cell and neighbour ring unconditionally.
2. Include cells in a forward wedge from camera yaw.
3. Include cells behind the player only in a short radius.
4. Include chunks connected through neighbour graph if their bounds intersect the frustum.
5. Expand the wedge/radius near boundaries to prevent turn/movement popping.
6. Keep far cells if they project within the frustum and are not blocked by simple wall/portal metadata.

Use these generated fields where useful:
portal_mask
blocker_mask
flags
chunk neighbours north/east/south/west
room/chunk origin_x/origin_z/width/depth/sector_size

Ordering safety:
Do not fix visibility by dropping geometry that happens to draw in the wrong order. Visibility should answer: could this cell/surface be visible from this camera region? Ordering should remain handled by depth bands/OT sorting. If an ordering bug appears, report it separately and do not hide it with culling.

Tests to add:
1. PVS for a cell always includes itself.
2. PVS includes immediate neighbours or a safety ring.
3. PVS yaw sectors are stable at sector boundaries.
4. Frustum filter does not reject a cell whose AABB intersects the near screen area.
5. Generated PVS tables have valid offsets/counts.
6. Demo3 cook emits non-empty PVS/view records if you add generated PVS tables.

Useful test files:
editor/crates/psxed-project/src/playtest.rs
engine/examples/editor-playtest/src/main.rs unit tests if available/appropriate
engine/crates/psx-level/src/lib.rs tests for record bounds

Run at least:
cargo test --manifest-path engine/crates/psx-engine/Cargo.toml world_render render3d
cargo test --manifest-path editor/Cargo.toml -p psxed-project playtest
make cook-playtest PROJECT=projects/demo3/project.ron
make build-editor-playtest
cd emu && ./target/release/frontend launch \
  --path ../build/examples/mipsel-sony-psx/release/editor-playtest.exe \
  --guest-frames 60 \
  --steps 120000000 \
  --dump-hw /tmp/psoxide-demo3-worker2-pvs-check.ppm \
  --dump-hash \
  --dump-guest-profile

Because this task affects visibility/camera/active rooms, also run:
cd emu && ./target/release/frontend launch \
  --path ../build/examples/mipsel-sony-psx/release/editor-playtest.exe \
  --guest-frames 240 \
  --steps 480000000 \
  --hold-forward \
  --dump-hw /tmp/psoxide-demo3-worker2-pvs-forward-check.ppm \
  --dump-hash \
  --dump-guest-profile

Report:
display hash
vram hash
screenshot path
render total cycles/frame
room cycles/frame
world flush/sort cycles/frame
tri prims/frame
room cells/frame
room cells drawn/frame
room cells culled/frame
room surfaces/frame
room chunks/frame
room chunks considered/frame
any visibility fallback counters

If your hash changes, include screenshot paths and explain visible differences. If visibility/PVS changes make more appropriate geometry visible, hash changes are acceptable, but you must explain what changed and verify there are no obvious holes/missing walls/floors in default demo3 and the 240-frame hold-forward capture.

Final handoff format:
Worker: 2
Goal:

Changed files:
- ...

Implementation summary:
- ...

Tests run:
- ...

Demo3 default:
- display hash:
- vram hash:
- screenshot:
- render total cycles/frame:
- room cycles/frame:
- world flush/sort cycles/frame:
- tri prims/frame:
- room cells drawn/frame:
- room surfaces/frame:

Demo3 forward:
- display hash:
- vram hash:
- screenshot:
- render total cycles/frame:
- room cycles/frame:
- visible issues:

Known risks:
- ...

Merge notes:
- ...
```
