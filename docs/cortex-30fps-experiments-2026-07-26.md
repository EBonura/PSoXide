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

Hoisting the identity projection load out of each adaptive subdivision
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
| R1 | Surface-level zero-fog warm-path gate | rejected | safe form: v3 14.03→14.36 FPS, but 1/927 transient hashes changed; packet-fast form changed 734/927 |
| R2 | `#[inline(never)]` hot dispatcher leaves | rejected | v3 14.03→13.67 FPS, render mean +4.3%, I-cache stalls +1.7%; 534/927 lockstep hashes changed |
| R3 | Residency-built `SurfaceDrawRecord` + option variants | queued | Remove static per-surface interpretation |
| R4 | Residency-built lattice UV/RGB attributes | rejected | +57 KB RAM; v3 render mean +14,110 cycles vs frozen baseline; 14.03 FPS unchanged |
| R4a | Address lattice leaves by index instead of copying four vertices | rejected | v3 render mean +3,903 cycles vs frozen baseline; 14.03 FPS unchanged |
| R5 | Packet template copy/patch; remove arena double-write | rejected | MIPS codegen already emits the 14 packet words directly into the arena; no temporary packet copy exists |
| R6 | Offline whole-subdivision proof + runtime demotion | queued | No topology or threshold change |
| R7 | Fully cooked render clusters / packet-ready primitives | queued | RAM/code/stream budget gated |
| R8 | Retain TR identity GTE state across room surfaces | rejected | unsafe variants changed 1–38/1,047 v1 frames; exact variant regressed v1 render mean by 2,302 cycles |
| V1 | Carry portal window + far plane through all-cells fallback | queued | Must fail open for root/overlap rooms |
| V2 | Per-wedge disjoint frustum rejection after conservative union | queued | Every admitting path remains OR-ed |
| V3 | Cooked variable-length portal-to-cell masks | queued | Debug proof against current frustum path |
| T1 | Event-driven active-room field copies / borrowed slices | queued | Exact state and replay route |
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
