+++
title = "hello-audio"
description = "Controller-triggered SPU samples"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Initialize the SPU, parse and upload cooked audio, then trigger voices from the controller. The sample blobs are included from assets/audio/freesfx/psau in the checkout. The full source includes sound timing and the on-screen labels.

[Read the step-by-step how-to](@/docs/sound-effects.md).

{{<example_player name="hello-audio" />}}

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

```sh
make disc EXAMPLE=hello-audio
```

Open `build/examples/mipsel-sony-psx/release/hello-audio.cue` in the emulator with its sibling BIN in the same directory. The [first-program walkthrough](@/docs/first-ps1-program.md#4-get-the-emulator) explains installation and loading.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-audio" />}}
