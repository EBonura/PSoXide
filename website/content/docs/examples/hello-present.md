+++
title = "hello-present"
description = "Queued flips and GPU completion"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Queue display changes through the runtime and complete a frame with the GPU draw-done signal. Read the sequence around arm_draw_done, the closing command and the queued flip: a VBlank by itself does not prove the GPU finished drawing.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

```sh
make disc EXAMPLE=hello-present
```

Open `build/examples/mipsel-sony-psx/release/hello-present.cue` in the emulator with its sibling BIN in the same directory. The [first-program walkthrough](@/docs/first-ps1-program.md#4-get-the-emulator) explains installation and loading.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-present" />}}
