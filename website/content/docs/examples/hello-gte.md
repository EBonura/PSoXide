+++
title = "hello-gte"
description = "A rotating wireframe cube"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Define eight vertices and twelve edges, load the GTE rotation and translation state, project the vertices and draw the connecting lines. This demonstrates projection and drawing; a solid mesh renderer would also need clipping and face handling.

[Read the step-by-step how-to](@/docs/project-3d.md).

{{<example_player name="hello-gte" />}}

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

```sh
make disc EXAMPLE=hello-gte
```

Open `build/examples/mipsel-sony-psx/release/hello-gte.cue` in the emulator with its sibling BIN in the same directory. The [first-program walkthrough](@/docs/first-ps1-program.md#4-get-the-emulator) explains installation and loading.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-gte" />}}
