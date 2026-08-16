# Convergence merge train and worktree disposition

Date: 2026-08-11. This is the binding integration procedure for the final
three-repository merge, recorded here so any successor executes the same
order and the same safety checks. It supplements
`docs/finish-line-plan-2026-08-11.md`.

## 1. Funnels and destinations

Reviewed branches flow through three integration branches into three repo
mains. Nothing is copied by hand between worktrees; everything moves by
merge.

| Repository | Integration worktree | Integration branch | Final destination |
|---|---|---|---|
| PSoXide | `PSoXide-convergence` | `codex/quake-psoxide-convergence` | `PSoXide/main` |
| Quake-PSX | `quake-psx-convergence` | `codex/quake-convergence` | `quake-psx/main` |
| Demo disc | `psx-demo-disc-quake-shareware` | `codex/quake-shareware-demo-disc` | `psx-demo-disc/main` |

The PSoXide integration worktree carries exactly one dirty file, the owner's
saved camera state in `editor/archive/fixtures/brush-first-playable/project.ron`.
Never stage, reset, format, or overwrite it.

The canonical PSoXide checkout (`/Users/ebonura/Desktop/repos/PSoXide` on
`codex/windowed-classic-affine`) and the canonical Quake checkout
(`/Users/ebonura/Desktop/repos/quake-psx` on `codex/all-rust-quake`) both
hold unrelated owner WIP. They are read-only for this campaign. No final
merge happens in a dirty checkout.

## 2. Order

1. Finish and review the feeder lanes (P2, P3 for PSoXide; Q2a, Q2b, Q3, Q4
   for Quake) in their own worktrees. Merge only gated commits, `--no-ff`.
2. Merge `PSoXide/main` INTO the convergence branch (no rebase), resolving
   the main-side difference. As of this snapshot that difference is six
   commits touching only `emu/` frontend code plus `tools/verify-web-dist.py`
   (`5c5656a9`, `fac785f7`, `ffcd1b96`, `6010b0fa`, `3b408158`, `b85c7ac7`);
   none touch `engine/`, `editor/`, or `docs/`, so no conflict with the
   camera edit is expected. Two of them touch `emu/crates/emulator-core/src/
   {input_tape,pad}.rs` and frontend input routing, so the replay gates must
   be rerun after the merge: a frontend input change is exactly the kind of
   thing that can move a deterministic replay.
3. Run the complete PSoXide matrix: host suites, real MIPS build, every
   image-free replay gate, canonical guest staging, and the adversarial
   malformed/capacity cases.
4. Merge the convergence branch into `PSoXide/main` from a clean worktree.
5. Update Quake's declared `PSOXIDE_REV` and dependency to that exact final
   PSoXide commit, and re-hydrate.
6. Merge the reviewed Quake feeder branches into the Quake convergence
   branch. Never re-merge evidence-only or superseded branches.
7. Run the complete Quake matrix (host, cook, MIPS, every route gate, audio,
   ambient, disc, two-checkout reproducibility), then merge into
   `quake-psx/main`.
8. Build Quake's deterministic shipping artifacts and provenance sidecar
   from those final revisions.
9. Repin the demo-disc branch to the exact final PSoXide and Quake
   revisions and regenerate every hash and the receipt. The pins to update
   live in the demo-disc `Makefile`: `QUAKE_SRC`,
   `QUAKE_EXPECTED_REV`, `QUAKE_EXPECTED_PSOXIDE_REV`,
   `QUAKE_EXPECTED_PROVENANCE_SHA256`, `QUAKE_EXPECTED_CUE_SHA256`,
   `QUAKE_EXPECTED_BIN_SHA256`, `QUAKE_EXPECTED_EXE_SHA256`.
10. Run `make check` and `make quake-headless-check`, proving two
    deterministic launcher to loader to Quake chain-loads without images.
11. Merge the opt-in demo-disc integration into `psx-demo-disc/main`. Quake
    stays OFF by default (`make disc` unchanged; `make quake-disc` is the
    opt-in). Web and itch release paths remain blocked pending the owner's
    legal decision.
12. H1 stays an owner action. `docs/h1-owner-hardware-handoff.md` supplies
    the burn command, the console checklist, and the expected evidence. No
    hardware success may be claimed without it.

## 3. Safety checks before every merge

- `git merge-base --is-ancestor <branch> <integration>` to detect work that
  is already contained.
- `git cherry <integration> <branch>` to detect patch-equivalent work that
  would otherwise be merged twice (`PSoXide-bsp-prop-hulls` at `9441e158`
  is exactly this case: patch-equivalent, do not merge).
- Never merge a dirty donor tree wholesale. Port reviewed work onto a fresh
  branch from the current integration head instead.
- Inspect `git status` in the integration worktree before and after every
  merge, and confirm the camera edit is still the only dirty entry.

## 4. Worktree disposition

Evidence-only (committed HEAD already contained in the relevant integration
branch): the completed PSoXide feeder worktrees, the completed Quake feeder
worktrees, and the `/private/tmp` audit trees. Retain while useful; they are
recovery points, not merge sources. Remove only after the final
three-repository merge, and only with explicit authorization.

Review-only divergent branches, never merged wholesale:
`quake-psx-policy-checkpoints` (two unique commits for host-testable menu
and movement policy: cherry-pick individually only if still applicable) and
`quake-psx-target-graph-start` (the old E1M1 donor, superseded by the Q1
salvage; recover individual tests or ideas only after comparison).

Separate campaigns that must never be bulk-merged into convergence:
`PSoXide-fidelity-pass-4`, `PSoXide-hlpsx-audio`, `PSoXide-queued-gp1`,
`PSoXide-stress`, `PSoXide-ui-aesthetic`, and the Claude worktree under
`PSoXide/.claude/worktrees/`.

Preserve before any cleanup: unique untracked documentation (for example
`docs/weapon-attachment-handoff.md` in `PSoXide-weapon-attach`), the dirty
emulator files in the `/private/tmp/psoxide-gpu-opcodes` tree, and the
`Cargo.lock` delta in `PSoXide-quake-bsp` until the final lock audit.

## 5. Completion artifacts

The campaign ends with a cleanup manifest listing removed worktrees,
retained dirty worktrees, branches safe to delete, the final three
repository heads, the pins, the gate results, and the artifact hashes,
followed by an independent falsification pass that challenges merge
completeness, BSP authority, reproducibility, route coverage, demo-disc
payload identity, and hardware status before anything is declared done.
