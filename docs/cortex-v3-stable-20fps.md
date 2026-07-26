# cortex_v3: the architecture for pixel-identical, stable 20 FPS on real hardware

Evidence base, all gathered on branch `emu/accuracy-from-silicon` at `0bab5cd8`:

- The supplied captures: `/tmp/cortex-v3-final-verified.gwBLQF` (corrected build) and `/tmp/cortex-v3-head-normal.5Ce1mo` (clean HEAD), replays of `cortex_v3.pxtape` on the baked disc (BIN SHA-256 `7ac60512...`).
- A relink of the playtest guest as an ELF, byte-identical to the shipped `editor-playtest.exe` (verified with `cmp`), so every PC sample below is symbolized against the exact binary that produced the captures.
- A fresh instrumented replay of the v3 tape with 1-tick PC-sample windows (per-function cycle attribution).
- A fresh `cortex_v1` capture made during this analysis: `cortex_v1` disc rebuilt from this HEAD with the same feature set (`cd-stream-bench emulator-telemetry`), replayed against `cortex_v1.pxtape` (recorded 25 Jul) with identical instrumentation. Artifacts in the session scratchpad (`v1-profile.csv`, `v1-counters.csv`, `v1-gpu.csv`, `v1-pcw.csv`).
- Source reads of the full room-render hot path, the visibility pipeline, the cooker, and both `project.ron` files.

Units: the profiler's `*_cycles` columns are emulator bus cycles under the flat 2-cycles-per-instruction model. 1 NTSC vblank = 564,480 cycles. 20 FPS = one visual frame per 3 vblanks = 1,693,440 cycles of wall time per visual period, shared between render, the 60 Hz sim ticks that must run inside that period, and flip overhead.

---

## A. Executive verdict

`cortex_v3` is CPU-bound on per-surface constant overhead that its own configuration multiplies, and the single largest multiplier is one project setting: **`fog_enabled: true`**.

The engine has a correct fast path for static room quads: prewarm a complete GPU packet once per room load, patch four position words per frame. The eligibility gate for that path is `uses_direct_baked_vertex_rgb() = !fog_enabled || fog_far <= fog_near` (`engine/crates/psx-game-runtime/src/room_lighting.rs:359-361`). cortex_v1 has fog off, so every eligible surface takes the patch-only path. cortex_v3 has fog on (near 11,900, far 29,500), so **every one of its 364 surfaces takes the full dynamic path every frame**: material lookup, per-vertex fog-blended lighting, full 14-word packet reconstruction, four copies of a 68-byte options struct, and the Tomb Raider lattice built from scratch for every floor and wall within 5 sectors. This is why the same engine produces 989 cycles per primitive in v1 and 1,752 in v3.

The gate is far coarser than the math it guards. `room_fog_weight` returns exactly 0 for any depth `<= fog_near` (`room_lighting.rs:374-378`), and weight 0 returns the baked color bit-for-bit (`:382-385`). Fog near is 11,900 units, 7.7 sectors. Nearly all geometry the route actually renders sits inside that band. The engine discards the fast path wholesale to guard a tint that is provably zero on most of what it draws.

Two further structural facts complete the picture:

1. **Most rooms draw with no portal culling at all.** 3 to 4 of the ~5 drawn rooms per frame take the no-anchor fallback `draw_indexed_cached_room_vertex_lit_all_cells` (the in-code comment at `playtest_scene.rs:656-665` says exactly this), which takes no portal-window parameter and disables the far plane. The corrected portal-union window only culls the minority of rooms that have a PVS anchor. On top of that, the cooked PVS is 100% non-selective for cortex_v3 (`visibility_radius: 32` floods every room; the 104 cell bitsets dedupe to 14 bytes of all-ones), so every per-cell rejection that happens at all happens at runtime, per frame.
2. **A 60 Hz sim tax of ~172k cycles per tick, ~30% of it memcpy.** memcpy is 12.7% of all gameplay CPU (one inner loop, 118M cycles over the route); regression of per-period memcpy against stage composition gives `memcpy ~= 0.295 x update + 0.028 x room_surface_draw`, i.e. ~48k cycles of every sim tick is bulk copying (the `apply_current_active_room_fields` / residency / collision-sector family), and almost none of it is in the renderer. Three sim ticks ride inside every 3-vblank visual period, so this tax alone consumes 516k of the 1,693k budget.

The mistaken architectural assumption, stated once: *"visibility and caching make per-frame cost proportional to what is visible."* In this project's configuration every mechanism that was supposed to enforce that proportionality is inert (PVS all-ones), bypassed (fallback path, no window, no far plane), or disqualified (fog gate kills packet prewarming), and what remains pays a large re-derivation constant per surviving surface, multiplied by ~5x primitive amplification from subdivision. The GTE is 1% of samples. The GPU (now modeled on this branch) averages 1.01 vblank and overlaps CPU work. Portal traversal is 0.5%. The frame is spent re-deriving, re-classifying, re-copying, and re-building things that do not change from frame to frame.

## B. Verified facts versus assumptions

| # | Claim | Status | Basis |
|---|---|---|---|
| 1 | Gameplay visual frame (v3, corrected): mean 3.02 vblanks, p50 3.10, p90 3.67, p99 4.42, max 4.74. 55% of frames exceed the 3-vblank deadline | **Fact** | 385 visual frames in the final capture's last 1,644 rows |
| 2 | Cadence is bimodal: {3 ticks: 129, 4: 69, 5: 143, 6: 31, 7: 9, 8: 1}. Mean gap 4.27 ticks = 14.06 FPS | **Fact** | same |
| 3 | `room_surface_draw` mean 802,528, max 1,563,925 (47% of the visual task). Halving it fixes 80% of deadline misses; zeroing it fixes all | **Fact** | profile columns + simulation over periods |
| 4 | Sim tick costs 173,665 cycles (sim-only rows); `update` 153-164k | **Fact** | profile |
| 5 | memcpy = 12.7% of gameplay CPU, memset = 4.4%; memcpy tracks `update` (r = 0.93), not the render | **Fact** | symbolized PC samples (byte-identical ELF), per-period regression |
| 6 | GTE projection is ~1% (`project_world_index_group_gte` 0.95%); portal traversal ~0.5-1.9% | **Fact** | PC samples, profile |
| 7 | GPU: 1.01 vblank mean, 1.51 max; ~350-458 primitives/frame, max 685; `textured_quad_cycles` dominant; ~68-93 texture-window changes per frame | **Fact** | `gpu.csv` (this branch models GPU/DMA timing; DMA is event-scheduled in parallel with the CPU, `emu/.../bus.rs:1601-1606`) |
| 8 | cortex_v1 at this HEAD, same features, own tape: 25.5 effective FPS, render 765k mean, `room_surface_draw` 316k mean / 799k max, 1-2 rooms drawn, 320 prims (62 quads / 258 tris), GPU 1.04 vblank | **Fact** (measured this session) | fresh v1 capture |
| 9 | v1 fog off, v3 fog on; fog weight is identically 0 at depth <= fog_near; fog-on disables the warmed packet path per room | **Fact** | `project.ron` both, `room_lighting.rs:359-378` |
| 10 | 3-4 of ~5 drawn rooms take the all-cells fallback (no portal window, far plane disabled) | **Fact** (code + in-code comment); *per-frame count not yet logged* | `playtest_scene.rs:608-701`, comment at `:656-665`; `ROOM_VISIBILITY_FALLBACK_DRAWS` counter exists but is not in the counter CSV |
| 11 | Cooked PVS for v3 is all-ones (14 bytes for 104 cells) | **Fact** | `level_manifest.cooked.rs` PVS tables |
| 12 | Warmed quad still pays ~620-860 cycles/frame; dynamic surface ~3-6k; dynamic subdividing surface ~6-10k | **Strong inference** | static instruction counting over the read code paths, consistent with measured 9,027 cycles per considered surface (802,528 / 88.9) |
| 13 | The stack-spill codegen of the `draw_indexed_cached_room...` megafunction adds a material fraction of its 9.6% self time | **Strong inference** | disassembly shows `lw at, N(sp)` reloads inside the inner loop; exact fraction unmeasured |
| 14 | v3 is slower than v1 because of workset (rooms drawn, fog gate, subdivision density, props), not engine divergence | **Fact for the mechanisms in #8-#11; inference for their exact shares.** The two tapes cover different routes in different levels, so this is a mechanism comparison, not a controlled A/B | both captures |
| 15 | Emulator cycle totals track console | **Assumption inherited from prior silicon work**; flat 2 cyc/instr, no I-cache model, no DMA bus stealing on the CPU. The proposed fixes reduce instructions and memory traffic, so hardware error skews in our favor | project memory |
| 16 | `image_props` (six CylinderProps) costs 130k mean / 369k max in v3 vs 16k in v1 | **Fact** | profile columns; `cylinder_prop_uv_at` 0.9% of samples |
| 17 | Prewarm pool slot stealing hurts v3 | **False for current content** (6 rooms <= 8 slots; a room keeps its slot). Latent hazard only: `visible_chunk_limit: 10` violates the documented <= 8 invariant (`room_cache.rs:374-380`) for any future content with more rooms | code read + arithmetic |

Additional evidence still required: (a) per-stage-tagged PC samples (window-level samples cannot split render from sim because a 3-5 vblank render bleeds across windows); (b) `ROOM_VISIBILITY_FALLBACK_DRAWS` and the `room-surface-profile` counters (`surfaces_considered`, warm hits, subdivision count) in the counter log; (c) a route-equivalent v1-vs-v3 A/B (same corridor shape) if the mechanism shares need to be pinned to percentages.

## C. Why the same engine behaves differently

Settings are identical for every render policy knob (`HybridWalls`, split mode `All`, max edge 0, draw order `Distance`). The differences that matter:

| Mechanism | cortex_v1 | cortex_v3 | Consequence |
|---|---|---|---|
| Fog | off | on (11,900/29,500) | v3 loses the warmed-packet path for all surfaces; adds per-vertex fog lighting + `room_depth_prep` |
| Topology | one large open map (51x31 sectors, 59 ceilings for 215 floors), 7 portals, runtime rooms of ~82 cells | fully enclosed interior: 6 rooms, 12 directed portals, 104 floors + 104 ceilings + 155 walls, rooms of 10-27 cells | v3 draws 2-4 rooms/frame (histogram 1:96, 2:209, 3:65, 4:13) vs v1's 1-2 (1:282, 2:412, 3:30) |
| Camera vs geometry | open sightlines, much geometry beyond the 5-sector subdivision band | corridors; nearly every visible floor/wall is inside `far_depth = 5 x 1536 = 7,680` | v3 subdivides most of what it draws: 4-5 packets + optional crack cover per surface (`ROOM_TOMB_RAIDER_SUBDIVISION_LEVELS = 1`, kinds FLOOR_WALL) |
| Surfaces reaching the surface loop | ~22-40/frame | 88.9/frame | direct multiplier on the per-surface constant |
| Props | 6 BoxProps, 1 cylinder | 6 CylinderProps | `image_props`: 16k vs 130k mean, 369k max (UVs re-derived per frame, `cylinder_prop_uv_at`) |
| Streaming limits | 6/6 | 10/10 | no current cost (6 rooms), but violates the prebuilt-pool invariant on paper |
| Sector size | 1664 | 1536 | subdivision band scales with S; smaller sectors mean more cells per world area, minor |

Surface-to-primitive amplification in v3: 88.9 considered surfaces produce roughly 250-350 room primitives (total 458 including player/props), i.e. a subdividing quad becomes 4 leaf quads plus an underdraw quad in the 2.5S-5S band. The portal pass itself is 8.5k/tick and is irrelevant to the difference; what the portals *admit* is the entire story, compounded by most admitted rooms drawing through the un-windowed fallback.

What to measure in v1 to complete the proof (beyond what I captured): the `room-surface-profile` counters on both projects over route-equivalent segments, giving per-frame `surfaces_considered`, warm-path hits, and subdivision counts. Expected: v1 shows high warm-hit ratio and low subdivision share; v3 shows 0% warm hits (fog) and >40% subdivision share.

## D. Cycle-budget reconstruction

Timing model: profiler cycles = 2 x instructions; GTE ops and loads cost the same 2 cycles (no I-cache, no RAM wait states, no DMA stealing modeled). GPU runs in parallel (event-scheduled); the CPU only waits for it where the guest explicitly drains (present flip). All numbers are the corrected v3 capture, gameplay only.

**The visual-period equation.** A visual period of n ticks must fit the render work plus n sim ticks plus flip overhead:

```
n x 564,480  >=  render_work + n x sim_tick + flip_overhead
sim_tick ~= 172k (measured 173,665 sim-only)   flip_overhead ~= 40k (present floor is 4-6k; overlay + drain typically < 40k)
n = 3 (20 FPS):  render_work <= 1,693,440 - 516k - 40k  ~=  1,137k
n = 2 (30 FPS):  render_work <=  1,128,960 - 344k - 40k  ~=   745k
```

**Measured stage decomposition per visual frame (mean / max):**

| Stage | Mean | Max | Share of visual task |
|---|---:|---:|---:|
| visual_render_task | 1,706,108 | 2,674,081 | 100% |
| render (CPU work) | 1,400,869 | 2,263,382 | 82% |
| room | 887,428 | 1,687,923 | 52% |
| room_surface_draw | 802,528 | 1,563,925 | 47% |
| room_cell_select | 49,577 | 90,369 | 2.9% |
| room_project | 20,456 | 45,545 | 1.2% |
| room_visible_list + depth_prep | 10,165 | | 0.6% |
| player | 200,262 | 213,254 | 11.7% |
| image_props | 130,495 | 369,214 | 7.6% |
| model_instances | 36,528 | 197,735 | 2.1% |
| sky | 23,781 | 36,007 | 1.4% |
| world_flush | 46,737 | 56,592 | 2.7% |
| camera (render side) | 42,242 | 92,251 | 2.5% |
| render residual (glue) | ~69,900 | | 4.1% |
| present (wait, not work) | 304,614 | 569,015 | 17.9% |
| ot_submit / ot_wait | ~150 | | ~0% |

`render` mean 1,401k vs the 1,137k three-vblank allowance: the *average* frame already misses, and the p99 render is 2,216k. That is why 14.01 FPS coexists with "the renderer is slightly below the 2-vblank average" claimed earlier: that earlier average included boot/menu frames and, more importantly, `visual_render_task` includes `present`, which is 305k of idle waiting (the vblank spin in `run_scheduled` is 13.6% of all gameplay samples; present does not correlate with modeled GPU cycles, r = -0.10). Work quantizes to vblank boundaries: a 2.48-vblank render that starts on a boundary finishes in its 3rd vblank, then 3+ piled-up sim ticks at 172k each push the next visual start out another 1-2 vblanks. Hence the 5-tick mode (143 of 384 periods).

**Required cut, per period, to make every period fit n = 3** (holding sim constant): 276 of 384 periods need a cut; p50 of the needed cut is 352k, p90 is 754k, max is 1,296k. The maximum exceeds the *entire mean* of `room_surface_draw`, so no single-stage fix closes the tail; the fix must reduce both the per-surface render constant (the bulk) and the sim tick (516k of every period), and it must bound the worst frame, not the average.

**GPU side:** mean 570k (1.01 vblank), p99 803k, max 868k (1.54). With the phase-1 async kick, the GPU overlaps the post-render sim tick and the next frame's early CPU work; 1.54 vblanks fits comfortably inside a 3-vblank period. GPU is not the binding constraint at 20 FPS (it would be at 30). Two caveats for hardware: linked-list DMA steals RAM bus cycles from the CPU (unmodeled), and ~68-93 texture-window state changes per frame are pure command overhead worth auditing later.

**Ambiguities identified and resolved:** (a) `render` sums `scene.render` and `render_overlay` (`app.rs:678-705`); overlay is small. (b) `present` is wait, not work; excluded from budgets. (c) memcpy/memset attribution cannot be split by PC-sample windows (render bleeds across vblank windows); resolved by regression across 384 visual periods. (d) The GPU log's `route_tick` is offset from `guest_frame` by the pre-telemetry boot ticks (25-28 in these runs); all joins above account for it.

## E. Root-cause ranking

| Rank | Cause | Evidence | Expected contribution (cycles/frame) | Confidence | Falsification experiment | Bound |
|---|---|---|---|---|---|---|
| 1 | Fog gate disables the warmed packet path for every surface (full dynamic rebuild per frame) | B#9; v1 vs v3 per-prim 989 vs 1,752; `shade_cached_baked_vertices` + gouraud submit family ~13% of samples | 250-450k mean, more at tail | High | Set `fog_enabled: false` in a diagnostic build, replay the tape: `room_surface_draw` should drop toward v1-like per-prim cost. Diagnostic only (changes pixels where fog tints) | CPU |
| 2 | Subdivision constant: every floor/wall within 5S rebuilds midpoints, lattice, 4 leaf packets + underdraw from scratch (~2,000-2,100 instr each) | static path cost; corridors put most surfaces in-band; tomb_raider submit functions ~6% of samples | 250-400k mean | High | Count subdividing surfaces/frame (`room-surface-profile`); multiply by static cost; compare to stage | CPU |
| 3 | 3-4 of ~5 rooms draw through the all-cells fallback: no portal window, no far plane | B#10; cells drawn 25.4 vs candidates 45.9; rsd scales with rooms drawn (633k @1 room to 1,126k @4) | 150-300k on multi-room frames (the tail) | High for mechanism, Medium for magnitude | Log `ROOM_VISIBILITY_FALLBACK_DRAWS` per frame; thread the window through and diff cells-drawn | CPU |
| 4 | Sim-tick memcpy: ~48k/tick of bulk copies in the update path (`apply_current_active_room_fields` et al.), 3 ticks ride in every period | memcpy r = 0.93 with update; 19-20 static call sites in update-path functions | 145k per 3-tick period | High | Byte-size logging at the call sites; make copies event-driven; sim must stay deterministic (identical `route.csv`) | Memory |
| 5 | CylinderProp UV re-derivation (`image_props`) | 130k mean, 369k max vs v1's 16k | 90-110k mean, up to 330k tail | High | Prewarm cylinder packets/UVs at load; A/B stage column | CPU |
| 6 | Megafunction codegen: register spills in the surface loop; 4 KB I-cache pressure from the inlined `#[inline(always)]` giant | disassembly spill pattern; function is 9.6% self | 50-150k (overlaps #1/#2 savings) | Medium | Split into phase loops; measure self-time delta at equal feature set | CPU/I-cache |
| 7 | Per-candidate cell work: GTE transform + double AABB extent computation + every-frame depth re-sort (`CAMERA_DEPTH_UNKNOWN` forces it) | cell_select 49.6k mean; extents computed twice on the portal path | 20-40k | Medium-High | Compute extents once; skip re-sort when camera cell unchanged | CPU |
| 8 | memset 25k/tick (arena/residency clears) | 4.4% samples; static sites in residency + render | 30-60k/period | Medium | size-tagged logging | Memory |
| 9 | GPU texture-window churn, OT flush, pad `poll_once_diag` (~12k/tick), sky | small columns | < 60k combined | High | n/a for now | mixed |

Portal traversal, GTE projection, OT insertion (~36 cycles/packet), and the union-window test itself are all measured noise (< 1% each). They are not on the list.

## F. Recommended architecture

The smallest architecture that meets the budget keeps every algorithm the engine already trusts and changes *when* work is done, not *what* is computed. Four cooperating changes:

### F.1 Surface-level fast-path validity under fog (replaces the room-level fog gate)

Cooked truth: fog weight is identically 0 at depth <= fog_near. The warmed packet's colors are the baked colors, which are exactly what the dynamic path produces at weight 0. Therefore:

```rust
// room-level (existing): fog_live = fog_enabled && fog_far > fog_near
// surface-level (new): the warmed path is valid iff
//   !fog_live || metrics.max_z <= lighting.fog_near
// metrics.max_z is already computed for HybridWalls risk classification.
```

Surfaces beyond `fog_near` (distant rooms seen through doorways) keep the current dynamic fogged path, bit-for-bit. Everything nearer patches four position words into its prewarmed packet, exactly as cortex_v1 does today. Zero new data. Perhaps 15 lines in `indexed_cache.rs:1077-1106` plus plumbing `fog_near` into the gate.

### F.2 Cooked one-level subdivision lattice + prewarmed leaf templates

The lattice's 5 midpoint UVs and 5 midpoint colors are view-independent (`midpoint_i32` / `midpoint_u8` of authored corner values). Cook them.

Offline (cooker, `psxed-project`): for every subdivision-eligible surface (floor/wall, whole quad), emit a `CachedRoomSurfaceLattice` record computed with the *same* integer midpoint functions the runtime uses (bit-equality is testable, see G):

```rust
#[repr(C)]                      // 48 bytes
struct CachedRoomSurfaceLattice {
    uv: [[u8; 2]; 9],           // 18 B: 9-point UV lattice (corners + 5 midpoints)
    rgb: [[u8; 3]; 9],          // 27 B: 9-point baked color lattice
    _pad: [u8; 3],
}
```

~260 eligible surfaces x 48 B = ~12.5 KB added to the streamed world pack (+2 KB per room, ~7 ms at 2x CD; negligible).

At room prewarm (extend `prewarm_indexed_cached_room_quads`): additionally build, per eligible surface, 4 leaf packet skeletons plus reuse the existing root packet as the crack-cover underdraw. Leaf skeletons carry header, tpage, CLUT, tex-window, the cooked leaf UVs, and the cooked leaf colors; positions blank.

Per frame, a subdividing surface becomes:

```
project 9 lattice points          (existing 2x RTPT path, root corners reused)
for leaf in 0..4: patch 4 positions into leaf packet; push_command   (~60-90 instr each)
if underdraw band: patch root packet positions; push_command
```

Static estimate: ~2,000-2,100 instructions today to ~450-650, a 3-4x cut on the dominant surface class. RAM: worst room is 79 surfaces; 79 root + 4 x 60 leaf packets ~= 320 packets/slot. At ~60 B/packet that is ~19 KB/slot, 8 slots ~= 154 KB (current pool is ~118 KB; +36 KB). If RAM is tighter than that, the fallback variant cooks only the lattice table (12.5 KB) and builds leaves by copying the root template (14-word copy) and patching UV + RGB + positions: ~120-150 instr/leaf, still a >2x cut, +0 packet RAM.

Fog interacts correctly: a subdividing surface with `max_z <= fog_near` uses the prewarmed leaves; beyond that it uses the existing dynamic lattice path unchanged.

### F.3 A visibility floor for every drawn room (kill the un-windowed fallback)

Two changes, both purely subtractive and both reusing proven machinery:

1. **Thread the existing portal-union window and the far plane through the all-cells fallback path.** The union window (`portal_cell_window`) does not depend on the PVS anchor that the fallback lacks; the fallback simply has no parameter for it (`indexed_cache.rs:576-610`). Root room and any room without a provable window keep `None` semantics exactly as today.
2. **Per-wedge refinement after the union-rect test**: a cell that passes the union window is then tested against each admitting frustum (they are already stored per room, `frustum_first/frustum_count`); reject only if outside *all* wedges. Frustum counts per room are 1-2 in practice, the test is the same inflated-AABB-vs-tangent-plane arithmetic, and it recovers the tightness the rectangular union loses when two disjoint paths admit one room. This answers the brief's question in section 3: the component-wise union is the right *first* test but the wrong *only* test; the union of wedges, evaluated lazily per cell, is strictly tighter and cannot reject anything a single admitting path would keep.

Also: compute `cell_aabb_view_extents` once per cell instead of twice (frustum test and window test currently each derive it), and skip the every-frame cell depth re-sort when the camera anchor cell and yaw bucket are unchanged (the PVS candidate cache already has exactly this key).

### F.4 Event-driven sim copies

`apply_current_active_room_fields` and friends copy room field arrays every tick. Copy on transition (room set or current-room change) instead, or borrow the cooked slices directly where lifetime allows. Target: update from ~160k to ~110-120k/tick. This is 130-160k returned per 3-tick period, bought with no renderer risk at all. The same pass should prewarm CylinderProp UVs (rank 5).

### Worst-case behavior and the render schedule

With F.1-F.3 the per-frame cost model becomes linear in *bounded* quantities:

```
render = base + sum over drawn rooms of:
         cand_cells x C_cell  +  verts x C_vert  +  plain x C_warm  +  subdiv x C_leafset
base ~= 430k  (player 200k + props-after-fix ~40k + sky 24k + flush 47k + camera 42k + glue 70k)
C_cell ~= 1.1k (measured: 49.6k / 45.9)   C_vert ~= 60   C_warm ~= 0.7-0.9k   C_leafset ~= 1.2-1.5k
```

The cooker computes the worst case exactly (section K) and fails the bake if the bound exceeds the budget; the runtime never needs a quality-degrading escape hatch because the bound is a content guarantee, not a runtime clamp.

## G. Correctness argument

The visibility scheme never *adds* culling that a proven path lacks; it only extends already-proven tests to rooms that currently skip them, and refines with a strictly-conservative wedge test.

- **Multiple disjoint portal paths.** All frustums reaching a room are stored (`push_frustum`, dedup only removes component-wise-superset duplicates of the same portal/source). The union window keeps every path by construction (min/max over all). The wedge refinement rejects a cell only if it is outside *every* admitting wedge; a cell visible through path P intersects P's clipped frustum, which is exactly P's wedge. `portal_cell_window_union_keeps_every_admitting_path` already pins the union; add the analogous wedge test.
- **Cyclic portal graphs.** Traversal is BFS with a depth cap and containment dedup; a cycle terminates via the cap. Rooms reached only at the cap boundary become frontier rooms without frustums, which yields window `None` and a full draw. Conservative.
- **Root room.** Window `None` by rule (`active_room_visibility.rs:138-140`); only the plain camera frustum applies, unchanged.
- **Stacked / overlap rooms.** They are added without frustums by design (`room_visibility.rs:286-292`), get `None`, full draw. cortex_v3 has none, but the rule is preserved.
- **Near-plane crossings.** The wedge test operates on view-space tangent planes with the same support-term inflation as the existing window test (`world_render.rs:2066-2095`); a cell straddling the near plane inflates to intersection. Portal clipping already handles apertures crossing the near plane by the Sutherland-Hodgman clip in view space; a degenerate clip yields no frustum, which yields `None`, which yields a full draw. Failure mode is always "draw more".
- **Large cells straddling a portal edge.** The AABB support term inflates each plane by the cell's half-extent projection, so a cell partially inside the wedge always passes.
- **Camera movement near portal seams.** The traversal rebuilds on a 1-sector/yaw-key hysteresis; between rebuilds the stored frustums are stale by at most that hysteresis. This is the *existing* staleness contract (the current union window has it too); the wedge refinement inherits, not worsens, it. If a seam artifact is ever observed, the fix is tightening the hysteresis, not the wedge math.
- **Horizontal and vertical clipping.** Both are in the tangent representation (left/right and min_y/max_y); vertical portals get the same treatment, and the vertical-hole special case (`camera_in_vertical_portal_footprint`) already bypasses clipping conservatively.
- **Fixed-point rounding and saturation.** Q12 tangents; the AABB test uses saturating i32 arithmetic (existing); inflation is toward acceptance. Cooked lattice equality is not a rounding question at all: the cooker runs the identical `midpoint_i32`/`midpoint_u8` integer functions, and a cook-time test asserts byte equality against the runtime-computed lattice for every surface (same pattern as the existing bit-exact lattice-vs-recursive test).
- **Fog cutoff.** `room_fog_weight` returns 0 for depth <= fog_near before any multiply (`room_lighting.rs:375-377`), and weight 0 short-circuits to the unmodified tint (`:383-385`). The gate compares the quad's maximum GTE `sz` against `fog_near`; `sz` is the same value the dynamic path would feed to the weight function. Equality is exact, not approximate. Test: for a synthetic room with fog near straddling a wall, warm and dynamic submissions must produce byte-identical packets on both sides of the boundary.

## H. Visual-equivalence argument

"Pixel-identical" is defined operationally: for the same input tape, for every sim tick at which both builds present a visual frame, the 320x240 `display_hash` must be equal (`visual-hashes.csv`, already emitted per frame). Because a faster build renders *more* frames (state is sim-tick-driven and deterministic, so a frame presented at tick T shows state(T) in both builds), the comparison is on the intersection of presented ticks, plus a human contact-sheet pass for the new-only frames. Exact hashing is appropriate everywhere; perceptual comparison is appropriate nowhere (the whole design preserves arithmetic, not appearance).

Per property: authored surfaces are untouched (no geometry added/removed); subdivision topology is decided by the same `max_z < 5S` comparison on the same projected values; projected positions come from the same RTPT wrappers with the same rounding; UVs and Gouraud colors in prewarmed leaves are the cooked outputs of the identical midpoint functions (bit-equal by test); backface culling still runs the same NCLIP per surface; crack-cover underdraw is preserved (it reuses the root packet, as the fused path already does); translucent surfaces never enter the warm pool (existing rule), so blending order is unchanged; depth slots use the same `PreparedTriangleDepth` math; and OT submission order is preserved because the surface iteration order (cell order, then surface order within cells) and the per-bucket reverse-walk flush are unchanged. No material re-sorting is introduced anywhere, precisely to keep same-depth-bucket packet order stable.

## I. Concrete implementation plan

Ordered by the roadmap in L. File references are current at `0bab5cd8`.

1. **`engine/examples/editor-playtest/src/playtest_scene.rs`**
   - Now: emits `ROOM_VISIBILITY_FALLBACK_DRAWS` counter but not into the headless counter log; fallback path at `:608-701` calls the all-cells draw without a window.
   - Change: add the fallback count and `room-surface-profile` counters to the counter emission block (`:1040-1148`); pass `portal_cell_window(active.index)` and the far plane into the all-cells call.
   - Test: replay tape, assert hash-set equality; assert cells-drawn decreases on multi-room frames.
2. **`engine/crates/psx-engine/src/world_render/indexed_cache.rs`**
   - Now: fog gate at `:1077-1106` is room-level; cell loop computes AABB extents twice (`:391`, `:409`); depth sort every frame (`:439`); `draw_indexed_cached_room_vertex_lit_all_cells` (`:576-782`) takes no window; per-surface options copied by value 1-4x.
   - Change: (a) surface-level fog cutoff `metrics.max_z <= fog_near` in the warm gate; (b) single extent computation shared by both cell tests; (c) add `portal_window: Option<&PortalCellWindow>` + far plane to the all-cells variant; (d) wedge refinement hook (slice of admitting frustums); (e) borrow `&WorldSurfaceOptions` with a small per-surface override struct instead of by-value copies.
   - Memory: none. Expected: fog cutoff 250-450k/frame; window+far-plane 150-300k on tail frames; options borrowing 20-60k.
   - Tests: existing 270-test suite; new: warm-vs-dynamic byte equality across the fog_near boundary; window-parity test extending `portal_cell_window_union_keeps_every_admitting_path` to the all-cells path.
3. **`engine/crates/psx-engine/src/render3d/world_pass_gouraud.rs`** + **`render3d.rs`**
   - Now: lattice built per frame (`:356-442`): 5 midpoints, 216-byte lattice array, 11 CTC2 re-issued per surface, 4 leaf packets constructed from scratch.
   - Change: `submit_tomb_raider_warmed_lattice(leaf_packets: &mut [QuadTexturedGouraud; 4], lattice_uv_rgb: &CachedRoomSurfaceLattice, ...)`: project 9 points (existing `project_tomb_raider_view_lattice_gte` untouched), patch positions, push. Memoize the CTC2 view-projection load per room render, re-issue only when the GTE state was clobbered (model passes run after rooms, so per-room is safe; assert via a debug flag).
   - Expected: 250-400k/frame. Tests: extend the existing bit-exact lattice test to compare warmed-leaf output words against the dynamic path.
4. **`engine/crates/psx-engine/src/world_render.rs`**
   - Add `CachedRoomSurfaceLattice` (48 B, `#[repr(C)]`, size/align asserted against the level mirror like `:848-855` does today) and the per-wedge `cell_aabb_intersects_frustum` helper reusing the window test's inflation.
5. **`editor/crates/psxed-project` (cooker, `cook_visibility.rs`)**
   - Now: cooks cells/surfaces/PVS; PVS floods to all-ones at radius 32.
   - Change: emit the lattice table per surface using the engine's own midpoint functions (same pattern as it already calls `cache_room_vertex_lit_surfaces`); add a *bake-time validation report*: for every (cell, yaw-bucket) camera sample, run the real visibility path and report max cells/surfaces/primitives admitted (section K). Optionally fix `visibility_radius` semantics later; not needed for the budget once F.3 lands.
6. **`engine/examples/editor-playtest/src/active_rooms.rs` / `playtest_update.rs` (sim)**
   - Now: `apply_current_active_room_fields` and residency copy room fields every tick (19 memcpy call sites; ~48k/tick measured).
   - Change: copy on room-set/current-room transitions only; keep a dirty flag. Determinism gate: `route.csv` and `guest-hashes.csv` byte-identical before/after.
7. **`engine/crates/psx-engine/src/world_render/room_draw.rs` / megafunction split (later stage)**
   - Split classify and emit into two loops communicating through a scratch emit list:

```rust
struct EmitEntry { surface: u16, flags: u8 /* kind|warm|subdiv|risky */, depth_slot: u8 }
// pass 1: for accepted cells -> for surfaces: metrics, backface, screen, fog-cutoff,
//         subdivision decision -> push EmitEntry   (small loop body, no spills)
// pass 2: for EmitEntry -> patch-or-build packet, push_command   (order preserved)
```

   - Expected: recovers a fraction of the 9.6% self time via spill/I-cache relief; measure before committing (the earlier options-borrowing lesson applies: cadence, not stage time, is the acceptance metric).

## J. Benchmark and experiment matrix

All emulator runs use the same tape, disc recipe, and flags as the final capture; every run records `--profile-log`, `--counter-log`, gpu stats, visual hashes, and `--dump-hw`. "Hash gate" means: identical `display_hash` at every common presented tick.

| # | Hypothesis | Instrumentation | Baseline | Expected | Reject if | Visual | Platform |
|---|---|---|---|---|---|---|---|
| 0a | Counters exist but aren't logged | enable `room-surface-profile`, `cell-select-profile`, add fallback counter to CSV | n/a | per-frame surfaces/warm-hits/subdiv counts | overhead > 1% of frame | n/a | emu |
| 0b | Stage-tagged PC samples separate sim/render exactly | emu-side: tag each PC sample with the active guest telemetry stage | window regression (r=0.93) | memcpy split confirmed | n/a | n/a | emu |
| 1 | Fog gate is rank-1 | diagnostic build, `fog_enabled: false` | rsd 802k | rsd drops 250-450k; per-prim toward ~1,000 | drop < 150k | none (diagnostic; pixels differ where fog tints) | emu |
| 2 | Surface-level fog cutoff is pixel-identical and captures most of #1 | F.1 patch | rsd 802k, hashes | most of #1's drop, hash gate passes | hash mismatch, or < 60% of #1's drop | hash gate + contact sheet | emu |
| 3 | Cooked lattice + warmed leaves | F.2 patch + bit-exact cook test | rsd after #2 | further 200-350k | hash mismatch or cook-equality failure | hash gate | emu |
| 4 | Fallback windowing cuts multi-room tails | F.3 patch | cells drawn 25.4; tail frames | cells drawn -20-40% on 3-4-room frames; p99 render -150-300k | any hash mismatch (window must be pure subtraction of off-screen work) | hash gate | emu |
| 5 | Sim copies are event-drivable | F.4 patch + size-tagged memcpy log | update 160k/tick | update <= 120k; `route.csv` byte-identical | route divergence (determinism!) | hash gate | emu |
| 6 | v1-vs-v3 mechanism shares | both projects, current build, `room-surface-profile` on, route-equivalent corridor segments (record a v1 tape through its most enclosed area) | this session's captures | v1: high warm-hit ratio; v3 pre-fix: 0% warm hits | counters contradict the fog-gate story | n/a | emu |
| 7 | Emulator deltas hold on silicon | burn post-fix disc (existing drutil protocol), on-console FPS overlay soak in the worst corridor (the 4-room junction near guest_frame ~1100) | console FPS today | sustained >= 20 FPS, no tear/artifacts | console misses where emulator fits (would implicate DMA stealing / I-cache; then measure with the SIO probe protocol) | eyes on console | hardware |

## K. Worst-case performance proof

Upper bounds, per visual frame, after F.1-F.3, for cortex_v3 content:

| Quantity | Bound | Source of bound |
|---|---:|---|
| Rooms drawn | 6 (all) | room count; draw order caps at active rooms |
| Portal windows retained | <= 64 frustums total, 1-2/room typical | traversal caps |
| Cells tested (candidates) | sum of drawn rooms' cells <= 104 | PVS all-ones (today); cooker-validated after |
| Cells accepted | <= candidates; measured 25.4 mean, cooker-validated max | F.3 + bake report |
| Surfaces visited | <= 6 x accepted cells (max surfaces/cell = 6) | cooked cell records |
| Unique vertices projected | <= 429 (whole set) | cooked vertex count |
| Subdivision leaf quads | <= 4 x eligible visited | LEVELS = 1 |
| Crack-cover prims | <= 1 x subdividing visited | band rule |
| GPU packets | <= 800 (arena cap, overflow-guarded) | `WorldRenderPass<800>` |
| OT operations | 1 insert/packet @ ~36 cycles | reverse-walk flush |

**The proof is a cooker validation, not a runtime clamp.** At bake time, for every camera sample (each of the 104 cells x 8-16 yaw buckets x 2 heights), run the actual visibility code (portal traversal, windows, wedges, cell tests) and record the admitted (cells, plain surfaces, subdividing surfaces, primitives). Take the maximum over all samples plus one hysteresis-step of slack. Apply the frame model:

```
render_max = 430k + 5k x rooms + 1.1k x cells + 0.06k x verts + 0.85k x plain + 1.5k x subdiv
```

Example with the observed worst view (4 rooms, ~79 candidate cells, ~46 accepted, ~110 plain + ~70 subdividing surfaces, ~300 verts): `430k + 20k + 87k + 18k + 94k + 105k ~= 754k`, against the 1,137k three-vblank allowance: 34% headroom for spikes, streaming, and model-count variance. If a future map's bake report exceeds the allowance, the bake fails with the offending camera sample listed, and the *content* is fixed (a portal moved, a room split), never the image. The coefficient table is re-measured whenever the emit code changes (experiment 0a gives it for free). This is the enforceable invariant the current engine lacks: today nothing bounds "rooms x cells x surfaces x subdivision" except level design luck.

Residual unbounded terms and their guards: primitive arena (800 cap, overflow counters already exist, bake report must show margin); player/equipment (fixed rig, 200k measured, stable); props (bounded per room after prewarming); streaming (event-driven, off the visual path).

## L. Staged roadmap

| Stage | Content | Expected gain | RAM/ROM | Eng. risk | Visual risk | Rollback |
|---|---|---|---|---|---|---|
| 1. Instrumentation | 0a + 0b (counters to CSV, stage-tagged PC samples) | knowledge | ~0 | trivial | none | delete flags |
| 2. Exact low-risk | F.1 fog cutoff; F.4 sim copies; cylinder-prop prewarm; single extent computation; options borrowing | 400-650k/frame + 130-160k/period | ~0 | low (each gated by hash + determinism) | none if hash gate holds | each is an independent small patch; revert individually |
| 3. Cooker format | F.2 lattice table (+12.5 KB pack, +36 KB pool in the leaf-pool variant); bake validation report | 200-350k/frame; the worst-case proof | +12.5 KB ROM, 0-36 KB RAM | medium (format bump, cook-equality test mandatory) | none (bit-equal by test) | keep dynamic path as fallback behind a feature flag for one release |
| 4. Renderer architecture | F.3 fallback windowing + wedge refinement; megafunction phase split | 150-300k on tails; codegen 50-150k | ~0 | medium (ordering must be preserved; measure cadence not stages) | low; hash gate | window param defaults to `None` = old behavior |
| 5. Hardware validation | burn, worst-corridor soak, SIO probe if console diverges | confidence | n/a | n/a | n/a | n/a |

Projected end state: render mean ~700-850k (1.3-1.5 vblanks), p99 under ~1.1M, every period fits n = 3 with the sim at ~120k/tick; cadence histogram collapses to {3}, i.e. a locked 20 FPS, with the bake report guaranteeing it for future content. If stage 2+3 land fully, many frames fit n = 2 and the game will oscillate 20-30 FPS; a fixed 3-tick pacing knob (present-hold) can lock 20 for consistency if preferred.

## M. What not to do

- **Single-aperture room clipping**: rejects geometry visible through the other portal path; the multi-path union/wedge scheme exists precisely because rooms are multiply-admitted. Guard: the parity test on the all-cells path plus the wedge test's "outside *all* wedges" rule.
- **Geometry removal, draw-distance cuts, subdivision reduction, crack-cover removal**: all change pixels (8,324 px measured for crack-cover alone). Every optimization here re-times existing arithmetic; none alters it. The hash gate is the enforcement mechanism, run on every experiment.
- **Broad view caching** (the rejected view-space cache): caching view-dependent values trades recompute for memory traffic on a machine with no D-cache; it lost once and will lose again. The only "caches" in this plan hold view-*independent* data (UVs, colors, packets).
- **Local-stage wins that regress cadence** (the rejected model-options borrowing): the acceptance metric for every stage-4 change is the cadence histogram and total run cycles, never a stage column alone. The earlier regression was almost certainly a code-layout/register-pressure side effect elsewhere; the megafunction split must be judged the same way.
- **Reintroducing dense-init removal**: the self-initializing bitset already solved it; the dense path's remaining O(room-vertices) work (identity permutation fill, full projection) is real but small at 429 vertices; leave it until the bake report says otherwise.
- **Chunked blend flushes and other measured-negative micro-moves**: banked as negative results; nothing in this plan touches them.

## N. Final recommendation

**Implement F.1 first**: the surface-level fog cutoff in the warm gate (`indexed_cache.rs:1077-1106`), comparing the already-computed `metrics.max_z` against `lighting.fog_near`. It is ~15 lines, it attacks the single largest measured differential between the two projects, and it is provably pixel-identical (weight is 0 by definition below fog_near).

**First benchmark**: replay `cortex_v3.pxtape` on the rebuilt disc with the standard instrumentation. Success: `room_surface_draw` mean drops by >= 250k (target 300-450k), the cadence histogram's 5-tick bucket shrinks materially, and the visual-hash gate passes at every common presented tick.

**Expected saving**: 250-450k cycles per visual frame (high confidence for >= 250k, given the v1/v3 per-primitive gap and the sample mass in the dynamic submit family).

**Most important correctness risk**: a surface whose four projected `sz` values straddle `fog_near` across frames will flip between warm and dynamic paths; both must produce byte-identical packets at the boundary (weight 0 on one side, weight > 0 only strictly beyond near). The boundary-equality unit test in section I.2 covers exactly this.

**Go/no-go**: go if the hash gate passes and rsd drops >= 250k; no-go (and escalate to stage-tagged PC sampling before touching anything else) if the drop is under 150k, because that would falsify the fog-gate ranking and promote the subdivision constant (F.2) to first place.
