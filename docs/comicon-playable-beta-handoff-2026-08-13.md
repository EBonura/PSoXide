# Comicon playable-beta handoff

Written 2026-08-13. This is the authoritative close-out for the current
PSoXide editor, Quake-PSX, and default demo-disc convergence campaign.

This document supersedes older status summaries for the current state. Older
plans and the long convergence handoff remain valuable engineering history,
but their TODO lists and completion language must not be treated as current
without comparing them to the exact revisions below.

## 0. Mandatory instructions to the next model

Do not begin by implementing anything. First try to disprove this handoff.

1. Re-read every exact revision with `git rev-parse HEAD` and every tree with
   `git status --short --branch`. Evidence belongs only to the revision on
   which it ran. Never transfer a green result silently to a descendant.
2. Recalculate the final disc hashes and compare them with the embedded
   provenance receipt. Check the launcher and Quake version strings in the
   pressed image. A stale ignored `dist/` directory is not evidence.
3. Distinguish four different claims:
   - host unit/integration tests passed;
   - a real MIPS guest compiled;
   - a headless emulator replay passed;
   - an original PlayStation passed.
   Only the first three exist at this checkpoint. Do not imply the fourth.
4. Challenge the strongest negative claim: **the full Quake shareware episode
   has not been certified start-to-finish**. E1M3, E1M4, E1M5, E1M6 and E1M8
   still lack durable end-to-end ordinary-play routes. Do not call this an
   Episode 1 release until the owner completes a manual playthrough or later
   automation proves it.
5. Challenge the PSoXide native-editor UX personally before changing code.
   The final texture-lock and UV fixes have automated egui coverage, but the
   owner has not yet accepted their feel in the native window.
6. Preserve user work. Canonical promotion is complete, but the pre-promotion
   backup refs, stash objects, Quake preservation commit and PSoXide untracked
   paths in section 12 remain recovery evidence. Never clean, drop or rewrite
   them until the owner has accepted the promoted repositories and hardware
   candidate.
7. Preserve the WIP listed in section 11 by immutable stash object hash. Never
   apply it directly onto a release branch and never integrate a mixed worker
   tree wholesale.
8. Prefer image-free/headless validation. Take screenshots only when visual
   judgment is the point; a screenshot is not a substitute for telemetry,
   hashes, deterministic replays, or native owner acceptance.
9. Do not rebase, push, publish, or change repository default branches unless
   the owner asks. The canonical promotions are currently local-only.
10. If a claim in this file disagrees with live code or a live artifact, the
    live evidence wins. Record the discrepancy instead of rationalising it.

The correct next priority is level authoring, not more route automation. Only
resume Quake route work if manual play exposes a hard blocker or the owner
explicitly asks for full certification.

## 1. Executive outcome

The scoped beta has reached a useful stopping point:

- The **PSoXide Editor** is a usable BSP editor for the owner's original
  souls-like game. It is not a Quake level editor. It can create, edit,
  texture, populate, cook and playtest simple PXBSP levels with the existing
  souls player, Rust Mantis enemy, swords, doors, triggers, checkpoints and
  liquid hazards.
- **Quake-PSX** is a substantial playable-beta port of Quake 1.06 shareware.
  Its shipping build uses the shared PSoXide BSP/collision foundation and
  contains the shareware Episode 1 maps and gameplay. Important production
  defects discovered by route work were fixed and banked. It is not yet
  certified perfect or start-to-finish.
- **Quake shareware is on the default PSoXide demo disc**, not behind a flag.
  The only opt-in pressing remains `HL=1`, which adds Half-Life. The final
  default disc contains no Half-Life.
- **Cortex Ignition remains the visible legacy grid-based fallback** on the
  default disc. It was not replaced by the new BSP souls slice. The BSP slice
  remains an editor/standalone candidate until its build and chain-load path
  is independently fixed and proven.
- The combined disc was built and its launcher-to-Quake chain-load passed two
  deterministic headless replays. This is a burn candidate, not hardware
  certification.
- The validated histories have been promoted into the canonical PSoXide,
  Quake and demo-disc folders. All overlapping owner work was preserved first
  through immutable refs, a tracked-only stash or a dedicated preservation
  commit. The promotion remains local and unpushed.

For tomorrow: launch the editor, perform the short native checklist in section
7, then start building the Comicon level. Do not wait for exhaustive Quake
automation.

## 2. Product boundary

### 2.1 PSoXide

PSoXide is the owner's game engine and editor. Its product is an original
souls-like technology demo using Quake-inspired BSP world structure and shared
low-level BSP algorithms. It retains PSoXide capabilities such as entities,
skeletal characters and animation, authored hit/hurt capsules, weapon
attachments, lights, materials, combat, checkpoints and game-state reset.

PSoXide levels are not Quake `.bsp` maps and the editor does not author Quake
content. Quake has its own cook, content and gameplay runtime.

### 2.2 Quake-PSX

Quake-PSX is a separate game using Quake shareware data and PSoXide's shared
engine/toolchain foundation. The current target is a playable Comicon beta,
not perfect parity. Registered-only Quake monsters with no shareware instance
are out of scope. Save/continue, Quake CD music and full-version opt-in support
are explicitly deferred.

### 2.3 Demo disc

The demo disc contains separate programs. The PSoXide souls demo and Quake do
not become one game by sharing a disc or BSP code. Quake is always present on
the default pressing. `HL=1` adds Half-Life; it does not enable Quake.

The pressed `CORTEX IGNITION` entry deliberately remains
`editor/samples/cortex_v1`, the legacy grid project. It is the reliable
fallback while levels are authored in the new BSP editor. Do not point the
disc recipe at `souls-bsp-vertical-slice` merely because that project is the
new architecture: the two payloads have separate readiness and evidence.

### 2.4 Streaming

The Comicon souls slice uses measured whole-map PXBSP residency. Fine-grained
microsection streaming is deferred until authored maps become large enough to
justify its complexity. BSP does not prevent future section streaming, but it
requires explicit section boundaries, portals/PVS ownership, cross-section
entity rules and transition budgets. Do not block current level design on it.

## 3. Exact frozen state

| Product | Canonical worktree | Branch | Functional/final revision | State |
| --- | --- | --- | --- | --- |
| PSoXide editor | `/Users/ebonura/Desktop/repos/PSoXide` | `main` | `b2db85c4b86c3c1ea9748a92d39c85dff8b7610d` | functional/docs promotion base; this update is docs-only |
| Quake-PSX | `/Users/ebonura/Desktop/repos/quake-psx` | `codex/all-rust-quake` | `28507a6dd605730a43909d6b2258f081def68a79` | promoted and clean |
| Demo disc | `/Users/ebonura/Desktop/repos/psx-demo-disc` | `main` | `2061540e234da16fd7a8378b0fc61f5d810ddd68` | promoted, rebuilt and clean |
| Shipping engine pin | `/Users/ebonura/Desktop/repos/PSoXide-final-pin` | detached/pinned | `79d51dd2f2fd78cfb8aa418e2ad123730f56ac3d` | exact Quake and disc contract |

Canonical PSoXide `main` receives this update as a documentation-only
descendant of `b2db85c4`. The editor/runtime evidence remains tied to the named
functional heads below; adding prose does not turn old test output into
new-head evidence.

The demo disc deliberately pins PSoXide `79d51dd2`, not editor head
`b2db85c4`. The later PSoXide commits are host editor/UX, documentation and
gate hardening.
Quake's shipping provenance declares the exact `79d51dd2` engine input. Do not
casually advance the demo gitlink or rewrite that contract.

## 4. Architecture and ownership

```text
PSoXide editor
  -> authors PSoXide project.ron + resources
  -> cooks PXBSP/material/entity/gameplay data
  -> builds PSoXide souls guest
  -> embedded or standalone playtest

Shared PSoXide engine/toolchain at 79d51dd2
  -> psx-bsp formats, tracing, PVS and renderer foundations
  -> consumed by PSoXide souls runtime
  -> consumed by separately built Quake-PSX runtime

Quake-PSX
  -> canonical Quake 1.06 shareware PAK
  -> Quake cook + Rust gameplay/runtime
  -> standalone Quake cue/bin/exe + provenance

PSoXide demo disc
  -> PSoXide gitlink at 79d51dd2
  -> canonical Quake sibling checkout, not a submodule
  -> six fail-closed Quake source/artifact pins
  -> legacy grid Cortex fallback is visible as program 1/11
  -> relocates the Quake image and exposes it as program 11/11
  -> launcher also exposes a twelfth visible CREDITS card
```

The shared collision API is allocation-free and integer-only. Trace flags are
byte-backed rather than Rust `bool`, preventing invalid-bool UB when a failed
trace promises byte-preserving output. Production Quake fixes found during
the final route campaign are already present in `28507a6`; do not cherry-pick
their worker-branch equivalents again.

## 5. PSoXide evidence boundary

### 5.1 What is implemented

- Synchronized 3D and Top/Front/Side orthographic views; Orbit and Free camera.
- 2D and 3D brush selection, coincident-brush cycling, marquee/multi-select.
- Convex brush creation and visible Move, Resize, Edge and Vertex modes.
- Numeric origin, size and face-plane editing; grid snap; duplicate; delete;
  Hollow; two-point Clip; undo-safe drag transactions.
- Per-face material assignment, 3D paint and eyedropper.
- U/V offsets, rotation, independent U% and V%, Reset UV, and Inspector-level
  **Lock texture while moving**.
- The move preview and committed move use the same UV-lock path. Held U/V or
  rotation edits use one explicit transaction anchor, preventing cumulative
  multi-frame drift. An unchanged release frame clears the transaction.
- Point/entity placement, point lights, character profiles and equipment.
- PSoXide vocabulary for doors, triggers, checkpoints, liquids and brush model
  ownership.
- Draft/Release cooking, capacity/error focus, Play and Rebuild & Play.
- A 16384 by 16384 roofless starter courtyard with neutral cobble/brick
  textures, five brushes, a player spawn and two lights.
- Starter-character sync for the souls player, Rust Mantis, swords, clips and
  profiles.
- Souls runtime support for combat, authored hit/hurt capsules, weapon
  attachment, doors, trigger-to-checkpoint activation, lava damage, player
  death, checkpoint respawn and world reset.

### 5.2 Exact safe claims

At `11dfb9f9`:

- `psxed-project`: 500 passed, 1 ignored.
- `psxed-ui`: 406 passed, 1 ignored.
- frontend: 169 passed, 1 ignored.
- `make editor-blank-playtest-check`: passed.
- Formatting retained the same 80 pre-existing drift entries as the
  `b414b2ba` baseline; no bulk formatting churn was introduced.

At `b414b2ba`:

- `psx-bsp`: 73 passed.
- `psx-game-runtime`: 92 passed.
- `psx-engine`: 318 passed.
- `make runtime-numeric-guard`, `make editor-blank-playtest-check`,
  `make editor-bsp-liquid-check`, `make combat-checkpoint` and
  `make editor-souls-bsp-check` were reported green.

At `ca2d33e9`:

- `make editor-souls-bsp-check` passed after its cadence filter was made
  non-vacuous and required exactly two `tick_authority` tests.
- The gate requires the staged and in-tree guest layouts to reproduce the
  same simulation rather than mislabelling layout-dependent presentation as
  an identical binary.

The souls gate pins 6 attack starts, 4 melee hits, 1 stagger, 1 enemy death,
4 hits taken, 2 weapon attachments, 1 checkpoint activation, 1 door
activation, 6 liquid events, 1 player death and 2910 PVS suppressions. Its
negative tape pins combat and progression to zero while PVS suppression still
accrues. These are real-MIPS-build plus headless-emulator claims, not silicon.

### 5.3 Known PSoXide limitations

- The owner has not manually accepted the final UV behavior in the native
  window after `ec907bff`, `7bf129ee` and `11dfb9f9`.
- Texture lock is a workspace preference, not serialized per brush. It
  preserves translation. Do not claim arbitrary reshape UV preservation.
- UV scale/rotation uses a solved polygon-centroid anchor. Resize, vertex edit
  or clipping moves the centroid, so a later UV interaction may anchor at a
  slightly different point.
- Persistent brush grouping was deliberately deferred.
- No claim exists for arbitrary huge/dense levels or production streaming.
- The native window, display scaling, handle ergonomics, camera feel, audio
  and original hardware remain human acceptance work.

Current user-facing truth lives in [the editor README](../editor/README.md),
[the fresh-project checklist](fresh-project-workflow-checklist.md), and
[the souls-slice checklist](souls-slice-acceptance.md). Some older sections of
the convergence handoff and the editor-playtest README describe gaps that have
since landed; treat them as historical.

## 6. Quake evidence boundary

### 6.1 What is banked

The final Quake branch includes the shared tracer integration, map cook and
runtime, twin-stick controls, collision/movement, PVS, movers, teleports,
target/use dispatch, key locks, damage, weapons, bounded particles/effects,
shareware monsters, Chthon and intermission work accumulated by the campaign.

The final route work also banked these production fixes:

- allocation-free render-BSP point tracing for weapon occlusion instead of
  misusing a render-node index as a clipnode root;
- E1M2's authored ordinary-input lift, one-shot shootable button, bridge,
  silver key/target, locks, floorplate and exit path;
- retention of zero-travel shootable buttons, required by E1M4's 4-unit-thick
  `lip=4` buttons;
- atomic pusher rollback and attribution of a dynamic blocking body, so a
  monster blocking a carried player is damaged rather than fabricating player
  crush damage;
- automatic-platform rider-state corrections;
- targeted `func_plat` behavior that lowers on target, holds at the low stop,
  and returns through its canonical inside trigger;
- E1M8's authored low-gravity override;
- a durable authored-spawn E1M7 route through the Chthon/lightning/
  intermission sequence.

### 6.2 Exact final evidence

At Quake `28507a6dd605730a43909d6b2258f081def68a79`:

- `cargo test -p quake-core` passed: 212 library tests, 12 collision-parity
  tests, 2 train-leg tests, 3 pusher tests, 1 render-trace test and 4
  shareware-activation tests.
- A provenance-grade real-MIPS shipping `disc` build passed against exact
  PSoXide `79d51dd2f2fd78cfb8aa418e2ad123730f56ac3d` with no guest features.
- Shipping guest recipe:
  `3f94b7935843619b24075dbe13514968d7315aa4305afab89e4d2e775c936610`.
- A final Start -> Easy slipgate -> E1M1 ordinary-input headless gate passed:
  120 frames, map mask `0x003`, transition mask `0x001`, mechanism mask
  `0x07`, VRAM FNV-1a `0x4088711e20b18be5`, display FNV-1a
  `0x2cf5c7812004027f`.
- Durable end-to-end per-map route evidence exists for Start, E1M1, E1M2 and
  E1M7.
- E1M4's two zero-travel shootable buttons were separately proven twice on a
  real MIPS guest using ordinary pad movement and R2 shotgun damage. That is a
  mechanism proof, not a full E1M4 route.
- E1M3's targeted platform and shootable-button mechanism received real-guest
  evidence, but its complete route harness was not polished into a gate.

Final standalone Quake artifacts:

| Artifact | SHA-256 |
| --- | --- |
| provenance | `8560401739d0def8ec08cfafee9ec02ba90dfb3d62933946f8d7198401d5506d` |
| cue | `5fa78b12b506d4190246e230183e1eebd677f201ff982a584bff10d88ee2594c` |
| bin | `50a6a9d792e3ef074dd49e300dcb69fb5f2d84a7d2d053c07ae9c8d473f0b1a2` |
| exe | `f0b3e7ff814c5eba320ea78bceb6f38bdec1938f971a52a87213dd774ca6de85` |

The executable is 366,592 bytes. The Quake image is 22,287,552 bytes, 9476
sectors.

### 6.3 What Quake does not yet prove

- No durable complete ordinary-play route for E1M3, E1M4, E1M5, E1M6 or
  E1M8.
- No continuous normal or secret Episode 1 end-to-end gate.
- No owner manual start-to-finish playthrough on emulator or hardware at this
  exact head.
- No guarantee of sustained 30 fps. The owner accepted playable frame pacing
  rather than a fixed 30-fps release requirement.
- Presentation remains incomplete compared with PC Quake: some sprites,
  particles, gibs, dynamic lights, HUD icons and sounds need later polish.
- Quake CD music is absent. The shareware archive contains no music tracks;
  disc music reuse is future work.
- Save/continue is deferred.
- Some lower-priority parity remains, including patrol/infighting details,
  two-stage secret doors, bubbles/explobox behavior, some radius-damage
  interactions and movement/door sounds.
- Original-hardware behavior and performance are unknown until the owner burn.

This is therefore a **playable-beta candidate**, not “Episode 1 complete.” A
manual playthrough is now more valuable than further speculative automation.

## 7. Tomorrow: start building a PSoXide level

### 7.1 Direct editor launch

```sh
cd /Users/ebonura/Desktop/repos/PSoXide/emu
cargo run -p frontend --release -- --editor --windowed
```

To open the tracked souls slice directly:

```sh
cd /Users/ebonura/Desktop/repos/PSoXide/emu
cargo run -p frontend --release -- --editor --windowed \
  --editor-project /Users/ebonura/Desktop/repos/PSoXide/editor/archive/fixtures/souls-bsp-vertical-slice \
  --editor-view 3d
```

`make run-release` from the repo root is also valid, but it opens the general
frontend; the command above enters the editor directly.

### 7.2 Fifteen-minute native acceptance

Before committing a day to a map:

1. Create a new Draft project and verify the roofless neutral courtyard.
2. In Top view, create and select a brush. Move and resize it using the visible
   modes, then undo and redo.
3. Select the same brush in 3D. Confirm the old frame is cleared correctly and
   picking works at the current display scale.
4. Select a face. Change U% and V% independently. Confirm each scales on its
   own axis instead of sliding.
5. Enable **Inspector -> Texture Coordinates -> Lock texture while moving**.
   Drag the brush, release, drag again and verify the texture does not swim.
6. Apply a different material to one face and verify immediate 3D repaint.
7. Save, press Play, then make one brush edit and press Rebuild & Play.

If these pass, stop testing the editor and build the level. If one fails,
record the smallest exact gesture sequence and project; fix that blocker only.

### 7.3 Minimal souls level workflow

1. File -> New Project.
2. Resources `+` -> Starter Characters.
3. Block out a modest roofless or indoor-connected map using convex brushes.
4. Place a player and one Rust Mantis enemy through Place -> Character.
5. Add a Logic entity, set Kind to Door, then select the door brush and set
   its **Model owner**.
6. Add a Trigger Volume whose Target names an Entity carrying
   Interactable -> Checkpoint.
7. Optionally mark a brush as Lava for a readable death/reset loop.
8. Save and Play. Use Rebuild & Play after geometry changes.

For the first Comicon map, favour one arrival space, a short connector/door,
one readable combat arena, a checkpoint and a landmark/boss reveal. The
current content density is one normal enemy plus an eventual larger boss;
doors and triggers are useful, but elaborate scripted encounters are not
required. Keep the map small enough for whole-map residency until measurements
show a real memory problem.

### 7.4 Relevant gates

Run only when needed; they are not a prerequisite for every brush edit:

```sh
cd /Users/ebonura/Desktop/repos/PSoXide
make editor-blank-playtest-check
make editor-souls-bsp-check
make editor-bsp-liquid-check
make combat-checkpoint
```

The normal authoring path is the editor's Play button. The CLI fallback is:

```sh
make cook-playtest PROJECT=projects/<name>/project.ron
make build-editor-playtest
```

`PROJECT` is relative to `editor/` because the make target changes directory.

## 8. Testing Quake now

Controls:

| Input | Action |
| --- | --- |
| Left stick or D-pad | Move |
| Right stick | Look |
| R2 | Fire |
| Triangle / Circle | Next / previous usable weapon |
| Cross | Jump |
| Square | Use |
| Start | Pause |

The most useful next Quake test is a normal human playthrough, not another
waypoint harness. Record map, approximate location, mechanism, input and
whether the problem reproduces after a clean launch. Separate a true gameplay
blocker from imperfect presentation or frame pacing.

Do not assume a failed automated route means a player is blocked: several WIP
routes failed because their steering chose an unsafe drop, wall edge or combat
line while the actual runtime mechanism was correct.

## 9. Final default demo disc

### 9.1 Artifact to burn

Burn this exact cue, not an older `.bak` image in the same directory:

`/Users/ebonura/Downloads/ps1 games/PSoXide Demo Disc/PSoXide Demo Disc.cue`

Current artifacts:

| Artifact | Size / SHA-256 |
| --- | --- |
| `PSoXide Demo Disc.bin` | 226,742,208 bytes; `105de3bb062185fd1fa7bfaec7ec7fda30197255168bb8616228e02cb901d57e` |
| `PSoXide Demo Disc.cue` | `b349a5871697fe49e1a79ef62bd82e5c4167a2eb7a54c363770063fba57b52fb` |
| `PSoXide Demo Disc.quake-provenance.json` | `3decf160d9c442a4245b29380b850b204bc87877fc5a5aa3e1caf4988a6b74c8` |

The default image is 96,404 whole sectors, about 216.2 MiB, with 11 programs,
one CREDITS card and 7 CD-DA tracks. Its 12 visible entries are, in order:
Cortex Ignition, Voxide, NitroXide, Celeste Collection, PSXcel, GH-PSX,
Breakout, Space Invaders, Magikaaaaarp Pong, Hardware Tests, Quake Shareware
and Credits. Half-Life is absent. Cortex is the legacy grid fallback, not the
new BSP slice.

Launcher identity: `v0.19-32-g2061540`.
Quake menu identity: `q28507a6`, program 11/11 and visible entry 11/12.

### 9.2 Exact combined-disc evidence

The definitive `make quake-headless-check` ran from the promoted canonical
repositories after a clean launcher rebuild. Canonical `make disc` and
`make check` also passed at demo-disc `2061540e`.
Both passes:

- used 12 visible menu entries and selected Quake with Right at tick 400,
  Right at tick 600 and Cross at tick 1000;
- exercised the pressed launcher table and loader;
- relocated and loaded the Quake executable at LBA 6601;
- performed later Quake runtime reads, including LBA 7603;
- reached the Quake Start map with no panic, load-error or stack marker;
- produced identical stdout, summaries and all six diagnostic logs.

Pinned/observed evidence:

- 9476 embedded Quake sectors and 178 loader payload sectors;
- final PC `0x800505dc`;
- 633 CD commands and 29,310 PC samples inside the guest payload;
- route ticks 2585, pad polls 949, cycles 1,476,941,551;
- VRAM FNV-1a `0x34b41a57cd8b06ec`;
- display FNV-1a `0xee3d4e8f8e5abc72`;
- 320 by 240 display, no screenshots or frame/audio dumps.

The test uses `--embedded-playtest`: the emulator HLE-boots the first EXE,
then exercises the real pressed launcher table, loader, relocated Quake image
and CD reads. It does **not** prove BIOS boot, physical CD timing or silicon.
It also does not prove every other menu program booted at this final head.

### 9.3 Legacy Cortex fallback evidence

The visible `CORTEX IGNITION` entry was separately selected from the same
combined cue in two headless runs with Cross at tick 400. Both runs:

- exercised the launcher and chain-loader exactly once with no panic, stack,
  or persistent-asset-load failure marker;
- loaded the legacy Cortex image beginning at LBA 1096 and its executable at
  LBA 1118, then performed a later runtime read at LBA 2121;
- finished at PC `0x800cba4c`, inside the 1,662,976-byte payload loaded at
  `0x80010000`;
- stopped at tick 1,000,000,000 after 2,440,841,926 cycles, with 4265 route
  ticks, 3641 pad polls and 130 CD commands;
- recorded 61,036 PC samples in total, including 55,749 inside the Cortex
  payload;
- produced VRAM FNV-1a `0x8977da48e4a91f2c` and display FNV-1a
  `0x6463597953eb61e4` at 320 by 240;
- produced byte-identical route, CD-command and PC-sample logs across both
  runs.

This is deterministic emulator evidence for the legacy fallback only. It is
not original-hardware evidence and it says nothing about the new BSP slice.

An exploratory attempt to bake `souls-bsp-vertical-slice` through the demo
disc's generic `build-project-disc` path failed in both its standalone and
combined images with `PERSISTENT ASSET LOAD FAILED: asset 2 reason 7`. Because
the standalone image also failed, combined-disc relocation was not the cause.
The canonical souls regression uses a different clean-cook/build recipe, but
that difference has not been proven causal. Investigate it later; do not
replace the working legacy fallback in the meantime.

### 9.4 Rebuild and verify commands

```sh
cd /Users/ebonura/Desktop/repos/psx-demo-disc
make disc
make check
make quake-headless-check \
  FRONTEND=/Users/ebonura/Desktop/repos/psx-demo-disc/games/PSoXide/target/release/frontend
```

The default `QUAKE_SRC` is the canonical sibling
`/Users/ebonura/Desktop/repos/quake-psx`; no temporary worktree override is
required.

For full-disc builds, use the demo repository's exact-pinned
`games/PSoXide` submodule. Do **not** pass the detached
`PSoXide-final-pin` worktree as `PSOXIDE`: the current hydration recipe can
self-target that external checkout and erase its files while staging ordinary
programs. That exact clean worktree was restored at `79d51dd2` after this trap
was observed. A future build-script fix should reject source/destination
aliasing before external overrides are considered safe.

Do not run `make quake-repin` against stale ignored Quake artifacts. Rebuild
Quake `dist/` from a clean exact source head first, validate its sidecar, then
measure and repin.

### 9.5 Hardware acceptance

The owner should:

1. boot the disc through the real BIOS;
2. confirm launcher `v0.19-32-g2061540`;
3. launch visible Cortex Ignition twice and verify the legacy grid fallback,
   characters, animation, attachments, movement/camera and stable audio;
4. select Quake Shareware `q28507a6` and launch it twice;
5. start New Game -> Easy and verify movement, look, fire, jump, use and audio;
6. play the normal episode as far as possible, recording the first hard
   blocker rather than a list of polish differences;
7. test E1M4's secret exit to E1M8 if secret-episode completion will be
   claimed;
8. watch for sustained frame-pacing or CD-read stalls;
9. return to the launcher and run the on-disc Hardware Tests entry.

The checked-in hardware handoff is [h1-owner-hardware-handoff.md](h1-owner-hardware-handoff.md).
Verify the recorder/device identity before using any burn command.

Redistribution remains blocked pending separate Quake-shareware legal and
release approval. Local building and testing do not grant publication rights.

## 10. Remaining work, classified

| Area | State | Release effect |
| --- | --- | --- |
| Native PSoXide UV/selection/play smoke | owner not yet run | first task tomorrow; beta acceptance |
| Build a presentable souls level | not started on this freeze | primary product work now |
| Quake manual Episode 1 playthrough | not run at final head | required before “episode complete” claim |
| Quake E1M3/4/5/6/8 durable routes | incomplete | deferred; not a playable-beta blocker |
| Quake full normal/secret continuous gates | incomplete | deferred certification |
| Original PlayStation burn | not run | required for Comicon hardware confidence |
| BSP souls slice generic disc bake | asset 2 checksum failure | deferred; legacy grid Cortex remains the default fallback |
| Stable 30 fps | not guaranteed | owner accepted playable pacing |
| PSoXide microsection streaming | deferred | only revisit for measured large-map need |
| Save/continue | deferred | not part of current demo |
| Persistent editor grouping | deferred | not needed for simple maps |
| Advanced TrenchBroom-class tooling | deferred | current simple-level workflow is enough |
| Quake music and presentation parity | deferred | polish, not current blocker |
| Full registered Quake opt-in | future | explicitly out of scope |

## 11. Preserved route and exploratory WIP

Use stash object hashes, not mutable stash indexes:

| Object | Contents | Action |
| --- | --- | --- |
| `c6273e32eeeba77f058db64fba4dbd30decaf2d5` | optional E1M3 deterministic route WIP after the targeted-plat fix | preserve/defer |
| `28060b555f33ad503f28044a67db464a769a9557` | newer E1M8 route harness after three-button guest proof | preserve/defer |
| `a6b41b8b4789f03125f3ab16ecdda40767faf5fe` | E1M4 normal/secret route harness | preserve/defer |
| `5f81afe86d4f87f42b4954f7f35c9cb755775cc7` | older root E1M8 route WIP | preserve until compared with the newer stash |

E1M3 also has `/tmp/quake-e1m3-route-optional-wip.patch`, currently 31 KiB,
SHA-256 `2015396c4aea63eb59a8ed5a88366f964aec1a606a32ba43bef386b1c98b0fbb`.
`/tmp` is not durable archival storage; the Git stash object is authoritative.

Recovery pattern:

```sh
git worktree add --detach <recovery-dir> '<stash-object>^1'
cd <recovery-dir>
git stash apply <stash-object>
```

Never apply these directly to canonical `codex/all-rust-quake`.

Additional dirty WIP:

- `/Users/ebonura/Desktop/repos/quake-psx-routes-e1m5-e1m7` contains a mixed
  E1M5 route harness, diagnostics and overlapping old production edits. The
  production pusher fixes are already in final. Do not integrate this tree
  wholesale. Its last meaningful route reached waypoint 64 before ordinary
  combat death; E1M6 has topology notes only.
- `/Users/ebonura/Desktop/repos/quake-psx-target-graph-start` is old, dirty and
  far behind final. Preserve for forensic comparison only.
- `/Users/ebonura/Desktop/repos/PSoXide-convergence` has the owner's camera
  edit in `editor/archive/fixtures/brush-first-playable/project.ron`. Preserve it
  separately; it does not belong in this release automatically.

All production fixes from the route workers have already been integrated in
rewritten final commits. Do not cherry-pick `56212ed`, `2ff8c5d`, `528759d`
or `bc5efae` on top of final; their equivalents are present.

## 12. Official repository promotion

### 12.1 Completed canonical heads

The owner-approved local promotion completed without rebasing, rewriting or
copying repositories:

- `/Users/ebonura/Desktop/repos/PSoXide` is on `main` at promotion base
  `b2db85c4b86c3c1ea9748a92d39c85dff8b7610d`; this handoff update is its
  docs-only descendant.
- `/Users/ebonura/Desktop/repos/quake-psx` remains on the established
  `codex/all-rust-quake` branch and was fast-forwarded to
  `28507a6dd605730a43909d6b2258f081def68a79`.
- `/Users/ebonura/Desktop/repos/psx-demo-disc` is on `main` at
  `2061540e234da16fd7a8378b0fc61f5d810ddd68`. This includes the canonical
  `../quake-psx` source default and its documentation normalization.
- The exact shipping engine pin remains the detached clean worktree
  `/Users/ebonura/Desktop/repos/PSoXide-final-pin` at `79d51dd2`.

The `*-comicon-final` worktrees remain linked historical checkpoints, but the
canonical folders above are now the working locations.

### 12.2 Recovery points preserved before promotion

Do not delete these until the owner has accepted the promoted repositories and
physical disc:

- PSoXide backup branch
  `codex/pre-comicon-psoxide-promotion-20260813` points to exact old head
  `5c5656a9c3fcddaf52661abf74f899bd85ea0c5a`.
- PSoXide's eight tracked owner edits are in tracked-only stash object
  `8f2bcb89dc1e336eea90cbd9b259a03d54185fb8`. Do not reapply it wholesale
  onto promoted `main`; recover from the old-base branch or restore selected
  files deliberately.
- PSoXide's 87 pre-existing untracked paths were left in place and were not
  captured, moved or deleted during promotion.
- Quake backup branch `codex/pre-comicon-promotion-quake-20260813` contains
  preservation commit `50ad111d34ae02dca54fe3bb533c3c08debb7815`, which
  banks all 17 pre-promotion tracked and untracked source paths. Do not
  cherry-pick it wholesale onto the promoted port.
- The four route stashes remain unchanged:
  `c6273e32eeeba77f058db64fba4dbd30decaf2d5`,
  `28060b555f33ad503f28044a67db464a769a9557`,
  `a6b41b8b4789f03125f3ab16ecdda40767faf5fe` and
  `5f81afe86d4f87f42b4954f7f35c9cb755775cc7`.
- Demo-disc backup branch `codex/pre-comicon-disc-promotion-20260813` points
  to exact old `main` head `10d6f24b29972d5f0124b8930b0391ed6001ebc5`.

### 12.3 Canonical rebuild and publication state

Canonical Quake was hydrated from exact PSoXide pin `79d51dd2`, its ignored
assets and shipping `dist/` were rebuilt from source, its provenance matched,
and `cargo test -p quake-core` passed. Canonical demo-disc `make disc`,
`make check` and the deterministic two-pass `make quake-headless-check` all
passed. The legacy Cortex chain-load also passed twice with byte-identical
logs and the same telemetry recorded in section 9.3.

The rebuilt default candidate identifies the launcher as
`v0.19-32-g2061540`; its current hashes are recorded in section 9.1. No
repository was pushed. Before any remote demo-disc publication, publish the
required PSoXide and Quake source history first so its source contract is
actually reproducible. Local burning and owner hardware acceptance may happen
before push.

## 13. Challenge checklist for a resumed session

The next model should answer these with live evidence:

- Do all three canonical repositories still descend from the promoted heads,
  and do all recovery refs, stash objects and preserved paths still exist?
- Does the combined receipt still match the exact current bin and cue hashes?
- Does the receipt pin Quake `28507a6` and PSoXide `79d51dd2`?
- Does Quake's own sidecar have empty shipping features and the same hashes?
- Is visible Cortex still the legacy `cortex_v1` fallback rather than the BSP
  slice?
- Is Quake still program 11/11 (visible entry 11/12), with Half-Life absent?
- Can the owner create, select, move, resize and texture a new brush natively?
- Does U% scale U, V% scale V, and texture lock survive release/re-drag?
- Can a new project sync starter characters and reach embedded Play?
- Can the tracked souls slice open a door, fight, die and checkpoint-respawn?
- Can the final Quake build be played manually through every normal map?
- Does E1M4's secret exit reach E1M8 on the final build?
- Are any failures actual engine blockers rather than automated-route steering?
- Does the physical disc boot through a real BIOS and remain stable under CD
  timing, GPU, DMA, SPU and controller behavior?
- Are legal rights in place before any shareware image is distributed?

When a check fails, preserve the exact head, artifact, command, input sequence
and first divergent observation. Do not dilute it with speculative fixes.

## 14. Definition of done from here

For **starting level production tomorrow**, done means:

- the fifteen-minute native editor checklist passes;
- a new PSoXide project can be saved and playtested;
- the owner is comfortable enough with selection, brush tools and texturing to
  block out the Comicon space.

For the **Comicon playable beta**, done means:

- a presentable PSoXide souls level exists and its core loop works;
- the default disc boots on the owner's PlayStation;
- the PSoXide entry and Quake entry both launch and are acceptably playable;
- any known Quake progression blocker discovered by manual play is fixed;
- claims remain limited to what was actually tested.

For **full Quake Episode 1 completion**, done is stricter and intentionally
future work: normal and secret routes start-to-finish, owner hardware
playthrough, acceptable performance, and no progression blocker across every
shareware map.

The campaign should now stop optimising the proof system and start using the
tool. Build the level, burn the candidate, and let real use choose the next
bug.
