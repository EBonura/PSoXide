# Overnight performance pass, 2026-09-02

Scope set by Manny before going to bed: performance work across the three
BSP games, Cortex Ignition 0.4 first, then quake-psx, then hl-psx, with a
screenshot and a table for every change in the morning. Every number below
comes from the whole-level Cortex tape bench (`make cortex-bench`, lockstep
visuals, stop at poll 5250 on the next display flip) unless stated. Bus
cycles are the primary metric because they include every RAM, cache and
multiply stall the emulator models.

Evidence folder: `docs/overnight-perf-2026-09-02/` (tape-end pairs at
4x, the grounding side-view pair, the start and end bench summaries).

## Where the cycles went (baseline, main at 53a6337c)

| share of bus cycles | what |
|---|---|
| 45.3% | instruction issue |
| 31.6% | RAM load stalls (no data cache on the R3000) |
| 10.2% | stack load stalls (register spills reloaded from RAM) |
| 8.9% | instruction-cache refills |
| 6.1% | multiply/divide interlocks |
| 5.2% | MMIO |

Hot functions by retired instructions: `Playtest::render` 21.6%,
`draw_pxbsp_faces` 17.4%, `submit_textured_model_geometry_impl` 12.1%,
`submit_classic_affine_mixed_batch` 6.9%, `CollisionHull::trace_into` 6.9%,
idle spin 6.3%.

Two profiles the emulator produced for this pass and which are worth
keeping in the toolbox:

- `--icache-event-log` with the exact victim/incoming line pairs. It showed
  that 10% of all refills were `materialize_pxbsp_face` and
  `draw_pxbsp_faces` evicting each other, 9% the pose sampler against the
  model submitter, 4% the collision kernels against each other: leaves
  that happened to land a multiple of 4 KiB from their caller's loop.
- `--ram-load-stall-line-log`, which attributed the top load stalls to the
  per-face `Face` decode in `draw_pxbsp_faces` (six loads, nine stack
  stores per face) and the plane-scan loop.

## Changes

### L1. Hot callers placed before their leaves in the guest link (landed)

`sdk/psoxide.ld` now lists a handful of shared engine functions first in
`.text`, each caller directly followed by the leaves the refill log
caught it fighting with. Name fragments match the mangled function
sections regardless of crate hash, so a renamed function silently falls
back to default placement. Every guest that links through the SDK script
gets the same layout, so quake-psx and hl-psx inherit it on repin.

| metric | before | after | delta |
|---|---|---|---|
| bus cycles | 7,569,976,132 | 7,488,289,396 | -1.08% |
| work instructions | 3,213,533,173 | 3,209,541,518 | -0.12% |
| icache share | 8.91% | 7.91% | -1.0 pt |

Visual gate: the layout disc stopped at poll 5251 hashes exactly as the
baseline at poll 5250 (`0x7cf9c3b2c159df7b`), i.e. the same pixels one
poll later; the faster build flips one vblank earlier. A first attempt
with lld's `--symbol-ordering-file` only moved `memcpy` because v0
mangled names carry the crate hash; the linker-script wildcard form is
what landed. `build_guest_staged.sh` gained
`PSOXIDE_GUEST_EXTRA_RUSTFLAGS` for this kind of experiment.

### F1. Face records borrowed in place through the draw and mark loops

`draw_pxbsp_faces` decoded every candidate face into a ten-byte `Face`
(six loads) which the compiler then kept on the stack (nine stores) and
reloaded per field. `FaceRef` reads each field at its use. The frame
mark loops also drop two bounds checks the map validated at load.

| metric | before (main with the ordering fix) | after | delta |
|---|---|---|---|
| bus cycles | 7,576,259,730 | 7,398,034,103 | -2.35% |
| work instructions | 3,213,516,622 | 3,172,557,314 | -1.27% |

Visual gate: the branch disc at poll 5251 hashes exactly as the baseline
at poll 5250 (`0x5c7e724d99a7773d`).

### G1. World faces classified against the frustum on the GTE

The plane-scan loop inside `draw_pxbsp_faces` was the hottest code in the
profile: three `mult`s per vertex per plane with the `mflo` interlock
each time. One `MVMVA` against the light matrix now yields three plane
distances per vertex and a second against the colour matrix the last
two; the scan is vertex-major with the plane-major early exits kept.
Constants are added on the CPU because MVMVA scales its translation by
4096 before adding. Both matrices are free during the world pass and
every lit submission reloads them.

| metric | before (F1) | after | delta |
|---|---|---|---|
| bus cycles | 7,398,034,103 | 7,213,524,876 | -2.49% |
| work instructions | 3,172,557,314 | 3,122,913,830 | -1.56% |
| muldiv share | 6.23% | 4.66% | -1.6 pt |

Visual gate: hashes identical to F1 at the same poll, which proves the
classification bit-exact over the whole tape.

### G2. Node boxes tested against the frustum on the GTE

The node-visibility rebuild classifies every visited node's box with two
support corners per plane; that loop was 7.1% of all retired
instructions. The corners now go through the plane matrices G1 loads,
one MVMVA per corner instead of three interlocked multiplies.

| metric | before (G1) | after | delta |
|---|---|---|---|
| bus cycles | 7,213,524,876 | 7,073,000,833 | -1.95% |
| work instructions | 3,122,913,830 | 3,164,671,083 | +1.34% |
| muldiv share | 4.66% | 3.01% | -1.7 pt |

More instructions, fewer cycles: the GTE path issues a few extra moves
but removes the multiply interlocks. Hashes identical at the same poll.

### R1. World pass rotates by the camera's exact trig (the grounding fix)

Details in the grounding section. On the whole-level tape this costs
cycles rather than saving them:

| metric | before (G1) | after | delta |
|---|---|---|---|
| bus cycles | 7,213,524,876 | 7,327,772,070 | +1.58% |
| work instructions | 3,122,913,830 | 3,243,165,227 | +3.85% |
| route ticks to poll 5250 | 12,619 | 12,819 | +200 vblanks |

The extra work is not the rotation itself (one 3x3 product per frame).
The world is now rotated by up to a few degrees differently from the
old table-and-linear-atan path, so the set of faces the frustum keeps on
this particular route is different and, on this tape, larger. That is
the correct view; the old one was drawing the world rotated against the
actors. Tape-end pair: `pair-rot-tapeend.png`.

### Morning session (2026-09-02, after the report was first written)

Manny asked for the three leads to be taken and for more. Same bench,
baseline now the end-of-night main (7,306,636,338 bus cycles).

| step | commit | bus cycles | delta | visual |
|---|---|---|---|---|
| fan writer outlined (`#[inline(never)]`) and placed after the batch submitter | not landed | 7,475,722,196 | +2.31% | identical |
| one MAC read per box-corner test | dc6d19b2 | 7,276,360,832 | -0.41% | identical |
| near-clip second phase, one dot per vertex, hoisted constants | d8bd9684 | 7,284,929,369 | +0.12% cycles, -0.62% instructions | identical |
| cube sky: reject faces early, ping-pong buffers, no per-cell zeroing | bf4f4903 | 6,966,750,918 | -4.65% cycles, -5.67% instructions | identical |
| collision trace: check indices once, unchecked aligned reads | f674e611 | 6,969,607,100 | +0.04% cycles, -1.63% instructions | identical |
| marked-face collector: word scan, unchecked chain write | 87e4ff53 | 6,955,326,200 | -0.21% | identical |
| node boxes from sum and extent (4 GTE products instead of 10) | not landed | 7,004,452,499 | +0.54% cycles, -1.35% instructions | identical |
| three per-face bounds checks dropped in the world draw head | 8f0dd05d | 6,925,050,691 | -0.44% | identical |
| per-frame visibility passes kept out of line (fewer spills) | landed after 8f0dd05d | 6,894,775,185 | -0.87% | identical |
| world objects outside the frustum skip their strict-occlusion traces | landed after d5836bff | 6,369,238,065 | -7.62% cycles, -8.10% instructions | identical (cadence shift) |
| small node boxes take only the reject test (no mask clearing) | not landed | 6,381,234,020 | +0.19% cycles, -0.86% instructions | identical |
| per-material policy byte before the material cache probe | not landed | 6,371,523,181 | +0.04% cycles, -0.54% instructions | identical |
| long frame chains retired with one table clear | landed after 95e76c00 | 6,302,975,010 | -1.04% | identical (cadence shift) |
| occlusion samples gated on the observer's PVS before tracing | landed after 095a2eee | 6,215,575,582 | -1.39% | identical |
| mark loops without per-face bounds checks | 0533642b | 6,182,443,894 | -0.53% | identical |
| camera side of each plane resolved once per face pass | 64f85a3a | 6,141,886,138 | -0.66% | identical |

Morning session so far against the end-of-night main: 7,306,636,338 to
6,141,886,138 bus cycles, -15.9%. Against the start of the night
(7,569,976,132): -18.9%, with the grounding fix included.

Manny's decision on the in-view occluded beacons: no rate limit; a
beacon appearing late would confuse. Everything landed keeps every
frame identical.

The world-object finding deserves a line of its own. The three points of
interest in 0.4 are cooked with direct brush occlusion, so every frame
the runtime traced up to seven camera-to-object segments per object
across the whole level to decide whether the beacon may draw; that was
about nine long BSP traces per tick and most of `trace_into`'s 7%. An
object the view frustum rejects is never drawn, so it is no longer
traced. The remaining cost is the in-view, occluded case, which still
pays seven traces a frame; rate-limiting that (re-testing an occluded
object every few frames) would cut most of what is left but would let a
beacon appear a few frames late, so it is Manny's call.

Two negative results. The sum-and-extent box test replaces ten MVMVAs
and thirty corner selects per node with four MVMVAs and three multiplies
by the identity `2 n.outer = n.(min+max) + |n|.(max-min)`; it cuts
instructions by 1.35% but costs 0.54% more cycles, because the ten
products end up on the stack and the reloads outweigh the GTE work
saved (unrolling did not change the generated code). Retried once the
selection pass was its own function, in case the spills came from
sharing the scene render's register allocation: still +0.66% cycles for
-0.26% instructions, so the identity stays unused. Outlining the classic-affine fan writer to end
the batch submitter's self-eviction made things slower, because the
`PacketWriter` state then lives in memory on every packet instead of in
registers; the refill count was a poor proxy for the cost of sequential
refills. The submitter stays as it was.

The sky result came from finally attributing `memcpy` and `memset`: the
hot call sites all sat next to `psx_bsp::sky::cube_face_plane_distance`.
Every mixed lattice cell was clipped against all six cube faces, and each
clip zeroed two 160-byte buffers, copied the cell in, copied every pass
back and zeroed the sample array, for a pass that emits a few dozen
triangles. A face every corner lies outside of on one plane is now
rejected before any buffer is touched, the four passes alternate between
two caller-owned buffers, and the sample array lives once per frame; the
per-vertex arithmetic is unchanged and the frames hash identically.

## Grounding: the floating actors

Manny's instruction was to build a test level with a camera exactly side
on and measure the distance to the ground. `gen_grounding_side_view`
clones the 0.4 player and both enemies node for node onto a flat slab,
puts the gameplay camera at floor height looking horizontally, and stands
a 16-unit cube beside each actor. With `--dump-draws`,
`tools/measure_side_view_grounding.py` turns each cube's bottom edge into
the floor line at that actor's depth and its height into the pixel scale,
so the float of the lowest rendered vertex comes out in engine units with
no camera model assumed.

What the fixture showed, at poll 1200 with the telemetry counters on:

| | player | light enemy | heavy enemy |
|---|---|---|---|
| motor y (counter) | 4.0 = slab top | | |
| lowest vertex above the world floor, before | +2.2 | +2.9 | +2.9 |
| after | -1.5 | -0.7 | -0.7 |

The decisive detail was that the shadow decals (drawn by the model pass
at the motor floor) projected above the horizon while the cube bases
(drawn by the world pass, same y) projected below it: the two passes were
not looking through the same camera. The model pass rotates with the
third-person camera's exact Q12 sine and cosine; the world pass recovered
angles from those with a linear-ratio approximation and then truncated
to the 256-step sine table (`>> 4`, 1.4 degrees per step), so the world
was rotated by up to 0.7 degrees against the actors standing in it. At
250 units of depth that is three units of floor, and the sign flips with
the camera's position between two table steps, which is why enemies
floated in some places and sank in others while the editor, which draws
without the world pass, showed the player and the heavy enemy correctly.

`pxbsp_view_rotation` now builds `Rx(pitch) * Ry(yaw)` directly from the
camera's trig pairs (a test pins it to the table form at table-exact
angles), and the world and sky passes load it through
`load_pxbsp_view_rotation`. The remaining -0.7 to -1.5 units is the
cooked "a hair into the floor" the earlier grounding work left on
purpose. Screenshot pair: `pair-grounding-sideview.png` (feet band at 4x,
before above, after below).

The light enemy sinking in the editor viewport is a separate, editor-side
placement question that was not investigated tonight.

## Results table

Cortex Ignition 0.4, whole-level tape, bus cycles (lockstep, stop at
poll 5250). "Visual" says how the frame at the same poll compares with
the previous row.

| step | commit | bus cycles | vs previous | vs start | visual |
|---|---|---|---|---|---|
| start of the night (main, 53a6337c) | | 7,569,976,132 | | | |
| draw-ordering fix (agent branch, landed) | 89a00205 | 7,576,259,730 | +0.08% | +0.08% | occlusion fixes only |
| L1 hot-text layout | 8cc3bcd3 | (bench on pre-ordering main: 7,488,289,396, -1.08%) | -1.08% | | identical (cadence shift) |
| F1 FaceRef | e482a091 | 7,398,034,103 | -2.35% | -2.27% | identical (cadence shift) |
| G1 GTE face classify | 019aaf69 | 7,213,524,876 | -2.49% | -4.71% | identical |
| G2 GTE box test | e6477f03 | 7,073,000,833 | -1.95% | -6.56% | identical |
| R1 exact world rotation | d0ecba39 | 7,306,636,338 | +3.30% | -3.48% | world now aligned with actors |

The rotation fix gives back about half of the perf gain on this tape.
The GTE op counts say the world pass itself did not grow (RTPT 913k vs
924k, NCLIP equal), so the extra cycles are outside the world draw:
`memcpy` and `memset` grew by 36M and 21M instructions and the scene
render by 47M. Per-tick cycle logs place the whole difference in one stretch
of the route, tape frames 1750 to 2750 (+7% to +8% per 250-frame
bucket), with every other bucket within +-1.5%: that stretch is the
open hall with the item pickups, where a few degrees of yaw bring a lot
more of the far wall into the frustum. The rest of the tape is neutral.
It is a correctness change and stays.

## quake-psx

Repinned locally (branches `repin/psoxide-8ff8769a` and, after the
morning session, `repin/psoxide-16decb2c`; the demo disc carries
4cdd4d7 at 1dce6e35). The E1M1 chain bench reports the chain incomplete
at every pin from 41b968f2 (the B4/B5 landing, before this night)
onwards and complete at 891850b6 just before it: the autopilot wedges
near waypoint 17 at a slightly different position each run.

What that is and is not. quake-psx renders through its own
`game/src/renderer.rs`, not through `psx_bsp::render`, so none of the
world-pass work here reaches it; of the B4/B5 landing it links only the
attributed-clip Q16 helpers and the sky scroll arithmetic, both
rendering-side and both exact by their oracle tests. The 1,000-frame
start route (`start-route-regress`) renders bit-identically at 891850b6
and at 16decb2c and costs the same cycles to within 500 in 1.89 billion,
so the SDK move did not change what quake draws or how fast it draws
the start. The chain bench's own cycle totals differ between the two
pins (6.84 billion against 5.84 billion for the same 3,800 guest frames),
but that difference is the stalled route rendering cheaper frames while
the player faces a wall, not a speed-up; an earlier draft of this report
read it as one and was wrong.

The desync itself is therefore a harness sensitivity: quake integrates
its movement with real frame time, so any change in how many vblanks a
frame takes anywhere along the 3,800-frame chain moves the autopilot
onto a different path, and this one wedges. Until the regression route
is tick-locked or re-recorded, the E1M1 fps number cannot be read from
this bench and SDK repins should be judged by frame hashes on the fixed
routes (the start route is one).

## Harmonising the Cortex and Quake world passes (evaluation, 2026-09-02)

Manny asked whether Cortex and quake-psx can be harmonised further so
the day's world-pass wins reach Quake. The short answer: they cannot be
bridged by sharing code as things stand, because Quake does not run the
code that improved, and the biggest Quake win available is one Quake
already has and does not ship.

What each game runs today:

| stage | Cortex 0.4 (`psx_bsp::render`, PXBSP path) | quake-psx (`game/src/renderer.rs`, 8.5k lines) |
|---|---|---|
| map format | PXBSP v5: nodes with quantised bounds, compact planes, PVS | classic Quake lumps through `ResidentMap` (the format psx-bsp was derived from) |
| visibility | PVS re-marked every frame into packed face states | visible-face list rebuilt only when the camera leaf changes (`prepare_visibility`) |
| frustum | node walk inheriting clip masks, node boxes on the GTE (today) | per-face GTE `aabb_outside_clip4`; 16-face block boxes and an exact-key selection cache exist behind feature flags |
| face classify | five-plane vertex-major GTE scan (today) | near-plane flag per face, CPU or GTE behind a flag; attributed clip for near faces |
| materialise/submit | `materialize_pxbsp_face` into the shared classic-affine mixed batch | own materialisers into `submit_quake_classic_affine_batch`, with subdivision caches |
| sky | psx-bsp cube lattice (fixed today) | own lattice (`quake_core::sky`), already bounded |
| shared today | `psx_engine::classic_affine`, `attributed_clip`, `psx_bsp::collision`, movers, formats | same |

So of the morning's twelve landed changes, the ones inside
`draw_pxbsp_faces`, `select_frame_pxbsp_faces`, `sky.rs` and the
editor-playtest world-object pass reach quake-psx not at all. The
1,000-frame start route confirms it: bit-identical and cycle-identical
across the pins.

Three ways to bridge, in order of return per effort:

1. Ship Quake's own accepted work. `game/Cargo.toml` carries 62
   `renderer-*` feature flags and `RENDERING.md` is a measured ledger of
   them; the exact-key selection cache and the 16-face block gate are
   marked accepted, `hoisted-indexed-world` is accepted, and the accepted
   feature-gated build measured 23.856 fps against 21.857 for the original
   renderer on the fixed E1M1 route. None of it ships: `default = []` and
   `build_disc` refuses features for shipping provenance ("requires the
   release profile with no guest features"). Promoting the accepted flags
   into `default` is a quake-psx policy change, not engine work, and it is
   worth roughly +7% to +9% on Quake's own ledger. It needs the E1M1 chain
   harness made frame-time independent first, because that is the only
   full-route gate and it currently desyncs on any timing change. The
   ledger also warns that dormant flags were authored against older
   stacks and may bypass later specialisations, so each one has to be
   re-measured on the current pin, not switched on together.

2. Port the four ideas that are format-independent into Quake's renderer:
   the camera-side-of-plane table (Quake tried and rejected a
   consecutive-same-plane cache, but not a per-plane table), the
   word-scanned collector and whole-table retire (Quake keeps a cached
   visible list instead, so this may not apply), the outlined per-frame
   passes to cut spills (Quake's ledger says the frame is memory-bound
   and names spills), and the vertex-major GTE near classification (Quake
   has `renderer-gte-near-classification` behind a flag already). Each is
   a day of work with Quake's own A/B, and each is single digits at best.

3. Converge on one world pass: move quake-psx onto the PXBSP format and
   `psx_bsp::render`. The formats already share a definition, the
   collision hulls and movers are already shared, and psx-bsp still
   carries the classic `ResidentMap` renderer Quake forked from. But
   Quake's renderer is ahead of the PXBSP path in places (selection
   cache, subdivision caches, liquid warps, water portals, sprites, brush
   entities with their own transforms), so convergence means the shared
   path has to absorb those first or Quake regresses. That is weeks, and
   the payoff is maintenance, not frame rate: the two passes already do
   comparable work per face.

Recommendation: do 1 now (it is Manny's policy call and a harness fix),
consider 2 opportunistically, and treat 3 as the long-term direction
only after the demo.

The reverse direction has one exact idea Cortex could borrow: Quake
rebuilds its PVS face list only when the camera leaf changes, while
Cortex re-marks every frame. In Cortex that mark pass is under 2% now,
so it is not urgent.

## hl-psx

Repinned to 8a40fa18 and then to the main tip 8ff8769a on the local
branch `repin/psoxide-8a40fa18` (not pushed) and rebuilt with the
telemetry disc each time; the tram ride produced exactly the same frame
hash (`0x955d28432f2aa63e`) and per-segment flip counts as at be93eb6b
(segment 0: 16.60 fps, segment 2: 19.33 fps). Re-run at 16decb2c after the
morning session: still the same hash and the same segments. hl-psx renders its world through its own
33k-line renderer and the classic affine writer, not through
`draw_pxbsp_faces` or the node-visibility rebuild, so tonight's psx-bsp
changes do not reach it, and the linker-script placement only moves
functions it does not have. The warping work Manny asked for is
untouched and is the next hl-psx item; the seam-census finding from
earlier in the day (the rail is drawn by the refined path, so the
tearing is not a missing-subdivision problem) stands.

## Leads not taken tonight

- `submit_classic_affine_mixed_batch` is a 40 KB function whose hot set
  spans about 15 KB and refills the 4 KiB instruction cache on its own
  (23% of all refills); two of its hot clusters alias each other in
  cache sets 0x2e to 0x3c. Splitting the surface loop head from the
  packet writer into separately placed functions is the next icache
  lever, and it is shared with quake-psx and hl-psx.
- `CollisionHull::trace_into` dispatches on the hull representation
  (three plane record kinds, two node kinds) inside its inner loop and
  reads clipnode children with unaligned `lwl/lwr`; a monomorphised
  trace per representation is worth about a fifth of its 7%.
- `memcpy` is 2.2% of instructions and the callsite sampler only sees
  the wrapper; a stack-walking sample or an instrumented memcpy would
  attribute it.
- Stack load stalls are 10% of bus cycles: the big render functions
  spill heavily. Smaller functions with fewer live values in the hot
  loops, or a scratchpad-resident working set for the world pass, are
  the levers.

## Evening pass (2026-09-02, 17:00 to 23:30)

### The quake stall was a compiler bug, and it was everywhere

The E1M1 chain bench had stalled at waypoint 17 for every SDK pin since
41b968f2. Guest-side probe rings showed the first door reaching its open
state while its brush stayed put: after `place_mover` the entity origin read
back as its own render index. The disassembly explains it. LLVM's MIPS
delay-slot filler had hoisted `lw s3,24(sp)` into the delay slot of the
mover-kind branch, and the branch target's first instruction was
`move s8,s3`. The R3000 has no load interlock, so that move read the stale
s3. Hardware and PSoXide behave identically. The mover arithmetic that the
bisect had blamed was proven innocent on the guest: 1,461 calls, zero
divergences between the i64 and u32 forms.

PSoXide's staged guest build has carried
`-Cllvm-args=-disable-mips-df-backward-search` since 21f15c1b; the game
repos never inherited it. Landed:

| Repo | Commit | Hazards before / after |
|---|---|---|
| quake-psx | 0094422 | 5 / 0 |
| nitroxide | f26a3cd | 2 / 0 |
| psxcel | ff9025e | 1 / 0 |
| voxide | a9d2200 | scan clean |
| gh-psx | 37b5b65 | scan clean |
| pico8-psx | 66f900e | 0 real (8 data false positives fixed in the scanner) |
| psx-demo-disc | eb322fd | loader and launcher flag inline |
| PSoXide | 035d61d8, ee31b815 | `tools/hazard_scan.py`; staged guest build refuses a hazardous image |

hl-psx is the exception: its image has 12 hazards and the flag costs
31,508 bytes of RAM it does not have. See below.

quake-build also set RUSTFLAGS itself for link maps, which replaces the cargo
config entry, so every map-requesting build (the accepted-stack bench among
them) silently lost the flag. Fixed in quake 37c9886.

### quake-psx: the accepted renderer stack now ships

The disc built with `default = []`, so every accepted gain in RENDERING.md
lived only in the benches. quake 37c9886 makes the accepted stack the
default.

| E1M1 chain route, PSoXide 16decb2c | fps | VRAM / display FNV |
|---|---|---|
| plain build (former shipping) | 21.219 | 0x6af3a0dee5cd6a90 / 0x621bf7ee03f427a4 |
| accepted stack (now default) | 24.314 | identical |

Demo disc 2756cdd carries it; quake-headless-check (both replays identical,
pins 0xc991ba1b29ab3b48 / 0xca8a8ab705d30905) and program-headless-check
(9 routes) pass.

Gameplay-only PC profile of the shipped build (route ticks 1200 onward):
vblank wait 18.5%, collision about 23% (hull trace 13.4%), renderer about
35%, game logic 7%, liquid 4%. A guest counter census shows 56 scene traces
per frame, all from the player's physics; monsters think at 10 Hz and barely
register. The hull walker is already explicit-stack with axial fast paths
and a broad phase (RENDERING.md's "recursive" note is stale). The per-tick
sequence mirrors SV_WalkMove, so the only cuts left change player physics.
At a real 30 fps cadence the bench's three ticks per frame become two, which
removes a third of that cost by itself.

The whole-route profile is dominated by the CD sector spin (21.5% of route
instructions), but every CD command sits inside the two map-load windows,
so that is load time, not fps.

### Cortex 0.4

Engine flag sweep on the whole-level tape against the trim baseline, noise
floor 0.151% (bench-noise vs bench-noise2):

| Feature | Bus cycles | Frames |
|---|---|---|
| classic-affine-gpu-polygon-clip | -0.220% | identical |
| classic-affine-gte-otz | -0.137% | cadence-shifted hash |
| classic-affine-level0-fast-path | does not compile | |

Nothing above noise. Two designs remain and both need RAM: the
aperture-bounded sky lattice (about 4 KB of code) and alternate-frame
selection reuse (a persistent copy of the 2,283-entry face chain, about
4.6 KB, because the frame chain is overlaid by model scratch). The cooked
manifest sits at every floor already: packet capacity 1,536, grid arenas
collapsed to one slot, and the persistent asset streamer at 319 pages
(about 638 KB) is content-derived. There is no engine-side RAM diet.

### hl-psx

Tram ride against the installed default build, same recipe and frontend:

| Build | Moving segments 0 to 4, fps |
|---|---|
| control, no flag | 16.70 / 13.01 / 19.40 / 9.77 / 16.49 |
| flag, opt-level z | 10.93 / 7.95 / 13.91 / 6.23 / 11.13 |
| flag, opt-level s | 13.63 / 10.45 / 17.26 / 8.19 / 12.94 |
| flag, full opt, model pool 46,720 | 16.18 / 12.61 / 18.96 / 9.49 / 16.04 |

The last row is the one to ship, about 3% for correctness, but it does not
fit: size-optimising the cold SDK crates recovers 8,192 B, the model pool can
give at most the 6,200 B the weapon-cache audit leaves (the audit only means
something against a cook made with the same cap), and cutting the view-model
pool moves the requirement rather than removing it. Still about 17 KB short.
Parked on branch `perf/delay-slot-flag` (325d240); main untouched.

### Decisions waiting

- Cortex: asset or content cuts; the renderer and the RAM carve are at their
  floors for this level.
- Quake: whether player physics may skip the step sweeps when the straight
  move already succeeds at full fraction, or whether the two-tick 30 fps
  cadence is the target build.
- hl-psx: where 17 KB of RAM comes from before its correctness fix ships.
