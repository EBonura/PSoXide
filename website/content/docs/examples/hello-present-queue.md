+++
title = "hello-present-queue"
description = "Whole frames through the present queue"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Publish whole frames through psx-rt's VBlank-kicked present queue (the `present-queue` feature). Each frame is one chain: a recorded preamble (draw target and clear), the ordering table, a recorded HUD band, then GP0(1Fh). The CPU never kicks or flips: the VBlank handler shows the previous frame and kicks the next chain on the first edge that finds the one before it drawn. Everything a chain links is double buffered, and `wait_arena_free` keeps the CPU off the side a walk may still read.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

```sh
make disc EXAMPLE=hello-present-queue
```

Open `build/examples/mipsel-sony-psx/release/hello-present-queue.cue` in the emulator with its sibling BIN in the same directory. The [first-program walkthrough](@/docs/first-ps1-program.md#4-get-the-emulator) explains installation and loading.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-present-queue" />}}
