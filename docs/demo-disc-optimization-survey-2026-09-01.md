# Demo-disc optimization survey: Cortex Ignition 0.4, quake-psx, hl-psx

Date: 2026-09-01. Base: PSoXide `d812d5bd` (worktree
`claude/psoxide-demo-disc-optimization-923892`), quake-psx `2ee30a8`,
hl-psx `571564a`, psx-demo-disc `ee7b3d7` (+ uncommitted Makefile pin edit).

This is a re-survey, not a re-derivation. Two documents from 2026-08-22
already rank the harmonisation work in detail and should be read first:

- `docs/engine-harmonisation-2026-08-22.md` (blockers B1..B5, proposals H1..H12)
- `docs/engine-harmonisation-plan-2026-08-22.md` (items H-00..H-32, rejections)

What this document adds:

1. which of their facts moved in the ten days since (section 1);
2. fresh emulator-side measurements taken today on the shipping discs of all
   three games with the same instrument (section 2);
3. a Cortex 0.4 gameplay attribution down to the function, which the 08-22
   docs did not have because the BSP room path has no sub-stage telemetry
   (section 3);
4. a visual-defect census with crops (section 4);
5. the ranked opportunity list that follows from 1..4 (section 5);
6. exact recipes so the next agent does not spend an hour re-finding them
   (section 6).

Evidence files live next to this document in
`docs/demo-disc-optimization-survey-2026-09-01/` (untracked; commit or not is
the owner's call). The scratchpad that produced them is ephemeral.

---

## 1. Verified state today (re-verify, these move daily)

| Claim in the 08-22 docs | Today |
|---|---|
| The disc presses four PSoXide revisions | **Mostly fixed 2026-08-30.** `PROGRAMS_EXPECTED_PSOXIDE_REV` and `QUAKE_EXPECTED_PSOXIDE_REV` are both `930b1201`; Quake and HL were repinned to it (`da8d30d`, `3071e93`, `11ee5b2`). Cortex-current is `99ba6890` in an **uncommitted** Makefile edit. H-01 is done except that last pin. |
| Convergence seams uncommitted in three repos | Landed. `scratchpad`, `attributed_clip`, `projection`, `psx-render-contract` are tracked and both games consume them via path deps into their hydrated `.psoxide/`. |
| quake-psx renderer.rs is a 4,317-line fork of `psx_bsp::render` | Now **8,488 lines** and still does not call `psx_bsp::render` or `psx_bsp::sky`. It uses `psx_engine::classic_affine` submitters plus `psx_bsp::{collision, mover, resident, pxbsp}`. 20 function names are shared with `psx-bsp/src/render.rs` (120 fns) and drift per name. Quake carries **57 `renderer-*` Cargo features**, of which 9 are the accepted stack; the other 48 are dormant experiments compiled conditionally. |
| hl-psx keeps its own HMD8 reader and PVS decoder | Unchanged. main.rs is 33,523 lines. It imports only `psx_engine::{scratchpad, projection, attributed_clip}` from the engine. `psx_asset::hmd8` still has no consumer. |
| Three RLE PVS decoders | Still three: `psx-bsp/src/pxbsp.rs:81`, `quake-psx game/src/renderer.rs:8436`, `hl-psx game/src/main.rs:19659`. `world_batch` is still declared at `quake-core/src/lib.rs:40`. |
| quake-psx pins a clean PSoXide rev | Its hydration source is a **local tree** `/private/tmp/psoxide-quake2-harmonize` at `e31ea70b` = PSoXide main + 1 commit ("Add renderer-owned resident map profile") on branch `codex/quake2-renderer-harmonization`. Not an ancestor of main; main is 125 commits past its base. `host/quake-build/main.rs:31` still expects `930b1201`. |
| hl-psx hydration | `.psoxide-source` = `git:897e90de` (2026-08-21), which is **before** `930b1201`. The disc repins HL through `PSOXIDE_FROM`, so the disc build and the repo's own build use different SDKs. |
| `psx-bsp/src/render.rs:375 vertex_outside_mask` does four i64 multiplies per vertex (memory `cortex-room-is-psx-bsp`) | **Resolved.** It is now `#[cfg(test)]` reference code; the live path is the plane-major `FrustumPlanes::cull_polygon` (`render.rs:488`, used at `:1879`). |
| Cortex has RAM headroom | **No.** Adding `emulator-telemetry` to the 0.4 shipping feature set fails the link: `.bss` overflows RAM by **11,452 bytes**. The shipping payload is 1,122,304 bytes (`t_addr 0x80010000 .. 0x80122000`). The July RAM survey's item 1..3 (cook the fixed capacities, ~230 KB) is now blocking instrumentation, not just tidy. |

Manny's live checkout (`~/Desktop/repos/PSoXide`, branch
`fix/bsp-on-plane-leaf-lookup`, `958a6603`) is 184 files / +6,639 ahead of
`d812d5bd` (vitality systems, HUD, Cortex 0.4 polish) and dirty. Nothing in
this survey was measured on that tree; all Cortex numbers are `d812d5bd`.

---

## 2. Measurements taken today

Instrument: the PSoXide frontend built from `d812d5bd`, headless `launch`, on
each game's shipping disc. No guest telemetry is needed for any of these; the
emulator produces them out of band (`--route-log`, `--cpu-cycle-profile-log`,
`--pc-line-log`, `--ram-load-stall-line-log`, `--icache-event-log`,
`--pc-sample-callsite-log`, `--dump-draws`, `--dump-hw` at 4x).

### 2.1 Frame rate and CPU cycle attribution (gameplay windows)

fps = display-start flips per route tick (one route tick = one vblank).

| Game / window | fps | instr per vblank | issue | RAM load stall | stack load stall | store stall | MMIO stall | I-cache refill | mul/div interlock |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Cortex 0.4, standing at spawn, held forward into the guardrail (ticks 4500..6488) | **20.0** | 256,311 | 45% | 35% | 10% | 3% | 3% | 10% | 5% |
| Cortex 0.4, idle at spawn (cortex6) | **20.0** | 257,562 | | | | | | | |
| hl-psx, tram ride c0a0 (ticks 3000..4051) | 11.5 (6.6..17.8 by 300-tick window) | 228,780 | 40% | 27% | 10% | 3% | **14%** | 13% | 2% |
| quake-psx, E1M1 spawn view, no input (ticks 2800..3231) | 13.5 | 273,769 | 48% | **40%** | 6% | 3% | 0% | 6% | 2% |

Reading:

- **RAM load stalls are the shared cost.** 37..46% of every vblank in all three
  games is the R3000 waiting on uncached main-RAM loads. Issue is under half.
  The hl-psx PERF_HANDOFF and quake RENDERING.md both already conclude "memory
  bound, not instruction bound"; Cortex is the same. hl-psx's uncached-read
  idioms (4-aligned hot records, `get_unchecked` byte reads, word LUTs) are
  therefore the one perf technique that cascades to all three by construction.
- Cortex holds a rock-steady 20.0 fps in every 300-tick window: it renders
  every third vblank. To reach 30 it must fit in two, i.e. drop ~25% of a
  frame's work. Of the instructions retired per vblank, **12% are the vblank
  spin** (`App::run_scheduled` lines `0x80020820..0x8002084c`: `lw`/`lw`/
  `subu`/`sltiu`/`bnez`), and that spin also accounts for 20% of the RAM load
  stalls. So the real work is ~88% of the slot.
- hl-psx's dips coincide with I-cache windows at ~47k and MMIO stalls at 14%:
  that is CD streaming during the tram ride, not the renderer. Its steady
  windows read 18..19 fps, matching its documented 18.26 route average.
- Quake's 13.5 fps is the heaviest possible view (the spawn hall, 3,084
  polygons in the dumped frame) on the free-running shipping loop; its
  documented 23.856 fps is the fixed-ticks E1M1 route average. Do not compare
  the two numbers. Re-measure with `host/quake-build e1m1-chain-bench` for a
  Quake baseline.

### 2.2 Primitive size census (last frame of each dump)

Per-frame `--dump-draws`, polygons after the last fill command. "Extent" is
the larger of the screen-space width and height of a primitive.

| Game | polys/frame | textured quads / tris | mean extent | textured polys >32 px | >48 px | >64 px | >128 px |
|---|---:|---:|---:|---:|---:|---:|---:|
| Cortex 0.4 (spawn view) | 364 | 116 / 248 | 29.2 px | 9% | 9% | 8% | 5% |
| quake-psx (spawn hall) | 3,084 | 536 / 2,548 | 24.3 px | 16% | 10% | 8% | 4% |
| hl-psx (tram) | 526 | 213 / 313 | **50.2 px** | **57%** | **40%** | 23% | 5% |

hl-psx draws six times fewer polygons than Quake at a lower frame rate, and
the polygons it draws are twice as large on screen. That is the measured shape
of "no BSP tessellation": Quake and Cortex run `ClassicAffineProfile`'s depth
bands (`subdivide_once_at 136`, `subdivide_twice_at 60` OTZ; PXBSP uses
272/136) on every admitted surface, hl-psx runs a **bounded 16-candidate
per-frame patch pool** (`AFFINE_PATCH_CANDIDATE_CAP = 16`,
`WORLD_AFFINE_MAX_EXTRA_PACKETS = 176`, one 2x2 split per native patch) on
top of cook-time UV-grid cuts, with `WORLD_CLASSIC_AFFINE_SELECTION = true`
selecting by a near-presence rule (`RANKED_NEAR_PRESENCE_Z` / `_SPAN_PX`).

### 2.3 Cortex 0.4 gameplay attribution (exact PC-line counts, ticks >= 4500)

Built from the worktree with the shipping feature set (`cd-stream-bench`),
exe SHA-256 `1de5e327…16bb`, linker map in the evidence folder. Attribution by
`tools/pc_line_attribution.py` against that map. Two runs: held forward into
the spawn guardrail (cortex4) and idle (cortex6).

| Symbol | held forward | idle |
|---|---:|---:|
| `App::run_scheduled` (vblank spin, see 2.1) | 12.2% | 19.6% |
| `Playtest as Scene::render` (inlined room/sky/HUD body) | 18.5% | 18.6% |
| `psx_bsp::collision::CollisionHull::trace_into` | **18.1%** | **11.0%** |
| `render3d::submit_textured_model_geometry_impl` (player + enemies) | 10.9% | 11.3% |
| `psx_bsp::render::Renderer::draw_pxbsp_faces` | 9.4% | 10.0% |
| `CollisionHull::point_contents_from` | 4.1% | 3.6% |
| `render3d::flush_blended_model_vertex_chunk` | 3.2% | 3.2% |
| `CharacterBlockerTraceProvider::trace_into` (per-model loop) | 1.9% | |
| `psx_pad::poll_once_diag` (SIO0 setup-delay busy wait, required on silicon) | 1.7% | 1.7% |
| `psx_bsp::collision::plane_contact` | 1.3% | |
| `BspRuntime::visible_bounds_mask` + its `memcmp` | 1.1% + 0.9% | 1.4% |
| `compiler_builtins u64_div_rem` + `__divdi3` (64-bit divides) | 1.1% + 0.2% | 1.1% |
| `memset` (draw_pxbsp_faces 30%, Scene::render 28%, resolved_focus 13%) | 0.9% | 1.0% |
| `memcpy` | 0.9% | 0.9% |
| `AnimationPoseSample::pose` + `pose_v3_unchecked` | 1.7% | |

RAM load stall attribution (same run) follows the same order: spin 19.7%,
`Scene::render` 15.0%, `trace_into` 14.9%, model submit 11.5%,
`draw_pxbsp_faces` 10.6%, `point_contents_from` 5.0%.

I-cache refills (20.9M events in the window) by incoming function:
`trace_into` 11.0%, provider `trace_into` 9.3%, `plane_contact` 8.4%,
`update_gameplay` 8.0%, model submit 7.7%, `Scene::render` 5.2%,
`AnimationPoseSample::pose` 4.9%, `point_contents_from` 2.8%. **Collision is
~31% of all I-cache refills**: `plane_contact` is a separate out-of-line
function called from the tracer's inner loop and the two evict each other
against the renderer.

Exact eviction pairs (victim -> incoming) make the two ping-pongs explicit:

| Pair | share of refills |
|---|---:|
| `trace_into` -> `plane_contact` | 5.0% |
| `plane_contact` -> provider `trace_into` | 3.0% |
| `plane_contact` -> `trace_into` | 2.6% |
| provider `trace_into` -> `trace_into` | 3.2% |
| `AnimationPoseSample::pose` -> model submit | 4.6% |
| model submit -> `AnimationPoseSample::pose` | 3.8% |

So two call triangles (tracer / plane_contact / provider, and pose sampling /
model submit) alias each other in the 4 KiB direct-mapped cache. Inlining
`plane_contact` and hoisting pose sampling out of the per-part submit loop are
the layout-level fixes those pairs point at.

Callsite recovery (`--pc-sample-callsite-log`):

- `memcmp` is 100% from `BspRuntime::visible_bounds_mask`
  (`editor-playtest/src/bsp_runtime.rs:575`, `cached.bounds == *bounds` on a
  derived `PartialEq` struct compiles to a `memcmp` call).
- `point_contents_from` is 80% from `trace_into` and 20% from
  `BspRuntime::player_contents` (liquid sampling of the standing player).
- `__divdi3` is 84% from `psx_bsp::collision::plane_contact`; the `u64`
  fast path there (`collision.rs:722..756`) already comments on this cost, and
  the wide path is what remains.
- The provider (`psx-bsp/src/collision_provider.rs:156..175`) runs every trace
  against the world hull **and then every brush model** unconditionally
  (`for model in self.models.iter()`), with no bounds pre-test.

### 2.4 What the idle A/B says

With no input the player still spends **11.0% + 3.6% = ~15% of retired
instructions in collision**, and the frame rate does not change (20.0 fps both
ways) because the scheduler is quantised to whole vblanks. The character motor
issues its stand/floor/contents probes every fixed tick regardless of motion,
and each probe walks the world hull plus every model hull.

---

## 3. Cortex Ignition performance: what the data says the levers are

Ordered by measured size, all on the `d812d5bd` shipping build at the 0.4 spawn.

1. **Collision, ~15% idle / ~25% moving against geometry.** Three independent
   cuts, each gated on the combat-checkpoint replay hash:
   - Skip traces whose segment AABB misses a model hull's bounds
     (`collision_provider.rs:166`, `:233`). Today every model is traced every
     query.
   - Rate-limit or cache the idle probes in `character_motor.rs` (stand
     position, supporting floor, `player_contents`) when the motor's input and
     position are unchanged since the last fixed tick.
   - Make `plane_contact` `#[inline(always)]` into `trace_into` and take the
     `i64` out of the common path (`plane_contact` computes both distances in
     `i64` before deciding). This is the same tracer quake-psx uses, so a win
     lands in both; quake's RENDERING.md already names `trace_into` as 7.36%
     of its frame and "an explicit-stack hull trace is untried".
2. **Model submit + blended vertex flush, ~14%.** Already the subject of the
   E6 rewrite thread in memory (`cortex-30fps-architecture-review`). Nothing
   new measured here beyond confirming its share on 0.4.
3. **`Scene::render` body + `draw_pxbsp_faces`, ~28%.** The room path has no
   sub-stage markers, so this is where `--pc-line-log` is the only instrument.
   Next step is a line-level view of `draw_pxbsp_faces` and the inlined body
   (the `hot lines` table from `pc_line_attribution.py --limit 200`) to find
   which loop dominates. Not done today.
4. **Small, mechanical, cascade-free:** `visible_bounds_mask` field compare
   instead of struct `==` (kills the `memcmp`, ~1%); `memset` of per-frame
   arrays in `draw_pxbsp_faces` and `Scene::render` (~0.6%); the remaining
   64-bit divide in `plane_contact` (~1.3%). Together ~3%.
5. **RAM headroom before any of this**: the telemetry build does not link.
   Items 1..3 of `docs/ram-vram-survey-2026-07-25.md` (cook `MAX_BOX_PROP_STATE`
   and the other fixed capacities, ~230 KB) are now a prerequisite for
   profiling Cortex with guest telemetry at all.

### 3.1 64-bit arithmetic in the guest (rule decided 2026-09-01: none, cooker only)

The Cortex 0.4 guest links `__divdi3`, `__moddi3`, `__udivdi3`, `__umoddi3` and
compiler-builtins `u64_div_rem` (1,412 B); in gameplay they retire ~1.4% of
instructions, all via `psx_bsp::collision::plane_contact`. Multiplies and
shifts on `i64` are inlined and invisible to the profiler, so the source audit
is the only census. Live sites (test modules excluded), hottest first:
`psx-bsp/src/render.rs:382..458` (`FrustumPlanes`, i64 plane distances per
vertex per plane), `psx-bsp/src/collision.rs:702..757` (`plane_contact`),
`psx-engine/src/attributed_clip.rs:44,84,86` (shared clip kernel),
`sdk/crates/psx-gte/src/scene.rs:930..945` (software AVSZ),
`psx-math/src/int32.rs:118` (`mul_q12`, used widely), `ui.rs:330`,
`vitality.rs:695`, `psx-asset/src/lib.rs:975`, `render3d.rs:2726`,
`bsp_runtime.rs:1158..1194`, `poi.rs:154..177`, `mover.rs:273`, `sky.rs:560`,
`world_render.rs:116,1833,1915` (grid path). `u64` bit sets
(`visible_bounds_mask`, `spatial_active_mask`, `visible_instance_mask`,
`psx-vram` rows) are exempt: no arithmetic. quake-core has 18 sites, hl-psx
95, in their own trees. Gate for the cleanup: no `di3` / `div_rem` symbols in
the guest link map.

What is **not** a lever (measured before, do not retry): GTE offload
(`keep-work-on-gte-not-cpu`), scratchpad (99.6% full in the room path), the
retained packet pool for the BSP room, blend-chunk flushing, NCLIP/AVSZ3.

---

## 4. Visual-defect census (HD frames in the evidence folder)

| Game | Defect | Where in the frame | Mechanism | Fix owner |
|---|---|---|---|---|
| hl-psx | Saw-tooth affine warp on the tram rail and floor strips | `hl-tram-rail-warp-crop.png` | Long, thin, foreshortened GT4s left whole; 40% of textured polys are >48 px (2.2). The 16-candidate pool spends its budget on the near-presence rule, not on high UV-span-per-depth edges | hl-psx selection policy; the shared `predicted_warp_16ths` criterion (`ps1-texture-warping-measured`) is the ranking function it lacks |
| hl-psx | 1-px black cracks in the ceiling, and a sliver lattice at the left ceiling edge | `hl-tram-ceiling-cracks-crop.png` | T-junctions where a split patch borders an unsplit neighbour. hl-psx has a `seam-census` feature that tints by emit mechanism, so the bordering mechanisms can be named per pixel | hl-psx (its `blocked_edges` / edge-reservation logic) |
| Cortex 0.4 | Thin dark diagonal seam across the left wall panels | `cortex-0.4-left-wall-seam-crop.png` | Unverified. Candidates: an underdraw crack-seal triangle (`underdraw_slot_bias 8`) sorting in front, or a subdivision edge mismatch between adjacent brush faces | PSoXide `classic_affine` / PXBSP; reproduce with `--dump-draws` and find the primitive covering that pixel |
| Cortex 0.4 | Left rail lines step by a pixel where a vertical post crosses them | same crop | Probably integer snap of shared vertices projected through two different subdivision levels | same as above |
| quake-psx | None seen in the spawn hall at 4x | `quake-spawn-4x.png` | | |

### 4.1 Cortex 0.4 world UV wrap (root cause found 2026-09-01, evening)

Manny's symptom: textures and UVs look right in the editor and wrong in-game
from the first gameplay frame. Chain, each link verified:

1. The spawn frame's world is 31 textured polys (`cortex-0.4-spawn-draw-groups.png`:
   blue = guardrail, green = deck). The deck quad is 508 px wide on screen
   and carries vertex UVs U 0..64, V 0..128: one texture repeat stretched over
   the whole platform. VRAM is fine (`cortex-0.4-vram-page640-decoded.png`,
   every DP City texture decodes cleanly with its CLUT) and the GP0(E2)
   windows point at the right slots, so the texels are right and the
   coordinates are wrong.
2. `psxed-project/src/brush_pack.rs:480` packs vertex UVs as
   `(value.round() as i64).rem_euclid(256) as u8`. Any surface whose texel
   span exceeds 255 wraps; `brush::rebase_texel_uvs` (`brush.rs:1524`) only
   shifts spans that fit and documents "axes whose span cannot fit the
   window are left alone (the historic wrap behaviour)".
3. `brush_world.rs:953` calls `subdivide_surfaces_to_budget(...,
   ENGINE_SURFACE_EXTENT_UNITS, MAX_RESIDENT_WORLD_FACES = 6144, ...)`.
   `brush_compile.rs:170` doubles the extent cap until the level fits the
   face budget. At the fine extent every patch fits the u8 window; after a
   doubling, surfaces span more than 255 texels at 16 units per texel and
   wrap. Nothing warns.
4. 0.3 fit the budget at the fine extent; 0.4 added enough brushes that the
   loop coarsened. The editor preview subdivides at the authored constant
   and renders float UVs, so it never wraps.

Fix shape (cook only, no runtime change): cap `coarse_extent` at the largest
extent whose texel span still fits 255 for that material's repeat, and when
the face budget still cannot be met either split along the texture axes at
240 texels (qbsp `SubdivideFace`) or fail the cook with the face count,
instead of silently wrapping. Add a cook-time count of wrapped faces so the
regression is visible in the log.

Manny said Cortex "has a couple of rendering issues". The seam above is the
only one visible at the spawn; the others need his list or a route that
reaches them. Do not start a fix without reproducing it in `--dump-hw` first.

---

## 5. Ranked opportunities

Ranking is cascade value divided by (risk x cost), same rule as the 08-22
docs. H-numbers refer to those docs; new items are S-numbered.

### Tier 0: unblock (small, do first)

- **S-01 Commit the Cortex-current pin** (`psx-demo-disc/Makefile`, uncommitted
  `99ba6890`) or collapse it onto `930b1201` per H-01. One line.
- **S-02 Cortex RAM headroom** (section 3 item 5). Without it no Cortex
  guest-telemetry build links, so every other Cortex perf item has to be
  measured emulator-side only.
- **S-03 quake-psx SDK provenance.** Its `.psoxide` is hydrated from a
  `/private/tmp` worktree on an unmerged branch. Either land `e31ea70b` on
  PSoXide main and repin, or re-hydrate from `930b1201`. Until then the disc's
  Quake `.bin` and the repo's build come from different SDKs (B3 again).

### Tier 1: cascade to all three

- **S-04 Uncached-read minimisation as a shared discipline.** 37..46% of all
  cycles in all three games are RAM load stalls (2.1). The hl-psx idioms are
  in `hl-psx-transferable-techniques`. Apply first to the two shared hot
  paths measured today: `psx_bsp::collision::trace_into` (node/plane record
  reads) and `classic_affine` submit. Gate: byte-exact display/VRAM hash on
  each game's canonical route.
- **S-05 Collision tracer** (section 3 item 1). Shared between Cortex and
  Quake today; H-14/H7 would bring hl-psx onto it too, and the AABB pre-test
  is the first thing hl-psx's `phys.rs` would want back.
- **H-13 / H8 one PVS decoder, H-11 delete `world_batch`**: still open, still
  cheap, still three copies.

### Tier 2: the BSP renderer fork (the item Manny's question is about)

- **H-10 / H5 one BSP traversal.** The gap grew: quake's fork is 8.5k lines
  with 57 feature flags. The 08-22 plan's four-step order (move the 20 common
  free functions down unchanged, then `materialize_face`, then
  `mark_visible_faces`, then `draw_frame`) still holds. New recommendation:
  **prune the 48 dormant `renderer-*` features first** (RENDERING.md's
  "Closed by measurement" sections name most of them). Un-forking a renderer
  that carries 48 conditional experiments is several times the work of
  un-forking the 9-feature accepted stack.
- **S-06 Adaptive subdivision criterion into hl-psx.** Not "adopt
  `psx_bsp::render`" (rejected in the 08-22 doc: different algorithm). Only
  the ranking function: hl-psx keeps its bounded pool and its emitters, but
  ranks candidates by predicted warp (`err = du * |zb-za| / (2(za+zb))`, the
  integer port already exists as `predicted_warp_16ths`) instead of the
  near-presence rule. Gate: hl-psx `affine-heatmap` build before/after on the
  tram and c2a5 scenarios, packet pressure unchanged.

### Tier 3: single-game, cheap

- Cortex `visible_bounds_mask` `memcmp`, `memset`s, `plane_contact` `i64`
  (section 3 item 4).
- Cortex spawn seam (section 4): reproduce, then fix in `classic_affine`.
- hl-psx ceiling cracks: run the `seam-census` build on the tram frame.

## 5.1 Merged task list (2026-09-01 evening, agreed with Manny)

Benefit column names the class and the game. Basis is the survey measurement.
UI/HUD code is parked while Manny's UI work is in flight.

| # | Task | Repo, files | Benefit | Basis | Gate | Size |
|---|---|---|---|---|---|---|
| P1 | Segment-AABB pre-test before each model hull trace | PSoXide `psx-bsp/collision_provider.rs:166,:233` | Perf: Cortex, Quake | tracer 11% idle / 18% moving, provider 1.9%, every query walks every model | combat-checkpoint hashes; Quake E1M1 parity | S |
| P2 | `plane_contact` inlined, rewritten in i32 (kills `__divdi3`/`u64_div_rem`) | PSoXide `psx-bsp/collision.rs:701..757` | Perf: Cortex, Quake | 1.4% instructions in 64-bit divides; 10.6% of I-cache refills | as P1 + no `di3` symbols in link map | S |
| P3 | Skip/cache idle motor probes when position and input unchanged | PSoXide `psx-engine/character_motor.rs` | Perf: Cortex | ~15% of instructions in collision at idle | combat-checkpoint + door replay | M |
| P4 | `FrustumPlanes` in i32 | PSoXide `psx-bsp/render.rs:382..458` | Perf: Cortex | per vertex per plane per PVS face | 120-visual-frame exact capture | M |
| P5 | `attributed_clip` fractions in i32 | PSoXide `psx-engine/attributed_clip.rs:44,84,86` | Perf: all three | per clipped edge, shared | byte-exact hashes on each route | S |
| P6 | Hoist pose sampling out of the per-part submit loop | PSoXide `render3d`, `psx_asset` pose | Perf: Cortex | 8.4% of refills pose/submit evictions | hash gate | M |
| P7 | `mul_q12` split multiply + warm i64 sites (ui, vitality, asset, software AVSZ) | PSoXide `psx-math/int32.rs:118` etc. | Perf: Cortex small; rule | inlined i64 products | hash gate + link map | S |
| P8 | `visible_bounds_mask` field compare, per-frame memsets | PSoXide `bsp_runtime.rs:575`, `draw_pxbsp_faces`, `Scene::render` | Perf: Cortex | ~2.9% | hash gate | XS |
| P9 | Cook the fixed capacities (RAM headroom) | PSoXide, RAM survey items 1..3 | Enabler | telemetry build over by 11,452 B | telemetry links; shipping hash unchanged | M |
| P10 | Uncached-read discipline on tracer records and `classic_affine` submit | PSoXide | Perf: all three | RAM load stalls 37..46% of cycles | hash + cycle-profile stall share | L |
| P11 | Model submit rewrite (E6) | PSoXide `render3d` | Perf: Cortex | ~14% | hash gate | L |
| V1 | Stop UV wrap in the cook (extent cap by texel span, split at 240, fail loudly, log) | PSoXide `brush_compile.rs:170`, `brush_pack.rs:480` | Visual: Cortex (reported bug) | deck quad 508 px, U 0..64 | recook, preview == in-game | S |
| V2 | Rank HL patch candidates by predicted warp | hl-psx `rank_affine_patch` | Visual: HL | mean extent 50 px, 40% > 48 px | affine-heatmap tram + c2a5, packets unchanged | M |
| V3 | Close HL T-junction cracks | hl-psx edge reservation, seam-census | Visual: HL | 1-px ceiling cracks | seam pixels gone, equal packets | M |
| V4 | Cortex spawn wall seam | PSoXide `classic_affine` | Visual: Cortex | one line, unverified cause | dump-draws repro first | S |
| V5 | HL per-polygon cost (PERF_HANDOFF rewrite) | hl-psx | Enabler for V2 | 526 polys at 18 fps vs Quake 3,084 at 24 | its harness | L |
| I1 | Land `e31ea70b` on main, repin Quake, rehydrate from git, refuse `local:` for disc/ship | PSoXide, quake-psx | Provenance | Quake builds from /private/tmp | quake-build check, bench in noise | S |
| I2 | Commit or collapse the Cortex-current pin | psx-demo-disc Makefile | Provenance | uncommitted 99ba6890 | make check | XS |
| I3 | hl-psx own pin to 930b1201 | hl-psx | Provenance | repo hydrates 897e90de | regress matrix | S |
| I4 | Link-map gate on `di3`/`div_rem` | PSoXide guest build | Rule enforcement | only proof a site is gone | self | XS |
| I5 | Prune dormant renderer features, then one BSP traversal (H-10) | quake-psx, PSoXide | Cascade | 8.5k-line fork, 57 flags | parity + bench | L |
| I6 | One PVS decoder, delete world_batch | all three | Cascade | three copies | hash gates | S |

Dependencies: P1+P2 ship together (one gate). V2's ranking swap is free; a
larger split budget waits on V5 or a measured packet reserve.

### Do not re-open (measured rejections, section 6 of the 08-22 plan)

Clipping arithmetic policy, `psx-engine` depending on the contract crate,
dynamic dispatch in hot loops, extracting from hl-psx `play()`, scratchpad
staging, full double-buffered OT, `opt-level = "s"`, global two-level affine
selector in Quake, GTE work moved to CPU.

---

## 6. Recipes (all verified today)

Frontend: `~/Desktop/repos/PSoXide/target/release/frontend` (Manny's checkout,
built today) or the worktree's `target/release/frontend`
(`cd emu && cargo build -p frontend --release`, ~3 min cold).

Cortex 0.4 shipping disc (Manny's):
`editor/projects/cortex-ignition-tech-demo-0.4/baked/cortex_ignition_tech_demo_0_4.cue`.
It boots `UiScene(3)` and then a splash that waits for CROSS; the Makefile's
`CORTEX_ANIM_PRESS` fires too early for this project. Working entry:

```bash
frontend launch --path <cue> --embedded-playtest --steps 1500000000 \
  --press "1000:cross:30,1500:cross:30,2000:cross:30,2500:cross:30,3000:cross:30,3500:cross:30,4000:cross:30" \
  --hold-forward --route-log route.csv --dump-hw final.ppm --dump-hash
```

Gameplay is on screen from about route tick 3000; use 4500 as the start of
any windowed log. `--hold-forward` walks the player into the spawn guardrail,
so omit it for an idle baseline.

Cortex 0.4 with a matching linker map, without touching Manny's stage root:

```bash
cd <worktree>/emu && PSOXIDE_GUEST_STAGE_ROOT=/tmp/psoxide-psx-guest-survey \
  PSOXIDE_GUEST_CARGO_HOME=/tmp/psoxide-psx-guest-v1/cargo-home \
  ../target/release/frontend build-project-disc --project ../editor/projects/cortex-ignition-tech-demo-0.4/project.ron
cd <worktree> && PSOXIDE_GUEST_STAGE_ROOT=/tmp/psoxide-psx-guest-survey \
  PSOXIDE_GUEST_CARGO_HOME=/tmp/psoxide-psx-guest-v1/cargo-home \
  PSOXIDE_GUEST_LINK_MAP=<out>/link.map make build-editor-playtest
```

The second build reproduces the identical exe hash (checked: `1de5e327…`), so
the map matches the disc. The guest output is a PSX-EXE, not an ELF, so
`tools/pc_symbolize.py` (needs `nm`) does not apply; use
`tools/pc_line_attribution.py <pc-line.csv> <link.map>` and, for the RAM
stall log, rename its header to `line_pc,instructions,percent` first.

Attribution run:

```bash
frontend launch --path <cue> --embedded-playtest --steps 1500000000 --press "<as above>" \
  --pc-line-log pcline.csv --pc-line-start-route-tick 4500 \
  --ram-load-stall-line-log ramstall.csv --ram-load-stall-line-start-route-tick 4500 \
  --icache-event-log icache.csv --icache-event-start-route-tick 4500 \
  --cpu-cycle-profile-log cycles.csv --route-log route.csv
```

`icache.csv` is ~21M rows for 2,000 vblanks; summarise it offline (the pair
script used today is in the scratchpad, not the repo).

Callsites: `--pc-sample-callsite-log callsites.csv --pc-sample-instructions 4096`;
symbolise `pc` and `return_address` against the map.

quake-psx: `~/Desktop/repos/quake-psx/dist/quake-psx.cue` (built 2026-08-25),
`--digital-pad --press "2200:cross:240"` (`ACCEPT_TICK` in
`host/quake-build/main.rs:8251`). Gameplay from ~2500. Canonical fps:
`cargo run --release -p quake-build -- e1m1-chain-bench` (see VALIDATION.md).

hl-psx: `~/Desktop/repos/hl-psx/dist/hl-psx.cue` (2026-08-30).
`--press "600:cross:60,1200:cross:60,1800:cross:60,2400:cross:60,3000:cross:60"`
reaches the c0a0 tram ride by ~2400. For any other map use the regression
harness (`host/hl-build regression.rs`, 30 scenarios, needs a
`debug-map-boot` build) rather than menu presses.

HD frame: `PSOXIDE_HW_DUMP_SCALE=4 frontend launch ... --dump-hw x.ppm`
(1280x960). `--dump-draws` gives every GP0 packet of the run; the last frame
starts at the last `0x02` fill.

MCP debug server (`emu/crates/frontend/src/mcp.rs`, feature `mcp`) exposes
screenshot / VRAM dump / RAM read-write / step / pause / load_game, but it
needs the GUI process, which Manny launches himself. The headless flags above
cover everything this survey needed.

## 6.1 Before/after protocol for every performance task (rule from Manny, 2026-09-01)

No perf change lands without a before and an after on the same benchmark,
same build features, same frontend binary. Visual exactness is part of the
gate: a speedup that changes the display or VRAM hash is a visual change
until proven intended.

| Game | Benchmark | Command | Metric | Noise floor | Exactness |
|---|---|---|---|---|---|
| quake-psx | E1M1 chain route (complete level, fixed ticks) | `cargo run --release -p quake-build -- e1m1-chain-bench` (two runs per build, built in) | `full_level_fps_x1000`, `full_level_elapsed_bus_cycles` | 0.122 fps code-layout band (documented in VALIDATION.md / RENDERING.md); reject anything inside it | `vram_fnv1a_64`, `display_fnv1a_64` byte-identical, plus `visual-parity-regress` |
| quake-psx (collision-heavy) | monster route | `e1m1-monster-route-bench` | same | same | same |
| hl-psx | Initial tram ride (c0a0), poll-bound tape, gameplay ticks only | `make psoxide-profile` then `python3 tools/perf/tram_bench.py` and `tools/perf/visual_perf_gate.py --gate` | route fps, p05, median, slow windows, flips (RENDER_PIPELINE.md "Measurement contract": 18.26 fps, p05 12.10, median 19.53, 61/122 slow, 11,468 flips) | run each scenario twice (built into `regress`); the gate's own deadline-miss / skipped-vblank / packet-overflow limits | final display + VRAM hash from the same run; build with `emulator-telemetry` only, never `performance-telemetry` for an A/B |
| Cortex Ignition 0.4 | A route Manny records in the editor (Record button, `<config>/editor/playtest_tapes/<project>.pxtape`), transcribed once to a poll-bound `PXITAPE2` fixture (`--input-tape <pxtape> --input-tape-transcribe <fixture.pxitape.csv>`) and committed under `editor/archive/fixtures/` | `frontend launch --path <cue> --embedded-playtest --input-tape <fixture> --steps 6000000000 --dump-hash --route-log --cpu-cycle-profile-log` (the combat-checkpoint pattern) | bus cycles to tape end, display flips over the tape (fps), `visual_deadline_misses` when a telemetry build links; `--pc-line-log` for attribution | not yet measured: establish it first by building twice with a no-op change and comparing (Quake's 0.122 band came from that); until then treat anything under ~1% as noise | `display_fnv1a_64` / `vram_fnv1a_64` at tape end, `lockstep-visuals` feature for frame-exact A/B, `tools/cortex_30fps_report.py` |

Dependency to verify before the Cortex fixture exists: frame-bound `.pxtape`
replay (and therefore transcription) may need guest frame markers from an
`emulator-telemetry` build, which does not link on 0.4 until P9 lands. If so,
P9 is the first Cortex task, or the tape is recorded on a smaller project
that still links with telemetry. Poll-bound `.pxitape.csv` replay itself works
on the shipping build (combat-checkpoint proves it).

Reporting format for every perf PR: a two-row table (before / after) with the
metric, the noise floor, the hashes, and the link-map symbol check where the
change touches 64-bit code.

## 6.2 A/B protocol for every visual task (rule from Manny, 2026-09-01)

A visual change ships with a before/after pair from the same route and the
same camera, at 4x (`PSOXIDE_HW_DUMP_SCALE=4 --dump-hw`), plus the numbers
that say what changed and that nothing else did. Manny reviews the pair
before anything ships (standing rule for layout/visual changes).

| Game | Fixed viewpoints | Pair | Quantities | Must not move |
|---|---|---|---|---|
| hl-psx | `regress` scenarios (30, poll-bound routes; `world-subdivision` on c2a5, `anomalous-*` on c1a0, tram `endgame-tram-door`) and the tram frame used in this survey | `--dump-hw` at the scenario's frame, 4x, before/after side by side, plus a crop of the affected region | `affine-heatmap` build: per-frame affine error histogram in texels; `seam-census` build: seam pixels by bordering mechanism; `--dump-draws` primitive census (count, mean extent, share > 48 px); packet pressure and eviction telemetry from the replay | route fps and p05 from 6.1 (a visual gain that costs frames is a trade Manny decides, not a default), packet overflows = 0, hashes of scenarios the change should not touch |
| quake-psx | `tools/visual-parity-cameras.json` fixed E1M1 camera (`visual-parity-regress`) | the runner's own hashes plus a 4x `--dump-hw` pair for the eye | submitted command counts, texture-window state, draw-packet overflow, the PS1-side visual probe | `e1m1-chain-bench` inside the 0.122 band; untouched maps' parity hashes |
| Cortex Ignition | the tape from 6.1 at fixed ticks (spawn, plus one tick per reported issue), and `frontend dump-editor-preview` at the matching camera for editor-vs-game comparisons | `--dump-hw` 4x before/after, editor preview alongside when the change is a cook change (V1) | `--dump-draws` UV spans per material group, primitive census, `--dump-vram` page decode when textures are involved (the 2026-09-01 method: decode the 4bpp page with each CLUT, overlay draw groups) | tape hashes of frames the change should not touch; the 6.1 perf numbers |

Reporting format: two images (or four with the editor preview), one table of
the quantities above, and the perf row from 6.1. "It looks better" without
the table is not an A/B.

---

## 7. Open questions for Manny

1. Which are the "couple of rendering issues" in Cortex 0.4? Only the spawn
   wall seam was visible at the spawn. A route or a screenshot per issue
   turns each into a `--dump-draws` reproduction.
2. Is the Cortex-current pin (`99ba6890`, uncommitted) meant to stay separate
   from `930b1201`, or collapse per H-01?
3. quake-psx's SDK: land `codex/quake2-renderer-harmonization` on main, or
   re-hydrate from `930b1201`?
4. For S-05, is a change in the shared tracer acceptable if it is byte-exact
   on Quake's E1M1 route and Cortex's combat checkpoint, or does Quake's fork
   protocol (`--allow-psoxide-drift` refusal) need a repin step first?

## 8. Caveats

- All fps and cycle numbers are emulator-side. The emulator does not model
  GPU draw or DMA time; on silicon a CPU win can be hidden behind fill rate
  (`emulator-matches-console-perf` says the CPU-bound regime matches).
- The quake and hl-psx windows are single views, not routes. Their own
  harnesses produce the canonical numbers.
- The Cortex attribution is `d812d5bd`, one commit behind Manny's 184-file
  working tree. The collision share is structural (the motor and tracer are
  unchanged there), the HUD/UI shares are not.
- My worktree guest build briefly staged into the shared
  `/tmp/psoxide-psx-guest-v1` root (first attempt, before switching to a
  private root). The exe there was rebuilt by Manny's own Play at 19:47, so
  no stale artefact should remain, but if his next Play misbehaves, touch the
  stage sources (`find /tmp/psoxide-psx-guest-v1/stage -name '*.rs' -exec touch {} +`).

## 9. PSoXide action plan (drafted 2026-09-01, branch by branch)

Base every branch on `main` after Manny's `fix/bsp-on-plane-leaf-lookup` work
lands; FF-merge in order; UI/HUD files untouched; repin the games only at the
marked points. Task ids refer to section 5.1.

| Branch | Tasks | Scope | Gate | Size |
|---|---|---|---|---|
| B0 `bench/cortex-fixture` | 6.1, 6.2, I4 | Manny's tape -> poll-bound fixture under `editor/archive/fixtures/cortex-0.4/`; `make cortex-bench` (two replays, cycles/flips/misses/hashes); `make guest-symbol-gate` (no `di3`/`div_rem` in the link map); commit this doc | two replays identical; noise floor from two no-op builds | S-M |
| B1 `build/ram-headroom` | P9 | cook fixed capacities (RAM survey 1..3) | tape hashes unchanged; telemetry build links | M |
| B2 `fix/cook-uv-wrap` | V1 | `brush_compile.rs` extent cap by texel span, 240-texel split, loud failure; wrapped-face log | 6.2 pair editor vs in-game; UV spans; face/packet counts | S |
| B3 `perf/collision-trace` | P1+P2 | provider segment-AABB pre-test; `plane_contact` inline + i32 | combat-checkpoint + door; B0 table; pc-line share; symbol gate | S |
| B4 `perf/i32-kernels` | P5+P7 | `attributed_clip`, `mul_q12`, software AVSZ, warm sites | byte-exact hashes; symbol gate green | S |
| B5 `perf/room-frustum-i32` | P4+P8 | `FrustumPlanes` i32; `visible_bounds_mask` compare; memsets | 120-frame exact capture; B0 table | M |
| B6 `perf/idle-motor-probes` | P3 | reuse probes while position/input unchanged | combat-checkpoint + door; B0 idle segment | M |
| B7 `perf/pose-hoist` | P6 | pose once per part in model submit | hashes; icache pair share | M |
| B8 `visual/spawn-seam` | V4 | after `--dump-draws` names the primitive | 6.2 pair | S |
| B9 `chore/land-quake-sdk` | I1 | FF `e31ea70b` onto main (or drop it) | psx-bsp tests | XS |

Waves: 0 = B9, B0 || B1. 1 = B2 || B3, then repin quake-psx and run its
benches. 2 = B4 (then repin hl-psx, tram gate), B5, B6. 3 = B7, B8, then a
re-attribution run decides between P10 and P11.
Needed from Manny before wave 0: the recorded tape; keep-or-drop `e31ea70b`.

### 9.1 Execution log (2026-09-01 evening)

Bench = `make cortex-bench` on the whole-level tape, `lockstep-visuals` build,
compared to the lockstep baseline (bus cycles 7,170,338,576; vram
`0x4bcf7983f4adcc89`, display `0x1a31c1db8cb7879c`). Bus cycles are the
primary number under lockstep; instruction counts are stall-blind.

| Branch | State | Bus cycles | Hashes | Notes |
|---|---|---:|---|---|
| B9 `chore/land-quake-sdk` | on main (`dd4b0d6e`) | n/a | n/a | cherry-pick of e31ea70b, psx-bsp 154 tests |
| B0 `bench/cortex-fixture` | on main (`b6398a0e`) | baseline | baseline | tape fixture, `cortex-bench`, `guest-symbol-gate`; lockstep made the default after a cadence divergence at tick 978 |
| B1 `build/ram-headroom` | on main (`2e58a7cb`) | +0.12% | identical | .bss -21,344 B, .text -9,628 B, image end -29,536 B; +0.12% is inside the unmeasured layout band (noise probe running) |
| B2 `fix/cook-uv-wrap` | pushed, awaiting Manny's 6.2 review | +6.10% | differ (intended) | 371 surfaces split, +693 faces; tape-end pair: guardrail tiles instead of smearing; needs B1 to link |
| B3 `perf/collision-trace` | on main (`5d2e45ae`) as the plane_contact rewrite only | -0.57% | identical | +0.95% instructions, I-cache refill share 9.22% -> 8.61%. The segment-box pre-test per model hull was measured separately: +0.23% cycles on top (a model hull walk is cheaper than the inverse rotation the test needs) and its standalone variant hung the guest at boot (361 polls, `pc 0x80010248`, not chased), so it was dropped |
| noise probe | first run on top of B1: +0.104% cycles / +0.284% instructions, i.e. the same as B1 itself, so the layout shift cost ~0.02% and B1's +0.28% instructions is real and unexplained (identical output). Second probe on the baseline's exact base: **-0.048% cycles, +0.000% instructions, identical hashes. Noise band is under 0.1% of bus cycles.** | | | |

| B6 `perf/idle-motor-probes` | pushed for review | -0.10% vs main `5d2e45ae` | identical | memoise `player_contents` per (position, height); only idle ticks benefit, the tape is mostly in motion |
| main at `5d2e45ae` (B1 + B3) vs the old baseline | measured | **+0.08%** | identical | B1 alone was +0.12% and B3 alone -0.57%; combined they read +0.08%. B1 removed 9.6 KB of .text, which relaid every function after it. **Large relayouts swing bus cycles by ~0.5% even though a one-function layout probe moved only 0.05%.** Sub-1% cycle deltas are therefore not conclusive from one build; B3's value is the rule compliance and the I-cache refill reduction, not a proven 0.5% |

Consequence for the protocol: for changes expected under ~1%, report retired
instructions and the stall breakdown alongside cycles, and either accept them
on those grounds or bench them under two or three deliberate layouts (the
`layout_probe` no-op at different sizes) before claiming a cycle win. Cycle
deltas above ~1% (B2's +6.1%) stand on one build.

Things learned that change the plan:
- Grid room capacities were already gated on `playtest_pxbsp` for the arenas;
  RAM is now dominated by resident content (persistent asset streamer 659 KB
  of the 796 KB arena) and the 110 KB frame render scratch. Further headroom
  is content policy (clip residency), not capacities.
- The numeric guard (`psoxide-dev runtime-numeric-guard`) is red on main at
  `world_objects_runtime.rs:39` (a `u64` bit mask) and does not scan psx-bsp.
- `attributed_clip`'s Q16 i64 path is a documented, measured exception
  (E1M1 gate: faster and smaller than Q12). B4/B5 must be one change with
  the PXBSP plane math, gated on the 6.2 A/B, not a mechanical edit.
