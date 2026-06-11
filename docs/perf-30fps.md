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

## Attack list (revised)

1. world_flush forensics: find the June->now regression commit; then
   the pipelining arc (double-buffered OT, async submit).
2. PVS default decision: verify the far-cell visual bug vs PVS in this
   build (frame diff), then retire vis-full-active-chunks if clean.
3. RAM architecture: stream the ~500k of embedded assets; rethink the
   135k always-resident menu-image cache; right-size from map data.
   VRAM allocation survey (same instrument philosophy) queued.
4. Player path / GTE utilisation (330k for ~250 verts).
5. update (181k), image_props (137k).
