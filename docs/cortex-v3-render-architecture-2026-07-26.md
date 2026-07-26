# cortex_v3 render architecture: diagnosis and the smallest visual-preserving fix

Target: every gameplay frame inside 3 NTSC vblanks (stable 20 FPS) on original
PlayStation hardware, with pixel-identical output.

Repo `/Users/ebonura/Desktop/repos/PSoXide`, branch `emu/accuracy-from-silicon`,
commit `0bab5cd8`.

All numbers below were recomputed from the supplied CSVs and from the shipped
MIPS link map (`/tmp/cortex-v3-final.map`). Where I use a figure from the brief
without re-deriving it, I say so.

---

## A. Executive verdict

**The renderer's cost is not proportional to what it draws. It is proportional
to how many cached surface records it walks past.** Every surface pays a fixed
decision tax of roughly **5,600 to 7,000 cycles** before a single primitive is
emitted, and cortex_v3's cell granularity feeds that loop about **5.7x more
surfaces per frame** than cortex_v1 while producing only **1.76x the triangles**.

The two-point cost fit is unambiguous. Modelling
`room_surface_draw = a·surfaces_considered + b·primitives_emitted`:

| Fit source | a (per surface) | b (per primitive) |
|---|---:|---:|
| cortex_v1 gameplay vs cortex_v3 gameplay (different projects, same binary family) | **5,608** | 743 |
| cortex_v3 clean HEAD vs cortex_v3 corrected (same project, same binary) | **7,020** | 439 |

Two independent fits, one cross-project and one within-project, land within 25%
of each other on `a`. At 88.9 surfaces per rendered room frame that is
**499k–624k cycles of pure per-surface overhead out of 804.6k**, i.e. **62–78%
of `room_surface_draw`, and 36–44% of the entire render stage**, spent deciding
what a surface is rather than drawing it.

The corroborating evidence is the strongest single fact in the dataset: the
portal-union cull removed 26% of surfaces (120.5 → 88.9) and **changed the
triangle count by 0.7 out of 412** (411.6 → 412.3). Thirty-two surfaces per
frame were being fully classified, material-resolved, option-constructed,
backface-tested and then discarded, at a measured **7,020 cycles each**. They
contributed exactly zero pixels. That workload is still there for the remaining
surfaces that survive to backface/screen rejection.

Why the tax is that large is visible in the link map:
`draw_indexed_cached_room_surface` is fully inlined into
`draw_indexed_cached_room_vertex_lit_visible_cells`, which compiles to
**0x8aa0 = 35,488 bytes**. The R3000A instruction cache is **4,096 bytes,
direct-mapped**. The inner loop body is **8.66x the entire I-cache**. It
re-derives, per surface per frame, facts that are static properties of the
surface: `cached_surface_kind`, the material slot lookup, `is_animated`,
`is_translucent`, `cached_uv_material`, `wall_material_for_direction`,
`warmed_room_quad_ready_value`, and a freshly constructed `WorldSurfaceOptions`
from `cached_surface_subdivision_options` / `triangle_depth_options` /
`horizontal_depth_options`.

**The mistaken architectural assumption is that a cached room surface is a
*record to be interpreted at draw time*.** It should be a *precompiled draw
command* whose only camera-dependent inputs are four projected vertices, one
depth span, and one backface sign.

---

## B. Verified facts vs assumptions

### Established by the supplied measurements

| Fact | Value | Source |
|---|---|---|
| Profile rows are one per **sim tick**, not per vblank | `sim_ticks == 1` every row | recomputed |
| `visual_render_task == render + present` | 1,404,382 + 305,407 = 1,709,789 vs measured 1,710,415 | recomputed |
| Gameplay effective FPS, corrected build | **14.01** (384 visual / 1,644 ticks) | recomputed |
| Gameplay render stage mean | **1,404,382** cyc; p50 1,394,686; p90 1,840,485; p99 2,215,465; max 2,263,382 | recomputed |
| `room_surface_draw` share of render | **57.3%** (804,618 / 1,404,382) | recomputed |
| Surfaces considered per rendered room frame | 120.5 → **88.9** | recomputed, matches brief |
| Triangle primitives per frame, HEAD vs corrected | 411.6 → **412.3** (unchanged) | recomputed |
| Cost per surface considered, v3 | **9,055** cyc | recomputed |
| Cost per surface considered, v1 | **16,722** cyc | recomputed |
| Primitives per surface, v1 vs v3 | **14.96** vs **4.64** | recomputed |
| `room_cell_select` per candidate cell | **1,083** cyc | recomputed |
| Surfaces per drawn cell, v1 vs v3 | **1.35** vs **3.50** | recomputed |
| Every `room_surf_*` and `room_submit_*` CSV column is **all-zero** | 40 dark columns in both runs | recomputed |
| `ot_wait` ≈ 66 cyc | GPU draw time is unmodelled | recomputed, matches CLAUDE.md |
| Inner loop compiled size | **35,488 bytes** (`0x8aa0`) | link map |
| Second full copy, `..._all_cells` | **32,604 bytes** (`0x7f5c`) | link map |
| Emulator models a faithful 4 KB direct-mapped I-cache | 256 lines x 4 words, per-word valid, 5 stall cyc per RAM line fill | `emu/crates/emulator-core/src/cpu/icache.rs` |

The last row matters: **the 804.6k figure already contains I-cache stalls.** The
emulator is not flattering the code here.

### Strong inferences

| Inference | Basis | Confidence |
|---|---|---|
| Per-surface fixed tax is 5,600–7,000 cyc | two independent 2-point fits agreeing within 25% | high |
| The tax is dominated by instruction count, not stalls | 9,055 cyc/surface at BIAS=2 is ~4,527 instructions; a 4–5 KB code path can only account for ~1,300–1,600 stall cycles (footprint/16 x 5) | medium-high |
| cortex_v3's cells are ~2.6x coarser in surfaces-per-cell than cortex_v1's | 3.50 vs 1.35 surfaces per drawn cell | high |
| cortex_v3 is not *bigger* than cortex_v1, it is *less occludable* | cooked manifests: v1 has 461 `CachedRoomSurface` / 237 `CachedRoomCell`; v3 has **397 / 107**. v1 survives 3.4% of its surfaces per frame, v3 survives 22.4% | medium (manifest files in `/tmp` may be stale) |
| The rectangular portal union is currently only rejecting geometry that downstream culls would reject anyway | 26% fewer surfaces, 0.2% more triangles | high |

### Unproven assumptions in the current narrative

| Assumption | Status |
|---|---|
| "cortex_v3 exposes several nearby rooms through portals at once" | **Not supported.** `room_active_chunks` mean 2.6, max 4; `room_stream_resident_slots` a flat 6. HEAD and corrected are identical here. Room count is not the variable. |
| "its camera places more large surfaces close enough to trigger subdivision" | **Contradicted.** cortex_v3 amplifies 4.64 primitives per surface; cortex_v1 amplifies **14.96**. cortex_v1 is the subdivision-heavy project. |
| "surface-to-primitive amplification is the problem" | **Contradicted.** v3's amplification is 3.2x *lower* than v1's. |
| "portal traversal is expensive" | **Refuted.** 8,512 cyc/frame, 0.6% of render. |
| The clean-HEAD cortex_v3 run is a cortex_v1 comparison | **It is not**, and the brief is right to say so. The real comparison is below. |

### Additional evidence required

1. **A per-surface stall counter.** Without it, the split between instruction
   count and I-cache stalls inside the 5,600–7,000 cyc tax is inferred, not
   measured. This is experiment J1 and it gates the whole ranking.
2. **Re-enabling the `room-surface-profile` feature.** All 26 `room_surf_*` /
   `room_submit_*` columns are dark, so the interior of the 804.6k is a black
   box in the supplied evidence.
3. **A current-build cortex_v1 capture on an equivalent route.** The v1 numbers
   I used (`/tmp/cortex-v1-profile-game.gd9g0p.csv`, 900 ticks, 432 visual,
   28.80 FPS) are from an older build and a different route.

---

## C. Why the same engine behaves differently

The engine is a fixed cost function of the *shape* of the accepted work set, and
the two projects sit on opposite sides of that function.

### The like-for-like table

`/tmp/cortex-v1-profile-game.gd9g0p.csv` (gameplay, TR subdivision on,
`HybridWalls`) vs the corrected cortex_v3 gameplay window:

| Metric | cortex_v1 | cortex_v3 | ratio |
|---|---:|---:|---:|
| Effective FPS | 28.80 | 14.01 | 0.49x |
| render mean | 626,937 | 1,404,382 | **2.24x** |
| render p90 | 897,309 | 1,840,485 | 2.05x |
| render max | 986,491 | 2,263,382 | 2.29x |
| `room` | 297,197 | 889,739 | 2.99x |
| `room_surface_draw` | 261,748 | 804,618 | **3.07x** |
| `room_cells_drawn` | 11.6 | 25.4 | 2.19x |
| **`room_surfaces_considered`** | **15.7** | **88.9** | **5.66x** |
| `room_projected_vertices` | 25.9 | 128.7 | 4.97x |
| **`tri_primitives`** | **234.2** | **412.3** | **1.76x** |
| `world_commands` | 243.7 | 450.4 | 1.85x |
| primitives per surface | **14.96** | **4.64** | 0.31x |
| cycles per surface | 16,722 | 9,055 | 0.54x |
| cycles per primitive | **1,118** | **1,952** | **1.75x** |
| authored surfaces (cooked) | 461 | 397 | 0.86x |
| authored cells (cooked) | 237 | 107 | 0.45x |
| **surface survival rate** | **3.4%** | **22.4%** | **6.6x** |

Read the last three rows together. **cortex_v3 has 14% less authored geometry
than cortex_v1 and 55% fewer cells, and it still walks 5.66x as many surfaces
per frame.** This is not a content-volume difference. It is a visibility-
efficiency difference, and it is the whole story.

### The mechanism, item by item

**Cell granularity is the primary lever.** Cell acceptance is all-or-nothing
for the surfaces the cell owns (`indexed_cache.rs:529-570` walks
`cell.surface_first .. +surface_count` unconditionally). cortex_v1 packs 1.35
surfaces into each drawn cell; cortex_v3 packs 3.50. Accepting one cortex_v3
cell drags in 2.6x more surfaces than accepting one cortex_v1 cell, and the
per-surface tax is paid on all of them. 25.4 cells x 3.50 = 88.9 surfaces,
matching the measurement exactly. cortex_v3 cooked 397 surfaces into 107 cells
(3.71 per cell) where cortex_v1 cooked 461 into 237 (1.95 per cell).

**Simultaneously visible rooms are not the lever.** `room_active_chunks` is 2.6
mean / 4 max in both cortex_v3 runs. Six resident slots, always. The streaming
limits (10 vs 6) never bind.

**Subdivision thresholds are the *opposite* of the stated hypothesis.**
cortex_v1's few, huge, close surfaces trigger the TR lattice hard: 14.96
primitives per surface, and its 1,118 cyc/primitive reflects a path that spends
most of its time inside the compact recursive subdivision code
(`submit_tomb_raider_cached_room_quad`, 9,564 bytes, which is 2.3x the I-cache
rather than 8.7x). cortex_v3's many small wall bands mostly emit a single warm
quad, so it pays the 35 KB dispatch prologue for a 4.64-primitive payload. Per
emitted primitive cortex_v3 is **1.75x more expensive** precisely because its
primitives are cheap relative to the fixed tax.

**Wall-band density and material boundaries** are the authoring cause of the
granularity: stacked wall sections, diagonal walls, and material splits each
produce a separate 40-byte `CachedRoomSurface` in the same cell.

**Portal topology and stacked rooms** matter only through the cell mask they
admit, and the measured `portal_visibility` cost (8,512 cyc, 0.6%) confirms
traversal itself is free.

**Ordering-table and packet cost** is the `b` term: 439–743 cyc per primitive,
so 181k–306k of the 804.6k. Real, but second.

### What must be measured in cortex_v1 to prove this

A current-build cortex_v1 capture (experiment J6) recording
`room_surfaces_considered`, `room_cells_drawn`, `tri_primitives`, and
`room_surface_draw`. The prediction, stated in advance and falsifiable: **the
regression of `room_surface_draw` on `(surfaces_considered, tri_primitives)`
across both projects will produce a single common slope pair, `a` in
5,000–7,500 and `b` in 400–900, with R² > 0.85.** If cortex_v1 needs a
materially different `a`, the "shared fixed tax" model is wrong and the cause is
project-specific after all.

---

## D. Cycle-budget reconstruction

### Units and how I resolved the ambiguities

- Profiler cycles are the emulator's CPU cycle counter, BIAS = 2 per instruction
  (`emu/crates/emulator-core/src/cpu.rs:2600`) **plus** modelled memory and
  I-cache fill stalls (`bus/memory_timing.rs:234`, `cpu/icache.rs`). It is *not*
  a pure instruction count, but instruction count is the dominant term.
- 1 NTSC vblank = 33,868,800 / 60 = **564,480 cycles**. Confirmed against the
  data: `present` max is 569,015, i.e. one full vblank of edge wait.
- 2 vblanks = 1,128,960. 3 vblanks = **1,693,440**.
- One CSV row is **one sim tick** (60 Hz), not one vblank. `visual_frames == 1`
  marks a render tick. Averaging any stage over all rows understates it by the
  render/sim ratio; every per-frame figure below is averaged over visual rows
  only.
- **`visual_render_task = render + present`.** `update` is charged separately.
  This is the single most important boundary to get right, and the brief's
  "visual-render task 1.71M" already includes ~305k of vblank-edge wait that is
  not work.

### The per-delivered-frame identity

```
cycles_per_delivered_frame
    = render                                   (visual tick)
    + update(visual tick)                      (visual tick)
    + present                                  (vblank-edge quantisation)
    + (ticks_per_frame - 1) x frame_cycles(sim-only tick)
```

Corrected build, gameplay window (last 1,644 rows), measured:

| Term | cycles |
|---|---:|
| render | 1,404,382 |
| update on the visual tick | 153,914 |
| present (edge wait) | 305,407 |
| 3.281 sim-only ticks x 173,980 | 570,830 |
| **total** | **2,434,533** |

2,434,533 / 564,480 = **4.313 vblanks per delivered frame** → 60 / 4.313 =
**13.91 FPS**, against a measured 14.01. The identity closes to 0.7%.

**This is why 14.01 FPS coexists with a "1.11M average render".** Three
separate effects each inflate the real cost above the headline:

1. The 1.11M full-run average includes 106 boot/loading frames that render
   almost nothing. Gameplay-only is 1,404,382, **26% higher**.
2. `present` adds 305,407 cycles of vblank-edge rounding per frame. It is not
   work, but it is wall-clock you cannot get back at a given cadence.
3. Every delivered frame carries **3.28 sim-only ticks at 173,980 cycles each**.
   The sim tax alone is 570,830 cycles per frame, 23% of the frame budget, and
   no render optimisation touches it.

### The 20 FPS budget

At a stable 3-vblank cadence there are exactly 3 sim ticks per frame: two
sim-only plus the visual tick.

```
3 x 564,480 = 1,693,440                       total budget
 - 2 x 173,980 = 347,960                      sim-only ticks
 -     153,914                                update on the visual tick
 ---------------------------------------------
 render budget R <= 1,191,566 cycles
```

| Case | current render | budget | required cut |
|---|---:|---:|---:|
| mean | 1,404,382 | 1,191,566 | **-15.2%** |
| p50 | 1,394,686 | 1,191,566 | -14.6% |
| p90 | 1,840,485 | 1,191,566 | -35.3% |
| p99 | 2,215,465 | 1,191,566 | -46.2% |
| **max** | **2,263,382** | 1,191,566 | **-47.4%** |

**Average 20 FPS needs a 15% render cut. *Stable* 20 FPS needs 47%.** That gap
is the whole engineering problem, and it is exactly why an average-FPS framing
would mislead here.

### Render stage decomposition (gameplay, corrected)

| Stage | cyc/frame | % render | note |
|---|---:|---:|---|
| **`room_surface_draw`** | **804,618** | **57.3%** | the target |
| `player` | 200,784 | 14.3% | `textured_model_faces` 120,614 dominates |
| `image_props` | 130,834 | 9.3% | p90 226,234, a real tail source |
| `room_cell_select` | 49,706 | 3.5% | 1,083 cyc per candidate cell |
| `world_flush` | 46,858 | 3.3% | |
| `camera` | 42,352 | 3.0% | on a *render* tick; see E6 |
| `model_instances` | 36,623 | 2.6% | p90 180,743, bimodal |
| render remainder | 24,758 | 1.8% | |
| `sky` | 23,843 | 1.7% | |
| `room_project` | 20,509 | 1.5% | **159 cyc/vertex, GTE is fine** |
| `portal_visibility` | 8,512 | 0.6% | not the problem |
| `room_visible_list` | 6,186 | 0.4% | |
| `room` remainder | 4,715 | 0.3% | |
| `room_depth_prep` | 4,005 | 0.3% | |
| `frame_clear` | 79 | 0.0% | |
| **sum** | **1,404,382** | 100% | closes exactly |

Note the shape: projection is 1.5%, GTE is a non-issue, portal traversal is
0.6%, and one stage is 57%.

### Inside `room_surface_draw`

```
804,618 = a x 88.9 + b x 412.3
```

with `a` in [5,608, 7,020] and `b` in [439, 743]:

| Component | cycles | share |
|---|---:|---:|
| per-surface fixed tax | 498,551 – 624,078 | **62–78%** |
| per-primitive emission | 181,000 – 306,367 | 22–38% |

At BIAS = 2, `a` = 5,608–7,020 cycles is **2,800–3,500 instructions per
surface** before any primitive exists. Reference points for how absurd that is:
`room_project` runs the full GTE projection at **159 cycles per vertex**, and
the player's `textured_model_faces` culls and builds packets at **538 cycles per
emitted triangle**.

### GPU and DMA

`ot_wait` is 66 cycles because the emulator does not model GPU draw time. On
silicon, 412 world primitives plus ~224 player triangles of textured-Gouraud
fill is roughly 0.3–0.6 vblank of GPU work. Under the phase-1 pipeline the draw
is kicked with `submit_async` and overlaps the following fixed ticks, which at a
3-vblank cadence provide 347,960 cycles (0.62 vblank) of cover. **The GPU should
stay covered**, but this is an assumption the emulator cannot validate and it is
what experiment J8 exists to check.

---

## E. Root-cause ranking

### E1. Per-surface fixed decision tax (rank 1)

- **Evidence**: two independent fits give `a` = 5,608 and 7,020. 32 surfaces
  removed by the portal union cost 7,020 each and emitted zero triangles.
- **Contribution**: 499k–624k cyc/frame, 36–44% of render.
- **Confidence**: high.
- **Bound**: CPU, instruction count.
- **Falsify with one experiment**: hoist the static half of the per-surface work
  into a per-room prepass (J2). If `room_surface_draw` does not drop by at least
  250k with `tri_primitives` unchanged, the tax is not where I say it is.

### E2. The instrumentation is dark (rank 2, gates everything)

- **Evidence**: all 26 `room_surf_*` / `room_submit_*` columns are exactly zero
  in both supplied runs. The `RoomSurfaceMicroProfile` methods at
  `indexed_cache.rs:62-196` take `_cycles` and discard it unless the
  `room-surface-profile` feature is on.
- **Contribution**: none directly, but it makes the 804.6k unattributable, and
  the brief's own hypothesis list ("the profiler's stage boundaries conceal a
  different bottleneck") is currently unfalsifiable.
- **Confidence**: certain.
- **Note**: the feature also `#[cfg]`-disables the warm-quad fast paths
  (`indexed_cache.rs:1077`, `:1261`, `:1492`), so a profile-feature run is *not*
  the same code. Attribute proportions, never absolute cycles, from it.

### E3. I-cache thrash in a 35 KB inner loop (rank 3)

- **Evidence**: `draw_indexed_cached_room_vertex_lit_visible_cells` is 35,488
  bytes against a 4,096-byte direct-mapped I-cache: **8.66 cache images**. The
  loop cannot be resident. `..._all_cells` is a second 32,604-byte copy;
  `submit_tomb_raider_cached_room_quad` 9,564; `draw_near_clipped_...` 5,308.
  Room render code spans ~86 KB, 21x the cache.
- **Contribution**: bounded above by (path bytes / 16) x 5 stall cycles. A
  4–5 KB taken path gives 1,300–1,600 cyc/surface, so **115k–140k per frame**,
  roughly 20% of the per-surface tax.
- **Confidence**: medium. The mechanism is certain; the magnitude is inferred.
- **Bound**: memory (instruction fetch).
- **This explains two rejected experiments.** 8.3 (model-options borrowing) and
  8.4 (view-space cache) both improved an isolated stage and regressed total
  cadence. In a direct-mapped cache 8.7x oversubscribed, *moving code changes
  which lines alias*. A local win that shifts a hot function across a 0x1000
  boundary can cost more globally than it saves locally. Any future measurement
  that reports a single stage without total-runtime and frame-count deltas is
  not evidence.
- **Falsify**: J1, an I-cache stall counter.

### E4. `room_cell_select` at 1,083 cyc per candidate cell (rank 4)

- **Evidence**: 49,706 cyc for 45.9 candidates, directly measured. Each
  candidate runs `cached_room_cell_index_for_visible`, a GTE `view_vertex`, a
  `cell_aabb_visible` frustum test, and now a second
  `cell_aabb_intersects_portal_window` test (`indexed_cache.rs:361-438`).
- **Contribution**: 49,706 cyc/frame, 3.5% of render. Rose 34% (37,073 →
  49,706) when the portal window was added.
- **Confidence**: high.
- **Bound**: CPU.
- **Falsify**: replace with a cooked portal→cell bitmask (F2). If the stage does
  not fall below 10k, the cost is in the lookup, not the tests.

### E5. Cell granularity: 3.50 surfaces per drawn cell (rank 5, content-side)

- **Evidence**: 3.50 vs cortex_v1's 1.35. Cell acceptance is all-or-nothing.
- **Contribution**: it is the *multiplier* on E1, not an independent cost. It is
  why cortex_v3 walks 88.9 surfaces where cortex_v1 walks 15.7.
- **Confidence**: high on the ratio, medium on the cause (cooker cell sizing vs
  authoring density).
- **Falsify**: J7, cook cortex_v3 at a smaller sector size and check whether
  `room_surfaces_considered` falls faster than `room_cells_drawn` rises.

### E6. `camera` at 42,352 cycles on a render tick (rank 6)

- **Evidence**: `camera` is charged inside `render`, p90 71,642. Camera pose is
  a simulation quantity; on a 30-or-lower-Hz render this is either recomputed
  work or genuinely render-side view setup.
- **Contribution**: up to 42k/frame, 3% of render.
- **Confidence**: low. This is a question, not a finding.
- **Falsify**: read the call site in `playtest_scene.rs` and check whether the
  same camera solve also runs in `update`.

### E7. `image_props` and `model_instances` tails (rank 7)

- `image_props` mean 130,834, p90 226,234, max 369,214.
  `model_instances` mean 36,623 but p90 180,743: strongly bimodal.
- Together they contribute up to 407k on bad frames. **They are not the mean
  problem but they are a third of the tail**, and the tail is what "stable"
  means. Any worst-case proof must bound them too.

### Explicitly demoted

- **GTE / projection**: `room_project` is 1.5% at 159 cyc/vertex. Not a lever.
  This is consistent with the existing `keep-work-on-gte-not-cpu` finding.
- **Portal traversal**: 0.6%.
- **DMA / `ot_submit`**: 82 cycles. Noise.

---

## F. Recommended architecture

The invariant to establish:

> **A cached room surface's draw behaviour is fully determined at room-residency
> time except for four projected vertices, one depth span, and one backface
> sign. The render loop must read a precompiled command, not interpret a
> record.**

Three changes. They are independent and each is separately shippable.

### F1. Per-surface precompiled draw records ("surface classes")

**Observation that makes this cheap:** the combinatorics of
`WorldSurfaceOptions` are tiny. `cached_surface_subdivision_options` and
`triangle_depth_options` / `horizontal_depth_options` map
`(kind, use_triangle_depth, surface_risky)` onto at most 12 distinct option
structs for the entire frame. Today each of 88.9 surfaces constructs one from
scratch, per frame, by value.

Build the table once per frame, index it per surface.

#### Cooked / prewarmed representation

Extend the existing prewarm pass (`prewarm_indexed_cached_room_quads`,
`indexed_cache.rs:1615`), which already runs at room residency and already owns
the packet pool. Add a parallel array:

```rust
/// One per cached room surface. Built once when a room becomes resident,
/// alongside the warmed packet pool. Nothing here depends on the camera.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct SurfaceDrawRecord {
    /// Resolved material after `wall_material_for_direction` +
    /// `cached_uv_material` + layer selection. Index into the room's
    /// resolved-material table.
    resolved_material: u8,
    /// Index into the frame's `OptionVariantTable` when the surface is
    /// classified not-risky.
    options_calm: u8,
    /// Index into the same table when classified risky.
    options_risky: u8,
    /// Precomputed `warmed_room_quad_ready_value(...)`.
    warm_ready: u8,
    /// Packet corner permutation folding winding / reverse_front / split
    /// order. Replaces `warmed_room_quad_packet_vertices` at draw time.
    packet_perm: [u8; 4],
    /// Static classification bits, see below.
    class: u8,
    /// `triangle_index`, or 2 for a whole quad.
    triangle_index: u8,
    /// Depth-span threshold this surface must exceed to be "risky".
    /// Pre-resolved from kind + CACHED_SURFACE_HORIZONTAL_NON_FLAT so
    /// `cached_horizontal_surface_is_risky` disappears from the hot path.
    /// `i32::MIN` means "always risky", `i32::MAX` means "never".
    risk_threshold: i32,
}
// 4 + 4 + 1 + 1 + 4 = 16 bytes, 4-byte aligned, one cache line per 4 surfaces.
```

Class bits:

```
bit 0  WHOLE_QUAD          triangle_index >= WHOLE_QUAD_TRIANGLE_INDEX
bit 1  IS_CEILING          drives winding, already folded into packet_perm
bit 2  IS_WALL
bit 3  HAS_BAKED_RGB
bit 4  SLOW_MATERIAL       animated or translucent -> never warm, never prebuilt
bit 5  TR_ELIGIBLE         kind is in options.tomb_raider_subdivision_kinds
bit 6  DOUBLE_SIDED        material.sidedness, hoisted out of the cull test
bit 7  reserved
```

Memory: **16 bytes x 364 surfaces = 5,824 bytes** for cortex_v3, next to the
existing 14,560-byte surface cache. Sized for the resident-room limit it is
16 x max_surfaces_per_room. Disc impact: **zero** if built at residency (my
recommendation), or +5,824 bytes per level if cooked. Build it at residency:
`prewarm_indexed_cached_room_quads` already walks exactly this data, and keeping
it out of the cooked format avoids a format version bump.

The per-frame option table:

```rust
/// At most 12 distinct WorldSurfaceOptions exist per frame. Built once in
/// the room prologue, indexed by SurfaceDrawRecord::options_{calm,risky}.
struct OptionVariantTable {
    variants: [WorldSurfaceOptions; 12],   // ~12 x 48 = 576 bytes on the stack
    len: u8,
}
```

#### Runtime loop

```rust
// Prologue, once per room render (not per surface).
let opts = OptionVariantTable::build(options, depth_mode, subdivision_mode);

for accepted in 0..accepted_cell_count {
    let cell        = cells[accepted_cell_indices[accepted] as usize];
    let cell_depth  = accepted_cell_depths[accepted];
    let submit      = CachedRoomSubmitDepths::from_cell_options::<OT>(
                          tile_depth_options_from_depth(options, cell_depth));

    for i in cell.surface_first .. cell.surface_first + cell.surface_count {
        let rec = &records[i];
        let surf = &surfaces[i];

        // ---- the only camera-dependent work ----
        let Some(p) = indexed_projected_quad(projected, surf.vertex_indices)
            else { return near_clip_slow(i); };          // #[inline(never)]
        let m = ProjectedQuadMetrics::new(p);
        if m.outside_screen(bounds) { continue; }        // ~90 cyc
        if backface_sign(p, rec.class) < 0 { continue; } // ~120 cyc
        let risky = m.depth_span() >= rec.risk_threshold;
        let o = &opts.variants[if risky { rec.options_risky }
                               else     { rec.options_calm } as usize];

        // ---- dispatch on a precomputed class, into small leaf fns ----
        if rec.class & SLOW_MATERIAL != 0 {
            emit_cold(rec, surf, p, m, o, submit, ...);          // #[inline(never)]
        } else if o.tomb_raider_subdivision
               && tomb_raider_projected_quad_needs_subdivision(p, o.profile) {
            emit_subdivided(rec, surf, p, m, o, ...);            // #[inline(never)]
        } else if let Some(q) = pool.get_mut(i) {
            emit_warm(rec, q, p, m, o, submit);                  // #[inline(never)]
        } else {
            emit_cold(rec, surf, p, m, o, submit, ...);
        }
    }
}
```

**What disappears from the hot path, per surface, per frame:** the
`cached_surface_kind` decode, the `materials.get` bounds-checked lookup, the
`is_animated` / `is_translucent` branches, `animated_cached_uv_words` setup,
`cached_uv_material`, `wall_material_for_direction`, the entire
`cached_surface_risk_for_modes` → `cached_surface_is_risky` →
`cached_horizontal_surface_is_risky` chain (replaced by one `i32` compare
against `rec.risk_threshold`), three `WorldSurfaceOptions` constructions,
`warmed_room_quad_ready_value`, `warmed_room_quad_packet_vertices`, and the
`match kind` with its four near-duplicate 100-plus-line arms.

**What must be preserved bit-for-bit** (see G and H): the depth-span comparison
must use the same `saturating_sub` and the same threshold constant; `packet_perm`
must reproduce `warmed_room_quad_packet_vertices` exactly, including the
`WARMED_ROOM_QUAD_REVERSE` vs `WARMED_ROOM_QUAD_REVERSE_FRONT` distinction
documented at `indexed_cache.rs:1600-1606`.

**Expected saving**: `a` falls from 5,600–7,000 to a projected 1,200–1,800
(projection fetch + screen test + backface + one compare + one indexed dispatch).
At 88.9 surfaces: **-340k to -480k cycles per frame**, confidence **medium-high**
on the direction and **medium** on the magnitude, because the split between
instruction count and I-cache stalls is inferred (E1/E3) and only J1 settles it.

**Secondary effect, deliberate**: `#[inline(never)]` on the four leaf emitters
breaks the 35,488-byte function into a ~1.5 KB loop plus ~1–2 KB leaves. The
dominant warm-quad path becomes **~3 KB, resident in the 4 KB I-cache across all
88.9 surfaces**. This is the E3 fix and it is free once F1 is done.

### F2. Cooked portal→cell visibility bitmask

The brief asks whether the component-wise rectangular union is the best
conservative representation. **It is correct, and it is the weakest useful one.**
Its structural failure mode is exactly the case that motivated it: two apertures
on opposite sides of a room union to a rectangle covering everything between
them. The measurement confirms it is not pulling its weight: it removed 26% of
surfaces and **changed the triangle count by 0.2%**, so it is rejecting only what
the backface and screen tests would have rejected anyway. Correct, cheap, and
shallow.

Do not keep more rectangles. Four windows means four AABB tests per candidate
cell, and `room_cell_select` already costs **1,083 cycles per candidate**.

Instead, move the question offline where it is exactly answerable.

```rust
/// For each portal, the set of destination-room cells that can be seen
/// through that portal from ANY camera position in the source cell.
/// Computed by the cooker via exact aperture-frustum / cell-AABB
/// intersection, swept over the source room's cells.
///
/// Rooms are <= 6x6 = 36 cells, so one u64 per portal covers a room with
/// 28 bits spare. A level with 6 portals costs 48 bytes.
pub struct PortalCellMask(u64);
```

Runtime, per room, replacing the whole per-candidate frustum/window pass:

```rust
// admitted_portals: the portals that admitted this room during traversal
let mut mask = 0u64;
for p in admitted_portals { mask |= portal_cell_masks[p].0; }
// Root room / stacked room / unprovable window -> no restriction.
if admitted_portals.is_empty() { mask = u64::MAX; }

for visible in visible_cells {
    if mask & (1 << visible.cell_ordinal) == 0 { continue; }   // ~6 cycles
    ...existing depth + frustum path...
}
```

One `u64` OR per admitting portal plus a 6-cycle bit test per candidate replaces
a GTE `view_vertex` and two AABB tests for cells that cannot contribute. It is
strictly more precise than the rectangle union (it is an exact per-cell answer,
not a bounding box) and strictly cheaper.

- **Memory**: 8 bytes per portal. **48 bytes** for cortex_v3. Disc: +48 bytes,
  one `u64` array in the level manifest.
- **Cooker cost**: 6 portals x 36 source cells x 36 dest cells = 7,776 exact
  aperture-frustum / AABB tests. Milliseconds.
- **Expected saving**: `room_cell_select` 49,706 → **under 10,000**
  (**-40k**), plus a further reduction in `room_surfaces_considered` that
  compounds with F1's reduced `a`. Confidence **high** on the cell-select
  saving, **medium** on the surface-count reduction (it depends on how much
  slack the rectangle union is leaving, which J4 measures).
- **Keep the rectangle union** as the fallback for any room where the cooker
  cannot prove a mask (stacked/overlap rooms without a recorded frustum). Never
  regress to single-aperture.

### F3. Bound the schedule (worst case only)

F1 and F2 fix the mean. They do not, by themselves, prove the p99. See K.

### Non-goals, deliberately

- **No change to the subdivision lattice.** The bit-exact one-level lattice
  (`project_tomb_raider_view_lattice_gte`) stays exactly as it is. It is 22–38%
  of `room_surface_draw` and it is *earning* its cycles: 439–743 per emitted
  primitive is a reasonable price for a textured Gouraud packet.
- **No change to crack-cover underdraw.** It stays, with the existing root
  reuse.
- **No new view-space cache** (8.4 failed, and F1 removes the recomputation it
  was trying to cache without adding a cache).
- **No offline geometry splitting at portal boundaries.** F2 makes it
  unnecessary and it would change the authored surface set, which H forbids.

---

## G. Correctness argument

The claim: F2 cannot remove visible geometry.

**Definition.** `PortalCellMask[p]` has bit `c` set iff there exists at least one
point `x` in the source cell of portal `p` and at least one point `y` in the AABB
of destination cell `c` such that the segment `xy` passes through the aperture of
`p` and `y` is inside the view frustum for some orientation. The cooker computes
this by exact aperture-frustum extrusion against the cell AABB, with all
comparisons rounded **outward**. A bit is set whenever visibility cannot be
disproven.

**Multiple disjoint portal paths.** Masks are OR-ed across all admitting
portals. A cell visible through *any* admitting portal has its bit set by that
portal's mask, independent of every other. This is the union of exact per-path
answers, not a bounding box over them, so it is strictly tighter than the
current rectangle and equally safe. The existing test
`portal_cell_window_union_keeps_every_admitting_path` extends directly: replace
the window assertion with a mask assertion.

**Cyclic portal graphs.** Masks are a static property of the portal, not of the
traversal. Traversal order and revisits cannot change the OR. Cycles affect only
which portals end up in `admitted_portals`, and the OR is commutative,
associative and idempotent.

**Root room.** `admitted_portals` is empty, so `mask = u64::MAX`. No restriction,
matching the current `portal_cell_window = None` behaviour exactly.

**Stacked and overlap rooms without a recorded frustum.** Same: no admitting
portal, `mask = u64::MAX`. The cooker must also emit `u64::MAX` for any room
whose mask it cannot prove, and this must be the *default* value of the array so
a cooker bug fails open rather than closed.

**Near-plane crossings.** The mask is a purely geometric room-space relation and
does nothing near-plane-specific. Cells whose surfaces cross the near plane still
reach `draw_near_clipped_cached_room_surface` unchanged, because the mask only
gates cell candidacy and the near-clip path is downstream.

**Large cells straddling a portal edge.** Because the test is
existential ("is there *any* sightline"), a cell straddling the aperture edge has
its bit set. There is no partial acceptance and therefore no partial-cell
clipping bug, which is precisely the failure mode of the rejected
single-aperture cull.

**Camera movement near portal seams.** The mask is swept over **every point of
the source cell**, not the camera's instantaneous position. Sub-cell camera
motion therefore cannot change the answer, which eliminates the popping class of
bug entirely. The source-cell granularity must match the granularity the
traversal uses to select `admitted_portals`; if traversal ever becomes
sub-cell, the sweep must be widened to the containing cell, never narrowed.

**Horizontal and vertical clipping.** The sweep uses the full 3D aperture and
the full cell AABB including `min_y`/`max_y`. Vertical portals in stacked rooms
are handled by the same test with no special case.

**Fixed-point rounding.** Every comparison in the cooker rounds **outward**:
aperture extents round away from the aperture centre, cell AABBs round away from
the cell centre. A bit is set on ties. The runtime does no arithmetic at all,
only `mask & (1 << ordinal)`, so there is no runtime rounding to get wrong. This
is a strict improvement over the current window path, which does saturating
`i32` AABB arithmetic at runtime.

**Saturation and overflow.** The runtime operation is a shift and an AND on a
`u64` with `ordinal < 64`, enforced by a cooker assertion that
`cells_per_room <= 64`. This is the one new content limit the design introduces
and it must be a hard cooker error, not a clamp.

**The failure-open invariant.** State it as an assertion and test it: for every
room, `popcount(cooked_mask) >= popcount(runtime_accepted_cells)` over a full
tape replay. If the cooker is ever wrong, this fires before pixels do.

---

## H. Visual-equivalence argument

### What "pixel-identical" should mean here

Three tiers, applied to different things:

| Tier | Applies to | Method |
|---|---|---|
| **Bit-exact unit** | the lattice, `packet_perm`, `risk_threshold` | property tests asserting the new path equals the old path for all inputs in the surface's domain |
| **Frame-hash exact** | the full route | `--dump-hash` VRAM + display hash equality on every dumped frame across the whole tape |
| **Perceptual** | nothing in this change | not applicable; if a hash differs the change is wrong |

F1 and F2 are pure work-elimination. There is **no tier-3 case**: any hash
difference is a bug, not a tradeoff. This is the discipline that experiment 8.1
(crack-cover removal, 8,324 pixels differing) failed, and it is the right bar.

### Preservation, item by item

- **Authored surfaces**: F1 changes representation only; every surface record is
  still visited. F2 changes only *which cells* are candidates, under the G
  argument that it cannot reject a visible one.
- **Subdivision topology**: `emit_subdivided` is the existing
  `submit_tomb_raider_cached_room_quad` / `_triangle`, called with the same
  `WorldSurfaceOptions` value. The option is now *table-looked-up* rather than
  *constructed*, so the test is that `OptionVariantTable::build` produces a value
  equal to `cached_surface_subdivision_options` for every
  `(kind, use_triangle_depth, surface_risky)` triple. That is 12 cases, testable
  exhaustively.
- **Projected positions and rounding**: unchanged. `room_project` and the GTE
  path are untouched, and F1 does not move any arithmetic.
- **UV interpolation**: `uv_words` come from the same `surface.uv_words` or the
  same `animated_cached_uv_words` on the `SLOW_MATERIAL` path.
- **Gouraud colours**: `indexed_vertex_lighting_colors` is unchanged and still
  called with the same arguments on the cold path; the warm path still reuses
  baked colours under the same `prebuilt_static_colors_ready` condition.
- **Backface behaviour**: this is the sharpest risk. `class` must fold
  `material.sidedness` and `is_ceiling` into the same decision that
  `projected_quad_backface_culled` / `projected_split_triangle_backface_culled`
  make today, including the `reverse_quad_winding` applied to ceilings *before*
  the cull test at `indexed_cache.rs:1244-1248`. Test: for every surface in
  every cooked room and a swept set of camera poses, assert the new sign equals
  the old.
- **Crack-cover underdraw**: untouched, including the root-projection reuse.
- **Transparency ordering**: `SLOW_MATERIAL` forces translucent surfaces onto
  the existing cold path, preserving `with_material_layer` exactly as the
  comment at `indexed_cache.rs:1123-1128` requires.
- **Depth policy**: `submit_depths` and `PreparedTriangleDepth` are computed
  identically; only the option *lookup* changes.
- **GPU command ordering**: **surfaces are emitted in the same order.** F1 does
  not reorder. Any future bucketing by class (a real further win) must be gated
  on a separate proof that OT slot keys are a total order within a cell, and
  validated by frame hashes. I am not proposing it now.

---

## I. Concrete implementation plan

### I1. `engine/crates/psx-engine/src/world_render/indexed_cache.rs`

**`draw_indexed_cached_room_surface` (lines 1021-1594)**

- *Current*: 570 lines, four near-duplicate arms, fully inlined into a
  35,488-byte caller. Interprets a `CachedRoomSurface` from scratch every frame.
- *New*: deleted. Replaced by the loop body in F1 plus four `#[inline(never)]`
  leaves: `emit_warm_quad`, `emit_cold_quad`, `emit_split_triangle`,
  `emit_subdivided`. Each under ~2 KB.
- *Data flow*: reads `&SurfaceDrawRecord` and `&OptionVariantTable` instead of
  `materials: &[WorldRenderMaterial]`, `depth_mode`, `subdivision_mode`.
- *API*: internal only.
- *Memory*: -0 (code shrinks).
- *Saving*: the bulk of the 340k–480k.
- *Test*: `emit_*` leaves reproduce the old arms for every `class` bit pattern.

**`prewarm_indexed_cached_room_quads` (line 1615)**

- *Current*: builds the static half of the warm packet pool.
- *New*: also fills `&mut [SurfaceDrawRecord]`. It already walks
  `(surfaces, materials)`, so this is the natural and only correct home.
- *API*: one added `&mut [SurfaceDrawRecord]` parameter.
- *Memory*: +16 bytes per surface in the resident arena.
- *Test*: `record_matches_legacy_derivation` over every cooked room in
  cortex_v1, cortex_v3 and demo_11.

**`cached_surface_risk_for_modes`, `cached_surface_is_risky`,
`cached_horizontal_surface_is_risky` (lines 2189-2261)**

- *Current*: a three-call chain per surface per frame.
- *New*: collapsed at prewarm into `SurfaceDrawRecord::risk_threshold: i32`. The
  hot path becomes `m.depth_span() >= rec.risk_threshold`. Keep the functions for
  the prewarm pass and the tests.
- *Test*: for every surface and a sweep of depth spans, threshold compare ==
  legacy chain.

**`cached_surface_subdivision_options` (line 2210)**

- *Current*: constructs a `WorldSurfaceOptions` per surface per frame.
- *New*: called only by `OptionVariantTable::build`, at most 12 times per frame.
- *Test*: exhaustive over the 12-case domain.

**`draw_indexed_cached_room_vertex_lit_visible_cells` (line 299)**

- *Current*: 35,488 bytes.
- *New*: the F1 loop. Target under 4 KB so the loop plus the dominant leaf is
  I-cache resident.
- *Test*: a link-map size assertion in CI. `#[test]` cannot see this, so add a
  `make` check that greps the map and fails above a threshold. This is the only
  guard that would have caught the 8.3 and 8.4 regressions.

**`draw_indexed_cached_room_vertex_lit_all_cells` (line 584, 32,604 bytes)**

- Check whether cortex_v3 reaches it at all (`room_vis_fallback_draws` is
  all-zero in both runs, which suggests not). If it is dead for the shipping
  configuration it is 32 KB of ROM and a second copy to keep in sync; if it is
  live it must get the same treatment. **Resolve this before starting**, it may
  be free deletion.

### I2. `engine/crates/psx-engine/src/world_render.rs`

- Add `SurfaceDrawRecord`, `OptionVariantTable`, `PortalCellMask`.
- Keep `PortalCellWindow` and `cell_aabb_intersects_portal_window` as the
  fallback for rooms with no cooked mask.

### I3. `engine/crates/psx-engine/src/render3d.rs` and `render3d/world_pass_gouraud.rs`

- No functional change. `project_tomb_raider_view_lattice_gte` and the submit
  helpers are called with the same values.
- Watch the link map: `submit_textured_gouraud_quad_prescreened_uv_words_prepared_depth`
  is 4,916 bytes and `submit_textured_gouraud_view_triangle_uv_words` is 6,312.
  These are already large enough to matter for I-cache residency of the leaves.

### I4. `engine/crates/psx-level/src/portal_visibility.rs` and the cooker

- Cooker (`editor/crates/psxed-project`): emit `PortalCellMask` per portal by
  sweeping source cells against destination cell AABBs through the aperture,
  rounding outward. Default `u64::MAX`. Hard-error if any room exceeds 64 cells.
- Level manifest: one `[u64; PORTAL_COUNT]` array. **+48 bytes** for cortex_v3.

### I5. `engine/examples/editor-playtest/src/active_room_visibility.rs`

- `portal_cell_window` keeps producing the union window (fallback), and
  additionally OR-s the cooked masks of the admitting portals.
- The empty-`admitted_portals` case must produce `u64::MAX`, not `0`. Write that
  test first.

### I6. `engine/crates/psx-engine/src/world_render/tests.rs`

New tests:

1. `surface_draw_record_matches_legacy_derivation` (all cooked rooms)
2. `option_variant_table_covers_all_twelve_cases`
3. `risk_threshold_matches_legacy_risk_chain`
4. `packet_perm_matches_warmed_room_quad_packet_vertices`
5. `backface_class_matches_legacy_cull` (swept camera poses)
6. `portal_cell_mask_superset_of_runtime_accepted_cells`
7. `portal_cell_mask_empty_admitting_set_is_unrestricted`
8. Keep `portal_cell_window_union_keeps_every_admitting_path`, extended to masks.

---

## J. Benchmark and experiment matrix

Every run uses the cortex_v3 tape via the headless replay in
`emu/crates/frontend/src/cli.rs`, `--steps 2000000000`, gameplay window = last
1,644 rows. **Every experiment reports `render` mean *and* p99 *and* max, total
runtime, and delivered frame count.** A single-stage delta is not a result;
8.3 and 8.4 both passed that bar and both regressed.

### J1. Is the per-surface tax instruction count or I-cache stalls?

- **Hypothesis**: I-cache stalls are 15–25% of `room_surface_draw`, not the
  majority.
- **Instrumentation**: add a cumulative I-cache fill-stall counter to
  `cpu/icache.rs` / `bus.rs:1313`, exported per profile row.
- **Baseline**: `room_surface_draw` 804,618.
- **Expected**: 120k–200k of it is stall.
- **Reject if**: stalls exceed 400k, in which case F1's code-splitting becomes
  the primary fix and the record table is secondary.
- **Visual**: none (emulator-side only).
- **Where**: emulator. **Run this first.**

### J2. Does hoisting static per-surface work pay?

- **Hypothesis**: F1 cuts `room_surface_draw` by 340k–480k with
  `tri_primitives` unchanged.
- **Instrumentation**: standard profile plus `--dump-hash`.
- **Baseline**: 804,618 / 412.3 primitives.
- **Expected**: 330k–470k, primitives within +/-1.
- **Reject if**: under 200k, or `tri_primitives` moves by more than 1.
- **Visual**: **frame hashes must match on every dumped frame.**
- **Where**: emulator, then hardware.

### J3. Does splitting the mega-function pay on its own?

- **Hypothesis**: `#[inline(never)]` on the four leaves, with no other change,
  cuts `room_surface_draw` by 80k–150k.
- **Instrumentation**: profile plus the link-map size of the loop function.
- **Expected**: loop under 4 KB; 80k–150k saved.
- **Reject if**: total runtime regresses. This is the 8.3/8.4 failure mode and it
  is a live risk. **Do J3 before J2** so its effect is separable.
- **Visual**: hashes must match (it is a pure inlining change).
- **Where**: emulator *and* hardware. Emulator I-cache modelling is faithful but
  RAM timing is a model.

### J4. How much slack is left in the rectangle union?

- **Hypothesis**: the cooked mask cuts `room_surfaces_considered` from 88.9 to
  under 70 and `room_cell_select` from 49,706 to under 10,000.
- **Baseline**: 88.9 surfaces, 45.9 candidates, 49,706 cyc.
- **Expected**: cell-select under 10k (high confidence); surfaces 60–75 (medium).
- **Reject if**: surfaces do not fall below 80, which would mean the rectangle
  union is already near-exact and F2 is only worth its cell-select saving.
- **Visual**: hashes must match, and
  `portal_cell_mask_superset_of_runtime_accepted_cells` must hold for the whole
  tape.
- **Where**: emulator.

### J5. Where is the tail?

- **Hypothesis**: the worst 5% of frames are dominated by
  `room_surface_draw` + `image_props` + `model_instances` together.
- **Instrumentation**: sort visual rows by `render` descending, dump the top 20
  with the full stage breakdown and `--dump-hw` for each.
- **Baseline**: render max 2,263,382 vs mean 1,404,382.
- **Expected**: `room_surface_draw` at its p99 1,451,468 plus `image_props` at
  259,271 plus `model_instances` at 195,507 accounts for over 80% of the worst
  frames.
- **Reject if**: a stage outside this set dominates the tail, which would
  redirect the worst-case work.
- **Where**: emulator. **Required before any worst-case claim.**

### J6. The real cortex_v1 vs cortex_v3 comparison

- **Hypothesis**: both projects fit one common `(a, b)` cost model.
- **Instrumentation**: cook and build **cortex_v1 and cortex_v3 from the same
  commit**, record a fresh cortex_v1 tape of comparable length and camera
  behaviour (a corridor traverse with a similar rotation profile), replay both
  with `--profile-log` and `--counter-log`, and regress
  `room_surface_draw ~ surfaces_considered + tri_primitives` across the pooled
  per-frame rows of both.
- **Baseline**: the fits in section A (5,608/743 and 7,020/439).
- **Expected**: a single fit with `a` in 5,000–7,500, `b` in 400–900, R² > 0.85.
- **Reject if**: the projects need materially different slopes, which would mean
  the difference is project-specific and section C is wrong.
- **Visual**: not required (diagnostic).
- **Where**: emulator. **This is the experiment the brief correctly says is
  missing, and it is the one that validates the whole diagnosis.**

### J7. Is cell granularity the multiplier?

- **Hypothesis**: re-cooking cortex_v3 at a smaller sector size lowers
  surfaces-per-drawn-cell toward cortex_v1's 1.35 and lowers
  `room_surfaces_considered` faster than it raises `room_cells_drawn`.
- **Instrumentation**: cook at 1536 (current), 1024, 768; replay each.
- **Expected**: surfaces/cell falls; total considered surfaces falls at least
  15% at 1024.
- **Reject if**: considered surfaces rise, meaning cell overhead dominates.
- **Visual**: hashes will **not** match across sector sizes (different cooked
  geometry partition). Use a perceptual contact-sheet comparison here, and treat
  this experiment as *content guidance*, not a shippable change.
- **Where**: emulator.

### J8. Hardware validation

- **Hypothesis**: the emulator's 3-vblank cadence holds on silicon.
- **Instrumentation**: burn the disc, `fps-overlay`, plus a hardware timer
  around the render stage.
- **Expected**: within 10% of the emulator per-frame figure, consistent with the
  existing `emulator-matches-console-perf` finding.
- **Reject if**: hardware is more than 20% worse, which would mean the I-cache
  or RAM timing model diverges under this access pattern.
- **Visual**: photograph the same route positions and compare against the
  contact sheet.
- **Where**: **hardware, mandatory.** Nothing here is proven until it runs on
  the console.

---

## K. Worst-case performance proof

Averages are not the acceptance criterion. Here is the chain that turns "20 FPS
on average" into "no frame exceeds 3 vblanks", and where it currently breaks.

### The bound chain

| Quantity | Bound source | cortex_v3 value | bounded? |
|---|---|---|---|
| Rooms drawn | `room_stream_visible_limit` = 10; observed `room_active_chunks` max 4 | 4 | yes, by config |
| Portal windows retained | one union per room | 1 | yes, by construction |
| Cells tested | sum of `visible_cells` over active rooms | max 79 | **no static bound** |
| Cells accepted | subset of the above | max 55 | **no static bound** |
| Surfaces visited | `sum(cell.surface_count)` over accepted cells | max 180 | **no static bound** |
| Unique vertices projected | `min(sum(cell.vertex_count), room_vertex_count)` | max 304 | yes, per room |
| Subdivision children | one level, 3x3 lattice, bounded by profile | implicit | yes, but not stated |
| Crack-cover primitives | one per subdivided root | implicit | yes |
| GPU packets | `PRIMITIVE_PACKETS` = 86,016 bytes; `tri_primitive_remaining` shows a **1,536** pool | max 625 used | yes, hard |
| OT operations | one per primitive, OT = 8,192 bytes / 2,048 slots | max 701 commands | yes, hard |

**Three rows are unbounded, and they are the three that drive cost.** The pool
and OT limits are hard but sit at 1,536 and 2,048 against observed maxima of 625
and 701, so they bind at roughly 2.5x the observed load. They will not save you;
they will only stop a crash.

### What a real bound requires

The cooker must compute, offline, over every (source cell, orientation bucket)
pair in the level:

```
max_surfaces_per_frame = max over camera cells C, yaw buckets Y of
    sum over rooms R admitted from (C, Y):
      sum over cells c in R with mask bit set and AABB in frustum(C, Y):
        cells[c].surface_count
```

With F2's `PortalCellMask` this is *directly computable* from data the cooker
already builds. The orientation sweep at, say, 16 yaw buckets x 3 pitch buckets
over 107 cells is ~5,100 evaluations. Seconds.

Then the frame-cost bound follows from the F1 cost model:

```
render_worst <= a_new x max_surfaces
              + b x max_primitives
              + room_cell_select(max_cells)
              + player_max        (213,254 measured, geometry is fixed)
              + image_props_max   (369,214 measured)
              + model_max         (197,735 measured)
              + fixed             (sky 36,007 + world_flush 56,592 + camera 92,251 + rest)
```

Requiring `render_worst <= 1,191,566` and substituting the measured maxima:

```
1,191,566 >= a_new x S_max + 743 x P_max + 213,254 + 369,214 + 197,735 + 195,000
```

The four non-room terms already total **975,203 cycles**. That leaves **216,363
cycles** for all room work in the worst frame.

**This is the finding that matters most for the "stable" requirement, and it is
not in the current framing.** Even with a perfect room renderer costing zero,
the measured maxima of `player` + `image_props` + `model_instances` + fixed
overhead leave only 216k for the room. At `a_new` = 1,500 and `b` = 743 that
allows roughly **S_max = 60 surfaces and P_max = 160 primitives** in the worst
frame, against current maxima of 180 and 625.

Two of those four terms are not simultaneous in practice: `model_instances` p50
is 4,742 and `image_props` p50 is 99,049, so the true joint worst case is well
below the sum of marginals. **J5 must measure the joint tail**, not the sum of
maxima, or this bound will be needlessly pessimistic and drive over-engineering.

### The honest verdict

**A strict worst-case proof is not possible with the current content model**,
because nothing bounds surfaces-per-frame. What must be introduced:

1. **A cooker-computed `max_surfaces_per_frame` per level**, from the F2 masks
   and an orientation sweep.
2. **A cooker hard error** when it exceeds a budget constant (start at 90,
   derived from the 216k figure once J5 replaces the sum-of-marginals with the
   joint tail).
3. **A runtime assertion** in debug builds that the per-frame count never
   exceeds the cooked bound, catching cooker/runtime divergence.

Without those three, "stable 20 FPS" is a measurement on one tape, not a
property of the engine. With them it is a property of the level, checkable at
build time, and a level that violates it fails to cook instead of stuttering.

---

## L. Staged roadmap

### Stage 1 — instrumentation only

| | |
|---|---|
| **Work** | J1 (I-cache stall counter), re-enable `room-surface-profile` in a diagnostic build, add the link-map size check to `make`, J5 (tail dump), **J6 (the real cortex_v1 comparison)** |
| **FPS** | 0 |
| **RAM/ROM** | 0 shipping |
| **Eng risk** | low |
| **Visual risk** | none |
| **Rollback** | delete |

Non-negotiable prerequisite. Everything after this is currently unfalsifiable.

### Stage 2 — low-risk exact optimisations

| | |
|---|---|
| **Work** | J3: `#[inline(never)]` on the four leaves. No logic change. |
| **FPS** | +0.4 to +0.8 (80k–150k of 1,404,382) |
| **RAM/ROM** | ROM roughly neutral, possibly smaller |
| **Eng risk** | low, but **this is the 8.3/8.4 failure mode**: measure total runtime and delivered frames, not one stage |
| **Visual risk** | none, hashes must match |
| **Rollback** | one attribute per function |

### Stage 3 — F1, the draw records

| | |
|---|---|
| **Work** | `SurfaceDrawRecord` + `OptionVariantTable` + the rewritten loop |
| **FPS** | **+2.5 to +3.5** (340k–480k), taking gameplay to roughly 17–18 |
| **RAM** | +5,824 bytes resident. ROM: none (built at residency) |
| **Eng risk** | medium. The backface/winding class bits are the sharp edge |
| **Visual risk** | medium-high without tests, near-zero with tests 1–5 in I6 |
| **Rollback** | keep the old path behind a cargo feature for one release, A/B by hash |

### Stage 4 — F2, the cooked portal masks

| | |
|---|---|
| **Work** | cooker mask emission, manifest field, runtime OR + bit test |
| **FPS** | **+0.8 to +1.5** (40k cell-select, plus the compounding surface reduction) |
| **RAM/ROM** | +48 bytes level, +8 bytes runtime |
| **Eng risk** | medium. Cooker format change, needs a version bump |
| **Visual risk** | low if the mask defaults to `u64::MAX` and test 7 in I6 exists |
| **Rollback** | ignore the manifest field, fall back to the rectangle union |

### Stage 5 — worst-case bound and hardware validation

| | |
|---|---|
| **Work** | cooker `max_surfaces_per_frame` + hard error; J8 on silicon |
| **FPS** | 0, but converts "20 FPS on this tape" into "20 FPS by construction" |
| **RAM/ROM** | 0 |
| **Eng risk** | low technically; may reject existing content, which is a design decision, not a bug |
| **Visual risk** | none |
| **Rollback** | warn instead of error |

Projected cumulative: **14.01 → 18.5–19.5 FPS mean** after stage 4, with the
worst case still needing stage 5 plus whatever J5 says about the
`image_props` / `model_instances` tail.

**I am not claiming stages 1–4 reach a stable 20.** They reach an average 19-ish
and a much tighter distribution. The last stretch is a tail problem, and section
K shows the tail is currently constrained as much by `player` + `image_props` +
`model_instances` (975k of measured maxima) as by the room renderer. Be
suspicious of any plan, including this one, that claims the mean and the tail
fall to the same fix.

---

## M. What not to do

**Never return to single-aperture room clipping.** The failure was structural,
not a bug: one aperture is not a sound window for a room reachable by several
paths. F2 is the correct generalisation because it is an *exact per-cell answer
per portal*, OR-ed, rather than a bounding approximation.

**Never remove geometry, draw distance, subdivision, or crack-cover.** 8.1
removed crack-cover for a measurable speedup and 8,324 differing pixels. The
acceptance bar is frame-hash equality; a pixel delta is a failed experiment, not
a tradeoff to weigh.

**Never add a broad view cache.** 8.4 failed because it added bookkeeping and
memory traffic to a loop that is already fetch-bound at 8.66 I-cache images.
Adding data traffic to a cache-thrashing loop is negative-sum. F1 removes the
recomputation rather than caching it, which is why it does not repeat 8.4.

**Never accept a source-level micro-optimisation measured in isolation.** 8.3
improved the model stage and regressed total cadence. In a 4 KB direct-mapped
cache, moving code changes which lines alias, and a local win can be a global
loss. **Every future perf claim in this subsystem must report: render mean, render
p99, render max, total runtime, delivered frame count, and the link-map size of
the changed function.** If a result does not include all six, it is not a result.
Add the link-map size check to `make` so this is enforced rather than remembered.

**Do not chase the GTE.** `room_project` is 1.5% of render at 159 cycles per
vertex. This is consistent with the existing `gte-cull-depth-scalar-by-choice`
and `keep-work-on-gte-not-cpu` findings and there is nothing left there.

**Do not chase portal traversal.** 0.6%.

**Do not reorder surface emission for cache locality** without first proving OT
slot keys are a total order within a cell and validating by frame hash. It is a
real further win and it is out of scope until F1 lands.

---

## N. Final recommendation

**Highest-leverage next implementation**: F1, the per-surface `SurfaceDrawRecord`
plus the 12-entry `OptionVariantTable`, built in
`prewarm_indexed_cached_room_quads` and consumed by a rewritten
`draw_indexed_cached_room_vertex_lit_visible_cells` whose four emit paths are
`#[inline(never)]`. It attacks the one term that is 36–44% of the render stage
and it changes no geometry, no projection, no subdivision, and no ordering.

**First benchmark to run**: not F1. Run **J1 and J6** first, in parallel.

- J1 (I-cache stall counter) decides whether F1's record table or F1's function
  splitting is the primary mechanism. Both ship together, but if stalls turn out
  to be over 400k the sequencing and the expected savings change.
- J6 (a real current-build cortex_v1 vs cortex_v3 capture) is the experiment the
  brief correctly identifies as missing, and it is the one that can falsify this
  entire diagnosis. My prediction is on the record in section C: a single common
  `(a, b)` fit with R² > 0.85.

**Expected cycle saving**: **340,000–480,000 cycles per visual frame** from F1
(confidence medium-high on direction, medium on magnitude), plus **80,000–150,000**
from the function split, plus **~40,000** from F2's cell-select collapse.
Total **460,000–670,000** against a required mean cut of **212,816**. That
overshoots the mean target with margin, which is what you want, because the
p99 needs 1,023,899 and even the optimistic end of this range does not reach it
alone.

**Most important correctness risk**: the backface and winding class bits. The
`WARMED_ROOM_QUAD_REVERSE` vs `WARMED_ROOM_QUAD_REVERSE_FRONT` distinction, and
the ceiling `reverse_quad_winding` applied *before* the cull test at
`indexed_cache.rs:1244-1248`, are exactly the kind of detail that survives code
review and fails on one wall in one room at one angle. Write test 5
(`backface_class_matches_legacy_cull`, swept camera poses over every cooked
surface) **before** writing the record.

**Go/no-go**: proceed to F1 if and only if J6 produces a single pooled
`(a, b)` fit with `a` in 5,000–7,500 and R² > 0.85. If cortex_v1 needs a
materially different slope, the fixed-tax model is wrong, section C is wrong,
and the cause is project-specific: stop and re-diagnose from the J5 tail dump
instead.

---

### Two corrections to the brief's framing, for the record

1. **`visual_render_task` includes `present`.** The "1.71M per visual frame"
   figure carries 305,407 cycles of vblank-edge wait that is quantisation, not
   work. The number to optimise is `render` = 1,404,382, and the budget for it
   at 20 FPS is 1,191,566, so the mean gap is 15%, not the ~50% the 1.71M-vs-1.13M
   comparison implies.

2. **The subdivision-amplification hypothesis is backwards.** cortex_v3 emits
   **4.64** primitives per surface; cortex_v1 emits **14.96**. cortex_v3 is the
   *low*-amplification project. Its problem is that it pays a large fixed tax on
   a small payload, which is the opposite failure mode and wants the opposite
   fix.
