# PSoXide License And Provenance Audit

Audit date: 2026-04-29 (last revised 2026-06-03).

Scope: repository source, checked-in binary assets, generated artifacts,
third-party references, and public README media.

Publication decision: PSoXide is published as all three:

- PS1 emulator and debugger,
- PS1 SDK/runtime engine,
- editor and game prototype.

That is the right identity for the project, but it means the public
release bar is higher than for a single-purpose emulator or SDK. Code,
assets, docs, canaries, and generated outputs all need a coherent story.

## Current Position

PSoXide is licensed under **GPL-2.0-or-later**. The full license text
is at [`LICENSE`](../LICENSE) at the repo root, alongside the project
notice, third-party references, and credit.

The choice is deliberate. Several emulator-core subsystems are
parity-matched against, and in places derived from, PCSX-Redux
(GPL-2.0-or-later). Some of that code is best described as a derivative
work under copyright law. Licensing PSoXide under the same
GPL-2.0-or-later is exactly what the GPL asks of a derivative work: it
keeps the lineage explicit and the distribution compliant, rather than
trying to argue the derivation away.

## Resolved Blockers

### Root license files (resolved 2026-04-30)

`LICENSE` now contains the GPL-2.0 canonical text plus the project
notice and third-party reference notes. All `Cargo.toml` `license`
fields declare `GPL-2.0-or-later`.

### PCSX-Redux derivation (addressed 2026-06-03)

Several emulator-core subsystems are parity-matched against, and in
places derived from, PCSX-Redux: the event scheduler, DMA DICR
semantics, SPU ADSR tables and voice model, MDEC AAN IDCT + YUV->RGB
pipeline, CD-ROM command timing (transcribed from `core/cdrom.cc` with
upstream line numbers), SIO baud timing, CPU cycle bias, bus/video
timing, timers, and the scanline-delta triangle rasterizer (both the CPU
and compute-shader paths). These should be treated as derivative works
of Redux.

GPL-2.0-or-later is the response the GPL prescribes for a derivative
work, so distributing this code under it is compliant. To make the
lineage explicit rather than implicit, every derived source file now
carries a `## Provenance` header that names PCSX-Redux, its copyright
holders (Copyright (C) the PCSX-Redux authors), and its license, and
points back to this audit; inline `Redux` references mark the specific
points of correspondence. Files that are implemented from hardware
documentation and only parity-verified against Redux (the GTE, the
interrupt controller, the pad, and XA-ADPCM decode) carry a header that
says exactly that and are not claimed as derived.

A correction worth recording: an earlier pass (2026-04-30) rewrote
"port of" / "Direct port of" comments into softer "behaviour-parity"
wording without changing the code. That was the wrong direction. For a
derivative work, both GPL compliance and plain honesty call for louder
attribution, not quieter. The per-file provenance headers described
above supersede that change and restore explicit derivation language.

## Resolved (continued)

### Bundled asset provenance (resolved 2026-04-30)

A complete asset inventory now lives at
[`docs/asset-provenance.md`](asset-provenance.md), covering branding,
3D models, textures, fonts, SPU tone blobs, OBJ reference meshes, and
README media. Per-directory `PROVENANCE.md` files exist beside the
fonts ([`emu/crates/frontend/assets/fonts/PROVENANCE.md`](../emu/crates/frontend/assets/fonts/PROVENANCE.md))
and SPU tones ([`sdk/crates/psx-spu/vendor/PROVENANCE.md`](../sdk/crates/psx-spu/vendor/PROVENANCE.md))
for the items where local context matters.

Provenance is documented; the asset-level **release-gating** TODOs
that remain (exact Pexels URLs, SPU tone regeneration) are tracked in
`asset-provenance.md` itself, not here. Meshy model provenance is
recorded there as paid-subscription, private, customer-owned generated
assets; retain the subscription/export evidence with project records.

### BIOS-output golden PNGs (resolved 2026-04-30)

The four direct-from-Sony-BIOS milestone PNGs (SCE diamond logo,
PlayStation 3D-P splash, "Licensed by SCEA™", BIOS shell) were removed
from `emu/crates/emulator-core/tests/milestones/` on 2026-04-30. Tests
were already hash-only and continue to compile and run; the PNGs
served only as human-readable artefacts. The `tests/milestones/`
directory is now empty and auto-cleaned by macOS.

### Launcher menu trademark cleanup (resolved 2026-04-30)

The launcher / pause overlay was renamed to "Menu" across code, docs,
and prose, dropping the Sony-owned shell terminology from the project.
The frontend overlay module is now `emu/crates/frontend/src/ui/menu.rs`;
state/input/library item types use `Menu*` names; menu theme constants
use `MENU_*`. Comments that previously framed the overlay as derived
from vendor-specific console UI were rewritten to factual descriptions of the
overlay's behaviour. The launch-simulation example was renamed
`probe_menu_launch_sim.rs` for consistency. Frontend (40) and
emulator-core (332) test suites pass after the rename.

### Dependency license audit (resolved 2026-04-30)

`cargo-deny` was installed and run across all five workspaces (root,
`emu`, `sdk`, `engine`, `editor`) with the allow-list defined in
[`deny.toml`](../deny.toml). All workspaces pass (`licenses ok`).

The transitive dependency tree carries the following non-permissive
licenses, all of which are compatible with GPL-2.0-or-later and are
explicitly allow-listed:

- **BSL-1.0** (Boost Software License 1.0) — permissive, OSI-approved,
  FSF Free/Libre. Reaches the tree via `clipboard-win` and
  `error-code` through `arboard` → `egui-winit`.
- **OFL-1.1** (SIL Open Font License) — covers fonts bundled by
  `epaint_default_fonts` (the egui default font crate). Fonts are
  data, not linked code; bundling is "mere aggregation", not
  derivative work.
- **Ubuntu-font-1.0** — same crate, same aggregation rationale.

Re-run any time with:

```bash
for ws in . emu sdk engine editor; do
  (cd "$ws" && cargo deny --manifest-path Cargo.toml check licenses \
    --config "$(git rev-parse --show-toplevel)/deny.toml")
done
```

`cargo-deny` advisories / bans / sources checks have not been run yet
(separate from licenses). They are left at their default-permissive
settings in `deny.toml`; tightening them is a future-hardening item,
not a publish blocker.

## Outstanding Blockers

None. All previously identified blockers are resolved. The remaining
items below are quality / completeness, not legal blockers.

## Lower-Risk Or Documented Items

- `sdk/crates/psx-font/vendor/PROVENANCE.md` documents font8x8 and IBM
  VGA font sources as Public Domain.
- `engine/examples/showcase-lights/vendor/cube.obj` is marked
  hand-authored public domain in the file header.
- `engine/examples/showcase-3d/vendor/teapot.obj` identifies itself as
  a simplified Utah Teapot. Add explicit public-domain/attribution
  notes before release.
- `engine/examples/showcase-3d/vendor/suzanne.obj` was generated by
  MeshLab, but the underlying Suzanne mesh provenance/license is not
  written down. Add it before release.
- `engine/examples/editor-playtest/generated/level_manifest.rs` is a
  tracked placeholder with no `include_bytes!`. Cooked generated
  manifests, rooms, textures, and models are ignored and regenerated.
- `/build/` is ignored and should stay untracked.

## Generated Artifacts

Expected public-source contract:

- Tracked: `engine/examples/editor-playtest/generated/level_manifest.rs`
  placeholder only.
- Ignored/regenerated:
  `engine/examples/editor-playtest/generated/level_manifest.cooked.rs`,
  plus cooked generated `rooms/`, `textures/`, and `models/`
  directories under `engine/examples/editor-playtest/generated/`.
- Ignored/regenerated: `/build/examples/.../*.exe`.
- Tracked cooked demo assets: small `.psxt`, `.psxm`, `.psxmdl`, and
  `.psxanim` blobs that examples or the default project need from a
  clean clone.

The tracked cooked demo assets still need provenance because generated
binary blobs inherit the licensing of their source material.

## Third-Party References

References in docs/comments include:

- nocash PSX-SPX,
- PCSX-Redux,
- DuckStation,
- public PS1 hardware behavior notes.

Reference citations are fine. For public licensing, distinguish clearly
between:

- specifications and observations used to implement behavior,
- external tools used for testing,
- source code translated into this repository.

The third category is the one that changes license obligations.

## Recommended Pre-Publish Tasks

1. Resolve the asset-level TODOs in
   [`asset-provenance.md`](asset-provenance.md) (exact Pexels URLs,
   SPU tone regeneration-or-delete decision, and retention of Meshy
   subscription/export evidence).
2. Capture fresh README screenshots from a clean clone after the
   asset-level TODOs are settled.
3. Any remaining trademark-adjacent prose surfaced by future review.
