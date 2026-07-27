# cortex_v1 / cortex_v3 30 FPS experiment ledger

Target: both projects sustain one visual frame per two NTSC VBlanks on original
PlayStation hardware, without changing authored geometry, visibility, draw
distance, textures, lighting, fog, subdivision topology, or packet ordering.

Only full poll-bound tape results with guest-frame-aligned visual hashes may
advance the baseline. Diagnostic builds are never used for absolute performance
claims.

## Frozen baseline

- Source: `448d8d23` on `emu/accuracy-from-silicon`.
- Frontend SHA-256:
  `c425ccd33d0bebdd98499d50412d9d51f2ee9c3655a73dbf0bc6737153223939`.
- cortex_v1 tape: PXITAPE2, 2,026 samples from poll 72, SHA-256
  `a8d9dd3b235c9a6cdc539b0b341282e3e7b9e1fa55afb7e7d980c9c0cf6194fc`.
- cortex_v1 disc SHA-256:
  `5e334f602b9be9b051bd8b5e0c3d193c31b2c56e63d533acb4038645564e04c8`.
- cortex_v3 tape: PXITAPE2, 1,643 samples from poll 214, SHA-256
  `ffce0f7a6451a1f269d45f2dd82b88d836c32e2d9d7387368bfb4e225e3c5ea4`.
- cortex_v3 disc SHA-256:
  `7ac605124a4ef99c164a96c6db548e5f63f920cd2c35515c5b5db0deda8259b4`.
- Toolchain: `rustc 1.96.0-nightly (362211dc2 2026-03-24)`, LLVM 22.1.0.

Gameplay begins at the first visual frame with both an active room chunk and a
considered room surface. `period <=2vb` groups every 60 Hz update since the
previous visual with the current render and compares their CPU work to
1,128,960 cycles. It excludes the `present` VBlank-edge wait.

| Project | Effective FPS | Visuals / gameplay ticks | Render mean | Render p95 | Render max | Periods <=2 VBlanks | Surfaces / visual | Primitives / visual |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| cortex_v1 | 25.54 | 725 / 1,703 | 764,652 | 1,092,343 | 1,252,038 | 69.8% | 39.8 | 294.6 |
| cortex_v3 | 14.03 | 384 / 1,642 | 1,404,382 | 1,994,267 | 2,263,382 | 0.3% | 88.9 | 412.3 |

## Accepted measurement infrastructure

### M0: deterministic two-tick visual gate

`lockstep-visuals` caps catch-up at the configured two-tick visual interval.
The guest therefore renders the same simulation states even when two builds
take different wall-clock time. This mode is for visual equivalence only;
ordinary uncapped builds remain the performance truth.

Two independent full runs of each lockstep binary produced:

| Project | Scheduled gameplay cadence | Visual hashes | Final display | Final VRAM |
|---|---:|---:|---|---|
| cortex_v1 | 29.98 FPS (851 / 1,703) | 1,047 / 1,047 exact | exact | exact |
| cortex_v3 | 30.00 FPS (821 / 1,642) | 927 / 927 exact | exact | exact |

The ordinary cortex_v3 disc rebuilt after adding the feature-gated
instrumentation is byte-identical to the frozen baseline
(`7ac60512...259b4`), proving zero performance-build code drift.

### M1: direct TR-subdivision entry counters

The previous CSV counted authored whole quads, not surfaces entering TR
subdivision. New diagnostic-only counters measure the exact predicate and
successful submission.

| Project | Considered surfaces / visual | TR candidates / visual | TR submitted / visual | Result |
|---|---:|---:|---:|---|
| cortex_v1 | 39.0 | 14.4 | 6.8 | many candidates demote/fall back |
| cortex_v3 | 88.3 | 18.0 | 18.0 | only 20% of considered surfaces enter TR |

This rejects the review estimate that roughly 80 of 88.6 cortex_v3 surfaces
subdivide. The ~413 room primitives per visual are mainly amplified elsewhere,
so precomputed TR lattice attributes remain worth testing but are not the first
architectural lever.

### M2: emulator-owned instruction-cache refill counters

The headless route log now reports refill events, words, and the exact stall
cycles the emulator already charges for cached instruction fetches. The
counters are host-only diagnostic history: they are excluded from save states
and cannot affect guest state or timing.

Full replays of the frozen, unmodified discs reproduced every visual hash and
the final display/VRAM byte-for-byte:

| Project | I-cache stall cycles / visual | Share of mean render | Stall cycles / considered surface |
|---|---:|---:|---:|
| cortex_v1 | 176,124 | 23.0% | 4,425 |
| cortex_v3 | 388,056 | 27.6% | 4,365 |

The absolute v3 cost is large enough to justify code-layout experiments, but
the nearly identical per-surface cost argues against a special v3-only cache
pathology. Most of the difference currently tracks v3 doing 2.23x as much
surface work. Candidate R2 must therefore demonstrate a full-replay reduction;
body size or disassembly appearance alone is not evidence.

### M3: room-only packet and command boundaries

Diagnostic builds now count the primitive-arena packets and world commands
emitted strictly inside room-surface draws. Water, props, models, and actors
are outside the boundary. This resolves the ambiguity in the global GPU and
packet-arena counters without changing normal builds.

The cortex_v3 diagnostic replay measured 88.5 whole-quad surface records, 18.0
successful TR subdivisions, 131.9 arena packets, and 169.8 room commands per
visual. The packet arena count is lower because warmed whole quads live in the
persistent prebuilt pool; the command count includes both storage sources.
Thus TR subdivision applies to only 20% of surfaces but still accounts for
about 90 of 170 room commands (four child quads plus one underdraw per
subdivided surface). Template/lattice work remains relevant, but its honest
upper bound is roughly half the room command stream rather than nearly all
surfaces.

### M4: tolerate legacy short initial route rows

The report tool now treats a missing trailing field in the initial route row
as its counter's zero value. Older frozen logs were written before
`port1_polls` was present on that row, and Python's `DictReader` exposes the
missing value as `None`. This is a parser-only compatibility fix; it does not
change any measured row or guest execution.

### M5: exact lockstep scheduling and host-side OT/DMA tracing

The original `lockstep-visuals` implementation only capped the number of
catch-up updates before a visual. A slow and a fast binary could still present
different simulation ticks after accumulated wall-clock backlog, which made a
single transient hash difference look like missing geometry. The diagnostic
mode now waits until exactly one configured visual interval of fixed updates
has completed before rendering. Ordinary builds retain the deadline-driven
scheduler.

The emulator can also record every linked-list GPU DMA node as
`(transfer,address,header)` through `PSOXIDE_DUMP_DMA_LL`. This observes the
actual ordering table without changing guest code generation. Rebuilt
like-for-like cortex_v1 control and candidate discs each emitted 1,047 visual
checkpoints and traversed exactly 2,260,381 DMA nodes.

### M6: shipping-style build without guest telemetry

The benchmark feature set includes `emulator-telemetry`, while the hardware
feature set does not. To test whether stage/counter emission materially
distorted the target, both projects were rebuilt from R10 without that feature
and measured solely through emulator-owned display flips, I-cache counters,
and GPU command logs. Gameplay boundaries were aligned by poll-bound tape
frame, not guest telemetry.

| Project | Instrumented FPS | No-telemetry FPS | I-cache stalls / visual | Final display |
|---|---:|---:|---:|---|
| cortex_v1 | 27.004 | 26.898 | 170,679→167,971 | exact |
| cortex_v3 | 15.713 | 15.713 | 346,461→350,047 | exact |

Removing every guest stage marker and counter did not recover a single
cortex_v3 visual and slightly hurt cortex_v1 through code-layout changes. The
30 FPS gap is therefore real engine work, not profiler overhead. Moving the
counter block behind the GPU kick is demoted: it cannot be a production win
because the entire block is already absent from production builds.

### M7: warmed room-surface micro-profile and workload-density check

The surface micro-profile was replayed first in its original diagnostic form,
which deliberately disables the warmed packet path, then again with that path
temporarily enabled. Both are instrumentation builds and are used only for
proportions. The warmed-path cortex_v3 run measured:

| Counter | Cycles / visual |
|---|---:|
| `room_surface_draw` | 917,896 |
| projection gather/validity | 50,725 |
| screen bounds | 7,874 |
| kind decode | 6,945 |
| material lookup/animation | 30,459 |
| backface | 13,778 |
| lighting | 62,782 |
| instrumented submission leaves | 369,391 |

Kind plus material—the main static interpretation a residency record would
remove—account for only 37,404 cycles in this deliberately slowed build.
Submission is nearly 10x larger, and the unassigned remainder includes
option/risk decisions, TR reprojection, branches, and profiler overhead. This
falsifies the reviews' predicted 200–320k saving from a surface record by
itself. A record may still help a genuinely smaller submit architecture, but
static classification is not the main missing budget.

The ordinary R10 tapes also rule out a portal loop. cortex_v1 and cortex_v3
draw almost the same number of cells (23.7 versus 25.4 per visual), while v3
contains 88.9 considered quads versus 40.1. That is 3.50 surfaces per drawn
cell in v3 versus 1.69 in v1. v3 screen-rejects 20.7 and backface-rejects 6.1
of those quads; the remaining density is authored visible geometry. The same
engine is doing roughly twice the surface work, not revisiting rooms through a
portal cycle.

## Accepted engine changes

### V0: reuse the cell vertical AABB extent

The visible-cell loop previously derived the same `half_y` extent separately
for the frustum and portal-window tests. It now retains the first result and
reuses it when both tests run, while preserving the lazy calculation on paths
that need only one test.

| Project | FPS | Mean render | p95 render | I-cache stalls / visual | `room_cell_select` / visual | Visual proof |
|---|---:|---:|---:|---:|---:|---|
| cortex_v1 | 25.54→25.61 | 764,652→763,256 | 1,092,343→1,086,879 | 176,124→174,895 | 56,349→55,105 | 1,047 / 1,047 exact; display and VRAM exact |
| cortex_v3 | 14.03→14.07 | 1,404,382→1,401,072 | 1,994,267→1,989,066 | 388,056→386,202 | 49,706→48,221 | 927 / 927 exact; display and VRAM exact |

Surface, primitive, and visible-cell counts are unchanged. This is a small
engine-wide win rather than the main answer, but it meets the strict visual
gate on both projects and reduces the targeted cell-selection work.

### V4: exact cylinder-prop UV edge interpolation

Cylinder/card props evaluate authored quad UVs at runtime. Most generated
vertices lie on `u` or `v` edges, where the full four-corner bilinear equation
reduces exactly to a two-corner interpolation. The runtime now takes those
four endpoint paths while retaining the original equation for interior
samples. An exhaustive host test compares all 256 positions on all four edges
across varied corner sets against the original implementation.

| Project | FPS | Mean render | I-cache stalls / visual | Visual proof |
|---|---:|---:|---:|---|
| cortex_v1 | 25.61→25.58 | 763,256→763,475 | 174,895→172,802 | 1,047 / 1,047 exact; identical DMA-node count |
| cortex_v3 | 14.07→14.25 | 1,401,072→1,394,252 | 386,202→374,342 | 925 / 925 exact; display and VRAM exact |

The v1 render delta is within replay noise, while v3 saves 6,820 mean render
cycles and 11,860 I-cache stall cycles per visual. This is retained as an
engine-wide, data-layout-preserving win.

## Rejected engine changes

### R1 retest: zero-weight fog bypass

The earlier zero-fog idea was retested in five shapes against the newer exact
gate. The mathematically safe form only bypassed four fog blends when all four
prepared fog weights were zero; it never changed packet topology, geometry,
ordering, or subdivision. The best inline v3 result was 14.25→14.40 FPS,
mean render 1,394,252→1,374,463 cycles, and I-cache stalls
374,342→367,764 per visual. Its lockstep v3 run matched 925/925 hashes.

That form perturbed one v1 presentation boundary. Moving the test into
`RuntimeRoomLighting`, trying a four-compare test and a single-OR test, and
forcing the fog shader out of line could each make either project's full tape
exact, but never both simultaneously. The out-of-line form that was exact in
v1 saved only 9,949 v3 render cycles and changed 1/925 v3 lockstep hashes; the
compact inline form that was exact in v3 changed 2/1,046 v1 hashes. Final VRAM
and display images were identical in every variant, but the full-route
guest-frame gate intentionally rejects transient differences too. All R1
variants were removed.

### R3a: borrow `WorldSurfaceOptions` in the per-surface loop

Replacing the by-value `WorldSurfaceOptions` argument to
`draw_indexed_cached_room_surface` with a reference was exact across 1,047
cortex_v1 and 926 cortex_v3 lockstep checkpoints. It is nevertheless a severe
cross-project regression:

| Project | FPS | Mean render | I-cache stalls / visual |
|---|---:|---:|---:|
| cortex_v1 | 25.58→22.15 | 763,475→877,235 | 172,802→211,058 |
| cortex_v3 | 14.25→14.29 | 1,394,252→1,386,410 | 374,342→372,001 |

The dependent loads/register pressure improve the v3 mix slightly while
expanding v1's hot working set enough to lose 3.43 FPS. The borrow-only change
was removed. A future surface-record experiment must reduce interpretation as
a whole; it cannot assume that replacing a large value argument with a pointer
is cheaper on MIPS-I.

### R3b: residency-cached surface class bits

The prebuilt-quad residency pool was widened by one byte per surface and used
to cache surface kind plus animated/translucent material classification. The
draw path consumed those bits while leaving geometry, material resolution,
subdivision, packet construction, and ordering unchanged.

All 925 comparable cortex_v3 lockstep hashes matched. The normal tape,
however, showed no useful reduction: FPS stayed 14.25, mean render moved
1,394,252→1,395,491 cycles, and I-cache stalls moved
374,342→372,657 per visual. The lockstep render mean moved only
1,451,868→1,451,511 cycles. This prices the cached decisions at noise while
adding 2 KiB to the eight-slot pool, so the change was removed. A complete
`SurfaceDrawRecord` must eliminate substantially more interpretation than
kind and two material predicates to justify its data traffic.

### R3c: six-entry per-cell option table

The exact `(floor|ceiling|wall) × (calm|risky)` option results and prepared
depths were built once per accepted cell, replacing per-surface calls to the
depth/subdivision option constructors. All 36 focused renderer tests passed,
but the larger stack record and indexed loads are substantially worse on
MIPS-I. The cortex_v1 lockstep tape moved from 791,057 to 849,858 mean render
cycles, p95 from 1,124,007 to 1,189,537, and I-cache stalls from 171,364 to
186,976 per visual; one of 1,046 hashes also changed. The table was removed
without spending a cortex_v3 run. Any eventual compiled record must encode
the few decisions compactly in the record itself, not indirect through a
copied `WorldSurfaceOptions` table.

### R3 disposition: do not build the proposed full record pool

The warmed surface profile falsifies the proposal's stated cost model.
`cached_surface_kind` plus material resolution cost 37,404 diagnostic cycles,
while packet/subdivision submission cost 369,391 of the 917,896-cycle room
surface stage. The three independently shippable parts of the proposed
`SurfaceDrawRecord` were then measured: option borrowing catastrophically
regressed cortex_v1 (R3a), residency-cached classification added RAM for no
speedup (R3b), and the option-variant table regressed render by 58,801 cycles
(R3c). Outlining the leaves separately also regressed 4.3% (R2).

Building the full 16-byte record and 32-KiB fixed pool would combine four
already-failed mechanisms while leaving the measured dominant submission
work intact. R3 is therefore rejected by decomposition rather than landing a
larger version of the same data traffic. The record idea may be revisited only
if a new representation removes packet/subdivision work, not merely static
interpretation.

### R7 disposition: cooked render clusters are rejected by constituents

The proposed cluster architecture is the union of five separable mechanisms:
outlined leaves (R2), static surface records/options (R3), cooked lattice
attributes (R4/R4a), packet copy/patch templates (R5), and offline
subdivision demotion (R6). Each mechanism was tested independently so its
effect could not be hidden by a large rewrite. The exact variants either
regressed or compiled to the existing behavior; the only large cycle win,
offline/GTE demotion, lacked the camera-volume proof and changed hundreds of
images.

There is therefore no measured positive constituent to justify adding the
proposed 57–90 KiB fixed arenas and another streamed representation. R7 is
closed by decomposition. A future cluster format needs a new primitive
submission algorithm with an independently measured win; repackaging the
rejected R2–R6 mechanisms together is not an experiment.

### R10a: defer the forced-split options copy

After R10, the largest named `memcpy` caller was a 68-byte
`WorldSurfaceOptions` clone before projected-quad hardware-extent checks. A
lazy form evaluated the identical safety predicate from the scalar edge
threshold and constructed the forced-split options only on the fallback.
Disassembly confirmed that the copy moved off the common accepted-quad path.

It helped cortex_v3 lockstep by 6,610 render cycles but hurt cortex_v1 by
856 cycles, increased its I-cache stalls by 1,002, and changed one of 1,044
common visual checkpoints (with two cadence-set replacements). Its ordinary
cortex_v1 run also fell 26.99→26.78 FPS. The change was removed; this is
another measured instance where adding a branch/code-layout change around a
large by-value options object is not an engine-wide win on the 4 KiB
direct-mapped instruction cache.

### R11/R11b: avoid the full portal-result reset

The second-largest named post-R10 `memcpy` caller was
`PortalVisibilityResult::clear`: every camera refresh copied the complete
fixed-capacity `EMPTY` result, including 64 frustum and 32 frontier slots.
R11 reset only counts and statistics, relying on every declared reader being
count-bounded. It changed two cortex_v1 lockstep images, proving that this
logical contract is not strong enough for an exact engine change.

R11b restored every previously exposed prefix slot to its exact sentinel
before zeroing the counts, preserving the original full-pool invariant because
all never-used tails begin empty. It saved 2,072 mean render cycles and 2,651
I-cache stall cycles in cortex_v1 lockstep, but still changed one of 1,046
common images. Both forms were removed. The portal result keeps its full reset
until the implicit state/layout sensitivity is isolated separately.

### R12/R12b: leave camera duplicate-set overflow storage uninitialized

Four adjacent `memcpy` callsites in the camera solve were the construction of
four 356-byte `CheckedCameraCells` values. Each copied a 72-entry
`u32::MAX` fallback array even though its `len` began at zero. R12 changed the
entries to `MaybeUninit`; LLVM replaced the four reads/copies with one
1,424-byte zero fill. R12b separated that logically uninitialized overflow
storage from the initialized bitset/count, reducing the generated
initialization to a single 272-byte zero fill and shrinking
`update_vblanks_with_collision_rooms` by 228 bytes.

The optimized form preserves duplicate detection for every key and passed all
25 focused camera tests. It saved 2,739 camera cycles per gameplay tick in
cortex_v1 lockstep and 2,520 in the ordinary tape. However, the new layout
added 1,797 I-cache stall cycles per visual in lockstep and 1,976 in the
ordinary run. Effective FPS remained 26.99, two-vblank periods slipped
79.2%→79.0%, and one presentation checkpoint moved (all 1,045 common images
were exact). With no net engine gain, both forms were removed before spending
a cortex_v3 run.

### R13: const-specialize cached-room depth/subdivision policies

The cached-room entry points were temporarily made const-generic over the
generated depth and subdivision modes. This forced a project-specific MIPS
monomorph and should have removed every policy branch if the existing enum
arguments were blocking constant propagation.

The cortex_v3 lockstep replay was identical to R10 in every reported
performance metric: mean render 1,416,120, p95 2,002,178, max 2,259,241, and
321,902 I-cache stall cycles per visual. All 925 display hashes matched. LLVM
was already propagating these constants through the shared engine; the API
change generated the same machine behaviour and was removed.

### R14/R14b: accelerate TR root world-to-view transforms

R14 moved the three/four root transforms before TR subdivision from the CPU to
the otherwise-idle GTE. It exposed genuine arithmetic headroom: cortex_v3
lockstep mean render fell 40,675 cycles and p95 fell 46,225. It is not
visually valid. The GTE combines yaw and pitch in one matrix operation while
the established path rounds after yaw and again after pitch. That difference
changed 820 of 925 display hashes and reduced the primitive mean from 418.2 to
376.3, so the experiment was immediately removed.

R14b retained the exact CPU arithmetic and batched three/four vertices behind
one shared camera-basis load. All 925 hashes matched and lockstep render saved
599 cycles, but the ordinary v3 replay regressed 3,966 mean render cycles,
p95 regressed 10,225, and two-vblank coverage fell 0.9%→0.5%. LLVM already
hoists enough of the original calls; the explicit batch's layout is worse in
the production path. It too was removed.

### R15: explicitly outline the TR room entry leaves

Both cached-room TR entry functions were marked `#[inline(never)]` to test a
narrower version of the reviews' code-layout proposal without disturbing the
ordinary warm path. The complete cortex_v3 lockstep capture was exactly
identical to R10—every timing percentile and I-cache count, plus all 925
hashes. LLVM already keeps these large generic entries out of the caller, so
the attributes were removed as redundant.

### R6: offline whole-subdivision proof is not currently sound

The proposed `WHOLE` classification requires a conservative minimum
camera-to-surface distance over every legal camera position. The engine's
current content contract does not provide that volume to the cooker: free
orbit and collision-bypassing camera modes exist, and rooms without a complete
camera collision hull are valid. Under the proposal's own fail-open rule,
every surface without such a proof remains `SPLIT`; classifying any of them
`WHOLE` would change the subdivision topology for a legal view.

Consequently the current engine-wide form is a no-op if correct and a visual
regression if made aggressive. It is rejected until projects can opt into a
cooked, enforced camera-volume contract. The existing runtime projected-edge
test remains authoritative.

### T1a: defer fallback material-array clear

`apply_current_active_room_fields` previously cleared the full current-room
material array before scanning the active window and copying the resident
room's real materials over it. Moving that clear to the actual missing-room
case preserves state, but this call is not frequent enough on the measured
steady-state path: cortex_v1 update mean improved only about 105 cycles.
Meanwhile code-layout/I-cache effects raised render mean 791,057→794,762,
raised I-cache stalls 171,364→174,890, and shifted one lockstep presentation
boundary. The reorder was removed. T1 must instead eliminate repeated
residency/material copies at their callers, guarded by explicit dirty state.

### T1b: skip settled residency reconciliation

The residency owner retained the ordered desired set plus its visible/pinned
prefix and skipped scheduler reconciliation only when both were unchanged and
every requested room was resident or already loading. The CD pump, VRAM
eviction pass, and persistent-asset requests remained active, so no streaming
progress was delayed. This saved 2,397 update cycles/tick and 2,342
`sim_residency` cycles/tick on cortex_v1, but render mean rose 5,714 cycles,
I-cache stalls rose 5,116 per visual, and 1/1,046 lockstep hashes changed.
The dirty state and fast path were removed. This proves repeated scheduler
planning is real but too small to justify more hot-code footprint in this
layout.

### T1c: decode only the selected collision floor triangle

Out-of-band return-address sampling resolved the previously opaque
compiler-builtins `memcpy` cost. Two copies inside
`RoomCollision::sector` accounted for 37.8% of all sampled `memcpy`
instructions: collision was decoding and returning a 152-byte render-rich
`WorldSector`, including materials, UVs, ceiling data, and both split
triangles, even when the character or camera needed one floor triangle.

The accepted path decodes only the selected floor triangle's flags and
heights. It applies the same horizontal override records and interpolation
inputs as the full decoder. Renderer data, portal state, draw order, and
packet emission are untouched.

| Project | FPS | Update mean | Camera mean | Render p95 | I-cache / visual |
|---|---:|---:|---:|---:|---:|
| cortex_v1 | 25.58→25.97 | 143,171→127,681 | 37,433→35,481 | 1,098,246→1,096,729 | 172,802→174,334 |
| cortex_v3 | 14.25→15.09 | 159,415→137,814 | 42,157→40,851 | 1,996,668→1,977,978 | 374,342→357,343 |

All 925 cortex_v3 lockstep hashes matched. In cortex_v1, all 1,045 common
checkpoints matched; one old checkpoint was replaced by one newly rendered
checkpoint when the faster update crossed a presentation deadline. There
were no mismatching images. The normal tapes rendered 11 additional v1
frames and 23 additional v3 frames. Focused character, camera, and asset
tests also pass.

### T1d: use the header-only sector probe for character walls

Character cylinder wall checks decoded the full sector solely to read its wall
range. The camera path already had a collision-only probe containing exactly
`has_floor`, `first_wall`, and `wall_count`; the character path now uses the
same probe and probe-wall accessor.

All 925 cortex_v3 common lockstep hashes and all 1,045 cortex_v1 common
lockstep hashes match. As with T1c, cortex_v1 replaced one presentation
checkpoint after crossing a deadline, with no mismatching common image.

| Project | FPS | Update mean | Two-vblank periods | I-cache / visual |
|---|---:|---:|---:|---:|
| cortex_v1 | 25.97→26.28 | 128,321→118,072 (lockstep) | 71.4%→73.9% | 174,334→171,976 |
| cortex_v3 | 15.09→15.38 | 138,437→125,478 (lockstep) | 0.2%→0.5% | 357,343→348,665 |

### R10: borrow the world pass at flush

Return-address profiling resolved the largest remaining `memcpy` caller to
`Playtest::render`: consuming `WorldRenderPass<2048>` by value at
`flush(self)` made LLVM copy the entire 8,212-byte pass into a second stack
slot. The payload was almost entirely its two 2,048-entry ordering arrays.
`flush(&mut self)` removes that copy without changing the pass, command,
packet, or ordering-table representation. MIPS disassembly confirms the
8,212-byte `memcpy` is gone and the renderer body shrank by 12 bytes.

| Project | FPS | Mean render | p95 render | Render periods <=2 VBlanks | Visual proof |
|---|---:|---:|---:|---:|---|
| cortex_v1 | 26.28→26.99 | 770,832→742,800 | 1,100,178→1,069,593 | 73.9%→79.2% | 1,046 / 1,046 exact |
| cortex_v3 | 15.38→15.71 | 1,394,974→1,364,086 | 1,979,163→1,953,348 | 0.5%→0.7% | 925 / 925 exact |

The controlled lockstep deltas are nearly project-independent:
29,735 cycles per visual in cortex_v1 and 29,725 in cortex_v3. This is an
engine-wide structural win rather than a workload-specific shortcut.

### V1: carry portal window and far plane into the all-cells fallback

The no-PVS/all-cells path was given the same conservative portal-window AABB
test and far plane as the PVS path, while root/overlap rooms without an
incoming aperture retained the existing fail-open no-far behaviour. All 926
cortex_v3 lockstep hashes matched, but the tape exposed no usable window in
the fallback rooms: considered surfaces were unchanged (88.6→88.7), mean
render moved only 55 cycles, and FPS remained 14.25. The extra runtime branch
was removed. A useful portal refinement must preserve each admitting wedge
rather than relying on information absent from the fallback.

### V2: reject cells outside every individual portal wedge

The visible-cell path kept its component-wise portal-window union as a broad
test, then required a surviving cell to intersect at least one individual
admitting portal wedge. Scratch capacity matched the 64-frustum visibility
result and overflow was explicitly fail-open. A focused host test confirmed
the intended disjoint-window OR semantics.

The refinement is not conservative with respect to the current room-cell
representation: it changed 61 of 926 cortex_v3 lockstep display hashes,
beginning at guest frame 698, which is direct missing-geometry evidence. It
also made the normal run slower:

| FPS | Mean render | p95 render | I-cache stalls / visual |
|---:|---:|---:|---:|
| 14.25→14.03 | 1,394,252→1,413,654 | 1,996,668→2,017,582 | 374,342→383,976 |

The per-wedge scan was removed. The conservative union remains mandatory
until a cooked cell mask can prove coverage against actual submitted
geometry, not only a cell AABB and one clipped path.

### V5: use the existing coarse-yaw visible-cell cache mode

The default `vis-anchor-pvs-candidates` build caches the cooked PVS candidate
set and performs the conservative camera/portal AABB checks during each draw.
The already-implemented `vis-coarse-yaw` alternative was replayed unchanged to
test whether an apparently unapplied visibility option could cache a tighter,
pre-culled list.

All 925 cortex_v3 lockstep hashes matched, but the actual workload did not
change: 47.3 candidate cells, 26.6 drawn cells, and 93.3 surfaces per visual in
both builds. The ordinary tape regressed mean render by 1,159 cycles and p95 by
3,659 while gaining one cadence-quantized visual. The conservative blocker and
safety rules retain the same cells, so this mode does not solve the density
problem and is not made the default. The finer-yaw form (neither anchor nor
coarse-yaw feature) was also replayed; it produced metrics exactly identical
to R10 and the same 925 hashes.

### V3: cooked variable-length portal-to-cell masks

A cooker-side existential interval sweep projected each complete source-room
camera volume through every directed portal toward destination cached-cell
AABBs. The complete candidate used variable-length destination masks, outward
slack, a four-sector camera-volume expansion, multi-path OR at runtime, and
fail-open handling for root, overlap, capped, or malformed paths. The existing
PVS, camera frustum, and portal rectangle remained downstream. cortex_v3's 12
directed portals retained 211/225 portal/cell pairs; cortex_v1 retained
370/409. The masks were therefore only 6.2% and 9.5% selective before
multi-path union.

The cooker/schema/runtime candidate passed all 310 `psx-level` and
`psx-engine` tests but failed the replay gate. cortex_v3 lockstep surfaces
moved only 93.3→93.1 per visual, while mean render regressed
1,416,120→1,422,386 cycles, p95 regressed 2,002,178→2,014,407, and I-cache
stalls rose 321,902→326,242. Thirteen of 925 images changed beginning at guest
frame 926. The current engine has no formal camera-volume contract strong
enough to prove those rejected cells unreachable, so the entire format and
runtime path were removed.

### G1: GPU timing and texture-window command census

The normal R10 tapes show that the GPU is not the component missing the
two-vblank target. Across gameplay visuals, cortex_v1 averages 621,664 GPU
cycles and peaks at 928,430; cortex_v3 averages 713,238 and peaks at 878,670.
The CPU render stage alone averages 742,800 and 1,364,086 cycles respectively,
and the measured ordering-table wait is negligible. The GPU therefore finishes
inside the CPU critical path even in cortex_v3.

Texture-window commands are highly redundant in stream order, but too small to
matter at this scale:

| Project | Commands | Draws | Texture-window writes | Window changes | Redundant writes |
|---|---:|---:|---:|---:|---:|
| cortex_v1 | 691.2 | 350.2 | 337.0 | 19.4 | 317.7 |
| cortex_v3 | 1,045.7 | 505.6 | 498.3 | 102.7 | 395.6 |

Each redundant window is one GP0 data word attached to a packet. Even a
zero-cost, final-order-aware suppression mechanism could remove only about 396
words per cortex_v3 visual, while it would complicate the packet header and
ordering-table ownership needed to prevent texture-state leakage. It cannot
recover the roughly 350k CPU cycles still needed for a stable two-vblank
cortex_v3 frame. Texture-window dedup is rejected by this hard upper bound;
future GPU work is gated on contrary real-hardware evidence.

### E8 diagnostic: decompose the prop-rendering tail

The outer `image_props` stage is an aggregate over box, cylinder, arch,
debris, shard, and image-card rendering. A diagnostic build with the existing
`PSXO_PROFILE_BOX_PROPS=1` switch separates the named sub-stages. On the
current cortex_v3 tape its 131,932-cycle mean comprises 40,406 cycles of box
props, 1,873 of floor debris, 1,322 of shards, and only **5 cycles** of image
cards. The cooked level has zero image-card and arch records, one box prop
expanded to 34 surfaces, and six cylinder props expanded to 120 surfaces.
The approximately 88k-cycle remainder is therefore the cylinder path.

This falsifies the proposed image-card record/template optimization for this
workload. The actionable prop target is cooked cylinder submission; its UV
interpolation was already reduced by V4, while the fully cooked-UV R9 variant
failed the strict painter/hash gate.

### R16: cache exact CPU view vertices across subdivided surfaces

The indexed renderer projects shared world vertices once, but Tomb Raider
subdivision transforms each surface's four roots to camera space again.
R16 added a lazy exact-CPU view cache indexed by the existing room vertex id.
It retained `WorldCamera::view_vertex` arithmetic (unlike the visually
different GTE experiment) and therefore targeted only duplicate transforms.

The additional 52 KiB scratch and per-reference validity checks cost more than
the shared transforms saved. cortex_v3 lockstep render rose
1,416,120→1,416,765 cycles, I-cache stalls rose 321,902→325,019, and one of
925 hashes changed at guest frame 1626. The experiment was removed without a
normal-mode run.

### R17: reuse static packet payload by frame-arena slot

R17 marked primitive-arena slots with their previous packet kind and, when a
textured-gouraud quad returned to the same slot with the same material and UV
payload, patched only dynamic positions and colours. This tests packet-template
reuse without adding a persistent per-surface pool.

The premise is invalid because arena slot identity is not stable across frames:
culling and mixed packet types shift which surface owns a slot. cortex_v3
lockstep changed 546 of 925 hashes, with the first mismatch at guest frame 216.
It also regressed mean render work 1,416,120→1,436,933 cycles, p95
2,002,178→2,027,968, maximum 2,259,241→2,291,672, and I-cache stalls
321,902→333,863 per visual. The experiment was fully removed.

The safe alternative—a persistent four-child packet template for every
potentially split room surface—would require roughly 459 KiB in addition to the
existing approximately 114 KiB root pool at current limits. Positions and
colours would still be dynamic, as would midpoint projection, depth selection,
and command insertion. Together with R5's proof that LLVM already writes packet
words directly into their final arena addresses, this rejects frame-arena and
resident full-packet caching for the current PS1 RAM budget.

### R18: bypass model UV offset reconstruction when the offset is zero

Both tapes use authored model UVs, so their per-instance UV offset is zero. R18
added an inlined zero-offset branch around the three wrapping byte additions per
face. All 925 cortex_v3 lockstep hashes matched, but mean render work rose
1,416,120→1,418,486 cycles and p95 rose 2,002,178→2,002,398. I-cache stalls
fell by 3,873 cycles per visual, but the added hot-loop control flow cost more
than the arithmetic it removed. The experiment was reverted without normal-mode
or cortex_v1 runs.

### R19: const-specialize the authored-UV model batch

R19 moved the R18 decision out of the face loop: the bucketed extent-safe batch
was monomorphized for zero and non-zero UV offsets so the common authored path
contained neither offset arithmetic nor a per-face branch. This is the best
form of the UV-specialization proposal for the captured workloads.

All 925 cortex_v3 lockstep hashes matched, but duplicating the already-large
batch displaced hotter code from the 4 KiB I-cache. Mean render work rose
1,416,120→1,423,710 cycles, p95 rose 2,002,178→2,012,782, and I-cache stalls
rose 321,902→328,723 per visual. It was fully removed. R18 and R19 together
close the zero-offset model-UV direction: neither a runtime branch nor
compile-time duplication is profitable on this target.

### P1: exact camera collision-solve memoization

The camera already gathers its collision-room set only when a full,
non-hash key changes and runs the spring-arm sweep every second tick. P1 added a
monotonic collision-set revision and reused an interval solve only when that
revision and every solver input—focus, yaw, pitch, height, distance, minimum
distance, and margin—matched exactly.

cortex_v3 lockstep kept all 925 hashes and reduced camera/update means by
4,368/4,492 cycles per tick. cortex_v1's complete visual-hash sequence was also
identical (one presentation checkpoint moved from guest frame 377 to 378), and
camera/update fell 1,910/1,966 cycles per tick. However, delivered normal
cortex_v1 cadence regressed 26.99→26.81 FPS and the two-vblank hit rate fell
79.2%→77.5%; cortex_v3 improved only 15.71→15.79 FPS. Because both projects
must improve, the memoization was removed despite its local CPU saving.

### E4b: gate the duplicated front-of-player room setup

The second placed-model pass reconstructs room camera, options, and lighting
for every visible room. E4b skipped that setup when the cooked room owned no
model instance; both current projects contain only one placed instance, so this
is the narrowest exact form of the reviewers' pass-2 deduplication proposal.

It reduced cortex_v1 lockstep render work 765,819→761,269 cycles, p95
1,098,666→1,096,046, maximum 1,224,056→1,218,560, and I-cache stalls
171,812→169,292. However, guest frame 584 produced a unique changed display
hash. The otherwise empty calls therefore participate in queued-frame or packet
history that static room membership does not capture. The gate was removed
without v3 or normal-cadence runs.

### T2: spread active-room construction across ticks

The proposed crossing-spike scheduler is already the shipping implementation.
`RuntimeScheduleConfig::active_job_builds_per_tick` is `1`, and
`ActiveRoomWindow::step_job` stops after one accepted room build. Streaming
residency is pumped separately; a blocked build stops rather than skipping
ahead, and the completed window is published only by `finish_job`.

R10 stage correlation also falsifies this as the cortex_v3 frame-time cause.
Only 100 of 1,856 lockstep rows performed active-window work, its mean across
the run was 381 cycles, and its maximum was 68,742. None of the eight most
expensive cortex_v3 gameplay frames performed an active-window build. Those
frames instead spent 1.08–1.55 million cycles in `room_surface_draw`, with the
model tail reaching about 190,000 cycles in the other peak region. Increasing
the per-tick build count would concentrate work; reducing it below one cannot
make forward progress. No code change is warranted.

### R20: place the TR lattice in PS1 scratchpad RAM

R20 placed only the 216-byte nine-vertex one-level subdivision lattice at
`0x1f800000`, leaving projection, packets, topology, and host behavior
unchanged. This directly tested the proposed typed-scratchpad staging without
attempting a risky stack switch or DMA access.

The scratchpad is not private renderer storage under the current runtime
contract: 223 of 1,046 cortex_v1 lockstep hashes changed from the start of
gameplay geometry. It also produced no speedup—mean render moved
765,819→765,859 cycles—and increased I-cache stalls
171,812→175,859 per visual. The experiment was removed before v3 or normal
replay. Scratchpad work remains closed until the runtime can explicitly own and
test a region across interrupts and all SDK subsystems.

### M8/R21: attribute bulk memory and omit bucket-only slot scratch

An untouched R10 cortex_v3 replay sampled the guest PC every 64 retired
instructions (6,132,839 samples). During gameplay, `memcpy` and `memset`
accounted for 5.52% and 4.51% of samples. Call-site unwinding identified a
4,102-byte `memset` emitted while constructing every bucketed world pass.
Bucketed submission stores its links in compact commands and never reads the
pass's two inline 2,048-entry linked-list arrays.

R21 constructed only the live pass fields and left those `MaybeUninit` arrays
untouched. cortex_v3 kept all 925 lockstep hashes and reduced mean render
1,416,120→1,408,640 cycles. cortex_v1 reduced mean render
765,819→759,754 cycles and kept identical primitive/surface counts, but three
of 1,047 visual images changed. A route-tick capture proved this was visible,
not a hash-only artefact: 804 floor pixels differed at guest frame 456, while
the adjacent frames were exact. A same-binary R10 rerun was bit-for-bit
deterministic. The optimization was removed.

### R22: do not clear unused room-search array tails

The room-membership neighbour BFS initializes two 256-entry `RoomIndex`
arrays to `0xffff` even though it writes and reads only their cursor-bounded
prefixes. R22 replaced them with explicit `MaybeUninit` prefix buffers. All 68
`psx-game-runtime` tests passed, and the intended stage improved:
`sim_room_track` fell 7,553→5,804 cycles per tick and total update fell
126,231→124,227 in cortex_v1 lockstep.

The full frame did not improve: render rose 129 cycles, and guest frames 558
and 576 changed display hashes. Because the frozen R10 disc reruns exactly,
these are candidate-induced differences, not replay noise. The prefix buffers
were removed before normal or cortex_v3 runs.

### R24: initialize only the used collision-room prefix

The remaining 60 Hz bulk-copy group initialized six 80-byte
`CharacterCollisionRoom` entries before gathering a cursor-bounded prefix,
then copied a complete six-entry camera cache. R24 used `MaybeUninit` only
inside the collector, exposed only the initialized prefix, and updated only
the used persistent entries. The collector's writes and every consumer's
count bound were explicit.

cortex_v3 kept all 925 lockstep images and improved `sim_collision`
11,574→9,475 cycles/tick and total update 119,503→117,113. Code-layout effects
raised render 1,416,120→1,418,078 and I-cache stalls
321,902→323,445, leaving total frame work about 1,540 cycles lower.
cortex_v1 improved more: `sim_collision` 12,590→9,764, update
126,231→123,500, render 765,819→764,257, and I-cache stalls
171,812→169,599. However, guest frame 576 changed, so the candidate was
removed.

Together with T1a/T1b, R11/R11b, R12/R12b, R21, and R22, this closes the
review's broad event-driven/prefix-copy suggestion: every identified large
tail has now been measured, and every locally faster form changed at least one
presentation checkpoint or regressed the complete frame.

### R23: borrow cached-quad aggregates across the outlined submit boundary

PC attribution found a 68-byte `memcpy` in
`submit_textured_gouraud_quad_prescreened_uv_words_prepared_depth`, the
outlined common entry for cached room quads. R23 passed projected vertices,
packed UV words, and colours by reference through that boundary, leaving
packet order, depth, culling, splitting, and packet construction unchanged.
All 269 `psx-engine` tests passed.

The MIPS result was worse immediately. cortex_v3 lockstep mean render rose
1,416,120→1,422,505 cycles, p95 rose 2,002,178→2,011,977, and I-cache stalls
rose 321,902→325,286. It also changed 512 of 925 display hashes beginning at
guest frame 216. The aggregate copy was therefore not a profitable redundant
move under this ABI/code layout. The candidate was removed without spending a
cortex_v1 run.

### R4/R4a: precompute lattice attributes or remove leaf copies

Two variants tested the proposed one-level TR lattice data rewrite. A full
residency pool stored the five midpoint UV/RGB attributes per eligible surface,
costing 57,344 bytes at the existing room/surface limits. It preserved output
but increased cortex_v3 mean render work by 14,110 cycles versus the frozen
baseline (17,420 versus V0), with effective FPS unchanged at 14.03. A smaller
variant kept the existing attributes but addressed child leaves by lattice
index instead of copying four compact vertices; it increased mean render work
by 3,903 cycles versus the frozen baseline.

Both variants were removed. On this MIPS target the added helper/argument and
resident-memory traffic costs more than the five midpoint interpolations or
four small leaf copies they replace.

### R5: packet template copy/patch to remove an arena double-write

The proposed double-write does not exist in optimized MIPS code. Disassembly
of `submit_textured_gouraud_quad_leaf_uv_words_prepared_depth` shows the packet
words written directly to the address computed from the primitive arena. Its
72-byte stack frame holds saved registers and scalar spills; no temporary
`QuadTexturedGouraud` is materialized and copied. The 832-byte function has 208
instructions, one call (command insertion), and one direct sequence of packet
stores. Replacing the constructor with explicit reserve-and-patch code would
only restate code LLVM already emits, so R5 is rejected without perturbing the
runtime.

### R8: retain TR identity GTE state across surfaces

Hoisting the identity projection load out of each Tomb Raider subdivision
submission gave cortex_v3 a small local win: 14.07→14.18 FPS, mean render
1,401,072→1,398,923 cycles, and I-cache stalls 386,202→382,926 per visual.
All 927 v3 lockstep hashes matched.

It is not a valid engine-wide change. The fully hoisted form changed one of
1,047 cortex_v1 frames. Restoring the triangle load made every v1 hash exact,
but regressed its normal render mean by 2,302 cycles and I-cache stalls by
2,893 per visual. Two lazy ownership variants changed 37–38 v1 frames, proving
that other surface paths can replace implicit GTE control state between
submissions. The experiment was fully reverted; state loads remain local to
each submission.

### R9: cook every cylinder-prop UV into the surface record

Moving the complete bilinear result into each cooked cylinder surface improved
cortex_v3 more than the edge-only runtime change (mean render work fell by
about 30,000 cycles), but altered the resident record/schema and produced
transient painter-order differences in the first lockstep comparison. The
schema rewrite was removed in favour of V4, which captures a measurable part
of the win without changing data layout or packet identity.

### M9: joint render-tail attribution

Sorting the R10 cortex_v3 normal capture by total render work avoids the
invalid sum-of-independent-maxima argument in the architecture reviews. Across
the twenty most expensive visual frames, mean render work was 2,057,263
cycles. `room_surface_draw` contributed 1,345,219 cycles (65.4%);
`image_props`, `model_instances`, and `player` brought the joint share to
87.4%. The same frames averaged 152.1 considered room surfaces and 496.5
triangle primitives.

The largest frame (guest frame 717) cost 2,267,126 render cycles:
1,385,262 room surfaces, 214,114 props, 183,649 models, and 187,533 player.
The tail is therefore not a portal-loop or streaming spike. It is the
simultaneous cost of the normal room-emission path and the three ordinary
object paths. This confirms that sustained 30 FPS cannot be obtained by
optimizing only a rare fallback or only average room work.

### M10: pooled cortex_v1/cortex_v3 room-cost model

Fresh R10 captures from the same engine commit were regressed using
`room_surface_draw ~ room_surfaces_considered + tri_primitives`. The common
1,196-frame fit was:

```
room_surface_draw = -200,823 + 6,858 * surfaces + 878 * primitives
R² = 0.8419, RMSE = 113,562 cycles
```

This narrowly misses the review's predeclared R² > 0.85 acceptance threshold.
Separate fits were materially different: cortex_v1 estimated 3,699 cycles per
surface and 841 per primitive (R² 0.444), while cortex_v3 estimated 6,660 and
534 (R² 0.697). Adding a project intercept raised R² to 0.8545; adding
project-specific slopes raised it to 0.8642 and improved AIC by 176 points
versus the common model. Surface/primitive correlation is low in each capture
(0.078/0.118), so this is not merely two collinear counters exchanging weight.

J6's proposed universal two-coefficient model is rejected. Both projects do
run the same engine; the result says their routes exercise different mixtures
of that engine's surface kinds, projection/subdivision branches, culling, and
object work. Candidate changes must therefore pass both project tapes instead
of being selected from a pooled slope.

### R25: hoist invariant cylinder surface options

The cylinder-prop loop reconstructed the same depth, cull, subdivision, and
maximum-edge policy for every visible surface. R25 built that invariant portion
once per draw and applied only the varying material inside the loop. All 68
`psx-game-runtime` tests passed and all 925 cortex_v3 lockstep display hashes
matched.

The MIPS result regressed: mean render rose 1,416,120→1,418,028 cycles, p95
rose 2,002,178→2,005,406, and I-cache stalls rose
321,902→323,741 per visual. The compiler's original chained construction has
better code layout/data flow on this target. The change was removed without
spending a cortex_v1 run.

### M11: gameplay-window PC attribution

A normal cortex_v3 build was sampled every 64 retired instructions, entirely
from the emulator, producing 6.13 million samples. Filtering the windowed
capture from route tick 300 removes boot/menu work and leaves 5.09 million
gameplay samples. `tools/pc_symbolize.py --min-window-start` now performs this
filter reproducibly.

The hottest gameplay symbols were the vblank-edge spin in `run_scheduled`
(14.82%), model geometry submission (11.65%), the visible-cell room renderer
(10.83%), `memcpy` (5.59%), `memset` (4.52%), cached-room TR quad submission
(3.77%), TR quad leaves (3.13%), and SIO0 pad polling (2.09%). Portal visibility
refresh was 0.25%. This independently confirms that portals are not looping
and that the remaining render cost is ordinary geometry/model emission plus
bulk memory traffic.

The scheduler sample is idle presentation quantisation, not removable render
work: the loop waits for the vblank IRQ before the framebuffer swap. Moving the
next pad poll/update before that edge samples input up to one tick early and
changes simulation/input latency; portal visibility also depends on the
post-update camera. The suggested present-wait work hoist is therefore rejected
under the visual/semantic-preservation contract. The SIO0 setup spin is also
not optional: the documented SCPH-1200 silicon sweep fails at 384 spins and is
clean at 768, while the engine's 1,024-spin default supplies the hardware
margin. It remains in the 60 Hz input path.

### R26: prepared exact room-fog quotient

The PC profile showed integer divides in the fogged room vertex-depth loop.
R26 prepared `floor(2^24 / fog_span)` once per room, then replaced each vertex
divide with a multiply and a proven one-step correction. An exhaustive test
over representative spans, including cortex_v1's 19,968 and cortex_v3's
17,600, showed exact equality for every depth in each interval. All 333 engine
and runtime tests passed and all 925 cortex_v3 lockstep hashes matched.

The target stage improved (`room_depth_prep` 4,397→3,363 cycles), but code
layout moved cost elsewhere: `room_project` rose 21,350→23,674, total render
rose 1,416,120→1,417,586, and I-cache stalls rose
321,902→326,413 per visual. The candidate was removed without a cortex_v1 run.

### R27: compile linked-list slot scratch out of bucketed passes

Fresh call-site sampling reconfirmed a 4,102-byte `memset` while constructing
each visual frame's `WorldRenderPass`. R27 replaced the two inline OT-sized
arrays with zero-sized storage under an explicit bucketed-only engine feature,
so this was a structural removal rather than R21's uninitialised-state
shortcut. The ordinary engine build retained its existing linked/sorted
storage and all 269 engine tests passed; the bucketed-feature test also passed.

cortex_v3 lockstep kept all 925 hashes and reduced mean render
1,416,120→1,411,040 cycles. cortex_v1's 1,047 presented image hashes were also
identical in sequence; the faster build tagged one identical image at guest
frame 378 instead of 377 because it crossed the telemetry marker earlier.
However, the normal pacing gate rejected it: cortex_v1 mean render improved
742,800→738,056, yet delivered FPS fell 26.99→26.88 and two-vblank periods
fell 79.2%→78.0%. The smaller code image changed direct-mapped I-cache/vblank
phase enough to lose three delivered visuals. The candidate was removed.

### S2: smaller world sectors

The reviews' “smaller sectors” suggestion is not an engine cache-granularity
knob in this format. `sector_size` is the authored world-space scale used by
room geometry, collision, portals, cameras, fog/subdivision distances, and
route coordinates (1,664 in cortex_v1 and 1,536 in cortex_v3). Changing it
rescales the level and changes the image and gameplay; subdividing the
*render/culling* cells while retaining authored geometry would instead require
a new conservative spatial index and can increase duplicate references.

The existing cooker already emits per-cell PVS bitsets at the authored cell
granularity, and V3 directly tested finer portal-to-cell admission without a
useful reduction. A sector-size mutation therefore violates the
visual-preservation contract and is rejected without an invalid A/B. A future
independent render-cluster index is covered by R7 and remains rejected by its
measured constituent costs.

### R28: initialize only the used blended-model index prefix

Call-site attribution found a 64-byte clear for the blended-vertex chunk in
every model part. R28 represented the chunk as `MaybeUninit<u16>` elements,
wrote each element before advancing the cursor, and exposed only the proven
initialized prefix. All 269 engine tests passed. Both lockstep tapes were
pixel-exact: cortex_v3 matched 925/925 hashes and cortex_v1 matched all 1,046
comparable hashes with no missing or extra frames.

The render work improved consistently. cortex_v3 lockstep mean/p95/max fell
1,416,120/2,002,178/2,259,241→
1,405,633/1,988,983/2,244,613, and I-cache stalls fell 5,751 per visual.
cortex_v1 fell 765,819→758,528 mean with 3,281 fewer I-cache stalls.
Normal cortex_v3 improved 15.71→15.75 FPS and mean render by 9,111 cycles.
Normal cortex_v1, however, fell 26.99→26.88 FPS and two-vblank periods fell
79.2%→78.0%, even though mean render improved 7,033 cycles. Whole-route
accounting showed the saved render time becoming idle `present` wait, but the
phase change still lost three delivered visuals. Because both projects must
improve under the delivered-cadence gate, the candidate was removed.

### C1: warning-only cooker performance envelope

The cooker now computes a camera-independent upper envelope from the data it
actually emits: the maximum surfaces in any single-room PVS, the sum of the
heaviest `visible_chunk_limit` room PVS sets, the corresponding authored
triangle and prop-surface pressure, and the heaviest
`resident_chunk_limit` streamed payload/sector footprint. It also reports the
fixed one-level TR packet pressure before any hardware-extent fallback and
compares that planning figure with the 1,536-packet runtime arena. The
calculation is warning-only because combining the heaviest rooms ignores
portal reachability and is intentionally conservative.

Both projects cook successfully with the new report:

| project | visible rooms | single-room PVS surfaces | room-surface envelope | authored-triangle envelope | TR+prop pre-HW packets | resident payload / stream |
|---|---:|---:|---:|---:|---:|---:|
| cortex_v1 | 6 | 138 | 390 | 771 | 2,015 | 48,432 / 53,248 B |
| cortex_v3 | 6 | 79 | 364 | 728 | 1,974 | 34,784 / 38,912 B |

The frozen lockstep tapes stay below the predicted envelopes: cortex_v1's
surface/primitive maxima are 73/551, and cortex_v3's are 183/639. The
predictor therefore passes its predeclared soundness check on both routes.
Both conservative pre-hardware packet figures exceed 1,536, so neither project
can yet claim an all-theoretical-views packet guarantee; the cooker says so
explicitly and requires recorded-view/hardware validation rather than silently
certifying the content.

The new focused unit test passes. The full `psxed-project` run executes 377
tests with 367 passes, 9 pre-existing starter-project expectation failures,
and 1 ignored diagnostic; none of the failures touches the performance
envelope or streamed-room accounting.

## Final clean-source reproduction and hardware hand-off

The accepted source at `7d7ace73` was rebuilt from a fresh cook of each
project and replayed again after all rejected candidates had been removed.
Every ordinary-mode metric reproduced the R10 baseline exactly. Both
lockstep runs reproduced every captured image:

| project / mode | effective FPS | render mean / p95 / max | periods <=2 VBlanks | lockstep hashes |
|---|---:|---:|---:|---:|
| cortex_v1 normal | 26.99 | 742,800 / 1,069,593 / 1,211,569 | 79.2% | n/a |
| cortex_v3 normal | 15.71 | 1,364,086 / 1,953,348 / 2,267,126 | 0.9% | n/a |
| cortex_v1 lockstep | 29.96 scheduled | 765,819 / 1,098,666 / 1,224,056 | 77.0% | 1,046 / 1,046 exact |
| cortex_v3 lockstep | 29.98 scheduled | 1,416,120 / 2,002,178 / 2,259,241 | 1.2% | 925 / 925 exact |

Hardware-safe CUE/BIN pairs were then built with the accepted engine feature
set plus the on-screen presented-FPS/worst-gap overlay, and without emulator
telemetry:

`cd-stream-bench world-order-bucketed world-grid-visible ot-2048
vis-anchor-pvs-candidates tr-subdivision-lattice fps-overlay`

Both full poll-bound tapes complete on those exact disc images and the sampled
screens show complete route geometry. The structural preburn checks pass
(`SYSTEM.CNF`, `PSX.EXE`, `WORLD.PAK`, and `UI.PAK`; cortex_v1 also has its
audio track). Artifact hashes:

| project | burn image | SHA-256 |
|---|---|---|
| cortex_v1 | `editor/projects/cortex_v1/baked/cortex_v1.bin` | `1da8d36f57821dd16f6daef420c43a8749c1f8592a6b0c040227fb7b87eb0e6d` |
| cortex_v3 | `editor/projects/cortex_v3/baked/cortex_v3.bin` | `010acdfe24a5c965906deab937a47b79ec1931a1a112385c40f91efdeb88a878` |

The local overlay reads 24–30 FPS with worst gaps of 2–4 VBlanks along the
cortex_v1 route and 14–17 FPS with worst gaps of 4–6 along cortex_v3. These
are emulator observations, not silicon results. H1 remains open until the
same images are run on a physical NTSC PlayStation through the portal seams,
the point-blank near-plane views, and the heaviest combat/junction views.

### R29: remove dense-room identity-index scratch writes

The current gameplay-only PC profile and disassembly exposed a dense cached-room
loop that writes `projected_indices[index] = index` before projecting the same
contiguous vertex cache directly. A source audit found no production caller
reading the scratch after return, so R29 removed the loop and explicitly made
post-draw scratch contents unspecified. All 269 engine tests plus the compile
time guards passed after replacing one legacy assertion about this internal
scratch state with the function's observable projected-vertex count.

cortex_v3 passed the first full gate: all 925 hashes matched, mean render fell
1,416,120→1,408,816 cycles, p95 fell 2,002,178→1,991,145, and I-cache stalls
fell 321,902→318,288 per visual. cortex_v1 also became cheaper
(765,819→762,387 mean render; 171,812→169,804 I-cache stalls), but failed exact
visual equivalence at guest frame 592: 1,045/1,046 hashes matched. A targeted
stop-and-dump reproduced the difference and localized it to exactly one display
pixel at `(228, 148)`, changing RGB `(41,57,49)`→`(33,41,33)`; the immediately
preceding and following images were exact. No ordinary-mode benchmark was run
after that mandatory visual gate failed. The source and test contract were
restored completely.

### R30: share the visible/all-cell surface-emission tail

The two cached-room entry points each contain the same surface-emission walk,
and their baseline MIPS symbols are 38,212 and 32,840 bytes. R30 extracted that
walk into one `#[inline(never)]` generic helper so both selection policies could
reuse a single hot implementation. All 269 engine tests and compile-time guards
passed.

The target build invalidated the premise: cortex_v3's lockstep PSX-EXE payload
grew 1,349,632→1,445,888 bytes (+96,256) instead of shrinking, and the replay
reported `PERSISTENT ASSET LOAD FAILED` before completing the route. The large
argument surface and generic monomorphization produced a worse target layout
than LLVM's existing inlined/merged code. This is a hard RAM/layout failure, so
the candidate was restored before any performance claim or v1 run.

### R31/R31b: inline the one-level projected TR quad leaf

The one-level lattice calls a separate 2.5 KiB projected-quad leaf four times
per subdivided quad. R31 forced that leaf to inline. All 269 engine tests and
compile-time guards passed, and the cortex_v3 target became materially cheaper:
mean/p95/max render fell
1,416,120/2,002,178/2,259,241→
1,394,444/1,976,522/2,233,584 cycles, while I-cache stalls fell
321,902→308,721 per visual.

The output was not equivalent. Only 495/926 ordered image checkpoints matched;
the strict guest-frame comparison reported 431/925 mismatches beginning at
guest frame 386. Emulator-owned GP0 capture proved this was not merely a hash
checkpoint phase shift: command counts and GPU cycle counts stayed identical,
but command-word hashes first diverged at tape frame 2 and only 343/923
presented command streams matched. The forced-inline candidate was rejected.

R31b changed the force to an ordinary `#[inline]` hint. LLVM retained the
baseline target layout: payload size, every measured cycle statistic, I-cache
stalls, and all 925 image hashes were exactly unchanged. The no-op hint was also
removed.

### R32: force-inline cached baked-room fog shading

Gameplay-only PC sampling attributes 1.21% of samples to
`RuntimeRoomLighting::shade_cached_baked_vertices`, despite its ordinary inline
hint. R32 forced that four-vertex fog path to inline. All 68 runtime tests and
policy experiments passed. cortex_v3 then matched all 925 lockstep images while
mean/p95/max render fell
1,416,120/2,002,178/2,259,241→
1,380,942/1,939,881/2,203,474 cycles, I-cache stalls fell
321,902→318,070, and two-vblank periods rose 1.2%→2.1%.

cortex_v1 also became cheaper (765,819→760,047 mean render and
171,812→167,841 I-cache stalls), but failed the exact visual gate. Its ordered
sequence matched 1,046/1,047 images; guest frame 552 differed, and one
presentation checkpoint moved from guest frame 376 to 375. A targeted
frame-552 dump localized the real image difference to 135 pixels in the
extreme-left `(0..31, 112..126)` screen patch. Adjacent frames were exact.
Because the output change is real, no normal-mode benchmark was run and the
inline force was removed.

R32b kept the fog computation forced-inline but routed the no-fog return through
an outlined helper, attempting to preserve v1's call boundary. It retained most
of the v3 gain (1,416,120→1,381,866 mean render and
321,902→309,415 I-cache stalls), but changed 1/925 v3 images at guest frame
1658. It was also removed before a v1 run.

### R33: force-inline the projected split-safety predicate

The compact `projected_triangle_can_skip_split` predicate accounts for 0.75% of
gameplay PC samples and has several world/model callers. R33 forced it inline
after all 55 focused renderer tests passed. On the MIPS target those many call
sites expanded the cortex_v3 executable from 1,349,632 to 1,490,944 bytes
(+141,312), and the route immediately reported
`PERSISTENT ASSET LOAD FAILED`. The candidate was reverted without a v1 run.

### O1/O2/O3: target optimization-level and LTO sweep

The PS1 has a 4 KiB direct-mapped instruction cache, so smaller whole-program
code can outperform locally shorter `-O3` paths. Three clean cortex_v3
lockstep builds tested that premise without source changes:

| build | payload | mean / p95 render | I-cache stalls | primitives | visual result |
|---|---:|---:|---:|---:|---|
| baseline `-O3`, fat LTO | 1,349,632 B | 1,416,120 / 2,002,178 | 321,902 | 418.2 | 925/925 exact |
| O1 `-O2`, fat LTO | 1,296,384 B | 1,414,921 / 2,016,323 | 326,957 | 366.9 | 820/925 changed |
| O2 `-Os`, fat LTO | 1,171,456 B | 1,829,420 / 2,593,612 | 379,921 | 335.4 | 698 changed, 1 extra |
| O3 `-O3`, thin LTO | 1,374,208 B | 1,465,921 / 2,058,163 | 344,639 | 418.2 | 58/925 changed |

`-O2` and `-Os` remove emitted primitives and produce widespread missing
geometry despite compiling the same safe source; they are unusable on the
experimental MIPS-I backend. Thin LTO retains primitive counts but is slower,
larger, and still changes output. The existing `-O3`, one-codegen-unit, fat-LTO
profile remains the validated target configuration.

### R34: localise the `-O2` geometry divergence

Guest-frame-aligned GPU census corrected the initial aggregate diagnosis.
Disabling the caller-owned prebuilt quad pool at `-O2` changed only 1/925
images and left textured-triangle and textured-quad counts identical at every
common tape frame; the apparent primitive recovery was only the arena counter
starting to include packets that the prebuilt pool normally owns.

`room-surface-profile` then found the actual boundary. Both O3 and O2 identify
18.1 TR subdivision candidates per visual, but O3 successfully submits 18.0
while O2 submits only 7.0. The rejected candidates fall back to their authored
root quad, which removes about 41 generated textured quads per aligned gameplay
frame. Fixed 64-instruction delays both after and before the identity RT/TR
control-register load changed no submission result, rejecting both CTC2 settle
and preceding-operation drain as causes.

Finally, compiling with default features disabled so the old recursive TR path
really replaced the 3x3 lattice restored 18.0 submissions and 418.2 arena
primitives, but render mean rose to 1,703,801 cycles, I-cache stalls to 392,835,
and 715/925 images still differed from the O3 lattice reference. The divergence
is therefore an optimization-sensitive miscompile inside the experimental
MIPS-I lattice path, not portal visibility, packet lifetime, or OT/DMA loss.
The clean O3 lattice build remains the only validated configuration.

### R35: overlap lattice RTPT batches

R35 replaced the blocking two/three RTPT sequence with `rtpt_kick`/`read`,
converting one projected row while the next GTE batch was in flight. The
topology, inputs, GTE operations, packet counts, and ordering were unchanged,
and all ten focused Tomb Raider renderer tests passed.

The full cortex_v3 lockstep gate rejected it: mean render rose
1,416,120→1,417,257 cycles, I-cache stalls rose 321,902→322,977, and 171/925
images changed despite the same 418.2 primitive mean. The shorter inter-op
schedule is not bit-neutral on the measured GTE model, so the pipeline was
removed.

### R36: prepared-depth baked-fog hook

The indexed renderer already proves that fog is active and prepares four fog
weights before asking `RuntimeRoomLighting` to shade baked RGB, but the generic
hook rechecks the fog state and unwraps an optional depth array. R36 added an
explicit prepared-depth trait hook whose runtime implementation performed only
the four existing blends.

All 40 focused world-render tests and the runtime lighting test passed, and all
925 cortex_v3 lockstep images matched. The smaller source operation was not a
target win: mean render rose 1,416,120→1,416,825 cycles and I-cache stalls rose
321,902→322,487. The extra trait/code shape outweighed the redundant branches,
so it was restored before v1 or normal-mode runs.

## Candidate matrix

| ID | Candidate | State | Acceptance / rejection evidence |
|---|---|---|---|
| M2 | Emulator-owned I-cache refill events/stall cycles by route window | accepted | 176k v1 / 388k v3 stalls per visual; hashes and VRAM exact |
| M3 | Room-only packet/command boundary counters | accepted | v3: 131.9 arena packets and 169.8 commands/visual; TR contributes about 90 commands |
| M6 | Rebuild both projects without `emulator-telemetry` | diagnostic complete | v3 unchanged at 15.713 FPS; v1 -0.106 FPS; telemetry is not the missing budget |
| M7 | Warmed surface micro-profile + workload-density check | diagnostic complete | v3 kind+material only 37.4k diagnostic cycles vs 369.4k submit; 25.4 cells but 88.9 authored quads, no portal loop |
| V0 | Reuse cell AABB `half_y` across frustum/portal tests | accepted | v1/v3 render mean -0.18%/-0.24%; all 1,974 lockstep hashes and final VRAM exact |
| R1 | Surface-level zero-fog warm-path gate | rejected after five shapes | best exact-v3 form: 14.25→14.40 FPS, but 2/1,046 v1 hashes changed; exact-v1 form changed 1/925 v3 hashes |
| R2 | `#[inline(never)]` hot dispatcher leaves | rejected | v3 14.03→13.67 FPS, render mean +4.3%, I-cache stalls +1.7%; 534/927 lockstep hashes changed |
| R3 | Residency-built `SurfaceDrawRecord` + option variants | rejected by decomposition | Classification is 37.4k vs submission 369.4k; R2/R3a/R3b/R3c all regress or produce no speedup |
| R3a | Borrow per-cell options in the surface loop | rejected | exact visuals; v3 render -7,842 cycles, but v1 +113,760 cycles and 25.58→22.15 FPS |
| R3b | Cache surface kind and dynamic-material class at residency | rejected | 925/925 v3 hashes exact; FPS unchanged, mean render +1,239 cycles for +2 KiB pool RAM |
| R3c | Six-entry per-cell option table | rejected | v1 render +58,801 cycles, p95 +65,530, I-cache +15,612; 1/1,046 hashes changed |
| R4 | Residency-built lattice UV/RGB attributes | rejected | +57 KB RAM; v3 render mean +14,110 cycles vs frozen baseline; 14.03 FPS unchanged |
| R4a | Address lattice leaves by index instead of copying four vertices | rejected | v3 render mean +3,903 cycles vs frozen baseline; 14.03 FPS unchanged |
| R5 | Packet template copy/patch; remove arena double-write | rejected | MIPS codegen already emits the 14 packet words directly into the arena; no temporary packet copy exists |
| R6 | Offline whole-subdivision proof + runtime demotion | rejected pending camera-volume contract | Current legal cameras are not cooker-bounded; correct fallback classifies every unproved surface `SPLIT` |
| R7 | Fully cooked render clusters / packet-ready primitives | rejected by decomposition | R2–R6 independently cover every proposed constituent; none supplies an exact engine-wide win |
| R8 | Retain TR identity GTE state across room surfaces | rejected | unsafe variants changed 1–38/1,047 v1 frames; exact variant regressed v1 render mean by 2,302 cycles |
| V1 | Carry portal window + far plane through all-cells fallback | rejected | 926/926 hashes exact; v3 surfaces/FPS unchanged and render -55 cycles (noise) |
| V2 | Per-wedge disjoint frustum rejection after conservative union | rejected | 61/926 v3 hashes changed; FPS 14.25→14.03 and mean render +19,402 cycles |
| V5 | Existing coarse-yaw cached cell filtering | rejected | 925/925 hashes exact but cell/surface counts unchanged; normal v3 render +1.2k and p95 +3.7k |
| V3 | Cooked variable-length portal-to-cell masks | rejected | v3 surfaces 93.3→93.1, render +6,266, p95 +12,229, I-cache +4,340; 13/925 images changed |
| T1 | Event-driven active-room field copies / borrowed slices | rejected by decomposition | T1a/T1b and all attributed prefix/tail-copy variants R11/R12/R21/R22/R24 fail the full visual/performance gate |
| T1a | Clear fallback materials only when current room is absent | rejected | update -105 cycles, but render +3,705 and I-cache +3,526; one presentation boundary shifted |
| T1b | Skip settled unchanged residency reconciliation | rejected | update -2,397 cycles/tick, but render +5,714, I-cache +5,116, and 1/1,046 hashes changed |
| T1c | Decode only the selected collision floor triangle | accepted | v1 25.58→25.97 FPS; v3 14.25→15.09 FPS; all common lockstep images exact |
| T1d | Use header-only sector probes for character wall checks | accepted | v1 25.97→26.28 FPS; v3 15.09→15.38 FPS; all common lockstep images exact |
| R10 | Borrow `WorldRenderPass` during flush instead of copying 8,212 bytes | accepted | v1/v3 lockstep render -29,735/-29,725 cycles; all 1,971 hashes exact |
| R10a | Lazily build forced-split options after the quad extent check | rejected | v3 render -6,610, but v1 render +856, I-cache +1,002, and 1/1,044 common hashes changed |
| R11/R11b | Logical-only or used-prefix portal-result reset | rejected | prefix form saved 2,072 render cycles in v1 but changed 1/1,046 common images; count-only changed 2 |
| R12/R12b | Avoid initializing camera duplicate-set overflow entries | rejected | camera -2,520 cycles/tick, but render I-cache +1,976/visual; v1 stayed 26.99 FPS and <=2vb slipped 0.2 points |
| R13 | Const-specialize room depth/subdivision modes | rejected | v3 metrics exactly identical to R10; all 925 hashes exact, so LLVM already specializes them |
| R14/R14b | GTE or exact batched CPU TR root transforms | rejected | GTE saved 40.7k but changed 820/925 images and primitive topology; exact batch regressed normal v3 render by 4.0k |
| R15 | Explicitly outline cached-room TR entry leaves | rejected | all v3 metrics exactly equal to R10 and 925/925 hashes exact; compiler already outlines them |
| R16 | Cache exact CPU view vertices by room vertex id | rejected | +645 v3 lockstep render cycles, +3,117 I-cache stalls, +52 KiB scratch, and 1/925 hashes changed |
| R17 | Reuse static quad payload by frame-arena slot | rejected | 546/925 v3 hashes changed; render +20,813 and I-cache +11,961 cycles because arena slot ownership is unstable |
| R18 | Skip model UV reconstruction for zero offsets | rejected | 925/925 hashes exact, but v3 lockstep render +2,366 cycles despite I-cache -3,873 |
| R19 | Const-specialize authored model UV batches | rejected | 925/925 hashes exact, but v3 lockstep render +7,590 and I-cache +6,822 cycles |
| E4b | Skip pass-2 room setup for rooms with no model instance | rejected | v1 render -4,550 cycles, but guest frame 584 changed uniquely |
| R20 | Stage the nine-vertex TR lattice in PS1 scratchpad | rejected | 223/1,046 v1 hashes changed; render +40 and I-cache +4,048 cycles |
| M8 | Attribute bulk-memory call sites with PC sampling | diagnostic complete | 6.13M samples; gameplay `memcpy` 5.52%, `memset` 4.51%; concrete callers identified |
| M9/J5 | Joint worst-20 render-tail attribution | diagnostic complete | room surfaces are 65.4%; room + props + models + player are 87.4%; no portal/streaming spike |
| M10/J6 | Common v1/v3 surface/primitive cost model | rejected | pooled R² 0.8419 misses the >0.85 gate; project intercept/slopes materially improve fit |
| M11 | Gameplay-only PC attribution | diagnostic complete | 5.09M samples: models 11.65%, room wrapper 10.83%, TR quad/leaves 6.90%, portal refresh 0.25% |
| R21 | Leave bucketed pass's unused linked-list arrays uninitialized | rejected | v3 -7,480 render cycles and exact; v1 -6,065 but 3/1,047 images changed, including 804 captured floor pixels |
| R22 | Leave unwritten room-search array tails uninitialized | rejected | v1 update -2,004 and room-track -1,749 cycles/tick, but render +129 and 2/1,047 images changed |
| R23 | Borrow cached-quad aggregate arguments | rejected | v3 render +6,385, p95 +9,799, I-cache +3,384 cycles; 512/925 images changed |
| R24 | Initialize/copy only used collision-room prefixes | rejected | v1 update -2,731 and render -1,562 cycles, but 1/1,046 images changed; v3 exact but render +1,958 |
| R25 | Hoist invariant cylinder surface options | rejected | 925/925 v3 hashes exact, but render +1,908 and I-cache +1,839 cycles |
| R26 | Prepared exact room-fog quotient | rejected | depth prep -1,034 cycles, but total render +1,466 and I-cache +4,511; 925/925 hashes exact |
| R27 | Compile linked-list slot scratch out of bucketed passes | rejected | all v1/v3 presented images exact and render -4.7k/-5.1k, but normal v1 FPS 26.99→26.88 and <=2vb 79.2%→78.0% |
| R28 | Initialize only the used blended-model index prefix | rejected | exact hashes and v3 15.71→15.75 FPS, but normal v1 fell 26.99→26.88 FPS despite render -7.0k |
| R29 | Remove dense-room identity-index scratch writes | rejected | v3 exact and render -7.3k; v1 render -3.4k but 1/1,046 hashes changed by one pixel for one frame |
| R30 | Share visible/all-cell surface-emission tail | rejected | engine tests pass, but v3 payload grows +96,256 B and persistent asset loading fails |
| R31/R31b | Force/hint inline the projected TR quad leaf | rejected/no-op | forced form saves 21.7k render cycles but changes 431/925 v3 hashes and GP0 words; ordinary hint is bit-for-bit baseline |
| R32/R32b | Force-inline cached baked-room fog shading | rejected | R32: v3 exact and render -35.2k, but v1 changes one frame; outlined no-fog R32b changes 1/925 v3 frames |
| R33 | Force-inline projected split-safety predicate | rejected | focused tests pass, but v3 payload grows +141,312 B and persistent asset loading fails |
| O1 | Whole program at `-O2` | rejected | -53,248 B, but 820/925 v3 images change and primitives fall 418.2→366.9 |
| O2 | Whole program at `-Os` | rejected | -178,176 B, but render rises 29%, primitives fall to 335.4, and 698 images change |
| O3 | Thin instead of fat LTO at `-O3` | rejected | +49.8k render cycles, +22.7k I-cache stalls, and 58/925 v3 images change |
| R34 | Localise `-O2` geometry divergence | diagnostic complete | Prebuilt pool and OT/DMA falsified; O2 detects 18.1 TR candidates but submits 7.0. Recursive TR restores 18.0 at +146k cycles and still changes 715/925 images; experimental lattice miscompile |
| R35 | Overlap consecutive lattice RTPT batches | rejected | Same primitive count, but render +1,137 cycles, I-cache +1,075, and 171/925 v3 images changed |
| R36 | Prepared-depth baked-fog trait hook | rejected | 925/925 v3 hashes exact, but render +705 cycles and I-cache +585 |
| T2 | Spread active-window crossing spikes across ticks | already implemented; diagnostic complete | One accepted room build/tick; no active-window work in v3's eight worst gameplay frames |
| T3 | Cook every cylinder-prop UV into the surface record | rejected | ~30k v3 render-cycle win, but schema/layout rewrite changed transient painter ordering |
| V4 | Exact cylinder-prop UV edge shortcuts | accepted | v3 render mean -6,820 and I-cache -11,860; all 1,972 lockstep hashes exact |
| E8 | Record/template optimization for prop tail | diagnostic narrowed | image cards cost 5 cycles; v3 tail is ~40k box + ~88k cylinder, so card templates are irrelevant |
| G1 | GPU overdraw/timing census and texture-window dedup bound | diagnostic complete | v3 GPU mean/max 713k/879k cycles; ~396 redundant one-word writes cannot close a ~350k CPU-cycle gap |
| P1 | Exact camera collision-solve memoization | rejected | exact visual sequence and lower camera CPU in both; normal v1 cadence regressed 26.99→26.81 FPS and <=2vb 79.2%→77.5% |
| S1 | Hoist pad/visibility work into present wait | rejected by dependency/contract | present spin is tear-free vblank quantisation; input and visibility depend on the next tick/post-update camera, and early polling changes latency |
| S2 | Reduce authored sector size | rejected by contract/audit | `sector_size` is world scale, not cache granularity; changing 1,664/1,536 rescales geometry, collision, portals, and routes |
| C1 | Cooker worst-view 30 FPS/RAM/packet validator | accepted, warning-only | v1/v3 observed maxima 73/551 and 183/639 stay below 390/771 and 364/728; both theoretical packet envelopes correctly warn above 1,536 |
| H1 | Real-hardware timer, cadence, tear, seam, and near-plane sweep | burn images prepared; awaiting silicon | Structural checks and full no-telemetry tape replays pass; mandatory final gate remains physical |

Run `python3 tools/cortex_30fps_report.py <run-dir>...` for the standard table.
Pass exactly two lockstep run directories plus `--compare-lockstep` to make any
guest-frame hash mismatch, missing frame, or extra frame fail the command.
