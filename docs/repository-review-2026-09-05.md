# Repository presentation review, 5 September 2026

Reviewed source: main `97a604f6`. This is a repository structure, documentation,
packaging and dependency review, not a full gameplay or security audit.

## Changes made

- Root navigation identifies Cortex 0.4b and routes readers to the SDK,
  editor, architecture, contribution and downstream build guides.
- Engine and SDK crate tables match their manifests. The engine overview no
  longer claims MIPS-only builds or universally allocation-free execution.
- The downstream guide describes Cargo-pinned hydration and explicit local
  overrides. Its older committed-release procedure is labelled historical.
- Tools documentation lists the three actual workspace tools and removes the
  deleted `psx-exe-pack` link.
- Dated acceptance reports are identified as historical evidence. The asset
  provenance document no longer claims complete coverage of newer assets.
- The native release recipe stages the existing New Project courtyard instead
  of deleted `editor/samples/cortex_v1`, includes LICENSE, and explains that
  game builds still require the source/toolchain. This does not establish a
  fully standalone editor distribution.
- The pinned formatter was applied to 20 Rust files. Main's CI run
  [33960665886](https://github.com/EBonura/PSoXide/actions/runs/33960665886)
  failed at formatting before build/tests/lint. Its dependency-policy job
  passed. Formatting changes contain no intended behavioral changes.

- Fixed the optional level-zero renderer's mixed `u32`/`u16` maximum,
  removed two unused depth-key calculations without changing packet ordering,
  and resolved host/test-only unused-variable and const-mutation warnings.

## Findings requiring a separate change

| Priority | Finding | Next step |
| --- | --- | --- |
| High | SDK hydration assumes the whole source-tree layout and copies much more than the SDK. | Define a standalone SDK package and prove it with a downstream game. |
| High | Native packaging is not an independent game-development install: editor fixtures and guest build paths still assume source/toolchain availability. | Add a source-free installation smoke test and explicit SDK/runtime discovery before advertising a standalone editor. |
| Medium | Several Cortex versions and large review/source assets are tracked together. | Keep Cortex with the editor; separate that bundle from SDK/emulator consumers and archive historical evidence separately. |
| Medium | The central asset provenance record is dated June and does not inventory later Cortex assets. | Reconcile project-specific source/attribution notes with a current asset inventory before packaging those assets for a release. |
| High | All-feature engine Clippy still fails on six unused experimental renderer helpers and three style lints. | Reconcile experimental feature precedence and test coverage, then run the full CI pipeline; do not suppress the gate or claim the repository is fully green. |

The SDK separation recommendation and dependency evidence are in
[sdk-separation.md](sdk-separation.md). Most downstream games already have
separate repositories; Cortex assets are the main game content inside this one.

## Inventory and limits

`git ls-tree -r -l HEAD` reports 3,326 tracked paths and 689,684,978 bytes of
file content at the reviewed revision. The editor subtree contributes
555,394,561 bytes, assets 87,808,793 bytes, and SDK 5,172,256 bytes. These sums
count repeated content at each path and do not measure compressed Git history.

Cargo manifests and `psoxide-link` source were checked directly. The existing
architecture graph was useful for orientation but predates this revision;
current source takes precedence. Navigation links were checked against tracked
paths, including paths excluded from the review's sparse worktree. Historical
reports may intentionally reference removed code; those reports were not
silently rewritten to imply current validation.

## Validation

- Formatting checks pass for all three workspaces; the source mfc0/mfc2
  delay-slot checker passes.
- SDK math: 21 unit tests pass. Optional level-zero affine renderer: 30 tests
  pass. Gameplay runtime: 206 unit tests and 5 integration tests pass.
  The all-feature engine workspace compiles, with the six experimental
  unused-function warnings reported above.
- The native sample staging was exercised in a temporary directory: all six
  files match the source, referenced assets exist, README and LICENSE are
  included. A universal macOS binary was not rebuilt in this pass.
- Current navigation links resolve against tracked paths. One historical July
  report intentionally still links to removed grid-cooker source.
- A local full default engine test attempt reached 575 passing tests but
  stopped at a grounding diagnostic because generated Aletha model fixtures
  were absent in the isolated checkout. It does not establish a full-suite
  pass; the documented full workflow cooks fixtures first.
- All-feature Clippy remains failing as described above. The remote branch CI
  is the authority for the full clean-checkout build and fixture workflow.

No new console test, full native release, SDK extraction or history rewrite
is implied.
