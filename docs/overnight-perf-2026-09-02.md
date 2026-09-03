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

## Night pass 2 (2026-09-02, 23:30 onward): trampolines and selection reuse

hl-psx could not afford the delay-slot nop flag, so `tools/hazard_patch.py`
now reroutes each hazardous branch through a trampoline in a `.data` array
psx-rt declares for every guest, and PSoXide's staged guest build patches
instead of passing the flag. Cortex 0.4 gets back 40,960 bytes of heap and
41,604 bytes of code, and 2.75% of bus cycles. hl-psx trampolines its
thirteen sites for 392 bytes with a tram ride identical to control.

With the RAM back, alternate-frame world selection reuse landed for Cortex:
odd frames draw the previous frame's selected chain on the exact clip path,
so the PVS mark and the node walk run at half rate.

| Cortex 0.4 whole-level tape | bus cycles vs trim baseline |
|---|---|
| trampolines instead of the nop flag | -2.75% |
| plus alternate-frame selection reuse | -9.3% |

Evidence: `cortex-trampolines-polls.png` and `cortex-selection-reuse-polls.png`
in this folder (left control, right candidate, diff on the right). The
selection-reuse sheet is pixel-identical at polls 800, 1600, 2400 and 4800;
polls 3200 and 4000 differ by the frame-cadence offset two identical builds
already show on this tape. The switch is the `world-selection-reuse` line in
`engine/examples/editor-playtest/Cargo.toml`'s default features.

Negative: the aperture-bounded sky lattice rebased onto this base measured
+4.19% bus cycles and was reverted.

### hl-psx profile (night pass 2)

Whole-ride PC profile of the trampolined build: `play` 16.8%, the packet
emitters about 35% (quad corners 10.0, world loop tris 4.2, submodel faces
3.5, flat CV 3.2, soft cells 2.7, patches 2.5, and a long tail), and 8.4% in
the CD sector reader's wait loop. Re-profiled strictly inside the first
moving segment (route ticks 1100 to 3321) the reader disappears: that spin
is the five load gaps between segments, which the tram metric already
excludes, not frame time. The moving frame is the emitter tail plus the game
loop's own 17%; nothing there is a cheap cut.

Two follow-ups on selection reuse, measured and left out: selecting every
third frame instead of every other is another -1.53% with a clean poll sheet
but two-frame late faces at the edge (one line in `draw_pxbsp_world`,
`self.frame % 3 != 0`); keeping the select frame's side-plane clip proofs on
reuse frames is neutral (+0.10%, identical hashes), so the exact clip is not
where the draw pass spends its time.

### classic-affine writer variants on Cortex (2026-09-03)

| Feature | Bus cycles vs selection-reuse build |
|---|---|
| classic-affine-compact-subdivision-emitters | +2.19% |
| classic-affine-compact-subdivision-kernels | +2.13% |
| classic-affine-compact-level2-kernel | +3.07% |
| classic-affine-compact-world-level2-kernel | +1.20% |
| classic-affine-speculative-level0 | identical |

Quake's verdict holds on Cortex: the code-size variants lose to RAM traffic
and the writer is at its floor.

### Quake: subdivision load governor (2026-09-03)

The heavy-frame tail is geometry, so a governor that subdivides less on the
frame after a three-field frame (depth bands 96/40 instead of 136/60) was
tried. It engaged on 2,994 of 3,796 frames and measured 23.06 fps against
24.29: unsubdivided near faces exceed what the GPU polygon clip may take and
fall back to the CPU clipper, which costs more than the lattice it saved.
Reverted.

## Morning pass (2026-09-03, 05:00 onward): the sky, and where the rest is

Manny's brief for this stretch: keep going, a table breakdown, and
fundamental rendering changes are fine as long as the game still looks
right. The whole-level tape, lockstep, bus cycles:

| step | commit | bus cycles | vs previous | fps on the tape | hashes |
|---|---|---|---|---|---|
| selection reuse (end of night pass 2) | 6c73bfe5 | 5,660,905,451 | | 15.885 | |
| rotation-keyed cube-sky packet cache | e7f6a55b | 5,522,666,319 | -2.44% | 16.283 | identical |
| two-face sky cells split on the shared edge | e5eccb83 | 5,475,824,970 | -0.85% | 16.42 | identical |

Frames at three fields or fewer (20 fps or better) went from 12% of the
tape at the start of the night to 39% after the sky cache; the modal frame
is now three to four fields instead of four to five.

### The cube sky

The cube sky is a 12x9 screen lattice whose cells project onto the six
cube faces; cells that straddle a cube edge were clipped against every
face's four planes. Its packets carry staged OT slot tags rather than
addresses, and the stream depends on nothing but the view rotation and the
sky's VRAM slot, so a frame that keeps the previous rotation now copies the
last stream (9.2 KB of .bss) instead of walking the lattice. The telemetry
build puts a miss at about 185k cycles and a hit at 26k; on this tape 46% of
gameplay frames miss because the third-person camera eases its yaw on most
frames, which is why the cache took 2.4% rather than the 5% a 76% hit rate
would have given.

The miss itself was three quarters mixed cells, so those now split once on
the plane of the shared cube edge (the only plane of either face that can
cut a convex cell whose corners all lie on the two faces), reuse the texels
each corner already resolved, and fan each side. Cells whose corners reach
three faces, or whose edge point classifies to a third face, keep the full
clipper. A sweep over 2,048 rotations checks every split polygon against
the six-face clipper's: identical up to the clipper's own rounding drift
(it interpolates along already-rounded polygon edges after the first cut,
so its vertices wander by a pixel), which the test bounds at two pixels of
boundary distance. Bench hashes are identical.

### What bounds Cortex now

The telemetry build (profiler overhead of roughly 17k cycles per stage
included) puts the median gameplay frame at 1.78M cycles: room 49% (about
840k, selection plus draw for some 700 faces), the player model 16.5%
(about 300k for Aletha's 26 joints, 265 vertices and 220 submitted
triangles), the other model instances 11%, sky 5.5%, update 5%. The cooked
models are Aletha 506 faces plus a 94-face sword, the light enemy 530, the
heavy enemy 756: at 320x240 that is a PS1 hero-model budget for every actor
on screen. The release cook already runs the full Quake portal-flow vis and
the hot leaves still see 282 of 347 leaves, so the open elevated map, not
the PVS, is why 700 faces reach the draw pass. Across the whole run 40% of
bus cycles are RAM load stalls (31.5% data, 8.9% stack) and 9.6% icache
refills: the R3000 has no data cache and this code is load-bound, which is
why the code-size writer variants lose and why each remaining trim is a
few percent. Levers that need Manny's decision rather than more profiling:
the every-third-frame selection (-1.5%, two-frame late edge faces), cooking
Aletha with a single bind per vertex (the CPU-blended seam vertices are
2.7% of instructions; joints would show at the seams), and the enemy
polygon counts.

### quake-psx

`renderer-screen-frustum` had been accepted in RENDERING.md but never added
to the default feature list; it is in now (quake b5681e2): 24.440 fps on
the E1M1 chain bench against 24.314, VRAM and display hashes identical.
The chain bench also writes `quake-psx.map` beside its disc so the PC-line
histogram can be attributed.

A fresh profile of that build on the chain route: the vblank spin in
`gpu_end_frame` is 22% of instructions and 72% of frames take three fields
(21% take two), so the work inside a three-field frame is about 2.3 fields;
30 fps on this route is roughly a 15% cut away. The rest: the Quake
classic-affine writer 11.2%, `draw_frame` 10.7%, the CD sector reader 9.9%
(load gaps, outside the fps metric), `materialize_surface` 4.8%, the liquid
warp 4.8%, alias models 4.6%, collision 4.6%, the layered sky's per-corner
texel math 3.2%.

The layered sky's texel lattice depends on the rotation alone, so psx-bsp
now caches it (0170baa9). Built against this worktree the chain bench reads
24.410 against 24.440 on the pin, inside the 0.122 fps layout-noise band
with identical hashes: the autopilot turns on nearly every frame of this
route, so the cache does not engage there. Kept because it is exact and
free, but it is not a reason to repin quake.

The liquid warp resamples every visible 64x64 tile each frame because the
turbulence phase advances every three ticks, one frame at the fixed cadence.
`renderer-liquid-half-rate` quantises the tick to six so the phase advances
two steps every second frame (same speed, half the resamples).
On the same worktree SDK build the chain bench reads 24.694 fps against
24.410; on the pinned shipping SDK (16decb2c) the new default stack is
24.749 fps against the 24.314 that shipped yesterday, display hash
identical, VRAM hash moved by the water phase alone. It is the default
now (quake d8c5580). Water animates at ten updates a second, which is what
a PS1 of the period did with animated textures; if it reads as choppy on
the CRT the feature is one line to drop.

### hl-psx

The tram profile is a wide, flat emitter tail with no single hot spot:
`try_emit_quad_corners` is 11% of instructions spread evenly over a
1,300-instruction straight-line body, about 1,200 cycles per cooked quad.
The one rendering knob that is a judgement rather than a rewrite is the
runtime affine refinement threshold, `WORLD_AFFINE_SPLIT_ERROR_TEXELS`,
which was 1 with the comment "performance is deliberately secondary". At 2
the moving tram segments read 16.90 / 13.14 / 19.79 / 9.87 / 16.72 fps
against 16.67 / 13.05 / 19.38 / 9.76 / 16.49, and captures at the same
route ticks differ by 50 to 600 pixels (a floor edge, a rail sliver), so the
cooker's UV-grid cuts were already carrying the correction. Shipped (hl
caaac51); the gain is small because the runtime pass was already small.
The lossless RAM recovery Manny asked for is the trampoline patcher from the
night pass; nothing further was taken from hl tonight.

### Where the three games stand

| game | shipped this session | route metric | before | after |
|---|---|---|---|---|
| Cortex Ignition 0.4 | sky packet cache, edge-split sky cells | whole-level tape fps | 15.885 (night pass 2) / 14.338 (start of night) | 16.42 |
| quake-psx | screen frustum default, liquid half rate | E1M1 chain bench fps | 24.314 | 24.749 |
| hl-psx | two-texel runtime refinement | tram moving segments fps | 16.67 / 13.05 / 19.38 / 9.76 / 16.49 | 16.90 / 13.14 / 19.79 / 9.87 / 16.72 |

What 30 fps would take, per game, from the profiles: Cortex needs half its
CPU work removed and the profile is flat and load-bound, so the levers
left are content (actor polygon counts, the open map) and Manny's two
visual calls above. Quake's work per three-field frame is about 2.3
fields, so a further 15% of CPU work would put most of the chain route on
two fields; the candidates are the writer and `draw_frame` (22% together)
and the alias models, none of which has a cheap exact cut left. hl-psx is
1,200 cycles a quad through a straight-line emitter; that is a rewrite of
the world emission path, not a setting.

### Two quake follow-ups after the disc (2026-09-03, 06:40)

The second subdivision band alone (`subdivide_twice_at` 60 to 40, the near
band kept at 136 so no face outgrows the GPU clip) measured 24.798 fps
against 24.694 on the same worktree SDK, inside the 0.122 fps noise band,
and was reverted rather than traded for a visual change. Alternate-frame
selection reuse, the Cortex lever, was not attempted on quake: its
selection is about 3% of the route behind the exact-key cache it already
has, against 18% on Cortex, so the ceiling is not there.

## hl-psx image quality: the tessellation mechanism (2026-09-03, 09:30 to 12:30)

Manny's new focus: hl-psx tessellation, compared against quake and Cortex,
measured rather than eyeballed. Everything here is a measurement or a
before/after; the shipped tree is unchanged.

### The three mechanisms

quake-psx and Cortex go through psx-engine's classic-affine writer: every
face inside a fixed depth band is split into a lattice, one level inside
136 units (Cortex 272) and two inside 60 (Cortex 136), no per-face error
test, no budget beyond the packet arena. It is uniform, stable from frame to
frame, and it costs what it costs.

hl-psx has its own two-stage system. The cooker pre-cuts "affine candidate"
faces on a UV grid (`host/hl-bsp`, 128 to 256 texel cells, under a per-chunk
added-triangle budget), so most of the tunnel rings arrive as small cells
already. At runtime, `WORLD_CLASSIC_AFFINE_SELECTION` splits any patch quad
whose per-edge error (`affine_edge_error`, in texels) exceeds
`WORLD_AFFINE_SPLIT_ERROR_TEXELS` into a 2x2, routes patches above four
texels to a screen-bounded quadtree (`emit_soft_cell`, leaves capped at 96
px, at most 12 such routes a frame) and spends at most 352 extra packets a
frame. Quads with a vertex inside 8 units are ineligible; near-plane
crossers on the cooked-patch path take the quadtree, but on the ordinary
loop-face path they fall to a raw clipped fan with no correction at all.

### Instruments

- `--features affine-heatmap,debug-map-boot` tints every world triangle by
  predicted error (blue under 2 texels, green 2 to 4, yellow 4 to 8, magenta
  above 8); `tools/affine_distortion.py` decodes a frame. Blind spot found
  today: quadtree leaves and raw clipped fans carry no tint, so the metric
  only sees the fast-path quads.
- `--features seam-census` tints by emitter (red cooked patch quad, orange
  patch fallback, blue loop face, magenta raw triangle).
- Xash3D runs headless here: `SDL_VIDEODRIVER=dummy` with the software
  renderer, a cfg that loads c0a0 at a fixed 20 Hz and calls `screenshot`
  every 200 frames, from `~/Desktop/repos/hl-reference-runtime` (use the
  `xash3d` client, not the `xash` dedicated server). The c0a0 intro ride
  lines up with the hl-psx tram capture, so the same tunnel can be put next
  to a perspective-correct reference.

### Measurements on the tram ride (route ticks 1000 to 21000)

| build | heatmap: share of tinted screen above 8 texels | tram fps (moving segments) |
|---|---|---|
| main (caaac51) | 29.7% (p90 78%) | 16.90 / 13.14 / 19.79 / 9.87 / 16.72 |
| E1: threshold 1, quadtree cap 32, budget 512 | 32.3% | 15.68 / 12.40 / 18.66 / 9.42 / 15.68 |
| E2: quadtree leaves 48 px instead of 96 | 29.4% (leaves untinted) | 14.62 / 11.17 / 17.88 / 9.36 / 14.63 |
| E3: loop-face near crossers take the quadtree | 29.2% | 17.02 / 13.25 / 19.88 / 14.95 / 16.81 |

E1 says the budgets are not the limiter: roughly 20 patches split per frame
with budget to spare, and the frames look the same. E2 makes the near rock
wall slightly finer at a 13% fps cost. E3 is the interesting one: the
slowest segment jumped from 9.87 to 14.95 fps because the quadtree prunes
the half of a near-crossing floor that lies behind the camera, which the
raw clipped fan was paying for, and the frames look the same at every tick
compared except one, where a one-pixel seam opens between the tunnel rim
and the tram wall (tick 14000, `hl-e3-seam-14000.png`). Not shipped: besides that
seam, the regression suite's `hazard-ordering-yaw` scenario exhausts the
world primitive arena with E3 (the quadtree fans out on a close Hazard
Course wall), so the loop-quad route needs the same per-frame cap the
cooked-patch route has. The diff is `hl-loop-quadtree-e3.patch`.

What the census and the heatmap agree on: the surfaces that stay above
eight texels are the floor under the car and the wall beside it, which
cross the near plane, and those are exactly what neither the error rule nor
the budgets reach. The visible blockiness on the rock walls at ticks 8400
and 9600 is texture magnification, not tessellation.

Evidence: `hl-xash-vs-ps1-before.png` (Xash reference beside hl-psx main at
four matching points), `hl-natural-main-e3-e2.png`, `hl-heatmap-base-e3-e1.png`,
`hl-seam-census.png`.

### Reducing actor faces (Manny's question)

Yes: quadric edge collapse (Garland and Heckbert) is the standard mesh
decimation; Blender's Decimate modifier in collapse mode is that algorithm
and preserves UVs and vertex-group weights. Headless Blender on the source
GLBs: rust_mantis 610 to 366 triangles and Aletha 609 to 364 at ratio 0.6,
silhouettes intact at game distance (`cortex-decimation-trial.png`; the
Aletha render is a silhouette because her material is not embedded in the
GLB). Merging coplanar faces alone gains little on these meshes; collapse
at 0.6 to 0.7 is the range to judge in game. The decimated GLBs are in the
scratchpad; the next step is to cook one enemy through
`import_glb_model` and put the frame and the fps next to the original.

## Afternoon (2026-09-03): Manny's emulator pass, the merged disc, and the hl black triangles

Manny's verdicts on the morning disc: quake "unbelievably smooth", Cortex
great but missing his latest HUD and sounds, Half-Life smoother but with
black triangles popping and severe warping. Done in response:

- His `fix/cortex-boss-theme` branch (14 local commits) and his uncommitted
  project, cook_ui and gameplay-sound edits were merged onto the play
  branch and then, at his request, the play branch into main (PSoXide
  086eee6d). The disc's runtime and Cortex entry now pin main; the HL
  pressing was rebuilt with fresh carousel shots (title menu, heavy-enemy
  combat with the new HUD) and both headless checks green (psx-demo-disc
  5940e76). Both images sit loose at the top of `~/Downloads/ps1 games/`.
- hl-psx was repinned to PSoXide b7b7b95b and lost its private
  `HAZARD_TRAMPOLINES` (hl 2152766): the runtime pin's psx-rt already
  exports one and the HL pressing had failed to link.

### The black triangles

Per-frame captures of the tram (`--route-screenshot-interval 4`) plus the
seam census locate them: holes inside near-plane-crossing wall quads that
take the view-space quadtree, touching the screen edges. A GP0 dump of one
such frame (`--stop-at-poll 1013 --dump-hw --dump-draws`, tick 3684) proves
the wedge is uncovered by any primitive; its two neighbours share the hub
vertex (259,157), and the frame also contains a degenerate leaf
`(260,157) (450,373) (450,373)`. Four experiments on that exact frame, each
a rebuild and a poll-bound dump:

| change | wedge at poll 1013 |
|---|---|
| per-leaf cull skipped when the parent quad is front in view space | unchanged (removed a left-edge wedge at tick 3680) |
| all three raw-path culls moved to a view-space determinant (`culled_view`) | unchanged |
| minimum two-unit extent before a cell halves | unchanged |
| cell prune (lateral and vertical) disabled | unchanged |
| every soft-path cull disabled | unchanged |

So the missing leaf is never generated, and the degenerate one says the
cell already had two coincident corners.

Guest-side logging (patch `hl-black-wedge-instrumentation.patch`, log
`hl-black-wedge-poll1013-guest-log.txt`) then established: the quads whose
trees emit degenerate leaves are cooked quads with three collinear
corners, a triangle carrying a T-junction vertex on one edge (root
`(17,99,-118) (17,83,554) (17,99,338) (17,99,554)`, all four corners on the
wall plane x = 17); routing such quads through the triangle near-clip path
removes every degenerate leaf but leaves the wedge; the leaves that should
cover the wedge are emitted with sane UVs, colours around 80 and front
windings, and no packet push fails in those frames. Yet the dumped frame's
packet list has no triangle over the wedge. The dumped list is the last
frame the GPU consumed, which lags the guest's emission by a frame or two,
and in it the hub fan comes from the native 2x2 child path rather than the
quadtree, so the two views of the frame were never the same frame. What
is needed next is a harness that pins one guest frame: build with
`emulator-telemetry`, stop on the frame counter rather than a poll, and
dump packets and guest log for that same frame. Nothing from this
investigation is shipped; hl main stays at 2152766. First suspect for
that harness: the shipping 2x2 child path (`emit_affine_quad_children`)
pushes its four GT4 children unchecked, while the bow-tie, near-plane and
guard-band `safe` test with a triangle fallback exists only on the
tram-grid policy; a near child whose re-projected midpoints clamp to
+-1023 becomes an oversize or self-crossing quad the GPU silently drops,
which is a hole bounded by the fan's own edges that moves with the camera. The view-space facing verdict and the extent guard are kept as a
patch (`hl-view-space-facing-and-min-extent.patch`, tram fps unchanged)
rather than shipped, since they do not fix what Manny sees. Evidence:
`hl-black-wedges-census.png`, `hl-black-wedge-tick3684.png`.

Measured 17:30: that suspect built (`affine_child_safe` on every 2x2
child, triangle fallback otherwise) leaves the wedge in place at polls
1012, 1013 and 1014. Not shipped either; the edit stays uncommitted in the
hl-psx tree. The frame-pinned harness is the next step, not another guess.

Note for anyone measuring hl-psx visually: two runs of the same build do
not produce the same frames at the same tick (the tram route is
timing-dependent), so before/after comparisons must be poll-bound dumps
of one frame, never interval screenshots from two runs.

## VoXide: renderer, world generation, draw distance (2026-09-03, 12:30 to 16:30)

Manny's ask: push the VoXide renderer and world generation, and show more
blocks. Bench protocol: the spawn meadow, standing (`make profile`'s scene;
START pulse at tick 700, 900M steps) for per-stage cycles from the telemetry
build, and a poll-bound run for whole-route bus cycles. The pinned frontend
and the current one agree to the cycle. `--hold-forward` walks into a bush at
spawn, so there is no real walking route yet (VoXide turns with the right
stick only).

### Shipped on main (voxide e35323c), exact

- The tint table (`refresh_mat_ccmd`, 192 divides) was rebuilt every frame;
  its inputs move a few times a minute. Keyed on them.
- The near-cell clipper zeroed two 384-byte polygon buffers per call and
  copied both on every clipped plane; memset was 3% of the frame. Static
  scratch, swapped by reference.

Spawn loop body 1,617,041 to 1,403,405 cycles (-13%); the walking route to
pad poll 1200 went 2,596,795,601 to 2,044,981,625 bus cycles and its frames
from four fields to three. Frames are pixel-identical apart from game-time
drift.

### Measured and dropped

Prepacked plant packet words (no change: the builders were already folded);
per-layer tiling of the cave noise in world generation (terrain hash
identical, boot cycles unchanged: cave sampling is not where generation time
goes; the streaming cost the README describes is chunk meshing, 90K plane
cells per chunk). The remaining spawn profile is flat and load-bound: the
face loop iterates about 1,260 faces to emit 291 quads, the near band spends
80K instructions on 47 triangles, plants 9%, vsync wait 16%.

### More blocks: a far LOD (branch `lod/far-tops`, 0f783fe, not on main)

Raising the far plane alone does not work: 20 blocks costs +23% loop body,
24 blocks +42%. The branch adds a real LOD for the outer ring of the 5x5:
the column heightfield at two-block cells, ground blocks only, tops merged
2D, plus a skirt face wherever a neighbour cell is lower so terraced hills
keep their silhouette. A far chunk holds a few dozen faces instead of a 90K
cell scan, so the wider ring adds little streaming load, and far chunks
publish no plants. Far plane 24 blocks, near-ring sides still 16.

| pose | release tree (16 blocks) | far LOD (24 blocks) |
|---|---|---|
| spawn, standing | 1,403,405 (20 fps) | 1,633,378 (20 fps) |
| ground, yaw 0x0C00 (bushes, hill) | 1,616,044 (20 fps) | 1,881,283 (15 fps) |
| aerial (VISTA_AERIAL) | 785,048 (30 fps, all haze) | 1,464,373 (20 fps) |

Loop-body cycles, telemetry builds. The same renderer at 24 blocks without
the LOD measured 1,987,221 at the spawn. So: half again the draw distance at
the release's frame time in the spawn view, a 20 to 15 drop on the heaviest
ground pose, and a landscape where the release shows fog from the air.
Manny judges whether that trade ships; the branch is pushed, main keeps only
the exact wins. Evidence: `voxide-aerial-16-20-24.png`,
`voxide-lod-tops-vs-skirts-aerial.png` (why the skirts exist),
`voxide-spawn-16-vs-24-lod.png`.

### Second pass on main (2026-09-03, 17:00 to 18:30), exact

Two more wins measured on the same spawn scene with the telemetry build,
both with the display hash of the shipping build unchanged
(0xbab32d639647f0cd at pad poll 1200):

| step | loop body | face loop | note |
|---|---|---|---|
| main e35323c (tint memo, static clip buffers) | 1,403,405 | 957,108 | |
| plants and mob boxes pre-culled on the GTE (48cde9c) | 1,291,529 | 944,494 | world faces 1,136,286 to 1,032,675, mobs 81,083 to 72,779 |
| clipper buffers in the scratchpad (77a67d0) | 1,254,356 | 907,011 | 912 of the 1,024 bytes |

From the morning's 1,617,041 that is -22.4%; the 30 fps line is 1,142,476,
so this scene is 112k cycles short of two fields. Plants behind the camera
or off to the side were projected corner by corner and, with a corner in
the GTE near band, handed to the software clipper; one MVMVA on the cell
centre with the face loop's own sphere-vs-cone bound removes them before
that, and the same test on each mob box centre removes a herd behind the
player. The near-cell clipper's polygon buffers were main-RAM statics; the
scratchpad makes every load and store in it one cycle.

A measurement trap for whoever continues: `--stop-at-poll` bus cycles are
wall cycles. VoXide vsyncs, so a frame costs whole fields and three
different executables measured 2,044,981,625 to the digit; only the
telemetry build's "loop body" is a work metric. The prepacked plant packet
words measured "no change" that way for the same reason, and were reverted
before anyone noticed; they are still worth re-measuring with the telemetry
build.

