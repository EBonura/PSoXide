+++
title = "hello-memprobe"
description = "Memory routine size and alignment checks"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Compare the runtime memory routines against reference loops across sizes and alignments. The program reports MEMPROBE PASS or FAIL on screen and through debug output. This is a diagnostic rather than a game-loop template.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

```sh
make disc EXAMPLE=hello-memprobe
```

Open `build/examples/mipsel-sony-psx/release/hello-memprobe.cue` in the emulator with its sibling BIN in the same directory. The [first-program walkthrough](@/docs/first-ps1-program.md#4-get-the-emulator) explains installation and loading.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-memprobe" />}}
