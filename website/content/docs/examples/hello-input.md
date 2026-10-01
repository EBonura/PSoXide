+++
title = "hello-input"
description = "Digital and analog controller input"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Poll port 1 and display the controller state. The D-pad changes the background, while face buttons draw coloured triangles. Read the state display when checking digital versus analog mode; controller capabilities can differ.

[Read the step-by-step how-to](@/docs/controller-input.md).

{{<example_player name="hello-input" />}}

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

```sh
make disc EXAMPLE=hello-input
```

Open `build/examples/mipsel-sony-psx/release/hello-input.cue` in the emulator with its sibling BIN in the same directory. The [first-program walkthrough](@/docs/first-ps1-program.md#4-get-the-emulator) explains installation and loading.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-input" />}}
