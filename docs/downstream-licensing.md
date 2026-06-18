# Downstream Licensing and Provenance

This document explains, in one place, how PSoXide is licensed, how it was
built, what is and is not derived from other projects, and what that means
if you build and distribute something on top of it. It exists so you do not
have to reconstruct the picture from the README, the license audit, and the
per-file headers separately.

It is explanatory, not legal advice. For a binding answer about a specific
use, especially a commercial one, consult a qualified attorney.

## Summary

- PSoXide is licensed **GPL-2.0-or-later**.
- It is **not** a clean-room implementation, and it does not claim to be.
- Parts of the emulator core are derived from **PCSX-Redux**
  (GPL-2.0-or-later). That derivation is the reason the license is GPL, and
  it is tracked explicitly, file by file.
- The project was built with **heavy AI assistance**. That is disclosed here
  and in the README. Disclosure is an honest statement, not a warranty that
  the code is clean-room or free of third-party influence.
- If you distribute binaries that include PSoXide engine/runtime/SDK code,
  GPL obligations apply to that code and to your modifications of it. Your
  own game content stays yours, as long as it is not itself derived from
  GPL-covered PSoXide code or assets.

## License

PSoXide is licensed under **GPL-2.0-or-later**. The full text and the project
notice are in [`LICENSE`](../LICENSE), and every workspace `Cargo.toml`
declares `license = "GPL-2.0-or-later"`.

The GPL choice is not aesthetic. It is the license that the code PSoXide
derives from already uses, and it is what that license requires of a
derivative work. See "Provenance model" below.

## How PSoXide was built

PSoXide was developed with heavy use of AI coding assistants, with a human
directing the architecture, debugging, and hardware verification. This is
stated plainly in the README's "How This Was Built" section and repeated
here because it matters for provenance.

What that disclosure means:

- A large part of the code was written by an AI assistant under human
  direction, review, and integration.
- AI assistance is **not** a clean-room process. An LLM is trained on large
  amounts of existing code, so AI-written code can carry influence from its
  training data that neither the tool nor the author can fully audit.
- Disclosing AI assistance is therefore **not** a warranty of clean-room
  provenance or of non-infringement. It is the opposite: an honest statement
  that this code was not produced under clean-room conditions.

The project's response to that uncertainty is not to wave it away, but to
track provenance explicitly and conservatively. See "Provenance model" and
"Known risk model" below.

## Provenance model

PSoXide distinguishes, per file, between three kinds of origin:

1. **Derived from PCSX-Redux.** Several emulator-core subsystems are
   parity-matched against, and in places derived from, PCSX-Redux
   (GPL-2.0-or-later): for example the event scheduler, DMA semantics, SPU
   ADSR tables and voice model, MDEC IDCT and colour pipeline, CD-ROM command
   timing (transcribed from `core/cdrom.cc`), and parts of the
   hardware-renderer primitive pipeline. These files carry a `## Provenance`
   header naming PCSX-Redux, its copyright holders, and its license, with
   inline `Redux` markers at the points of correspondence. They are treated
   as derivative works of Redux.

2. **Implemented from hardware documentation and real-console testing.**
   Other subsystems (for example the GTE, the interrupt controller, the pad,
   and XA-ADPCM decode) are implemented from public hardware documentation
   (nocash PSX-SPX) and verified against real PlayStation hardware, with only
   behavioural parity-checking against other emulators. These carry a header
   that says exactly that, and are not claimed as derived. The GTE, for
   instance, is validated bit-exact against a real-console conformance corpus
   rather than against another emulator's source.

3. **Project-originated PSoXide code.** The SDK, runtime engine, editor,
   tools, and other project code that is not marked as derived or externally
   sourced carry the project's GPL-2.0-or-later license. This is separate from
   the AI-assistance disclosure above, which is not a clean-room guarantee.

The full audit, including the Redux subsystem list and the corrections made
over time, is in [`license-audit.md`](license-audit.md). External projects
used only as **behavioural references** or **external test tools** (Mednafen,
DuckStation, JaCzekanski's ps1-tests, the MiSTer core) are credited in
[`LICENSE`](../LICENSE), contribute no source code, and a cross-language
similarity scan recorded in the audit found no copied code from them.

## What this means if you build on PSoXide

PSoXide is a stack: emulator and debugger, SDK, runtime engine, editor, and
tools. If you build and **distribute** a project on top of it, the GPL
applies to the PSoXide code you ship and to your modifications of it.

- If your distributed binary **includes or is linked with** PSoXide
  engine/runtime/SDK code, that covered code (and your changes to it) must
  remain available under GPL-compatible terms to the people you distribute
  to. This is ordinary GPL copyleft, the same deal as building on any other
  GPL engine (id Software's GPL Doom/Quake engines are the classic example).
- **Mere aggregation.** Shipping your game as a separate program alongside
  PSoXide (for example bundling the emulator/editor next to a separate game
  executable on the same medium) is aggregation, not a combined work, and
  does not by itself place your separate program under the GPL. Distributing
  the GPL components still requires offering their corresponding source.
- **You can sell GPL software.** The GPL permits charging money; it requires
  that recipients receive the corresponding source for the GPL-covered parts.

If your goal is a fully closed-source commercial product built on the PSoXide
SDK/engine, note that the engine is offered as GPL today, and plan your
licensing accordingly.

## Independent game content

Game content is separate from the engine and toolchain code. Art, textures,
models, music, writing, level and world data, and other project-specific
assets you create remain owned by you and under whatever license you choose,
**provided** they are not themselves derived from GPL-covered PSoXide code or
from GPL-covered assets in this repository.

This is the same code-versus-content split that lets a GPL engine power a
commercial game with proprietary assets: content loaded as data is not a
derivative work of the engine code. PSoXide does not claim ownership of
content you create with the editor, cooker, SDK, or runtime. Provenance for
the assets that ship *in this repo* is tracked separately in
[`asset-provenance.md`](asset-provenance.md).

## Known risk model

The honest residual risk is provenance uncertainty from AI-assisted
development: because AI-written code can be influenced by training data that
cannot be fully audited, no clean-room guarantee is possible for AI-assisted
code. PSoXide does not claim one.

What the project does to keep that risk small and visible:

- **Explicit derivation tracking.** Known derivations carry per-file
  `## Provenance` headers; the lineage is stated, not disguised.
- **License audit.** [`license-audit.md`](license-audit.md) records the
  derivation list, the corrections made, and a cross-language similarity scan
  against the reference emulators that found no copied code (with its own
  honest caveat that cross-language scanning cannot prove a negative with
  certainty).
- **Dependency checks.** `cargo-deny` enforces a GPL-compatible dependency
  license allow-list across every workspace (see [`deny.toml`](../deny.toml)),
  and CI runs it on every change.
- **Conservative replacement.** Where real-hardware testing showed a borrowed
  behaviour was wrong (the triangle rasterizer edge rule), the code was
  rewritten from documented behaviour and verified against silicon rather
  than kept.
- **Asset provenance documentation.** Every tracked binary asset's origin and
  license is recorded in [`asset-provenance.md`](asset-provenance.md),
  including items whose provenance is still being resolved.

This does not reduce downstream risk to zero, and this document does not
claim that it does. It states the position accurately so you can make an
informed decision.

## Not legal advice

This document explains how the repository is licensed and documented. It is
not legal advice and creates no warranty. For a binding determination about a
specific use, especially a commercial one, consult a qualified attorney.
