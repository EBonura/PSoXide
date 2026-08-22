# Shared engine standardisation: PSoXide, Quake-PSX and HL-PSX

## Objective

The target is one maintained PS1 engine stack with game-specific adapters, not
one monolithic renderer. Quake, GoldSrc and PSoXide differ in authored formats,
visibility policy, lightmaps and gameplay semantics. They do not differ in PS1
hardware, fixed-point/GTE projection, clipping mathematics, GPU packet formats,
scratchpad constraints, DMA ownership or validation requirements.

Every migration must be behavior-exact first. A shared implementation is kept
only after each affected game independently passes its canonical performance,
visual, RAM, packet-capacity, loading and gameplay gates.

## Ownership boundary

| System | Shared engine responsibility | Game adapter responsibility |
|---|---|---|
| PS1 runtime | interrupts, DMA, CD/SPU/GPU access, frame ownership | scheduling and presentation policy |
| Math/GTE | fixed-point primitives, transforms, projection kernels | coordinate-system and camera conventions |
| Scratchpad | physical address, alignment, host backing, bounds, clear | byte layout, phase lifetimes, overlap proof |
| GPU packets | packet types, OT insertion, material/window encoding | surface ordering, blending policy, authored material lookup |
| Clipping | attributed near/guard-band polygon kernel | input surface topology and fallback thresholds |
| BSP/PVS | common decoding/traversal primitives where formats agree | Quake, GoldSrc and PXBSP visibility policy |
| Models | common resident readers and projection/packet kernels | Quake alias models versus skeletal HMD8/PSXMDL adapters |
| Streaming | pack/CD primitives and residency planner contracts | scene manifests and game-specific transition state |
| Gameplay | reusable motor/entity components where semantics match | Quake QC behavior, GoldSrc entities and Cortex-specific rules |

Dynamic dispatch must not enter a PS1 hot loop. Adapters are monomorphized or
resolved at load/cook time so standardisation does not add per-face or
per-vertex overhead.

## First retained convergence: CPU scratchpad

Before this change all three games used the same 1 KiB hardware scratchpad in
three incompatible ways:

- PSoXide exposed a private `psx-engine` address helper.
- Quake-PSX hardcoded `0x1f80_0000` in its renderer.
- HL-PSX carried a separate absolute symbol, host backing and pointer helper.

`psx_engine::scratchpad` is now the shared address/alignment authority and is
available to host tests. Each game deliberately retains its existing byte
layout: Quake keeps its 1,020-byte batch, PSoXide keeps its 896-byte blended
model batch and affine workspace, and HL-PSX keeps its phase-overlapped layout
and guarded projection-stack trampoline.

### Acceptance evidence

| Game | Gate | Result | Decision |
|---|---|---|---|
| HL-PSX | complete packed disc compared with the pre-change disc | all 481,061,616 bytes exact; model/animation and RAM audits passed | retain |
| Quake-PSX | two deterministic E1M1 visual captures | framebuffer and GPU logs exact; same post-brightness world hash | retain |
| Quake-PSX | fixed-three-ticks E1M1 route, two runs/build | 22.282 -> 22.270 fps; -0.012 fps versus documented +/-0.122 layout noise; display/VRAM and gameplay probe exact | retain |
| PSoXide | native tests and release MIPS engine build | scratchpad and blended-chunk tests pass; `psx-engine` MIPS build passes | retain engine seam |
| PSoXide project | full editor-playtest link | blocked outside this change: generated UI manifest has more than the four-font cap | rerun when UI work lands |

The Quake visual regression's hardcoded expected world hash is stale after the
brightness change: both the unmodified renderer and candidate deterministically
produce `0x951a75babd8f2904`, while the oracle still expects
`0x66082c53b0c45ec8`. This is not attributed to the scratchpad migration.

The isolated three-game candidates above were built from a staged SDK that
contained the shared scratchpad change on top of the pinned engine. After the
gates completed, both game repositories were rehydrated from the real PSoXide
workspace. Quake's source/content check passes there. HL's live recompile is
currently blocked by unrelated in-progress API skew in that dirty workspace
(`psx-engine` references UI/material APIs not yet present in its hydrated
`psx-gpu`/`psx-level`). This does not invalidate the byte-exact isolated
candidate, but the live integration build must be repeated once those
concurrent edits are coherent.

## Second retained convergence: attributed clipping and fixed-point policies

All three renderers now consume one allocation-free
`psx_engine::attributed_clip` traversal. The kernel owns convex edge walking,
historical output order, fixed-capacity emission and `MaybeUninit` output. A
monomorphized adapter still owns the signed-distance domain and selects one of
the shared measured interpolation primitives:

| Adapter | Retained distance/interpolation policy | Reason |
|---|---|---|
| HL-PSX | canonical endpoint order, overflow-safe Q12 ratio and truncating wide mix | adjacent GoldSrc triangles must produce the same clipped edge; coordinates can exceed the narrow multiply domain |
| Quake-PSX | cached transformed depth, Q12 fraction and rounded bounded mix | the cache avoids retransforms; the existing rounded policy is visually exact and no slower than canonical truncation |
| PSoXide PXBSP | exact world-plane `i64` distances, Q16 fraction and wide mix | same-tree E1M1 proved Q16 smaller and faster than reducing the exact plane result to Q12 |

This is one clipping system with compile-time numeric policies, not three
loops and not runtime dispatch. Standardising the control flow does not mean
forcing every coordinate domain through the same precision when the console
measurements reject it.

### Acceptance and rejection evidence

| Game/candidate | Visual/state result | Performance/size result | Decision |
|---|---|---|---|
| HL shared traversal + canonical Q12 | final framebuffer exact; 506 common guest frames have zero non-time state differences | room `-0.135%`; whole render `+0.142%`, localized to unrelated model-code link placement; executed clip body is 396 B smaller and linker size is held stable with unreachable padding | retain shared seam; track global section layout separately |
| Quake shared traversal + existing rounded Q12 | frame, world, HUD and GPU output byte-exact; deterministic E1M1 route passes twice | retained fixed route is 22.258 fps | retain |
| Quake canonical truncating Q12 | same display/VRAM/gameplay hashes as rounded Q12 | 22.262 fps (`+0.004`, below the documented 0.122-fps layout-noise floor) and +272 guest bytes | reject arithmetic change |
| PSoXide canonical Q12 | static world remains exact; changed fixed-tick pixels are confined to animated actors/cadence | room about `+0.12%`, whole render `+0.16..0.18%`, +2 KiB linked sector, deadline misses 6 -> 31 | reject arithmetic change |
| PSoXide shared Q16 helpers | all 119 focused shared/BSP tests pass | final telemetry MIPS executable is byte-identical to the local-Q16 control (`a3402530...`) | retain |

Quake's visual command still reports the previously documented stale expected
hash (`0x66082c53b0c45ec8`); rounded control, canonical experiment and final
shared-helper build all produce the same `0x951a75babd8f2904`. HL's anomaly
route also still ends on the pre-existing primitive-arena proof overflow in
both control and candidate, after writing the complete comparison artifacts.

## Third retained convergence: projection, outcodes and draw surfaces

The shared projection layer is deliberately a zero-cost facade rather than a
new runtime abstraction. `psx_engine::projection` directly re-exports the
scheduled RTPT/RTPS primitives, owns the common screen/guard-band outcode
operations, and retains the established per-renderer rejection rules. HL keeps
its plane order and early exits, while the classic-affine path keeps its exact
triangle and quad pair tests. There is no dynamic dispatch or out-of-line call
in a projected-vertex or packet loop.

The cooker contract is a separate dependency-free crate,
`psx-render-contract`. It pins two little-endian records:

- a 10-byte convex `CookedDrawSurface` header shared by Quake and PXBSP cooks;
- a 24-byte packet-ready `CookedDrawSurfaceCommand` used by the opt-in GoldSrc
  source-world pipeline.

The contract intentionally does not own PVS policy, material animation,
lighting, subdivision, packet ordering or source BSP parsing. Those remain
compile-time/load-time adapters. Shipping PXBSP still performs its established
direct byte loads, so the shared host contract does not add checked decoding to
the guest hot path.

### Acceptance and architecture evidence

| Gate | Result | Decision |
|---|---|---|
| PSoXide E1M1, 120 visual frames | framebuffer, VRAM, eight fixed-tick screenshots, 24 non-time counter fields across 288 common frames and 23 GPU fields across 328 ticks are exact | retain |
| PSoXide cadence | 288 simulation ticks, 120 visuals, 144 skipped vblanks, 6 deadline misses and 23 lateness vblanks in both builds | exact |
| PSoXide stage timing | render `-0.017%`, room `-0.063%`, visual task `+0.011%`; all are neutral at this route's measurement scale | no regression |
| PSoXide native contract/engine/BSP tests | 2 + 346 + 114 pass; final editor cooker tests 12 + 7 pass | retain |
| Quake final cooker integration | all 30 cooker tests pass with the dependency-free contract | retain host boundary |
| Quake MIPS integration before contract extraction | complete guest build passed with the same projection/outcode runtime change | retain; rerun the final extracted recipe when the external execution gate is available |
| HL default guest integration before contract extraction | clean-target MIPS build passed; `hl-bsp` passed all 155 tests | retain the runtime seam; rerun the final extracted recipe and complete Hazard Course tape before claiming a final three-game performance gate |

The subsequent canonical gameplay audit narrowed those open claims:

- Quake's fixed-step E1M1 route walked all 60 waypoints and mechanisms twice,
  transitioned to E1M2, and retained exact display/VRAM hashes. Throughput was
  22.282 fps control versus 22.258 fps shared candidate, a -0.024 fps delta
  inside the suite's measured +/-0.122 fps identical-work link-layout spread.
- HL's 12,265-poll Black Mesa Inbound train route retained byte-exact final
  display and VRAM, but delivered 13,767 flips versus 13,793 control (-0.19%).
  Route-normalised throughput fell 21.874 -> 21.814 fps (-0.27%), the median
  gameplay window fell 18.596 -> 18.370 fps, and below-19.5 windows rose
  57 -> 58. The final input sample was consumed between route-log rows, so the
  analyzer reports index 12,263 rather than 12,264 even though the final poll
  count is complete. This is a performance rejection pending isolation, not a
  three-game no-regression result.

One rejected experiment is architecturally important. Making `psx-engine`
depend on the draw-format crate changed PSoXide's guest link layout and
deterministically degraded pacing: simulation ticks rose 288 -> 370, skipped
vblanks 144 -> 185 and deadline misses 6 -> 47. Repeating the build reproduced
the result. The dependency was removed from the hot guest engine; cookers and
opt-in readers now depend directly on the small contract crate. The final
layout returns to exact cadence and visual/state output.

Hydrated external SDKs also exposed a stale-object hazard: copied source files
could retain an mtime older than the previous SDK artifacts, allowing Cargo to
reuse obsolete rlibs. `psoxide-link` now stamps hydrated files after copying so
dependency changes force the required rebuild.

## Ordered convergence queue

1. **Model residency and animation.** Finish moving HL-PSX's HMD8 reader and
   skeletal projection through `psx-asset`/`psx-engine`. Quake alias models
   remain a distinct adapter to the shared classic-affine packet path.
2. **Frame submission and streaming.** Centralise DMA/OT/framebuffer lifetime
   and scene asset residency contracts after per-game timing traces prove the
   same ownership schedule is safe.
3. **Packet emitter convergence.** Move only the byte-exact common packet
   assembly below the game adapters. Keep surface order, blending and material
   animation above it, and gate each migration independently on console timing.

Before continuing that queue, remove the PXBSP renderer's wide-integer hot
path. Its exact boundary fallback currently performs `i64` plane evaluation,
Q16 interpolation and an iterative `i64` square root during frustum setup; the
MIPS link map contains `__divdi3`/`__udivdi3`. Keep the Q16 *fractional
precision* if it remains visually necessary, but derive view-space distances
and the bounded ratio with scaled 32-bit operands. Acceptance requires no
wide-division helper reachable from the renderer plus exact E1M1 visual/state
output and a measured timing win.

## Non-negotiable acceptance gates

- PSoXide: exact guest-state/counter rows, GPU rows, screenshots, framebuffer
  and VRAM on the current Arena/E1M1 route.
- Quake-PSX: authored route probes, fixed-simulation performance route,
  framebuffer/VRAM hashes, packet overflow and resident-arena margin.
- HL-PSX: complete poll-bound Hazard Course tape, slow-window cadence,
  guest-aligned screenshots, VRAM, packet capacity and memory report.
- All games: exact input/gameplay timing unless a behavior change is separately
  requested; no hidden low-quality configuration and no performance claim from
  host wall time.
