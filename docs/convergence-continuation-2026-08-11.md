# Convergence continuation packet (2026-08-11, session end)

Read this with `docs/finish-line-plan-2026-08-11.md` (the authoritative
plan), `docs/quake-psoxide-convergence-handoff.md` sections 0.19-0.20.2 (the
evidence history), `docs/convergence-f0-baseline-2026-08-11.md` (the frozen
baseline and requirements matrix), and
`docs/convergence-discrepancy-report-2026-08-11.md`.

This session executed F0, S1, P1 (both halves), and Q1 to green gates, then
stopped mid-flight in P2 and Q2 when three parallel workers hit a model
usage limit. Nothing is left in a broken state: every integration branch is
committed and gated; the interrupted work sits in its own worktrees.

**Challenge this document rather than inheriting it.** Every number below
came from a run in this session, but a successor should re-derive the ones
it depends on. The falsification targets in the plan's section 8 still
apply, and two new ones are added at the end of this file.

## 1. Exact heads

| Lane | Worktree | Branch | Head | State |
|---|---|---|---|---|
| PSoXide integration | `/Users/ebonura/Desktop/repos/PSoXide-convergence` | `codex/quake-psoxide-convergence` | `33173d85` | one user-owned dirty file: `editor/projects/brush-first-playable/project.ron` (saved orbit camera, missing final newline). PRESERVE. |
| Quake integration | `/Users/ebonura/Desktop/repos/quake-psx-convergence` | `codex/quake-convergence` | `0eca976` | clean |
| Demo disc | `/Users/ebonura/Desktop/repos/psx-demo-disc-quake-shareware` | `codex/quake-shareware-demo-disc` | `ba250ef` | clean, still on the OLD pins; do not repin until P3 and Q4 are final |
| Quake source pin | `/Users/ebonura/Desktop/repos/PSoXide-rc1-pin` | detached | `f9f83c35` | clean; every Quake command needs `--psoxide /Users/ebonura/Desktop/repos/PSoXide-rc1-pin` |

PSoXide integration history this session (oldest first): `cd5b0f0d` F0
freeze, `e0baf170` shared PVS decompressor (S1), `072da1c7` merge P1a
runtime+telemetry, `342e7ee0` handoff, `449f1a1e` merge P1b slice+gate,
`add9e5b1` gate frontend fix + VRAM re-pin + handoff, `33173d85` merge P2
test coverage.

Operational note for successors: git stashes are REPOSITORY-wide, not
worktree-local. A `git stash push` in one worktree and a `git stash pop` in
another moves the work between trees. This happened once here and was
undone by patch; prefer a temporary commit or an explicit patch file when
parking work in a multi-worktree repo.

Quake integration history: `d0e75c8` COVERAGE.md, `0eca976` merge of the
nine-commit E1M1 route salvage (`bd7ca5d..cedaa8a`).

## 2. Interrupted worker worktrees (resume or discard deliberately)

| Worktree | Branch | Head | Dirty | Status |
|---|---|---|---|---|
| `/Users/ebonura/Desktop/repos/PSoXide-p2-hardening` | `codex/editor-p2-hardening` | `10170123` | `editor/crates/psxed-ui/src/tests/brush_tools.rs` (+302 lines, uncommitted) | P2 item 6 landed as commit `10170123` and was reviewed and MERGED into the integration tree as `33173d85` (psxed-ui 382 to 386 green with it, verified with the dirty batch stashed out). The remaining dirty file is the next batch, mid-write, and is NOT merged. |
| `/Users/ebonura/Desktop/repos/quake-psx-q2a-entities` | `codex/quake-q2a-entities` | `0eca976` (no commits) | `crates/quake-core/src/{mover,targets}.rs`, `game/src/entity.rs` (+74/-15) | Q2a had just begun generic target dispatch. Small enough to review or discard and restart; do not assume it compiles. |
| `/Users/ebonura/Desktop/repos/quake-psx-q2b-player` | `codex/quake-q2b-player` | `0eca976` (no commits) | clean | Q2b never got past reading code and cache setup. Restart from the mission brief in section 5. |

Worker branches created this session and already merged: `codex/souls-slice-runtime`
(`c4f9e098`), `codex/souls-slice-project` (`b698b7dc`), `codex/quake-e1m1-route`
(`cedaa8a`). Their worktrees (`PSoXide-souls-runtime`, `PSoXide-souls-slice`,
`quake-psx-e1m1-route`) are clean and retained as evidence.

## 3. What is proven, and at which evidence level

Levels per the plan: L1 host tests, L2 real MIPS build, L3 two deterministic
image-free replays from an exact-source frontend, L4 original hardware.
**No L4 evidence exists. Nothing here is a hardware claim.**

PSoXide at `add9e5b1`:

```text
L1  psxed-project 491, psxed-ui 382, psx-bsp 71, psx-engine render 111,
    psx-game-runtime 90, emulator-core 537   (all green)
L3  make combat-checkpoint  PASS  melee 4, stagger 1, death 1, taken 3,
                                  door tape 3 attempts / 0 connections,
                                  vram 0x007fb6683f98d82b
L3  make editor-blank-playtest-check  PASS  vram 0xdb1a46181b00783a,
                                  display 0xac182dca36383a5d, 0 images
L3  make editor-bsp-liquid-check  PASS  12 lava events, respawn at f181
L3  make editor-souls-bsp-check  PASS  (the P1 milestone gate)
      hits 4, stagger 1, enemy death 1, taken 4, attack starts 6,
      duplicate rejections 44, checkpoint 1, door 1, lava 6,
      player deaths 1, weapon attachments 2, pvs suppressions 2911,
      post-respawn gauge 1003862/1001536,
      vram 0xc41710bde0e93e15, display 0x3c7a6bd9154f23de
      negative tape: every gameplay counter 0, suppressions 812
```

Quake at `0eca976`:

```text
L1  builder 32, quake-core 61 + 12 parity, quake-cook 16
L2  real mipsel-sony-psx build PASS
L3  e1m1-chain-regress PASS  55/55 waypoints, mechanisms 0x1fff,
      532 frames, target edges 16, vram 0x6b0c7f7099ac36fb,
      display 0xdd46a732b521efe1  (identical across 4 runs incl. the
      integration-tree rerun)
L3  start-route-regress PASS (first time on this base)
L3  map/combat/arsenal/monster/audio/ambient/disc all PASS
```

## 4. Two findings a successor must not lose

**The MIPS guest is not checkout-path-reproducible.** Byte-identical
sources plus byte-identical cooked `generated/` produced two same-size
`editor-playtest.exe` files differing in 312,194 bytes across two
worktrees (no embedded path strings; cargo derives per-crate metadata from
the workspace path, reordering codegen). Gameplay counters and the display
hash matched exactly; only the VRAM hash differed, because the slice's
streaming leaves loading-transient pixels outside the gameplay draw window.
Consequences: the souls-gate VRAM pin is canonical per checkout (documented
in `tools/editor_souls_bsp_check.sh`), and **I1 must add a canonical guest
staging build** (Quake already does this: see its content-addressed
`/private/tmp/quake-psx-guest-v1` staging) before any cross-checkout
artifact comparison is meaningful for PSoXide.

**Replay gates must rebuild the frontend.** Both shell gates only built it
when the binary was missing. A stale frontend reads newer telemetry ids as
`unknown`, which produced a false red here and could produce a false green
elsewhere. Fixed in `add9e5b1`; keep it unconditional in any new gate.

## 5. Remaining work, in dependency order

### P2, editor production hardening (in progress)

Verified-green already (do not redo): brush creation in all four views,
face/edge/vertex/arbitrary-plane manipulation, multi-selection with
transforms/duplicate/delete, snapping and numeric fallbacks, texture lock,
selection sync with always-visible Move/Resize/Edge/Vertex, one-drag undo
transactions with redo, per-project view/camera/snap persistence, BSP
contents with mixed selections, searchable record pickers, Play/Rebuild
freshness, external watch with conflict latch, roofless courtyard default,
image-free gates.

Remaining (the audit's ranked gap list):

1. Face-texturing workflow: implemented but untested; add real-egui tests
   for "Apply to face" and the face UV controls, prove one undo per edit,
   then extend the Material Paint tool to paint BSP faces in 3D.
2. Grouping: no persistent group concept exists (only transient
   multi-selection). Flat named groups, persisted, one undo per operation.
3. Error focus coverage: capacity overflows, Pack/Collision/Light/Pxbsp
   failures and the 32 plain `error()` sites carry no focus target; only
   the first error of a report gets one.
4. Repeated conflict/reload/save cycles (single cycle is proven).
5. Slice findings from P1b, each verified and reported by that worker:
   the BSP place lane parks trigger anchors at surface+1 so a freshly
   placed TriggerVolume can never fire without a manual edit; TriggerVolume
   default `wait_ticks: 0` refires every tick and soft-locks overlays
   (default should be fire-once); starter-sync tests may leak
   `starter_combat_sync_*` dirs into `editor/projects/`.
6. Item 6 of the P2 brief already landed at `10170123`; its follow-on batch
   is the uncommitted `brush_tools.rs` diff.
7. Owner acceptance: `docs/souls-slice-acceptance.md` is written and waiting.
   Automated agents may not claim native-window usability.

### P3, grid boundary and final pin (not started)

Project-format discriminator, classify every grid path, prove old grid
projects still load, fail closed if a BSP project instantiates grid spatial
state, full matrix, one clean pin. Also triage here: `make
runtime-numeric-guard` fails at baseline with **26 pre-existing violations**
(11 in psx-gte `scene.rs`, 6 in `bsp_runtime.rs`), none introduced this
session.

### Q2, common Quake systems (in progress, both halves un-landed)

`COVERAGE.md` in the Quake repo is the authoritative census. Entity half:
generic target dispatch beyond movers; teleporters (54 easy instances,
every map, the single largest gap); trains and path corners (12 + 174, and
trains currently cook as static solid brushes that silently block routes in
e1m2-e1m6); key doors (they never consult the inventory today); centerprint
messages; secret counters; real skill filtering; spikeshooters and
fireballs; episode state (sigil, episode/boss gates, `info_player_start2`).
Player half: lava/slime damage, fall damage, drowning, death and
fire-to-respawn with the original loadout, the four powerups, megahealth
rot-down.

### Q3, monsters (not started)

Scope correction that must survive: shareware Episode 1 authors **zero**
Enforcer, Fish, Hell Knight, Shalrath, and Tarbaby (registered content).
Required, with easy-mode counts: Ogre 40, Zombie 43 (renders but is
deliberately not damageable), Knight 24, Wizard 14, Shambler 14, Demon 4,
Chthon 1 (entirely absent: no model, no hitbox, no lava balls, no
`event_lightning` kill). Dynamic body blocking is a prerequisite: monsters
and the player currently pass through each other at all three compose
sites.

### Q4, episode completion and presentation (not started)

Per-map routes for Start and E1M1-E1M8, the normal and secret routes (the
secret exit is **E1M4 to E1M8**, and E1M8 returns to E1M5; E1M7 returns to
Start), Chthon, intermission, the episode ending, plus sprites, particles,
explosions, flashes, light styles, and the `episode1-regress` gate.

### I1, D1, H1, R1 (not started)

I1 must include the canonical guest staging work from section 4. D1 repins
only after P3 and Q4. **H1 needs the owner: a burn and a console.** R1
closes documentation and asks the owner separately about pushing and about
Quake data distribution.

## 6. Rules that produced clean results here

Run every Quake command with `--psoxide /Users/ebonura/Desktop/repos/PSoXide-rc1-pin`.
Give each worker its own worktree and branch; review the diff and rerun the
gates from the integration tree before merging with `--no-ff`. Never merge
a dirty donor tree wholesale. Keep guest hot paths i32/u32, allocation-free,
recursion-free. Derive route waypoints from authored map data, never from
hard-coded coordinates. When a pinned hash shifts, prove the cause before
re-pinning and put the evidence in the commit message; never weaken a
behavioral counter. Watch disk: the Quake guest stage cache
`/private/tmp/quake-psx-guest-v1` grows one staged tree per module edit and
filled the volume twice this session.

## 7. New falsification targets for the next session

Added to the plan's section 8 list:

- **Does the souls slice still prove itself if the tracked project is
  regenerated on a different machine?** The VRAM pin is checkout-canonical
  today; confirm the counters and display hash really are portable by
  running the gate from a third checkout before I1 relies on it.
- **Do the new gameplay counters count what their names claim?**
  `PLAYER_CHECKPOINT_ACTIVATIONS` and `GAME_ENTITY_PVS_SUPPRESSIONS` are
  now exercised by the slice, but `PLAYER_WEAPON_ATTACHMENTS` is inferred
  from equipment draw counts rather than from socket resolution itself.
  Try to make it lie: unequip mid-life, kill the player during a swing,
  respawn with a broken socket reference.
