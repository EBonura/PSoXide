# PSoXide License And Provenance Audit

Last revised: 2026-10-09. Dated sections record what an audit found on the day
it ran; the current position below supersedes them where they differ.

Scope of this file: the SDK repository (`crates/`, `sdk/`, `tools/`, `assets/`,
the website). The emulator, the editor and engine, and the games each keep
their own provenance record, listed below.

## Current Position

The SDK is licensed under **GPL-2.0-or-later**. The full license text is at
[`LICENSE`](../LICENSE), alongside the project notice and the list of projects
that were consulted as references. Every `Cargo.toml` declares the same license.

Policy: code that turns out to be copied or translated from another project is
rewritten, not disclosed. A disclosure in this file does not change the
license obligations of a copy; only removing the copy does.

Where each repository records its own provenance:

| Repository | Record |
|---|---|
| PSoXide-emulator | [`docs/PROVENANCE.md`](https://github.com/EBonura/PSoXide-emulator/blob/main/docs/PROVENANCE.md) (which modules were rewritten from documentation, which behaviours are still matched to PCSX-Redux traces, the texture filter) and [`docs/hle-bios-provenance.md`](https://github.com/EBonura/PSoXide-emulator/blob/main/docs/hle-bios-provenance.md) (the high-level kernel) |
| PSoXide-editor | [`docs/license-audit.md`](https://github.com/EBonura/PSoXide-editor/blob/main/docs/license-audit.md) and [`docs/provenance-checklist.md`](https://github.com/EBonura/PSoXide-editor/blob/main/docs/provenance-checklist.md) |
| hl-psx | [`PROVENANCE.md`](https://github.com/EBonura/hl-psx/blob/main/PROVENANCE.md): source-informed adaptation of Valve's public Half-Life SDK, and fixed-point adaptations of GPL-licensed Quake code |
| quake-psx | [`PROVENANCE.md`](https://github.com/EBonura/quake-psx/blob/main/PROVENANCE.md) |
| This repository | [`docs/asset-provenance.md`](asset-provenance.md), [`docs/firmware-cleanup.md`](firmware-cleanup.md), `sdk/crates/psx-font/vendor/PROVENANCE.md` |

## What Changed Since The Earlier Audits

- The emulator's PCSX-Redux lineage is smaller than the 2026-06 audit
  below describes. The event scheduler, DMA, SIO0, SPU and MDEC were rewritten
  from the nocash PSX-SPX notes, console measurements and ps1-tests captures.
  The CPU, bus, video timing, GPU and CD-ROM modules still contain behaviour
  that was first matched to PCSX-Redux traces; the emulator's
  `docs/PROVENANCE.md` lists each case and marks it `gate-pinned` in code.
- The high-level kernel emulation replaced the external BIOS path. It is
  written from public hardware documentation and console measurements.
- The JINC2 and xBR texture filters were ports of third-party shader code
  (Hyllian's jinc2 and DuckStation's xBR). They were removed from the emulator
  on 2026-10-08. The Edge filter that replaces them was written from a
  specification of the mathematics alone.
- The 2026-06-11 revision below, which replaced the CPU triangle rasterizer
  with one derived from silicon measurements, still stands.

## Historical Results

### PCSX-Redux derivation (addressed 2026-06-03; revised 2026-06-11)

At that date several emulator-core subsystems were parity-matched against, and
in places derived from, PCSX-Redux: the event scheduler, DMA DICR semantics,
SPU ADSR tables and voice model, the MDEC AAN IDCT and YUV to RGB pipeline,
CD-ROM command timing, SIO baud timing, CPU cycle bias, bus and video timing,
timers, and the hardware-renderer primitive pipeline. They were treated as
derivative works of Redux and carried `## Provenance` headers naming it.
The scheduler, DMA, SIO0, SPU and MDEC items have since been rewritten (see
above); the rest is tracked in the emulator's provenance record.

Revision 2026-06-11, triangle rasterization: the CPU scanline-delta triangle
rasterizer was replaced in 2026-06 (`2f4b0063`) after real-hardware VRAM
read-back tests showed Redux's edge-coverage rule differs from silicon. The
current `gpu.rs` triangle path implements the silicon-verified center-sampled
coverage rule from documented behaviour, with no source from Mednafen or
DuckStation, and is verified pixel-exact against a console.

Revision 2026-06-11, parity harness: the lockstep parity-oracle crate and its
Makefile targets were removed; real hardware is the accuracy oracle.

Additional behavioural references credited in `LICENSE` (no code derived from
either): JaCzekanski's ps1-tests, the real-console GTE conformance corpus
consumed at runtime by `gte_fuzz_replay` and fetched from upstream, never
committed; and the MiSTer PSX core (GTE internal operation ordering). The GTE
is implemented from hardware documentation and validated bit-exact against
that corpus (1100/1100 as of 2026-06-11).

A correction worth keeping: an earlier pass (2026-04-30) rewrote "port of"
comments into softer "behaviour-parity" wording without changing the code.
That was the wrong direction. Where code is derived, the attribution should be
louder, not quieter.

### Cross-language similarity scan (2026-06-18)

The emulator layer (`emu/crates/emulator-core/src` and
`emu/crates/psx-gpu-render/src`, as they were then) was scanned against full
checkouts of DuckStation, PCSX-Redux and the Mednafen PS1 core (Beetle-PSX)
with three methods: identifier isolation, verbatim 7-word comment-phrase
matching, and numeric table fingerprinting.

- Identifier overlap was similar across all three references (DuckStation 46%,
  Redux 40%, Mednafen 39%). A line-by-line translation of one emulator would
  put it far above the others. The DuckStation-isolated identifiers were common
  English words, Rust idioms, shared PSX-SPX names and register addresses, and
  graphics-API terms.
- Three verbatim comment phrases were shared with DuckStation: two nocash
  PSX-SPX SPU reverb register names and one generic rasterization fragment.
- The only non-trivial shared numeric sequence was the JPEG/MPEG zigzag scan
  order the MDEC uses, a published constant.

The scan did not report the DuckStation and Hyllian shader ports listed above,
which a source inspection on 2026-10-08 found. Scanning compares tokens,
constants and comments, not semantics, read the 2026-06-18 result as
corroboration with a defined scope, not as clearance.

## Resolved

### Root license files (resolved 2026-04-30)

`LICENSE` contains the GPL-2.0 text plus the project notice and the list of
consulted references. All `Cargo.toml` `license` fields declare
`GPL-2.0-or-later`.

### Bundled asset provenance (resolved 2026-04-30)

[`asset-provenance.md`](asset-provenance.md) inventories branding, 3D models,
textures, fonts, OBJ reference meshes and README media. The font sources are in
`sdk/crates/psx-font/vendor/PROVENANCE.md`: font8x8 and IBM VGA (public
domain), Kenney (CC0-1.0), Google Fonts families (OFL-1.1 or Apache-2.0) and
Spleen 5x8 (BSD-2-Clause; binary distributions embedding it must carry the
notice in `vendor/spleen-LICENSE`). Asset-level items that are still open (the
exact Pexels URLs, retention of the Meshy subscription and export evidence) are
tracked in `asset-provenance.md`, not here.

### Dependency licenses (checked 2026-04-30)

`cargo-deny` was run with the allow-list in [`deny.toml`](../deny.toml) and
reported `licenses ok`. The non-permissive licenses in the dependency tree are
all GPL-compatible and allow-listed: BSL-1.0 (via `clipboard-win` and
`error-code`), OFL-1.1 and Ubuntu-font-1.0 (fonts bundled by
`epaint_default_fonts`; data, not linked code). That run covered the
repository before the split. This repository's CI does not run
`cargo-deny`; re-run it after dependency changes:

```bash
cargo deny --manifest-path Cargo.toml check licenses --config deny.toml
```

## Open Items

- Run `cargo-deny` against the split repositories and decide whether to add it
  to their CI. The earlier statement that CI enforces it described the
  repository before the split.
- Any attribution or naming corrections that a future review surfaces.

## Third-Party References

References in docs and comments include nocash PSX-SPX, PCSX-Redux,
DuckStation and public PS1 hardware notes. A citation is fine. For licensing,
keep three things apart: specifications and observations used to implement
behavior, external tools used for testing, and source code translated into this
repository. The third changes license obligations.
