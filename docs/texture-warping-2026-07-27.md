# Minimising PS1 texture warping: an empirical measurement

2026-07-27. Tool: `emu/crates/emulator-core/examples/texwarp.rs`.

```bash
cd emu && cargo run -p emulator-core --release --example texwarp /tmp/texwarp
```

## Why this exists

Texture warping on PS1 is normally argued about by eye. This measures it in
**texels of sampling error, per pixel**, so mitigations can be ranked instead of
guessed at.

## The instrument

A self-identifying texture: a 64x64 15bpp texture where
`texel(u,v) = 0x8000 | (v << 6) | u`. Every texel is a unique 16-bit value, so
reading a rendered pixel back out of VRAM says exactly which texel the GPU
sampled at that pixel. Drawn as a raw-texture opaque primitive (GP0 `0x25` /
`0x2D`), the texel reaches VRAM verbatim: no blending, no dither, no CLUT.

Ground truth is analytic, not another renderer. Each scene is a planar
*parallelogram* in camera space, so UV is exactly affine in 3D. For a pixel
centre we cast a ray, intersect the plane in closed form, and get the
perspective-correct `(u,v)`. Error is the L2 distance in texels between sampled
and correct.

The rasterizer is `emulator-core`'s own: the center-sampled DDA verified
pixel-exact against silicon. These are hardware numbers, not a model of hardware.

Scenes: floors from head-on to 85 degrees at two distances, receding walls
(horizontal depth axis), and three doubly-tilted planes where *both* surface
axes vary in depth. The doubly-tilted ones matter, see the 1D-subdivision trap
below.

### Validation

Three independent checks that the instrument reads true:

1. **Head-on quad** (zero true warp) reads mean 0.69 / max 1.41 texels. That
   1.41 = sqrt(2) is +/-1 texel in each of u and v: the PS1 UV DDA's own
   fixed-point truncation. That is the noise floor. Nothing can beat it, and
   any strategy scoring a mean below ~0.7 is indistinguishable from perfect.
2. **Visual**: `err-*.ppm` heatmaps and `uv-*.ppm` sampled-UV dumps. The
   baseline quad is a solid red band (>4 texels) through the middle of the
   floor; the fixed version is near-black with faint seams at split lines.
3. **Closed form vs measurement**, below.

## The closed form

For one edge spanning `du` texels between depths `za` and `zb`, affine
interpolation lands the screen midpoint at the wrong texel by exactly

```
err_texels = du * |zb - za| / (2 * (za + zb))
```

Measured against the real worst-case error over 177 samples:

| ratio measured_max / predicted | median | p90 | max |
| --- | --- | --- | --- |
| | 1.58x | 2.40x | 2.91x |

So the expression is a valid runtime subdivision criterion, and **`err * 2.4`
bounds the true worst-case texel error at p90**. That is cheap enough to
evaluate per edge on the CPU: one subtract, one add, one divide.

Everything the formula implies is confirmed in the data:

* Error scales with `du`, the **UV span**, not with the polygon's size on
  screen. Halving the texture resolution on a surface (`uvhalf-quad1`) halves
  the error exactly: 10.79 -> 5.43 mean texels, at zero cost.
* Error scales with the **depth ratio**, not absolute distance. The same 85
  degree floor at 700 units reads 12.16 mean; pushed to 1800 units it reads
  3.20, because the near/far ratio across it shrank.
* Error is **zero on any polygon of constant depth**, which is why splitting
  along iso-depth lines does all the work and splitting across them does none.

## Results

Aggregate over the warping scenes, sorted by mean texel error. `prims` is the
real cost driver (draw calls and GTE transforms).

| strategy | prims | verts | mean | p95 | max |
| --- | ---: | ---: | ---: | ---: | ---: |
| scr-16x16 | 256 | 1024 | 0.84 | 1.88 | 2.44 |
| obj-16x16 | 256 | 1024 | 0.90 | 1.79 | 2.33 |
| scr-8x8 | 64 | 256 | 0.91 | 1.80 | 2.39 |
| **adapt-0.5tx** | **14.1** | **56.3** | **0.96** | 2.18 | 3.10 |
| obj-8x8 | 64 | 256 | 1.02 | 1.74 | 2.27 |
| scr-4x4 | 16 | 64 | 1.10 | 2.35 | 3.14 |
| **adapt-1tx** | **9.3** | **37.3** | **1.14** | 2.43 | 3.62 |
| obj-4x4 | 16 | 64 | 1.27 | 2.16 | 2.74 |
| adapt-2tx | 4.5 | 18.0 | 1.98 | 4.30 | 5.65 |
| scr-1x16 | 16 | 64 | 2.04 | 4.21 | 4.71 |
| obj-1x8 | 8 | 32 | 2.43 | 4.56 | 5.47 |
| obj-2x2 | 4 | 16 | 3.17 | 5.62 | 6.72 |
| uvhalf-quad1 | 1 | 4 | 5.43 | 8.62 | 10.22 |
| tri2-diagBest | 2 | 6 | 8.82 | 14.29 | 16.29 |
| tri2-diagB | 2 | 6 | 9.05 | 14.70 | 16.74 |
| **quad1** (baseline) | 1 | 4 | **10.79** | 17.41 | 19.71 |
| tri2-diagA | 2 | 6 | 10.79 | 17.41 | 19.71 |

Full per-scene data in `results.csv`.

### 1. Adaptive subdivision wins outright

`adapt-0.5tx` matches `obj-8x8` (0.96 vs 1.02 mean) using **14 primitives
instead of 64**. `adapt-1tx` beats `obj-4x4` (1.14 vs 1.27) with 9 primitives
instead of 16. Roughly **4x fewer primitives for the same visual result** than
the uniform grid.

It wins because it spends splits only where the depth ratio is bad. On the
doubly-tilted plane it emits 20 primitives; on the distant floor, 4. A uniform
grid has to be sized for the worst surface in the scene and then pays that
everywhere.

### 2. Screen-space splits beat object-space splits, but only slightly

`scr-NxN` (splits evenly spaced in 1/z) beats `obj-NxN` at every N, by about
0.1 to 0.2 texels of mean. Real, consistent, and much smaller than expected.
The reason is that once N is large enough to matter, both schemes have small
enough cells that the placement barely matters. It is worth taking because it
is free (one divide per split rather than an add), but it is not the lever.

The split for a segment from depth `za` to `zb` at screen fraction `sigma`:

```
s = sigma * za / (sigma * za + (1 - sigma) * zb)
```

### 3. 1D subdivision is a trap

On single-axis scenes (a plain floor, a plain wall), splitting only the axis
whose depth varies is the best deal in the whole table: `scr-1x16` gets 0.47
mean texels on the 75 degree floor with **16 primitives**, matching `scr-16x16`
at 256.

On a doubly-tilted plane it collapses: `scr-1x16` reads **6.74** mean texels,
barely better than a 2-way split, because the un-subdivided axis carries just as
much warp. Aggregate mean is 2.04 vs 1.10 for `scr-4x4` at the same primitive
count.

If the geometry is guaranteed axis-aligned (axis-aligned floors, walls, and
ceilings, which is most of a room), 1D subdivision is the cheapest large win
available. For arbitrary-orientation surfaces it must be per-axis adaptive, which
is what `adapt-*` does.

### 4. Diagonal choice is free and worth about 18%

Two triangles instead of a native quad costs nothing in warping if the diagonal
is picked wrong (`tri2-diagA` = 10.79, identical to `quad1`: hardware picks that
diagonal). Picking the diagonal whose endpoints are **closest in depth**, so the
cut runs along the iso-depth direction, gives 8.82 mean, an 18% reduction for a
vertex-order swap.

On the doubly-tilted scene alone the gap is 2x (15.89 -> 7.51). It is worth
doing on any large unsubdivided surface. It is *not* worth doing once the
surface is subdivided: `adapt-1tx-best` scores 1.10 vs `adapt-1tx`'s 1.14 while
doubling the primitive count, because a small cell already spans little depth.

### 5. Subdivision has a hard floor, and past 8 splits you buy noise

`obj-16x16` (256 primitives) is not better than `obj-8x8` (64): 0.90 vs 1.02
mean, inside the 0.69 noise floor. On the `wall85` scene 16x16 is actively
*worse* on max error than 8x8 (3.61 vs 2.83).

Two reasons, both real hardware effects, both getting worse with more splits:

* **PS1 vertex UVs are 8-bit integers.** Every split vertex rounds its UV, up to
  0.5 texel of error injected per split, and those seams are visible as faint
  bands in the `err-scr-1x16` heatmap.
* **Screen coordinates snap to the integer grid.** More vertices means more snap
  displacement, which also shows up as `spill`: pixels drawn outside the true
  silhouette, which climbs from 1-2 at low subdivision to 5 at `adapt-0.5tx`.

**Stop at roughly 1 texel of predicted error.** Below that you are paying
primitives to add quantisation noise.

## The cost/correctness optimum

```bash
python3 tools/texwarp_chart.py /tmp/texwarp/results.csv /tmp/texwarp/tradeoff.png
```

Correctness is measured (mean texel error). Cost is converted to guest cycles
using the two figures this repo has actually measured on cortex
(`docs/engine-30fps-architecture-2026-07-26.md`):

```
cycles_per_surface = prims * 1951 + unique_verts * 159
```

1,951 is the cortex_v3 per-emitted-primitive cost; 159 is projection per unique
vertex. Note `unique_verts`, not vertices submitted: adjacent cells share
corners, so an NxN grid projects `(N+1)^2` points, not `4N^2`.

The primitive term outweighs the vertex term by roughly 12:1. **Primitives are
the currency.** Every conclusion below follows from that.

### The budget line

cortex_v1's render mean is 850k cycles against a ~915k budget, and a frame walks
~88.6 room surfaces. Handing the entire render budget to surfaces gives a
ceiling of **10,327 cycles per surface**, which is 5.3 primitives at today's
cost. The engine currently emits ~5 packets per surface, so the model reproduces
the observed number: it is calibrated, not invented.

### The frontier

| | strategy | cyc/surface | mean texels |
| --- | --- | ---: | ---: |
| | quad1 (baseline) | 2,587 | 10.79 |
| | adapt-8tx | 4,654 | 4.88 |
| **knee** | **adapt-4tx** | **7,626** | **2.36** |
| | adapt-2tx | 10,396 | 1.98 |
| | adapt-1tx | 20,965 | 1.14 |
| | adapt-0.5tx | 31,293 | 0.98 |
| | scr-8x8 | 137,743 | 0.91 |
| | scr-16x16 | 545,407 | 0.84 |

**The Pareto frontier is adaptive subdivision essentially all the way down.**
Below 137k cycles per surface, which is 13x the frame budget, no uniform grid is
ever Pareto-optimal. Every uniform-grid and 1D point on the chart sits above and
to the right of an adaptive point that is both cheaper and more correct. That is
the whole result in one sentence.

### The maximum

The knee of the frontier is **`adapt-4tx`: 7,626 cycles per surface for 2.36
mean texels of error**, and it is the knee under *both* cost scenarios, so the
answer does not depend on fixing the emission path first. It buys **78% of the
available error reduction for 17% of the cost** of chasing the noise floor.

Under the current budget, `adapt-4tx` is also the most correct point that fits.
`adapt-2tx` costs 10,396 against a 10,327 ceiling: it misses by 0.7%. That is
inside the error bar on the surface count, so the honest answer is a band:

> **Target 2 to 4 texels of predicted error. That is `adapt-2tx` to
> `adapt-4tx`, roughly 3 to 5 primitives per surface.**

Anything tighter does not fit the frame. Anything looser gives up correctness
the budget would have paid for.

### The lever that moves the optimum

The same doc argues a template-patched emission path should reach ~400 cycles
per primitive instead of 1,951. Re-running the analysis at that cost (right
panel of the chart) moves the budget-constrained best from `adapt-4tx` to
**`adapt-0.5tx`: 0.98 mean texels, which is the instrument's noise floor**, at
9,449 cycles, still inside budget.

**Fixing the per-primitive emission tax converts directly into
visually-perfect texture mapping at no extra frame cost: 2.36 -> 0.98 texels, a
2.4x correctness gain, for free.** Warping and the 30fps problem are the same
problem, and F-1 in the architecture doc is the fix for both.

## Real content: the probe (2026-07-28)

Everything above is synthetic planes. Before changing the renderer on the
strength of that, a read-only probe measured the same closed form against what
`AdaptiveSubdivisionProfile` actually decides, on real cortex_v3 rooms:

```bash
make probe-warp                       # WARP_PROBE_PROJECT to pick a project
```

It evaluates `predicted_warp_16ths` at each of the four subdivision decision
sites in `indexed_cache.rs`, buckets by what the depth-band rule chose, and
emits count / sum / max through the existing `room-surface-profile` telemetry.
It changes no geometry. 1200 guest frames, `--hold-forward`, gameplay confirmed
in the frame dump.

**9,226 surface decisions over 265 drawing frames (34.8 per frame):**

| depth-band rule | surfaces | % of all | mean predicted warp | worst |
| --- | ---: | ---: | ---: | ---: |
| split it | 5,810 | 63.0% | 15.2 tx | 125 tx |
| skipped it | 3,416 | 37.0% | 12.4 tx | 72 tx |

Predicted warp here means "what this surface would look like as one polygon", so
on the split row it is warp the rule removed, and on the skipped row it is warp
still on screen.

### This refutes the hypothesis the probe was built to test

The synthetic bench predicted the depth-band rule wastes primitives on surfaces
that cannot warp, and that the closed form would cut ~4x. **On real content
there is no waste to cut.** Only 95 surfaces, 1.6% of all splits, were split
despite being unable to warp. The rule is not over-subdividing.

It is under-subdividing, and badly. **37% of every surface drawn is left as a
single polygon while carrying a mean 12.4 texels of predicted error**, and not
one of those 3,416 surfaces was below a texel. The depth-band rule is not
spending too much in the wrong place; it is not spending enough anywhere.

So swapping the criterion does not save primitives. It costs more. At 1,951
cycles per primitive against a 10,327-cycle surface budget, cortex cannot buy
that correctness today.

**That reorders the plan. F-1 (templated emission) is the prerequisite, not the
follow-up.** The correctness is unaffordable until primitives are cheap, and the
cost/correctness chart already showed what happens once they are: the affordable
optimum moves from 2.36 texels to the noise floor.

### Caveat on the absolute numbers

A texel of error is measured against the authored UV span, and cortex room
surfaces use large spans (up to the full 0..255 range) where the bench used 64.
A surface tiling its texture four times shows four times the texel error for the
same visual distortion. The ranking and the split/skip comparison are unaffected,
since both sides are measured identically, but do not read "12.4 texels" as
directly comparable to the bench's table above.

## F-1 first attempt: the I-cache lever is a null result (2026-07-28)

The probe put F-1 (per-primitive emission cost) on the critical path, so the
cheapest slice of it got tested first.

### Clean baseline

`make probe-warp` without `room-surface-profile`, because its per-stage cycle
counters are themselves ~30% of `room_surface_draw`. cortex_v3, `--hold-forward`,
281 drawing frames:

| | cycles/frame |
| --- | ---: |
| `frame_cycles` | 1,680,106 |
| `render` | 1,247,709 |
| `room_surface_draw` | **630,639** (50.5% of render) |
| `room_cell_select` | 56,502 |
| `room_project` | 19,004 |
| `update` | 116,062 |
| primitives (`tri_primitives`) | 468 |
| **cycles per primitive** | **1,348** |

Two calibration notes. The frame is over the 1,128,960-cycle 30 fps slot, so this
scene misses, consistent with the architecture review. And 1,348 cyc/prim against
that doc's 1,951 is a different part of the level, not a discrepancy: use this
baseline for A/B, not the doc's.

### The experiment

`draw_indexed_cached_room_surface` is a 570-line body marked `#[inline(always)]`.
That is what builds the 35,488-byte surface loop the review measured against a
4,096-byte direct-mapped I-cache, and "make the emit arms `inline(never)` leaves"
is part of the doc's own E6. One attribute, no behaviour change.

| | baseline | `inline(never)` | delta |
| --- | ---: | ---: | ---: |
| `room_surface_draw` | 630,639 | 631,373 | **+0.1%** |
| `render` | 1,247,709 | 1,248,200 | +0.0% |
| `frame_cycles` | 1,680,106 | 1,680,054 | -0.0% |
| primitives | 468 | 467 | -0.1% |
| guest exe size | 1,368,064 B | 1,318,912 B | **-49,152 B** |

**The change is real and the result is null.** The binary shrank 48 KB, so the
attribute took effect and the loop genuinely got smaller. Performance did not
move by more than noise.

So I-cache residency of the surface loop is not what costs 1,348 cycles per
primitive. The 48 KB is available for free if RAM ever becomes the constraint,
but it is not a frame-rate lever and it was reverted rather than shipped inside
this work.

### What that leaves

F-1's remaining content is the part the architecture doc always said it was:
E5 (per-surface draw records) then E6 (lattice templates, `copy_payload_from`
children, GTE-state hoist), a rewrite of a 2,596-line file behind a
packet-byte-equality gate. That is a project, not a session, and the cheap
slices of it do not substitute:

* the I-cache lever, measured above, is null;
* the packet double-write is ~14 stores, order tens of cycles against 1,348;
* the 62 by-value `WorldSurfaceOptions` passes are mostly inlined away, so the
  80-byte copies the doc counts are not obviously all real.

The honest status is that the warping fix is still gated on F-1, and F-1 has not
been made cheaper by this attempt.

## E4 and the attribution of room_surface_draw (2026-07-28)

### The gate

A/B runs need `lockstep-visuals` (the `editor-playtest` feature that fixes the
visual cadence). Without it a timing change makes the scheduler drop different
frames, the runs end on different content, and the end-state hashes are not
comparable. Baseline and E4 both produced `display=0x6aa47b4835e5f2e9`, so the
gate works and every number below is from a byte-identical render.

```bash
make probe-warp WARP_PROBE_FEATURES="cd-stream-bench emulator-telemetry lockstep-visuals" \
  WARP_PROBE_LOG=/tmp/a.csv
```

### E4: null, marginally negative

Predicted −80 to −150k. `#[inline(never)]` on all four emit arms
(`submit_adaptive_cached_room_{triangle,quad}` and the two
`submit_*_cached_uv_words` arms, two of which were `inline(always)`):

| | baseline | E4 | delta |
| --- | ---: | ---: | ---: |
| `room_surface_draw` | 638,166 | 642,500 | **+0.7%** |
| `render` | 1,274,641 | 1,278,674 | +0.3% |
| primitives | 478 | 478 | 0 |

Reverted. With the container experiment above, that is **two independent
refutations of the I-cache hypothesis**, one on the 35 KB container and one on
the leaves. E1 (a direct I-cache stall counter) was supposed to run before E4
precisely to catch this, and skipping it cost two build cycles.

### Instrumentation overhead has to be subtracted

Adding one timed section to `RoomSurfaceMicroProfile` moved the profiled stage
from 768,232 to 789,447: **each timed section costs ~21,215 cyc/frame**. With 8
sections that is ~169,722, and `789,447 - 169,722 = 619,726` against an
independently measured clean-build 638,166, agreeing to 2.9%.

So most of the "unattributed 48%" in a profiled run is the profiler. Any
`room-surface-profile` number must have ~21k per section subtracted before it
means anything.

### Where room_surface_draw actually goes

Corrected shares of the clean 638,166 cyc/frame stage, 68.9 surfaces and 136.1
packets per frame:

| region | cyc/frame | share | |
| --- | ---: | ---: | --- |
| `submit` | 309,937 | **48.6%** | **E6's target**, 2,277 cyc/packet |
| unattributed | 216,399 | 33.9% | loop body, still unmeasured |
| `projected` | 39,985 | 6.3% | GTE + depth prep, already known good |
| `material` | 18,618 | 2.9% | E5 |
| `options` | 18,523 | 2.9% | E5 |
| `lighting` | 16,089 | 2.5% | E5 |
| `backface` | 8,332 | 1.3% | E5 |
| `screen` | 5,708 | 0.9% | E5 |
| `kind` | 4,574 | 0.7% | E5 |

### This resizes E5

The architecture doc predicts E5 at −200 to −320k, which on a 638k stage would
be 31 to 50% of it. **Everything E5 targets sums to 11%**, or 70k, and a perfect
E5 cannot beat that. The doc oversized it by roughly 3 to 4x.

The per-surface `WorldSurfaceOptions` rebuild specifically, which is what the
`OptionVariantTable` would remove, is 269 cyc/surface and 2.9% of the stage. The
four variants it selects between are all draw-invariant, so the table is easy,
but it is worth under 3% and was left unbuilt on that basis.

**E6 is the whole prize: 48.6% of the stage at 2,277 cycles per packet.** That
is where the emission rewrite has to go, and it is the same lever the warping
fix is waiting on.

The remaining 34% is genuinely unmeasured loop body. It should be instrumented
before anyone optimises it, at ~21k/section of measurement cost.

## Closing the attribution, and E6's independence (2026-07-28)

Two counters close the partition: one around the per-cell setup, one around the
whole `draw_indexed_cached_room_surface` call. Then
`call - sum(inner sections)` is the surface body no counter reaches, and
`stage - cell_setup - call` is the loop's own overhead.

Instrumentation cost is now measured directly rather than extrapolated:
profiled 807,881 against clean 638,166 is **169,715 over 10 sections, ~16,971
each**. That needs no assumption about how often each section runs, unlike the
earlier single-section deltas which ranged 9k to 21k depending on whether the
section was per-surface or per-cell.

Corrected against the clean 638,166 cyc/frame stage:

| region | cyc/frame | share |
| --- | ---: | ---: |
| `submit` | 310,154 | **49%** |
| surface body, untimed | ~220,755 | **35%** |
| `projected` (GTE + depth prep) | 38,125 | 6% |
| classification (material/lighting/backface/screen/kind) | 52,632 | 8% |
| `options` | 18,514 | 3% |
| per-cell setup | 12,346 | 2% |
| loop overhead | ~0 | ~0% |

**The loop is not the problem and neither is per-cell setup.** The 35% that no
counter reached is inside the surface body, between the timed sections, in the
same function family as `submit`. Together **submit plus that body is 83% of the
stage**, and E6 rewrites both.

### E6 does not need E5

The doc sequences E5 → E6 because E6's templates were meant to hang off the
records. Reading `submit_adaptive_cached_room_quad`, E6's three parts are all
separable from `SurfaceDrawRecord`:

* `LatticeAttrs` is a new prewarm-filled pool, parallel to records, not built on
  them;
* `copy_payload_from` children is internal to the submit arm;
* the GTE-state hoist is a loop-level change.

So **E6 can be done without paying for E5's 11%.**

Two concrete items visible in that arm already:

* four `camera.view_vertex(...)` calls per surface re-derive camera-space
  positions the GTE computed during projection and discarded;
* two more 80-byte `WorldSurfaceOptions` copies (`with_cull_mode` then
  `with_material_layer`) that sit *inside* submit, so the 2.9% `options` counter
  does not include them.

## Recommendations for the engine

Current state: `render3d.rs` uses `adaptive_subdivision`, four-way splits
banded by camera depth, with a fixed 3x3 lattice by default. The probe says it
under-subdivides on 37% of surfaces and wastes almost nothing.

In priority order, revised after the probe:

1. **Do F-1 (templated emission) first.** It was already the top perf finding in
   `docs/engine-30fps-architecture-2026-07-26.md`. The probe shows it is also
   the gate on texture quality: the correctness cortex is missing costs *more*
   primitives, and at 1,951 cycles each it cannot afford them. Nothing else on
   this list is worth doing before it.
2. **Then replace the depth-band trigger with the closed form.** Split while
   `2.4 * du * |zb-za| / (2*(za+zb))` exceeds the target. The evidence for this
   is now "it fixes the 37% the depth bands miss", not the primitive saving the
   synthetic bench suggested, which the probe did not find. It also removes the
   per-project tuning of band distances.
3. **Make the recursion per-axis, not four-way.** Separable bisection is
   crack-free (no T-junctions), and it is how the extra primitives from (2) get
   spent only on the axis that warps rather than on both.
4. **Cap subdivision at ~1 texel predicted error.** Below that the primitives
   buy quantisation noise, not correctness. Only becomes the operative limit
   once F-1 lands; today the budget binds first.
5. **Pick the diagonal by depth on large unsubdivided surfaces.** Free, ~18%,
   and it applies to exactly the 37% the rule currently leaves whole. This is
   the one item worth doing *before* F-1, because it costs no primitives.
6. **Treat texture density as a warping knob at cook time.** A surface with half
   the UV span warps half as much for free. Given how large cortex's UV spans
   are, this may be the cheapest real reduction available on the big floors.

## Caveats

* The `gpucyc` column comes from the emulator's area-based GPU cost model, which
  over-counts partial edge pixels and so inflates subdivided rows. Treat
  `prims` / `uverts` as the cost truth and `gpucyc` as indicative.
* The cost axis is CPU only. GPU fill is unchanged by subdivision (identical
  coverage), so it does not shift the ranking, but it does mean the budget line
  is a CPU budget.
* The 10,327 cyc/surface ceiling assumes the whole render budget goes to
  surfaces and that the frame walks 88.6 of them. Both move with content, which
  is why the recommendation is a 2-to-4-texel band rather than one strategy.
* Vertices are snapped with round-to-nearest. The GTE truncates, which would
  shift the snap noise floor slightly but not the ranking.
* All scenes are single flat parallelograms. Warping on skinned/animated meshes
  has the same underlying cause but the subdivision options differ.
