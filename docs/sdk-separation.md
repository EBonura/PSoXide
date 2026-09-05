# Proposal: separate SDK distribution from the development tools

Status: proposal, based on main `97a604f6` on 5 September 2026. No repositories
or package paths have been moved by this review.

## Recommendation

Keep **Cortex and the editor together**. Cortex is the reference game that
exercises authoring, cooking, runtime and playtesting, so changes to those
systems should still land in one commit. Establish the build boundaries
before moving Git history, starting with the SDK.

| Proposed repository | Contents |
| --- | --- |
| SDK | Bare-metal crates, linker/runtime, shared hardware and cooked-format contracts, small examples, standalone bootstrap and disc tools |
| Emulator | Emulator core, standalone frontend, renderer, debugging and profiling; consumes shared SDK contracts |
| Editor + engine + Cortex | Authoring UI, cookers, engine/gameplay runtime, Cortex and its assets, New Project template and integration fixtures; pins SDK and emulator |
| Demo disc | Existing integration/release repository: launcher, loader, packer, component/game locks and complete-disc validation |
| Existing game repositories | Remain separate; consume the SDK plus engine/cookers/emulator components they actually use |

These names describe ownership, not repository creation instructions. A
separate repository for every crate would create unnecessary versioning work.

The [disc-wide dependency audit](demo-disc-dependencies.md) covers every
game, the hardware suite and the demo build itself. The engine and host
cookers must be distributable from the editor repository without bundling
Cortex assets into every downstream SDK cache.

## Why this helps

An SDK user should be able to build a triangle or a small game without fetching
Cortex audio, editor experiments or an emulator GUI. SDK releases can then
state their target/toolchain support and tested downstream versions clearly.
The tools can continue shipping as one convenient application even when their
source dependencies live elsewhere.

At the reviewed revision, the tracked tree contains 689,684,978 bytes across
3,326 paths. `editor/` accounts for 555,394,561 bytes; `sdk/` is 5,172,256 bytes.
These are sums of file sizes, not unique Git blob sizes, compressed clone size
or installed size. Identical assets repeated in project versions count again.
Moving files does not remove them from existing Git history.

## Dependencies to resolve first

1. Move `psxed-format` into a neutral shared package without changing the
   binary format. SDK `psx-asset` must not require a path into an editor repo.
2. Package `psx-hw` and the shared GTE, trace and disc contracts with explicit
   ownership. Preserve one implementation and shared conformance tests.
   Keep `psx-pack` tests against `psx-iso` available after extraction.
3. Replace `psoxide-link` assumptions about `<root>/tools/psoxide-link` and
   `<root>/sdk/psoxide.ld` with a documented SDK package layout. Support the
   old layout during downstream migration. Copying the whole tree is not a
   long-term SDK installer.
4. Make the MIPS compiler, linker, host packer and minimal examples work from
   a clean SDK-only checkout. Engine-dependent games additionally need a
   pinned engine/runtime, not a hidden dependency on the tools' working tree.
5. Give editor Play an explicit tools/runtime location and compatibility
   check. Keep Cortex and the New Project template with the editor. Expose a
   pinned emulator-core integration for the embedded Play viewport and keep
   the standalone emulator frontend free of editor/game dependencies.

Cargo already supports explicit Git revisions and records them in lockfiles.
That is sufficient for the first extraction; publishing every crate to a
registry immediately is not necessary. Workspace-inherited metadata must be
rewired when packages leave their current root. See the official
[dependency reference](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#specifying-dependencies-from-git-repositories)
and [workspace reference](https://doc.rust-lang.org/cargo/reference/workspaces.html).

## Migration sequence and acceptance

1. Define the SDK package manifest and move shared contracts inside the
   existing repository. Preserve the current consumer paths through a
   transition where needed. Keep this separate from gameplay changes.
2. Add an SDK-only CI job that builds a minimal PS1 disc and tests the shared
   hardware/format contracts without `editor/projects` or the GUI dependencies.
3. Extract the SDK with its license, provenance, tests and reproducible build
   instructions. Pin one small downstream game to it first.
4. Verify that game and Cortex, Quake and Half-Life against the old build:
   output sections/RAM budgets, symbol/hazard checks, deterministic gameplay
   replay, audio offsets and performance. Investigate differences instead of
   assuming a file move is harmless to a bare-metal build.
5. Migrate every consumer in the [demo-disc matrix](demo-disc-dependencies.md),
   including game cookers and emulator-linked tools. Extend disc provenance
   and test both editions, every game entry and relocated CDDA mapping. Record
   SDK, emulator and editor/runtime revisions. Retain Cortex with the editor;
   archive historical captures and duplicate project versions separately when
   their regression value and asset provenance are established.

The emulator extraction is the second step: its current frontend defaults to
the editor feature and also has unconditional engine/format dependencies.
Separate its application shell from the editor's embedded viewport before
moving it, while preserving shared emulation code and conformance tests.
The first useful deliverable is an independently usable SDK, not a set of
repositories that still require the original layout to build.
