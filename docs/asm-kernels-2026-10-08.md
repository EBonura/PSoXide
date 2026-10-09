# Hand-scheduled kernels, 2026-10-08

Three kernels were written and measured against the three games that use shared
code the most (Half-Life, Quake, Cortex); the third, a batched projection macro,
was dropped (see below). Each one that is kept is opt in, the portable or
inline-asm code stays the default, and each is tested bit for bit by
`sdk/examples/hello-asmprobe` (random and edge inputs, a deliberately wrong kernel
to prove the harness can fail). This page records what each one measured and why
the whole-benchmark gain is smaller than the function gain.

## How the R3000 model rewards hand scheduling

Hwtest v1.23 on a console fixed the emulator's load rule (`hides_in_load_shadow`
in the emulator core): a RAM load costs seven clocks, and the instructions
behind it keep issuing as long as they neither touch the bus nor read the loaded
register. The first two pay their clock, the next four are free. LLVM does not
know this, so a loop of `load, use` pays the whole wait. All three kernels
spend their ALU instructions inside those shadows and issue loads one packet
or one lane ahead.

## What was measured

Per-function cycles come from the emulator's exact profiler
(`PSOXIDE_LIMIT_PROFILE`), which charges each retired instruction its real
cost including load shadows, store queue, GTE and multiply waits. Emulator
`c743674` (SWC2 interlocks like MFC2). None of the kernels uses SWC2.

| Kernel | Where | Function before | Function after | Whole benchmark |
|---|---|---|---|---|
| Tagged-stream OT link (`psx-gpu`, `asm-kernels`) | Quake e1m1 chain bench | 170.5 M cycles in `gpu_end_frame` (6.05 M packets, 28.2 per packet) | 140.5 M (23.2 per packet), -17% to -19% | -0.18% summed function cycles, 28.80 to 28.85 fps |
| Eight-lane one-plane clip (`psx_math::clip_lanes`) | Half-Life, Manny's chapter-three recording (polls 391 to 4095), GoldSrc view clip | `clip_view_plane` 178.6 M cycles (94 k calls, about 1,900 per call) | 103.1 M (about 1,100 per call), -42%. With the plane setup now inlined into the caller, `clip_view_plane` plus `visible_clip` goes 275.4 M to 214.1 M, -22% | 18.670 to 18.801 fps over all frames (+0.7%); chapter-two tape 18.269 to 18.413 (+0.8%). Free RAM unchanged at 17,264 B |

In-situ check of the clip kernel: a Half-Life build that runs the portable definition next to the kernel on every call and counts disagreements saw 0 in 94,061 calls over the same tape (count and all eight lanes of every output vertex).

Probe numbers (`hello-asmprobe`, cycles by the root counter, emulated):

* link: 28.6 to 23.0 cycles per packet (-19.6%), 2081 differential cases (2,641 cases in the whole probe).
* clip, typical HL polygons: 2609 to 1623 cycles per call (-38%), 1600 differential cases. The same kernel measured 2476 to 1293 (-48%) in an earlier layout of the probe: the harness and the kernel share I-cache sets, so read the in-game numbers above first.

## The Half-Life build, and what the vertex layout cost

Half-Life needed `SVert` to be `repr(C)` so that the kernel can read it as eight lanes
(the default layout puts `x` somewhere else; the build proves the offsets). That change on
its own, with the profile re-collected, made Half-Life 0.8% slower (18.517 fps on the same
tape, `memcpy` 36.8 M to 69.8 M cycles, because `SVert` passed by value now goes through
memory). The kernel, measured against that same source without the kernel call, is worth
+1.5% (18.517 to 18.801). Taking `SVert` by reference in the emitters that pass it by value
should recover the 0.8%; that is a Half-Life change, not part of this branch.

## Why the whole benchmark barely moves

In the Quake bench the cycles the link kernel saves reappear one for one in the
wait loop that follows it in `gpu_end_frame` (123.3 M there against 94 M
before): at that point the CPU is waiting for the GPU, so earlier CPU work buys
nothing. The same applies to anything that runs ahead of a GPU-bound present.
Judge these kernels by the function column, and expect real frame-time wins
only in games that are CPU-bound at the point the kernel runs (Half-Life and
Cortex are; Quake's chain route is partly GPU-bound).

## Targets that were not worth a kernel

* A batched in-place RTPT projection kernel (next triple's loads under the running
  RTPT) won 31% in a probe with records in main RAM and 22% on Quake's projection
  loops (its records live in the scratchpad, so only the GTE wait is recovered), but
  0% on Cortex's, and the whole Quake bench moved by 0.06%. No game adopts it, so it
  was removed from the branch.

* GTE waits are small: `MFC2`/`CFC2` wait time is at most 1.56% of cycles in
  Quake, 0.64% in Half-Life and 1.24% in Cortex, and the model projection
  loops are already software pipelined.
* `attributed_clip` itself is generic over the vertex type and the plane
  adapter, so it cannot be one assembly routine. The only hot adapter is
  GoldSrc's view clip (Half-Life, Counter-Strike); `clip_lanes` is that
  adapter's shape. Cortex spends 0.25% and Quake 0.4% in clipping.
* `psx_rt::mem` memcpy is already hand written and bound by RAM loads (no data
  cache); a 32-byte unroll would save about one cycle in nine.
* `SectorReader::read_sector`, `wait_vblank` and `psx_pad::poll_state` are
  spin loops: their cycles are waiting.

## Using the kernels

* `psx-gpu`: build with `features = ["asm-kernels"]`. `OtFrame::add_tagged_packet_stream_unchecked`
  then runs the kernel; the slot-shift variant and the packed-command passes
  are untouched.
* `psx-math`: `clip_lanes::clip_lanes8_plane` with `asm-kernels`.
