# TR5 PSX performance and architecture survey for PSoXide

Date: 2026-07-23

## Scope and source integrity

This report compares the PlayStation-specific renderer and game-runtime
architecture in TR5 with the current PSoXide engine, then ranks every material
technique found in that review as:

- adopt or prototype;
- already present;
- useful only when a stated scaling condition is met; or
- deliberately reject.

The TR5 source is pinned to commit
[`6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667`](https://github.com/TOMB5/TOMB5/tree/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667).
Renderer claims come from the actual `SPEC_PSX/*.MIP` MIPS assembly and
PlayStation C, not from the PC renderer. Some files under `GAME/` contain
reconstructed C; those are used only where the implementation is present and
readable, and are marked as architectural rather than cycle-exact evidence.

The PSoXide comparison is against branch `codex/tomb-raider-tessellation` as it
existed on 2026-07-23. This report does not claim that an unimplemented
candidate has been performance-tested.

## Verification vocabulary

| Grade | Meaning |
|---|---|
| Source-verified | The behavior is visible in the pinned TR5 implementation. |
| Code-verified | The current PSoXide implementation or gap was inspected directly. |
| Runtime-profiled | The relevant current PSoXide path was measured in Cortex v1. |
| A/B-tested | Competing PSoXide implementations were run through the same test route. |
| Proposed | The change is not implemented, so any expected gain remains a hypothesis. |
| Hardware-gated | Emulator results are insufficient; a real PS1/PS2 burn is required. |

## Executive result

The immediate Cortex v1 problem is not a lack of generic GTE batching, portal
culling, or GPU overlap. It is the CPU work that dynamically generates and
submits the new room-subdivision leaves.

The matched gameplay route renders only 23 authored room surfaces and projects
38 unique room vertices, yet the room-surface draw stage costs **529,830 CPU
cycles per visual frame**. The micro-profile attributes **448,790 cycles, or
81.6% of that instrumented stage**, to surface submission. GTE use is only
**0.6% of the two-VBlank render budget**, and GPU/DMA wait is about **60 CPU
cycles per gameplay frame**. Therefore:

1. The highest-priority TR5 adaptation is its table-driven subdivision:
   precompute the one-level topology, project its nine unique vertices once,
   and reuse warmed four-leaf packet templates.
2. The next architectural wins are model LOD before skeleton work, a
   room-indexed visible-object/matrix stash, and per-room/top-three light
   selection. These matter more as scenes gain actors and dynamic props.
3. Scratchpad placement and empty-OT compaction are credible PS1-specific
   micro-optimizations, but must be ratified on hardware.
4. Full double buffering, more generic RTPT batching, code overlays, and
   whole-level residency do not address the measured bottleneck and should not
   be adopted now.

## Cortex v1 evidence

### Reproducible normal build

The normal build used `cd-stream-bench emulator-telemetry` and this fixed input
route:

```text
0x4000@45+12,0x4000@80+16,0x4000@120+20,
0x4000@250+12,0x4000@400+12,0x4000@550+12
```

The run produced 900 frame markers, 359 visual frames, and 172 gameplay visual
frames with a non-zero active-room mask. The final display hash was
`0xabfd0c374fc59a8a`.

Gameplay-only means:

| Metric | Mean per gameplay visual frame | Maximum |
|---|---:|---:|
| Visual render task, including pacing | 1,415,454 | 1,510,719 |
| Frame cycles | 1,514,443 | 1,564,410 |
| Render stage | 985,589 | 1,141,744 |
| Present/VBlank wait | 429,083 | 567,552 |
| Room total | 585,341 | 585,491 |
| Room cell selection | 44,636 | 44,698 |
| Room unique-vertex projection | 7,283 | 7,304 |
| **Room surface draw** | **529,830** | **529,953** |
| Placed model instances | 42,178 | 191,011 |
| Player | 204,806 | 215,842 |
| World flush/sort | 43,529 | 49,919 |
| GPU/DMA wait | 60 | 68 |

The route considered exactly 23 authored surfaces and 38 projected room
vertices per gameplay frame. It emitted a mean 364 primitives/world commands
and peaked at 530.

The GTE profile recorded 194,261 operations over the run. Its estimated cost
was 6,379 cycles per visual frame, **0.6%** of the 1,127,706-cycle two-VBlank
budget.

### Focused room-surface micro-profile

The same project and input route were rebuilt with
`room-surface-profile`. The final display hash stayed identical. The profiling
build deliberately disables several warmed shortcuts so its absolute
room-surface cost rose from 529,830 to 549,980 cycles; use its substage ratios,
not its absolute total, to rank work.

| Instrumented room-surface component | Mean cycles | Share of instrumented stage |
|---|---:|---:|
| Material lookup | 11,020 | 2.0% |
| Projected-vertex fetch | 14,731 | 2.7% |
| Screen bounds | 2,389 | 0.4% |
| Surface-kind decode | 2,757 | 0.5% |
| Backface test | 3,121 | 0.6% |
| Lighting | 8,767 | 1.6% |
| **Submission/subdivision** | **448,790** | **81.6%** |
| Instrumentation/function overhead not assigned above | 58,406 | 10.6% |

The route contained 16 visible floor surfaces and five visible wall surfaces;
two surfaces were screen-culled before kind accounting, and no ceiling reached
the kind/submission stage. Consequently, this route proves where the current
submission time goes but does **not** measure the savings from disabling
ceiling subdivision.

### Existing one-level versus two-level A/B

An earlier matched route on this branch established:

| Mode | Render cycles/visual | Room surface per hit | Visual frames | Deadline misses | Result |
|---|---:|---:|---:|---:|---|
| One four-way level | 963,110 | 529,834 | 342 | 105 | Within two-VBlank render budget |
| Two four-way levels | 1,149,722 | 862,791 | 321 | 126 | Over budget |

This is why Cortex now uses one cyan subdivision band only. The second
magenta/near band is rejected for Cortex-sized tiles.

## Ranked recommendations

| Priority | Technique | Current disposition | Evidence |
|---|---|---|---|
| P0 | Table-driven, warmed one-level subdivision leaves | **Prototype next** | Source-verified, code-verified, runtime-profiled; proposed implementation |
| P0 | Surface-kind subdivision mask | **Add as a policy knob; default floor + wall** | Code-verified; ceiling saving not yet A/B-tested |
| P1 | Model MIP/LOD before joint calculation | **Adopt for model-heavy scenes** | Source-verified, code-verified gap; current Cortex impact is small |
| P1 | Per-frame visible-object and matrix stash | **Adopt when model count grows** | Source-verified, code-verified gap |
| P1 | Cooked per-room light ranges and top-three object lights | **Prototype after tessellation** | Source-verified, code-verified gap |
| P1 | Scratchpad hot working set | **Prototype behind an ownership API** | Source-verified, code-verified gap, hardware-gated |
| P2 | Empty ordering-table link compaction | **Hardware A/B only** | Source-verified, code-verified gap; emulator GPU wait is negligible |
| P2 | Active entity/effect lists and room-indexed ranges | **Adopt when counts exceed small fixed arrays** | Source-verified, code-verified partial gap |
| P2 | Fixed active-AI budget plus incremental LOT search | **Future navigation architecture** | Source-verified; PSoXide has no comparable pathfinder yet |
| P3 | Targeted sqrt/division lookup tables | **Profiler-triggered only** | Source-verified; current hot path does not justify blanket adoption |
| — | Portal clip traversal | Already present | Source- and code-verified |
| — | Roomlet/cell AABB rejection | Already present | Source- and code-verified |
| — | Indexed unique-vertex room projection | Already present | Code-verified and runtime-profiled |
| — | Scheduled RTPT triples | Already present | Code-verified; GTE has 99.4% estimated headroom |
| — | Packed face streams/direct OT packet linking | Already present | Source- and code-verified |
| — | Fixed packet arenas and overflow telemetry | Already present | Source- and code-verified |
| — | Baked lighting and once-per-object material lighting | Already present | Source- and code-verified |
| — | Blob shadow simplification | PSoXide is already cheaper | Code-verified |
| — | 30 Hz NPC thinking with scaled deltas | Already present | Code-verified |
| Reject | Full two-frame render buffering | Do not adopt | Adds latency; current DMA wait is negligible |
| Reject | Generic additional GTE batching | Do not prioritize | Current GTE cost is 0.6% of budget |
| Reject | Code overlays as a frame-rate fix | Do not adopt for this goal | Saves RAM, not measured frame time |
| Reject | Whole-level texture/room residency | Do not regress | PSoXide streaming is more capable |
| Reject | Wholesale assembly rewrite or TR5 engine rebase | Do not do | The useful architecture can be ported selectively |

## Detailed findings

### 1. Table-driven one-level subdivision and warmed leaf packets

**TR5 evidence.** TR5 embeds quad and triangle vertex tables at the start of
[`ROOMLETB.MIP`](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/SPEC_PSX/ROOMLETB.MIP#L5-L31).
`InitSubdivision` prepares interpolation state and `SubPolyGT4` walks the
predefined topology rather than rebuilding an arbitrary mesh
([lines 506-688](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/SPEC_PSX/ROOMLETB.MIP#L506-L688)).
The room loop selects the subdivision path and submits the resulting GT4
packets directly
([lines 1062-1137](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/SPEC_PSX/ROOMLETB.MIP#L1062-L1137)).

**Current PSoXide.** One-level subdivision is correctly enabled in
[`runtime_config.rs`](../engine/examples/editor-playtest/src/runtime_config.rs#L247-L255).
However, a subdividing quad cannot use the warmed authored-quad fast path:
[`tomb_raider_warmed_quad_requires_dynamic_submit`](../engine/crates/psx-engine/src/world_render/indexed_cache.rs#L1936)
forces it back to dynamic submission. The Gouraud subdivision path constructs
edge midpoints and a center, then recursively projects/submits four children in
[`world_pass_gouraud.rs`](../engine/crates/psx-engine/src/render3d/world_pass_gouraud.rs).

**Proposed shape.**

- Cook or prewarm one fixed one-level topology per authored quad:
  four corners, four edge midpoints, and one center.
- Project those **nine unique camera-space vertices once**, as three RTPT
  operations.
- Reuse four warmed GT4 packet templates containing immutable UVs, material
  words, winding, and baked/interpolated color.
- Patch only projected XY/depth, the OT link, and any animated UV state.
- Preserve the existing dynamic path for near-plane clipping, animated or
  translucent exceptions, diagnostic colors, and malformed/capacity fallback.
- Record a counter for warmed-subdivision hits, dynamic fallbacks, leaf
  packets, and overflows.

This is the only recommendation directly aligned with the measured 448,790
cycle submission hotspot. The gain is not claimed until an A/B build exists.

### 2. Surface-kind subdivision policy

The current `CachedRoomSubdivisionMode::All` permits floors, ceilings, and
walls to subdivide. The actual depth gate is camera-space: a root subdivides
only when its farthest projected vertex is inside `far_depth`. Cortex uses a
1,664-unit sector, so the first-level threshold is `1,664 × 5 = 8,320`.

Add a compact surface-kind mask independent from the existing
`All`/`DepthSorted`/`Risky` mode:

```text
floor   enabled
wall    enabled
ceiling disabled by default
```

Floors and walls are the most visible affine-error cases for the normal
third-person camera. Ceiling subdivision should remain opt-in for low-ceiling
or upward-looking scenes. This is a safe policy seam, but Cortex's current
benchmark route has no submitted ceilings, so a separate ceiling-heavy visual
and performance tape is required before claiming a gain.

### 3. Model MIP/LOD selection before skeleton and mesh work

**TR5 evidence.** Animated objects select a MIP object from depth before the
bounding-box and joint work
([`ANIMITEM.MIP` lines 337-376](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/SPEC_PSX/ANIMITEM.MIP#L337-L376)).
Static objects use the same descriptor-switch idea. The `object_mip` field is
part of the PSX object type
([`STYPES.H` lines 66-82](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/SPEC_PSX/STYPES.H#L66-L82)),
and setup assigns thresholds
([`SETUP.C` lines 900-950](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/GAME/SETUP.C#L900-L950)).

**Current PSoXide.** Bounds rejection already happens before full drawing, but
`LevelModelRecord` carries one mesh/atlas rather than a near/far pair. Add an
optional LOD model reference plus switch/hysteresis distances, resolve it after
the cheap object bounds test, and only then calculate joints/project vertices.
Do not switch the camera-close player model.

The current Cortex route has at most one placed model and spends about 42,178
cycles/frame on placed model instances, so this is an architectural scaling
feature rather than the first Cortex fix.

### 4. Visible-object list and matrix stash

**TR5 evidence.** `stash_the_info` stores the selected mesh pointer and GTE
matrix
([`ANIMITEM.MIP` lines 512-536](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/SPEC_PSX/ANIMITEM.MIP#L512-L536)).
`CalcAllAnimatingItems_ASM` walks only draw-room item/static lists, culls and
calculates visible objects, then `DrawAllAnimatingItems_ASM` consumes the stash
([lines 538-801](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/SPEC_PSX/ANIMITEM.MIP#L538-L801)).

**Current PSoXide.** Placed model and shadow paths scan the global
`model_instances` table, filter by room, and repeat work across shadow,
behind-player, and in-front-player passes
([`instances.rs`](../engine/crates/psx-game-runtime/src/model_rendering/instances.rs#L105-L148)).

Cook contiguous per-room instance ranges, then build one fixed-capacity
visible draw list per frame. Each entry should contain the instance index,
selected LOD, phase, origin, bounds result, material/light result, and pose or
joint-matrix handle. Shadows and both depth passes consume that list. This
removes repeated global scans and creates one place to enforce object budgets.

### 5. Per-room light ranges and top-three object lights

**TR5 evidence.** `S_CalculateLight` keeps a small scratchpad result and ranks a
bounded set of relevant lights for the object
([`LIGHT.MIP` lines 21-170](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/SPEC_PSX/LIGHT.MIP#L21-L170)).
The later selection logic retains three light records rather than carrying the
entire room list through mesh work.

**Current PSoXide.** `RuntimeRoomLighting::point_lights` scans the whole level
light slice and filters `light.room == room_index` for each shading call
([`room_lighting.rs`](../engine/crates/psx-game-runtime/src/room_lighting.rs#L131-L230)).
The Cortex manifest currently contains 24 lights. Baked room vertices bypass
most of this cost, but placed models and unbaked surfaces still pay it.

Cook lights grouped by room with `(first, count)` ranges. For each visible
model, cheaply reject by radius, rank at most the strongest/nearest three, and
shade all of that model's materials from the compact result. A squared-distance
prefilter should run before any integer square root. Profile model-heavy and
unbaked-light scenes before deciding whether the top-three cap is visually
acceptable.

### 6. Scratchpad ownership for hot working sets

**TR5 evidence.** TR5 repeatedly uses the PS1's 1 KiB scratchpad:

- portal traversal queue/state in
  [`MATHS.MIP` lines 1823-1875](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/SPEC_PSX/MATHS.MIP#L1823-L1875);
- room subdivision/interpolation in
  [`ROOMLETB.MIP`](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/SPEC_PSX/ROOMLETB.MIP#L688-L706);
- object matrices and hot pointers in
  [`ANIMITEM.MIP`](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/SPEC_PSX/ANIMITEM.MIP#L538-L596);
- compact lighting state in
  [`LIGHT.MIP`](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/SPEC_PSX/LIGHT.MIP#L21-L40).

**Current PSoXide.** Production room/model code does not use scratchpad RAM.
Introduce one explicit non-reentrant scratchpad lease with typed regions, not
ad-hoc hard-coded addresses. First candidates are the nine unique tessellation
vertices, four leaf descriptors, a small portal queue, or the three selected
lights.

This is hardware-gated: emulator cycle models may not reproduce scratchpad
latency or main-RAM contention, and ownership must be safe across interrupts,
debug paths, and nested renderer calls.

### 7. Empty ordering-table link compaction

**TR5 evidence.** `OptimiseOTagR` rewires runs of empty OT entries before the
draw
([`SHADOWS.MIP` lines 161-195](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/SPEC_PSX/SHADOWS.MIP#L161-L195));
`GPU_EndScene` calls it before submission
([`GPU.C` lines 45-68](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/SPEC_PSX/GPU.C#L45-L68)).

**Current PSoXide.** The shipping playtest uses a 2,048-slot OT.
`OrderingTable::clear` links every slot to its predecessor, so DMA traverses
empty entries as zero-word packets
([`ot.rs`](../sdk/crates/psx-gpu/src/ot.rs#L37-L75)).

Track the lowest/highest occupied slot or occupied-slot bitset during
insertion, then compact empty runs before the asynchronous kick. This may save
DMA linked-list hops on hardware, but the emulator route reports only about 60
cycles/frame of OT wait. Do not spend CPU scanning all 2,048 entries; the
compactor must use occupancy data already maintained by the bucketed renderer.

### 8. Active entity/effect lists and per-room ownership

**TR5 evidence.** Items and effects live in fixed pools with free and active
linked lists
([`ITEMS.C` lines 120-222](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/GAME/ITEMS.C#L120-L222)).
The control phase advances only `next_item_active` rather than scanning every
level record.

**Current PSoXide.** `GameEntities::tick_delta` scans every entity record,
then tests state and performs a linear active-room membership query
([`entities.rs`](../engine/crates/psx-game-runtime/src/entities.rs#L637-L700)).
It already runs collision-heavy NPC thinking at 30 Hz with a two-tick delta,
which is a strong existing optimization.

For larger populations, add:

- an awake-entity bitset or packed index list;
- cooked per-room entity ranges for sleeping Idle/Patrol actors;
- O(1) active-room bit membership;
- a small free/active list for particle/effect pools above roughly 128 slots.

Do not add linked-list complexity for today's tiny fixed populations. Cortex's
game-logic stage is about 27,656 cycles/hit, far below the room renderer.

### 9. Fixed active-AI budget and incremental LOT search

**TR5 evidence.** TR5 caps active baddies to a small fixed set and evicts by
distance
([`LOT.C` lines 219-329](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/GAME/LOT.C#L219-L329)).
`UpdateLOT` passes an expansion budget into `SearchLOT`
([`BOX.C` lines 827-900](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/GAME/BOX.C#L827-L900)).

PSoXide currently has simple state/motor AI rather than a general pathfinder.
When navigation arrives, use a cooked sector/zone graph, fixed active-agent
slots, and an incremental BFS/A* expansion budget per visual frame. Never let a
single path request consume an unbounded frame.

### 10. Portal rectangles and bounded room traversal

**TR5 evidence.** `DrawRooms` initializes screen bounds and calls the room
bound traversal
([`DRAWPHAS.C` lines 206-255](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/SPEC_PSX/DRAWPHAS.C#L206-L255)).
`GetRoomBoundsAsm` uses a bounded scratchpad queue and propagates clipped
portal rectangles
([`MATHS.MIP` lines 1823-2323](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/SPEC_PSX/MATHS.MIP#L1823-L2323)).

PSoXide already performs recursive portal-frustum traversal, supports multiple
frustums per room, and rejects redundant frustums in
[`portal_visibility.rs`](../engine/crates/psx-level/src/portal_visibility.rs#L499-L740).
The benchmark sees one drawn room and two portal tests; portal visibility is
not the current bottleneck.

### 11. Roomlets and layered bounding rejection

**TR5 evidence.** `DrawRoomletListAsm` walks only draw rooms, performs coarse
room rejection, transforms each roomlet AABB, and then draws accepted roomlets
([`ROOMLETB.MIP` lines 1178-1515](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/SPEC_PSX/ROOMLETB.MIP#L1178-L1515)).
The readable `ROOMLET` type stores bounds and compact vertex/face offsets
([`SPECTYPES.H` lines 472-494](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/SPEC_PC_N/SPECTYPES.H#L472-L494)).

PSoXide's cooked populated cells already provide the analogous compact AABB
layer. The route considers 79 cells, rejects 62, and draws 17. More roomlet
hierarchy would add overhead to this small accepted set and is not indicated.

### 12. Unique vertex projection and RTPT scheduling

PSoXide already collects unique cached room vertex indices and projects them
once before walking faces
([`indexed_cache.rs`](../engine/crates/psx-engine/src/world_render/indexed_cache.rs#L299-L475)).
Model projection already pipelines RTPT triples and groups skinning work by
joint. TR5 likewise schedules useful CPU work around raw GTE operations in its
roomlet and object loops.

The current route projects only 38 room vertices and estimates 99.4% GTE
headroom. Generic additional RTPT batching is therefore rejected. The
tessellation recommendation still uses three RTPTs because it removes repeated
CPU projection and packet work for nine known vertices; it is not an attempt
to cure GTE saturation.

### 13. Packed face streams, specialized loops, and direct OT links

TR5 decodes packed face indices, runs NCLIP/AVSZ, performs depth rejection, and
links fixed-size GT3/GT4 packets directly into the OT
([`DRAWOBJ.MIP` lines 1850-2011](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/SPEC_PSX/DRAWOBJ.MIP#L1850-L2011)).

PSoXide already has predecoded packed model faces, extent-safe specialized
branches, comparison-free bucketed insertion, and a MIPS packed reverse-link
loop in
[`ot.rs`](../sdk/crates/psx-gpu/src/ot.rs#L120-L210). Rewriting these paths in
more assembly is not justified by the profile.

### 14. Fixed arenas and hard overflow behavior

TR5 uses fixed polygon buffers and checks the packet end before advancing.
PSoXide already uses fixed primitive/command arenas, counts remaining slots,
and exposes overflow telemetry
([`render.rs`](../engine/crates/psx-engine/src/render.rs#L481-L629)).
The benchmark reports ample primitive capacity and no room primitive
overflows. Preserve this architecture.

### 15. Baked and once-per-object lighting

TR5 calculates object lighting once after restoring a stashed object matrix,
then draws its meshes. PSoXide shades a placed model material once per
instance/layer and uses baked room vertex RGB directly when fog permits.
This is already the right architecture.

The missing scaling piece is not "more baked light"; it is the per-room/top-
three light selection described above.

### 16. Simplified shadows

TR5 contains a dedicated projected shadow renderer. PSoXide currently draws
each actor shadow as one subtract-blended textured quad
([`instances.rs`](../engine/crates/psx-game-runtime/src/model_rendering/instances.rs#L151-L190)).
That is already cheaper than a multi-segment projected silhouette. No TR5
change is recommended.

### 17. Double polygon buffers and asynchronous GPU submission

TR5 keeps two OTs/polygon buffers and flips after sync/VBlank
([`GPU.C` lines 20-119](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/SPEC_PSX/GPU.C#L20-L119)).
PSoXide already kicks its OT asynchronously and drains it at presentation
([`render.rs`](../engine/crates/psx-engine/src/render.rs#L430-L476)).

A full extra frame of render buffering would add roughly 33 ms of input
latency at 30 Hz. With only about 60 cycles/frame of measured GPU/DMA wait,
there is no throughput evidence to justify that latency. Keep the current
single-frame async overlap.

### 18. Targeted sqrt/division lookup tables

TR5 ships `SqrtTable`, `DIV3TAB`, and `DIV4TAB`
([`LOAD_LEV.C` lines 29-58](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/SPEC_PSX/LOAD_LEV.C#L29-L58)).
PSoXide point-light falloff currently calls an integer square root for every
accepted light.

Do not replace general math blindly. First cook per-room light ranges and
profile model lighting. If square root remains hot, use a bounded table or a
visually tested squared/Manhattan falloff approximation only in that lighting
path.

### 19. Code overlays

TR5 loads and relocates `SETUP.MOD` into memory reused during level setup
([`ROOMLOAD.C` lines 53-114](https://github.com/TOMB5/TOMB5/blob/6abe6b2975ca5abf50e6df0a4a80ed84ca8d2667/SPEC_PSX/ROOMLOAD.C#L53-L114)).
This is a RAM-footprint technique, not a frame-time technique. PSoXide already
streams room payloads and manages VRAM residency; an overlay system should be
considered only if executable RAM, not frame time, becomes the blocker.

### 20. Streaming and residency

TR5's reviewed room renderer assumes a mostly level-resident static data set.
PSoXide already has active-room pinning, LRU slots, prefetch, failure backoff,
and separate RAM/VRAM residency in
[`room_streaming.rs`](../engine/crates/psx-game-runtime/src/room_streaming.rs).
Do not regress to whole-level residency.

### 21. 30 Hz simulation cadence

PSoXide already runs collision-heavy NPC thinking at 30 Hz with
`delta_ticks = 2` while retaining 60 Hz player/combat and logic work
([`playtest_update.rs`](../engine/examples/editor-playtest/src/playtest_update.rs#L35-L100)).
This captures the relevant fixed-budget console scheduling pattern. Further
cadence cuts should be driven by per-system telemetry.

## Recommended implementation sequence

1. **Instrument the intended win.** Add counters for authored subdivision
   roots, warmed one-level hits, dynamic fallbacks, generated unique vertices,
   leaf packets, and capacity failures.
2. **Build the fixed one-level leaf cache.** Nine unique vertices, four GT4
   packets, static UV/material/color interpolation, dynamic position/depth
   patching.
3. **A/B Cortex v1.** Use the exact 900-marker route above. Require unchanged
   display hash or an eyes-on equivalent image, no new overflows, lower room
   submission cycles, and no increase in deadline misses.
4. **Add a surface-kind mask.** Test floors+walls against all surfaces using a
   separate low-ceiling/upward-camera tape before changing the project default.
5. **Cook room-indexed instance and light ranges.** Add top-three light
   selection and reuse the visible-object list across shadows/depth passes.
6. **Add model LOD records.** Verify hysteresis and player exclusion with a
   model-heavy test scene.
7. **Hardware-only experiments.** Scratchpad lease first, then OT empty-run
   compaction. Keep each behind a compile-time feature until real-hardware
   cycle and stability burns pass.

## Acceptance gates

For every renderer candidate:

- `cargo test -p psx-engine -p psx-game-runtime -p psx-level` remains green;
- the fixed Cortex v1 route reaches gameplay, not the title/loading screen;
- display hash or screenshot comparison proves visual equivalence;
- no primitive, command, stream, or cache overflow counter regresses;
- gameplay-only stage means and maxima are reported separately from
  title/loading frames;
- the normal, non-profiler build is used for the final performance number;
- any scratchpad or DMA claim is repeated on real hardware.

For LOD and surface-kind policy, visual equivalence is not expected to be
bit-identical; instead require an explicit eyes-on gate and stable transitions.

## Source files reviewed

TR5 PSX implementation:

- `SPEC_PSX/ROOMLETB.MIP`
- `SPEC_PSX/MATHS.MIP`
- `SPEC_PSX/DRAWPHAS.C`
- `SPEC_PSX/ANIMITEM.MIP`
- `SPEC_PSX/DRAWOBJ.MIP`
- `SPEC_PSX/LIGHT.MIP`
- `SPEC_PSX/GPU.C`
- `SPEC_PSX/SHADOWS.MIP`
- `SPEC_PSX/LOAD_LEV.C`
- `SPEC_PSX/ROOMLOAD.C`
- `SPEC_PSX/STYPES.H`
- `SPEC_PC_N/SPECTYPES.H`
- `GAME/ITEMS.C`
- `GAME/CONTROL.C`
- `GAME/LOT.C`
- `GAME/BOX.C`
- `GAME/SETUP.C`

Primary PSoXide comparison areas:

- `engine/crates/psx-engine/src/world_render.rs`
- `engine/crates/psx-engine/src/world_render/indexed_cache.rs`
- `engine/crates/psx-engine/src/render3d.rs`
- `engine/crates/psx-engine/src/render3d/world_pass*.rs`
- `engine/crates/psx-engine/src/render.rs`
- `sdk/crates/psx-gpu/src/ot.rs`
- `engine/crates/psx-level/src/portal_visibility.rs`
- `engine/crates/psx-game-runtime/src/model_rendering*.rs`
- `engine/crates/psx-game-runtime/src/room_lighting.rs`
- `engine/crates/psx-game-runtime/src/entities.rs`
- `engine/crates/psx-game-runtime/src/room_streaming.rs`
- `engine/examples/editor-playtest/src/runtime_config.rs`
- `engine/examples/editor-playtest/src/playtest_scene.rs`
- `engine/examples/editor-playtest/src/playtest_update.rs`

## Commands and test artifacts

Focused profiling build:

```sh
cd emu
EDITOR_PLAYTEST_FEATURES='cd-stream-bench emulator-telemetry room-surface-profile' \
  cargo run -p frontend --release -- build-project-disc \
  --project ../editor/projects/cortex_v1
```

Normal build restoration:

```sh
cd emu
EDITOR_PLAYTEST_FEATURES='cd-stream-bench emulator-telemetry' \
  cargo run -p frontend --release -- build-project-disc \
  --project ../editor/projects/cortex_v1
```

Matched route:

```sh
target/release/frontend launch \
  --path editor/projects/cortex_v1/baked/cortex_v1.cue \
  --embedded-playtest \
  --pad-pulses '0x4000@45+12,0x4000@80+16,0x4000@120+20,0x4000@250+12,0x4000@400+12,0x4000@550+12' \
  --guest-frames 900 \
  --steps 2000000000 \
  --profile-log /tmp/tr5-survey-cortex-v1-normal-profile.csv \
  --counter-log /tmp/tr5-survey-cortex-v1-normal-counters.csv \
  --dump-hw /tmp/tr5-survey-cortex-v1-normal-final.ppm \
  --dump-hash \
  --dump-guest-profile
```

Local artifacts:

- `/tmp/tr5-survey-cortex-v1-normal.log`
- `/tmp/tr5-survey-cortex-v1-normal-profile.csv`
- `/tmp/tr5-survey-cortex-v1-normal-final.ppm`
- `/tmp/tr5-survey-cortex-v1-room.log`
- `/tmp/tr5-survey-cortex-v1-room-profile.csv`
- `/tmp/tr5-survey-cortex-v1-room-final.png`

Regression command:

```sh
cd engine
cargo test -p psx-engine -p psx-game-runtime -p psx-level
```

Result on the surveyed worktree:

- `psx-engine`: 261 unit tests passed;
- `psx-engine` time-type compile-fail guard: 1 passed;
- `psx-game-runtime`: 61 unit tests passed;
- `psx-level`: 41 unit tests passed;
- no failures;
- four `psx-engine` documentation examples remain intentionally ignored.
