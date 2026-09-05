# Cortex Ignition 0.4b code-size pass — 2026-09-05

The 64-bit division issue identified here is resolved in the
[arithmetic follow-up](cortex-arithmetic-2026-09-05.md). The measurements below
record the preceding backend-pruning pass.

Baseline: `61b4c7b1` on `codex/cortex-04b-souls-polish`. This is the normal
v0.4b spawn, assets and scheduler, not a diagnostic gameplay build.

## Measured result

| Resident section | Before | After | Reduction |
|---|---:|---:|---:|
| Machine code (`.text`) | 750,808 B | 712,032 B | **38,776 B / 5.16%** |
| Initialized data (`.data`, including read-only data) | 385,824 B | 385,696 B | 128 B |
| Zero-initialized storage (`.bss`) | 851,204 B | 851,204 B | 0 B |
| Free heap after each tested route | 2,128 B | 41,040 B | **38,912 B more** |

The executable's smaller resident footprint frees RAM. The raw disc remains
28,445,088 bytes: disc layout, padding and CD audio have different size constraints.
This is not a whole-disc compression result.

## Changes retained

The project cook already declares whether the world uses PXBSP. Camera and
player/entity collision still selected their backend through runtime `Option`
checks, keeping unreachable grid collision and spring-arm routines linked.
Use the cooked constant at those decisions. BSP initialization already fails
explicitly if loading fails, so a missing BSP is not a legitimate grid fallback.

Likewise, compile the streamed-grid room and equipment render loops only for
grid projects. Keep the shared BSP gameplay content, player, equipment,
lighting, particles, shadows and UI passes. Grid projects still compile their
original paths. The final map no longer contains `nearest_wall_hit_around`,
`body_stand_position`, `body_hits_solid_wall` or `commit_body_step`.

Remove the unused `check_run_complete` routine and its stale comments/name
constant. It belonged to the old all-enemies-dead ending, superseded by the
heavy-enemy thank-you message. This cleanup contributes **no claimed binary
saving**: the linker already discarded that unused routine.

No art, audio, combat rules, collision accuracy, visibility quality or frame
cadence changes. Compiler settings remain `opt-level=2`, LTO and one codegen
unit. Earlier `-Os` experiments documented in the guest Cargo.toml made long
frames substantially worse; global size optimization is not an acceptable
shortcut here.

## Performance and correctness

Identical emulator executable and normal build flags on each side; default
features plus `cd-stream-bench emulator-telemetry`. Stage measurements below
select gameplay rows with `camera > 0` and positive `visual_render_task` values.
Values are emulated bus cycles, including RAM/cache stalls, not host timings.

| Replay | Render median before → after | Render p95 before → after | Camera median change |
|---|---:|---:|---:|
| Dash, 1,800 polls | 602,548 → 598,196 | 803,648 → 790,719 | -3.88% |
| Light enemy combat, ~3,310 polls | 1,231,989 → 1,227,105 | 2,128,646 → 2,103,277 | -2.62% |
| Recorded traversal, 5,250 polls | 1,087,269 → 1,084,985 | 1,702,204 → 1,666,420 | -4.10% |

All three finish with the same sampled player/camera position, facing, action
and player attack/hit totals. Dash and traversal final RGB frames are
pixel-identical. Combat ends at 3,312 versus 3,314 polls due to scheduler batch
boundaries; both log three projectile releases and the same final spatial
state. Its screenshot differs slightly in animation phase and was inspected.
These normal-cadence replays are not a frame-by-frame deterministic proof.

No new long-frame regression in these samples: worst reported gameplay
lateness remains seven vblanks in both builds; late gameplay updates are
2→2, 449→447 and 564→559. Small per-stage fluctuations remain (dash collision
p95 rises from 22,761 to 22,865 cycles). The result is a size reduction with
slightly cheaper measured work, not a claim that every stage is faster.

Do not use whole-route bus cycles as a speedup here: the shipping scheduler
waits for vblank. For example traversal totals are 3,190,880,971 versus
3,190,880,965 cycles while the candidate renders eight more gameplay samples.
Nor do these emulator measurements establish original-hardware frame rate.

Validation: 206 runtime tests; 58 playtest tests with the cooked BSP fixture;
58 playtest tests with the grid placeholder fixture. All pass. Both backends
are compiled. All six baseline/candidate RAM dumps have zero guest faults.
The rebuilt executable has zero scanned MIPS hazards. The guest symbol gate
**fails on both baseline and candidate** for the same five 64-bit division
helpers; this inherited issue is the first follow-up below. Disc preburn passes with `WORLD.PAK`, `UI.PAK` and two audio tracks.

Detailed counts, hashes, heap readings and sampled state are in
[the receipt](../editor/projects/cortex-ignition-tech-demo-0.4b/review/code-size/summary.json).
Replays and screenshots are retained alongside the receipt; raw profiler CSVs,
logs and baseline/candidate EXE/map snapshots are in `/tmp/cortex-size/` on the
review machine. Rebuild through the project's `tools/build_review_disc.sh`,
then replay with `frontend launch --embedded-playtest --guest-debug-log`,
`--profile-log`, `--counter-log`, `--cpu-cycle-profile-log`, `--dump-display`
and `--dump-ram`. Use the receipt's poll limits. The traversal tape is
`editor/archive/fixtures/cortex-0.4/whole-level.pxtape`; the two shorter tapes
are in the receipt directory. Keep compiler/features and emulator identical.

## Remaining candidates, in priority order

1. **Replace the remaining 64-bit divides with proven narrow arithmetic.**
   Disassembly traces signed divide/remainder calls to material UV scrolling
   (`LevelMaterialUvMotion::offset_at_tick`, also inlined into model material
   selection, and BSP `fill_pxbsp_material_cache`). Unsigned division is also
   in the player's animation phase transfer in `playtest_runtime.rs`.
   The five linked helpers occupy 1,820 B, plus caller setup. Reuse the
   existing `psx_math::int32::div_u64_by_u32` high/low-word implementation or
   reduce the periodic UV calculation before multiplying. Test negative UV
   speeds, phase wrap, arbitrary periods and maximum tick/cycle values
   against the old host-only wide arithmetic. Simple `as u32` narrowing
   would corrupt long animation cycles. No arithmetic change is bundled
   into this backend-pruning pass; the inherited symbol-gate failure remains.

2. **Cook out unreachable composed UI scenes.** Project scene/state 15,
   `Ending`, still has seven authored UI nodes, although no current control
   routes to it and the retired runtime trigger was its caller. A cooker
   reachability pass could omit that scene from shipping tables/packs while
   preserving the editor asset. Account for dynamic scene requests and
   loading overlays before generalizing this to other projects. Saving is
   primarily data; do not promise a whole UI renderer disappearing.
3. **Outline cold scene setup and menu code selectively.** The final map has
   27,580 B in `GameApp::render_ui_scene`, 16,056 B in `enter_flow_state`,
   11,100 B in `switch_resources` and 6,592 B in `draw_focus_ring`.
   Inspect duplicated inlined gradient/shape/resource setup before trying
   `#[inline(never)]` on shared cold helpers. Benchmark menu input, loading,
   pause/resume and the gameplay transition, not only steady-state combat.
4. **Generate UI/material capability flags from the cook.** Shared runtime
   dispatch handles more UI node/paint/texture variants than one disc uses.
   Emit proven usage masks so absent implementations can be removed at build
   time. Retain all variants reachable through settings and alternate HUDs.
   Requires cooker/runtime contract tests; savings are not yet measured.
5. **Audit the largest hot generic functions carefully.** `Scene::render`
   is 95,340 B, `update_gameplay` 54,208 B, classic-affine submission 39,964 B,
   textured model geometry 34,636 B, and the BSP blocker trace 18,100 B.
   Extract genuinely repeated setup/validation outside hot loops before
   adding dispatch or breaking beneficial inlining. Function sizes include
   inlined callees; they are not independent removable features. Removing
   renderer unrolling can cost much more than the saved bytes are worth.
6. **Distinguish obsolete experiments from shipped cost.** Alternative OT
   ordering (`global/slot/linked`), visibility experiments, projected shadows,
   collision/vertex overlays and `lockstep-visuals` are opt-in. Their disabled
   paths already contribute zero to this normal binary. Archive unsuccessful
   experiments if maintenance cost warrants it; keep useful hardware/profiling
   controls. Do not remove grid support from the engine just because this
   project is BSP-only.

Existing authored-count entity/model/logic capacities and static caches have
already received size work. Shrinking them arbitrarily risks cook failures,
missing content or capacity overflows; derive further reductions from actual
asset requirements. Avoid moving hot scratch buffers to the heap merely to
make `.bss` look smaller.
