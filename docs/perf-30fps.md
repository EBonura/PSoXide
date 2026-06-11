# perf-30fps campaign log

Goal: 30 fps on real hardware. Charter: trust no instrument blindly;
optimise at any level (SDK to engine); rewrites welcome; aim for
conciseness, elegance, simplicity. Most of the engine predates the
silicon-accurate emulator, so architecture decisions are open.

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
