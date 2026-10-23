+++
title = "hello-overlay"
description = "Loading and unloading a code module in a borrowed arena"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Load a code module into a workspace the game already owns, call into it, unload it and admit it again. The module's private Rust state, a typed caller in the resident code, a cold entry and a resident floating-point service are compiled in one compiler invocation. The program checks a first call (count 7, value 3.25), the trap on return, unload, an overwrite of the arena by gameplay, re-admission into the same slot, rejection of the old handle, and a second call (count 12, value 5.125). The overlay package is a completed-transport fixture, so the example proves neither CD reads nor memory recovered. See [psx-residency](@/docs/crates/psx-residency.md) for the checked reader and arena API.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

This example has its own Makefile. From `sdk/examples/hello-overlay`, with the repository's pinned toolchain:

```sh
make OUT=/absolute/private/output
```

The recipe leaves the `.overlay` section out of the resident executable and traps its typed call site until admission. Run the produced CUE on a headless frontend and require `CALLER ARENA OVERLAY PASS` with both values above and zero MIPS hazards. That is emulator qualification, not original hardware qualification. The example's README describes the packer, which is specific to this sample, and the generic `overlay-compose` recipe that games use.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-overlay" />}}
