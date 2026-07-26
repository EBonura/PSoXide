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

## Candidate matrix

| ID | Candidate | State | Acceptance / rejection evidence |
|---|---|---|---|
| M2 | Emulator-owned I-cache refill events/stall cycles by route window | accepted | 176k v1 / 388k v3 stalls per visual; hashes and VRAM exact |
| V0 | Reuse cell AABB `half_y` across frustum/portal tests | accepted | v1/v3 render mean -0.18%/-0.24%; all 1,974 lockstep hashes and final VRAM exact |
| R1 | Surface-level zero-fog warm-path gate | rejected | safe form: v3 14.03→14.36 FPS, but 1/927 transient hashes changed; packet-fast form changed 734/927 |
| R2 | `#[inline(never)]` hot dispatcher leaves | rejected | v3 14.03→13.67 FPS, render mean +4.3%, I-cache stalls +1.7%; 534/927 lockstep hashes changed |
| R3 | Residency-built `SurfaceDrawRecord` + option variants | queued | Remove static per-surface interpretation |
| R4 | Residency-built lattice UV/RGB attributes | queued | Test after M1 corrected the upper bound |
| R5 | Packet template copy/patch; remove arena double-write | queued | Must preserve packet bytes and OT slots |
| R6 | Offline whole-subdivision proof + runtime demotion | queued | No topology or threshold change |
| R7 | Fully cooked render clusters / packet-ready primitives | queued | RAM/code/stream budget gated |
| V1 | Carry portal window + far plane through all-cells fallback | queued | Must fail open for root/overlap rooms |
| V2 | Per-wedge disjoint frustum rejection after conservative union | queued | Every admitting path remains OR-ed |
| V3 | Cooked variable-length portal-to-cell masks | queued | Debug proof against current frustum path |
| T1 | Event-driven active-room field copies / borrowed slices | queued | Exact state and replay route |
| T2 | Spread active-window crossing spikes across ticks | queued | No delayed visible residency |
| T3 | Image-prop resolved records and packet templates | queued | Targets v3 tail |
| G1 | GPU overdraw/timing census and silicon constraint | queued | No GPU reorder before hardware evidence |
| P1 | VBlank/frame-pacing wait diagnosis | queued | Idle is not counted as recovered CPU work |
| C1 | Cooker worst-view 30 FPS/RAM/packet validator | queued | Fit only after surviving engine changes |
| H1 | Real-hardware timer, cadence, tear, seam, and near-plane sweep | queued | Mandatory final gate |

Run `python3 tools/cortex_30fps_report.py <run-dir>...` for the standard table.
Pass exactly two lockstep run directories plus `--compare-lockstep` to make any
guest-frame hash mismatch, missing frame, or extra frame fail the command.
