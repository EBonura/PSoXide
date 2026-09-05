# Cortex 0.4b: remove wide division from the guest

Baseline: `501e1af4`, after the backend-pruning pass. All five previously
linked 64-bit division/remainder helpers are absent from the rebuilt guest.
`tools/guest_symbol_gate.sh` now passes and the normal project disc builder
runs it before copying the executable and packing the disc.

## Exact replacements

The material UV and BSP texture scroll paths now share
`psx_math::int32::scroll_q8_wrapped`. Animation gait phase transfer uses
`mul_div_u32`. No authored speeds, UV phases, animation timing or gameplay
rules change.

For scrolling, let `D = ticks_per_second * 256` and texture period be `P`.
Truncating `speed * tick / ticks_per_second / 256` toward zero equals division
by `D`. Reducing tick modulo `D * P` removes an integer multiple of
`speed * P` texels, preserving the final wrapped result for either sign.
`D * P` fits u32 even for rate 65535 and period 256; the reduced distance is
less than `abs(speed) * P <= 2^23`. Keep negative motion's original symmetric
truncation, and normalize zero-sized BSP textures to period one as before.
Ordinary tick counts skip the wrap division entirely.

`mul_div_u32` preserves both words of the native R3000 MULTU result. A zero
high word uses ordinary hardware DIVU; larger products use the existing
32-bit restoring-division routine. There is no software 64-bit division.
The Rust u64 product is explicitly allowlisted because MULTU produces both
words natively, like the existing fixed-point multiply helper. It is not a
narrowing cast that loses high bits. Animation's `local < outgoing_cycle`
ensures the quotient is smaller than the incoming cycle and fits u32.

Tests compare against the original wide arithmetic in host-only code:
more than 900,000 checks cover every signed scroll speed, all periods 1–256,
negative fractional motion, phase/tick wrap boundaries, rates through 65535,
maximum u32 ticks, full-width animation cycles and deterministic random
inputs. Consumer tests retain disabled-motion, zero-rate and BSP zero-period
behavior. All 488 tests pass: math 21, level 43, BSP 160, runtime 206,
cooked playtest 58.

## Size and native replay validation

The final `.text` is 710,796 B versus 712,032 B: another **1,236 B removed**.
Heap free after both routes remains 41,040 B because executable section
alignment absorbs this smaller reduction. The BIN remains 28,445,088 B.

| Replay | Render median before / after | Render p95 before / after |
|---|---:|---:|
| Traversal, 5,250 polls | 1,084,985 / 1,086,130 cycles (+0.11%) | 1,666,420 / 1,700,543 (+2.05%) |
| Combat, ~3,310 polls | 1,227,105 / 1,230,759 cycles (+0.30%) | 2,103,277 / 2,125,349 (+1.05%) |

These are positive `visual_render_task` samples on gameplay rows (`camera > 0`).
The candidate and baseline finish traversal with a pixel-identical RGB image.
Both replays preserve sampled player/camera positions, facing, action and hit
counts. Both combat runs release three projectiles; the final render differs
slightly in animation phase because the scheduler stops at poll 3314 versus
3316. These are normal cadence runs, not frame-by-frame lockstep proofs.

Worst gameplay lateness remains seven vblanks. Late updates are 559 / 563 on
traversal and 447 / 449 on combat. Small timing changes from code layout and
render scheduling remain; the higher p95 means this is **not a measured FPS
improvement**, despite simpler arithmetic. The removal restores the numeric
rule and reduces code size without changing mathematical results.

All four baseline/final RAM dumps have zero guest faults. The final binary
passes the symbol and MIPS hazard gates. Preburn passes with the world/UI
packs and both CD audio tracks. Exact hashes, heap values and measurements
are in the [receipt](../editor/projects/cortex-ignition-tech-demo-0.4b/review/arithmetic/summary.json).

## Reproduce

Build via `editor/projects/cortex-ignition-tech-demo-0.4b/tools/build_review_disc.sh`
with unchanged O2/LTO/default features plus `cd-stream-bench emulator-telemetry`.
No diagnostic gameplay overrides. The receipt contains exact EXE hashes,
section sizes, sampled state, timing distributions and fault/heap readings.

Replay `editor/archive/fixtures/cortex-0.4/whole-level.pxtape` to poll 5250 and
`editor/projects/cortex-ignition-tech-demo-0.4b/review/code-size/volley-observe.csv`
to poll 3310, using `frontend launch --embedded-playtest --guest-debug-log`,
`--profile-log`, `--counter-log`, `--cpu-cycle-profile-log`, `--dump-display`
and `--dump-ram`. The emulator executable was snapshotted once for both sides.
Raw CSVs, EXE/map snapshots and logs are retained in `/tmp/cortex-arithmetic/`
on the review machine. These are emulator measurements, not a hardware FPS claim.
