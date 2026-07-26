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

## Candidate matrix

| ID | Candidate | State | Acceptance / rejection evidence |
|---|---|---|---|
| M2 | Emulator-owned I-cache refill events/stall cycles by route window | accepted | 176k v1 / 388k v3 stalls per visual; hashes and VRAM exact |
| M3 | Room-only packet/command boundary counters | accepted | v3: 131.9 arena packets and 169.8 commands/visual; TR contributes about 90 commands |
| V0 | Reuse cell AABB `half_y` across frustum/portal tests | accepted | v1/v3 render mean -0.18%/-0.24%; all 1,974 lockstep hashes and final VRAM exact |
| R1 | Surface-level zero-fog warm-path gate | rejected after five shapes | best exact-v3 form: 14.25→14.40 FPS, but 2/1,046 v1 hashes changed; exact-v1 form changed 1/925 v3 hashes |
| R2 | `#[inline(never)]` hot dispatcher leaves | rejected | v3 14.03→13.67 FPS, render mean +4.3%, I-cache stalls +1.7%; 534/927 lockstep hashes changed |
| R3 | Residency-built `SurfaceDrawRecord` + option variants | queued | Remove static per-surface interpretation |
| R3a | Borrow per-cell options in the surface loop | rejected | exact visuals; v3 render -7,842 cycles, but v1 +113,760 cycles and 25.58→22.15 FPS |
| R3b | Cache surface kind and dynamic-material class at residency | rejected | 925/925 v3 hashes exact; FPS unchanged, mean render +1,239 cycles for +2 KiB pool RAM |
| R3c | Six-entry per-cell option table | rejected | v1 render +58,801 cycles, p95 +65,530, I-cache +15,612; 1/1,046 hashes changed |
| R4 | Residency-built lattice UV/RGB attributes | rejected | +57 KB RAM; v3 render mean +14,110 cycles vs frozen baseline; 14.03 FPS unchanged |
| R4a | Address lattice leaves by index instead of copying four vertices | rejected | v3 render mean +3,903 cycles vs frozen baseline; 14.03 FPS unchanged |
| R5 | Packet template copy/patch; remove arena double-write | rejected | MIPS codegen already emits the 14 packet words directly into the arena; no temporary packet copy exists |
| R6 | Offline whole-subdivision proof + runtime demotion | rejected pending camera-volume contract | Current legal cameras are not cooker-bounded; correct fallback classifies every unproved surface `SPLIT` |
| R7 | Fully cooked render clusters / packet-ready primitives | queued | RAM/code/stream budget gated |
| R8 | Retain TR identity GTE state across room surfaces | rejected | unsafe variants changed 1–38/1,047 v1 frames; exact variant regressed v1 render mean by 2,302 cycles |
| V1 | Carry portal window + far plane through all-cells fallback | rejected | 926/926 hashes exact; v3 surfaces/FPS unchanged and render -55 cycles (noise) |
| V2 | Per-wedge disjoint frustum rejection after conservative union | rejected | 61/926 v3 hashes changed; FPS 14.25→14.03 and mean render +19,402 cycles |
| V3 | Cooked variable-length portal-to-cell masks | queued | Debug proof against current frustum path |
| T1 | Event-driven active-room field copies / borrowed slices | queued | Exact state and replay route |
| T1a | Clear fallback materials only when current room is absent | rejected | update -105 cycles, but render +3,705 and I-cache +3,526; one presentation boundary shifted |
| T1b | Skip settled unchanged residency reconciliation | rejected | update -2,397 cycles/tick, but render +5,714, I-cache +5,116, and 1/1,046 hashes changed |
| T1c | Decode only the selected collision floor triangle | accepted | v1 25.58→25.97 FPS; v3 14.25→15.09 FPS; all common lockstep images exact |
| T1d | Use header-only sector probes for character wall checks | accepted | v1 25.97→26.28 FPS; v3 15.09→15.38 FPS; all common lockstep images exact |
| R10 | Borrow `WorldRenderPass` during flush instead of copying 8,212 bytes | accepted | v1/v3 lockstep render -29,735/-29,725 cycles; all 1,971 hashes exact |
| R10a | Lazily build forced-split options after the quad extent check | rejected | v3 render -6,610, but v1 render +856, I-cache +1,002, and 1/1,044 common hashes changed |
| R11/R11b | Logical-only or used-prefix portal-result reset | rejected | prefix form saved 2,072 render cycles in v1 but changed 1/1,046 common images; count-only changed 2 |
| T2 | Spread active-window crossing spikes across ticks | queued | No delayed visible residency |
| T3 | Cook every cylinder-prop UV into the surface record | rejected | ~30k v3 render-cycle win, but schema/layout rewrite changed transient painter ordering |
| V4 | Exact cylinder-prop UV edge shortcuts | accepted | v3 render mean -6,820 and I-cache -11,860; all 1,972 lockstep hashes exact |
| G1 | GPU overdraw/timing census and silicon constraint | queued | No GPU reorder before hardware evidence |
| P1 | VBlank/frame-pacing wait diagnosis | queued | Idle is not counted as recovered CPU work |
| C1 | Cooker worst-view 30 FPS/RAM/packet validator | queued | Fit only after surviving engine changes |
| H1 | Real-hardware timer, cadence, tear, seam, and near-plane sweep | queued | Mandatory final gate |

Run `python3 tools/cortex_30fps_report.py <run-dir>...` for the standard table.
Pass exactly two lockstep run directories plus `--compare-lockstep` to make any
guest-frame hash mismatch, missing frame, or extra frame fail the command.
