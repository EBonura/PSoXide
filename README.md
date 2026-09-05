# PSoXide

<p align="center">
  <img src="assets/branding/psoxide-logo.svg" alt="PSoXide" width="520">
</p>

<p align="center">
  <a href="LICENSE"><img alt="License: GPL-2.0-or-later" src="https://img.shields.io/badge/license-GPL--2.0--or--later-blue.svg"></a>
  <img alt="Rust: nightly" src="https://img.shields.io/badge/rust-nightly-orange.svg">
  <img alt="Platforms: macOS · Linux · Windows" src="https://img.shields.io/badge/platforms-macOS%20%C2%B7%20Linux%20%C2%B7%20Windows-lightgrey.svg">
  <a href="https://bonnie-studios.itch.io/psoxide"><img alt="Web emulator: live" src="https://img.shields.io/badge/web%20emulator-live-brightgreen.svg"></a>
</p>

## One PlayStation stack, many games

PSoXide is the common source base behind **Cortex Ignition**, **Quake PSX**,
**HL-PSX**, **Voxide**, **Nitroxide**, **PSXcel**, **GH-PSX**, the **Celeste
Collection**, and the **PSoXide Demo Disc**.

It is not only an emulator. This repository contains the full Rust development
stack used to make those projects: a bare-metal PlayStation SDK, a shared game
engine, a BSP level and asset editor, host-side cookers, disc mastering tools,
an accuracy-focused emulator, debugger, profiler, and the hardware-validation
infrastructure that keeps them agreeing with one another.

The goal is straightforward: solve a PlayStation problem once, prove it, and
make the result available to every game instead of maintaining a different
renderer, loader, controller layer, or disc pipeline in each repository.

**Try it in a browser:** the [PSoXide web build](https://bonnie-studios.itch.io/psoxide)
includes the demo disc and the SDK/engine examples. It needs no installation or
retail BIOS.

| | |
| --- | --- |
| ![PSoXide BSP editor](assets/media/readme/editor-preview.png) | ![PSoXide UI editor](assets/media/readme/editor-ui-preview.png) |
| ![PSoXide emulator](assets/media/readme/emulator-preview.png) | ![A streamed engine playtest](assets/media/readme/demo3-playtest.png) |

> [!IMPORTANT]
> PSoXide is pre-1.0 software. The end-to-end workflow is real and is already
> used by complete game ports and original projects, but APIs, file formats and
> editor workflows can still change. Native release binaries have not been cut
> yet; build from source or use the web version.

## Start here

| I want to… | Read |
| --- | --- |
| Try the emulator | [Web player](https://bonnie-studios.itch.io/psoxide) or [build from source](#quick-start) |
| Write a PS1 game in Rust | [SDK](sdk/README.md) and [downstream build setup](docs/downstream-projects.md) |
| Author a level | [Editor quickstart](editor/README.md#bsp-level-quickstart) |
| Understand the codebase | [Repository architecture](docs/repository-architecture.md) |
| Contribute or report a bug | [Contributing](CONTRIBUTING.md) |
| Find technical references | [Documentation index](docs/README.md) |

## What PSoXide powers

The projects do not all use the same amount of the stack. A small game can use
the SDK directly; a 3D game can adopt the engine and streaming runtime; a port
can share the low-level kernels while retaining its own game-specific world and
entity code.

| Project | What it is | PSoXide's role |
| --- | --- | --- |
| [Cortex Ignition Tech Demo 0.4b](editor/projects/cortex-ignition-tech-demo-0.4b/) | The flagship third-person action-RPG test: authored BSP level, player, enemy and boss | Editor, cooker, engine, animation, combat, asset streaming, emulator and disc build |
| [Quake PSX](https://github.com/EBonura/quake-psx) | Quake shareware Start and Episode 1 for the original PlayStation | SDK, fixed-point/GTE rendering, audio, input, disc I/O, profiling and shared runtime work |
| [HL-PSX](https://github.com/EBonura/hl-psx) | A bring-your-own-assets Half-Life port targeting the original PlayStation | SDK, renderer/runtime primitives, content pipeline, streaming, emulator regression and profiling |
| [Voxide](https://github.com/EBonura/voxide) | A Minecraft-style survival sandbox | SDK, runtime, input, audio, world data and disc pipeline |
| [Nitroxide](https://github.com/EBonura/nitroxide) | A Rocket League-style game with CPU and split-screen play | SDK, graphics, input, audio and reusable game systems |
| [PSXcel](https://github.com/EBonura/psxcel) | A functional spreadsheet application controlled with a joypad | SDK, 2D rendering, input and memory-card storage |
| [GH-PSX](https://github.com/EBonura/gh-psx) | A Guitar Hero-style rhythm-game prototype | SDK, low-latency input, graphics and CD audio |
| [Celeste Collection](https://github.com/EBonura/celeste-collection-psx) | Native PlayStation builds of both Celeste Classic games | SDK, launcher, input, rendering and compact disc packaging |
| [PSoXide Demo Disc](https://github.com/EBonura/PSoXide-demo-disc) | One chain-loading disc containing the games, examples, hardware tests and CD audio | Shared program build, relocation, provenance, validation and final disc mastering |

Cortex is authored in this repository. The other games live in their own
repositories and consume a tested PSoXide revision, either as a pinned source
dependency or an explicitly hydrated local SDK. That keeps releases
reproducible without turning this repository into a monorepo of copyrighted or
project-specific assets.

## How the stack fits together

```text
 source assets / maps / project
               │
               ▼
       editor and host cookers ──────► PS1-native textures, models,
               │                       animation, audio and BSP data
               ▼
        shared engine or raw SDK
               │
               ▼
        PSX executable + CUE/BIN ─────► emulator, demo disc or real console
               ▲                              │
               └──── deterministic replay, profiling and hardware evidence
```

| Layer | What is shared |
| --- | --- |
| **SDK** | Bare-metal startup, GPU/GTE, DMA and ordering tables, SPU, CD-ROM, SIO, pads, fonts, memory cards, fixed-point math and `no_std` utilities |
| **Engine** | Scene/app lifecycle, PXBSP worlds, visibility, collision, model animation, particles, lighting/fog, UI, gameplay runtime and CD-backed asset residency |
| **Editor and cookers** | BSP brush authoring, scene/UI editing, material and UV tools, model/animation import, PS1 texture conversion, validation and one-click Play |
| **Emulator** | CPU and instruction cache, GTE, GPU, DMA, CD-ROM, SIO, timers, MDEC, SPU, HLE BIOS, debugger, savestates, free camera and guest profiling |
| **Disc and validation tools** | PSX-EXE/CUE/BIN production, streamed pack layout, relocation, deterministic input tapes, display/VRAM hashes and real-hardware probes |

The shared interfaces are deliberately small. Quake keeps Quake gameplay;
HL-PSX keeps GoldSrc-specific entities and formats; Cortex uses the authored
PSoXide scene model. Common PS1 work moves down into the SDK or engine only
after its performance and visual behaviour can be checked in every affected
game.

## Current state

The stack can author a project, cook its assets, build an optimized MIPS
executable, master a CUE/BIN image, boot it inside the editor, inspect it in the
emulator and run the same image on original hardware.

Current proof points include:

- a versioned Cortex Ignition tech-demo candidate with an authored BSP level,
  animated player, enemy and boss;
- the complete Quake shareware episode cooking and running from original data;
- the full Half-Life campaign asset set cooking from a user's own installation,
  with deterministic train-ride and gameplay regressions;
- CD-streamed worlds, models, UI and audio under fixed PS1 RAM/VRAM budgets;
- shared low-level rendering and clipping work measured across Cortex, Quake
  and Half-Life rather than against a synthetic benchmark alone;
- standalone discs and a chain-loading multi-game demo disc.

The remaining work is mostly hardening rather than proving the concept:

- original-hardware performance and compatibility passes continue across the
  games;
- the editor is usable for full BSP levels but remains pre-1.0;
- emulator compatibility outside the tested homebrew and game routes is not
  complete;
- native desktop release packaging and broader CI coverage still need work.

## Quick start

The repository pins its nightly Rust toolchain in `rust-toolchain.toml`. On
macOS and Linux the top-level `Makefile` is the shortest route; Windows host
builds can use Cargo directly.

```bash
git clone https://github.com/EBonura/PSoXide.git psoxide
cd psoxide

make check && make test                # host, engine and SDK checks
make hello-tri-disc && make run-tri    # build and boot a minimal PS1 disc
make run                               # launch the frontend/editor
make run-release                       # fully optimized frontend
```

Open **Cortex Ignition Tech Demo 0.4b** from the editor project browser and use
**Play** to cook, compile, master and boot it in the embedded viewport. For a
small starting point, use **File → New Project** instead of copying the demo.

If you are starting a separate game repository, read
[Downstream game projects](docs/downstream-projects.md) before copying a
sibling's build setup. It documents the supported pinned and local-development
modes, standard build verbs and the SDK functionality that should not be
reimplemented per game.

<details>
<summary><strong>Platform prerequisites</strong></summary>

### macOS

```bash
xcode-select --install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### Debian / Ubuntu

```bash
sudo apt install build-essential make pkg-config libasound2-dev libudev-dev \
  libx11-dev libxi-dev libxrandr-dev libxinerama-dev libxcursor-dev \
  libxkbcommon-dev libwayland-dev mesa-vulkan-drivers
```

### Windows

```powershell
winget install Rustlang.Rustup
winget install Microsoft.VisualStudio.2022.BuildTools --override "--add Microsoft.VisualStudio.Workload.VCTools --includeRecommended --passive --wait"
rustup show
cargo check --workspace --all-features
cargo run -p frontend
```

The pinned nightly installs `rustfmt`, `clippy`, `rust-src` and `llvm-tools`.
The bare-metal target is built with `-Zbuild-std`; no precompiled standard
library exists for `mipsel-sony-psx`.
</details>

<details>
<summary><strong>Useful Make targets</strong></summary>

```bash
make check                        # compile the workspace
make test                         # run the test suite
make fmt                          # format the Rust sources
make lint                         # run the configured lints
make examples                     # build every SDK/engine/game example disc
make <name>-disc                  # build one example disc
make run-<name>                   # boot one example
make validate                     # exact display/VRAM regression matrix
```

The `Makefile` is the source of truth for Unix-like development hosts. Each
major area has its own README with narrower commands and architecture notes.
</details>

## Validation on emulators and hardware

PSoXide's emulator is both a product and part of the test rig. Downstream games
can record deterministic input, replay it against a fixed executable, collect
per-stage cycle counts, compare display and VRAM hashes, and then run the exact
same CUE/BIN on a console.

Some of the hardware-backed results already encoded as regressions are:

- **GTE: 1100/1100 bit-exact cases** against JaCzekanski's real-console
  `ps1-tests` corpus;
- **triangle coverage measured on silicon**, including the edge rule used by
  the software and host preview rasterizers;
- **GTE pipeline hazards measured on hardware**, including the load delay and
  mid-command register behaviour that originally exposed skinned-mesh
  corruption;
- **disc, DMA, SPU and controller timing routes** exercised by standalone
  programs and by relocated programs on the demo disc.

Hardware evidence is kept useful: a finding becomes a unit test, a deterministic
route, a captured hash contract, or a small console probe that can be rerun.

## Editor and runtime inspection

The editor has orthographic and free-fly 3D navigation, BSP brush/face/edge/
vertex editing, grid snapping, grouping and visibility controls, material and
UV editing, leak diagnostics, scene/UI authoring, model animation preview and
an embedded live playtest.

For deterministic UI captures, use the headless offscreen path:

```bash
make editor-ui-screenshot \
  EDITOR_UI_PROJECT=editor/projects/default \
  EDITOR_UI_VIEW=room \
  EDITOR_UI_OUT=/tmp/psoxide-editor.png
```

The emulator's free camera detaches inspection from the guest camera:

| Control | Action |
| --- | --- |
| Toolbar **EYE**, or tap **L3+R3** | Toggle free camera |
| Left stick / right stick | Move / look |
| **R2** | Move faster |
| Hold **L3+R3** | Reset to the guest camera |

While free camera is active, controller input is withheld from the guest so an
inspection pass cannot accidentally change the recorded game route.

## Agent control (MCP)

The native frontend can expose the running emulator over MCP. This is optional
and binds only to localhost by default:

```bash
cargo run -p frontend --features mcp
```

Connect to `http://127.0.0.1:7355/mcp`. The available operations load or reset
a disc, pause/resume/step frames, capture the display or VRAM, inspect or write
RAM, toggle wireframe and query execution status. Because frame stepping works
while paused, an automated investigation can reproduce the same frame and
inspect the state that produced it instead of reasoning from screenshots alone.

## Repository map

| Area | Contents |
| --- | --- |
| [`emu/`](emu/README.md) | Emulator core, desktop frontend, host renderer and validation |
| [`sdk/`](sdk/README.md) | Bare-metal PS1 SDK crates and `hello-*` examples |
| [`engine/`](engine/README.md) | Shared scene/runtime engine, BSP, gameplay modules and examples |
| [`editor/`](editor/README.md) | Project model, BSP/UI authoring, asset import and cook pipeline |
| [`crates/`](crates/README.md) | Shared `no_std` formats and PS1 primitives |
| [`tools/`](tools/README.md) | Disc mastering, executable and development utilities |
| [`docs/`](docs/README.md) | Architecture, downstream integration, hardware notes and validation |

## Examples

The repository includes small programs that double as documentation and
regression fixtures: bare-metal `hello-*` discs, geometry/lighting/fog/model/
particle showcases, hardware tests, and small games including Pong, Breakout,
Space Invaders and Magikarp Pong.

Build everything with `make examples`. The same examples are embedded under
**Examples** in the desktop and web frontends, so a new checkout has useful PS1
software to run before any external game data is supplied.

| ![3D showcase](assets/media/readme/examples/showcase-3d.png) | ![Fog showcase](assets/media/readme/examples/showcase-fog.png) | ![Model showcase](assets/media/readme/examples/showcase-model.png) |
| --- | --- | --- |

## BIOS and game data

Homebrew examples and editor Play use PSoXide's HLE BIOS path and do not need a
retail BIOS. Retail-disc compatibility testing requires a BIOS dumped from a
console you own; select it in the frontend or set `PSOXIDE_BIOS`.

PSoXide does not supply commercial game data. Quake PSX obtains and verifies
the freely distributed Quake 1.06 shareware data during its local build.
HL-PSX reads assets from the user's own lawful Half-Life installation and does
not distribute Valve's maps, models, textures or audio.

## Provenance and license

PSoXide is **GPL-2.0-or-later**. It began with substantial PCSX-Redux-derived
work and has since replaced or independently reworked many subsystems using
hardware documentation, focused console probes and parity testing. Files that
retain derived work carry provenance headers; this project does not make a
clean-room claim.

Development has also used substantial AI assistance under human direction and
hardware verification. That assistance does not replace the repository's
source provenance, licensing obligations, tests, or the requirement to verify
claims on the target hardware.

Games distributed with PSoXide SDK or engine code must satisfy the applicable
GPL obligations for that covered code. A game's original art, levels, models,
music and other content remain the creator's. Commercial homebrew is allowed;
the licence requires the covered source to remain available under compatible
terms, not that original game assets become GPL.

Before distributing a game, read
[Downstream licensing](docs/downstream-licensing.md), the
[licence audit](docs/license-audit.md), [asset provenance](docs/asset-provenance.md)
and [`LICENSE`](LICENSE).
