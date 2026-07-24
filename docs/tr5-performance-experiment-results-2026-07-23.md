# TR5 performance finding experiments

Date: 2026-07-23

This is the implementation and measurement ledger for the 21 findings in
[`tr5-performance-architecture-survey-2026-07-23.md`](tr5-performance-architecture-survey-2026-07-23.md).
It records tested behavior, including negative and inapplicable results; a
source-level resemblance to TR5 is not counted as a performance win.

## Verdict rules

- **Net positive:** correctness gates pass and the representative workload
  improves materially without an unacceptable RAM, code-size, latency, or
  maintenance cost.
- **Neutral / conditional:** the implementation is correct but the measured
  benefit is within noise or only appears beyond a stated scaling threshold.
- **Net negative:** it regresses the representative workload or fails a
  correctness/resource gate.
- **Hardware-gated:** emulator and static checks cannot establish the PS1
  bus/cache/DMA effect. It is not counted as a positive until hardware data
  exists.
- **Not applicable:** the proposed mechanism has no corresponding workload or
  would solve a different resource problem. This is an explicit tested design
  verdict, not an assumed win.

The primary runtime workload is the fixed 900-frame Cortex v1 input tape:

```text
0x4000@45+12,0x4000@80+16,0x4000@120+20,
0x4000@250+12,0x4000@400+12,0x4000@550+12
```

Runtime comparisons use common rendered guest frames with a non-zero active
room mask whenever a candidate changes deadline behavior. This prevents a
faster candidate's additional visual frames from changing the scene mix in
the average. Focused host tests are used where Cortex v1 cannot exercise the
finding.

## Final matrix

This table stays deliberately incomplete until every finding has direct
evidence.

| # | Finding | Implementation under test | Evidence | Verdict |
|---:|---|---|---|---|
| 1 | Table-driven one-level subdivision and warmed leaves | Fixed 3x3 lattice; nine unique projections, four constant leaves | Host packet equivalence + Cortex v1 A/B | **Net positive; adopted** |
| 2 | Surface-kind subdivision policy | Compact mask; floor+wall default, ceilings opt-in | Upward-camera real band + forced-near stress A/B | **Net positive when close ceilings exist; adopted policy** |
| 3 | Model LOD before skeleton/mesh work | Hysteresis/player-exclusion policy prototype | Focused policy test + Cortex asset/counter inventory | **Conditional future win; not applicable to Cortex** |
| 4 | Visible-object list and matrix stash | Fixed-list scaling prototype | Focused operation-count test + Cortex counters | **Conditional above multiple global instances; reject for Cortex** |
| 5 | Per-room/top-three object lights | Exact cooked room ranges; top-three prototype removed | 15 focused tests + two Cortex v1 A/Bs | **Room ranges neutral/conditional; top-three net negative** |
| 6 | Scratchpad-owned hot working sets | Typed 628-byte layout prototype | Layout test + existing hardware roundtrip test | **Hardware-gated; not counted positive** |
| 7 | Empty OT link compaction | Final-submit relink prototype, then removed | Packet-chain test + Cortex v1 A/B | **Net negative; rejected** |
| 8 | Active entity/effect lists and room ownership | Packed awake/room-mask prototype | Focused equivalence/scaling test + Cortex counters | **Conditional large-pool win; reject for current population** |
| 9 | Fixed active-AI budget and incremental LOT | Resumable fixed-frontier search prototype | Focused hard-budget test + feature inventory | **Budget policy valid; pathfinding not applicable yet** |
| 10 | Portal rectangles/bounded traversal | Existing recursive clipped-frustum implementation | 27 focused tests + Cortex counters | **Net positive existing path** |
| 11 | Roomlets/layered bounding rejection | Existing populated-cell AABB layer | Focused geometry tests + Cortex counters | **Net positive existing path** |
| 12 | Unique projection and RTPT scheduling | Existing indexed projection + fixed lattice | Projection equivalence tests + Cortex/GTE counters | **Net positive existing; more generic batching rejected** |
| 13 | Packed face streams/specialized loops/direct OT links | Existing packed model and bucketed OT paths | Model counters + 6 OT tests | **Net positive existing path** |
| 14 | Fixed arenas/overflow behavior | Existing packet/command arenas | Arena tests + capacity/overflow counters | **Net positive safety architecture** |
| 15 | Baked + once-per-object lighting | Existing baked room RGB and per-model material shade | Lighting/packet tests + room micro-profile | **Net positive existing path** |
| 16 | Simplified shadows | Existing single-quad actor shadow vs shadows-off reference | Cortex v1 A/B | **Retain; modest 1.84% render cost** |
| 17 | Double polygon buffers/async GPU | Existing single-frame async OT; evaluate measured wait/latency | OT tests + Cortex wait profile | **Current overlap positive; extra frame buffer rejected** |
| 18 | Targeted sqrt/division LUT | Bounded distance approximation prototype, then removed | 7 lighting tests + Cortex v1 A/B | **Neutral; rejected** |
| 19 | Code overlays | Link-map RAM applicability probe | MIPS link map and payload/static footprint | **Not a frame-time win; RAM-only conditional** |
| 20 | Streaming/residency | Existing paged room/asset streaming and active-room residency | 7 focused tests + Cortex load/prefetch counters | **Net positive existing path; whole-level residency rejected** |
| 21 | 30 Hz simulation cadence | Existing even-tick NPC update vs 60 Hz reference | Cortex v1 A/B + delta invariance tests | **Net positive; keep 30 Hz** |

## Experiment 1 — fixed one-level subdivision lattice

### Candidate

The original one-level quad splitter created the correct five shared
midpoints, but each of its four child quads independently projected four
vertices. The candidate lays the points out as a fixed 3×3 lattice, projects
the nine unique points as three RTPT groups, and indexes four constant leaf
quads:

```text
0--1--2
|  |  |
3--4--5
|  |  |
6--7--8

[0,1,3,4] [1,2,4,5] [3,4,6,7] [4,5,7,8]
```

It is isolated by the `tr-subdivision-lattice` feature. The first runtime
prototype revealed that its early return omitted the existing whole-quad
underdraw pass: it emitted exactly 14 fewer primitives on every matched
gameplay frame and changed the image. That version is recorded as a failed
iteration. Restoring the underdraw produced the corrected candidate below.

### Correctness evidence

- `cargo test -p psx-engine --features tr-subdivision-lattice`:
  261 unit tests plus the time-type compile test passed before the focused
  equivalence test was added.
- `one_level_lattice_packets_match_recursive_reference_bitexact` compares a
  non-planar, depth-skewed quad against the recursive implementation. All four
  GPU packet byte sequences, OT slots, depths, layers, word counts, orders, and
  aggregate stats are identical.
- Across 46 guest frames rendered by both the baseline and corrected candidate,
  surface count stayed 23, projected authored vertices stayed 38, and primitive
  and world-command counts were identical on every frame.
- The matched runtime screenshots show the same room geometry. Their full
  display hashes differ because the candidate meets more render deadlines and
  therefore has a different render/interpolation history for the animated
  player; packet equivalence is the stronger room-render correctness check.
- Primitive and command overflow counters remained zero.

### Performance evidence

Matched gameplay frames:

| Metric | Recursive baseline | Fixed lattice | Delta |
|---|---:|---:|---:|
| Room surface draw | 529,827 cycles | 447,647 cycles | **-82,180 (-15.51%)** |
| Room total | 585,335 cycles | 503,074 cycles | **-82,261 (-14.05%)** |
| Render stage | 961,448 cycles | 881,103 cycles | **-80,345 (-8.36%)** |
| Emitted primitives | 335.83 | 335.83 | 0 |
| World commands | 335.83 | 335.83 | 0 |

Whole 900-frame tape:

| Metric | Recursive baseline | Fixed lattice | Delta |
|---|---:|---:|---:|
| Visual frames | 359 | 402 | +43 |
| Deadline misses | 88 | 45 | **-43 (-48.9%)** |
| Render cycles/visual | 961,099 | 916,808 | -4.61% |
| Estimated GTE cycles/visual | 6,379 | 6,073 | -4.80% |

The PSX executable payload rounded from 845,824 to 854,016 bytes (+8 KiB).
That cost is acceptable for the measured frame-time recovery, but will remain
in the final resource audit.

### Verdict

**Net positive; adopted as an editor-playtest default.** The speedup is far
outside run-to-run noise, restores 43 visual frames on the fixed tape, and
preserves the exact room leaf packets and command topology.

Raw evidence:

- `/tmp/tr5-exp01-baseline-profile.csv`
- `/tmp/tr5-exp01-baseline.log`
- `/tmp/tr5-exp01-lattice2-profile.csv`
- `/tmp/tr5-exp01-lattice2.log`
- `/tmp/tr5-exp01-baseline-frame646.png`
- `/tmp/tr5-exp01-lattice-frame646.png`
- `/tmp/tr5-exp01-frame646-diff.png`

## Experiment 2 — floor/wall subdivision policy

### Candidate

`TombRaiderSubdivisionKindMask` makes floors, ceilings, and walls independently
eligible before the cached room renderer chooses the generated-vertex path.
The candidate uses `FLOOR_WALL`; `tr-subdivision-ceilings` restores `ALL` for a
project whose ceiling textures visibly need correction.

A focused unit test,
`floor_wall_subdivision_mask_keeps_ceilings_on_authored_path`, proves that the
mask preserves floor and wall subdivision while disabling it for ceilings.

### Workloads

The ordinary Cortex tape never submits a visible ceiling. A second 1,200-sample
input tape holds the right stick upward after loading. The detailed discovery
build measured, per gameplay render:

| Surface outcome | Mean/count |
|---|---:|
| Floors reaching kind stage | 15 |
| Ceilings reaching kind stage | 3 |
| Walls reaching kind stage | 6 |
| Profiled surfaces after varying culls | 28.91 mean |

Those ceilings are visible but outside the normal TR subdivision distance.
Therefore two A/B pairs were run:

1. the real sector-scaled distance band;
2. an experiment-only four-times distance band that forces the same three
   ceilings through subdivision and measures the upper-bound cost.

### Real-band result

159 common gameplay renders:

| Metric | All kinds | Floor + wall | Delta |
|---|---:|---:|---:|
| Room surface draw | 531,973 | 531,140 | -833 (-0.16%) |
| Room total | 592,684 | 591,859 | -825 (-0.14%) |
| Render stage | 988,688 | 987,747 | -940 (-0.10%) |
| Primitives | 375.07 | 375.07 | 0 |
| World commands | 377.05 | 377.05 | 0 |

The final displayed image hash is identical:
`0x1ac69e8ac32dc1ba`. This pair is neutral: the ceilings were already outside
the distance gate, so the small cycle delta is code-layout/noise rather than a
real ceiling saving.

### Forced-near stress result

The stress profile is not a proposed shipping distance. It exists to make
three real Cortex ceilings exercise the policy:

| Metric | All kinds | Floor + wall | Delta |
|---|---:|---:|---:|
| Room surface draw | 552,317 | 500,208 | **-52,109 (-9.43%)** |
| Room total | 612,994 | 560,906 | **-52,088 (-8.50%)** |
| Render stage | 1,008,654 | 956,073 | **-52,581 (-5.21%)** |
| Primitives | 376.86 | 367.04 | -9.81 |
| World commands | 376.86 | 368.03 | -8.83 |

The two screenshots have only 510 changed pixels (0.66%), including
render-cadence differences, and no perceptible ceiling degradation in the
captured view. Counts, authored projected vertices, and considered surfaces
remain equal; only generated ceiling leaves disappear.

### Verdict

**Net positive when a close ceiling would otherwise subdivide; adopted as the
default policy.** It is neutral on current Cortex v1 because its visible
ceilings are already too far away, but avoids a measured ~52k-cycle cost in
the ceiling-bearing stress case. Projects can opt back into ceiling correction
with `tr-subdivision-ceilings`.

Raw evidence:

- `/tmp/tr5-ceiling-micro-profile.csv`
- `/tmp/tr5-ceiling-micro.log`
- `/tmp/tr5-exp02-all-profile.csv`
- `/tmp/tr5-exp02-all.log`
- `/tmp/tr5-exp02-floor-wall-profile.csv`
- `/tmp/tr5-exp02-floor-wall.log`
- `/tmp/tr5-exp02-wide-all-profile.csv`
- `/tmp/tr5-exp02-wide-all.log`
- `/tmp/tr5-exp02-wide-floor-wall-profile.csv`
- `/tmp/tr5-exp02-wide-floor-wall.log`
- `/tmp/tr5-exp02-wide-all.png`
- `/tmp/tr5-exp02-wide-floor-wall.png`
- `/tmp/tr5-exp02-wide-diff.png`

## Experiment 7 — empty ordering-table link compaction

### Candidate

The prototype scanned the complete ordering table immediately before the final
DMA kick. For every occupied depth slot it walked to the tail packet and
relinked that tail directly to the preceding occupied slot, skipping empty
zero-word entries. Submission then began at the highest occupied slot. The
prototype was deliberately placed at final submission rather than at the end
of the room pass, because models, the player, equipment, and other draws can
still append packets after room rendering.

A focused SDK test built packets in two sparse slots, including two packets in
one slot. The normal and compacted iterators returned the same three packet
pointers, word counts, and order, and the compacted head selected the highest
occupied slot. All seven OT tests and all 263 engine tests passed.

### Result

On 61 common gameplay visual frames with the same active room, primitive and
world-command counts were identical on every frame:

| Metric | Normal OT | Compacted OT | Delta |
|---|---:|---:|---:|
| OT submit | 128 cycles | 52,745 cycles | **+52,617** |
| OT wait | 54.00 cycles | 54.41 cycles | +0.41 |
| Render stage | 874,823 cycles | 877,496 cycles | +0.31% |
| Whole visual task | 975,438 cycles | 1,239,808 cycles | **+27.10%** |
| Primitives | 335.03 | 335.03 | 0 |
| World commands | 335.03 | 335.03 | 0 |

Across the full 900-frame route, visual frames fell from 402 to 367 and
deadline misses rose from 45 to 79. The relink scan is timed inside OT submit;
its ~52.6k-cycle cost is vastly larger than the effectively unchanged measured
DMA/GPU wait. The differing final display hash follows from the lost render
deadlines, while equal matched-frame packet counts and the chain test establish
semantic equivalence.

### Verdict

**Net negative; rejected and removed.** Empty OT links are cheap enough that
CPU-side discovery and tail walking cannot pay for themselves on this
workload. A hardware-only bus effect would need to recover more than 52k CPU
cycles per render, while the current measured wait is only ~54 cycles.

Raw evidence:

- `/tmp/tr5-exp07-baseline-profile.csv`
- `/tmp/tr5-exp07-baseline-counters.csv`
- `/tmp/tr5-exp07-baseline.log`
- `/tmp/tr5-exp07-compact-profile.csv`
- `/tmp/tr5-exp07-compact-counters.csv`
- `/tmp/tr5-exp07-compact.log`

## Experiment 21 — 30 Hz NPC thinking

### Candidate/reference pair

The shipping path runs collision-heavy entity state updates on even simulation
ticks with `delta_ticks = 2`. The experiment-only `npc-think-60hz` feature runs
the same code every tick with `delta_ticks = 1`. Player control, combat arcs,
logic, and rendering stay unchanged.

The runtime unit test `two_tick_delta_preserves_patrol_speed_and_state_clock`
passes, along with the full 61-test `psx-game-runtime` suite. It establishes
the intended time-scaling invariant independently of the performance route.

### Result

The normal 900-frame Cortex tape produced the same final display hash in both
builds: `0xb6fd12f53e6e1165`.

| Metric | 30 Hz NPC | 60 Hz NPC | Delta |
|---|---:|---:|---:|
| Entity thoughts, whole tape | 258 | 516 | +100% |
| Gameplay update, matched sim frames | 85,313 | 100,810 | **+15,497 (+18.16%)** |
| Game-logic stage, matched sim frames | 33,415 | 48,929 | **+15,514 (+46.43%)** |
| Frame cycles, matched sim frames | 1,325,344 | 1,383,062 | +57,718 (+4.35%) |
| Visual frames, whole tape | 402 | 368 | -34 |
| Deadline misses, whole tape | 45 | 79 | **+34** |

### Verdict

**Net positive; retain 30 Hz NPC thinking with two-tick deltas.** The 60 Hz
reference performs twice the decisions, produces the same final image, costs
about 15.5k extra gameplay-update cycles, and loses 34 visual frames to missed
deadlines on the fixed tape.

Raw evidence:

- `/tmp/tr5-exp21-30hz-profile.csv`
- `/tmp/tr5-exp21-30hz.log`
- `/tmp/tr5-exp21-60hz-profile.csv`
- `/tmp/tr5-exp21-60hz.log`

## Policy and applicability experiments

Findings 3, 4, 6, 8, 9, and 19 do not have representative runtime workloads
in Cortex v1. Their prototypes therefore validate semantics, capacity, and the
claimed scaling threshold; they do not manufacture a Cortex cycle win.
`tr5_policy_experiments` contains the test-only implementations so premature
runtime state does not ship.

### 3 — model LOD selection

The policy prototype selects near/far LODs with a 4,096-unit switch and
256-unit hysteresis. It stays near across samples within the dead band, changes
to far only above 4,352, returns near only below 3,840, and forcibly retains
near LOD for the player at every distance. The test passes.

Cortex has two model records, one mesh asset per record, no alternate LOD
assets, one placed instance, and only 25 placed-model draws over the complete
route. A runtime schema and mesh-streaming change cannot improve this project
until alternate meshes and a model-heavy scene exist.

**Verdict: conditionally positive architecture, not applicable to Cortex v1.**
Keep the tested hysteresis/player policy for a future LOD asset slice; do not
ship unused record fields now.

### 4 — prepared visible-object list and matrix stash

The operation-count prototype compares three consumers (shadow,
behind-player, and in-front-player). With 256 global instances and eight
visible in one room, repeated global scans perform 768 room comparisons;
prepare-once plus three compact consumers performs 280 operations. With
Cortex's single placed instance, the corresponding counts are three versus
four before paying for matrix/pose storage.

The runtime counters agree with the small-workload side: one bounds test per
gameplay render, 189 of 214 rejected, and only 25 actual model draws.

**Verdict: conditional scaling win, net negative complexity for Cortex.** Add
the stash together with per-room instance ranges when a scene has many placed
instances; do not add it for one.

### 5 — room light ranges and top-three cap

The cooker now stable-sorts lights by room. `room_light_slice` uses two binary
partition searches when a room lighting view is built, after which every shade
walks only that exact room slice. The range test returns the correct empty and
two-light slices; 14 cooker light/component tests pass. Cortex's 22 lights are
spread across six rooms.

The exact-range A/B preserves the display hash
`0xb6fd12f53e6e1165`, every primitive/command count, 402 visual frames, and 45
misses. Against the prior normal route, whole visual time changes by +0.01%;
individual lighting-owning stages remain within roughly ±0.4%. This is neutral
for one model but removes the full-level scan growth without changing light
results, so the range representation remains adopted as a scaling seam.

The separate top-three prototype first radius-rejected lights using squared
distance, ranked the three nearest without square roots, then ran normal
falloff only for those three. It also preserves the final hash, but on 212
matched gameplay renders:

| Metric | Exact room range | Top three | Delta |
|---|---:|---:|---:|
| Render stage | 896,694 | 904,599 | **+7,905 (+0.88%)** |
| Model instances | 37,645 | 38,335 | +690 (+1.83%) |
| Player | 201,607 | 206,826 | +5,219 (+2.59%) |
| Visual frames / misses | 402 / 45 | 400 / 47 | **-2 / +2** |

**Verdict: exact ranges neutral/conditional and retained; top-three selection
net negative and removed.** Ranking overhead exceeds the avoided light work in
this scene.

Raw evidence:

- `/tmp/tr5-exp05-room-range-profile.csv`
- `/tmp/tr5-exp05-room-range-counters.csv`
- `/tmp/tr5-exp05-room-range.log`
- `/tmp/tr5-exp05-top3-profile.csv`
- `/tmp/tr5-exp05-top3-counters.csv`
- `/tmp/tr5-exp05-top3.log`

### 6 — typed scratchpad ownership

The layout prototype combines nine tessellation vertices, four leaf
descriptors, a 32-entry portal queue, and three compact light records in 628
bytes with four-byte alignment, fitting the PS1's 1 KiB scratchpad with 396
bytes spare. The repository's hardware suite already contains a scratchpad
byte/half/word roundtrip test.

No production lease was installed: emulator timing cannot prove scratchpad
latency/contention, and safe ownership must be ratified against interrupt and
nested-render behavior on a console.

**Verdict: layout valid but hardware-gated; not counted as a positive.**

### 8 — active entity/effect lists

The packed-index prototype produces the same awake set from engaged state and
an O(1) room bit mask. Its scaling assertion compares a 256-slot pool with 16
awake entries, where the compact walk is over eight times smaller than the
pool scan.

Cortex runs only 258 entity thoughts over the route, while the measured
game-logic hit is ~27.9k cycles and the 30 Hz cadence already removes half the
think passes. A linked/free list would add mutation and ordering state without
recovering the current bottleneck.

**Verdict: conditional above a materially sparse large pool; reject for the
current population.**

### 9 — fixed AI/LOT search budget

The fixed-frontier prototype searches a small graph in resumable increments
and asserts that no call expands more than two nodes. It reaches the goal over
at least three calls, proving the hard budget and retained frontier semantics.

PSoXide has no cooked navigation graph or path-search workload today; entities
use direct state/motor movement. There is therefore no runtime comparison to
accelerate.

**Verdict: budget policy validated, but LOT/pathfinding is not applicable
until navigation exists.**

### 18 — targeted lighting distance approximation

The prototype replaced accepted point-light integer square root with a bounded
`max + mid/2 + min/4` three-axis distance approximation while preserving the
same radius and Q8 division. All seven lighting behavior tests passed.

Against the exact room-range build, all 214 matched gameplay renders retain
the same primitive/command counts, full PPM output, display hash, 402 visual
frames, and 45 misses:

| Metric | Integer sqrt | Approximation | Delta |
|---|---:|---:|---:|
| Whole visual task | 1,134,780 | 1,134,637 | -144 (-0.01%) |
| Render stage | 896,733 | 895,249 | -1,484 (-0.17%) |
| Model draw | 17,563 | 17,560 | -3 (-0.02%) |
| Player draw | 178,107 | 178,107 | effectively 0 |

The tiny apparent room-stage movement is code layout/noise: baked room
vertices do not call dynamic light distance at all. The stages that do call it
show no material gain.

**Verdict: neutral; approximation removed.** A lookup table would add ROM/RAM
footprint to an unmeasured hotspot.

Raw evidence:

- `/tmp/tr5-exp18-approx-profile.csv`
- `/tmp/tr5-exp18-approx-counters.csv`
- `/tmp/tr5-exp18-approx.log`
- `/tmp/tr5-exp18-approx.ppm`

### 19 — code-overlay applicability

A normal MIPS link with a map file reports:

| Region | Address/size |
|---|---:|
| Text start / end | `0x80010000` / `0x800b71c8` |
| Data end / BSS start | `0x800df800` |
| BSS end | `0x801d7508` |
| Static footprint from load base | 1,864,968 bytes |
| Free gap before reserved stack | 133,624 bytes |
| Reserved stack | 32,768 bytes |
| Static share below stack reserve | 93.31% |

This shows real RAM pressure, but overlays do not shorten any measured frame
stage; they trade loading/relocation complexity for reclaimable setup or
mutually-exclusive code/data memory.

**Verdict: not a performance win.** Revisit as a RAM-capacity feature if the
remaining ~130.5 KiB gap becomes the blocker.

Raw evidence: `/tmp/tr5-exp19-link.map`.

## Validated existing findings 10–15 and 17

These findings already have production implementations. They were tested with
focused suites plus the adopted-lattice Cortex route rather than replaced by
second implementations that would only restate the same algorithm.

### 10 — portal rectangles and bounded traversal

`cargo test -p psx-level portal` passes 27 focused tests covering recursive
horizontal/vertical clipping, multiple frustums into one room, near/far
boundaries, backfaces, and offscreen rejection. The Cortex route performs two
portal tests per gameplay render, retains one visible room/frustum, and draws
one room. Portal traversal is correct and far below the room submit cost.

**Verdict: net positive existing path.** Keep the clipped traversal; it is not
the present bottleneck.

### 11 — roomlet/cell bounding rejection

The engine suite passes the sphere/AABB conservative-bound and widened-
reference equivalence tests. On every matched Cortex gameplay render the
populated-cell layer considers 79 cells, rejects 62, and draws 17: a 78.5%
rejection rate before surface submission.

**Verdict: net positive existing path.** An additional hierarchy over only 17
accepted cells is not indicated.

### 12 — unique projection and RTPT scheduling

The `contiguous_gte_projection_matches_ordered_index_projection`,
`tomb_raider_identity_rtpt_matches_world_projection`, and fixed-lattice packet
equivalence tests pass. Cortex projects 38 unique authored room vertices for
23 four-corner surfaces, versus a 92-input non-indexed upper bound: 54 fewer
inputs (58.7%). Its whole GTE estimate remains about 0.5–0.6% of the render
budget.

**Verdict: unique projection is net positive; generic extra RTPT batching is
rejected.** The fixed nine-point tessellation lattice was worthwhile because
it removed repeated CPU/packet work, not because the GTE is saturated.

### 13 — packed streams and direct/bucketed OT insertion

The adopted Cortex run decodes 568 packed model faces per gameplay render,
submits 266.39 through the specialized fast path, and records zero fallback
faces. Six `psx-gpu` OT tests pass, including packed reverse insertion order,
multi-slot DMA order, and duplicate-chain termination.

**Verdict: net positive existing path.** There is no measured case for another
assembly rewrite.

### 14 — fixed arenas and overflow behavior

The primitive arena capacity, reuse, and relink tests pass. Cortex retains a
mean 1,176 packet slots after gameplay renders and reports zero room primitive
overflows. The candidate tessellation experiments also preserved zero
overflows.

**Verdict: net positive safety architecture.** Fixed capacity remains
predictable and has ample headroom; no dynamic allocation is warranted.

### 15 — baked and once-per-object lighting

The seven lighting accumulation tests and prebuilt static-room packet tests
pass. The detailed room profile attributes only 8,767 cycles (1.6%) to room
lighting, while submission was 448,790 cycles (81.6%). Placed-model code calls
`shade_model_material` once per instance/layer before face submission.

**Verdict: net positive existing path.** More blanket baking is not useful;
the remaining scaling experiment is finding 5's room-range/top-three dynamic
light selection.

### 17 — async OT submission versus another frame buffer

The OT chain/order tests pass and the current path kicks submission before the
present-stage drain. On the adopted Cortex build, OT submit averages 96 cycles
and GPU/DMA wait averages 54.6 cycles (63 maximum) per gameplay render. Even
eliminating all wait would recover only 0.005% of the two-VBlank budget. A
second completed render frame would add roughly one 30 Hz frame (~33 ms) of
input latency.

**Verdict: current single-frame asynchronous overlap is net positive; a full
extra polygon/render buffer is net negative for this workload.**

### 20 — streaming and residency

Four paged-room tests pass, covering exact sector charging, fragmented-pool
compaction with live-byte preservation, stale-handle invalidation, and
cross-page reads/writes. Three packed-asset streamer tests pass, covering
in-place idempotent startup, capacity rejection without aliasing, and stable
word-aligned storage.

The Cortex tape services 2,457 room requests with 2,106 prefetches, 136 misses,
29 CD chunk loads, five chunk hits, and up to seven resident slots without a
failed-load counter. The cooked level is eight rooms and 54,220 bytes of room
payload, but the runtime keeps only the active/resident working set pinned.

**Verdict: net positive existing path.** Replacing this with whole-level
residency would consume more RAM and remove prefetch/eviction capability
without improving the measured room submission bottleneck.

Evidence:

- `cargo test -p psx-level portal` — 27/27 passed.
- `cargo test -p psx-game-runtime` — 61/61 passed.
- `cargo test -p psx-engine --features tr-subdivision-lattice` — 263/263
  passed plus the time-type compile test.
- `cargo test -p psx-gpu ot` — 6/6 passed.
- `cargo test -p psx-game-runtime --features cd-stream-bench room_streaming`
  — 4/4 passed.
- `cargo test -p psx-game-runtime --features cd-stream-bench asset_streaming`
  — 3/3 passed.
- `/tmp/tr5-exp21-30hz-profile.csv`
- `/tmp/tr5-exp21-30hz.log`

## Experiment 16 — single-quad actor shadows

The reference feature `actor-shadows-off` removes player and placed-instance
shadow draws while leaving every model pass intact. Against the normal
shadowed build, 214 common gameplay renders measured:

| Metric | Single-quad shadows | Shadows off | Shadow cost |
|---|---:|---:|---:|
| Render stage | 894,813 | 878,374 | 16,439 (+1.84%) |
| Model-instance stage | 37,362 | 29,722 | 7,641 |
| Player stage | 202,396 | 192,634 | 9,762 |
| Primitives | 359.88 | 356.39 | 3.49 |
| World commands | 359.88 | 356.39 | 3.49 |

Both variants produce 402 visual frames and 45 deadline misses. The image hash
changes as expected because shadows are intentionally visible.

**Verdict: retain the current simplification.** Shadows are not free, but one
textured quad per actor costs only 1.84% of the render stage and does not change
the route's deadline result. A multi-segment TR-style projected silhouette
would add work without solving a measured visual defect.

Raw evidence:

- `/tmp/tr5-exp21-30hz-profile.csv`
- `/tmp/tr5-exp16-no-shadows-profile.csv`
- `/tmp/tr5-exp16-no-shadows.log`

## Final audit

The restored normal feature set (`cd-stream-bench emulator-telemetry`, with
the adopted lattice supplied by the editor-playtest defaults) completed the
900-frame Cortex v1 route:

| Gate | Final result |
|---|---:|
| Display hash | `0xb6fd12f53e6e1165` |
| Visual frames | 402 |
| Deadline misses | 45 |
| Render cycles / visual | 916,548 |
| Primitive/command overflows | 0 / 0 |
| PSX executable payload | 849,920 bytes |

Regression results:

- `cargo test -p psx-engine -p psx-game-runtime -p psx-level`: passed,
  including 262 normal engine tests, 62 runtime unit tests, five TR5 policy
  experiments, the level suites, and compile/doc tests.
- `cargo test -p psx-gpu ot`: 6/6 passed.
- `cargo test -p psxed-project`: 361 passed, one ignored, one failed. The
  failure is `cortex_project_deserializes_authored_enemy_combat_profile`,
  because its tracked fixture
  `editor/projects/cortex_ignition_v1/project.ron` is deleted in the existing
  user worktree. The performance work did not restore or alter that unrelated
  deletion.
- `git diff --check`: clean.

Final runtime evidence:

- `/tmp/tr5-final-normal-profile.csv`
- `/tmp/tr5-final-normal-counters.csv`
- `/tmp/tr5-final-normal.log`
- `/tmp/tr5-final-normal.ppm`
