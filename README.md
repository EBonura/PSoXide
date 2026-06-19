# PSoXide

<p align="center">
  <img src="assets/branding/psoxide-logo.svg" alt="PSoXide" width="520">
</p>

<p align="center">
  <a href="LICENSE"><img alt="License: GPL-2.0-or-later" src="https://img.shields.io/badge/license-GPL--2.0--or--later-blue.svg"></a>
  <img alt="Rust: nightly" src="https://img.shields.io/badge/rust-nightly-orange.svg">
  <img alt="Platforms: macOS · Linux" src="https://img.shields.io/badge/platforms-macOS%20%C2%B7%20Linux-lightgrey.svg">
</p>

**PSoXide is an open-source PlayStation 1 development stack, written in Rust.**
One repository, five parts: an accuracy-focused **emulator** and debugger, a
bare-metal **SDK**, a runtime **engine**, an asset **editor**, and **disc
tooling**. It targets the real machine, projects export to CUE/BIN images that
boot in emulators or on original hardware.

It exists to build one game: a dark, third-person PS1 souls-like. Everything
else is in service of shipping that vertical slice and proving the full pipeline
end to end.

![PSoXide editor](assets/media/readme/editor-preview.png)

| | |
| --- | --- |
| ![Streamed playtest](assets/media/readme/demo2-playtest.png) | ![Streamed playtest](assets/media/readme/demo3-playtest.png) |
| ![Streamed playtest](assets/media/readme/demo4-playtest.png) | ![Streamed playtest](assets/media/readme/demo5-playtest.png) |

> **Early software**, useful, hackable, and moving fast, but APIs are not stable
> and there are no release binaries yet. See [Status](#status) for what works and
> what does not. Project page:
> [bonnie-games.itch.io/psoxide](https://bonnie-games.itch.io/psoxide).

## Features

- **Emulator** — CPU, GTE, GPU, DMA, CD-ROM, SIO, timers, MDEC, and SPU, with an
  HLE BIOS path so homebrew needs no retail BIOS. Desktop frontend
  (winit/wgpu/egui) with debugger panels for registers, memory, VRAM, execution
  history, profiling, and savestates.
- **SDK** — bare-metal Rust crates for the `mipsel-sony-psx` target: GPU, GTE,
  SPU, pad, fonts, DMA/ordering tables, and runtime.
- **Engine** — a Scene/App framework with a streamed room runtime (chunk
  residency, CD-sector packing, 60 Hz paced simulation), 3D with hardware
  lighting and fog, particles, and CD-DA.
- **Editor** — project model, 2D/3D viewports, room-grid authoring, asset
  cooking (`psxed`), and one-click **Play** that cooks, builds, boots a disc, and
  shows the live framebuffer in the viewport.
- **Disc tooling** — CUE/BIN builders and headless export of an authored project
  to a bootable image.

Per-crate detail lives in each area's README (see [Repository
layout](#repository-layout)).

## Status

Early, but the whole pipeline works end to end: author a project in the editor,
cook it, build a PS1 disc, and boot it. APIs and data formats still move, and
there are no release binaries yet, so build from source.

**More built-out than "early" suggests.** The emulator implements the full set
of PS1 subsystems, CPU, GTE, GPU, DMA, CD-ROM (with XA-ADPCM and CD-DA), SIO with
digital and DualShock pads, timers, MDEC, interrupts, a 24-voice SPU with reverb,
and memory cards that persist to disk. The SDK and engine are substantial, and
the editor is a real authoring tool rather than a mock-up.

**Rough edges and known gaps:**

- Broad commercial-game compatibility is incomplete; timing drift and long-tail
  peripheral behaviour are active research, tracked per game.
- Peripherals cover the digital/analog pad and memory cards only (no multitap,
  mouse, or light-gun).
- A few deliberate emulator simplifications: no instruction-cache model, and some
  rarely-used GPU/DMA/timer edge cases favour parity over silicon-exactness.
- The editor is usable but pre-1.0 (import UX, project templates, undo depth, and
  packaging still need work).
- No published release binaries, and the public CI currently gates dependency
  policy only, with build/test/lint advisory.

## Real-hardware accuracy

PSoXide is validated against an actual PS1 console, not just against other
emulators:

- **GTE: bit-for-bit** against JaCzekanski's real-console `ps1-tests` corpus
  (1100/1100 across all opcodes and registers), with the software GTE also
  covered by an extensive in-tree unit-test suite.
- **Triangle rasterizer matches silicon** — center-sampled coverage confirmed by
  VRAM read-back photographed on hardware, after the original reference edge rule
  was proven wrong on the console.
- **Silicon-measured GTE hazards** modeled by no other public emulator or the
  MiSTer core (the 2-instruction GTE load delay; a mid-MVMVA register commit) —
  they reproduce a real skinned-mesh corruption bit-for-bit.

Every finding is logged burn by burn in
[`docs/hardware-burn-ledger.md`](docs/hardware-burn-ledger.md): each divergence
between console and emulator becomes a conformance test or a fix.

## Quick start

You need a nightly Rust toolchain (pinned by `rust-toolchain.toml`) and `make`.
On Linux you also need the frontend's native libraries (see prerequisites below).

```bash
git clone https://github.com/EBonura/PSoXide.git psoxide
cd psoxide
make check && make test                # build + fast tests (no BIOS or games needed)

make hello-tri-disc && make run-tri    # build a homebrew example and boot it
make run                               # launch the desktop frontend
```

Open the editor from the frontend's **Create** menu, then hit **Play** to cook,
build, and boot the active project live in the viewport.

<details>
<summary><strong>Platform prerequisites</strong></summary>

**macOS**
```bash
xcode-select --install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

**Debian / Ubuntu** (native libraries for the desktop frontend)
```bash
sudo apt install build-essential make pkg-config libasound2-dev libudev-dev \
  libx11-dev libxi-dev libxrandr-dev libxinerama-dev libxcursor-dev \
  libxkbcommon-dev libwayland-dev mesa-vulkan-drivers
```

**Windows** (MSVC host; `make` is optional, use `cargo` directly)
```powershell
winget install Rustlang.Rustup
winget install Microsoft.VisualStudio.2022.BuildTools --override "--add Microsoft.VisualStudio.Workload.VCTools --passive --wait"
```

The pinned nightly installs `rustfmt`, `clippy`, `rust-src`, and `llvm-tools`
automatically. The bare-metal PSX target builds via `-Zbuild-std` + `rust-src`
(there is no prebuilt standard library for it).
</details>

<details>
<summary><strong>Using a retail BIOS</strong> (optional)</summary>

Homebrew examples and editor Play use the HLE BIOS path and need no BIOS. A real
BIOS is only required for retail-disc boot and the BIOS canaries. Dump your own,
then set it in the frontend (**Menu → Choose BIOS path**) or via
`export PSOXIDE_BIOS=/path/to/SCPH1001.BIN`. Only use BIOS and game images you
legally own.
</details>

<details>
<summary><strong>Common make targets</strong></summary>

```bash
make check / test / fmt / lint    # quality gates
make examples                     # build every SDK/engine/game example disc
make <name>-disc / run-<name>     # build / boot one example
make validate                     # exact-hash display/VRAM regression matrix
```

The `Makefile` is the source of truth on Unix-like hosts; the per-area READMEs
cover everything else.
</details>

## Provenance and licensing

PSoXide is developed with **heavy AI assistance**, with a human directing the
architecture, debugging, and hardware verification (that accounts for the commit
volume). This is disclosed openly and is **not** a clean-room claim.

It is licensed **GPL-2.0-or-later**, and that is *required*, not stylistic: parts
of the emulator core are derived from **PCSX-Redux** (GPL-2.0-or-later), and the
GPL is what that derivation obliges. Derived files carry per-file `## Provenance`
headers; subsystems written from hardware documentation and only parity-checked
say so explicitly. Your own game content — art, models, levels, music — stays
yours.

If you plan to build and **distribute** on top of PSoXide, start with
**[`docs/downstream-licensing.md`](docs/downstream-licensing.md)**. Full detail:
[`LICENSE`](LICENSE), [`docs/license-audit.md`](docs/license-audit.md), and
[`docs/asset-provenance.md`](docs/asset-provenance.md).

## Repository layout

| Area | What's inside |
| --- | --- |
| [`emu/`](emu/README.md) | Emulator core, desktop frontend, renderer, validation. |
| [`sdk/`](sdk/README.md) | Bare-metal PSX SDK crates and `hello-*` examples. |
| [`engine/`](engine/README.md) | Scene/App runtime engine, level schema, example games. |
| [`editor/`](editor/README.md) | Project model, asset cookers, and editor UI. |
| [`crates/`](crates/README.md) | Shared `no_std` PSX primitives. |
| [`tools/`](tools/README.md) | Disc-mastering and EXE utilities. |
| [`docs/`](docs/README.md) | Architecture, hardware reference, and planning notes. |

## Examples

The repo ships runnable examples that double as the SDK/engine test suite:
`hello-*` bare-metal SDK demos, 3D / lighting / fog / particle showcases, and
small games (Pong, Breakout, Space Invaders). Build them all with
`make examples`; descriptions are in the [`sdk/`](sdk/README.md) and
[`engine/`](engine/README.md) READMEs.

| ![showcase-3d](assets/media/readme/examples/showcase-3d.png) | ![showcase-fog](assets/media/readme/examples/showcase-fog.png) | ![showcase-model](assets/media/readme/examples/showcase-model.png) |
| --- | --- | --- |

## License

**GPL-2.0-or-later** (see [`LICENSE`](LICENSE)). Commercial homebrew is fine: you
can sell games built with PSoXide; the GPL only requires that the covered code
you distribute stays GPL-compatible, and your original assets remain yours. The
details, and what this means for projects built on top, are in
[Provenance and licensing](#provenance-and-licensing) and
[`docs/downstream-licensing.md`](docs/downstream-licensing.md).
