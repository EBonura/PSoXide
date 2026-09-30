+++
title = "hello-ot"
description = "Ordering-table submission"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Build an ordering table for three overlapping triangles. Their slots determine the drawing order. Read the primitive allocation and submission together: the GPU must finish reading command storage before it can be reused.

[Read the step-by-step how-to](@/docs/drawing-order.md).

{{<example_player name="hello-ot" />}}

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

```sh
make disc EXAMPLE=hello-ot
```

Open `build/examples/mipsel-sony-psx/release/hello-ot.cue` in the emulator with its sibling BIN in the same directory. The [first-program walkthrough](@/docs/first-ps1-program.md#4-get-the-emulator) explains installation and loading.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-ot" />}}
