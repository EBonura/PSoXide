# Second performance pass: shared code and generated MIPS

This pass starts after the earlier Comicon fixes: PSoXide 8127fab8, Quake
c2329be, and HL feeb9b3. Work was isolated from the active Cortex 0.6 editor
changes. The priority remained Cortex 0.5, Quake, then HL. The owner selected
the first chapter tram ride for HL instead of the archived Hazard Course tape.

## What is shared, and why

| Layer | Retained change | Consumers |
|---|---|---|
| Screen rejection | Stop calculating outcodes once rejection is impossible; preserve the historical pairwise rule | Cortex, Quake, HL |
| GPU polygon extents | One integer proof rectangle with mandatory exact fallback for outliers | Cortex model projection and HL world quads |
| Packet storage | Exclusive fixed-slot batch writer; commit only the emitted prefix once | Shared SDK, used by Cortex's model face walker |
| Retained asset lookup | Search two-byte model IDs before decoding a full header | Shared BSP asset library, used by Quake |

The fixed extent rectangle includes the viewport and spans exactly 1023 by
511 pixels. A nonzero code never rejects a polygon, changes subdivision, or
permits an oversized packet. Cortex recovers the old extrema from projected
part ranges; HL runs both original triangle checks. Empty model ranges retain
the exact fallback. The packet writer preserves 56-byte slot spacing, packet
addresses, capacity, command order, and the existing OT-linking pass.

This extends the existing shared clipping, projection scheduling, packet
formats, scratchpad ownership, and BSP collision code. A wholesale renderer
merge is not justified by these profiles: Quake's compact alias vertices,
Cortex's blended joint models, and HL's native patch correction have different
storage and numerical contracts. Forcing one layout or precision would add
RAM traffic or change pixels. Existing rejected experiments include common
Q12 precision, write-time OT linking, and fused world materialization; they
were not recycled as optimizations here.

## Evidence down to instructions

Cortex's baseline model-submit symbol retired 334.5 million instructions.
Projection maintained four extrema for every vertex, while its face loop
reloaded and stored arena counters per packet. The extent proof removes the
ordinary extrema fold; the batch cursor makes the packet loop advance a local
pointer and commits counters once. A checked-reservation prototype improved
cycles but duplicated the face loop: model submit grew from 28,156 to 33,568
bytes. Making the caller's existing capacity preflight explicit allowed LLVM
to discard that fallback copy and shrink the symbol to 27,920 bytes. That
variant was rejected: despite retiring fewer instructions, it cost 0.30% more
bus cycles than the checked version. The faster checked version is retained;
its extra code fits the existing RAM budget without shrinking any arena.

HL's generated quad routine contains repeated compare/branch extrema folds,
branch-delay slots and signed-coordinate conversions. With the shared extent
proof, measured instructions per invocation fall from about 543 to 504.
This is normalized over a moving scene, not an identical-input microbenchmark.
The routine grows from 5,280 to 5,440 bytes; the full route decides acceptance.

Quake's original lookup calls AliasModelHeader::decode inside the search loop,
reads only its ID, then decodes the matching header again. ID-only search
removes those rejected 68-byte header copies. Header-decoder instructions fall
from 20,500,452 to 12,371,562 across the route. The lookup itself grows from
348 to 404 bytes, but removes substantially more executed decode work.

The Cortex symbol gate still reports linked 64-bit division helpers, as it
did before this pass. The final intermediate replay attributes only about 610
instructions to those helpers, out of roughly 1.8 billion. They are not the
measured bottleneck. No claim is made that this pre-existing gate now passes.

## Validation boundaries

All timing is from the same PSoXide headless emulator, not host elapsed time.
Cortex uses the tracked whole-level tape on 0.5, fixed presentation cadence,
and stops at poll 5,250. Two runs must agree exactly. Quake uses its complete
60-waypoint E1M1 route, mechanism and sound checks, and E1M2 transition, twice.
HL runs from the menu for 27,220 route ticks with shipping arenas and normal
presentation cadence. Its stationary final segment is reported separately.

Cortex and Quake retain identical final display and VRAM hashes. HL retains
both hashes too; 38 of 68 fixed-tick screenshots are byte-identical to control.
The other screenshots shift with presentation cadence. Tunnel, tracks,
grates, moving machinery, reactor and final-station comparisons were reviewed.
No texture, actor, lighting, arena, subdivision, frame cap or gameplay setting
was reduced to obtain these measurements. Original-console testing is still
needed to establish silicon timing, and HL's slow stretches remain below
20 fps.

Host checks cover exhaustive outcode corner combinations, the entire i16
coordinate range for the proof rectangle, blended-vertex projection, packet
batch bounds/address/counter behavior, and retained model lookups including
missing IDs. The engine and BSP library suites pass 414 and 159 tests.

Raw measurements, binaries, maps, disassembly and replay commands are retained
under /tmp/astra-stack-pass-20260904. Benchmark builds use isolated guest stage
roots. Immutable SDK snapshots keep Cortex cooking from invalidating HL's
asset fingerprint. The Quake experiment-only stage-path override is excluded
from shipping changes.

## Retained measurements

| Game / full route | Control | Retained | Change |
|---|---:|---:|---:|
| Cortex bus cycles | 4,237,385,467 | 4,128,279,243 | -2.575% |
| Cortex work instructions | 1,583,897,056 | 1,537,045,018 | -2.958% |
| Quake bus cycles | 2,256,390,660 | 2,241,538,328 | -0.658% |
| Quake work instructions | 1,788,228,760 | 1,773,821,759 | -0.806% |
| HL rendered flips in gameplay | 8,885 | 8,915 | +30 |
| HL tram gameplay FPS | 21.585 | 21.658 | +0.34% |
| HL p05 five-second window FPS | 8.98 | 9.18 | +0.20 fps |

HL's moving segment FPS is 20.15, 13.40, 20.27, 17.00, and 21.58;
the stationary final segment is 29.63 fps. Its small aggregate benefit must
not be presented as a stable 20 fps result. Quake's full-route FPS moves
25.232 -> 25.399; work instructions are the more reliable rank because VBlank
quantization and code placement can move its FPS by about 0.122.

Cortex incremental candidates: early screen exits save 1.362% cycles; lazy
bounds save a further 0.164%; packet batching initially saves another 1.095%.
The final empty-range guard and formatted source were replayed again to obtain
the retained totals above. The rejected unchecked reservation saves 3.437%
work against original control but only 2.278% cycles, so it was fully removed.

Exact final display / VRAM hashes:

- Cortex: fdd4a01402c63c96 / 5d151bce1b4a5c55.
- Quake: 621bf7ee03f427a4 / 6c23b5e6511bc16e.
- HL: ef1272e9ccda14f1 / 239541ec4669bcce.
