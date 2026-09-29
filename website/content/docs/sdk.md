+++
title = "What's in the SDK"
description = "Every device crate and example in the PSoXide SDK, and the host tools that build discs and cook assets."
weight = 3
[extra]
kind = "Reference"
eyebrow = "Reference · SDK"
+++

The SDK is a set of small Rust crates for the PlayStation's hardware. There's no umbrella crate and no framework on top: a program depends on the subsystems it uses and calls them directly. Everything here targets `mipsel-sony-psx` and runs without an operating system. The engine and editor, which add scenes and an asset pipeline, live in the [editor repository](https://github.com/EBonura/PSoXide-editor).

These tables are generated from the [SDK's README](https://github.com/EBonura/PSoXide/blob/main/sdk/README.md), which the SDK's own tests check against its crate and example directories.

## Device crates

{{<sdk_index what="crates" />}}

## Examples

Each example is its own small workspace. Build any of them into a disc image with `make disc EXAMPLE=<name>`, as in the [first-program walkthrough](@/docs/first-ps1-program.md). The first six are the ones to read when learning. The rest are console tests: they exist to check a specific piece of the runtime or the hardware and print a result, so they're more useful as references than as starting points. [Checking code against PlayStation hardware](@/docs/hardware-checks.md) explains what several of them found. `hello-cdda`, `cdda-read-contention`, `hello-fmv` and `hello-pack` need more on the disc than the program (CD audio tracks, a movie or a `WORLD.PAK`), which the generic `disc` target doesn't add.

{{<sdk_index what="examples" />}}

## Host tools

The same repository holds the tools that run on your computer rather than on the console:

- [`mkisopsx`](https://github.com/EBonura/PSoXide/tree/main/tools/mkisopsx) masters a BIN/CUE disc image, with optional CD audio tracks and a `WORLD.PAK` of streamed data.
- [`psx-audio-cook`](https://github.com/EBonura/PSoXide/tree/main/crates/psx-audio-cook) converts audio to the SPU's ADPCM format. It resamples, encodes against an exact model of the SPU decoder, keeps loops seamless, and can share out a fixed amount of sound RAM between samples.
- [`psx-anim-cook`](https://github.com/EBonura/PSoXide/tree/main/crates/psx-anim-cook) encodes skeletal animation for the engine's model format.
- [`psoxide-pgo`](https://github.com/EBonura/PSoXide/tree/main/tools/psoxide-pgo) turns an emulator profile of a game into a profile-guided optimisation build.
- `tools/hazard_patch.py`, `tools/hazard_scan.py` and `tools/stack_guard.py` check every linked program for instruction-ordering hazards and stack overflows before it reaches a disc.

{% <callout kind="warn" title="Pre-1.0"> %}
Crates, formats and APIs still change. Games pin an exact SDK revision, and so should yours.
{% </callout> %}
