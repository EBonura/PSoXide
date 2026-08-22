# Engine harmonisation across the PSoXide programs

Status: proposal for review and verification. Nothing here has been executed.
Written 2026-08-22 against PSoXide `7b79bfea` (branch `codex/arena-playable-slice`,
dirty tree), quake-psx `72ecdc2`, hl-psx `26e2365`, psx-demo-disc at its current
submodule set.

## 0. How to use this document

This is written for an agent that will double-check it before anything is
acted on. Two rules for that agent:

1. **Every factual claim below is labelled.** `[MEASURED]` means it was read
   out of the tree during this survey and a file/line is given. `[INFERRED]`
   means it is a reading of the evidence, not the evidence itself.
   `[VERIFY]` means it needs a check that was not run here. Treat `[INFERRED]`
   as a hypothesis to falsify, not a finding to repeat.
2. **Section 8 is the work list.** It restates every claim that must hold for
   the proposals to be worth doing, with the command or file that settles it.
   If a claim in section 8 fails, the proposal that rests on it dies. Say so
   rather than repairing the proposal to survive.

The existing companion documents are
[shared-engine-standardisation-2026-08-22.md](shared-engine-standardisation-2026-08-22.md)
(what has been converged and gated so far),
[quake-psoxide-convergence-handoff.md](quake-psoxide-convergence-handoff.md)
(the Quake/PSoXide campaign, with its architectural north star at section 6),
[downstream-projects.md](downstream-projects.md) (the canonical downstream repo
layout) and [finish-line-plan-2026-08-11.md](finish-line-plan-2026-08-11.md).
This document does not restate them. It asks a different question: **what stops
an optimisation landing in PSoXide from reaching every program that could use
it, and what would have to change for that to become the default rather than a
project.**

---

## 1. The programs, and how each one consumes PSoXide

`[MEASURED]` Guest-side Rust line counts, and the consumption relationship.

| Program | Guest LOC | Host/cook LOC | PSoXide consumption | Frame loop | World model |
|---|---:|---:|---|---|---|
| Cortex Ignition (PSoXide `engine/examples/editor-playtest`) | 12,891 | editor workspace | in-repo | `psx_engine::App`/`Scene` | PXBSP |
| quake-psx | 27,614 (`game/src`) + 22,270 (`quake-core`) + 1,292 (`quake-formats`) | 9,977 + 5,176 (`quake-cook`) | `psoxide-link` hydration, pin `9c298d83` | own loop (`game/src/platform.rs`) | XBSP/PSB5 |
| hl-psx | 54,978 (`game/src`) + 396 (`hl-format`) | 29,933 | `psoxide-link` hydration, pin `9c298d83` | own loop (`game/src/main.rs`) | HLMH (private) |
| voxide | n/a surveyed | n/a | `psoxide-pin` `9c298d83`, no `psx-engine` | own loop | voxel |
| gh-psx | n/a surveyed | n/a | `psoxide-pin` `9c298d83`, `psx-engine` | `App`/`Scene` | none |
| psxcel | n/a surveyed | n/a | `psoxide-pin` `9c298d83`, `psx-engine` | `App`/`Scene` | none |
| nitroxide | n/a surveyed | n/a | `psoxide-pin` `9c298d83`, `psx-engine` | `App`/`Scene` | none |
| pico8-psx (Celeste collection) | n/a surveyed | n/a | `psoxide-pin` `9c298d83`, no `psx-engine` | own loop | none |
| psoxide-arcade | n/a surveyed | n/a | `psoxide-pin` `588d7637`, no `psx-engine` | own loop | none |

`[MEASURED]` The SDK/engine itself: `psx-engine` 45,731 LOC, `psx-game-runtime`
26,154, `psx-bsp` 11,565, `psx-level` 6,192, `psx-render-contract` 207, plus
the eighteen `sdk/crates/*` (largest: `psx-font` 6,834, `psx-asset` 5,066,
`psx-gpu` 3,497).

### 1.1 The consumption mechanism is already unified, and that is a real win

`[MEASURED]` Every downstream program now hydrates PSoXide through
`tools/psoxide-link` into a local `.psoxide/` directory, with the pin owned by
Cargo in a `psoxide-pin/Cargo.toml` (or the repo root manifest for
quake-psx/hl-psx). `psoxide-link`'s own crate doc records why: four games had
vendored submodules that drifted eight commits behind a measured SPU fix.

`[MEASURED]` [downstream-projects.md](downstream-projects.md) still documents
only "mode A: pinned submodule" and "mode B: sibling checkout". The hydration
mode that all nine programs actually use is not in it. Three repos
(gh-psx, nitroxide, pico8-psx) still carry a `third_party/PSoXide` directory
alongside `.psoxide/`, which is exactly the "never both" state that document
warns about.

**Action:** `[VERIFY]` then update `downstream-projects.md` to describe
hydration as the canonical mode and delete the stale `third_party/PSoXide`
trees. Low cost, removes a documented-but-wrong instruction.

---

## 2. What is genuinely shared today

Recording this precisely matters, because several of the proposals below are
"finish an adoption that was started", not "start a convergence".

| Seam | Owner | Adopted by | Evidence |
|---|---|---|---|
| CPU scratchpad address/alignment | `psx_engine::scratchpad` (102 LOC) | Cortex, quake-psx, hl-psx | byte-exact HL disc, exact Quake captures, see standardisation doc |
| Attributed convex clip traversal | `psx_engine::attributed_clip` (472 LOC) | all three, each with its own numeric policy | standardisation doc §"Second retained convergence" |
| Projection scheduling + outcodes | `psx_engine::projection` (152 LOC) | all three | `engine/crates/psx-engine/src/projection.rs` |
| Classic-affine materialise/submit kernels | `psx_engine::classic_affine` (3,103 LOC) | Cortex, quake-psx | `quake-psx game/src/renderer.rs:8-17` |
| XBSP/PXBSP wire format | `psx-bsp` | Cortex, quake-psx (via facade), editor cook | `quake-formats/src/lib.rs:15` is `pub use psx_bsp::*` |
| BSP hull tracing | `psx_bsp::collision` | Cortex, quake-psx | `quake-core/src/collision.rs:1-25` is an axis facade |
| Brush mover interpolation | `psx_bsp::mover` | Cortex, quake-psx | `quake-core/src/mover.rs:1-13` |
| CD sector read state machine | `psx_pack::cd::SectorReader` | hl-psx | `hl-psx game/src/cdstream.rs:1-14` |
| SPU sample playback / voice pool | `psx-sfx` | quake-psx, voxide, nitroxide, launcher | `quake-psx game/src/audio.rs:7` |
| Telemetry stage/counter ids | `psx-telemetry` (52 stages, 267 counters) | all instrumented programs | `sdk/crates/psx-telemetry/src/lib.rs:168,826` |
| Cooked draw-surface records | `psx-render-contract` (207 LOC) | Quake cook, PXBSP cook, HL opt-in path | `hl-psx game/src/map.rs:80` |
| Headless runner | `emu/crates/frontend` `launch` | all | per-game harnesses shell out to it |
| SDK hydration/pinning | `tools/psoxide-link` | all nine downstream | `psoxide-pin/Cargo.toml` in each |

`[INFERRED]` The pattern that works is visible in this table: a **small,
dependency-light, behaviour-exact kernel** with the policy left above it.
`scratchpad` (102 LOC), `projection` (152 LOC) and `psx-render-contract`
(207 LOC) were all adopted by three renderers. The large things
(`psx-engine` at 45,731 LOC, `psx-game-runtime` at 26,154) were adopted by
nobody outside Cortex. Size and dependency weight, not domain fit, look like
the discriminator.

---

## 3. Five structural blockers

These are the reasons an optimisation does not cascade today. Every proposal in
section 5 attacks one of them.

### B1. Promote-without-adopt

`[MEASURED]` Code is repeatedly lifted into the SDK and then the origin project
keeps its private copy:

- `psx_pad::aim_curve` (`sdk/crates/psx-pad/src/lib.rs:228`) documents itself as
  "the response used by `hl-psx`, promoted here so other PSoXide games do not
  grow subtly different controller curves". hl-psx still calls its own
  `aim_curve` at `game/src/main.rs:4187`. quake-psx uses the SDK one.
- `psx_asset::hmd8` (`sdk/crates/psx-asset/src/hmd8.rs`, 1,302 LOC) documents
  itself as "lifted from hl-psx ... so both games share one format rather than
  each carrying a private reader". hl-psx still loads models through
  `game/src/model.rs` (1,249 LOC). A whitespace-normalised diff of the two files
  reports 395 differing lines out of 2,551, so roughly 85% is common text.
- `psx-sfx`'s crate doc names four programs that each rewrote voice management.
  hl-psx is a fifth: `game/src/sfx.rs` (643 LOC) drives `psx_spu` voices
  directly and does not use `psx-sfx`.

`[INFERRED]` The promote step is cheap (write a crate, add tests) and the adopt
step is expensive (bump a pin, rebuild, re-run a full acceptance route,
re-measure). So promotion happens and adoption does not. The result is worse
than not promoting: two implementations now exist, and the SDK one is the
*unproven* one because the game still ships the private copy.

### B2. Fork drift in the one place it costs the most

`[MEASURED]` `engine/crates/psx-bsp/src/render.rs:1-5` says: "Lifted from
quake-psx `game/src/renderer.rs` commit 83a6349, same GPL-2 authorship. Frame
lifecycle, packet storage and entity ownership are caller-supplied so this
module can serve both runtimes."

`[MEASURED]` quake-psx does not use it. `grep -o 'psx_bsp::[a-zA-Z0-9_:]*'` over
`quake-psx/game/src` returns only `psx_bsp::resident`. The two files are now
2,936 LOC (PSoXide) and 4,317 LOC (Quake) and have diverged:

- nine free functions exist in both by the same name: `add_normal`,
  `subtract_normal`, `animate_special_surface`, `special_texture_window`,
  `texture_window_mask`, `packet_capacity`, `front_facing`, `plane_distance`,
  `lerp_vertex`. `add_normal`/`subtract_normal` are byte-identical;
  `animate_special_surface` and `special_texture_window` have already diverged
  (PSoXide keeps the liquid UV wobble and the legacy sky-window rule, Quake
  dropped the wobble and moved sky to a staged `SkyWindowPacket`).
- `Renderer::materialize_face` differs only in which materialise kernel it calls
  (`materialize_classic_affine_word_vertices` vs
  `materialize_classic_affine_indexed_vertices`) plus PSoXide's
  `normalize_baked_color` saturation loop.
- `Renderer::mark_visible_faces` has diverged more: Quake's carries a
  `visible_faces` list, a water-portal PVS union and 4-faces-per-word packing
  that PSoXide's does not have.

`[INFERRED]` The Quake copy is ahead on visibility work; the PSoXide copy is
ahead on PXBSP material/animation. Neither improvement reaches the other. This
is the single largest concrete instance of the user's stated concern.

`[MEASURED]` A fourth-order case of the same thing: the Quake PVS run-length
decoder exists in **four** places.

| Copy | Location |
|---|---|
| canonical | `engine/crates/psx-bsp/src/pxbsp.rs:81` (`decompress_visibility`), used by both resident readers |
| guest fork | `quake-psx game/src/renderer.rs:4265` plus `merge_visibility` at 4294 |
| guest fork | `hl-psx game/src/main.rs:19591` (`decompress_vis`) |
| host fork | `editor/crates/psxed-project/src/brush_pack.rs:1189` (`decompress_visibility_row`) |

### B3. Pin economics make adoption all-or-nothing

`[MEASURED]` [session-handoff-2026-08-20.md](session-handoff-2026-08-20.md)
states the problem in the author's own words about the HMD8 lift: "hl-psx pins
`psoxide-link` at rev `9c298d83` with path deps into a local `.psoxide`
directory, so it cannot see this until that pin moves, and moving it pulls in
everything since. That is a deliberate bump, not a drive-by."

`[MEASURED]` At the time of writing `9c298d83` is 80 commits and five days
behind PSoXide `HEAD`. Every downstream program except psoxide-arcade sits on
that one pin. Bumping it means one game absorbs 80 commits of engine change and
must re-run its full acceptance route to attribute any regression.

`[MEASURED]` psx-demo-disc carries **four separate PSoXide checkouts at four
different revisions** on a single disc:

| Submodule | Revision | Purpose |
|---|---|---|
| `games/PSoXide` | `9c298d83` (2026-08-17) | Quake's frozen SDK input |
| `games/PSoXide-cortex` | `687d2ae7` (2026-08-13) | legacy Cortex image |
| `games/PSoXide-cortex-current` | `ded86107` (2026-08-21) | current Cortex image |
| `games/PSoXide-runtime` | `588d7637` (2026-08-21) | the ordinary programs' shared runtime |

`[MEASURED]` `588d7637` is not present in the user's main PSoXide checkout and
is not an ancestor of `origin/main`. In the demo-disc submodule clone its only
remote branch is `origin/codex/shared-game-runtime` and its `origin` remote
points at `/tmp/psx-demo-disc-shared-runtime/games/PSoXide-runtime`.
`[VERIFY]` whether that is only local clone state (`.gitmodules` names the
GitHub URL) or whether the shipped disc's shared runtime genuinely has no
published ancestor. If the latter, the disc is not reproducible from public
history.

`[MEASURED]` quake-psx is additionally exempt from even the demo disc's
one-SDK rule: `psx-demo-disc/Makefile:15` says "Quake's independently verified
SDK input stays frozen here", and `sdk-coherence` (Makefile:612) only iterates
`games/*/.psoxide/.psoxide-source`, which Quake does not have because its disc
is consumed as a pre-built, sha256-pinned `.bin`. So the program with the
largest shared-code surface receives zero SDK cascade on the disc.

### B4. Crate granularity fights the linker

`[MEASURED]` `psx-engine` is one 45,731-LOC crate containing the App loop,
scheduler, character motor (5,195 LOC), third-person camera (2,555), UI (2,496),
`render3d` (4,330 + submodules), `world_render` (3,322 + submodules),
`classic_affine` (3,103) and the small neutral kernels quake-psx and hl-psx
actually want (`scratchpad` 102, `attributed_clip` 472, `projection` 152).

`[MEASURED]` Nine of its modules depend on `psx-level` (`scene.rs`, `ui.rs`,
`world_render.rs`, `world.rs`, `character_motor.rs`, `transitions.rs`, `app.rs`,
`third_person_camera.rs`, `game_app.rs`), and `psx-level` is the editor's cooked
level manifest schema. So Quake and HL compile Cortex's level schema, font,
asset, SFX and SPU crates to reach a 102-line address helper.

`[MEASURED]` `psx-bsp` depends on `psx-engine`, so consuming the BSP format
readers pulls the whole chain too.

`[MEASURED]` This is not theoretical. The standardisation doc records a
rejected experiment: making `psx-engine` depend on the draw-format crate
"changed PSoXide's guest link layout and deterministically degraded pacing:
simulation ticks rose 288 -> 370, skipped vblanks 144 -> 185 and deadline misses
6 -> 47. Repeating the build reproduced the result." hl-psx's release profile
sets `trim-paths = "all"` for the same class of reason: its manifest comment
records that building from a path 20 characters longer crossed a
`ALIGN(2048)` step and failed the link with ".bss will not fit in region RAM".

`[INFERRED]` On this target, dependency-graph shape is a performance variable.
That cuts both ways: it is an argument for splitting `psx-engine` into small
crates so a game links only what it uses, **and** a reason every split must be
gated on a cadence measurement, not assumed neutral.

### B5. No shared acceptance harness, so adoption cost is dominated by proving it

`[MEASURED]` Each program has rebuilt the same headless-verification
primitives:

| Primitive | quake-psx | hl-psx | PSoXide |
|---|---|---|---|
| PPM decode + region hash | `host/quake-build/main.rs:311-335` (`ImageRegion`, `PpmImage`) | `host/hl-build/regression.rs:735-798` (`ppm_token`, `ppm_rgb`, `ppm_changed_pixels`) | `emu/crates/psoxide-validation` |
| profile CSV column extraction | in `host/quake-build/main.rs` | `regression.rs:605-642` (`csv_column`, `csv_column_occurrence`) | `tools/vblank_chart.py` |
| percentile / timing summary | in `host/quake-build/main.rs` | `regression.rs:799-889` | `tools/cortex_30fps_report.py` |
| frontend discovery + launch | in `host/quake-build/main.rs` | `regression.rs:567-604`, `890-1003` | Makefile targets |
| guest-stage hydration + isolated CARGO_HOME | `host/quake-build/main.rs:37-177` | `host/hl-build/main.rs` | `tools/build_guest_staged.sh` |
| scenario table | implicit in per-feature regression bins | `regression.rs:38-502` (29 + 14 + 5 + 16 scenarios) | validation suite RON |

`[MEASURED]` `emu/crates/psoxide-validation` exists and is exactly this, but its
only dependent is `emu/crates/frontend`. No game uses it.

`[MEASURED]` CI coverage: PSoXide has `ci.yml` + `release.yml`; quake-psx has
`itch-release.yml`; psxcel/pico8-psx/voxide have deploy or check workflows;
**hl-psx, nitroxide and gh-psx have no workflows at all**. There is no
cross-repo job that builds every program against one candidate SDK.

`[INFERRED]` B5 is the root cause of B1 and B3. Adopting a shared kernel is a
30-minute code change and a multi-hour verification. Until the verification is
one command that a machine runs, adoption will keep losing to "later".

---

## 4. Duplication ledger by subsystem

Verdicts: **share** (one implementation, adapters above it), **adapt** (shared
kernel with compile-time policy, the pattern already proven for
`attributed_clip`), **keep** (genuinely different, do not unify).

### 4.1 BSP world rendering

| Program | Implementation | LOC |
|---|---|---|
| Cortex | `engine/crates/psx-bsp/src/render.rs` | 2,936 |
| quake-psx | `game/src/renderer.rs` (world core ~lines 717-2832) | ~2,100 of 4,317 |
| hl-psx | `game/src/main.rs` world sections + `world_pipeline.rs` + `render.rs` | ~1,800 `[VERIFY]` |

`[INFERRED]` Verdict **adapt**, Cortex and Quake only. The two are the same
code with three axes of divergence: vertex source encoding (word vs indexed),
material/animation policy (PXBSP materials vs Quake texture flags), and
visibility policy (plain PVS vs PVS + water portal). All three are
compile-time-selectable. HL stays **keep**: its world pipeline is a patch
tessellation with per-face lightmap colour and its own admission/refinement
policy, which is not the same algorithm.

`[MEASURED]` The lowest-level kernels are already shared: both call
`psx_engine::classic_affine::materialize_classic_affine_*`. The duplication is
the layer immediately above.

### 4.2 BSP data format

`[MEASURED]` `psx-bsp` defines PSB (`PSX%`..`PSX5`) and PXBSP (`PXB%` v1..v6) in
one crate. `quake-formats` is a 1,292-LOC facade that re-exports it and adds
three Quake-only markers. `hl-format` is a separate 396-LOC constants module for
`HLMA`..`HLMH` with byte-offset constants rather than typed records.

`[INFERRED]` Verdict for Quake/Cortex: **done, keep as is**. Verdict for HL:
**keep the format, share the container** (see H9). HL's world data genuinely
differs (per-face lightmaps, GoldSrc entity semantics, its cooked world-pipeline
lump), but the container discipline (magic, versioned lump directory, checked
readers, `RecordSlice`) is the same problem PXBSP solved with typed records
instead of offset constants.

### 4.3 Collision and player physics

| Program | Implementation | LOC |
|---|---|---|
| shared | `psx_bsp::collision` (`CollisionHull`, `Trace`, `TraceScratch`) | 1,562 |
| quake-psx | `quake-core/src/collision.rs` (Z-up facade over the shared tracer) + its own `trace_render_bsp_into` | 617 |
| quake-psx | `quake-core/src/movement.rs` (Quake accel/friction/water/step) | 2,077 |
| hl-psx | `game/src/phys.rs` (its own `SV_RecursiveHullCheck` port + GoldSrc movement) | 2,790 |
| Cortex | `psx_engine::character_motor` (third-person cylinder motor) | 5,195 |

`[MEASURED]` hl-psx `game/src/phys.rs:1-15` describes itself as "a fixed-point
Rust adaptation of Quake's GPL-licensed `SV_RecursiveHullCheck`". `psx_bsp::collision`
is the same algorithm, already caller-owned, allocation-free and stack-bounded
(`TRACE_STACK_CAPACITY = 64`), and already proven by Quake through the Z-up
facade.

`[INFERRED]` Verdict: the **tracer** is **share** (HL is the missing adopter);
the **movement policies** are **keep** (Quake physics, GoldSrc physics and
Cortex's third-person motor are three different games' feel, and unifying them
would be a behaviour change nobody asked for).

`[MEASURED]` The axis boundary is already isolated and cheap:
`quake-core/src/bsp_axis_adapter.rs` is 93 lines of `const fn` field swaps. HL is
Z-up like Quake, so it can reuse the same adapter shape.

### 4.4 Model formats and skinned rendering

| Program | Implementation | LOC |
|---|---|---|
| shared | `psx_asset::hmd8` | 1,302 |
| hl-psx | `game/src/model.rs` (the pre-lift original) | 1,249 |
| quake-psx | alias models via `psx_bsp::resident` model table + `submit_classic_alias_model` | shared kernel |
| Cortex | `.psxm`/`.psxanim` v3 via `psx-asset` + `psx-game-runtime/model_rendering` | 2,212 + 1,015 + 854 |

`[INFERRED]` Verdict: **share**, and the work is 85% done. This is the cleanest
available adoption: the SDK copy is tested against 232 real cooked chunks
(session handoff 2026-08-20) and the only blocker named is the pin bump.

### 4.5 Frame pacing and presentation

| Program | Implementation |
|---|---|
| Cortex | `psx_engine::scheduler` + `App::run_scheduled` + `App::present_pending` (async kick, overlay, true-vblank-edge flip) |
| quake-psx | `game/src/platform.rs` `gpu_begin_frame` / `gpu_present_pending_frame` (own one-frame pipeline) |
| hl-psx | own loop, plus the `decoupled-present` cargo feature (default on, measured +5.6% on the tram benchmark, 13.79 -> 14.55 fps) |

`[INFERRED]` Verdict: **adapt**. All three independently arrived at "submit
async, flip at a vblank boundary, decouple presentation from the simulation
clock". Three implementations of the same insight means three places to fix the
next pacing bug. But this is high risk: pacing is where the measured regressions
in the standardisation doc came from, and each game's cadence (Cortex 60 Hz
sim / 30 Hz visual, HL 20 Hz sim) is a shipped behaviour. Sequence it late.

### 4.6 CD streaming

`[MEASURED]` Three implementations, and one of them looks like an unpropagated
silicon fix:

- `sdk/crates/psx-pack/src/cd.rs:278-290` (`SectorReader::dma_read_sector`)
  is **PIO**. Its comment: "This used to be a chopping-burst DMA on channel 3
  (the hl-psx recipe). The CL1/CL2 silicon probes convicted that path on real
  hardware: the transfer is a state-dependent lottery, and the channel can latch
  its start bit and stay busy forever while moving nothing, which read as
  all-zero sectors everywhere the reader is used."
- `engine/crates/psx-game-runtime/src/cd_stream/hw.rs:373-385`
  (`dma_read_sector`) is still **chopping-burst DMA on channel 3**:
  `set_chcr(Channel::Cdrom, 0x1140_0100)`, with the comment "Matches the
  BIOS-style burst control word that the emulator models at Redux's quarter-rate
  CD DMA completion cadence."
- hl-psx `game/src/cdstream.rs` is a thin wrapper over `psx_pack::cd`, so HL
  already has the PIO path.

`[MEASURED]` Cortex uses the DMA one: `engine/examples/editor-playtest/src/runtime_arenas.rs:66`
holds a `psx_game_runtime::cd_stream::CdController`.

`[VERIFY] **This is the highest-priority item in the document.** Either
(a) Cortex still carries a path convicted on silicon, in which case the fix is
to route `psx-game-runtime` through `psx_pack::cd::SectorReader`, or
(b) there is a reason the background-streaming use pattern needs DMA that the
one-shot `SectorReader` pattern does not, in which case that reason belongs in
a comment at `hw.rs:373` next to the convicted control word. The current comment
justifies the value by what the **emulator** models, which is precisely the
failure mode the silicon probes exist to catch.

### 4.7 Audio

`[MEASURED]` quake-psx `game/src/audio.rs` (876) builds Quake bank policy on
`psx-sfx`. hl-psx `game/src/sfx.rs` (643) drives `psx_spu` voices directly with
its own rotation pool. Cortex uses `psx_engine::sfx` (52) over `psx-sfx`.

`[INFERRED]` Verdict: **share** the voice pool (HL is the missing adopter),
**keep** the bank/attenuation policy. `psx-sfx`'s crate doc lists three specific
silicon behaviours it exists to get right (one-shot repeat address latching,
release-rate ring-out, voice sharing); a private pool is a place those can be
wrong silently, and the doc says the Celeste collection was "inaudibly wrong
rather than right" for exactly that reason.

### 4.8 Options, HUD, menus, saves

`[MEASURED]` The clearest single piece of evidence for the whole document:

```
quake-psx  b5ef230  2026-08-21 15:22:19 +0100  "Raise the default brightness"  crates/quake-core/src/menu.rs
hl-psx     26e2365  2026-08-21 15:22:19 +0100  "Raise the default brightness"  game/src/settings.rs
```

Same change, same title, same minute, two codebases.

`[MEASURED]` hl-psx `game/src/settings.rs:26-53` already documents the
relationship in prose: Quake cooks six gamma'd palette rows and indexes them
from the texture-page word; HL has per-texture CLUTs so it applies the curve to
the per-map light palette and the model `shade` byte instead. It defines
`BRIGHTNESS_LEVELS = 6`, `DEFAULT_BRIGHTNESS = 5`, and a Q8 mix table, and its
comment says level 5 "is the shipped default shared with Quake-PSX".

`[INFERRED]` Verdict: **adapt**. The *mechanism* is legitimately different and
must stay so. The *policy* (level count, default, the Q8 mix curve, the
"< n >" options-row semantics) is one thing, currently maintained in two places,
already known to be one thing by the author. A ~60-line
`psx_engine::brightness` owning `LEVELS`, `DEFAULT`, `MIX[]` and `curve(u8) -> u8`,
with each game applying it where its pipeline allows, removes the class.

`[MEASURED]` The same overlap exists for the rest of the options screen: TV
screen offset (`psx_gpu::set_display_offset` in both), music/SFX volume steps,
analog deadzone (`psx_pad::Deadzone` in both). hl-psx's own comment at
`settings.rs:21` notes it went through `psx_pad::Deadzone` deliberately "so
there is one implementation of that shape rather than one per game".

`[VERIFY]` Whether Cortex's options screen (`psx_engine::ui` +
`psx_level::LevelUiScene`) exposes the same rows, and whether it could consume
the same policy constants.

### 4.9 Verification harness

Covered in B5. Verdict: **share**, and this is the highest-leverage item after
4.6 because it lowers the cost of every other item.

### 4.10 Telemetry

`[MEASURED]` `psx-telemetry` is single-source (52 stages, 267 counters, with
compile-time asserts that `STAGE_COUNT` and `COUNTER_COUNT` match the highest
id). But hl-psx `game/src/telemetry.rs:8-30` squats free slots: "hl-psx owns
these otherwise-unused room micro-profiler slots" and reuses ids 85-91, 95-96,
103, 107-108 with different meanings, because "PSoXide's generic profile CSV
still exposes the legacy column names".

`[INFERRED]` Verdict: **share with a registry**. Add a reserved per-game id
range (or a named `game::*` block) so a slot's meaning is not
position-dependent and a PSoXide profiler schema change cannot silently
re-label HL's data. Small change, prevents a whole class of measurement error.

---

## 5. Ranked proposals

Ranking is `cascade value / (risk x cost)`. "Cascade value" means: how many
programs does a future optimisation in this area reach once the change is made.

### Tier 1: do these first, they unblock the rest

**H1. Route `psx-game-runtime`'s CD reads through `psx_pack::cd::SectorReader`,
or document why not.**
Cascade: correctness, not performance. Risk: medium (throughput profile changes,
~1.3 ms/sector slower per the SDK comment; Cortex streams continuously).
Gate: Cortex canonical route with counter-log stream stats before/after, plus a
console read. See §4.6. **If (b) turns out to be true, the deliverable is a
comment, and that is a complete and acceptable outcome.**

**H2. Extract the shared acceptance harness into a crate every program uses.**
Take `emu/crates/psoxide-validation` and grow it (or add `psoxide-harness`
beside it) to own: scenario tables, frontend discovery, `launch` invocation,
PPM region decode and hash, profile/counter CSV column extraction, percentile
and timing summaries, and a pass/fail report. Port hl-psx's
`host/hl-build/regression.rs` scenario shape first: it is the most developed
(64 scenarios across four tables) and is already the right abstraction.
Cascade: **all nine programs**, and it directly reduces the cost of H3-H12.
Risk: low (host-side only, no guest bytes change).
Gate: each game's existing harness and the new one produce identical verdicts on
the same build before the old one is deleted.

**H3. Make "one SDK, every program, headless acceptance" a single command.**
`psx-demo-disc` already has 80% of this: `make programs PSOXIDE_FROM=<tree>`
forces every program onto one checkout, and `sdk-coherence` (Makefile:612)
proves it. What is missing is (i) quake-psx participating instead of shipping a
frozen `.bin`, and (ii) running each program's acceptance route rather than only
building it. With H2 in place this is a loop over per-game harness invocations.
Cascade: turns B3 from "a deliberate bump" into "a job that either passes or
names the game that broke".
Risk: low to build, high to keep green (see §9 open question 3).

### Tier 2: high value, well-understood, gated on Tier 1

**H4. Split `psx-engine` into a neutral core and a Cortex layer.**
Proposed split (names illustrative):

| New crate | Contents | Consumers |
|---|---|---|
| `psx-engine-core` | `scratchpad`, `projection`, `attributed_clip`, `classic_affine`, `fixed`, `angle`, `frames`, `transform`, `render` (OT/arena), `time`, `telemetry` | all |
| `psx-engine-app` | `app`, `scene`, `scheduler`, `game_app` | Cortex, gh-psx, psxcel, nitroxide |
| `psx-engine-cortex` | `character_motor`, `third_person_camera`, `world`, `world_render`, `render3d`, `ui`, `lighting`, `movement`, `collision_query`, `floor_sample`, `transitions`, `sfx` | Cortex |

`psx-bsp` then depends on `psx-engine-core` only, so quake-psx stops compiling
`psx-level`, `psx-font`, `psx-asset`, `psx-spu` and `psx-sfx` to reach a 102-line
address helper.
Cascade: high, and it is a precondition for H5 and H6 being affordable.
Risk: **high**, and specifically the B4 risk: this changes the guest link layout
for every program. The standardisation doc has a measured precedent of exactly
this degrading Cortex pacing (deadline misses 6 -> 47).
Gate: mandatory. Cortex cadence must be bit-identical (288 ticks, 120 visuals,
144 skipped vblanks, 6 deadline misses, 23 lateness vblanks on the current
route), Quake's fixed-three-ticks E1M1 route must stay inside its documented
±0.122 fps layout noise, HL's tram benchmark must not regress. If any of those
move, the split is wrong even if the code is cleaner.

**H5. Unify the Quake and PXBSP world renderers under one traversal.**
Target: `psx-bsp`'s `render.rs` becomes the single implementation, with three
compile-time policies (vertex source encoding, material/animation lookup,
visibility policy), following exactly the `attributed_clip` precedent: one
traversal, monomorphized adapters, no dynamic dispatch, each renderer keeps its
measured numeric and ordering rules.
Order of operations that keeps this tractable:
  1. move the nine already-common free functions down, unchanged, one at a time;
  2. reconcile `materialize_face` behind a `VertexSource` policy;
  3. reconcile `mark_visible_faces` behind a `VisibilityPolicy` policy, taking
     Quake's `visible_faces` list and 4-per-word packing as the shared base
     (it is the more developed one) and gating the water portal off for PXBSP;
  4. only then consider `draw_frame`.
Cascade: **this is the item the user's question is actually about.** After it,
a BSP render optimisation lands once.
Risk: high. Mitigation is the step size: every one of those four steps has an
independent byte-exact gate on both games.
Gate: Quake E1M1 framebuffer/VRAM/GPU-log byte-exact plus the fixed route fps;
Cortex 120-visual-frame exact capture plus cadence.

**H6. Adopt the SDK HMD8 reader in hl-psx.**
Cascade: medium (HL + Cortex share one animated-model reader; the Rust Mantis
and Aletha work then benefits from HL's 103-map hardening and vice versa).
Risk: low-medium. The diff is 395 lines on 2,551, and the SDK copy is already
validated against 232 real cooked chunks.
Cost: dominated by the pin bump, which is why it is gated on H3.
Gate: HL model/animation audit plus a complete Hazard Course tape.

**H7. Adopt `psx_bsp::collision` in hl-psx.**
Replace `game/src/phys.rs`'s recursive hull trace (not its movement policy) with
the shared caller-owned tracer behind a Z-up adapter modelled on
`quake-core/src/bsp_axis_adapter.rs`.
Cascade: medium-high. It puts all three BSP games on one tracer, which is where
collision bugs and collision optimisations both live.
Risk: medium. HL traces the **origin** as a point against a pre-expanded
`hull1`, which is a different input convention from Quake's box hulls;
`[VERIFY]` that `CollisionHull` supports the pre-expanded-hull point trace
without a behaviour change.
Gate: HL's semantic-input tape must produce a bit-identical simulation state
stream. Anything less is a physics change.

### Tier 3: cheap, low-risk, do them opportunistically

**H8. Close the promote-without-adopt gaps.** `aim_curve` (hl-psx), `psx-sfx`
voice pool (hl-psx), PVS RLE decode (quake-psx, hl-psx, and the editor host copy
in `brush_pack.rs`). Each is a small, independently gated diff. `[VERIFY]` that
`psx_pad::aim_curve` and hl's local one are numerically identical across the
full i8 domain before swapping; the SDK version clamps to `-128..=127` and the
local one clamps `abs()` to 128, which differ at `v = -128`.

**H9. Shared brightness/options policy.** ~60 LOC in the neutral core, consumed
by Quake, HL and Cortex, applied by each in its own pipeline. See §4.8.

**H10. Telemetry slot registry.** Reserve per-game counter id ranges so HL stops
squatting Cortex's room-profiler slots. See §4.10.

**H11. Fix the documented-mode drift.** Update `downstream-projects.md` to
describe `psoxide-link` hydration; delete the stale `third_party/PSoXide` trees
in gh-psx, nitroxide and pico8-psx. See §1.1.

**H12. Resolve the `PSoXide-runtime` provenance.** See B3. `[VERIFY]` first;
this may be nothing more than local clone state.

### Explicitly not proposed

- **Unifying HL's world renderer with the BSP one.** Different algorithm
  (patch tessellation with per-face lightmaps and its own admission policy). The
  shared surface is already correctly drawn at `projection` and
  `attributed_clip`.
- **Unifying the three movement/physics policies.** Quake feel, GoldSrc feel and
  Cortex's third-person motor are game design, not engine.
- **A common entity/logic runtime.** Quake QC semantics, GoldSrc entities and
  Cortex's actors overlap conceptually but the shared part (brush movers,
  triggers) is already converged for Quake/Cortex via `psx_bsp::mover`, and
  extending that to HL is a much smaller and better-defined job than a unified
  entity system.
- **Moving Cortex out of `engine/examples/`.** Tempting for symmetry, and it
  would remove the asymmetry where the flagship game shapes the engine
  in-tree while the others consume it through a pin. But it is a large
  disruption with no measured benefit, and Cortex being in-tree is what makes
  the engine's own tests meaningful. `[VERIFY]` whether the in-tree position
  is what lets Cortex-specific concerns (`psx-level` in `psx-engine`) leak; if
  so, H4 fixes the leak without the move.
- **One giant `psx-engine` that all games adopt wholesale.** The evidence in §2
  says small kernels get adopted and large frameworks do not.

---

## 6. The mechanisms, not just the code

Sections 4 and 5 are what to unify. This section is the part that decides
whether unification *stays* unified. `[INFERRED]` throughout: these are
proposals, none has been tried here.

### M1. An adoption ledger

Every promotion into the SDK records, in the promoted module's doc comment, the
programs that are expected to adopt it and whether they have. `psx-sfx` and
`psx_asset::hmd8` already write the first half of that sentence ("lifted from
hl-psx so both games share one format"); they just do not track the second half,
which is why the origin project quietly kept its copy. A one-line
`ADOPTERS: quake-psx (yes), hl-psx (no, pending pin bump)` in the crate doc,
checked by a `psoxide-dev` lint against a small manifest, turns B1 from
invisible into a listed debt. Cheap. `tools/psoxide-dev` already runs this kind
of policy guard (`lint_policy_guard`, `runtime_numeric_guard` at
`tools/psoxide-dev/src/main.rs:91,168`).

### M2. A drift detector for the known forks

For the forks that will exist for a while (H5 is not a weekend), a host test
that asserts the shared-name functions are still textually identical, and fails
loudly when one side edits `add_normal` or `plane_distance` without the other.
Cheap, and it converts silent drift into a build failure that names the two
files. Scope it to the nine names in B2 plus the four PVS decoders.

### M3. Continuous cascade, not periodic bumps

H3 gives the command. The mechanism is to run it on every PSoXide `main` push
and publish the result per game: "quake-psx: E1M1 route pass, 22.27 fps;
hl-psx: Hazard Course pass; cortex: cadence exact; voxide: boot pass". A pin
bump then stops being an act of faith. It becomes "the cascade job has been
green for this revision for three days".

`[VERIFY]` The blocker is machine time, not design. Estimate the wall-clock of
one full cascade run (nine guest builds plus nine headless routes) before
committing to per-push. If it is hours, run it nightly and on demand.

### M4. Make the pin bump incremental

`[INFERRED]` The reason bumps are scary is that they are 80 commits wide. Two
options, in increasing cost:

- **Bisectable bumps.** Because `psoxide-link --from` can force any tree, a
  failed cascade can be bisected across the SDK range automatically. That does
  not make the bump smaller, it makes attribution cheap, which is most of the
  fear.
- **Per-crate pins.** Let a game pin `psx-pack` at one revision and `psx-engine`
  at another. This is the standard answer and it is probably **wrong here**:
  the crates are path-deps inside one hydrated tree precisely so the linker
  script, runtime decoder, audio encoder and disc writer stay a reproducible
  set (`psoxide-link` crate doc). Splitting the pin re-opens the drift the
  hydration tool was built to close. Recommend bisectable bumps and reject
  per-crate pins unless the cascade job proves it needs them.

### M5. One acceptance vocabulary

`[MEASURED]` The three games already agree on what evidence counts (framebuffer
hash, VRAM hash, counter-row equality, fixed-route fps, packet-arena margin,
RAM report), which is why the standardisation doc's tables read consistently.
What differs is only the plumbing. H2 makes the vocabulary literal: one
`Verdict` type, one report format, one exit code. That is what lets M3 print a
single table.

---

## 7. Sequencing

`[INFERRED]` A dependency-ordered path. Each step is independently valuable, so
stopping after any of them leaves the tree better than before.

```
H1  CD read path resolved (or documented)        <- do first, correctness
H11 docs/third_party cleanup                     <- trivial, parallel
H12 PSoXide-runtime provenance verified          <- trivial, parallel
        |
H2  shared acceptance harness crate              <- unblocks everything
        |
H3  one-SDK cascade command                      <- turns bumps into a job
        |
   +----+----+----------------+
   |         |                |
H4 crate split   H8/H9/H10 small adoptions   M1/M2 ledger + drift detector
   |
H5 BSP renderer unification  (the main event)
   |
H6 HMD8 adoption      H7 shared tracer in HL
   |
4.5 frame pacing convergence (last, highest behaviour risk)
```

---

## 8. Verification tasks

Every claim the proposals rest on, with how to settle it. An agent working
through this should record pass/fail per row and stop the dependent proposal on
a fail.

| # | Claim | Label | How to settle |
|---|---|---|---|
| V1 | `psx-game-runtime` still uses chopping-burst CD DMA while `psx_pack::cd` uses PIO | MEASURED | read `engine/crates/psx-game-runtime/src/cd_stream/hw.rs:373-385` and `sdk/crates/psx-pack/src/cd.rs:278-300` |
| V2 | Cortex actually executes that path on hardware (not only in emulation) | VERIFY | trace `runtime_arenas.rs:66` -> `cd_stream` -> `hw::dma_read_sector`; confirm no PIO fallback; then a console read test |
| V3 | There is no documented reason Cortex needs DMA where `SectorReader` uses PIO | VERIFY | search PSoXide docs and git log for a rationale post-dating the CL1/CL2 probes |
| V4 | quake-psx does not consume `psx_bsp::render` | MEASURED | `grep -o 'psx_bsp::[a-zA-Z0-9_:]*' quake-psx/game/src -r \| sort -u` |
| V5 | The nine named free functions exist in both renderers and two have already diverged | MEASURED | diff the named bodies in `psx-bsp/src/render.rs` and `quake-psx/game/src/renderer.rs` |
| V6 | Quake's `mark_visible_faces` is a strict superset of PSoXide's in capability | INFERRED | read both; confirm `visible_faces`, portal union and 4-per-word packing have no PXBSP-side equivalent |
| V7 | Four copies of the PVS RLE decoder exist | MEASURED | the four locations in B2 |
| V8 | hl-psx does not depend on `psx-bsp` | MEASURED | `hl-psx/game/Cargo.toml` dependency list |
| V9 | `psx_bsp::collision::CollisionHull` can express HL's point-trace-against-pre-expanded-hull1 convention without behaviour change | VERIFY | read `psx-bsp/src/collision.rs` `CollisionHull` and `hl-psx/game/src/phys.rs`; write a host test that runs one HL hull through both and compares fraction/normal/plane |
| V10 | `psx_asset::hmd8` and `hl-psx/game/src/model.rs` differ by ~395 lines of 2,551 | MEASURED | `diff <(sed 's/[[:space:]]*$//' a) <(sed 's/[[:space:]]*$//' b) \| grep -c '^[<>]'` |
| V11 | Those 395 lines contain no behaviour change (only names/docs/the vertex cap) | VERIFY | read the diff |
| V12 | `psx_pad::aim_curve` and hl-psx's local `aim_curve` agree on all inputs | VERIFY | exhaustive host test over `-128..=127`; note the clamp asymmetry called out in H8 |
| V13 | hl-psx drives SPU voices directly rather than through `psx-sfx` | MEASURED | `hl-psx/game/src/sfx.rs:10-11` |
| V14 | Nine `psx-engine` modules depend on `psx-level` | MEASURED | `grep -rln psx_level engine/crates/psx-engine/src/` |
| V15 | The proposed H4 split has no module that would need to live in two crates | VERIFY | build the dependency graph of `psx-engine`'s 36 modules and check the partition is acyclic |
| V16 | Adding/removing a crate dependency measurably moves Cortex cadence | MEASURED (precedent) | the rejected experiment in `shared-engine-standardisation-2026-08-22.md` §"Third retained convergence" |
| V17 | The demo disc carries four PSoXide revisions | MEASURED | `git submodule status` in psx-demo-disc |
| V18 | `588d7637` has no ancestor on `origin/main` and its clone's remote is a `/tmp` path | VERIFY | re-check in a fresh clone; distinguish local state from published history |
| V19 | quake-psx is excluded from `sdk-coherence` | MEASURED | `psx-demo-disc/Makefile:612-628` and the `QUAKE_*` pin block at `Makefile:79-88` |
| V20 | `psoxide-validation` has no game consumers | MEASURED | `grep -rn psoxide-validation` across all repos |
| V21 | hl-psx, nitroxide and gh-psx have no CI workflows | MEASURED | `ls .github/workflows` per repo |
| V22 | The two brightness commits are the same change at the same minute | MEASURED | `git show --stat` on quake-psx `b5ef230` and hl-psx `26e2365` |
| V23 | Cortex's options screen exposes the same rows as Quake's and HL's | VERIFY | read `psx_engine::ui` and the `LevelUi*` records |
| V24 | hl-psx squats `psx-telemetry` counter slots by numeric id | MEASURED | `hl-psx/game/src/telemetry.rs:8-30` |
| V25 | A full nine-program cascade run is affordable at some cadence | VERIFY | time `make programs PSOXIDE_FROM=<tree>` plus each game's acceptance route |
| V26 | HL's world renderer is a genuinely different algorithm, not a drifted fork | VERIFY | read `hl-psx/game/src/world_pipeline.rs` and `main.rs` world sections against `psx-bsp/src/render.rs`; if it turns out to be a fork, H5's scope grows to three games |

---

## 9. Open decisions for the owner

These change the shape of the work and are not the agent's to pick.

1. **Does quake-psx rejoin the cascade, or stay a frozen artifact on the disc?**
   Freezing it is defensible (it is the one program with an external content
   provenance chain and a published sha256 pin). But it is also the program with
   the most shared code, so freezing it guarantees the H5 fork keeps drifting.
   A middle option: quake stays frozen *for the pressed disc* while still being
   built and gated by the cascade job.

2. **Is `psx-engine` allowed to be split?** H4 is the load-bearing structural
   change and it touches the linker layout of every program. If the answer is
   no, H5 is still possible but every BSP-game build keeps compiling
   `psx-level`, and the crate stays a place where Cortex concerns leak into
   shared code.

3. **What is the standard for "the cascade job is green"?** Byte-exact
   framebuffers are the current bar in the standardisation doc, and it is a good
   bar, but it is also why adoption is expensive. A tiered answer
   (byte-exact for renderer changes, cadence-and-hash for everything else)
   would make M3 affordable. This is a policy call, not a technical one.

4. **Is HL's `main.rs` allowed to be broken up?** 33,351 lines in one file is
   the practical blocker for H6/H7 and for anything else in HL. Splitting it is
   mechanical but it is also a link-layout change on a program whose manifest
   comments show it is already sensitive to that (`trim-paths`, the `.bss`
   overflow). `[VERIFY]` whether HL's `#[link_section]` usage
   (`.hlpsx_cold.brightness` and similar) already constrains placement enough
   that a file split is safe.

5. **Does Cortex stay in `engine/examples/`?** See §5 "explicitly not proposed".
   Not recommending the move; flagging that the asymmetry it creates is real
   and is at least part of why `psx-engine` grew Cortex-shaped.
