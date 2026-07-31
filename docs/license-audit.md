# PSoXide License And Provenance Audit

Audit date: 2026-04-29 (last revised 2026-07-31).

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

### PCSX-Redux derivation (addressed 2026-06-03; revised 2026-06-11)

Several emulator-core subsystems are parity-matched against, and in
places derived from, PCSX-Redux: the event scheduler, DMA DICR
semantics, SPU ADSR tables and voice model, MDEC AAN IDCT + YUV->RGB
pipeline, CD-ROM command timing (transcribed from `core/cdrom.cc` with
upstream line numbers), SIO baud timing, CPU cycle bias, bus/video
timing, timers, and the hardware-renderer primitive pipeline
(`psx-gpu-render`). These should be treated as derivative works of
Redux.

Revision 2026-06-11, triangle rasterization: the CPU scanline-delta
triangle rasterizer (formerly listed here as Redux-derived) was
REPLACED in 2026-06 (`2f4b0063`) after real-hardware VRAM read-back
tests proved Redux's edge-coverage rule wrong on silicon. The current
`gpu.rs` triangle path implements the silicon-verified center-sampled coverage
rule, written from documented behaviour (the same rule Mednafen and DuckStation
implement) with no source from either project, and is verified pixel-exact
against a real console. The compute-shader rasterizer crate was removed in the
2026-06 dedup. The `gpu.rs` module keeps its Redux provenance header for the
non-triangle paths (fills, rects, blending, dither) that retain Redux lineage.

Revision 2026-06-11, parity harness: the lockstep parity-oracle crate
and its Makefile targets were removed (real PS1 hardware is the
accuracy oracle). Retiring the testing harness does not change any derivation
status above; provenance headers stay.

Additional behavioural references now credited in `LICENSE` (no code
derived from either): JaCzekanski's ps1-tests (the real-console GTE
conformance corpus consumed at runtime by `gte_fuzz_replay`, fetched
from upstream, never committed) and the MiSTer PSX core (GTE internal
operation ordering). The GTE remains implemented from hardware
documentation; as of 2026-06-11 it is additionally validated bit-exact
against the ps1-tests real-console corpus (1100/1100), which is a
stronger independence basis than the retired Redux parity diffing.

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

### Cross-language similarity scan (2026-06-18)

To check the AI-assisted emulator code against the reference emulators
beyond the per-file provenance headers, a cross-language similarity scan was
run over the emulator layer (`emu/crates/emulator-core/src` and
`emu/crates/psx-gpu-render/src`) against full checkouts of DuckStation,
PCSX-Redux, and the Mednafen PS1 core (Beetle-PSX). Three methods were used:
identifier isolation (identifiers shared with one reference but not the
others), verbatim 7-word comment-phrase matching, and numeric
table/constant-sequence fingerprinting (to catch a copied lookup table).

Findings:

- Identifier overlap was comparable across all three references (DuckStation
  46%, Redux 40%, Mednafen 39% of the emulator's identifiers). A line-by-line
  translation of one emulator would put that emulator far above the others;
  the flat distribution is consistent with a hardware-focused implementation
  sharing common PS1 vocabulary, rather than an obvious line-by-line port of
  one reference emulator. The DuckStation-isolated identifiers were
  common English words, Rust standard idioms, shared PSX-SPX hardware names
  and register addresses, and standard graphics-API terms, with none of
  DuckStation's distinctive internal names.
- Only three verbatim comment phrases were shared with DuckStation, all
  benign: two are nocash PSX-SPX SPU reverb register names (shared upstream
  documentation) and one is a generic rasterization fragment.
- The only non-trivial shared numeric sequence is the standard JPEG/MPEG
  zigzag scan order used by the PS1 MDEC, a published specification constant
  present in every JPEG/MDEC implementation.

Conclusion: no copied code was found from DuckStation, Redux, or Mednafen
beyond the Redux derivations already disclosed above. Redux overlap is
expected and GPL-compatible.

Honest caveat: cross-language similarity scanning compares tokens, constants,
and comments, not semantics, so it cannot prove the absence of a line-by-line
semantic translation with mathematical certainty. The result is strong
corroboration of the provenance model above, read together with the
real-hardware validation process, which provides an additional non-source-code
basis for validating behaviour.

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

`cargo-deny` was installed and run across all workspaces (the repo-root
host workspace plus the `sdk` and `engine` device workspaces) with the allow-list defined in
[`deny.toml`](../deny.toml). All workspaces pass (`licenses ok`).

The transitive dependency tree carries the following non-permissive
licenses, all of which are compatible with GPL-2.0-or-later and are
explicitly allow-listed:

- **BSL-1.0** (Boost Software License 1.0) - permissive, OSI-approved,
  FSF Free/Libre. Reaches the tree via `clipboard-win` and
  `error-code` through `arboard` → `egui-winit`.
- **OFL-1.1** (SIL Open Font License) - covers fonts bundled by
  `epaint_default_fonts` (the egui default font crate). Fonts are
  data, not linked code; bundling is "mere aggregation", not
  derivative work.
- **Ubuntu-font-1.0** - same crate, same aggregation rationale.

Re-run any time with:

```bash
for ws in . sdk engine; do
  (cd "$ws" && cargo deny --manifest-path Cargo.toml check licenses \
    --config "$(git rev-parse --show-toplevel)/deny.toml")
done
```

`cargo-deny` now also runs in CI (see
[`.github/workflows/ci.yml`](../.github/workflows/ci.yml)) on every push and
pull request, checking `licenses` and `sources` across all three workspaces.
The `sources` check was hardened from `warn` to `deny` after confirming the
dependency tree resolves entirely from crates.io with no git or
alternate-registry sources. The `advisories` and `bans` checks are left at
`warn` on purpose (rationale is in the `deny.toml` section comments): a
crate's yanked/advisory status changing upstream should not break an
unrelated change's CI, and duplicate transitive versions carry no licensing
risk.

### BIOS independence and Sony-material re-audit (2026-07-31)

A full re-audit (source, complete git history, disc tooling, emulator,
SDK/runtime) confirmed: no Sony code, no BIOS image, and no
BIOS-extracted data anywhere in the repository or its history. No
BIOS-sized blob was ever committed; the only binaries in history are
project-built discs, a spectrum data file, and the public-domain IBM
PC VGA font. Findings and the follow-up changes:

- **Runtime BIOS surface reduced to debug output only.** The engine
  boot path used to make one BIOS service call, `A(44h) FlushCache`,
  after patching the exception vector. `psx-rt` now performs the
  documented isolate-and-tag-clear i-cache flush itself
  (`sdk/crates/psx-rt/src/cache.rs`, written from nocash PSX-SPX
  cache-control documentation), and the six unused BIOS trampolines
  (puts, the event functions, std_out_putchar) were deleted. The one
  remaining BIOS entry point is `A(3Ch) putchar`, reached only from
  panic/debug TTY output and kept deliberately as the canonical PS1
  debug channel; nothing on the frame or boot path calls the BIOS.
  Verified in emulation (display and VRAM hashes of five SDK examples
  bit-identical before/after); the on-silicon check rides the next
  hardware-tests run, since the test disc now uses the same routine.
- **Disc system area ships nothing from Sony.** `mkisopsx` zero-fills
  sectors 0..15 and contains no license or logo data. The only
  injection path is the explicit user opt-in (`--system-area` /
  `PSOXIDE_SYSTEM_AREA`), which nothing in the repo, Makefile, or CI
  uses.
- **PSX-EXE region marker.** `psx-rt` writes the conventional header
  string "Sony Computer Entertainment Inc. for North America area"
  into the EXE header region field (offset 4Ch). This is the one
  Sony-authored string in shipped artifacts: a functional
  interoperability field, not code or creative content. The BIOS does
  not verify it, so swapping in neutral text is possible; that swap
  is optional and would want a real-console boot check first.
  Decision open.
- **BIOS-output PNGs in git history.** The four milestone PNGs removed
  from tracking on 2026-04-30 remain retrievable from git history on
  any clone. Removing them for good needs a history rewrite
  (`git filter-repo` plus a force push that breaks existing clones).
  Decision open.
- **SPU reverb preset values.** `emulator-core/src/spu.rs` carries the
  32-word "Space Echo" reverb register preset (plus two single-word
  hardware-observed constants in `cpu.rs`), captured from the
  author's console and documented in nocash PSX-SPX. Hardware
  register state, not copied code; reviewed and accepted.
- **Wording cleanups.** "BIOS-compatible filesystem" (psx-mc) and
  "IBM VGA BIOS console font" (psx-font) read as PS1-BIOS-derived at
  a glance; both were reworded (card-manager interoperability and PC
  BIOS lineage respectively). The license-string literals in the
  emulator's region-detection tests are matching fixtures for discs
  the user supplies, never written to any artifact.

## Outstanding Blockers

None. All previously identified blockers are resolved. The remaining
items below are quality / completeness, not legal blockers.

## Lower-Risk Or Documented Items

- `sdk/crates/psx-font/vendor/PROVENANCE.md` documents the bundled font
  sources: font8x8 and IBM VGA (Public Domain), Kenney (CC0-1.0),
  Google Fonts families (OFL-1.1 / Apache-2.0), and Spleen 5x8
  (BSD-2-Clause; binary distributions embedding it must carry the
  notice in `vendor/spleen-LICENSE`).
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
