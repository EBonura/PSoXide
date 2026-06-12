# perf-30fps campaign log

Goal: 30 fps on real hardware. Charter: trust no instrument blindly;
optimise at any level (SDK to engine); rewrites welcome; aim for
conciseness, elegance, simplicity. Most of the engine predates the
silicon-accurate emulator, so architecture decisions are open.

## STATUS (2026-06-11, end of visibility-fix session)

Visibility: FIXED and tape-verified (portal-anchored PVS, all-cells
fallback that always draws; 333 tris vs draw-everything's 341).
Performance with CORRECT visuals: render vblank 1,126k (199%), ~20
fps -- the earlier sub-900k numbers were measured against broken
far-room culling and are void. The remaining ~250k to a clean 30 fps
slot, mapped by instruments this session, in expected-value order:

1. Room surface packets: prebuild at cache-build time (surfaces are
   static; only XY/depth change per frame). Micro-profile: submit
   36k/frame (~356/quad for ~101 quads), vertex gather 15k, lighting
   3.4k, rest loop scaffolding. The classic precompiled-display-list
   technique; biggest single design on the table (room band 328k).

   DESIGN (locked 2026-06-11, implementation in flight):
   - Per-frame variables ONLY: XY x4 (projection), OT slot/depth, and
     fog-blended RGB x4 when the room has FOG_ENABLED. Constants per
     resident room: command word, UV words, tpage/clut, baked base
     RGB (indexed_vertex_lighting_colors already has the
     use_direct_baked_rgb zero-cost path returning
     surface.baked_vertex_rgb).
   - Storage: a static per-slot packet pool OUTSIDE the per-frame
     arena (the OT links into it each frame; in-place patching is
     safe because the present flip drains ch2 before the next render
     touches packets). ~52 B/quad x ~219 surfaces x 6 slots ~= 68 KB
     against the ~196 KB freed by right-sizing.
   - Skeletons fill at room surface-cache build time
     (ROOM_SURFACE_CACHE stage, once per room load); per frame the
     draw loop patches XY from the projected vertices, computes one
     PER-CELL fog factor (cell verts share depth to within the fog
     quantum) instead of 4 per-vertex blends, and pushes the
     prebuilt packet pointer into the OT.
   - Expected: ~250 cyc/quad saved (~26k) + most of the 15k gather +
     lighting -> room band -40k to -60k/frame.
2. Player path: LLM-slot blend restructure + joints flattening +
   faces DIV (designs recorded below; player 318k).
3. Update residual: 99k avg / 310k max UNATTRIBUTED after the sim
   sub-stages (solve 68k, collision 13k, room-track 6k with 83k
   transition spikes). Needs 2-3 more stage markers; suspects are
   anim sampling, props, interactables, camera.
4. Visible-list cache split: the fill is camera-dependent (frustum
   filtering at fill time) so it refills every frame the camera
   moves; caching the camera-independent PVS candidate list per
   (room, anchor) and filtering per frame in cell_select is worth
   ~30k. The prewarm is already portal-anchored (tick B does the
   fill under phase 1's GPU cover).
5. image_props 139k: never opened.

Budget: 564,480 cycles per NTSC vblank; the 30 fps slot is 2 vblanks.
All numbers below are EMULATED cycles (relative guide, not silicon
truth -- the cycle model is Redux-era and never silicon-calibrated;
final verdicts need a hardware fps counter burn).

## Instrument validation (2026-06-11)

- The HWB explosion-probe overlay + capture was compiled into EVERY
  playtest build and cost **107k cycles per render vblank (19%)**.
  Now feature-gated (`psx-engine/vert-debug`,
  `editor-playtest/vert-debug-overlay`), off by default; probe builds
  request it via `EDITOR_PLAYTEST_FEATURES`.
- Guest-frame counting in `launch --embedded-playtest` REQUIRES the
  `emulator-telemetry` feature; a non-telemetry build never reports
  frames and runs to the step cap. Telemetry's own cost is therefore
  unmeasured by A/B; bounded by construction at <2%/frame
  (~46 stage markers x 2 MMIO writes).
- Profile recipe (the runbook's Python chart is gone; use psoxide-dev):
  `EDITOR_PLAYTEST_FEATURES='cd-stream-bench emulator-telemetry' cargo run -p frontend --release -- build-project-disc --project editor/projects/cortex_gameplay_probe/project.ron`
  then `launch --embedded-playtest --hold-forward --guest-frames 1600 --steps 2000000000 --profile-log <csv>`
  then `psoxide-dev vblank-chart --in <csv> --out <html>`.

## Baseline (clean: telemetry on, overlay off, hold-forward corridor)

Render vblanks avg **1,347,960 cycles = 239% of budget**, sim-only 38%,
789/800 deadline misses (~19 fps effective). June 6 baseline was 843k
(150%) -- a **+505k regression** since, decomposed:

| stage | June 6 | now | delta |
|---|---|---|---|
| world_flush | 47k | 357k (63%) | **+310k** |
| room | 64k | 218k (39%) | **+154k** (cell_select 88k, surface_draw 63k, project 52k) |
| player | 322k | 330k (58%) | +8k (padded transforms, as predicted) |
| update | 192k | 177k (31%) | -15k |
| image_props | 121k | 137k (24%) | +16k |
| camera | 43k | 58k (10%) | +15k |
| sky | 45k | 42k | -- |
| ot_wait / ot_submit | -- | ~5k / ~0 | GPU time unmodeled in emulation |

Prime suspect for the regression: the `vis-full-active-chunks` default
(draws every cell of every traversed room -- 77 cells/frame across 5
chunks -- instead of the cooked per-cell PVS; it was the correctness
fix for dropped far-room cells) and its amplification in world_flush.
Streaming is innocent in this corridor: 0 stream misses, ~0 chunk-load
cycles.

## Attack list (ordered)

1. Visibility: fix the cooked per-cell PVS properly and retire the
   draw-everything default. Expected: the single biggest win.
2. world_flush forensics: why 357k to flush packets; then the
   pipelining arc (double-buffered OT, async submit, build N+1 while
   the GPU draws N).
3. Player path / GTE utilisation: ~330k for ~250 verts = ~1.3k
   cycles/vertex of scalar glue around ~8-cycle GTE ops. Batch RTPT,
   GTE-resident loops, cut MTC2/MFC2 ceremony.
4. update (177k), image_props (137k), camera (58k).
5. Hardware truth: add an on-screen frame-time/fps counter to the game
   build and burn once per milestone -- emulated cycles steer, silicon
   decides.

## RAM budget (2026-06-11, linker-map instrument)

Map recipe: add `-Clink-arg=-Map=<file>` to the playtest RUSTFLAGS.
Region = 2M - 64K BIOS - 32K stack = 1,998,848 bytes. Used: .text 438k
+ .data 811k + .bss 717k = 1,966k -> **~32KB headroom**. Top consumers:
SCENE 226k (.data), STREAMED_ROOM_WORDS 213k (7 chunk slots),
PRIMITIVE_PACKETS 186k (single-buffered), UI_IMAGE_CACHE 135k (menu
images held resident through gameplay to skip one re-seek),
**~500k of .data is embedded assets** (all animation clips, atlas, UI
sfx baked into the EXE despite the CD streaming system), FONT_PACK_
SCRATCH 66k, WORLD_COMMANDS 53k. The memory budget is now a measured
quantity; the asset embedding is the biggest structural lever.

## Experiment 1: PVS vs draw-everything (2026-06-11)

Unblocked by right-sizing the PVS pools 1024 -> 192 (corridor peak 77
cells; overflow guards degrade gracefully; final size wants cook-time
worst-room stats) and, probe-project-locally, one fewer streaming slot
(7 -> 6; verified 0 stream misses).

Result: render 1,347,960 -> 1,197,105 (**-151k, 239% -> 212%**).
Cells drawn 76.7 -> 18.7 with **identical tri_primitives (286.2)**:
the brute-force mode's extra cells held zero triangles -- the cost was
pure per-cell bookkeeping (cell_select -78k, room_project -44k,
surface_draw -30k).

**Attribution correction:** world_flush is byte-identical across the
two modes (356,799 vs 356,803) -- it was NEVER the visibility
amplification. It regressed 47k -> 357k on its own since June 6
(1,247 cycles/primitive to flush 286 tris) and is now standalone
suspect #1 with its own forensic trail (whatever touched the flush /
OT path since June 6).

Cost ranking after PVS: world_flush 357k, player 330k, update 181k,
image_props 137k, room 63k, camera 58k, sky 42k (render 212%).

## Experiment 2: world ordering mode (2026-06-11) -- SOLVED

The world_flush regression commit is d09012e9 ("Fix runtime room
material omissions"), which silently flipped the default ordering from
world-order-bucketed to world-order-slot alongside its real fix (the
material table) and the FixedCell -> HybridWalls depth-mode change.
A/B of all four ordering modes on the PVS config, 1600-frame corridor,
render-vblank averages:

| mode | world_flush | render total | deadline misses |
|---|---|---|---|
| slot (old default) | 360k | 1,204k (213%) | 786/792 |
| global | 361k | 1,172k (208%) | -- |
| linked | 103k | 981k (174%) | 644/792 |
| bucketed | 48k | 859k (152%) | **0/792** |

Lessons:
- Slot and global land within 0.1% of each other despite different
  algorithms: the cost IS the per-frame comparison sort. Sorting ~280
  16-byte commands every frame costs ~310k cycles on a no-D-cache
  33 MHz CPU (~100+ cycles per compare-and-move). No per-frame
  comparison sort survives on PS1; the OT is the hardware radix sort.
- linked (exact order via submit-time insertion) shows where exactness
  is spent: +53k lands on the player, whose dense tri cluster shares
  slots (long insert walks). Rooms spread across slots and pay ~0.
- The exact-order premium (122k-313k) buys ordering within ONE OT slot
  quantum, below the depth resolution the OT already imposes. No
  visual difference observed (identical tri counts; correct-looking
  frames; classic PS1 titles never sub-slot-sorted).

Default flipped back to world-order-bucketed (Cargo.toml +
EDITOR_PLAYTEST_HARDWARE_FEATURES). The exact modes stay behind
features. d09012e9's real fixes (material table, HybridWalls depth)
are mode-independent and untouched. Residual risk: co-planar/decal
layering relies on submission order within a slot -- needs an
eyes-on playtest pass and the next milestone burn to sign off.

**bucketed + PVS = 0/792 deadline misses: the corridor runs 30 fps in
emulated cycles.** bucketed + draw-everything (current vis default)
still misses (render ~1,010k + sim ~215k > the 1,129k slot), so the
PVS decision now gates 30 fps.

Note: room_stream_misses=15 in every config, but all 15 land in
vblanks 1-7 (boot warm-up, before the first render vblank at 16).
Steady-state gameplay streams clean with 6 resident slots.

## Attack list (revised again)

1. PVS default decision -- now the 30 fps gate. The probe corridor
   shows pixel-identical frames and identical per-vblank tri counts
   vs draw-everything; the original far-room hole (f69ba419, demo10
   doorway) may have been fixed by that commit's portal-walk re-gate
   rather than by draw-everything. Verify the doorway sightline
   in-editor with PVS, then retire vis-full-active-chunks.
2. Player path / GTE utilisation (315k for ~250 verts) -- now the
   largest band.
3. update (182k), image_props (137k).
4. RAM architecture: stream the ~500k of embedded assets; rethink the
   135k always-resident menu-image cache. VRAM allocation survey.
5. Pipelining arc (double-buffered OT, async submit, build N+1 while
   GPU draws N) -- now that flush is cheap, this is about hiding the
   unmodeled GPU time; needs the hardware fps counter first.
6. Hardware truth: on-screen fps counter burn per milestone.

## Investigation: double buffering + vblank scheduling (2026-06-11)

Code read: app.rs run_scheduled, scheduler.rs, time.rs,
framebuf.rs, the OT submit path, and the playtest statics. Findings,
each verified in source and against the bucketed profile:

1. **The framebuffer is properly double-buffered** (vertical A/B at
   Y=0/Y=240; swap flips GP1 display-start + draw area/offset).
   The OT and the primitive arena are SINGLE-buffered; safe only
   because submit blocks until the DMA walk completes.

2. **present's vblank wait is edge-detect, not phase-aligned**
   (time.rs: return as soon as vblank_count != last_present). Any
   render frame slower than one vblank has already crossed an IRQ, so
   the wait returns in ~0 and fb.swap() executes MID-SCANOUT. At
   today's steady 859k build the flip lands ~52% down the visible
   frame, every frame. GP1(05h) takes effect immediately on silicon:
   that is a stable horizontal tear line on hardware. The emulator
   samples display-start once per frame, so emulation CANNOT show this
   artifact (and never has). Profile confirms: present stage = 130
   cycles average on render vblanks.

3. **Steady-state slot accounting** (bucketed + PVS): render row 859k
   + sim row 264k = 1,123k of the 1,128,960-cycle 30 fps slot. 0.5%
   headroom in emulated cycles, and the budget contains ZERO GPU draw
   time: on hardware the kick-then-wait submit serializes the whole
   GPU draw into that 6k margin. Emulated 30 fps therefore does NOT
   imply hardware 30 fps; the GPU draw is fully exposed.

4. **The scheduler itself is sound** (vblank-counter clock, sim
   catch-up, no-cap default = drop visuals not gameplay time). The
   30 fps cadence is work-driven, not phase-locked: it emerges only
   because work happens to fill ~2 vblanks.

5. **Pool sizes**: MAX_TEXTURED_TRIS = 3328 sizes both the primitive
   arena (186,368 B) and WORLD_COMMANDS (53,248 B) for 11x the
   observed ~290-tri peak. Right-sizing to 1024 frees ~166 KB -- more
   than enough to double-buffer BOTH (2x 57 KB arena + 2x 16 KB
   commands + 2x 8 KB OT) and still come out ~58 KB ahead of today.

6. **The post-submit overlay set is the pipelining blocker**: the
   atmosphere particles, collision debug, lock indicator and HUD draw
   with immediate GP0 writes AFTER the blocking submit (composited on
   top of the 3D scene). CPU GP0 writes during an active ch2
   linked-list DMA corrupt the command stream on hardware, so any
   async-submit design first needs these to become OT packets in the
   front-most slot (a convention the OT already has for UI).

### Proposed arc

Phase 1 -- boundary flip + async kick, no extra RAM:
- scene.render ends with submit_async (kick only).
- The flip moves out of the render action: at the NEXT vblank IRQ
  edge, draw_sync + swap + clear (a pending-flip flag in
  run_scheduled). The row-B fixed update runs between kick and flip,
  giving the GPU ~271k cycles (~8 ms) of free cover, and the flip is
  tear-free by construction.
- Prereq: overlays/HUD as OT packets (finding 6).
- Expected hardware effect: hides up to ~8 ms of GPU draw; ~290
  textured tris plus sky plausibly fit (3-9 ms by PS1 fill-rate
  folklore); the fps counter burn decides.

Phase 2 -- full pipeline, only if the counter says the window is
short: double-buffer OT + arena (net RAM win after right-sizing,
finding 5), kick frame N then immediately build N+1; GPU gets the
whole 2-vblank slot; +1 visual frame (33 ms) of latency.

Emulator follow-up (north star: match hardware): the frontend cannot
display mid-scanout GP1(05h) flips, so silicon would tear where
emulation looks clean. Add a cheap "display-start changed mid-scanout"
hazard counter to the GPU backend + debug sidebar so this class of
divergence becomes visible without a burn.

## Phase 1 SHIPPED: async kick + tear-free boundary flip (2026-06-11)

Phase 2 (full double-buffer pipeline) rejected: +33 ms input-to-photon
is too heavy for an action game. If silicon says phase 1's window is
short, the fallback is the latency-free two-part kick (far geometry
early, near + player late), not phase 2.

What changed:
- Scene::render_overlay: new hook for the 2D layer (HUD, prompts,
  atmosphere, debug). The app runner calls it at flip time, after
  draining the walker, so the immediate-GP0 font/UI stack stays as-is
  -- no packet conversion needed.
- The playtest render ends with submit_async + detach; the engine flip
  (present_pending in app.rs) does OT drain -> overlay -> wait for a
  true vblank IRQ edge -> swap. Under overload (next visual already
  due) it flips immediately instead of holding for the edge.
- GameApp routes pause-UI-over-gameplay and transition fades through
  render_overlay; UI-only scenes keep drawing in render.
- Hazard guards: VRAM uploads (psx-vram copy_to_vram_header) and
  hardware-boot-visual checkpoints drain channel 2 before touching
  GP0, so a fixed update streaming textures mid-walk cannot corrupt
  the chain.
- World-anchored overlay content uses a camera snapshotted at render
  time (the flip runs after the next fixed update has already moved
  the camera).

Verification (1600-frame corridor, vs pre-phase-1 bucketed+PVS):
0/792 deadline misses in both; slot total unchanged (1,126k of
1,129k); render row sheds the overlay cost (render stage 676k ->
636k), the sim row gains overlay 36k + present 76k (the tear-free
edge wait, now measured instead of implicit); final frame differs by
3/76,800 pixels (atmosphere particle time phase is one sim tick
fresher at the flip). Engine suite 208/208.

On hardware this buys: GPU draw overlapped by the tick-B update plus
the edge wait (~270k cycles ≈ 8 ms of cover), and the swap always
lands in the blanking interval (the standing mid-screen tear line is
gone). The flip-side ot_wait band is the uncovered GPU remainder --
the single number the fps-counter burn must read.

## Benchmark of record: user gameplay tape (2026-06-11)

The user recorded a 2,157-frame real-gameplay tape in the editor
(cortex_ignition_v1.pxtape in the playtest_tapes config dir). It
replays headless into genuine gameplay (final-frame check passed), so
it replaces the synthetic hold-forward corridor as the campaign's
benchmark. Replay recipe: build-project-disc for cortex_ignition_v1
with the candidate features, then launch --embedded-playtest
--input-tape <tape> --steps 2000000000 --profile-log <csv>.

First measurement (PVS + bucketed + phase 1): render vblank 1,041k
avg (184%), 808/921 misses (~20 fps) -- REAL scenes are far heavier
than the corridor: room 216k avg / 765k max (3+ visible rooms),
update 209k avg / 600k max spikes, image_props 138k avg / 568k max
spikes, player 318k, ~340 tris avg / 511 max. The corridor's 0-miss
result was a gentle room ring; the slot must absorb these scenes.
Revised cost ranking on real gameplay: player 318k, room 216k,
update 209k (spiky), image_props 139k (spiky), world_flush 50k.

Also confirmed live by the user: the PVS doorway hole (the room
beyond the arch door culled out) -- the cooked per-cell PVS really
does drop portal-visible far rooms. The PVS default stays blocked on
the cook-side fix (task list); these tape numbers were taken WITH
PVS, so the deficit above is what remains even once PVS ships.

## The arch hole SOLVED: it was the runtime anchor, not the cook (2026-06-11)

The cook-side BFS-cap theory was falsified (cap never triggered for
this project; recook byte-identical). The real bug: the per-room
visibility anchor is the PLAYER's position translated into each
room's local frame, CLAMPED onto the grid edge for rooms the player
is not inside. A far room seen through a portal used an arbitrary
boundary cell's wall-gated PVS (tiny or empty -> room culled
wholesale), and the clamped cell changed with every player step,
thrashing the visible-cell cache with constant PVS refills.

Fix: an outside anchor returns None and the existing full-room
fallback draws the room. Benchmark tape (PVS config): room 220k ->
108k, render vblank 954k -> 849k (150%), misses 78% -> 34% (~26 fps
real gameplay, from ~19 at session start). The broken anchor was
BOTH the visual hole and a hidden cache-thrash tax.

Gate before flipping the visibility default: eyes on the arch
sightline in the editor with the PVS build.

## Spike forensics: the hitches are room transitions (2026-06-11)

On the benchmark tape (anchor-fixed build), the 62 update spikes
(avg 716k vs 161k normal) correlate with room_visible_list at 16.2x
(203k vs 12.5k) plus elevated portal_visibility and room bands, and
cluster in ~20-tick bursts at room boundaries. A transition tick is
double-billed: update does the streaming apply + collision rebind
while the same slot's render cold-rebuilds the visible-cell lists for
every newly activated room. Remedies for the next round: amortize
the transition work across ticks (one room's visible-list prewarm per
tick instead of all at once) and stagger the streaming apply.

## Pool right-sizing: RAM win is ALSO a cycle win (2026-06-11)

MAX_TEXTURED_TRIS 3328 -> 1024 (tape peak 567, avg ~332; 2x headroom
kept). Frees ~166 KB of RAM (packet arena + world commands) AND cuts
the benchmark tape's render vblank by **-87k cycles (1,041k -> 954k,
misses 88% -> 78%, +8% visual frames delivered)**: per-frame pool
maintenance scales with CAPACITY, not usage, so oversized pools cost
cycles every frame, hidden across stages. "Question everything
cached" validated with a number; the remaining oversized pools
(UI_IMAGE_CACHE 135k, FONT_PACK_SCRATCH 66k, MAX_CACHED_ROOM_VERTICES
4096) deserve the same treatment.

Also closed: the NCLIP-backface idea from the faces task is dead on
arrival -- scene.rs already documents (silicon-measured) that the
NCLIP -> MAC0 back-to-back read returns STALE data on real hardware
and needs ~8 NOPs, which eats the win; the CPU cross stays. The faces
task reduces to the per-face slot_depth DIV.

## Diagnosis: player path / GTE starvation (2026-06-11)

Phase-1 profile, per render vblank: player 315k = joints 49k +
project 105k + faces 146k, for 252 vertices / 496 faces / 20 parts.
That is 415 cyc/vertex to project and 294 cyc/face to cull+pack,
while the GTE itself is ~0.3% of the frame. Code read
(world_pass_model.rs + psx-gte scene.rs):

- The GTE call is NOT the problem: project_triangle_mips is tight asm
  (6 MTC2 + RTPT + reads, ~25 cyc/vertex amortized). ~94% of the
  project stage is the Rust around it: per-triple `[vertex; 3]` batch
  copies, a per-vertex CPU-blend re-check even for unblended models,
  projected_from_gte unpack, 2 range-verdict checks and a 4-compare
  min/max bounds update per vertex, all on a no-D-cache CPU.
- The verdicts (in_front, inside_hw_bounds) and extent bounds feed
  the packed-faces fast-path selector, so they cannot be dropped --
  but the GTE FLAG register already records screen/Z saturation per
  op: one MFC2 of FLAG per triple can replace 6 scalar range checks,
  and the extent test can run on the GTE-side SXY values.
- The asm reads results immediately after RTPT, so the CPU eats the
  full GTE op latency (~23 cyc) every triple. Software-pipelining
  (kick triple N, do triple N-1's bookkeeping during the op, read N
  afterwards) hides it entirely under work we already do.
- joints 49k for ~25 joints (~2k each) despite GTE-compose paths
  existing (MODEL_GTE_JOINT_* flags): the cost is the per-joint
  Option-chain wrappers and fallbacks around the math.
- faces 294/face: backface cross on CPU (2 MULTs ~12 cyc each +
  stalls) where GTE NCLIP does it in 8 cycles from already-packed
  SXYs; plus per-face packet field writes.

Attack order (verify with the corridor profile after each):
1. Project loop rewrite: triple iteration without batch copies,
   blend check hoisted out of the unblended path, FLAG-based
   verdicts, software-pipelined RTPT. Target 415 -> ~120 cyc/vertex
   (project 105k -> ~30k).
2. Faces: NCLIP for backface, slim the packed writer. Target 294 ->
   ~180 cyc/face (faces 146k -> ~90k).
3. Joints: flatten the wrapper chains around the GTE compose.
   Target 49k -> ~25k.
Combined target: player 315k -> ~145k; render vblank ~860k -> ~690k
(122%), turning the 30 fps slot from razor-thin into comfortable.

### Correction from the first rewrite (measured)

The triple-loop rewrite (run-splitting, no staging copies, pipelined
rtpt_kick/read, tautological hw-bounds checks dropped) is bit-exact
(0/76,800 pixel diff) but bought only ~450 cycles: **64% of the
player's vertices (161/252) take the CPU-BLEND path**, which the
triple loop never touches. At ~590 cyc/vertex the blended path IS the
project stage (~95k of 104k).

Per blended vertex today: MVMVA with the primary joint, full CTC2
matrix+translation swap to the secondary, second MVMVA, swap to the
projection setup, RTPS, swap BACK to the primary -- ~3 full GTE
matrix loads per vertex, ~483 per frame. The fix is batching by
matrix: pass 1 transforms all of a part's blended vertices while the
primary is loaded; pass 2 groups by secondary joint (one load per
distinct joint); pass 3 lerps and projects the whole group under one
projection setup, then restores the primary once per part. Matrix
loads drop from ~3 per vertex to ~3 per part. Expected: project
105k -> ~35k. This replaces item 1's remaining work.

### Batching NEGATIVE result (built, measured, reverted)

The per-part batch (stage view_a in a scratch array, per-group
secondary loads, one projection setup) was bit-exact (0/76,800 pixel
diff) and made project WORSE: 104k -> 134k (+30k, 413 -> 533
cyc/vertex). Two lessons, both PS1-shaped:
- On a no-D-cache CPU, scratch-array round-trips (store view_a,
  reload for lerp, store blended, reload to project, flags, index
  re-derefs) cost MORE than the GTE matrix reloads they remove.
  CTC2 writes are cheap; RAM traffic is not. Keep hot pipelines
  register-resident.
- The result falsifies the matrix-load cost model itself: if ~3
  loads/vertex were the dominant ~120 cyc, removing two of them
  would have won despite the scratch. They are cheaper than modeled.
  The ~590 cyc/blended-vertex therefore lives elsewhere (the two
  MVMVA wrappers' packing/unpacking, the 6-multiply CPU lerp, the
  near-z/i16 guards, call overhead).
Next probe before any further rewrite: temporary sub-stage markers
inside the blended path (transforms vs lerp vs projection) so the
next attempt aims at a measured target, not a model.

### Blended-path decomposition (measured, throwaway stub builds)

Per blended vertex (161/frame), of ~590 total: projection segment
(identity load + zero TR + RTPS wrapper + guards) = 264; secondary
segment (matrix load + MVMVA + 6-multiply lerp) = 175; remainder
(primary MVMVA + call glue + the 91 unblended verts' runs) = ~33k
stage-wide. Cheap fixes measured small: scheduled RTPS wrapper in
project_gte_view_vertex bought only -1.9k (committed; bit-exact);
load_rotation is already lean (5 packed CTC2s, no padding). The
per-segment costs are spread across many ~5-cycle COP2/RAM touches,
not one hot spot.

Remaining levers, in order of expected value (next session):
1. Secondary joint into the GTE LLM slot: MVMVA can take LLM as its
   matrix, so the part's primary stays in R untouched (no per-vertex
   R thrash, no restore), the secondary reloads only when joint1
   changes between consecutive blended vertices, and only the
   projection still needs R=identity. Register-resident, no scratch.
2. GTE GPF/GPL for the lerp (vector x IR0 interpolate-accumulate)
   replacing 6 CPU multiplies, keeping work on the GTE.
3. Content lever: 64% blended vertices is very high for PS1 skinning
   (classic titles hard-skinned with overlapping parts). A cook-time
   blend-weight threshold or seam-only blending would shrink the
   expensive population directly -- an authoring decision, not an
   engine one.

Also found while scoping faces (#44): DepthBand::slot_depth does a
real 32-bit division per submitted face (offset*band_slots/span,
~36+ cycles); span is constant per draw call, so a per-call
reciprocal (MULT high-part, native on MIPS) removes a per-face DIV.

## Round results (2026-06-11, marathon session)

- Prebuilt room-quad pool + single-packet risky whole-quads SHIPPED
  (cd22fb12): room 328k -> 288k (-40k), -52 packets/frame. The win
  split: most from the quad upgrade (one GP0(3Ch) packet where two
  triangle leaves were submitted; per-surface averaged depth replaces
  the leaves' near-identical keys), the prebuilt pool removes the
  arena traffic for the quad path. Depth-key change awaits the user's
  end-of-campaign visual pass.
- slot_depth DIV hoist: measured NEGATIVE (+3.2k) and reverted. The
  batch-hoisted reciprocal map's RAM field loads + fixup cost more
  than the per-face 32-bit DIV in this timing model. Faces task
  closed (NCLIP half was already dead by the silicon stale-MAC0
  evidence). Lesson, twice confirmed now: per-primitive wins must
  remove MEMORY traffic, not ALU ops.
- Benchmark position with correct visuals: render vblank ~1,108k
  (196%). Remaining program: update residual markers (99k
  unattributed), player LLM-slot (#43), joints (#45), image_props,
  visible-list camera split.

## CORRECTION: room-quad round REVERTED (user-found visual regression)

The prebuilt pool + quad upgrade (cd22fb12) is reverted: the user saw
broken rendering in a plain `make run`. Root cause of the worst part:
the pool's skeleton fill was lazy PER SURFACE while the fill flag was
per ROOM -- a surface screen-culled or rejected on the frame its room
claimed a pool slot never got its skeleton written, and every later
frame patched and drew a ZEROED packet into the OT (invalid GP0
opcode word = corrupted command stream). The quad-depth upgrade also
changed draw order without a visual gate.

Verification failure, recorded so it cannot repeat: the change was
gated on cycle counts and ONE end-of-tape frame that did not exercise
the broken surfaces. New protocol for ANY change that touches packet
contents, draw order, or culling:
1. Frame dumps at MULTIPLE tape positions (--guest-frames N for
   several N), compared against the same positions on the previous
   build; any unexplained pixel diff blocks the change.
2. The DEFAULT build (`make run` features) must be one of the tested
   shapes, not only the PVS profiling shape.
3. Cycle wins are not evidence of correctness -- a change that draws
   less is faster and wrong.
If the prebuild returns, the fill must write EVERY surface's skeleton
at claim time (a dedicated fill pass over the room's surface slice),
not lazily from the draw path.

## New benchmark tape (2026-06-11 22:01, archived + committed)

The original tape was lost to an accidental overwrite; the
replacement (benchmarks/cortex_ignition_v1-bench-2026-06-11.pxtape,
also backed up beside the live tape) is a heavier and better route:
room transitions plus the arch-door sightline. Archival is step zero
of the protocol now. Old CSV numbers remain internally comparable but
all gates run against this tape.

Baselines on the new tape:
- DEFAULT build (make run shape): render 1,331k (236%), room 599k
  (1,115k max), tris 422, misses 96% (~17 fps). Draw-everything pays
  brutally on this route's multi-room sightlines.
- PVS shape (anchor-fixed): render 1,265k, room 477k, tris 371,
  misses 96%. Frame dumps at 400/700/1000/full are all CORRECT,
  including far-room geometry visible through a doorway at frame
  1000 (the arch-class case). -122k vs the default on this route.

Protocol note: cross-build pixel comparison is only valid at 0-miss
determinism (cadence diverges under overload), so per-build
multi-frame sanity + the user's eyes gate cross-build changes like a
default flip.

## Update band decomposed (2026-06-12, UPDATE_ACTOR/WINDOW stages)

New stages 46/47 + sim CSV columns split update (189k avg / 434k max
on the tape) into: camera 50k/tick (!), sim_solve 63k (173k max),
collision gather 13k, actor block 3k (innocent), window refresh 11k
(47k max), residency/track ~12k -- and a 36k-avg residual whose 209
spike ticks are the PREWARM's PVS fills (room_visible_list 184k on
those ticks): the visible-list cache key is camera-dependent, so
rotation refills every drawn room's set each tick. Two named targets:
1. update_follow_camera 50k EVERY tick (100k per 30fps slot): does
   its own collect_collision_rooms (margin = camera distance) plus a
   camera collision solve per tick. Read the solve, then either
   reuse/cached gather or solve-on-move-only.
2. Visible-list camera split (existing item, now quantified): 33k avg
   / 184k spikes on sim rows. The candidate list from a static anchor
   is camera-independent; cache it per (room, anchor) and do the
   cheap frustum filter per frame in cell_select.

## Camera-independent visible-cell fills shipped (2026-06-12, d840595c)

vis-anchor-pvs-candidates: the per-(room, anchor) candidate list is
camera-independent (stored depth-0, global-range filter only); the
per-frame camera work moves to the cheap accept test in cell_select.
Tape: room_visible_list 36,515 avg / 258,517 max -> 4,873 / 75,734.
cell_select takes the other side of the trade (+33k, ~2.6k per
candidate accepted -- a future lever). Net render 1,131k -> 916k avg
on the re-baselined script, misses 141/808. The rotation-refill
hitch class is dead. Corridor gate pixel-identical; dumps clean;
restored to default + hardware features.

## image_props decomposed, debris cache shipped (2026-06-12)

Instrumented run (PSXO_PROFILE_BOX_PROPS=1, sub-stage CSV columns):
image_props 117,683 avg / 294,570 max = box_props 15,919 +
box_prop_debris 72,835 (max 264,743!) + box_prop_shards 4,135 +
image_cards 24,535. Debris is 62% of the band.

Root cause: every broken prop re-derives break-time-static data per
chip per frame -- bilinear base colors (box_prop_face_color_at, 36
multiplies/chip), quad corner lerps, UV derivation. Only projection,
fog, and submit are legitimately per-frame.

Fix: 16-slot round-robin cache keyed by prop index (rendering.rs).
On first sight of a broken prop all 12 chips are filled eagerly
(quad, uvs, base colors, material); the draw path consumes cached
data and does only opts + project + fog + submit per frame. Eager
fill avoids any partial-validity hazard; 16 slots covers
MAX_BOX_PROP_STATE eviction in practice (re-fill is one-time cost).

Tape vs pre-change reference (same script, same tape):
- image_props 106,432 avg / 299,639 max -> 81,852 / 237,828 (-24.6k)
- render 915,674 avg -> 896,593; misses 141/808 -> 118/826
Gates: corridor pixel-IDENTICAL (0-skew determinism held), dumps at
400/700/1000/full clean and scene-matched to reference, engine suite
249 green.

## cell_select attributed + frustum hoist (2026-06-12)

New gated sub-stages (psx-engine feature cell-select-profile; CSV
columns cell_lookup/cell_depth/cell_collect) split room_cell_select
(100,232 avg / 157k p99, ~133 cells considered per frame, ~754
cycles/cell): lookup 3.9k + depth/cull 42.7k + collect 34.6k + ~23k
loop/sort remainder. Volume is NOT the problem (PVS candidate lists
total only ~65-79 cells); per-cell constant cost is.

Shipped (behavior-preserving): CellFrustum hoists the clamped
near/far/focal/screen constants out of the per-cell loop and replaces
six saturating 32-bit products per test with exact widening 32x32->64
products (single MULT each); CachedRoomCell now passes by reference in
the select/collect loops. Cell depth still computed per frame on the
GTE -- it feeds DepthPolicy::Fixed OT placement under HybridWalls, so
staleness is not an option.

Tape vs debris-cache baseline: cell_select 100.2k -> 95.7k avg,
cell_depth p99 88k -> 56k (the all-cells fallback scan got the big
cut), render avg 896.6k -> 858.0k (part denominator effect: 826 ->
858 renders on the same tape as skips fell), misses 118 -> 113.
Gates: corridor pixel-IDENTICAL, dumps 400/700/1000/1400/1700 clean
(final-frame close-up is the camera against the slat wall at tape
end, legit geometry), engine suite 249 green.

Also exported (host CSV only): room_cells_considered/culled/
range_culled and the room-surface-profile micro-profile counters
(room_surf_*, room_submit_*) for the next decomposition target:
room_surface_draw 173k avg / ~1.3k per surface considered.

Parked note: per-frame accept set is ~72% of candidates (37/133
culled); collect floor is the ready-flag dedup RAM traffic; further
cell_select cuts need a pipelined GTE center-transform loop (fill the
MTC2 settle gap with the previous cell's compare) -- do this only if
the band stays hot after the bigger rows shrink.

## Joints band: two negative results, task closed (2026-06-12)

Volume recalibration first: textured_model_parts avg 18 (the player
is the only animated model on the tape; model_instance_draws 0), so
textured_model_joints 48k avg = ~2,700 cycles PER JOINT. The GTE
compose itself (4 scheduled MVMVAs + loads) is ~250 of that; the band
is dominated by animation pose DECODE (2 endpoint decodes x ~24
bounds-checked i16 reads + a 12-lerp blend per joint per frame), not
the compose wrappers. The "flatten wrapper chains" premise was wrong.

Negative result 1, per-model RT/TR load hoist: moving the invariant
load_rotation(view_instance)+load_translation(0) out of the per-joint
compose measured FLAT (47.6k -> 48.9k, noise; only ~18 joints x ~25
cycles existed to win) and FAILED the corridor pixel gate by 3 pixels:
the emulator's CTC2 commit-delay model makes load-to-use distance
observable, so the hoist is not bit-neutral in the exact joint-compose
path that exploded on silicon (HWB-011). Flat win + hazard-path timing
change = rejected and reverted.

Negative result 2, endpoint-pose cache (decode endpoints on integer-
frame change, re-blend alpha per frame): measured NEGATIVE. The
player animation's endpoint frame pair advances on ~75% of rendered
frames, so the cache refilled almost every frame and added overhead:
joints distribution went bimodal 34k/58.6k (unblended/blended) to
0/40k/58.8k/73.2k with a new worse refill mode. Reverted, including
the psx-asset accessors.

Conclusion: the joints band is content-shaped. It shrinks when
Alberto's rigid-part robot models land (single-bone parts, no blend)
or if the animation sample rate drops (visual change, user gate).
No further engine-side lever here. Task #45 closed.

## Sky cyclorama packet cache shipped + gate refinement (2026-06-12)

Sky is a pure function of camera ROTATION (the dome is camera-centred;
translation is ignored), so on every non-turning frame all ~96 sky
packets are bit-identical to the previous frame. They now live in a
rotation-keyed static cache (exact key: sin/cos yaw+pitch raws + sky
record fields) and are relinked into the OT background slot per frame;
the grid trig + ~117 RTPS + packet build only run on key change. Same
DMA-drain invariant as the prebuilt room-quad pool.

Tape: sky 38,837 -> 23,268 avg (p50 unchanged ~43k: the follow camera
rotates on about half this route's frames; straights hit). Corridor
probe: sky ~0.3k, and the run went 10 miss rows -> 0.

GATE REFINEMENT (and a correction). The corridor gate compared FINAL
frames; that is only valid when both builds share miss cadence. The
sky and earlier joint-hoist builds eliminated the corridor's 10 miss
rows, so their final frames are legitimately fresher; final-frame
compare flags that as a "failure". The gate is now: pixel-compare a
PRE-DIVERGENCE frame (e.g. 300, before the reference's first miss at
row 309) plus the scene-matched checkpoint dumps. Correction to the
joints entry above: the "not bit-neutral in the CTC2 settle model"
claim was WRONG -- that 3-pixel diff reproduces with the joint hoist
absent and is unrelated to it (the hoist stays out on flat-cycles
grounds alone).

NEW FINDING, filed separately: a binary-layout-sensitive 3-pixel LSB
instability. Two unrelated builds (joint hoist; sky cache) produce
byte-identical frames that differ from the committed tree's frames by
exactly 3 fixed-position pixels ((216,30),(277,74),(240,226)), +-1
LSB pre-dither on dark world surfaces, at frame 300 and 1600 alike;
same tree cooks deterministically. Likely a layout-dependent guest
read (OOB LUT or uninitialized static) feeding lighting by one LSB.
Invisible in practice; tracked as its own hunt.

Sky-cache gates: frame-300 world identical except those 3 pre-existing
pixels, dumps 400/700/1000 clean with correct sky, engine suite 249
green.

## Fallback rooms get the lateral cell cull (2026-06-12) -- the big one

room_vis_fallback_draws (new CSV column) showed 3-4 of ~5 drawn rooms
per frame have NO portal anchor (active-but-not-portal-visible) and
took the all-cells fallback, which culled cells laterally ONLY for the
root room; neighbour fallback rooms drew EVERY populated cell with no
cell-level culling at all. Flipped cull_cells_laterally to true for
all fallback rooms: the sphere radius+margin test is the same
conservative bound the root room already trusts, so rejected cells are
off-screen and pixels are unchanged; only their projection + surface
walk is skipped.

Tape: render 844,966 -> 634,883 avg (-25%), room 350k -> 217k,
cells drawn 89 -> 37 avg (163 -> 92 p99), surfaces 135 -> 74,
misses 113/868 -> 54/1106 (4.9%), presented frames +27%.

Gates (max strictness -- this is the regression class the protocol
was born from): corridor frame-300 pre-divergence compare shows ONLY
the known 3-pixel layout-LSB signature; ten tape dumps
(200/400/550/700/850/1000/1150/1300/1500/1700) eyeballed clean,
including the arch-class far sightlines at 1000/1150/1300 with far
rooms complete through doorways; engine suite 249 green. User make
run remains the final gate.

## Camera collision-solve throttle (2026-06-12, USER FEEL-GATE PENDING)

The spring-arm sweep (ray march: up to 8 samples x 4 rooms x 9-cell
neighborhoods with per-wall intersection math) is ~40k of the camera's
~44k per-tick cost. New ThirdPersonCameraConfig::collision_solve_interval
runs the sweep every Nth tick and reuses the previous solve in between;
distance easing, pull-in snapping, and yaw/focus lag still run every
tick. Manual orbit input, lock-on, and recenter ALWAYS solve fresh so
the throttle never fights deliberate camera moves. Playtest sets 2 via
runtime_config::CAMERA_COLLISION_SOLVE_INTERVAL (set 1 to revert);
engine default stays 1.

Tape: camera 43.3k -> 35.7k avg per tick (the tape's manual-orbit and
lock-on stretches force fresh solves; plain walking gets the full
halving), update 156k -> 147k, misses 54/1106 -> 50/1199.

Gates: new unit test proves throttled == per-tick exactly in a static
scene; corridor frame-300 bit-identical camera path (clear-path solves
are constant, so hold-forward is unaffected); tape dumps 400/700/1000
clean; engine suite 250 green. The real gate is the user's hands:
worst-case collision reaction latency doubles to 33ms near walls.

## Instruction-cut pass on the cell scan + collect loops (2026-06-12)

The cost model discovery (cpu/timing.rs: flat BIAS=2 cycles per
instruction, Redux parity, no load or COP2 penalties) reframes every
hot-loop floor as an instruction-count problem. First pass over the
cell loops, all behavior-preserving:
- world_vertex_gte_input: one combined biased-range test (OR the three
  v+0x8000 values, compare once) replaces three try_from/Option
  chains; inlined-always. Also feeds the room projection funnel.
- All-cells scan: the per-iteration cell_index > u16::MAX check is
  hoisted into a slice cap before the loop.
- Accept writes in both scan loops are get_unchecked (arrays validated
  >= scan length at entry, one push per scanned element).
- Collect: ready/indices scratch pinned to exactly the room's vertex
  count at entry, making the ready-flag lookup the single
  data-dependent bound; the write-cursor capacity check is dropped
  (distinct pushes <= ready.len() == indices.len()); index loops are
  slice iteration.

Tape: render 583.3k -> 564.8k avg, p99 1,160k -> 1,127k (at the
2-vblank budget line for the first time), room 194.5k -> 184.0k,
room_project 24.0k -> 20.9k, cell_select 56.7k -> 54.3k, misses
50/1199 -> 45/1235 (3.6%).

Gates: corridor frame-300 shows only the known 3-pixel layout-LSB
signature; tape dumps 400/700/1000/1270/1300/1330 scene-coherent (the
dark 1300 right half is the camera swinging past the player's
shoulder, confirmed by the adjacent frames); engine suite 250 green.

Remaining instruction slack in these loops is the iterator/GTE-call
shape itself (~200 instr/cell); next bites there need hand-shaped
loops and are second-order to the player band (content) now.

## 3-pixel LSB instability SOLVED: overlay clock anchoring (2026-06-12)

Root cause found, and it is NOT a stray read. The three differing
pixels are 1px atmosphere overlay particles whose flicker phase is a
function of the raw `ctx.sim_tick` VALUE. Two anchoring leaks made
that value build-dependent:
1. `render_overlay` runs at the presentation flip and sampled the
   tick CURRENT AT FLIP TIME, which shifts with deadline-miss cadence
   (the overlay camera was already snapshotted in render() for exactly
   this reason; the tick was not).
2. The engine clock origin is set at app init, BEFORE CD loading, so
   raw tick values carry the build- and disc-dependent loading
   duration. Gameplay logic is immune (same-clock comparisons cancel
   the offset), but every value-based animation phase inherited it.

Diagnosis chain: the 3 pixels flicker over TIME within one build
(frame 290/310/330 in one state, 300 in the other), with identical
values at frame 300 and 1600 -- screen-anchored, blend-stable,
time-flickering = the 1px Average-blended atmosphere particles, whose
drift counters tick every 4-32 vblanks off the tick value.

Fix: (a) render() snapshots `overlay_sim_tick` beside
`overlay_camera`; (b) a `gameplay_epoch` captured at the first
gameplay update (re-anchored per gameplay entry) is subtracted for
every value-based animation phase: atmosphere, lock-on indicator, HUD
pulse, particle emitters, ambient model instances
(`phase_at_tick_q12`). Player/actor animation uses same-clock
relative ticks and is untouched.

Verification: timing-perturbed A/B (default vs cell-select-profile
build, same source) is pixel-IDENTICAL at frame 300; tape dumps
clean; suite 250 green. GATE UPGRADE: the corridor pixel gate is now
STRICT identity -- the 3-pixel allowance is retired. New reference
artifact: benchmarks/corridor-frame300-reference.ppm (regenerate by
dumping --guest-frames 300 on the cortex_gameplay_probe disc whenever
an intentional visual change lands).

## Burn build prepared: fps overlay + menu boot restored (2026-06-12)

fps-overlay feature (default OFF, feature-off build verified
bit-identical on the strict corridor gate): presented-fps + worst
inter-frame gap over a rolling 1s gameplay-tick window, drawn
top-right via the HUD font in render_overlay ("30 W2" = 30 fps,
worst gap 2 vblanks). Burn builds enable it so silicon framerate is
readable from a console photo.

FOUND DURING PREBURN: commit ef2cd557 ("streaming slots 7 -> 6")
silently carried `boot: SceneState((5)) -> ((4))` -- the game has
been booting STRAIGHT INTO GAMEPLAY since 2026-06-11 18:07, skipping
the Bonnie Studios splash and the main menu. That is why the CD-DA
preburn probe heard silence (no menu = no menu music; the cdda code
path is fine and plays once the menu exists). Boot restored to the
splash for the burn.

PROFILING GOTCHA (important): every benchmark-tape replay in this
campaign ran against boot-into-gameplay discs (boot=4). With the
menu boot restored, replaying the benchmark tape against the MAIN
project disc desyncs (its inputs assume gameplay from frame 0). For
future tape profiling either temporarily set boot back to
SceneState((4)) or re-record the tape on the menu flow. The corridor
probe project is unaffected (separate project, still
boot-into-gameplay).

Preburn: all seven local checks PASS (struct, disc-reads, internal
pad-pulse flow splash->menu->gameplay with the overlay visible,
cdda-audio 83% nonzero peak 5.2k, bios-cdrom, boot-flow,
streaming-guard). Pipeline note: pass the feature sets as SHELL ENV,
not make command-line vars -- command-line vars ride MAKEFLAGS
through cargo into every nested make and silently override the
recipe's internal-disc feature set (cost one confusing iteration:
identical bins, empty telemetry profile).
