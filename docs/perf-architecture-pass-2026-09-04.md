# Third renderer and architecture pass, 2026-09-04

The retained Cortex changes reduce whole-route emulated bus cycles by 1.771%
from the previous pass. This is an incremental saving, not a large frame-rate
breakthrough. Graphics, gameplay, model precision, subdivision and capacities
are unchanged. Original-console timing remains unmeasured.

## Retained changes

Static world-object PVS membership is cached for the current observer leaf.
The world registry and BSP are immutable for the runtime lifetime. Destruction,
frustum and direct occlusion checks still run each frame. The previous code
repeated these static bounds queries every render and displaced entries from
the smaller general bounds cache.

The shared authored-model face sink gathers three independent face words
before decoding their indices. Generated MIPS previously interleaved each
load with dependent index arithmetic and inserted a load-delay NOP. The
scheduled gather preserves all corner words and projected coordinates. A
slice iterator advances the face pointer directly, removing indexed address
reconstruction from the hot loop. The guest hazard scanner finds zero branch
hazards; its two straight-line load-use warnings are recorded separately.

Quake's weapon renderer now calls the shared model-ID lookup instead of
fully decoding each preceding model record. Missing-ID and first-match
semantics remain unchanged. No HL renderer experiment is retained.

## Measurements

Frozen frontend SHA-256:
8dd6ca938059b29a831fa754b78642865587bb7cf04e7a1a625c1017a47a1041.
Cortex uses the tracked whole-level tape on 0.5, cd-stream-bench and
lockstep-visuals, stopping at poll 5250. Every retained candidate runs twice.
Quake uses the complete 60-waypoint E1M1 chain route into E1M2, twice.
HL uses the full opening tram at the user's request.

| Full route | Previous pass / fresh control | Retained | Change |
|---|---:|---:|---:|
| Cortex bus cycles | 4,128,279,243 | 4,055,161,044 | -1.771% |
| Cortex work instructions | 1,537,045,018 | 1,489,337,232 | -3.104% |
| Quake bus cycles | 2,242,109,628 | 2,235,254,601 | -0.306% |
| Quake work instructions | 1,773,825,096 | 1,770,087,551 | -0.211% |

Cortex retains 2620 presentations and final display/VRAM hashes
fdd4a01402c63c96 / 5d151bce1b4a5c55. Its measured guest SHA-256 is
fcd603757e378900906faaa184f3fd3708b6fbbdd1e7b1dccd2fee1911bd8051;
BSS ends at 0x801f5854, below the 0x801f8000 reserved-stack boundary.
Quake retains 1682 presentations, all 60 waypoints, mechanisms 0x7fff,
mover sounds 0x07 and final hashes 621bf7ee03f427a4 / 6c23b5e6511bc16e.
The FPS movement is below Quake's documented 0.122 fps layout-noise band;
work and bus-cycle measurements are the evidence for this small saving.

## Rejected experiments

| Candidate | Result | Decision |
|---|---|---|
| Inherited BSP plane proofs | Cortex bus cycles +0.443% | Removed |
| Small model projection copy to scratchpad | Cortex bus cycles +0.603% against static PVS cache | Removed |
| HL cached per-vertex clip flags | Saves 31.3M quad instructions but adds 46.9M cache-write instructions and 3104 bytes; worst moving segment 17.00 to 16.96 fps | Removed |

HL retains the previous pass's 21.658 fps tram aggregate with slow stretches
below 20 fps. The rejected flag-cache image must not be shipped.

## Validation and evidence

Final host suites pass 159 BSP tests, 414 engine tests, 190 game-runtime tests
and six integration tests. Quake's host suite passes. The pre-existing Cortex
64-bit symbol gate still fails for linked divide helpers; this pass does not
claim to fix that gate. Final full-route display and VRAM hashes match control.
Intermediate checkpoint comparisons and release integration are recorded in
the demo-disc validation report after their checks finish.

Raw builds, maps, disassembly, rejected patches and replays are retained under
/tmp/astra-architecture-pass-20260904. The source baseline is SDK 26698622,
Quake 60190c78 and HL de51288. Experiments use isolated worktrees and fresh
Cortex guest stage roots. Benchmark features are not shipping features.
