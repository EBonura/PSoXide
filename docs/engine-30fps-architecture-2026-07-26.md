# Engine architecture review: sustaining 30 FPS on original PlayStation for cortex_v1 and cortex_v3

Repo `/Users/ebonura/Desktop/repos/PSoXide`, branch `emu/accuracy-from-silicon`, commit `0bab5cd8`.

This document extends `docs/cortex-v3-render-architecture-2026-07-26.md` (the 20 FPS review, "the predecessor") to the 30 FPS target. Every number below was recomputed from the supplied CSVs, the MIPS link map (`/tmp/cortex-v3-final.map`), and a fresh read of the renderer, cooker, scheduler, and portal code at this commit. Where the predecessor's findings hold, I cite them; where the new evidence overturns them, I say so explicitly.

Three headline corrections to the brief's framing, established below and used throughout:

1. **The "34% average reduction" is understated.** `visual_render_task = render + present`, and `present` averages 305k cycles of idle vblank-edge wait, while the sim-only tick (174k) that must share the same two vblanks is excluded. The honest requirement is a **43.5% mean render cut and a 65% worst-frame cut** (section B).
2. **Subdivision is the common case, not the exception.** With `far_depth = 5 x sector = 7,680` units and cortex_v3 rooms about 9,216 units across, roughly **80 of the 88.6 surfaces drawn per frame subdivide** (5 packets each). The renderer's "fast path" (warmed whole quad, ~250 to 350 instructions) is the rare path; the 5-packet subdivision emission (~10,000 cycles per quad) is the main loop, and it is built as an exception path.
3. **cortex_v1 does not sustain 30 FPS either.** On the July 23 capture it runs 28.87 effective FPS with **12.2% of frames exceeding the two-vblank work budget** (render p95 967,705 against a ~915k budget). Both projects fail the same requirement; they fail through the two ends of the same cost function.

---

## A. Executive verdict

**The primary architectural problem: the renderer treats a cached room surface as a record to be interpreted at draw time, and a GPU packet as a value to be constructed from scratch per primitive.** Measured consequence: **1,951 cycles per emitted primitive** in cortex_v3 (1,118 in cortex_v1), where a template-patched primitive on this hardware should cost 300 to 450. The interpretation tax re-derives, per surface per frame, facts that are constants of the cooked content (kind, material resolution, sidedness, winding, risk thresholds, subdivision eligibility, option structs), across a code path whose inner function alone is 35,488 bytes against a 4,096-byte direct-mapped I-cache. The emission tax rebuilds full 56-byte packets per subdivision child (14 word stores, twice, via the arena), re-derives UV and color midpoints in i32 every frame for geometry that never moves, and copies the 80-byte `WorldSurfaceOptions` three to six times per surface.

**Why it hits cortex_v3 hardest:** v3's content shape maximizes both taxes at once. Its cells hold 3.50 surfaces each (v1: 1.35), its portal layout leaves 22.4% of its surfaces surviving to the draw loop (v1: 3.4%), and its sector size (1536) puts essentially every visible surface inside the subdivision band, so nearly every drawn surface pays the 5-packet emission. Result: 88.6 surfaces and 411 primitives per frame at the highest per-unit cost in the codebase, 802k cycles in `room_surface_draw` alone, 1.40M in render, against a 790k render budget.

**How it affects cortex_v1:** v1 walks few surfaces (15.6) but amplifies them 15x into 234 primitives (near-band quads fan out through the hardware-extent splitter), so it pays mostly the per-primitive emission tax. Its render mean (850k) sits just under its own budget (~915k, its sim ticks are cheaper) and its p95 (968k) sits just over, which is exactly what "28.87 FPS with 12.2% dropped intervals" looks like.

**The single most important engine change:** compile the drawing at room-residency time instead of interpreting it at draw time. Concretely: a per-surface `SurfaceDrawRecord` (16 bytes) plus per-quad lattice attribute templates (28 bytes) built where `prewarm_indexed_cached_room_quads` already runs, a 12-entry per-frame `OptionVariantTable` replacing all per-surface options construction, subdivision children emitted by `copy_payload_from` + patch instead of full construction, and cell admission driven by cooked per-portal cell bitmasks OR-ed across every admitting path. One architecture, three consumers (room surfaces now, image props and models next), no geometry, projection, subdivision topology, or ordering change.

**Can it deliver 30 FPS?** Split answer, argued quantitatively in sections B, N, O:

- cortex_v1: **yes, with high confidence**, from the room-renderer work alone. Its worst frames need roughly a 15 to 25% cut and the fix removes 2 to 4x that.
- cortex_v3 mean-30: **yes, medium-high confidence.** The full room-side program projects render from 1,401k to 620 to 780k against the 790k budget.
- cortex_v3 sustained-30 (every gameplay frame inside two vblanks): **credible but not attainable from the room renderer alone.** The measured worst frames carry `player` 213k + `image_props` up to 369k + `model_instances` up to 198k; after a perfect room fix the pathological frame still lands near 850 to 960k. Sustained 30 requires applying the same emission architecture to props, a bounded-tail policy for models/player, and the cooker worst-case gate of section N. That is a roadmap, not a single change, and the guarantee at the end is by construction (cooker-enforced), not by measurement.

---

## B. Two-vblank budget reconstruction

### B1. Units and timing verification

- **Profiler cycles** are the emulator CPU cycle counter: a flat BIAS of 2 cycles per instruction (`emu/crates/emulator-core/src/cpu.rs:2600`) plus modeled memory timing and a faithful 4 KB direct-mapped I-cache with per-line fill stalls (`emu/crates/emulator-core/src/cpu/icache.rs`, `bus/memory_timing.rs`). Prior silicon work established the console tracks these numbers closely (the one known divergence regime is GPU fill-rate, which the emulator does not model at all: `ot_wait` reads 66 cycles here and is meaningless for the GPU).
- **Vblank length.** The profiler uses 33,868,800 / 60 = **564,480 cycles**. Real NTSC silicon: 263 scanlines x 3,413 GPU clocks at 53.693175 MHz gives a 59.826 Hz field rate, i.e. **566,124 CPU cycles per vblank**. The profiler's constant is therefore **0.29% conservative**, which is the correct direction. Confirmed in-data: `present` max is 569,015, one full vblank of edge wait. The two-vblank budget **1,128,960** stands.
- **One CSV row = one 60 Hz sim tick**, not one vblank pair. `visual_render_task = render + present` (verified: 1,400,869 + 304,614 = 1,705,483 vs measured 1,706,108; the residue is stage-boundary jitter). `update` is charged separately on the same row. All per-frame figures below are over the gameplay window (last 1,644 rows) and, for render-side stages, over the 385 visual rows only.

### B2. The per-delivered-frame identity (current state)

```
cycles per delivered frame = render(visual tick) + update(visual tick) + present(edge wait)
                           + (ticks_per_frame - 1) x frame_cycles(sim-only tick)
```

Measured, corrected build: 1,400,869 + 153,514 + 304,614 + 3.27 x 173,665 = **2,426,882** cycles = 4.30 vblanks per delivered frame = 13.96 FPS, against a measured 14.05 (385 visuals / 1,644 ticks). The identity closes to under 1%.

This is also why the brief's arithmetic misleads twice over: the 1.71M "visual task" includes 305k of quantization wait that evaporates once frames fit, and it excludes the 174k sim-only tick that does not.

### B3. The 30 FPS budget

At a locked two-vblank cadence there are exactly two sim ticks per frame: one sim-only, one visual.

```
1,128,960                       two vblanks
 - 173,665                      sim-only tick (mean; p95 261,814)
 - 153,514                      update on the visual tick (mean; p95 254,861)
 -  10,000                      frame glue (frame_cycles - update - visual task, measured ~10k)
 --------------------------------
 render budget R  ~= 791,781    (mean-shaped; every frame must satisfy
                                 render_i + update_i + simtick_i + glue <= 1,128,960)
```

| Case | current render | budget | required cut |
|---|---:|---:|---:|
| mean | 1,400,869 | ~790k | **-43.5%** |
| p50 | 1,392,310 | ~790k | -43.3% |
| p95 | 1,995,092 | ~790k | -60.4% |
| p99 | 2,215,299 | ~790k | -63.6% |
| **max** | **2,263,382** | ~790k | **-65.0%** |

Cross-check by direct simulation over the 385 visual rows (render_i cut by X%, plus that row's update, plus the mean sim tick): a 43% cut fits **52%** of frames; 50% fits 75%; 60% fits 94%; **65% fits 99%**. Mean-30 and sustained-30 are different engineering problems, roughly 20 points of cut apart.

For cortex_v1 (cheaper sim: sim-only tick 135,145, visual update 69,102) the render budget is **~915k**; current render is 849,513 mean / 967,705 p95 / 1,059,186 max, so v1 needs about **-15% at p95 and -25% at max**, and nothing at the mean.

### B4. Render decomposition (cortex_v3, gameplay, visual rows)

| Stage | mean | p50 | p95 | p99 | max | % of render |
|---|---:|---:|---:|---:|---:|---:|
| **room** | **887,428** | 857,818 | 1,413,066 | 1,555,883 | 1,687,923 | **63.3%** |
| . room_surface_draw | 802,528 | 771,038 | 1,292,159 | 1,435,618 | 1,563,925 | 57.3% |
| . room_cell_select | 49,577 | 45,914 | 80,585 | 86,050 | 90,369 | 3.5% |
| . room_project | 20,456 | 21,155 | 33,707 | 38,148 | 45,545 | 1.5% |
| . room_visible_list | 6,170 | 4,263 | 10,163 | 28,262 | 71,939 | 0.4% |
| . room_depth_prep | 3,995 | 3,533 | 9,591 | 10,611 | 12,517 | 0.3% |
| player | 200,262 | 202,166 | 211,036 | 212,524 | 213,254 | 14.3% |
| image_props | 130,495 | 97,449 | 240,779 | 259,178 | 369,214 | 9.3% |
| world_flush | 46,737 | 45,987 | 53,160 | 54,469 | 56,592 | 3.3% |
| model_instances | 36,528 | 4,742 | 193,472 | 195,347 | 197,735 | 2.6% |
| sky | 23,781 | 34,319 | 35,318 | 35,450 | 36,007 | 1.7% |
| portal_visibility (charged on some rows) | 8,490 | 0 | 40,796 | 46,793 | 51,207 | 0.6% |
| ot_submit / ot_wait / frame_clear | 227 | | | | | 0.0% |
| **render remainder** | **~67,000** | | | | | 4.8% |

The remainder is owned by, in code order (`playtest_scene.rs`): OT/arena begin, `active_room_draw_order`, per-room material/lighting/camera setup **built twice** (main loop and the instances-in-front pass), `draw_water`, entity markers, the ~45-counter telemetry block at `:1040-1149` (two volatile Expansion-2 stores each, uncovered by any stage), particles, and `render_overlay` (HUD/atmosphere, charged to RENDER by the second begin/end pair in `app.rs:703-705`).

Two important relocations against the predecessor's table: **`camera` (42,191/tick) is charged inside `update`, not render** (`playtest_update.rs:549-551`); it runs on every 60 Hz tick and costs ~84k per delivered 30 FPS frame, inside the 327k update reservation. `portal_visibility` also runs update-side (under `UPDATE_WINDOW`), event-driven (p50 = 0, ~40 to 50k when it fires).

### B5. Inside room_surface_draw: the cost is per-primitive, and subdivision is the norm

- Per considered surface: 802,528 / 88.62 = **9,055 cycles** (~4,500 instructions at BIAS 2).
- Per emitted primitive: 802,528 / 411.28 = **1,951 cycles** (~975 instructions). cortex_v1: **1,118**.
- Emission structure: with `max_levels = 1` (`runtime_config.rs:249`) a subdividing quad emits **4 lattice children + 1 crack-cover underdraw = 5 packets** in the middle band. Solving `5S + (88.6 - S) = 411` gives **S ~= 80**: nearly every drawn surface subdivides. The measured warmed non-subdividing path costs only ~250 to 350 instructions (agent-verified instruction walk of `try_submit_encoded_warmed_room_quad`), so the ~10,000 cycles per subdividing quad carry the stage.
- Where those ~10k go per subdividing quad (from the code walk of `world_pass_gouraud.rs` / `render3d.rs` / `prim.rs`): 4x full `QuadTexturedGouraud` construction plus arena re-write (~140-200 instructions), 384 bytes of leaf-vertex struct copies plus 16x UV unpack/repack (~120-160), 3 to 6 copies of the 80-byte `WorldSurfaceOptions` (~90-120), 40 midpoint ops in i32 (~80-120), 5x depth average + `slot_depth` divide + command push + stats merge (~100-130), 4x CPU `view_vertex` re-derivation and a 13-CTC2 GTE state reload per surface, plus I-cache misses across a subdivision family spanning ~30 KB of .text (`submit_adaptive_cached_room_quad` 9,564 B; leaf/split functions 832 to 6,312 B each; container function 35,488 B).
- What is *not* the problem: `room_project` runs the full GTE projection at **159 cycles/vertex** (RTPT batching, verified); portal traversal costs 0.6%; OT insertion in bucketed mode is ~11 instructions at push and 18 per packet at flush (`world_flush` = 104 cycles/command).

### B6. Sim, streaming, GPU

- Update tick decomposition (mean over all gameplay ticks): camera 42,191, sim_solve 35,420, game_logic 28,878, sim_residency 13,044, sim_collision 11,725, update_window 11,588 (max 138,915: synchronous window rebuild on room crossing), sim_room_track 7,201 (max 126,603: same event), actor 2,010. Streaming and prewarming already run on odd (non-visual) ticks by design (`vram_runtime.rs:180`), and `cd_room_chunk_loads` is zero throughout gameplay; streaming is **not** on the visual critical path today.
- GPU/DMA on silicon: the phase-1 pipeline kicks the OT with `submit_async` as the last act of the visual turn and drains it at the next visual's start; the uncovered part is the `draw_sync` spin after the next frame's CPU build (`app.rs:699-701`). ~640 textured Gouraud primitives at 320x240 with modest overdraw is plausibly 0.3 to 0.6 vblank of raster; the covering window at a 2-vblank cadence is the odd tick (~174k = 0.31 vblank) plus the present slack. This is the one budget line the emulator cannot validate, and it is experiment E10.

---

## C. Facts, inferences, and unknowns

| # | Claim | Status | Basis | Resolving measurement |
|---|---|---|---|---|
| 1 | 1 vblank = 564,480 profiler cycles, 0.29% conservative vs silicon | **Fact** | arithmetic + present max 569,015 | none needed |
| 2 | `visual_render_task = render + present`; `camera` and `portal_visibility` are update-side | **Fact** | stage sums close to <1%; code sites `app.rs:678-710`, `playtest_update.rs:549` | none |
| 3 | Render budget at 30 FPS ~= 790k (v3), ~915k (v1) | **Fact** (given tick costs) | B3 identity | none |
| 4 | 99.2% of v3 gameplay frames over 2 vblanks; 12.2% of v1 frames | **Fact** | per-row simulation | none |
| 5 | 9,055 cyc/surface, 1,951 cyc/primitive (v3); 1,118 (v1) | **Fact** | CSV ratios | none |
| 6 | Portal-union cull removed 26% of surfaces and 0.2% of triangles | **Fact** | HEAD vs corrected CSVs | none |
| 7 | ~80 of 88.6 drawn surfaces subdivide (5 packets each) | **Strong inference** | 5S+(88.6-S)=411; far_depth 7,680 vs room extent | E2 (surface-profile build): read `room_surf_whole_quads` vs split counters |
| 8 | Warm non-subdividing path ~250-350 instructions | **Strong inference** | instruction-level code walk | E2 proportions |
| 9 | Per-surface interpretation tax 5,600-7,020 cyc (predecessor's two fits) | **Reinterpreted**: the fits are real but collinear (prims ~= 5 x surfaces in v3), so "a" absorbs the subdivision emission; the physical quantity is ~10k per subdividing surface | fit + counter structure | E3 pooled regression on fresh same-commit captures |
| 10 | I-cache stalls are 15-25% of room_surface_draw, not the majority | **Hypothesis** | footprint arithmetic (35,488 B loop, 4,096 B cache) | E1 stall counter |
| 11 | image_props interior (what its 130k mean / 369k max buys) | **Unknown** | `box_props`/`image_cards` sub-counters all dark | E2 with `PSXO_PROFILE_BOX_PROPS=1` |
| 12 | v1 amplification (15 prims/surface) comes from near-band hw-extent splitting, not 2-level TR | **Hypothesis** (max_levels=1 makes 2-level impossible on current code; capture is from July 23, possibly older config) | `runtime_config.rs:249`, tests | E3 fresh v1 capture with counters |
| 13 | v1 and v3 share one per-unit cost model on the same commit | **Hypothesis** (predecessor predicted a in 5,000-7,500, R^2>0.85) | two-point fits | E3 |
| 14 | GPU raster fits the covered window at 30 FPS on silicon | **Unknown** | emulator does not model GPU time | E10 hardware timer + tear check |
| 15 | The worst-frame joint tail is below the sum of stage maxima | **Fact** (worst-20 render rows: image_props 185k not 369k, model 58k not 198k) | CSV joint analysis | keep monitoring in E3 |
| 16 | Streaming does not land on visual frames | **Fact** (design + zero chunk loads in window) | agent walk of `vram_runtime.rs`, CSV | none |
| 17 | Cadence-independent A/B hash comparison is currently impossible (455 vs 490 presented frames) | **Fact** | run comparison | E0 lockstep-visuals mode fixes it |
| 18 | Present-flip in-place packet patching is safe behind the DMA drain | **Fact** (documented invariant) | `world_pass_gouraud.rs:1766-1770` | none, but E6 must preserve it |

---

## D. Same engine, different workload

Measured side by side (v1 numbers are the July 23 capture: older build, different route; treat ratios as indicative until E3):

| Metric | cortex_v1 | cortex_v3 | ratio |
|---|---:|---:|---:|
| Effective FPS | 28.87 | 14.05 | 0.49x |
| render mean / p95 / max | 849,513 / 967,705 / 1,059,186 | 1,400,869 / 1,995,092 / 2,263,382 | 1.65x / 2.06x / 2.14x |
| room_surface_draw | 261,143 | 802,528 | 3.07x |
| surfaces considered / frame | 15.62 | 88.62 | **5.67x** |
| cells drawn / frame | 11.54 | 25.35 | 2.20x |
| surfaces per drawn cell | 1.35 | 3.50 | 2.59x |
| primitives / frame | 233.63 | 411.28 | 1.76x |
| primitives per surface | **14.96** | **4.64** | 0.31x |
| cycles per primitive | 1,118 | 1,951 | 1.75x |
| projected vertices / frame | 25.8 | 128.4 | 4.98x |
| active room chunks (mean/max) | 0.68 / 1 | 2.55 / 4 | |
| player | 139,041 (bimodal) | 200,262 (flat) | |
| image_props | 20,597 | 130,495 | 6.3x |
| update(visual) + sim-only tick | 69,102 + 135,145 | 153,514 + 173,665 | 1.60x |

Item by item, per the brief's list:

- **Visible-room count**: v3 draws 2.55 rooms mean (max 4), v1 usually 1. More rooms means more per-room prologues (options, lighting, camera-for-room, twice each with the two-pass model draw) and more cells admitted.
- **Portal-path multiplicity**: v3's 6-portal loop layout yields multiple frustums per room; the rectangle union is therefore loose exactly where v3 needs it tight. v1's 7 portals over a 51x31 open grid yield mostly single paths.
- **Stacked/overlap rooms**: neither project stacks floors; overlap rooms draw unclipped by design (no frustum). Cost neutral here, correctness-relevant in J.
- **Candidate and accepted cells**: v3 45.8 candidates -> 25.4 accepted; v1 53.6 -> 11.5. v1's candidates die at the frustum test (open map, narrow view); v3's survive (interior spaces). Every accepted v3 cell drags 3.50 surfaces in whole-cell granularity (`indexed_cache.rs:533`).
- **Surface density and material boundaries**: v3's stacked wall bands and material splits produce ~3.71 cooked surfaces per populated cell (397/107); v1 1.95 (461/237).
- **Unique projected vertices**: 128 vs 26 per frame; both cheap (159 cyc/vertex), neither a lever.
- **Camera-relative polygon size and subdivision**: v3 sector 1536 -> far band 7,680 covers essentially the whole room: ~90% of drawn surfaces subdivide at 5 packets. v1 sector 1664 -> far 8,320, but its route holds few, huge, close quads: fewer surfaces, far deeper amplification (15x) through near-band hardware-extent splitting.
- **Generated primitive count**: 411 vs 234; both far below the 1,536 packet / 2,048 OT caps.
- **Crack-cover count**: ~80/frame in v3 (one per subdividing quad in the middle band), the +25% packet term.
- **Material switching**: not measured separately; bounded by the per-surface material resolution that the record table removes.
- **Packet count / OT insertions**: 449 world commands at 104 cyc each at flush; not a lever (bucketed ordering already won this fight, comment in `Cargo.toml`).
- **Overdraw / GPU completion**: unmodeled; E10.
- **Streaming**: off the visual path in both.

**Why one architecture improves both:** v3's dominant term is (surfaces x interpretation tax) + (primitives x emission tax); v1's is almost purely (primitives x emission tax). The record table plus template emission cuts both coefficients; the portal cell masks cut v3's surface count specifically; nothing in the design keys on either project's content.

---

## E. Like-for-like cortex_v1 vs cortex_v3 benchmark (E3 in the matrix)

The existing v1 numbers are from an older build and route. The valid comparison:

1. **Build both from this commit, identical features.** For each project: `make cook-playtest PROJECT=editor/projects/<p>/project.ron`, `make build-editor-playtest`, then `mkisopsx` with the world pack **and the UI pack flags** (the omission failure mode is documented in the playtest runbook). Two discs, one binary family, cook manifests archived next to the CSVs.
2. **Two routes per project, one authored, one synthetic.**
   - Authored: an editor-recorded tape per project (Manny records; the existing cortex_v3 tape is reused as-is). Target ~1,600 polls, entering at least two portal chains and one widest-vista point.
   - Synthetic: the boot-into-gameplay disc with `--hold-forward`, 1,600 vblanks, as the route-independent control (both projects walk their main corridor).
3. **Instrumentation per run:** `--profile-log`, `--counter-log`, `--dump-hw --dump-hash`, `--steps 2000000000`. One extra diagnostic run per project with the `room-surface-profile` feature and `PSXO_PROFILE_BOX_PROPS=1` (proportions only; that feature disables warm fast paths, so never read absolute cycles from it).
4. **Collect** (all present in the current CSV schema): effective FPS, render mean/p50/p95/p99/max, visual task, per-stage table of B4, `room_cells_considered/drawn/culled`, `room_surfaces_considered`, `room_projected_vertices`, `tri_primitives`, `world_commands`, `room_active_chunks`, portal counters (`portal_vis_portals_tested/accepted`, reject and cap counters, frustum count), streaming counters, `resident/drawn/visible` masks, VRAM+display hashes, and the `--dump-hw` frame. From the diagnostic run: whole-quad vs split vs subdivided counts, screen/backface cull counts, packet fill vs push vs depth vs command proportions, and the image_props interior.
5. **Route normalization.** Do not chase identical camera paths; normalize by regression, not by averaging: pool per-frame rows from both projects and fit `room_surface_draw ~ a x surfaces_considered + b x tri_primitives` (and `room ~ ... + c x cells_drawn`). The falsifiable prediction, carried over from the predecessor and now with the collinearity caveat on record: one common fit with R^2 > 0.85; if the projects need materially different slopes, the shared-cost-model diagnosis is wrong and the plan stops for re-diagnosis. Content-driven differences (surfaces per cell, subdivision fraction) remain visible as different *operating points* on the same line, which is exactly what should be reported, not hidden.
6. **Comparable-event screenshots**: dump frames at each portal crossing (detectable from `current_room` transitions in the CSV) and at the max-`room_surfaces_considered` frame of each run.

---

## F. Root-cause ranking

Format: evidence / confidence / contribution / scaling / v1-vs-v3 / falsifier / bound.

**F-1. Per-primitive emission cost (packet construction, midpoints, options copies, leaf copies).** Evidence: 1,951 cyc/prim vs a ~350-cycle patch-path bill of materials; instruction walk of `with_packet_material_packed_uv_words` (14 stores, then `push_packet` writes all 14 again), 40 midpoint ops and 16 UV repacks per quad, 3 to 6 `WorldSurfaceOptions` copies. Confidence: high. Contribution: with ~411 prims, order 450 to 650k/frame in v3; the dominant term in v1 (234 prims x ~800 excess = ~190k of its 261k stage). Scales linearly with primitives, so it grows exactly where frames are worst. Falsifier: E6 (template emission) must cut room_surface_draw by >=200k with primitives unchanged. Bound: CPU instruction count, plus I-cache (the path spans ~30 KB).

**F-2. Per-surface interpretation tax (classification, options, material resolution re-derived per frame).** Evidence: the 26%-of-surfaces-removed / 0.2%-of-triangles-changed portal experiment priced fully-classified-then-discarded surfaces at ~7k each; ~19 branches per surface whose outcomes are static per surface (readable from `kind_flags`/`ready`); 9 draw-invariant branches re-tested per surface. Confidence: high. Contribution: order 150 to 350k/frame in v3 (bounded by the warm-path floor and the fit's collinearity); small in v1 (15.6 surfaces). Scales with surfaces considered. Falsifier: E5 (records) must cut >=150k with primitives unchanged. Bound: CPU + I-cache.

**F-3. I-cache thrash.** Evidence: 35,488 B inner function (+32,604 B `_all_cells` twin, ~86 KB room-render .text) vs 4,096 B direct-mapped cache; the two rejected micro-experiments (8.3, 8.4) behaved exactly like alias-shift regressions. Confidence: medium (mechanism certain, magnitude inferred at 115 to 200k/frame). Falsifier: E1 stall counter; if stalls exceed ~400k the leaf-splitting becomes the primary fix. Bound: instruction fetch.

**F-4. Tail stages: image_props (130k mean, 369k max, interior dark), model_instances (bimodal, 198k max), player (flat 200k).** Evidence: B4; worst-20 analysis shows the joint tail (props 185k, model 58k on worst render frames). Confidence: high on magnitudes, none on props interior. Contribution: caps sustained-30 regardless of room work (section N). v1: props nearly absent, player bimodal. Falsifier/measure: E2 interior dump, then E8. Bound: CPU.

**F-5. Cell selection at 1,083 cyc/candidate.** Evidence: 49,577 / 45.78; per-candidate GTE `view_vertex` + frustum AABB (9 muls) + portal-window test (~20 muls) with `cell_aabb_view_extents` computed twice when a window exists. Confidence: high. Contribution: 50k mean, 90k worst. Scales with candidates (79 max). Falsifier: E7 mask-first ordering must cut it below ~20k. Bound: CPU.

**F-6. Render-remainder overhead: duplicated per-room setup in the two-pass model draw, ~45-counter telemetry block, overlay.** Evidence: agent walk; remainder ~67k. Confidence: high. Contribution: 25 to 40k recoverable. Falsifier: E4b (move counters off the visual path, dedupe pass-2 setup). Bound: CPU.

**F-7. Update-side spikes: synchronous `load_active_room_window` on room crossing (update_window max 139k, room_track max 127k on the same event), camera solve 42k every tick.** Evidence: CSV maxima + code sites. Confidence: high. Contribution: occasional +200k on a single tick, a deadline-miss source at exactly the portal-crossing frames the masks also affect. Falsifier: E9b (spread window rebuild across background ticks) removes the spike. Bound: scheduler.

**F-8. GPU raster window on silicon.** Unmodeled; only E10 speaks to it. Everything above is CPU-side and the emulator's CPU model is trusted per prior silicon correlation.

Explicitly demoted, with the predecessor: GTE/projection (159 cyc/vertex, 1.5%), portal traversal (0.6%, though its divides make it 4x more expensive per tick than it needs to be), OT insertion (104 cyc/command), streaming (off-path), scheduler pacing (drops visuals rather than dilating time, correct).

---

## G. Recommended engine architecture: residency-compiled rendering

The invariant to establish:

> A cached room surface's draw behavior is fully determined at room-residency time except for four projected vertices, one depth value, and one backface sign. The render loop reads precompiled records and patches precompiled packets; it interprets nothing.

Complete data flow:

1. **Cooker inputs** (unchanged): authored `WorldGrid` per Room node, portal markers, materials, light bake. Existing outputs kept verbatim: `.psxw`, `.psxc` (cells 36 B, vertices 12 B, surfaces 40 B), manifest tables including the existing per-cell PVS (`VISIBILITY_CELLS`, `VISIBILITY_PVS_BITS`).
2. **Geometry partitioning** (unchanged): cells = authored sectors; a surface belongs to exactly one cell; vertices deduplicated per room. No re-gridding, no geometry splitting (H forbids changing the authored surface set).
3. **Visibility preprocessing (new, cooked): per-portal destination-cell masks.** For every directed portal record, the cooker computes the set of destination-room cached cells visible through that portal's aperture from **any** point of the source room admitted to the portal (existential sweep, outward rounding, ties set the bit). Representation: a bitset over the destination room's cached-cell ordinals, `ceil(cell_count/8)` bytes, stored in the same deduplicated byte-pool mechanism the PVS already uses (`find_existing_visibility_pvs_bits`). No new content limit: rooms may have up to 32x32 = 1,024 cells and the bitset scales; the predecessor's u64/64-cell proposal is superseded for this reason.
4. **Portal-path representation** (runtime, unchanged): the existing tangent-space frustum BFS (`portal_visibility.rs`), all paths retained, `PortalFrustum` gains nothing. Each frustum already records `(room, source_room, source_portal, depth)`.
5. **Runtime room selection** (unchanged): visible set = traversal output plus overlap rooms; residency and draw order as today.
6. **Runtime cell selection (changed).** Per drawn room R: fold `mask = OR over frustums f with f.room == R of portal_cell_mask[f.source_portal]`; if R has no frustum (root, overlap, capped) then `mask = all-ones`. Candidate loop order becomes: (a) mask bit test (~6 cycles), (b) existing PVS/candidate lookup, (c) GTE view transform + frustum AABB only for survivors. The rectangle-union `PortalCellWindow` is retained and applied after the mask (they reject different things: the mask is camera-position-coarse but aperture-exact; the window is aperture-coarse but camera-exact).
7. **Duplicate suppression** (unchanged, already correct): a surface is owned by one cell; a vertex is projected once per room via the self-initializing bitset.
8. **Vertex projection** (unchanged): RTPT batches, dense/sparse switch as today.
9. **Surface classification (changed): `SurfaceDrawRecord`.** Built at residency inside `prewarm_indexed_cached_room_quads` (which already walks surfaces x materials there): resolved material index, packet permutation, class bits (kind, ceiling, wall, baked-RGB, double-sided, TR-eligible, slow-material), risk threshold as one i32 compare, option indexes into a per-frame 12-entry `OptionVariantTable`. The hot loop reads the record; `cached_surface_kind`, `wall_material_for_direction`, `cached_uv_material`, the risk chain, and all per-surface `WorldSurfaceOptions` construction leave the per-frame path. The four emit paths become `#[inline(never)]` leaves so the dispatch loop plus the dominant leaf fit the 4 KB I-cache.
10. **Exact subdivision (unchanged math, changed data source).** Same trigger (`max(sz) < far_depth`), same one-level 3x3 lattice, same 5 new projections with root-corner reuse, bit-exact. New: per TR-eligible quad a residency-built `LatticeAttrs` entry holds the 5 midpoint UV words and 5 midpoint RGB triples (the 4 corners come from the surface record), so per-frame midpoint math shrinks to the 15 position midpoints the view-space lattice genuinely needs; child packets are built by `copy_payload_from(warmed_root)` + patch UV low-halfwords + patch colors + `set_positions`, not by full construction. The GTE projection-state reload hoists from per-surface to per-cell-run, after the far-leaf early-out.
11. **Crack-cover submission** (unchanged policy): middle band only, warmed-root position patch when available (already the cheapest case), same depth bias, same suppression on overflow and translucency.
12. **Packet generation**: whole quads keep the existing warm patch (`set_positions` + push). Children as in 10. The arena double-write disappears for every templated packet.
13. **Ordering-table insertion** (unchanged): bucketed 8-byte commands, reverse-prepend flush. Depth values computed by the identical expressions (bit-exact OT slots).
14. **CPU/GTE/GPU overlap** (unchanged structure): phase-1 async kick + odd-tick coverage + boundary flip stays. One addition: the counter-emission block and pass-2 per-room setup move behind the `submit_async` kick where legal, converting remainder cycles into GPU cover.
15. **Streaming integration** (unchanged): records and lattice attrs are built exactly where prewarm already runs, on background ticks, at room activation; slot-theft invalidation clears them with the same validity bytes.

The same record/template pattern is then applied to `image_props` (boxes and cards are textured quads with static UV/color, currently rebuilt per frame) and reviewed for the player path; that is stage E8, gated on the E2 interior measurement.

---

## H. Data structures and memory budget

```rust
/// Built at room residency by prewarm_indexed_cached_room_quads. 16 bytes.
#[repr(C)]
pub struct SurfaceDrawRecord {
    resolved_material: u8,   // index into the room's resolved-material table
    options_calm: u8,        // OptionVariantTable index when not risky
    options_risky: u8,       // OptionVariantTable index when risky
    class: u8,               // WHOLE_QUAD|CEILING|WALL|BAKED_RGB|SLOW_MAT|TR_ELIGIBLE|DOUBLE_SIDED
    packet_perm: [u8; 4],    // reproduces warmed_room_quad_packet_vertices exactly
    risk_threshold: i32,     // depth-span compare; i32::MIN = always risky, i32::MAX = never
    lattice_first: u16,      // index into LatticeAttrs pool, 0xFFFF = none
    _reserved: u16,
}

/// One per TR-eligible whole quad. 28 bytes. The 4 corner UVs/colors live in the
/// CachedRoomSurface; only the 5 lattice midpoints (indices 1,3,4,5,7) are stored.
#[repr(C)]
pub struct LatticeAttrs {
    mid_uv_words: [u16; 5],       // 10 B, packet-ready low halfwords
    mid_rgb: [(u8, u8, u8); 5],   // 15 B
    _pad: [u8; 3],
}

/// Per frame, on the stack. At most 12 distinct option variants exist
/// (kind x triangle-depth x risky under fixed project modes).
pub struct OptionVariantTable { variants: [WorldSurfaceOptions; 12], len: u8 }  // ~968 B

/// Cooked, per directed portal: bitset over destination-room cached cells.
/// Variable length, deduplicated in the same byte pool as the PVS bits.
pub struct LevelPortalCellMaskRecord { byte_first: u32, byte_count: u16, flags: u16 } // 8 B
```

| Item | Bytes | Where | cortex_v3 cost |
|---|---:|---|---:|
| SurfaceDrawRecord pool | 16 x 256 x 8 slots | .bss, beside PrebuiltRoomQuads | 32,768 |
| LatticeAttrs pool | 28 x 256 x 8 slots (or a shared 24 KB arena sized by cook) | .bss | 57,344 (arena option: 24,576) |
| OptionVariantTable | ~968 | stack, per frame | 0 resident |
| Portal cell masks | 8 B/record + deduped bits | manifest `.data` (always resident, like PVS) | 12 records x 8 + ~60 B bits ~= **156** |
| Cooker sweep tables | 0 runtime | cook only | 0 |
| Existing budgets unchanged | PrebuiltRoomQuads 116,744; CachedRoomProjection 57,344; packets 86,016; OT 8,192 | .bss | 0 delta |

Disc cost: masks ~150 B in the manifest (baked `.data`, no `.psxc` version bump needed; if later moved into chunks, the `.psxc` FLAGS word at offset 60 has 31 free bits to version a new section). RAM delta: **90 KB worst (66 KB with the shared lattice arena)** out of 2 MB, all in existing arena style, fixed capacity, no allocation. Scratchpad: the 1 KB D-cache is currently unused by the entire renderer; the leaf emitters should place the 96-byte leaf-vertex staging and the packet-under-construction there (a contained, later win; not load-bearing for the budget).

---

## I. Runtime pseudocode

```rust
// Per drawn room R. All arrays fixed-capacity; no allocation.
// [1] Mask fold: O(frustums) <= 64, ~6 cyc each.
let mut mask = CellMask::ALL;                       // fail-open default
if let Some(bits) = room_has_any_frustum(R) {
    mask = CellMask::NONE;
    for f in visibility.frustums() {                 // frustums are NOT contiguous per room;
        if f.room != R { continue; }                 // filter by room, never slice by first/count
        mask.or(portal_cell_masks[f.source_portal]); // OR = multiple-path accumulation
    }
}
// Root, overlap/stacked, capped rooms: no frustum -> mask stays ALL.

// [2] Cell selection: O(candidates) <= 192 (arena cap), observed max 79.
for cand in visible_cells {
    if !mask.test(cand.cell_ordinal) { continue; }             // ~6 cyc
    let cell = cell_lookup(cand)?;                             // direct-index hit or binary search
    let view = gte_view_vertex(cell.visibility_center);        // once, shared by both tests below
    if !frustum.cell_aabb_visible(view, half) { continue; }
    if let Some(w) = window && !w.intersects(view, half) { continue; }  // rectangle union kept
    accept(cell, depth_of(view));                              // accepted <= 192 hard
}
sort_accepted_by_depth();                                      // shell/bucket sort, O(n log n)-ish

// [3] Unique-vertex projection: O(unique vertices) <= 4096/room, RTPT batches of 3.
project_unique_vertices(accepted);                             // bitset dedup, exactly-once

// [4] Surface loop: O(surfaces in accepted cells) <= cooked bound (E9 enforces).
let opts = OptionVariantTable::build(options, depth_mode, subdivision_mode);  // once per room
for cell in accepted {
    let submit = CachedRoomSubmitDepths::from_cell(cell);      // 2 slot_depth divides per cell
    for i in cell.surface_range() {
        let rec = &records[i];  let surf = &surfaces[i];
        let Some(p) = indexed_projected_quad(projected, surf.vertex_indices)
            else { near_clip_cold(i); continue };              // #[inline(never)], unchanged math
        let m = ProjectedQuadMetrics::new(p);
        if m.outside_screen(bounds) { continue; }
        if backface_by_class(p, rec.class) { continue; }       // NCLIP, encoded winding
        let risky = m.depth_span() >= rec.risk_threshold;      // one i32 compare
        let o = &opts.variants[rec.opt_index(risky)];
        if rec.class & SLOW_MAT != 0 { emit_cold(rec, surf, p, m, o, submit); }   // translucent/animated
        else if rec.class & TR_ELIGIBLE != 0 && needs_subdivision(p, o) {
            emit_subdivided(rec, surf, p, m, o);               // #[inline(never)]:
            //   5 view-space position midpoints (15 i32 ops)
            //   2 RTPT for lattice points 1,3,4,5,7; corners reused bit-exact
            //   4 children: copy_payload_from(warm_root) + patch uv from LatticeAttrs
            //              + patch rgb + set_positions + depth avg + push   (~120-160 cyc each)
            //   crack cover: warm-root set_positions + biased depth + push  (middle band only)
        }
        else if let Some(q) = warm_pool.get(i) { emit_warm(rec, q, p, m, o, submit); } // patch+push
        else { emit_cold(rec, surf, p, m, o, submit); }
    }
}
// Overflow behavior unchanged: packet arena 1,536 / OT 2,048 hard caps; on overflow the
// subdivision path suppresses the underdraw first, then falls back to whole-quad emission,
// exactly as today (stats.primitive_overflow plumbing untouched).
```

Stage complexities and hard caps: mask fold O(64); candidates O(192); accepted O(192); unique vertices O(4,096); surfaces O(cooked bound, E9; observed max 180); primitives O(1,536 arena); OT ops O(2,048). Every cap exists today except the cooked surface bound, which section N adds.

---

## J. Correctness proof for the visibility design

The only behavior change to visibility is the per-portal cell mask AND-ed into cell candidacy. Claim: it cannot reject a cell containing visible geometry.

**Definition.** Bit c of `portal_cell_mask[p]` is set iff there exists a point x in portal p's source room region admitted to p, and a point y in destination cell c's AABB, such that segment xy passes through p's aperture (with every comparison rounded outward and ties set). The cooker sets the bit whenever visibility cannot be disproven.

- **Multiple disjoint portal paths.** Masks are OR-ed over every frustum reaching the room. A cell visible through any admitting path is set by that path's portal mask independently of the others. This is a union of per-path exact answers, strictly tighter than the current bounding rectangle and equally safe. The existing test `portal_cell_window_union_keeps_every_admitting_path` extends to masks verbatim.
- **Cyclic portal graphs.** The mask is a static property of the portal; OR is commutative, associative, idempotent. Cycles only affect which frustums exist, and the traversal's cycle guards (back-edge skip, redundant-frustum containment, depth cap, pool caps) are untouched. A frustum dropped by a cap leaves the room with fewer frustums; if it has none, the room is unrestricted (below).
- **Root-room rendering.** The root has no frustum: mask = all-ones, matching today's `portal_cell_window = None` exactly.
- **Stacked and overlap rooms.** `include_overlapped_rooms` pushes them with `frustum_count: 0`; no frustum matches; mask = all-ones. Same for any room whose window today would be None.
- **Near-plane crossings.** The mask gates cell candidacy only; the near-clip surface path (`draw_near_clipped_...`) is downstream and unchanged. The mask itself is a room-space relation with no near-plane arithmetic.
- **Large cells crossing portal boundaries.** The test is existential over the whole cell AABB: a straddling cell has its bit set. There is no partial-cell clipping, which was the failure mode of the rejected single-aperture design.
- **Camera movement across portal seams.** The sweep quantifies over every admitted source position, not the instantaneous camera, so sub-region camera motion cannot change the answer; no popping class exists. The sweep's source granularity must be at least as coarse as whatever admits portals during traversal (currently: the room). If traversal ever becomes source-cell-aware, the sweep may narrow only to match, never further.
- **Horizontal and vertical clipping.** The sweep uses the full 3D aperture quad (the cooked `[BL,BR,TR,TL]` world vertices, including vertical `kind:1` portals) against full cell AABBs including y bounds. Stacked-floor portals need no special case.
- **Fixed-point rounding and saturating arithmetic.** All rounding happens in the cooker, outward, in i64 if convenient (cook-side code has no 32-bit constraint). The runtime performs one AND and one shift on bytes; there is no runtime arithmetic to saturate. This is strictly safer than the current runtime rectangle test, which does saturating i32 multiplies per cell.
- **Dynamic objects crossing portals.** Masks gate room *cells*, not actors; model/player rendering paths are untouched and rooms an actor occupies are drawn under today's rules.
- **Transparent surfaces.** Translucent surfaces live in cells like any other; cell candidacy is material-blind; layer ordering is unchanged.
- **Surfaces spanning multiple visibility clusters.** A surface belongs to exactly one cell by construction (cache_build walks sectors), so there is no cross-cluster surface to lose.
- **Fail-open invariants**, enforced: (1) the mask array's default value is all-ones, so an absent or unproven mask fails open; (2) a cooker assertion that every mask row for rooms without cooked proof is all-ones; (3) a replay assertion (debug builds) that the mask never rejects a cell the frustum+window pipeline would have accepted and drawn with nonzero emitted primitives; run it over the full cortex_v3 tape before shipping.

---

## K. Visual-equivalence proof obligations

Three tiers, each with an explicit artifact:

| Tier | Applies to | Method |
|---|---|---|
| Bit-exact unit | `SurfaceDrawRecord` derivation, `OptionVariantTable`, `risk_threshold`, `packet_perm`, lattice attr values, child packet bytes, OT slot values | property tests: new path == legacy path over every cooked surface of cortex_v1, cortex_v3, and one stress map, plus swept camera poses for the backface class |
| Frame-hash exact | the full route | `--dump-hash` VRAM + display hash equality per presented frame |
| Perceptual | nothing | any hash difference is a bug, not a tradeoff (the crack-cover experiment set this precedent: 8,324 differing pixels = rejected) |

**The cadence problem, and its fix (new here).** Frame-hash comparison across builds is currently meaningless: the clean HEAD presents 455 frames and the corrected build 490, on the same tape, because faster builds drop fewer visual intervals; frame N samples different sim states. Add a **lockstep-visuals replay mode** (`--lockstep-visuals`): the scheduler renders on strict tick parity (every second tick), never drops a visual, and ignores deadlines. Sim state per tick is already deterministic under tape replay, so two builds in lockstep present identical frame sets and every hash must match bit-for-bit. All A/B experiments in M use this mode for the visual gate and the normal mode for the performance numbers.

Preserved item by item: authored geometry (records change representation, never membership); draw distance (untouched); texture quality and UV interpolation (same uv_words, same packet fields; lattice midpoint UVs precomputed by the same `midpoint_i32` expressions at residency, tested bit-equal); Gouraud lighting (same baked RGB path, same shading calls on the cold path); fog and atmosphere (fog builds disable the warm gate exactly as today: `SLOW_MATERIAL`/gate logic reproduces `use_direct_baked_rgb`); subdivision topology (identical trigger, identical lattice, root-corner reuse already proven bit-exact by `adaptive_lattice_root_projection_reuse_is_exact`); GTE results (projection code untouched); crack cover (identical); backface rules (encoded `ready` byte semantics preserved, including the `REVERSE` vs `REVERSE_FRONT` separation documented at `indexed_cache.rs:1600-1606` and the ceiling pre-cull winding reversal); depth policy (identical expressions, identical OT slots); transparency (translucent forced onto the cold path, layer bit read as today); primitive ordering (emission order per cell unchanged; no bucketing by class is proposed).

Rare-failure detection: a portal-seam soak (replay the tape with the J fail-open assertion active), a near-plane soak (the stress map's point-blank wall route), and per-frame hash on both, in lockstep mode.

---

## L. File-level implementation plan

| File | Current responsibility | Change |
|---|---|---|
| `engine/crates/psx-engine/src/world_render/indexed_cache.rs` | 2,596 lines; cell select, per-surface interpret-and-emit (`draw_indexed_cached_room_surface`, 570 lines, inlined into a 35,488 B caller); warm pool prewarm | The core rewrite. Surface loop becomes the section-I dispatch reading `SurfaceDrawRecord`; the four emit arms become `#[inline(never)]` leaves; `prewarm_indexed_cached_room_quads` additionally fills records and `LatticeAttrs`; risk chain and options constructors survive only as prewarm/test code. Cell loop gains the mask test ahead of the GTE transform and stops recomputing `cell_aabb_view_extents` and `half_y` twice. Memory +records/+lattice pools. Saving: the bulk of E5+E6+E7. Tests: record-vs-legacy derivation over all cooked rooms; leaf-vs-arm equivalence per class pattern; link-map size assertion for the loop (<4 KB) |
| `engine/crates/psx-engine/src/world_render.rs` | Cached types, `PortalCellWindow`, frustum/window tests, material resolution | Add `SurfaceDrawRecord`, `LatticeAttrs`, `CellMask` view over the cooked bit pool; keep window as fallback; `wall_material_for_direction`/`cached_uv_material` become residency-time only |
| `engine/crates/psx-engine/src/render3d.rs` | Projection, lattice, options types, profiles | No math changes. `project_adaptive_view_lattice_gte` untouched; add the per-cell-run GTE state hoist entry point; `WorldSurfaceOptions` remains, constructed only by `OptionVariantTable::build` |
| `engine/crates/psx-engine/src/render3d/world_pass_gouraud.rs` | Submit family, TR splitter, warm patch submit | Child emission via `copy_payload_from` + patch; kill the remaining by-value `*options` derefs and builder-chain copies (the borrow conversion at HEAD stopped at the signatures); underdraw path unchanged |
| `engine/crates/psx-engine/src/world_render/tests.rs` | Contracts incl. lattice exactness, window union | Add: record derivation equality; option-table 12-case exhaustive; risk-threshold vs legacy chain; packet_perm vs `warmed_room_quad_packet_vertices`; backface class swept poses; lattice-attr bit-equality; mask fail-open (empty admitting set = unrestricted); mask superset-of-accepted replay assertion; extend the union test to masks |
| `engine/crates/psx-level/src/portal_visibility.rs` | Frustum BFS | Untouched hot path. Optional later: fold the double corner transform (`portal_within_far` re-transforms) and the `div_q12_i32` pair; not load-bearing |
| `engine/crates/psx-level/src/lib.rs` | Level record formats | Add `LevelPortalCellMaskRecord` + byte pool (manifest tables, same pattern as PVS); no `.psxc` bump needed |
| `engine/examples/editor-playtest/src/active_room_visibility.rs` | Window fold, overlap rooms, counters | Add the mask fold beside the window fold; write the empty-admitting-set test first |
| `engine/examples/editor-playtest/src/playtest_scene.rs` | Frame orchestration | Move the counter block and pass-2 per-room setup behind the async kick; dedupe pass-2 setup; no ordering changes |
| `engine/crates/psx-engine/src/app.rs` + `scheduler.rs` | Cadence, phase-1 present | Add `--lockstep-visuals` support (scheduler flag: visuals by parity, no drops) |
| `emu/crates/frontend/src/cli.rs` + `emulator-core` | Replay, profiler | Lockstep flag plumbing; I-cache stall counter export (E1); no engine effect |
| `editor/crates/psxed-project/src/playtest/cook_visibility.rs` | Cell PVS cook | Add the portal-mask sweep (portals x source-region x destination cells, outward rounding, dedupe into the PVS byte pool) and the section-N worst-case sweep + diagnostics |
| `editor/crates/psxed-project/src/playtest/manifest.rs` | Manifest emission | Emit mask tables + sweep-result constants; hard errors per N |

---

## M. Experiment matrix

Ground rules, learned from the two rejected micro-optimizations: every performance claim reports render mean, p95, max, total runtime, delivered frame count, and the link-map size of any changed hot function. Visual gates run in lockstep mode with per-frame hash equality. One variable per experiment.

| ID | Hypothesis | Change / instrumentation | Baseline -> expected | Reject if | Visual gate | Where |
|---|---|---|---|---|---|---|
| E0 | Lockstep replay makes A/B hashes meaningful | scheduler flag + CLI plumbing | 455 vs 490 presented frames -> identical sets | hashes differ between two runs of the same binary | n/a (it is the gate) | emulator |
| E1 | I-cache stalls are 15-25% of room_surface_draw | stall counter in `cpu/icache.rs`, per-row export | expect 120-200k of 802k | stalls > 400k (reorders the plan toward code-splitting first) | none | emulator |
| E2 | The 802k interior and the 130k image_props interior are attributable | one diagnostic run: `room-surface-profile` + `PSXO_PROFILE_BOX_PROPS=1` (proportions only; the feature disables warm paths) | dark counters -> filled | contradiction with the S~=80 subdivision-share inference (#7 in C) | none | emulator |
| E3 | v1 and v3 share one `(a, b)` cost model at this commit | the section-E benchmark, pooled regression | one fit, R^2 > 0.85 | projects need different slopes -> stop, re-diagnose | hashes recorded for later gates | emulator |
| E4 | Leaf `#[inline(never)]` split alone pays | attributes only, no logic | room_surface_draw -80 to -150k | total runtime regresses (the 8.3/8.4 failure mode) | lockstep hash equal | emulator + hardware |
| E4b | Counter block + pass-2 setup off the critical path | move behind kick, dedupe setup | render remainder -25 to -40k | any hash diff | lockstep hash equal | emulator |
| E5 | Records + option table kill the interpretation tax | `SurfaceDrawRecord` + `OptionVariantTable` | room_surface_draw -200 to -320k, primitives within +-1 | < -120k, or primitives move | lockstep hash equal + unit tiers | emulator, then hardware |
| E6 | Template emission kills the per-child cost | `LatticeAttrs` + `copy_payload_from` children + GTE-state hoist | room_surface_draw further -200 to -300k; cyc/prim -> 500-700 | < -120k, or any packet byte diff in the unit tier | lockstep hash equal | emulator, then hardware |
| E7 | Cooked masks cut cell-select and worst-frame surfaces | mask cook + fold + mask-first ordering | cell_select 49.6k -> 12-20k; p95/max `surfaces_considered` down 15-35% (mean may move little; the rectangle already removed the mean slack) | cell_select > 30k and no tail reduction | lockstep hash equal + J assertions over the tape | emulator |
| E8 | The same record/template pattern bounds image_props | apply to box/card props per E2's interior | props mean -40 to -70k, max -150 to -220k | interior shows the cost is not packet/classification | lockstep hash equal | emulator |
| E9 | Cooker worst-case sweep predicts the runtime maxima | sweep only, no runtime change | predicted S_max/P_max >= observed 180/625 on the tape | prediction below observation (sweep unsound) | none | cook + replay |
| E9b | Room-crossing spike is removable | spread `load_active_room_window` across background ticks | update_window/room_track maxima 139k/127k -> < 40k | gameplay-visible activation lag | lockstep hash equal | emulator |
| E10 | The emulator cadence holds on silicon, GPU covered | burn, fps overlay, hardware timer around render, photo route; tear check at portals | within 10% of emulator; no tear; no draw_sync starvation | > 20% worse, or visible tear (GPU window too small: fall back to the two-part kick design, not phase 2) | photos vs contact sheet | **hardware, mandatory** |

Sequencing: E0-E3 first (instrumentation, nothing ships); E4/E4b next (cheap, separable); E5 -> E6 -> E7 in that order, each landed on its own hash-equal gate; E8 after E2; E9/E9b in parallel with E8; E10 before declaring any FPS number real. Every speculative change is measured alone before combining.

---

## N. Worst-case 30 FPS guarantee

Current bound status:

| Quantity | Cap | Observed max (v3 tape) | Binding? |
|---|---|---:|---|
| Active rooms drawn | 16 (config), visible_chunk_limit 10 | 4 | no |
| Portal frustums | 64 pool | well under | no |
| Candidate cells | 192 arena | 79 | no |
| Accepted cells | 192 | 55 | no |
| **Surfaces visited / frame** | **none** | **180** | **yes, the gap** |
| Unique vertices projected | 4,096/room | 304 | no |
| Subdivided surfaces / frame | none directly | ~implied 80-160 | yes |
| Generated primitives | 1,536 arena | 625 | crash-stop only |
| OT insertions | 2,048 | 701 | crash-stop only |
| image_props / model / player cycles | none | 369,214 / 197,735 / 213,254 | yes |
| Streaming on visual frames | design: background ticks | 0 loads observed | no |

**The cooker sweep** (computable entirely from data the cooker already builds plus the new masks): for every source cell C and yaw bucket Y (16 yaw x 3 pitch is ~5,100 evaluations for v3, seconds of cook time),

```
S_max(C,Y) = sum over rooms R admitted from (C,Y) via the portal graph:
               sum over cells c in R with mask bit set and AABB in cone(C,Y):
                 cells[c].surface_count
P_max(C,Y) = same sum weighted per surface: 5 if TR-eligible and within the far band
             from C, else 1, plus a near-band split allowance for surfaces within
             near_depth (the hw-extent worst case, bounded by MAX_TEXTURED_HW_SPLIT_DEPTH)
```

**The frame-cost bound** with post-fix coefficients (record-path surface cost `a'`, template-path primitive cost `b'`, both measured by E5/E6 before the constants are frozen):

```
render_worst <= a' x S_max + b' x P_max + cell_select(C_max) + player_max
              + props_budget + models_budget + fixed(sky + flush + remainder)
require: render_worst + update_p95(255k) + simtick_p95(262k) <= 1,128,960
   i.e. render_worst <= ~612k against joint-p95 ticks, ~790k against mean ticks
```

Worked with expected values (a' = 500, b' = 400, S_max = 140 after masks, P_max = 625): room term 320k + select 15k + player 213k + props 150k (post-E8 budget) + models 60k + fixed 130k = **888k**. Against the mean-tick budget (790k) that is 12% over; against joint-p95 ticks it is 45% over. Conclusions, stated plainly:

1. The room fix alone cannot prove sustained 30 for v3; the non-room maxima (player + props + models + fixed = 550k+) consume 70% of the budget before a single wall is drawn. The tail program (E8, player work) is not optional for sustained 30.
2. The joint tail is real but milder than the sum of maxima (worst-20 frames carry props at 185k, models at 58k); E3/E5-era data must replace the marginal maxima with the measured joint tail before the cooker constants are frozen, or the gate will over-reject content.
3. **The guarantee mechanism is the cooker, not the measurement.** Cook fails (hard error) when any (C,Y) violates the budget, with actionable diagnostics: "room R contributes N surfaces through portal p from cell (x,z); split the portal seam / add a blocker wall / reduce the wall-band stack here", plus warnings for: portal-path union admitting > K cells, subdivision amplification > 5x in a single view, packet count > 1,200 in a view, translucent overdraw above a threshold, any room over the 32 KB stream slot. A runtime debug assertion cross-checks the cooked bound against live counters during replay.
4. Bounds the cooker cannot see (player animation cost, props debris bursts) get engine-side budget caps only if E2/E8 show they can spike unboundedly; a cap that changes what renders is a visual change and lands behind its own gate.

With those four in place, "sustained 30" becomes a property of cooked content, checkable at build time, and a level that violates it fails to cook instead of stuttering on the console.

---

## O. Roadmap

| Stage | Work | Expected saving (mean render) | Worst-frame effect | Cumulative render (mean) | RAM/disc | Risks | Rollback | Go/no-go |
|---|---|---:|---:|---:|---|---|---|---|
| 0 | E0 lockstep + E1 stall counter + E2 diagnostic captures + E3 like-for-like | 0 | 0 | 1,401k | 0 | none | delete | E3 pooled fit R^2 > 0.85, else stop and re-diagnose |
| 1 | E4 leaf split + E4b remainder moves | -105 to -190k | similar absolute | ~1,240k | ~0 | the alias-shift regression class; six-metric reporting mandatory | attributes/reverts | total runtime improves, hashes equal |
| 2 | E5 records + option table | -200 to -320k | scales with S (worst frames save more) | ~980k | +33 KB | backface/winding class bits (the sharp edge); write the swept-pose test first | cargo feature keeping the legacy arm for one release | hash-equal + >= -120k |
| 3 | E6 lattice templates + child copy-patch + GTE hoist | -200 to -300k | scales with P | ~730k | +24 to 57 KB | packet byte drift; the in-place-patch flip invariant | feature gate | hash-equal + >= -120k; cyc/prim <= 700 |
| 4 | E7 cooked portal cell masks | -30 to -45k mean; the real product is the tail cut | p95/max surfaces -15 to -35% | ~690k | +0.2 KB | fail-open discipline in the cooker | ignore masks, window remains | J assertions clean over the tape |
| 5 | E8 props (and player review) on the same pattern | -40 to -70k mean | -100 to -200k on prop-heavy frames | ~630k | +small pools | props interior unknown until E2 | per-subsystem gates | hash-equal |
| 6 | E9 cooker sweep + enforcement; E9b crossing-spike spread | 0 mean | converts remaining spikes into cook errors | ~630k | 0 | may reject existing content (a design decision, not a bug) | warn instead of error | predicted maxima >= observed on the tape |
| 7 | E10 silicon validation (burn, timer, tear check) | 0 | confirms the GPU window | | 0 | fill-rate regime is unmodeled | n/a | within 10% of emulator, no tear |
| 8 | If E10 shows the GPU window short: two-part kick (far geometry early, near+player late), never phase 2 | n/a | n/a | | 0 | latency-neutral by design | revert kick split | no added input latency |

Projected end state: v3 render mean ~630k against a 790k budget, worst frames within budget except cooker-rejected views; v1 comfortably inside its 915k budget by stage 3. Stages are separately shippable; nothing depends on a later stage for correctness.

---

## P. Directions to reject

- **Three-vblank cadence as the target.** The predecessor document targeted 20 FPS; this review supersedes it. 3 vblanks remains a milestone (stage 2 lands roughly there), never the goal.
- **Average-only performance, and full-run averages.** 106 boot/loading frames deflate the full-run mean by 26%; `present` idle inflates apparent cost by 305k; the mean-vs-p99 gap is 815k. Every claim reports the distribution.
- **Single-aperture room clipping.** Structurally unsound for multi-path rooms; already caused a severe regression. The mask design is the correct generalization (exact per-portal answers OR-ed, fail-open).
- **Removing surfaces, draw distance, subdivision, or crack cover.** The crack-cover removal experiment (8,324 changed pixels) set the bar: hash-equal or rejected.
- **Project-specific exceptions and camera-route preprocessing.** Everything here keys on cooked content structure, not on either project or any route; the tape is evidence, never input to the build.
- **Broad caches.** The view-space cache failed because it added traffic to a fetch-bound loop. Records/templates remove recomputation instead of caching it; that distinction is the design.
- **Single-stage wins.** Two prior reverts (model-options borrowing, dense-init removal) improved a stage and regressed the frame. The six-metric report plus the link-map check is the standing defense.
- **Emulator-only validation.** The CPU model is trusted from prior silicon correlation; the GPU window is not modeled at all. E10 is mandatory before any FPS claim is real.
- **Unbounded fallbacks.** The near-clip path, the `_all_cells` fallback (32,604 B twin: verify it is unreachable in shipping configs and delete or gate it), and the props/debris paths must live under the same cooked bounds as everything else, or the guarantee in N is fiction.
- **Moving work to the GTE for its own sake.** 159 cyc/vertex projection and the NCLIP-based culls are already right; prior findings (`keep-work-on-gte-not-cpu`, `gte-cull-depth-scalar-by-choice`) stand.

---

## Q. Final recommendation

1. **Highest-leverage change:** residency-compiled rendering: `SurfaceDrawRecord` + `OptionVariantTable` + `LatticeAttrs` template emission + `#[inline(never)]` leaves, with cooked per-portal cell masks as the visibility half.
2. **Why it benefits both projects:** it cuts the two coefficients of the shared cost function; v3 pays mostly the surface coefficient (88.6/frame), v1 mostly the primitive coefficient (15 prims/surface), and both fail 30 FPS today (v3 on 99% of frames, v1 on 12%).
3. **Exact first implementation step:** none of the above. First is E0 + E1 + E3: the lockstep-visuals replay mode (without it no A/B is hash-comparable), the I-cache stall counter, and the same-commit v1/v3 captures with the pooled regression. E3 is the experiment that can falsify the whole diagnosis.
4. **Files and structures:** section L; core is `indexed_cache.rs` (records, leaves, mask-first cell loop), `world_pass_gouraud.rs` (copy-patch children), `cook_visibility.rs` + `manifest.rs` (masks, sweep), `psx-level` (mask records); structures in H (16 B record, 28 B lattice attrs, 8 B mask record + byte pool).
5. **Expected average saving:** 505 to 925k cycles/frame across stages 1-5 (point estimate ~770k), against a required mean cut of 610k. Confidence: medium-high on direction and ordering, medium on magnitudes until E1/E2 close the stall-vs-instruction split.
6. **Expected worst-frame saving:** larger in absolute terms than the mean (both taxes scale with the counts that define bad frames): projected ~1.3M off the 2,263k worst frame from stages 1-4, plus mask-driven surface-count cuts on vista frames; the residual tail is owned by props/player/models and stage 5.
7. **RAM and disc cost:** +57 to 90 KB RAM in fixed arenas (records 33 KB, lattice attrs 24 to 57 KB, masks negligible); disc ~+150 bytes of manifest tables.
8. **Main correctness risk:** the backface/winding class encoding (`REVERSE` vs `REVERSE_FRONT`, ceiling pre-cull reversal). Write `backface_class_matches_legacy_cull` (swept poses over every cooked surface) before writing the record builder.
9. **First benchmark:** the E3 like-for-like capture pair, reported with distributions and the pooled fit.
10. **Quantitative acceptance criterion:** on the cortex_v3 tape in normal (non-lockstep) mode: render mean <= 790k and >= 95% of gameplay visual frames satisfying `render + update + simtick + glue <= 1,128,960`, with per-frame lockstep hashes equal to baseline; on cortex_v1: 100% of frames within budget. Final acceptance only after E10 confirms silicon within 10% and no tearing.
11. **Can the design genuinely guarantee 30 FPS?** For cortex_v1: yes, from stages 1-3, by measurement and margin. For cortex_v3: mean-30 yes (stages 1-5); sustained-30 becomes a **by-construction property only when the section-N cooker gate is enforced**, because nothing else bounds surfaces-per-view or the props tail. Without stage 6, sustained 30 is a measurement on one tape; with it, a level that cannot hit 30 fails to cook, which is the only honest form of guarantee this hardware offers.
