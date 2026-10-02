+++
title = "PSoXide SDK documentation"
description = "21 crate guides, searchable Rust APIs and 16 complete example programs for building PS1 homebrew."
weight = 3
[extra]
kind = "Reference"
eyebrow = "Reference · SDK"
+++

The SDK is a set of small Rust crates for the PlayStation's hardware. There's no umbrella crate and no framework on top: a program depends on the subsystems it uses and calls them directly. Everything here targets `mipsel-sony-psx` and runs without an operating system. The engine and editor, which add scenes and an asset pipeline, live in the [editor repository](https://github.com/EBonura/PSoXide-editor).

Start with the [first-program walkthrough](@/docs/first-ps1-program.md),
[run the browser examples](@/docs/browser-examples.md), or browse the
[complete example sources](@/docs/examples/_index.md). Every crate below has its
own guide, dependency list, feature flags, source layout and API reference.

## How the SDK fits together

The crates form a few practical layers. Choose the pieces your program needs:

<div class="table-wrap" role="region" aria-label="SDK subsystem map" tabindex="0">

| Work | Start with | Supporting layer |
|---|---|---|
| Boot, panic, interrupts and frame timing | [psx-rt](@/docs/crates/psx-rt.md) | [psx-io](@/docs/crates/psx-io.md) for hardware access, [psx-tick](@/docs/crates/psx-tick.md) for fixed-rate game logic |
| Draw primitives and submit a frame | [psx-gpu](@/docs/crates/psx-gpu.md) | [psx-vram](@/docs/crates/psx-vram.md) for texture and palette layout |
| Transform and project geometry | [psx-gte](@/docs/crates/psx-gte.md) | [psx-gte-core](@/docs/crates/psx-gte-core.md), [psx-math](@/docs/crates/psx-math.md) |
| Read controls and build a UI | [psx-pad](@/docs/crates/psx-pad.md), [psx-font](@/docs/crates/psx-font.md) | [psx-osk](@/docs/crates/psx-osk.md), [psx-settings](@/docs/crates/psx-settings.md) |
| Play sounds and effects | [psx-spu](@/docs/crates/psx-spu.md), [psx-sfx](@/docs/crates/psx-sfx.md) | [psx-asset](@/docs/crates/psx-asset.md) for cooked audio |
| Load and manage assets | [psx-asset](@/docs/crates/psx-asset.md), [psx-pack](@/docs/crates/psx-pack.md) | [psx-cache](@/docs/crates/psx-cache.md) for residency |
| Save player data | [psx-mc](@/docs/crates/psx-mc.md) | [psx-settings](@/docs/crates/psx-settings.md) for preferences |
| Add visual effects or video | [psx-fx](@/docs/crates/psx-fx.md), [psx-fmv](@/docs/crates/psx-fmv.md) | GPU, SPU and CD services |
| Instrument a program | [psx-telemetry](@/docs/crates/psx-telemetry.md) | Emulator profiling support |

</div>

`psx-hw` and `psxed-format` are shared dependencies in the repository's root
`crates/` directory. Keep the repository layout intact: `sdk/` alone is not a
self-contained copy of all dependencies. The engine's scene and application
framework is a separate layer and is not required for these examples.

## Build the documented revision

The crate structures, full example listings and API reference use the same SDK
revision as the embedded players: `74a7b48cfbc58581d3becbf378a5a7ac06587d90`.
After [installing the tools](@/docs/first-ps1-program.md#1-install-the-tools):

```sh
git clone https://github.com/EBonura/PSoXide.git
cd PSoXide
git checkout 74a7b48cfbc58581d3becbf378a5a7ac06587d90
make disc EXAMPLE=hello-tri
```

The output is `build/examples/mipsel-sony-psx/release/hello-tri.exe`, plus a BIN
and CUE. Open the CUE to load the disc image. The build also runs the instruction
hazard and scratchpad-stack checks; retain those steps when adapting the build.

## Project structure

The small examples live in their own Cargo workspaces. Start by reading the
[complete hello-tri manifest and source](@/docs/examples/hello-tri.md#complete-source):

```text
PSoXide/
  rust-toolchain.toml         pinned compiler and components
  crates/                    shared formats and host tools
  sdk/
    Cargo.toml               workspace containing the 21 SDK crates
    psoxide.ld               target linker script
    crates/<crate>/src/      subsystem implementations
    examples/hello-tri/
      Cargo.toml             standalone example workspace and dependencies
      Cargo.lock             resolved dependencies
      src/main.rs            no_std/no_main entry and frame loop
  tools/sdk-examples.mk      compile, check and disc-mastering rules
```

The example manifest selects subsystem dependencies by path. Its `main.rs`
links `psx-rt`, initializes the hardware it uses and runs a frame loop. Assets
embedded with `include_bytes!` must be available at the referenced paths when
compiling. Other examples keep data on the disc and load it at runtime; an EXE
download alone does not contain those files or CD audio tracks.

For a separate game repository, use the repository's
[pinned component bootstrap workflow](https://github.com/EBonura/PSoXide/blob/74a7b48cfbc58581d3becbf378a5a7ac06587d90/README.md)
to retain the shared dependencies and matching SDK revision.

## API reference

Each crate guide links to its searchable Rust API: public modules, types,
functions, signatures, safety contracts, examples and source. The primary
reference is built for **`mipsel-sony-psx` with all Cargo features enabled**, so
PS1-only APIs such as the runtime BIOS calls and MDEC driver are included.
Check each crate's feature list before using an optional API in your program.

`psx-gte` and `psx-gte-core` also have a **host API** reference, because their
software simulation backend is not part of a PS1 build. API search is available
on every generated reference page. Use the PSoXide link above that reference to
return to the guides.

The inventory tables follow the SDK README; the detailed structures and complete
source listings are checked against the pinned code during publishing.

## Device crates

{{<sdk_index what="crates" />}}

## Examples

[Run six examples in your browser](@/docs/browser-examples.md), with how-tos for input, textures, drawing order, 3D and sound.

Each example is its own small workspace. Build any of them into a disc image with `make disc EXAMPLE=<name>`, as in the [first-program walkthrough](@/docs/first-ps1-program.md). The first six are the ones to read when learning. The rest are console tests: they exist to check a specific piece of the runtime or the hardware and print a result, so they're more useful as references than as starting points. [Checking code against PlayStation hardware](@/docs/hardware-checks.md) explains what several of them found. `hello-cdda`, `cdda-read-contention`, `hello-fmv` and `hello-pack` need more on the disc than the program (CD audio tracks, a movie or a `WORLD.PAK`), which the generic `disc` target doesn't add.

{{<sdk_index what="examples" />}}

## Host tools

The same repository holds the tools that run on your computer rather than on the console:

- [`mkisopsx`](https://github.com/EBonura/PSoXide/tree/main/tools/mkisopsx) masters a BIN/CUE disc image, with optional CD audio tracks and a `WORLD.PAK` of streamed data.
- [`psx-audio-cook`](https://github.com/EBonura/PSoXide/tree/main/crates/psx-audio-cook) converts audio to the SPU's ADPCM format. It resamples, encodes against an exact model of the SPU decoder, keeps loops seamless, and can share out a fixed amount of sound RAM between samples.
- [`psx-anim-cook`](https://github.com/EBonura/PSoXide/tree/main/crates/psx-anim-cook) encodes skeletal animation for the engine's model format.
- [`psoxide-pgo`](https://github.com/EBonura/PSoXide/tree/main/tools/psoxide-pgo) turns an emulator profile of a game into a profile-guided optimisation build.
- [`psoxide-hazard`](https://github.com/EBonura/PSoXide/tree/main/tools/psoxide-hazard) (`hazard-patch`, `hazard-scan`, `stack-guard`) checks every linked program for instruction-ordering hazards and stack overflows before it reaches a disc.

{% <callout kind="warn" title="Pre-1.0"> %}
Crates, formats and APIs still change. Games pin an exact SDK revision, and so should yours.
{% </callout> %}
