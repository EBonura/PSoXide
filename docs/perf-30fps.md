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
