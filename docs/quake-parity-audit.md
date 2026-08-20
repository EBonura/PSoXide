# Quake world-system parity audit

Status: first evidence-backed pass, 2026-08-20. This is a living checklist,
not a claim that the items marked partial are already good enough.

## What "Quake parity" means here

PSoXide uses Quake's brush/BSP world architecture, but that does not by itself
mean that every Quake renderer, compiler, physics, and environment feature is
present. Track three claims separately:

1. **Shared implementation:** the same Rust crate or algorithm is used by
   PSoXide and the current Rust Quake-PSX.
2. **Behavioral parity:** a different implementation is validated against the
   same source behavior and produces the same relevant result.
3. **Intentional replacement:** PSoXide keeps its own system because it is a
   general engine/editor rather than a Quake game port.

Do not describe a subsystem as inherited merely because an older renderer
revision contained something with the same name.

## Reference baselines

- Original Quake source: `Quake` revision
  `bf4ac424ce754894ac8f1dae6a3981954bc9852d`.
- Current all-Rust Quake-PSX: `quake-psx` revision
  `e32f6f66cff1759954f224846ce0b326c3d55d30`.
- Historical C `quakepsx` revision
  `9e1bbc4371915a050cb26cc2b9ff488bc35fea20` is useful for PS1 budgets and
  gameplay, but it explicitly leaves sky and water rendering as TODOs. It is
  not the feature-parity oracle for those systems.

The original source is the semantic oracle. The current Rust Quake-PSX is the
preferred PS1 implementation source when it has already solved and validated
the feature.

## Current matrix

| System | Current PSoXide status | Decision | Priority / parity gate |
| --- | --- | --- | --- |
| Editable convex brushes, CSG and BSP construction | Implemented natively in the editor. The current E1M1 project cooks more than 1,200 editable brushes through this path. | Keep the native authoring workflow; compare compiled topology and contents behavior with Quake tools. | P0 correctness. E1M1 must remain editable, cook without a source-BSP sidecar, and boot with stable geometry. |
| Portals, outside fill and PVS cook | **Implemented with a leak gate and pointfile.** Draft uses conservative connected-component rows. Release runs Quake's directed portal/separator flow only after entity-seeded outside fill proves the world sealed; a leak preserves conservative PVS and writes `brush_world.pts`, matching Quake QBSP's refusal to emit a portal file for full VIS. The editable E1M1 world is now sealed. | Keep the general flow implementation. Compare sealed Release visibility against the original BSP at representative view cells. | P0 topology, then P1 performance. No false negatives; a leaking map must be diagnosed rather than silently full-VISed. |
| Runtime BSP/PVS world traversal | Implemented and derived from Quake: camera-leaf lookup, RLE PVS, visible-face marks, node reachability, node AABB/frustum rejection, front-to-back traversal and a compact fallback chain. | Keep converging this inside the shared `psx-bsp` crate. | P0. Headless captures, packet counters and static world GPU output must remain equivalent across optimizations. |
| Classic affine surface rendering | Shared SDK implementation: camera-space subdivision, GTE projection, affine textured quads/triangles and crack underdraw. | Keep shared rather than forking editor/game variants. | P0. Golden packet/frame hashes plus hardware-timing gates. |
| Editor brush preview | **Host-side equivalent, not the guest renderer.** It renders editable polygons and now applies the runtime's near plus four guarded-side clipping policy before triangulation. Its arena and OT remain host-preview implementations. | Keep shared correctness tests and caches where practical. Do not imply that an editor-preview artifact proves the guest renderer has the same bug. | P0 usability. Full E1M1 navigation must remain responsive, with no near-plane holes or capacity truncation. |
| Hull collision and point contents | Implemented in `psx-bsp`: Quake-style clipnodes/hulls, segment traces, transformed submodels, and solid/water/slime/lava contents. | Keep as a shared core and continue trace-oracle tests. | P0. World/player hull traces and long probes must remain stable on E1M1. |
| Player movement policy | Intentional PSoXide replacement: third-person motor, fixed 60 Hz simulation, step/slope/camera rules. It consumes Quake-style hull queries but is not Quake movement. | Keep PSoXide behavior. Import only generally useful collision fixes, water-state semantics, and diagnostics. | Product-specific; no Quake feel-parity claim. |
| Brush submodels / movers | **Partial.** Translated door submodels render and collide. Rotation/scale are rejected, liquid movers are unsupported, and visibility uses one representative leaf instead of the full touched-leaf span. | Port the general transformed brush-model and touched-leaf/linking semantics; retain editor-native behavior components. | P1. Doors/platforms must render, collide, block, reopen, and remain visible across leaf boundaries. |
| Static lighting | Intentional first-stage replacement, but incomplete versus Quake: editor point lights and ambient are baked into vertex RGB. There are no Quake surface lightmaps or animated lightstyles. Geometry does follow lights placed in the editor at cook time. | Keep editor lights as the authoring source. Evaluate optional lightmap/lightstyle support where vertex light lacks the required fidelity; do not bypass lighting for imported maps. | P1 visual fidelity. Deterministic bake tests and editor-to-game comparisons. |
| Dynamic lights / lightstyles | **Missing as Quake systems.** PSoXide has other actor/effect lighting paths, but not Quake's surface lightmap style composition. | Import only if required by authored levels; lightstyles are more valuable than a wholesale dynamic-light renderer on PS1. | P2, after static correctness and sky. |
| Quake layered sky | **Implemented in the shared PXBSP path.** The cook accepts Quake's adjacent foreground/background pair, visible sky faces act only as apertures, and one constant-cost screen-space view-ray lattice renders the independently scrolling layers. PSoXide's panorama/cyclorama remains a separate authored mode. | Keep the current Rust Quake-PSX projection as the oracle and add representative authored-map captures as sky materials enter projects. | P0 environment parity. Integer tests cover translation invariance, direction, seams and constant packet cost; a real-project guest capture remains the final content integration gate. |
| Six-face skyboxes | Not an original software-Quake requirement. PSoXide's panorama/far-vista system is a separate engine feature. | Keep as a separate authored option. Name the Quake feature "layered sky" or "sky aperture" to avoid conflating it with a cubemap. | Optional. |
| Liquids | **Partial.** BSP contents, collision sampling, hazards, and general UV-scroll materials exist. Quake turbulent surface deformation, underwater view warp/tint parity, water transitions, and all related movement semantics are not one complete inherited system. | Port renderer turbulence and the useful camera/audio transitions; keep PSoXide damage/movement policy configurable. | P2. Validate water/slime/lava contents, surface animation, camera state and packet budgets separately. |
| Animated/special textures | PSoXide supports authored UV scroll and flipbooks. This is not the same import contract as Quake's `+0` texture chains and special-name rules. | Keep the more general material animation system; translate Quake naming conventions during Quake map/material import. | P2 importer convenience. |
| Entity PVS linking | **Partial.** Runtime points can be culled with cooked PVS, and alias entities carry one leaf index. Large entities and movers are not linked through every touched leaf as Quake efrags/`FindTouchedLeafs` do. | Add compact touched-leaf spans or an equivalent bounds-to-leaves result. Do not copy pointer-heavy efrag storage literally. | P1 correctness. Crossing a portal must not pop a visible large actor or mover. |
| Quake point entities, triggers and `func_*` import | Deliberately excluded from the geometry-only E1M1 import. PSoXide has its own typed scene nodes, logic graph, actors and doors. | Add explicit import adapters for useful Quake classes; never create hidden one-map runtime exceptions. | P2 workflow. Each mapping needs a documented semantic conversion and an unmapped-entity report. |
| Ambient leaf sounds | Not inherited as a Quake BSP feature. | Consider importing the leaf-ambient concept through PSoXide's audio system, without copying Quake's fixed sound set. | P3 atmosphere. |
| QuakeC/progs VM, networking, savegames, console/cvars, first-person gameplay and MDL-only actors | Intentional replacement. PSoXide already has editor-authored components, skeletal animation, combat/UI, fixed-tick simulation, and its own asset formats. | Do not import wholesale. Reuse isolated algorithms only when they serve the engine. | Out of parity scope. |
| Texture/asset formats | Intentional PSoXide replacement: PSXT/PXBSP/PSXMDL and editor resources. 4bpp is the default world/model policy; 8bpp remains supported for rare authored exceptions. | Preserve formats and budgets; import Quake material semantics, not raw runtime ownership. | Ongoing budget gates. |

## Immediate convergence order

1. Keep full-map editor interaction cost and preview correctness under the
   new E1M1 benchmark and visual regressions.
2. Validate layered-sky content with a representative authored map while
   keeping the PSoXide panorama as an explicit alternate mode.
3. Validate sealed E1M1 Release VIS against the original BSP at representative
   view cells. The Quake-style pointfile path is now available for future
   leaks; Draft and leaking maps retain conservative rows.
4. Generalize touched-leaf entity/mover linking and complete brush-submodel
   behavior beyond translated doors.
5. Add liquid turbulence/underwater presentation and, if the art direction
   needs it, optional lightstyle/lightmap support.

Every import should land in a shared engine/compiler module with a focused
oracle test. A project-name check, hidden source-BSP sidecar, or E1M1-only cook
branch fails the parity goal even if its screenshot looks correct.

## VIS and outside-fill implementation

Release VIS follows the original `qutils/VIS/FLOW.C` pipeline at its default
separator test level: directed portals, `BasePortalVis` might-see floods,
least-complex-first portal flow, source/pass separator clipping, and leaf-row
union. Completed portal results are published through a bounded dynamic worker
queue; in-flight portals use the conservative might-see bits, the same
correctness rule as Quake's `stat_working` jobs. Draft keeps the much cheaper
connected-component rows.

QBSP outside fill is now a prerequisite for exact VIS. Six finite headnode
portals use Quake's 24-unit `SIDESPACE`; scene player occupants seed an interior
portal flood one unit above their feet pivot. If that flood does not reach the
infinite exterior, unreachable exterior leaves become solid and Release runs
exact VIS. If it does reach the exterior, the cook reports a leak, writes the
entity-to-exterior portal-centroid trail to `brush_world.pts`, and retains
conservative PVS. Focused tests cover sealed fill, leak preservation and trail
shape, aligned portal chains, right-angle occlusion, disconnected leaves,
packed Release/Draft mode selection, and deterministic cooks.

Current E1M1 evidence (2026-08-20): the apparent leak had two concrete causes,
not a VIS exception. The benchmark discarded the authored spawn for a test
point that the original BSP classifies as solid, and the saved editable project
was missing source structural brush 50. The player occupant sample now respects
the engine's feet pivot, that one brush has been restored without regenerating
the other edited brushes, and E1M1 fills 879 unreachable exterior leaves. The
saved Release project runs 7,260 directed portal jobs across 1,345 visible
leaves in about 3.3 seconds and cooks to a 508,864-byte PXBSP with a 54,367-byte
exact PVS. The shipping/default guest build reaches gameplay at a validated
first-room third-person test spawn with textured, baked-lit BSP, player and
Mantis visible; captured display hash `0x914b42c4d5543943`.

## Sky implementation source

The shared layered-sky implementation was ported from these current Rust
Quake-PSX paths, not reconstructed from the historical C port:

- `crates/quake-cook/src/geometry.rs`: detects `sky*`, preserves the full
  256x128 source pair, and marks layered sky.
- `crates/quake-core/src/sky.rs`: camera-direction projection, vertical
  flattening, seam-safe packet UV rebasing, and translation/parallax tests.
- `game/src/renderer.rs`: visible sky faces select a material but emit no
  polygon; one bounded view-ray lattice is placed in the farthest ordering
  table slot.

Original semantic references are `WinQuake/r_sky.c` and
`WinQuake/gl_warp.c`. The important invariant is that brush geometry defines
where sky can be seen, not how the sky texture is projected.
