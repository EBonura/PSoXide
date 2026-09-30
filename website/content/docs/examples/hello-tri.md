+++
title = "hello-tri"
description = "A complete triangle and frame loop"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Initialize the GPU, create two framebuffers, clear the back buffer and draw a shaded triangle. The vertex positions change each frame. Wait for drawing to finish and for VBlank before swapping buffers.

[Read the step-by-step how-to](@/docs/first-ps1-program.md).

{{<example_player name="hello-tri" />}}

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

```sh
make disc EXAMPLE=hello-tri
```

Open `build/examples/mipsel-sony-psx/release/hello-tri.cue` in the emulator with its sibling BIN in the same directory. The [first-program walkthrough](@/docs/first-ps1-program.md#4-get-the-emulator) explains installation and loading.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-tri" />}}
