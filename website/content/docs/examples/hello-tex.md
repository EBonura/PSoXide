+++
title = "hello-tex"
description = "Cooked textures and indexed sprites"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Parse the two cooked texture blobs, upload their packed pixels and palettes, then draw sprites with the matching texture page and CLUT. The source shows where the repository assets are included at compile time. Keep those relative paths intact when building.

[Read the step-by-step how-to](@/docs/draw-textures.md).

{{<example_player name="hello-tex" />}}

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

```sh
make disc EXAMPLE=hello-tex
```

Open `build/examples/mipsel-sony-psx/release/hello-tex.cue` in the emulator with its sibling BIN in the same directory. The [first-program walkthrough](@/docs/first-ps1-program.md#4-get-the-emulator) explains installation and loading.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-tex" />}}
