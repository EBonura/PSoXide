# F0 baseline freeze: worktree inventory and requirements matrix

Date: 2026-08-11. Owner: convergence lead session executing
`docs/finish-line-plan-2026-08-11.md`. This is the F0 work-package artifact:
the frozen starting inventory, the baseline validation evidence, and the
requirements matrix that binds every completion claim to its owner and gates.

## 1. Worktree inventory at freeze

Active campaign lanes (the only trees the campaign writes to, plus the new
worker trees it creates):

| Lane | Path | Branch | Head | Dirty state |
|---|---|---|---:|---|
| PSoXide integration | `/Users/ebonura/Desktop/repos/PSoXide-convergence` | `codex/quake-psoxide-convergence` | `2af919d0` | exactly one user-owned edit: `editor/archive/fixtures/brush-first-playable/project.ron` (saved orbit camera + final newline; never reset/stage/commit without owner choice) |
| Quake integration | `/Users/ebonura/Desktop/repos/quake-psx-convergence` | `codex/quake-convergence` | `09ff502` | clean |
| Quake E1M1 route donor | `/Users/ebonura/Desktop/repos/quake-psx-target-graph-start` | `codex/quake-target-graph-start` | `d7e521e` | dirty by design: 7 tracked files + `game/src/e1m1_chain_regression.rs` + temporary `examples/` diagnostics. READ-ONLY donor; salvage ports onto a fresh branch (Q1); never merged wholesale |
| Demo disc | `/Users/ebonura/Desktop/repos/psx-demo-disc-quake-shareware` | `codex/quake-shareware-demo-disc` | `ba250ef` | clean; still pinned to pre-convergence revisions until D1 |
| Provisional PSoXide pin | `/Users/ebonura/Desktop/repos/PSoXide-rc1-pin` | detached | `f9f83c35` | clean; the exact `PSOXIDE_REV` Quake `09ff502` declares; used for every Quake gate until P3 |

Owner-owned trees the campaign must never write to:

| Path | Branch | Note |
|---|---|---|
| `/Users/ebonura/Desktop/repos/PSoXide` | `codex/windowed-classic-affine` | owner's live WIP (dirty), read-only |
| `/Users/ebonura/Desktop/repos/quake-psx` | `codex/all-rust-quake` | owner's parallel WIP, read-only |

Abandoned exploratory trees (clean at their starting heads, contain no
implementation, not a source of requirements):

- `/Users/ebonura/Desktop/repos/PSoXide-quake-editor-vertical-slice`
  (`codex/quake-editor-vertical-slice`, `5aff6078`)
- `/Users/ebonura/Desktop/repos/quake-psx-editor-pxbsp-slice`
  (`codex/editor-pxbsp-vertical-slice`, `9d3eaa6`)

Completed evidence/recovery worktrees, all verified clean on 2026-08-11 (do
not delete; `cargo clean` only if disk demands):
PSoXide: actor-weapon-authority `25df5a5d`, brush-editor-workflow `fd81708b`,
bsp-caller-owned-trace `adafa2bf`, bsp-draft-release-play `603126dc`,
bsp-editor-multiview `702cbef2`, bsp-liquid-volumes `9532e39f`,
bsp-playtest-lifecycle `f737fb7c`, bsp-prop-hulls `9441e158`,
bsp-prop-point-traces `388ae659`, bsp-trace-provider-runtime `0958fb4e`,
cortex-weapon-content `61deb273`, dynamic-bsp-blockers `937270d8`,
editor-brush-manipulation `8fc27f28`, editor-exact-blank-playtest `f752425a`,
editor-independent-challenge `ccf80940`, editor-level-c-tools `8d7252eb`,
editor-level-c1-hardening `d61a195a`, editor-playtest-workflow `645b3f9e`,
editor-simple-level-today `bbe07790`, instance-pose-authority `1294cc6b`,
reciprocal-pose-combat `e9c0db23`, plus unrelated owner worktrees
(fidelity-pass-4, hitbox-cleanup, hlpsx-audio, quake-bsp, queued-gp1,
scoped-affine, stress, ui-aesthetic, weapon-attach) untouched.
Quake: build-provenance `2d26f9e`, combat-adversarial-review `1fd5173`,
episode1-next-gate `396b861`, items-weapons `ca88979`, monster-physics
`9d3eaa6`, monster-runtime `69b99d6`, policy-checkpoints `6edb8cd`,
shared-bsp-trace `8cef817`, trace-parity `5bf1503`.
Detached `/tmp` audit trees (`psoxide-afc-audit`, `psoxide-gpu-opcodes`,
`PSoXide-xbsp-parity-old`, `quake-determinism` old/new, `quake-path-proof`,
`quake-psx-golden-83a`) are disposable evidence remnants.

## 2. Baseline validation at the frozen heads (2026-08-11)

PSoXide `2af919d0` (all green, one run each):

```text
psxed-project --lib: 489 passed, 1 ignored
psxed-ui --lib:      380 passed, 1 ignored
psx-bsp --lib:        70 passed
psx-engine --lib render: 111 passed, 207 filtered
psx-game-runtime --lib:  85 passed
make editor-blank-playtest-check: PASS
  2 x 119 route ticks, 121/60 guest/visual frames, XZ (192,192)->(192,80)
  vram 0xdb1a46181b00783a display 0xac182dca36383a5d, 0 image artifacts
  PXBSP 8140 B sha256 1d1d05e6...; EXE 985088 B sha256 3fc41688...
make editor-bsp-liquid-check: PASS
make combat-checkpoint: PASS
  melee 4, stagger 1, death 1, taken 3, door swings blocked,
  vram 0x007fb6683f98d82b
```

Quake `09ff502` hydrated from the rc1 pin (all green):

```text
check --psoxide PSoXide-rc1-pin: PASS
  hydrated local PSoXide-rc1-pin at f9f83c35 (clean), 1913 files
  shareware PAK cached at .quakepsx/cache/shareware/ID1/PAK0.PAK
host tests: root 28; quake-cook/core/formats: pass (unit tests live in root
  harness; see VALIDATION.md for per-crate counts at the pin)
compile (real mipsel-sony-psx executable): PASS
  guest recipe 42ec0910...
```

## 3. Requirements matrix

Milestone labels are used only after their exit gates pass
(`docs/finish-line-plan-2026-08-11.md` section 10). Evidence levels: L1
source/host tests, L2 real MIPS build, L3 two deterministic image-free
replays from exact-source frontend, L4 original PlayStation.

| Claim | Owner package | Source paths | Host test | MIPS gate | Replay gate | Hardware gate |
|---|---|---|---|---|---|---|
| Shared BSP traversal/PVS/contents/mover/collision is single-authority | S1 | `engine/crates/psx-bsp`, adapters in `quake-core`/`psx-game-runtime` | `psx-bsp --lib`, quake parity tests | n/a | n/a | n/a |
| Editor authors a fresh souls-like BSP level end to end | P1 | `editor/crates/*`, `engine/examples/editor-playtest`, `editor/archive/fixtures/souls-bsp-vertical-slice` | `psxed-project`, `psxed-ui` | `make editor-souls-bsp-check` (builds real MIPS) | same gate, two replays, pinned counters | H1 battery |
| Editor workflow is production-usable | P2 | `editor/crates/psxed-ui` | real-egui suites | blank-playtest gate | blank-playtest gate | owner native test |
| BSP projects cannot fall back to grid authority | P3 | `editor/crates/psxed-project`, `engine/crates/psx-game-runtime` | discriminator tests | full matrix | full matrix | n/a |
| Quake E1M1 authored target route completes | Q1 | `game/src/e1m1_chain_regression.rs`, `crates/quake-core` | mover/targets tests | `e1m1-chain-regress` | same, twice | H1 |
| Quake common systems (movement/bodies/targets/items/pools) | Q2 | `crates/quake-core`, `game/src` | per-system tests | per-system regress | authored-map probes | H1 |
| Every E1 monster classname behaves | Q3 | `game/src/entity.rs`, `crates/quake-core` | per-monster tests | `monster-regress` + batch gates | authored-map replays | H1 |
| Episode 1 completable in one session | Q4 | whole Quake tree | all suites | `episode1-regress` | two deterministic runs | H1 |
| Exact-pin reproducibility | I1 | both trees + `psoxide-link` | all suites | two-path byte-identical artifacts | pinned hashes | n/a |
| Combined demo disc chain-loads both games | D1 | `psx-demo-disc-quake-shareware` | `make check` | `make quake-verify` | `make quake-headless-check` + PSoXide chain-load gate | H1 |
| Original hardware accepted | H1 | hardware-test battery | n/a | n/a | n/a | owner burn + battery + CRT |
| Docs reproducible by a stranger | R1 | `docs/*`, READMEs, VALIDATION.md | n/a | n/a | n/a | n/a |

## 4. Architecture statement (binding for every worker)

The PSoXide Editor is the authoring tool for the owner's souls-like game
(PXBSP + PSoXide gameplay records). Quake-PSX is a separate game with its own
BSP29-to-PSB/WORLD.PAK cooker and runtime. Both consume the same canonical
`psx-bsp` mechanism. There is no Quake editor, no Quake-to-PXBSP importer,
and no shared gameplay schema; TrenchBroom is an interaction reference only.
Any plan or diff that violates this is rejected in review.

## 5. F0 exit-gate status

- No unknown worktree; every dirty file is owned and listed above: PASS.
- No active plan asks for a Quake editor/importer (the two exploratory trees
  are marked abandoned here and in the finish-line plan): PASS.
- Baseline revisions and limitations agree across the handoff (section 0.19),
  the finish-line plan (section 3), and this document: PASS.
- Grid-era documents marked historical and active authoring documents carry
  the souls-like editor statement: done in the same commit as this file.
