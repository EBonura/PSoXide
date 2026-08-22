# Engine harmonisation plan: one PS1 engine for the demo-disc games

Status: consultant draft, 2026-08-22, written from a read-only survey of the
repositories listed in section 1. Nothing in this document has been
implemented. It is input for a working agent whose first job is to verify the
claims below against the source before acting on any of them (section 10 is
the verification protocol).

The question asked: the demo-disc games already share a lot of code; can we go
further, to a point where most of them sit on the same engine, so an
optimisation made once cascades to all of them, with the BSP trio (PSoXide
Cortex, quake-psx, hl-psx) as the primary target.

Short answer: yes, and the machinery for it already exists. Three things stand
in the way today, in order of cost:

1. The convergence work that makes cascading possible is uncommitted in three
   repositories and unreachable by the pins every game builds against
   (section 3.1). Until that lands and the pins move, nothing cascades at all.
2. The demo disc presses four different PSoXide revisions (section 3.2). An
   optimisation made on `main` reaches the disc only after four separate
   repins, two of which are deliberately frozen.
3. The largest remaining duplicates are whole subsystems that were lifted into
   the engine and then not adopted by the game they were lifted from:
   `psx_bsp::render` (2,936 lines from quake-psx, not called by quake-psx)
   and `psx_asset::hmd8` (1,302 lines from hl-psx, not called by hl-psx).
   Section 5 lists these with evidence.

Everything that follows is organised so the working agent can take items in
priority order, each with its evidence, the proposed change, which games the
change cascades to, the gate that must pass, and the owner decisions it needs.

---

## 1. Scope and sources

Repositories surveyed (all under `/Users/ebonura/Desktop/repos/`):

| Repo | Role | HEAD at survey | Working tree |
|---|---|---|---|
| `PSoXide` | SDK + engine + editor + emulator | `7b79bfea` on `codex/arena-playable-slice` | dirty: 49 entries, 4 untracked engine paths |
| `quake-psx` | Quake 1 shareware port | `043be86` | dirty: 23 files, +881/-448 |
| `hl-psx` | Half-Life port | `26e2365` | dirty: 11 files, +264/-163 |
| `voxide`, `nitroxide`, `gh-psx`, `psxcel`, `pico8-psx` | non-BSP games | each at "Align the SDK pin: PSoXide main 9c298d83" | clean |
| `psx-demo-disc` | the disc: launcher, loader, mkdisc, 11 submodules | | |
| `psx-demo-disc/games/psoxide-arcade` | nested disc: breakout, invaders, magikarp-pong | | |

Documents that already govern this area and that this plan builds on rather
than replaces:

- `docs/shared-engine-standardisation-2026-08-22.md` (untracked): the three
  retained convergences (scratchpad, attributed clipping, projection/outcodes
  + `psx-render-contract`), their acceptance evidence, two measured rejections,
  and an ordered queue (model residency, frame submission/streaming, packet
  emitter).
- `docs/finish-line-plan-2026-08-11.md` section 2: the binding architectural
  boundary and the explicit non-goals (no shared on-disc level format required,
  Quake does not consume editor projects, no Quake-to-PXBSP gameplay importer).
- `docs/quake-psoxide-convergence-handoff.md` section 6: the ownership chain
  (`psx-level` records -> `psx-bsp` mechanism -> `psx-engine` providers ->
  `psx-game-runtime` policy -> game policy) and its rules (Q20.12 positions,
  Q3.12 normals, no allocation/recursion/float/64-bit in guest hot paths).
- `docs/quake-parity-audit.md`: what PSoXide shares with, mirrors, or
  deliberately replaces from Quake, with the immediate convergence order.
- `docs/bsp-engine-overhaul.md`: the "Shared with quake-psx" list and the
  adoption terms (wire parity + frame-hash tests + revision-pinned `.psoxide`).
- `docs/downstream-projects.md`: the canonical downstream layout and the
  "use the SDK, don't re-derive it" table from the 2026-07 audit.

Line counts below are `wc -l` of `.rs` sources at the survey revision. File
anchors are `path:line`. Numbers quoted from other documents carry their
source.

---

## 2. What "one engine" means here

The target is not one renderer. The three BSP games differ in authored format,
visibility policy, lighting model, coordinate conventions and gameplay. They do
not differ in the PS1: fixed-point GTE projection, clipping arithmetic, GPU
packet layout, OT mechanics, scratchpad, DMA ownership, CD sector machinery,
SPU, pad, memory card. The existing standardisation doc says this well and it
holds for the non-BSP games too.

The useful mental model is five layers. A change cascades exactly as far as the
layer it lands in is consumed. The point of this plan is to move code down the
stack until every game that needs a mechanism consumes the same copy of it, and
to make sure the pins carry the result to the disc.

```
L4  disc + pin        psoxide-link, mkisopsx/psx-iso, psx-demo-disc mkdisc, disc-toc
L3  game policy       quake-core + quake-psx game/, hl-psx game/, psx-game-runtime (Cortex),
                      voxide/nitroxide/gh-psx/psxcel/pico8/arcade game crates
L2  world mechanism   psx-bsp: XBSP/PSB5 + PXBSP readers, resident maps, PVS, traversal,
                      hull collision, movers, layered sky
L1  engine kernel     psx-engine: scratchpad, projection, attributed_clip, classic_affine,
                      render (OT frame, packet arenas), app/scheduler/frames,
                      collision_query, fixed/angle/transform/lighting
L0  SDK               psx-rt, gpu, gte(+core), vram, io, pad, spu, sfx, mc, pack, font,
                      fx, math, telemetry, cache, osk, asset, settings; crates/psx-hw
```

Cascade map today (which games actually execute each shared layer):

| Shared unit | PSoXide Cortex | quake-psx | hl-psx | voxide | nitroxide | gh-psx | psxcel | pico8 | arcade |
|---|---|---|---|---|---|---|---|---|---|
| `psx_gte::scene` projection | yes | yes | yes | yes | yes | yes | no 3D | no | pong only |
| `psx_gpu::OrderingTable` (+ `submit_async`) | yes | yes (direct) | yes (direct) | yes (direct) | via OtFrame | via OtFrame | no OT | no OT | via OtFrame |
| `psx_engine::render` OtFrame / packet arenas | yes | no | arena only | no | yes | yes | no | no | yes |
| `psx_engine::App` + `FrameScheduler` | yes | no | no | no | yes | yes | yes | no | yes |
| `psx_engine::classic_affine` | yes | yes | no | no | no | no | no | no | no |
| `psx_engine::attributed_clip` (uncommitted) | yes | yes | yes | no | no | no | no | no | no |
| `psx_engine::projection` (uncommitted) | yes | transitively | yes | no | no | no | no | no | no |
| `psx_engine::scratchpad` (uncommitted) | yes | yes | yes (+ own trampoline) | no | no | no | no | no | no |
| `psx_bsp` formats + resident | PXBSP | XBSP via `quake-formats` facade | no | | | | | | |
| `psx_bsp::render` traversal | yes | **no** | no | | | | | | |
| `psx_bsp::collision` | yes | yes (via adapter) | **no** (own `phys.rs`) | | | | | | |
| `psx_bsp::mover` | yes | yes | no | | | | | | |
| `psx_bsp::sky` | yes | **no** (own copy in `quake-core/sky.rs`) | no | | | | | | |
| `psx_asset::hmd8` | no consumer | no | **no** (own `model.rs`) | | | | | | |
| `psx_pack::cd::SectorReader` | yes | yes | yes | yes | yes | no CD | no CD | no CD | launcher |
| `psx_io::cdda` starter/clock | yes | yes | no (raw `cdrom`) | no CD-DA | yes | yes | | | pong: **no** (own) |
| `psx_pad::PadTracker` / `Ctx` edges | yes | no (own `EdgeTracker`) | yes | **no** (own) | yes | yes | yes | yes | yes |
| `psx_telemetry::emit` | yes | yes | yes | **no** (local copy of the same writes) | yes | | | | |
| `psx_fx` particles/rng/shake | | | rng+particles | rng only | | | | no (PICO-8 exact) | yes |
| `psx_mc` | yes | absent by decision | yes | yes | | | yes | yes | via settings |

Bold "no" entries are the duplicates this plan targets. Empty cells are
"not applicable".

---

## 3. Current state that blocks cascading

### 3.1 The convergence is uncommitted in three repositories

Observed at survey time:

- PSoXide (`codex/arena-playable-slice`): untracked
  `engine/crates/psx-engine/src/{scratchpad,attributed_clip,projection}.rs`
  (102 + 472 + 152 lines), untracked `engine/crates/psx-render-contract/`
  (207 lines + manifest), untracked
  `docs/shared-engine-standardisation-2026-08-22.md`, plus +1,182/-276 across
  `psx-bsp` (`pxbsp_resident.rs` 376, `render.rs` 316), `psx-engine`
  (`classic_affine.rs` 78), `psx-game-runtime` (`entities.rs` 177),
  `psx-level`, `editor-playtest`, `tools/psoxide-link` (the mtime stamping
  fix, 11 lines). `sdk/` and root `crates/` are clean.
- quake-psx: `game/src/renderer.rs` (86), `bonnie.rs` (473), `entity.rs`
  (354), `host/quake-build/main.rs` (83), `crates/quake-cook/src/geometry.rs`
  (20, the `CookedDrawSurface` adoption), `.psoxide/.hydration-stamp` reads
  `local /Users/ebonura/Desktop/repos/PSoXide at 7b79bfea (DIRTY, 58 changed files)`.
- hl-psx: `game/src/{main,map,render,scratchpad}.rs`, `host/hl-bsp/*`
  (the `CookedDrawSurfaceCommand` adoption), `host/hl-build/*`.

Consequences:

- Every pin in the ecosystem (`9c298d83` for quake-psx, hl-psx, voxide,
  nitroxide, gh-psx, psxcel, pico8-psx; `588d7637` for the disc programs and
  the arcade) predates all three seams. No shipped binary contains them.
- hl-psx's `verify_psoxide_rev_on_main` (`host/hl-build/main.rs:1384`) and the
  disc's `sdk-on-main` gate (`psx-demo-disc/Makefile:596-609`) both refuse
  pins that are not on PSoXide `main`. The convergence is on a branch.
- The standardisation doc records that hl-psx's live rebuild against the dirty
  workspace is blocked by API skew ("`psx-engine` references UI/material APIs
  not yet present in its hydrated `psx-gpu`/`psx-level`"). The byte-exact
  evidence was produced from a staged SDK, not from this tree.

This is item H-00 in section 5 and it precedes everything else.

### 3.2 The disc presses four PSoXide revisions

`psx-demo-disc/Makefile`:

| Variable | Rev | Builds |
|---|---|---|
| `PROGRAMS_EXPECTED_PSOXIDE_REV` (`:19`) | `588d7637` | voxide, nitroxide, psxcel, pico8, gh-psx, arcade, launcher, loader, hardware-tests |
| `CORTEX_CURRENT_EXPECTED_PSOXIDE_REV` (`:21`) | `ded86107` | Cortex Ignition (PXBSP) |
| `CORTEX_LEGACY_EXPECTED_PSOXIDE_REV` (`:25`) | `687d2ae7` | Cortex Ignition Legacy (grid), deliberately frozen |
| `QUAKE_EXPECTED_PSOXIDE_REV` (`:83`) | `9c298d83` | Quake (prebuilt artifact, verified by rev + 4 SHA-256s), hl-psx (`HL=1`) |

`PSOXIDE_FROM=$(PROGRAMS_PSOXIDE)` already forces the ordinary games onto one
SDK at disc-build time regardless of their own pin (`Makefile:201-216`), and
`psoxide-link --from` is the mechanism. The Quake and Cortex lanes do not go
through that override. Note also that `588d7637` is not an ancestor of the
local PSoXide checkout (it is not fetched locally), so the disc programs are
built from an SDK the current workspace cannot see.

This is H-01.

### 3.3 Two seams exist in the engine but their source game never switched

| Engine unit | Lifted from | Engine location | Game still running its own copy |
|---|---|---|---|
| BSP render traversal, PVS, face chains, alias models | quake-psx `game/src/renderer.rs` @ 83a6349 | `engine/crates/psx-bsp/src/render.rs` (2,936) | quake-psx `game/src/renderer.rs` (4,317); imports nothing from `psx_bsp::render` |
| Layered sky projection + submit | quake-psx `quake-core/src/sky.rs` @ e32f6f66 | `engine/crates/psx-bsp/src/sky.rs` (429) | quake-psx keeps `quake-core/src/sky.rs` (305) and hand-rolls the submit in `renderer.rs:3742` |
| HMD8 model container | hl-psx `game/src/model.rs` | `sdk/crates/psx-asset/src/hmd8.rs` (1,302), landed `0b8b3fa5` | hl-psx `game/src/model.rs` (1,249); diff is 399 lines, almost all docs and the vertex-cap constant |
| Projected-batch packet writer | `psx_engine::classic_affine::submit_classic_affine_projected_batch` | engine | quake-core `world_batch.rs` (634) is an in-place rewrite proven byte-identical on host and **referenced by nothing** in `game/`, `host/` or `tools/` |

These are H-10, H-12, H-20, H-11.

---

## 4. Inventory, condensed

Full per-file inventories were produced during the survey and are summarised
here; the working agent should re-derive any figure it relies on.

### 4.1 PSoXide engine and SDK

Engine workspace 89,849 lines: `psx-engine` 45,731, `psx-game-runtime` 26,154,
`psx-bsp` 11,565, `psx-level` 6,192, `psx-render-contract` 207. SDK workspace
33,633 lines across 18 crates (`psx-font` 6,834, `psx-asset` 5,066, `psx-gpu`
3,497, `psx-gte-core` 2,881, `psx-gte` 2,755, `psx-mc` 1,919, `psx-io` 1,770,
`psx-vram` 1,489, `psx-pad` 1,345, `psx-pack` 1,332, ...).

Inside `psx-engine`, roughly 19% is the legacy grid world (`world` +
`world_render` + submodules, about 8,790 lines) that `docs/world-format-roadmap.md`
marks historical, and `game_app` (3,342) + `ui` (2,496) are bound to the
editor's `psx-level` schema (43 and 12 `psx_level` references). The generic
kernel a downstream game can take without the editor schema is: `app` (via
`App::run`, 803), `scheduler` (568), `frames` (283), `scene` (388, with a
`psx_level` import to watch), `render` (1,226), `scratchpad`, `projection`,
`attributed_clip`, `classic_affine` (3,103), `render3d` (about 9,800 with
submodules, bound to `psx_asset` PSXMDL/psxanim), `collision_query` (126),
`character_motor` (5,195, Cortex-tuned constants), `third_person_camera`
(2,555, grid-coupled), `angle`/`fixed`/`transform`/`movement`/`lighting`.

`psx-bsp` is explicitly "Shared by the guest runtime, the editor cook and
quake-psx" (`Cargo.toml:7`) and carries provenance headers back to quake-psx
commits 83a6349 / 9e20a1b / e32f6f66. It holds two containers over one record
vocabulary: XBSP/PSB5 (`lib.rs`, `resident.rs`) and PXBSP v6 (`pxbsp.rs`,
`pxbsp_resident.rs`), plus `render.rs`, `collision.rs` (iterative, 64-entry
stack, Y-up Q20.12), `collision_provider.rs` (the `CollisionTraceProvider`
bridge, hull dimensions supplied by the caller), `mover.rs`, `sky.rs`.
`MAX_RESIDENT_MAP_BYTES = 1_100_000` (`resident.rs:31`).

`psx-game-runtime` is Cortex policy by design (`lib.rs:7-10`); the reusable
parts are `cd_stream` (1,228 + 630 hw), `actor_pose` (324), `asset_streaming`
(947), `vram` (1,902), `particles`. About 6,200 lines of `room_*`/`world_*`
are grid residency and legacy.

### 4.2 quake-psx

`game/src` 27,614 lines (19,622 without the 13 `*_regression.rs` probes);
`crates/quake-core` 22,270 (`no_std`, in the guest); `quake-formats` 1,292
(`pub use psx_bsp::*` at `lib.rs:15`, plus QIDX, QSB1, sprite flag,
`RESIDENT_MAP_ARENA_BYTES = 828_000`); `quake-cook` 5,176 (host);
`host/quake-build/main.rs` 9,977.

From PSoXide it takes: the whole `psx_bsp` record vocabulary,
`psx_bsp::resident::ResidentMap` (`asset.rs:5-8, 321, 388`),
`psx_bsp::collision` through `quake-core/collision.rs` (+ a 1,068-line host
parity test), `psx_bsp::mover` + PXBSP entity records, the full
`classic_affine` batch/alias surface (`renderer.rs:8-17`),
`psx_engine::attributed_clip` (adapter `QuakeNearClipPlane`,
`renderer.rs:4113-4154`), `psx_engine::scratchpad` (1,020 of 1,024 bytes,
`renderer.rs:3954-3981`), `psx_engine::div_q12_i32`, and the SDK (`psx-pack`
CD reader, `psx-sfx` `Player`, `psx-io::cdda`, `psx-pad` aim curve, fonts
for the intro only).

Hand-rolled and not using the engine: main loop (`quake.rs:37-1238`, free
running, `perf-fixed-ticks` 3-tick mode), double buffer + OT + present
(`platform.rs:36-51, 322-470`, direct `OrderingTable`), chunk cache above
`psx_pack::cd` (`platform.rs:609-890`, 12-entry `ASSET_CACHE`), PVS decode
(`renderer.rs:4265-4317`), world traversal (`renderer.rs:2618-2831`), text
and menu and HUD packet writers (`renderer.rs:2832-3637`), audio policy
(`audio.rs`, 11 ambient + 12 dynamic voices over `psx_sfx::Player`), input
handshake policy (`input_policy.rs`, "follows HL-PSX's live-input policy" per
`input.rs:52-56`).

Gates: fixed-ticks E1M1 bench (noise floor ±0.122 fps, current 22.27), visual
parity hashes (world/HUD regions; the expected world hash
`0x66082c53b0c45ec8` is documented stale against the deterministic
`0x951a75babd8f2904` after the brightness change), packet overflow
(`packet_overflow_avoided` must be 0; world packets ≤ 400,000, hardware
triangles ≤ 460,000), resident arena margin (825,966 of 828,000 bytes,
2,034 spare, hard-pinned), SPU high-water, boot heap ≥ 8,192 free. Sustained
route 19.008 fps.

### 4.3 hl-psx

`game/src` 54,978 lines, of which `main.rs` is 33,351 and `play()` alone is
5,171 (`main.rs:28181-33351`). `phys.rs` 2,790, `map.rs` 2,342, `menu.rs`
1,427, `logic_state.rs` 1,252, `model.rs` 1,249. Host 29,949 (`hl-bsp`
cooker 18,758).

From PSoXide it takes: `PrimitivePacketArena/Scratch/Sink` (`main.rs:107`,
threaded through about 30 emit functions), `attributed_clip` (`render.rs:9-12`,
adapter `GoldSrcViewClipPlane` at `render.rs:467`), `projection`
(`render.rs:13-15`, `main.rs:106`), `scratchpad` `SIZE`/`base_ptr`
(`scratchpad.rs:9, 76`), `psx_render_contract::CookedDrawSurfaceCommand`
(`map.rs:80`, runtime; `host/hl-bsp/src/main.rs:17`, cooker),
`psx_pack::cd::SectorReader` (the sector machine + HLZC decoder were extracted
into the SDK; `cdstream.rs` is now 708 lines of cache + shim), `psx_vram`
allocators, `psx_mc`, `psx_fx` rng + particles, `psx_pad` incl. `PadTracker`
and `Deadzone`, fonts.

Hand-rolled and not using the engine: frame loop with 20 Hz fixed catch-up +
decoupled present (`main.rs:29149-33351`; `:33332` says "Match psx-engine's
deadline semantics"), BSP + PVS over its own `.hlm` format (`map.rs`;
`decompress_vis:19591`, `merge_vis:19638` underwater merge,
`rebuild_pvs_cache:19724` 431 lines), player hull physics (`phys.rs`, raw i32
units, Q1.3.12 normals, Q27.5 plane distances), HMD8 reader (`model.rs`) and
skeletal projection (`main.rs:26150-26418`, bone-range GTE composition with a
scratchpad-hosted stack), packet emitter with ranked affine budget and quad
pairing (`main.rs:20597-26149`, `try_emit_quad_corners:21790` = 15.9% of
slow-window samples), whole-frame packet cache keyed on a 60-byte view key
(`WorldPacketCacheKey:3357`), OT key policy (`ordering.rs`), text
(`hltext.rs`), menus, HUD, SFX bank (`sfx.rs`, HSFX container, no `psx-sfx`),
CD-DA via raw `psx_io::cdrom`, camera shake, telemetry slot ids 85-108.

Constraints the survey found documented and measured: `play()` is 116,796 B,
about 89% of the MIPS PC16 branch-range ceiling, and three attempts to extract
code from it each grew it (`PERF_HANDOFF.md` section 4); static headroom
10,016 B; `MAP_BUF` has zero fleet slack (725,452 vs worst room 725,384);
4 KiB direct-mapped I-cache with a 32.6 KiB renderer working set; GTE is 1.3%
of a frame, per-face decode + packet construction about 31%; scratchpad cannot
be a DMA source; crate-wide `opt-level = "s"` faults at boot (PSoXide found the
same miscompile independently, see `sanctum-walkthrough-findings`). The
20 fps goal is recorded as `UNPROVEN` (71.8% of gameplay frames on pace).

Gates: `host/hl-build/regression.rs` runs 64 scenarios twice each for
determinism, packet overflow = 0, render p50 ≤ 1,674,624 cycles, visual
difference proofs; build-time audits over 96 maps + every changelevel;
canonical tapes (`hazard-t0a0.pxtape`, 12,265 polls, 11,190 flips; tapes are
poll-bound and break on any menu-row change); `playsize.sh` gates `play()`
size; "performance-telemetry has a different code layout and invalidates any
A/B".

### 4.4 The other programs

| Program | Guest LOC | Loop | Engine crates | Notable duplicates |
|---|---|---|---|---|
| voxide | 17,144 | hand-rolled, own `OT[2]`, `sim_n = dt.min(4)` (`main.rs:1721-1745`) | none | `telemetry.rs` (143) duplicates the `psx_telemetry::emit` MMIO writes under its own feature name and adds a `cycles()` reader the SDK lacks; pad edge/repeat (`main.rs:1331, 9428`); SoA particles (`main.rs:5094-5140`) |
| nitroxide | 7,430 + sim 5,791 | `App::run`, `VisualPacing::EveryNVBlanks(2)` | psx-engine | none significant; CD-DA track policy local (`music.rs:1-41`) |
| gh-psx | 805 | `App::run` | psx-engine | none |
| psxcel | 4,059 | `App::run` | psx-engine | none (source of `psx-osk`) |
| pico8-psx | shared 3,675 + games ~8,800 | three hand-rolled loops | none | deliberate: PICO-8 bit-exact fixed/rng/font/synth |
| arcade breakout/invaders/pong | 643 / 751 / 1,145 | `App::run`, `MicrogameShell` | psx-engine (+ psx-settings on 588d7637) | pong: hand-rolled CD-DA start machine (`main.rs:172-176, 255-290, 1060`) duplicating `psx_io::cdda::CddaStarter` |
| demo launcher | 2,189 | hand-rolled | none | pad edges (`main.rs:613-614, 731`) |
| arcade launcher/loader/disc-toc/carousel/mkdisc | 2,175 / 753 / 566 / 622 / 1,784 | | | loader, disc-toc, carousel are byte-identical forks of the outer disc's; mkdisc differs by 158 lines |

---

## 5. Proposals

Each item: ID, tier, games it cascades to, evidence, proposal, gate, decisions.
Tiers: T0 must precede everything; T1 BSP trio; T2 all games; T3 longer-horizon
or owner-decision-heavy. Within a tier the order is the recommended order.

### T0: unblock cascading

#### H-00 Land the in-flight convergence and move the pins

Cascades to: every game.

Evidence: section 3.1.

Proposal:
1. In PSoXide, commit the four untracked engine paths plus the dirty
   `psx-bsp`/`psx-engine`/`psx-game-runtime`/`psx-level`/`editor-playtest`/
   `psoxide-link` changes as a reviewed series on `codex/arena-playable-slice`,
   then merge to `main` per `psoxide-git-workflow`. Resolve the recorded API
   skew (`psx-engine` vs hydrated `psx-gpu`/`psx-level`) first; hl-psx's live
   rebuild is the test.
2. Commit the quake-psx and hl-psx adoptions, each pinned to the resulting
   PSoXide `main` rev through `psoxide-link` (both repos fail closed unless the
   pin is on `main`; do not use `--allow-psoxide-drift` for a shipping build).
3. Re-run the full gate set for each game against the hydrated `main` tree, not
   the staged SDK: PSoXide Arena/E1M1 route (exact counters, GPU rows,
   framebuffer, VRAM), quake-psx fixed-ticks bench + visual parity + packet +
   arena gates, hl-psx complete Hazard tape + regression matrix + memory report.
4. Fix the stale Quake visual-parity oracle (`0x66082c53b0c45ec8` vs the
   deterministic `0x951a75babd8f2904`) as its own commit with the brightness
   change as the stated cause, so later A/Bs are not polluted.

Gate: the standardisation doc's non-negotiable gates, verbatim.

Decisions: none new, this is finishing started work. Flag to the owner that
the Quake `VISUAL_PARITY` expected hash needs a deliberate re-pin.

#### H-01 One SDK revision on the disc, one repin verb

Cascades to: every program on the disc.

Evidence: section 3.2; `tools/psoxide-link/src/lib.rs:1-28` (the drift
story: three submodules sat eight commits behind a measured SPU fix);
`psx-demo-disc/Makefile:201-216`; `check-locks` found 21/56 stale locks on
2026-08-03 while `sdk-coherence` was green.

Proposal:
1. Collapse `PROGRAMS_EXPECTED_PSOXIDE_REV`, `CORTEX_CURRENT_EXPECTED_PSOXIDE_REV`
   and `QUAKE_EXPECTED_PSOXIDE_REV` into one `DISC_PSOXIDE_REV`. Keep
   `CORTEX_LEGACY_EXPECTED_PSOXIDE_REV` frozen (owner decision already made for
   the grid engine) and keep Quake's separate artifact hash pin, but make the
   Quake artifact's declared PSoXide rev equal `DISC_PSOXIDE_REV` as a gate.
2. Add a `make repin REV=<sha>` verb to `psx-demo-disc` that rewrites the
   Makefile var, bumps every submodule's `psoxide-pin/Cargo.toml` (or
   `Cargo.toml` for quake-psx/hl-psx), runs `cargo update -p psoxide-link`
   in each, regenerates lockfiles, and runs `check-locks` + `sdk-coherence` +
   `sdk-on-main`. Today this is a hand chore across 8 repos, which is exactly
   the drift mechanism the link tool was written against.
3. Adopt a cadence: PSoXide `main` is the dev line; a disc pressing repins all
   games in one commit. Document in `docs/downstream-projects.md`.

Gate: `make check` in `psx-demo-disc`; `make quake-headless-check` double
replay; `make relocation-check`.

Decisions: whether Cortex current should keep a separate pin from the other
programs at all (the survey found no technical reason; the README says it is
there so the old engine stays reproducible, which only the legacy pin needs).

### T1: the BSP trio

#### H-10 Make `psx_bsp::render` the one BSP traversal, adopted by quake-psx

Cascades to: PSoXide Cortex + quake-psx (hl-psx later via H-16).

Evidence: `engine/crates/psx-bsp/src/render.rs:3-5` "Lifted from quake-psx
`game/src/renderer.rs` commit 83a6349"; quake-psx `game/src/renderer.rs`
(4,317 lines) implements `prepare_visibility` (`:2498-2617`),
`mark_visible_faces` (`:2618-2831`), `decompress_visibility`/`merge_visibility`
(`:4265-4317`), `draw_entities` (`:2022-2206`), `draw_brush_entity`
(`:2336-2497`), `flush_batch` (`:3913-3953`), and imports nothing from
`psx_bsp::render`. `docs/bsp-engine-overhaul.md` "Adoption terms (agreed):
quake-psx swaps to psx-bsp only after wire-parity and headless frame-hash tests
pass". `docs/quake-parity-audit.md`: "Keep converging this inside the shared
`psx-bsp` crate."

Since the lift at 83a6349 both copies have moved: the engine copy gained PXBSP
node-owned face ranges, a two-bit face state, hierarchical node-frustum
rejection, a retained sorted-unique visible chain and the scratchpad batch
workspace (`render.rs:656-800`); the quake copy gained water portals
(`water_portal`, `:2498+`), translucent-water opposite-set merge, dynamic
lights (`:652-674`), liquid tile double-buffering (`:827-930`), sprites
(`:322-563`), the scoped-window audit (`:3419-3483`), and the visual-parity
counters. The two are now a fork, not a copy.

Proposal (two phases, both gated on quake's hashes):
1. Diff the two traversals function by function and classify each divergence
   as (a) a Quake policy that belongs in an adapter (water portal merge,
   dynamic lights, liquid tiles, sprite orientation modes), (b) an engine
   improvement quake should inherit (node-frustum rejection, face-state bits,
   retained visible chain), or (c) a PXBSP-only concern. Produce the table
   before touching code.
2. Extend `psx_bsp::render::Renderer` with the (a) hooks as monomorphized
   adapter traits (no dyn, per the standardisation rule), then point quake-psx
   `draw_frame` at `Renderer::draw_frame` for the world pass while it keeps its
   own entity/HUD/menu/sky/effects passes. Delete the duplicated traversal from
   `renderer.rs`.
3. Only then consider moving the (b) improvements into quake (they are free
   once it calls the shared code) and measure them on the fixed-ticks bench.

Expected effect: a traversal optimisation made once reaches Cortex and Quake.
The next one on the PSoXide side is already queued (`quake-parity-audit`
item 4, touched-leaf entity linking) and would otherwise need writing twice.

Risks: quake's `VisibleFace` is a pinned 48-byte record (`renderer.rs:637`)
and `MAX_VISIBLE_FACE_COUNT = 1_325` / `MAX_FACE_COUNT = 6_614` are budget
constants with a 2,034-byte resident margin; the engine `Renderer` sizes its
own state (`DEFAULT_PACKET_WORDS = 0x30000/4`). RAM parity must be shown, not
assumed. Quake is Z-up at its boundary (`bsp_axis_adapter.rs`); the shared
renderer takes a `ViewTransform` (`render.rs:117`), so the adapter stays where
it is.

Gate: quake visual parity (world + HUD region hashes, packet audit), fixed-ticks
bench within ±0.122 fps, resident margin unchanged, packet overflow 0; PSoXide
E1M1 120-frame exact gate unchanged.

Decisions: direction of authority for the fork (engine absorbs quake's newer
policy, or quake adopts the engine's newer mechanism). Recommendation: engine
is the mechanism authority, quake keeps policy in `quake-core`; that is the
north star already written down.

#### H-11 Delete `quake-core/src/world_batch.rs`

Cascades to: quake-psx (hygiene), and removes a trap for H-10.

Evidence: `crates/quake-core/src/world_batch.rs` (634 lines) is "an in-place
rewrite of `psx_engine::classic_affine::submit_classic_affine_projected_batch`"
with a host test proving byte-identity, declared at `lib.rs:40`, referenced by
nothing in `game/`, `host/`, `tools/`.

Proposal: delete it, keep the equivalence test by pointing it at the engine
function if it still adds value, or move the test into PSoXide.

Gate: `cargo test` in quake-core; guest build unchanged (the symbol is not
linked today, so the EXE should be byte-identical; verify with the provenance
hash).

#### H-12 Quake consumes `psx_bsp::sky` instead of its copy

Cascades to: PSoXide + quake-psx.

Evidence: `psx-bsp/src/sky.rs:5-9` "Ported from `quake-core/src/sky.rs` at
Quake-PSX revision e32f6f66"; quake keeps `quake-core/src/sky.rs` (305) and
`renderer.rs:3715-3888` (`SkyWindowPacket`, `submit_view_ray_sky_background`).
The engine adds `submit_view_ray_layered_sky` + `VIEW_RAY_SKY_PACKET_WORDS`.
Both agree on the 10x12 lattice and `SKY_OT_SLOT 2047`. Note `quake-core/sky.rs`
is one of quake's six intentional 64-bit guest sites.

Proposal: `quake-core` re-exports `psx_bsp::sky` for the projection half (same
pattern as `quake-formats`' `pub use psx_bsp::*`), quake's renderer calls the
engine's submit. Keep quake's texture-window scoping (the 2026-08-13 E2
window-restore fix in `VISUAL_PARITY.md`) if the engine submit does not already
do it; check `sky.rs` for the E2 terminal reset.

Gate: sky region of the visual parity hash, packet counts (`3,520 window
selectors / 3,344 resets` is the documented baseline).

#### H-13 One RLE PVS decoder

Cascades to: PSoXide + quake-psx + hl-psx.

Evidence: three implementations of the same Quake run-length row decode:
`psx-bsp/src/pxbsp.rs:67-82` (`decompress_leaf_row`, crate-private;
`decompress_visibility`), quake `renderer.rs:4265` (`decompress_visibility`,
`merge_visibility`), hl-psx `main.rs:19591` (`decompress_vis`) and `:19638`
(`merge_vis`, the underwater merge the engine lacks). All three produce a
`visible_leaves.div_ceil(8)` byte row.

Proposal: make the engine decoder `pub` (it is generic: bytes in, bitset out,
bounded, no allocation), add a `merge_into` for the second-row OR that both
quake (water portal) and hl-psx (underwater) need, and have both games call it.
Row sizes: quake `MAX_VISIBILITY_BYTES 160`, hl-psx `VIS_BITS`, PXBSP
`PXBSP_MAX_VISIBILITY_BYTES 1024`; the function takes a caller slice so the
budgets stay per game.

Gate: quake + hl hashes exact (this is a pure function; a host test with the
three games' own fixtures, e.g. quake's `build-psoxide/world-chunks/*.psb` and
hl-psx's cooked rooms, is the cheap proof).

#### H-14 hl-psx player collision on `psx_bsp::collision` through an adapter

Cascades to: hl-psx joins Cortex + quake on one tracer (a correctness fix or
optimisation in the tracer then reaches all three).

Evidence: hl-psx `phys.rs` (2,790) is a fixed-point `SV_RecursiveHullCheck`
port in raw i32 units, Q1.3.12 normals, Q27.5 plane distances, GoldSrc
`DIST_EPSILON = 1/32`; `psx_bsp::collision` is the canonical allocation-free,
iterative (64-entry stack) tracer in Y-up Q20.12 with `TRACE_PLANE_EPSILON_Q12
= 128` (= 1/32 unit in Q12). quake-core already did exactly this migration
through `quake-core/src/collision.rs` (617) + `bsp_axis_adapter.rs` (93) with a
1,068-line parity test. Quake and GoldSrc hulls share the clipnode model.
`finish-line-plan` 4.1: "a source audit finds no second shipping BSP or
collision authority."

Proposal:
1. Write the parity harness first: replay hl-psx's recorded traces (the
   reference-trace build emits `trace_*` lines) through both tracers on the
   host and diff results. The quake parity test is the template.
2. Decide the unit bridge: either hl-psx's cooker (`host/hl-bsp`) emits
   clipnode planes in the engine's Q20.12/Q3.12 form (preferred, zero runtime
   cost, matches "adapters are resolved at cook time"), or a runtime adapter
   converts per query (rejected unless measured free).
3. Swap `phys.rs`'s hull walk for `CollisionHull::trace` via
   `CollisionTraceProvider`; keep GoldSrc `PM_*` movement policy, duck hull,
   ladder and water logic in hl-psx. `character_motor` is not involved
   (different genre policy, see parity audit "Player movement policy").

Risks: hl-psx's `play()` PC16 ceiling (any +13 KB breaks the link) and the
documented rule "do not extract from `play()`". A tracer call is a call, not an
extraction, but the I-cache working-set measurement must be repeated. Precision:
Q27.5 distances vs Q20.12 means the cook must prove no range loss on the
largest hl map (c1a0 etc.); the audit over 96 maps exists to run.

Gate: hl-psx full regression matrix (determinism, packet, 20 fps budget),
Hazard tape flips/hashes exact, `playsize.sh`, memory report, plus the new
parity harness.

Decisions: whether hl-psx changes its cooked clip-hull representation (a
format bump, `HLMH` -> next) to carry engine-form planes. Recommendation: yes;
the cooker is in-repo and the format is already versioned.

#### H-15 Lift the analog-handshake input policy into `psx-pad`

Cascades to: quake-psx + hl-psx (+ any future game).

Evidence: quake `input.rs:52-56` "follows HL-PSX's live-input policy";
`input_policy.rs` (405) carries `ANALOG_RETRY_POLLS 15`, `ATTEMPTS 8`,
`UNKNOWN_MODE_TRUST_POLLS 3`, `AnalogRetry`, `EdgeTracker`, and a compile-time
assertion that its local `button` module equals `psx_pad::button`
(`:11-25`). hl-psx has the original in `main.rs:4153-4462`
(`poll_live_semantic_input:4291`). PSoXide's own SCPH-1200 work put
`DEFAULT_SETUP_SPINS` in `psx-pad` but not the retry/trust policy. `psx-pad`
already hosts `PadTracker`, `aim_curve`, `Deadzone`, `Pacing`.

Proposal: move `AnalogRetry` + the trust window into `psx_pad::handshake` (or
`tracker`), delete quake's local `button` mirror, make hl-psx and quake call it.
Keep each game's axis curves local (they are policy).

Gate: both games' input-dependent routes (quake start-route, hl Hazard tape)
exact; pad-poll counts exact (`SHIP_BOOT_MIN_PORT1_POLLS`, hl `pad_polls`).

#### H-16 hl-psx world format onto the `psx-bsp` resident core (owner decision)

Cascades to: hl-psx joins H-10/H-13 traversal; the whole BSP trio on one
world mechanism.

Evidence: hl-psx `map.rs` (2,342) reads its own `HLMA..HLMH` container with
GoldSrc attribute semantics (lightstyles, texture-animation chains,
brush-entity leaf membership, water-alpha in bit 15 of the lightmap face
count); `psx-bsp` has two containers already and a record vocabulary that the
finish-line plan explicitly does not require to be interchangeable across
games. `CookedDrawSurfaceCommand` is the one seam hl-psx already shares.

Proposal: do not unify the container. Split the `.hlm` into "BSP core lumps"
(nodes, planes, leaves, marks, clipnodes, vis, brush models) emitted in the
`psx-bsp` PSB5 record forms, and "GoldSrc payload sections" (lightmap planes,
animation chains, props, nav, logic) that stay hl-psx's. `PxbspResidentMap` /
`ResidentMap` then serve the core and hl-psx's `Map` wraps them exactly as
quake's `asset.rs::ResidentMap` wraps `psx_bsp::resident::ResidentMap`
(`asset.rs:295-911`). This is the quake pattern applied a second time and it
is what makes H-10's traversal reachable for hl-psx.

Cost: a cooker rewrite in `host/hl-bsp` (the BSP-lump writer portion), a
`map.rs` rewrite, and every `main.rs` accessor that reads those lumps
(`PvsFaceRec`, `rebuild_pvs_cache`, `camera_leaf`). The `play()` size
constraint makes this the riskiest item in the plan; the "cooked record sizes"
must not grow hl-psx's 10,016 B static headroom or `MAP_BUF`.

Gate: the whole hl-psx harness, twice (determinism), plus fleet audit over all
maps (`tools/perf/fleet_audit.py`) for RAM.

Decisions: owner. The finish-line plan says no interchangeable format is
required; this proposal respects that (payload sections stay private) but it is
a large hl-psx change for a cascade benefit whose size should be measured
first: run H-10 and H-13 on quake, measure the gain, then decide H-16.

#### H-17 Lightstyles and liquid turbulence as shared `psx-bsp` modules

Cascades to: quake-psx (owner) + PSoXide (currently missing both, parity audit
P2) + hl-psx (has lightstyles in GoldSrc form).

Evidence: `quake-core/lightstyle.rs` (243, 65-entry table, `ANIMATION_HZ`),
`quake-core/liquid.rs` (294, `R_GenTurbTile` 64x64), quake `renderer.rs:827-930`
(double-buffered liquid tiles); `docs/quake-parity-audit.md` rows "Dynamic
lights / lightstyles: Missing as Quake systems" and "Liquids: Partial".

Proposal: when PSoXide decides to add either, port from quake-core into
`psx-bsp` (as `sky.rs` was) and have quake re-export, rather than writing a
second implementation. Not urgent; listed so it is not built twice.

Gate: quake's hashes exact on re-export; PSoXide oracle tests as per the parity
audit's closing rule.

#### H-18 Shared chunk cache and blocking chunk read above `psx_pack::cd`

Cascades to: quake-psx + hl-psx + voxide + nitroxide + the two launchers.

Evidence: `psx_pack::cd` is the shared sector machine (867 lines) and its doc
(`cd.rs:40-43`) says the entry-table cache "is a game-side optimization: keep
it in the game". Result: quake has `ASSET_CACHE[12]` + `ChunkStream` +
`read_chunk_exact` (`platform.rs:609-890`, about 290 lines), hl-psx has
`PACK_CACHE_ENTRIES = 672` + `load_chunk` (`cdstream.rs`, 708 lines of cache
+ shim), and `psx-game-runtime::cd_stream` has `CdController` +
`WorldRoomSlotsReadJob<N>` (1,228 + 630). Three scheduling layers over one
transport.

Proposal: add a `psx_pack::cd::EntryCache<const N: usize>` (keyed residency is
what `psx-cache::SlotCache` already does; no guest uses `psx-cache` today) and
a `read_chunk_blocking(entry, dst)` that covers the quake and hl-psx blocking
paths. Leave the background budgeted job in `psx-game-runtime`. Games keep
their chunk-id maps.

Gate: hash-exact boots for every consumer (the CD timing model is silicon
calibrated; sector counts and poll windows in the headless gates will show any
drift).

#### H-19 Telemetry counter ids in one table

Cascades to: all telemetry consumers + the emulator frontend.

Evidence: hl-psx `telemetry.rs` defines `affine_counter::*` slots 85-108
outside `psx-telemetry`'s `COUNTER_COUNT 267`; voxide re-declares the MMIO
writes; the frontend decodes by shared tables.

Proposal: reserve a per-game range in `psx-telemetry` (or a registry of named
ranges) so ids cannot collide and the chart tooling names them.

Gate: `frontend launch --counter-log` columns unchanged for existing consumers.

### T2: all games

#### H-20 hl-psx switches to `psx_asset::hmd8`

Cascades to: hl-psx + PSoXide (the model-residency convergence queued first in
the standardisation doc).

Evidence: section 3.3; `hmd8-sdk-unification` memory ("step 4 is a deliberate
bump, not a drive-by"); `psx-asset/src/hmd8.rs:5-7`.

Proposal: after H-00 moves hl-psx's pin, replace `mod model` with
`use psx_asset::hmd8 as model` (plus the vertex cap via
`load_with_vertex_cap`), run the model audit over the 232 fixture chunks
(`HMD8_FIXTURE_DIR`), and delete `game/src/model.rs`. Then pursue the queued
skeletal-projection convergence (`main.rs:26150-26418` bone-range composition
vs `render3d` per-joint helpers) only with a measured case; the two designs
differ and hl-psx's is the one under a PC16 ceiling.

Gate: hl regression matrix; weapon gallery captures; `playsize.sh`.

#### H-21 voxide: delete the telemetry shim, adopt `PadTracker` and `ParticlePool`

Cascades to: voxide.

Evidence: `voxide/game/src/telemetry.rs` (143) carries the same three MMIO
writes as `psx_telemetry::emit` (whose header names voxide as a source of the
shim), gated on the game's `emulator-telemetry` feature instead of the SDK's
`emit`, and adds a `cycles()` reader of port `0xBF80_2F08` that the SDK module
does not expose (diff is 106 lines, mostly docs and the gate name); pad edges
`main.rs:1331, 9428-9430`; SoA particles `main.rs:5094-5140` vs
`psx_fx::ParticlePool` (voxide already uses `psx_fx::LcgRng`).

Proposal: promote `cycles()` into `psx_telemetry::emit` first (hl-psx and the
engine profiler may want it too), then delete voxide's shim and forward
`emulator-telemetry = ["psx-telemetry/emit"]` as nitroxide does; then the pad
and particle swaps. Three small commits, each gated by `make smoke` hashes
(`--dump-hash`) and the `make profile` counters. The particle swap changes
layout (SoA -> pool struct); measure before keeping.

#### H-22 magikarp-pong and the demo launcher: SDK CD-DA and pad helpers

Cascades to: arcade pong, demo launcher.

Evidence: pong `main.rs:172-176, 255-290, 1060` re-implements
`psx_io::cdda::CddaStarter` (+ `CddaEndDetector` for the loop restart);
launcher `main.rs:613-614, 731` re-implements `PadTracker` edges.

Proposal: adopt the SDK types; gh-psx, nitroxide and the launcher already use
`CddaStarter`, so the reference is in-tree.

Gate: `make check` + headless boot hash per program.

#### H-23 Un-fork the demo-disc infrastructure crates

Cascades to: psx-demo-disc + psoxide-arcade.

Evidence: the Rust sources of `loader/`, `disc-toc/`, `carousel/` are
byte-identical between the outer disc and the arcade submodule (only the
`Cargo.toml` path dependencies differ); `tools/mkdisc/src/main.rs` differs by
158 diff lines.

Proposal: the arcade consumes the outer disc's `loader`, `disc-toc`, `carousel`
and `mkdisc` as path or git dependencies (or the crates move into PSoXide
`crates/` next to `psx-iso`, which `mkdisc` already depends on). Diff the 158
mkdisc lines and parametrise.

Gate: both `make check` suites (carousel 20, disc-toc 6, mkdisc 20 tests);
`quake-headless-check` pinned frame hashes per pressing.

#### H-24 CD-DA track convention helper

Cascades to: nitroxide, gh-psx, quake-psx `music.rs`, the two launchers.

Evidence: each computes "menu tracks are 2..N, shifted by
`disc_base::shift_track`" locally (`nitroxide/music.rs:1-41`,
`quake-psx/music.rs:62-268` with `FIRST_TRACK 2`).

Proposal: one `psx_io::cdda::TrackSet { first, count }` with `nth(i)` applying
`disc_base`. Small; bundle with H-22.

#### H-25 hl-psx: `psx-sfx` bank and `psx_fx::shake`

Cascades to: hl-psx.

Evidence: hl-psx `sfx.rs` (643) is an SPU-resident bank + rotating one-shot
pool, same design as `psx_sfx::{Bank, Player<N>}` but over its own `HSFX`
container; camera shake is local (`main.rs:5797-5804`) while `psx_fx::shake`
is unused.

Proposal: adopt `Player<N>` for voice rotation first (the container can stay),
then decide on `Bank`. Low value, listed for completeness; do after H-20.

#### H-26 Frame loop: quake-psx onto `FrameScheduler` (not `App`)

Cascades to: quake-psx inherits PSoXide's pacing work (pipelined present,
deadline accounting, overload policy).

Evidence: quake `quake.rs:184-216` hand-rolls the elapsed-tick clamp and the
`perf-fixed-ticks` mode; `platform.rs:322-470` hand-rolls
`gpu_begin_frame`/`gpu_end_frame`/`gpu_present_pending_frame` which is the
same shape as `App::present_pending` + `OtFrame::submit_async`. PSoXide's
`FrameScheduler` (`scheduler.rs`, 568) is allocation-free and independent of
`Scene`; every other engine consumer goes through `App::run`.

Proposal: quake keeps its own `loop {}` and ownership of pad/audio/music but
asks `FrameScheduler::next_action` for fixed vs visual decisions, and presents
through `OtFrame`. `perf-fixed-ticks` becomes a `SchedulerConfig` (fixed N
ticks per visual) that PSoXide could also use for its own deterministic
benches. hl-psx is explicitly excluded (20 Hz sim, decoupled present, PC16
ceiling, "do not extract from `play()`"); revisit only if H-16 lands.

Gate: quake fixed-ticks bench within noise; ship-boot gates; input poll counts
exact (pad polling moves relative to vblank if the loop order changes, which
breaks tapes; keep the poll position).

Decisions: whether PSoXide wants a fixed-ticks-per-visual scheduler mode in the
engine. Recommendation: yes, it is a two-line config and it gives Cortex a
determinism bench it does not have.

#### H-27 voxide onto `App::run`

Cascades to: voxide.

Evidence: `main.rs:1721-1745` (`sim_n = dt.min(4)`), `OT[2]`/`SKY_OT[2]`,
`wait_vblank` shim `main.rs:3113`.

Proposal: candidate only. voxide is 17k lines with three loops (game, menu,
intro) and the port is mechanical but not small. Do it when voxide next needs
engine pacing features; otherwise leave.

Not proposed: pico8-psx loops (bit-exact PICO-8 semantics, trivially small) and
the launchers (no gameplay cadence to share).

### T3: longer horizon

#### H-30 Packet emitter convergence (queued item 3 in the standardisation doc)

Cascades to: all three BSP games.

Evidence: hl-psx's emitter (`main.rs:20597-26149`, about 5,500 lines incl.
affine ranking) vs `classic_affine` (3,103) used by PSoXide + quake. hl-psx
measured: `try_emit_quad_corners` 15.9% of slow samples; the 4x4 native grid
measured -12.04% room cycles on a moving view in the PSoXide A/B
(`PSX_HIGH_THROUGHPUT_TESSELLATION.md`); the standardisation doc's own rule is
"move only the byte-exact common packet assembly below the adapters".

Proposal: do not attempt a merge of the two emitters. Instead, identify the
byte-identical leaf operations (GP0 word packing: `gp0_vertex/xy/color/texcoord`
at hl `main.rs:1374-1389` vs the engine's `GpuPacket` impls; quad pairing
guard; GT4 child emission) and move those into `psx_gpu::prim` / `classic_affine`
helpers both can call. Measure each on console timing, per the queue.

#### H-31 Shared menu kit for raw-loop games (optional)

Evidence: every program draws its own immediate-mode menu; `psx_engine::ui` is
bound to `psx-level` records and used by none of the games; `MicrogameShell`
(on `588d7637`) already covers the arcade trio.

Proposal: only if a fourth game needs it. A `psx-menu` crate with rows, slider,
repeat navigation over `PadTracker` would replace hl `menu.rs` rows, quake
`menu.rs` pages, voxide `menu_*`, pong/launcher menus. Listed as a candidate,
not recommended now (over-building risk, and menu changes invalidate hl's
poll-bound tapes).

#### H-32 Touched-leaf entity linking as a `psx-bsp` primitive

Evidence: parity audit P1 ("Entity PVS linking: Partial"); quake has
`NearCandidates` (`entity.rs:8311-8354`) and leaf-linked render entities;
hl-psx has `refresh_live_entity_pvs` (`main.rs:20260`) and brush-entity leaf
membership cooked into `.hlm`.

Proposal: when PSoXide builds this, build it in `psx-bsp` with quake's
`entity.rs` as the policy consumer from day one so it is not written twice.

---

## 6. What not to harmonise (measured, do not retry)

These are recorded rejections with numbers; a working agent should not reopen
them without new evidence.

| Item | Evidence | Why not |
|---|---|---|
| One clipping arithmetic policy (canonical truncating Q12) | standardisation doc: Quake +0.004 fps (under the 0.122 noise floor) and +272 guest bytes; PSoXide room +0.12%, +2 KiB sector, deadline misses 6 -> 31 | control flow is shared, numeric policy stays per adapter |
| `psx-engine` depending on `psx-render-contract` | sim ticks 288 -> 370, skipped vblanks 144 -> 185, deadline misses 6 -> 47, reproduced | link layout; cookers depend on the contract, the hot engine does not |
| Dynamic dispatch in any hot loop | `projection.rs:4-7`: an ordinary shared call in the quad projection path loses to the direct-mapped I-cache | monomorphize or resolve at cook/load |
| Extracting functions out of hl-psx `play()` | `PERF_HANDOFF.md` section 4: three attempts each grew `play()`; 14-line candidate grew it 112,636 -> 112,824 B | PC16 ceiling at ~89% |
| Broad view cache in hl-psx / per-surface parallel arrays | `STABLE_20FPS_AUDIT.md:181`: 136 KiB at hl scale vs 10 KiB headroom | RAM |
| Packet staging in scratchpad | same doc | scratchpad is not a DMA source |
| Async OT double buffering (full phase 2) | hl-psx: ~5% ceiling for +140 KB; PSoXide: +33 ms input-to-photon, rejected by owner | |
| Crate-wide `opt-level = "s"` on the guest | hl-psx faults at boot; PSoXide observed a miscompile at `-Os` | toolchain nightly-2026-03-25 |
| A shared scratchpad allocator | `scratchpad.rs:9-12`: overlapping phase reservations are intentional | address authority only |
| Global two-level affine-error selector in Quake | `RENDERING.md`: commands 2,443 -> 6,788, GPU cycles +82.3% | keep per-game selection |
| Moving GTE work to the CPU to dodge hazards | memory `keep-work-on-gte-not-cpu` | |

Also a convention flag for the owner, not a rejection: `attributed_clip.rs`
introduces `i64` helpers (`crossing_fraction_q12_i64`, `crossing_fraction_q16_i64`,
`lerp_q16_i32` taking an `i64` fraction) for PXBSP's exact plane distances,
and quake-core keeps six intentional 64-bit guest sites. The convergence
handoff rule says "no new ... 64-bit arithmetic enters guest traversal,
movement, visibility, or combat hot paths", the PXBSP Q16 policy is measured
as smaller and faster than reducing to Q12, and `psx-rt` carries a corrected
`__divdi3`. These facts coexist; the working agent should confirm which rule
is binding for clipping and record the answer in the standardisation doc.

---

## 7. Design smells surfaced during the survey

Not proposals; things to raise with the owner.

1. `psx-bsp` is `#![allow(missing_docs)]` with a `ponytail:` note about
   verbatim-copy provenance (`lib.rs:13`). Fine as a lift; it is now the
   shared world crate and the docs should follow H-10.
2. `quake-formats` is a facade (`pub use psx_bsp::*`) plus six Quake-specific
   constants and the QIDX/QSB1 formats. Either rename it for what it is or fold
   the Quake additions into `quake-core`.
3. `RESIDENT_MAP_ARENA_BYTES = 828_000` (quake) vs `MAX_RESIDENT_MAP_BYTES =
   1_100_000` (`psx-bsp`): two budgets for one loader, one hard-pinned with a
   2,034-byte margin. Make the engine constant a default the game overrides by
   name, which it already effectively does via `with_capacity`.
4. `psx-cache` (483 lines) has no consumer anywhere in the ecosystem; H-18
   would give it one or it should be retired.
5. `psx-game-runtime` pulls `psx-hw` and `psx-io` (the only engine crate
   touching hardware data) because `cd_stream::hw` lives there. After H-18 the
   hardware half belongs in the SDK.
6. hl-psx's `scratchpad.rs` re-implements `ptr_at`/`clear` it already re-exports
   the base of; keep only the trampoline and the A/B features.
7. The demo disc's own `psoxide-arcade` pin (`588d7637`) differs from every
   sibling (`9c298d83`) and only matches because `PSOXIDE_FROM` overrides it at
   disc-build time. H-01 removes the class of bug.

---

## 8. Expected cascade after the plan

If H-00, H-01, H-10, H-13, H-14, H-15, H-18, H-20 land, the cascade map for
the BSP trio becomes:

| Layer | Cortex | quake-psx | hl-psx |
|---|---|---|---|
| scratchpad, attributed_clip, projection | shared | shared | shared |
| classic_affine packet path | shared | shared | own emitter (H-30 leaf helpers only) |
| BSP traversal + PVS + face marking | shared | shared (H-10) | own traversal over shared PVS decoder (H-13); shared traversal only after H-16 |
| hull collision + movers | shared | shared | shared tracer (H-14), own movement policy |
| sky / lightstyle / liquid | shared | shared (H-12, H-17) | own |
| model container | PSXMDL/psxanim + HMD8 available | alias table in psx-bsp | HMD8 shared (H-20) |
| CD transport + chunk cache | shared | shared (H-18) | shared (H-18) |
| pad handshake + edges | shared | shared (H-15) | shared (H-15) |
| frame scheduler | shared | shared (H-26) | own |
| pin | one disc rev (H-01) | one disc rev | one disc rev |

Every row marked "shared" is a place where one optimisation reaches all games
on the next repin, which H-01 makes a single verb.

---

## 9. Owner decisions needed (collected)

1. H-00: re-pin the stale Quake visual-parity world hash after the brightness
   change.
2. H-01: collapse Cortex current onto the programs' SDK rev; keep only the
   legacy Cortex pin frozen.
3. H-10: `psx-bsp` is the mechanism authority for the BSP traversal; quake-psx
   keeps policy in adapters. (This restates the north star; asking because it
   decides which of two forks wins.)
4. H-14: hl-psx bumps its cooked clip-hull representation to the engine's
   Q20.12/Q3.12 planes.
5. H-16: whether to split `.hlm` into psx-bsp core lumps + GoldSrc payload.
   Recommend deciding after H-10/H-13 are measured on quake.
6. H-26: add a fixed-ticks-per-visual mode to `FrameScheduler`.
7. Section 6 convention flag: which 64-bit rule binds the clipping kernel.

---

## 10. Verification protocol for the working agent

Do these before trusting any claim above, and again after each item.

### 10.1 Confirm the state

```bash
cd /Users/ebonura/Desktop/repos/PSoXide && git status --short | grep -E 'engine/|sdk/|docs/shared' && git log --oneline -5
```

```bash
for r in quake-psx hl-psx; do echo "== $r"; git -C /Users/ebonura/Desktop/repos/$r status --short | head; cat /Users/ebonura/Desktop/repos/$r/.psoxide/.hydration-stamp 2>/dev/null; done
```

```bash
grep -nE 'EXPECTED_PSOXIDE_REV' /Users/ebonura/Desktop/repos/psx-demo-disc/Makefile
```

### 10.2 Confirm the duplicate claims with greps

Quake does not call `psx_bsp::render`:

```bash
grep -rn 'psx_bsp::render\|psx_bsp::sky\|Renderer::draw_frame' /Users/ebonura/Desktop/repos/quake-psx/game/src /Users/ebonura/Desktop/repos/quake-psx/crates || echo "no consumer (claim holds)"
```

`world_batch` is dead:

```bash
grep -rn 'world_batch' /Users/ebonura/Desktop/repos/quake-psx --include='*.rs' | grep -v 'crates/quake-core/src/world_batch.rs'
```

hl-psx does not use the SDK HMD8:

```bash
grep -rn 'hmd8' /Users/ebonura/Desktop/repos/hl-psx/game/src || echo "no consumer (claim holds)"
```

Three PVS decoders:

```bash
grep -n 'fn decompress_vis\|fn decompress_visibility\|fn decompress_leaf_row\|fn merge_vis' /Users/ebonura/Desktop/repos/hl-psx/game/src/main.rs /Users/ebonura/Desktop/repos/quake-psx/game/src/renderer.rs /Users/ebonura/Desktop/repos/PSoXide/engine/crates/psx-bsp/src/pxbsp.rs
```

voxide telemetry shim (expect the same MMIO ports, a different feature name,
and a `cycles()` reader only on the voxide side; 106 diff lines at survey time):

```bash
diff /Users/ebonura/Desktop/repos/voxide/game/src/telemetry.rs /Users/ebonura/Desktop/repos/PSoXide/sdk/crates/psx-telemetry/src/emit.rs
```

Demo-disc forks (expect only `Cargo.toml` for loader and carousel, nothing
for disc-toc, `Cargo.toml` + `src/main.rs` for mkdisc):

```bash
cd /Users/ebonura/Desktop/repos/psx-demo-disc && for d in loader disc-toc carousel tools/mkdisc; do echo "== $d"; diff -rq --exclude=target --exclude=Cargo.lock $d games/psoxide-arcade/$d; done
```

### 10.3 Gates per game (run the existing ones, do not invent new evidence)

- PSoXide: `make combat-checkpoint` (canonical combat gate), the Arena/E1M1
  120-visual-frame exact gate from the standardisation doc, native suites
  (`psx-bsp`, `psx-engine`, `psx-game-runtime`, `psxed-project`, `psxed-ui`
  counts are recorded in the convergence handoff 0.3 for comparison).
- quake-psx: `host/quake-build` actions (see its `Action` enum for the exact
  verbs): check, visual-parity regression, e1m1-chain bench (two runs per
  build, compare against the ±0.122 floor), start-route, bestiary, disc +
  provenance. Read `VALIDATION.md` for the current pinned numbers before
  running.
- hl-psx: `cargo run --release -- regress --psoxide <tree>` (full matrix, twice
  each scenario is built in), the Hazard tape replay and flip count,
  `bash playsize.sh <baseline>`, `tools/perf/visual_perf_gate.py --gate`,
  memory report. Build with `--features emulator-telemetry` only; never
  `performance-telemetry` for an A/B.
- Disc: `make check`, `make quake-headless-check`, `make relocation-check`.

### 10.4 Rules that bit previous sessions (from the project memory and docs)

- Staged/hydrated guest builds can reuse stale rlibs because copied files keep
  old mtimes; check the `.exe` timestamp after every guest build
  (`psoxide-link` now stamps mtimes, uncommitted).
- A single-run visual A/B is not evidence when a live enemy is in the scene.
- Headless press scripts die on boot-flow changes; read the poll trajectory
  first. hl-psx tapes are poll-bound: a menu row shift invalidates them.
- Never point a remote git dependency at an unpushed commit.
- Never use `opt-level = "s"` for the guest.
- Keep engine/guest math i32/u32 fixed point unless a documented measurement
  says otherwise (section 6 flag).
- Do not `git commit --amend` or hard-reset in the owner's live trees; the
  owner commits from Zed in parallel.

---

## 11. Appendix: evidence index

Uncommitted engine seams: `engine/crates/psx-engine/src/scratchpad.rs` (SIZE,
`base_ptr`, `ptr_at`, `clear`; absolute symbol `__psoxide_scratchpad =
0x1f800000`), `projection.rs` (`ScreenClipBounds`, outcodes,
`classic_{triangle,quad}_screen_rejected`, `half_space_outcode5`, re-export of
`psx_gte::scene::project_*_scheduled`), `attributed_clip.rs` (`ClipTraversal`,
`AttributedClipPlane<Vertex>` trait, `clip_convex_plane{,_uninit}`, Q12/Q16
fraction and lerp helpers), `engine/crates/psx-render-contract/src/lib.rs`
(`CookedDrawSurface` 10 B, `CookedDrawSurfaceCommand` 24 B, zero deps).

Consumers of the seams: quake `renderer.rs:8-17, 3954-3981, 4113-4154`;
hl-psx `render.rs:9-15, 467-500`, `main.rs:105-108, 381, 2596, 2938, 23333,
26408, 27487`, `scratchpad.rs:9, 25-45, 76`, `map.rs:80`,
`host/hl-bsp/src/main.rs:17, 1300, 12188`; PSoXide `psx-bsp/src/render.rs:9-22,
791-800`, `classic_affine.rs:23-30`; quake-cook `geometry.rs:3, 20, 358, 376`.

Format containers: XBSP/PSB5 `psx-bsp/src/lib.rs` (15 lumps, record sizes
Vertex 12, Plane 14 / CompactPlane 12, TextureInfo 14, Face 10, Leaf 14,
Node 16, ClipNode 6, BrushModel 32, MapEntity 50, alias header 68); PXBSP v6
`pxbsp.rs` (16 lumps, Materials replaces TextureInfo, StreamingIndex added,
`PxbspEntity` 32 B, `PxbspBrushDoor` 16 B, `PXBSP_MAX_VISIBILITY_BYTES 1024`);
quake-cook `map.rs:77-113` emits PSB5; editor `brush_pxbsp.rs:764` emits PXBSP;
hl-psx `.hlm` `HLMA..HLMH` (`shared/hl-format`, header 52/56 B).

Budgets: quake `GPU_ARENA_BYTES 0x30000` x2, `OT_DEPTH 2048` x2, resident
828,000 (used 825,966), packet ceilings 400k/460k; hl-psx `OT_LEN 320`,
`MAX_RENDER_PACKETS 1469` slots x 56 B, arena 950,832 B, static headroom
10,016 B, `play()` 116,796 B; PSoXide `DEFAULT_PACKET_WORDS 49,152`,
persistent asset cap 344 pages (measured ceiling 354), sanctum example
`PRIMITIVE_PACKETS` 229 KB at ceiling 4096 x 56 B.

Pins: quake-psx `host/quake-build/main.rs:37` + root `Cargo.toml`; hl-psx
`Cargo.toml:20-21` + `host/hl-build/main.rs:16`; sibling games
`psoxide-pin/Cargo.toml:22`; arcade `psoxide-pin/Cargo.toml:11`; disc
`Makefile:19, 21, 25, 83`. Toolchain everywhere: `nightly-2026-03-25`.
