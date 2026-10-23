+++
title = "hello-flipcap"
description = "The presentation cap on display flips"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Check the presentation cap: no display flip comes sooner than two VBlanks after the one before, which is 30 frames per second on NTSC, unless the game opts out by name. Every phase presents as fast as the CPU allows, so each would flip on every VBlank without the cap. The first phase queues tiny frames with `queue_display_control_at_vblank`, the path the decoupled-present games use, and the handler holds each flip until the cap allows it. The second calls `DoubleBuffer::swap` in a loop with no VBlank wait of its own, and the swap waits for its slot. The third sets the minimum flip interval to `PresentCap::UNCAPPED` and runs the queued loop again, which flips on every VBlank.

After each phase the program reads the flip-interval histogram from `interrupts::flip_histogram`. The two capped phases must hold no interval under 2 and run at about one flip per two VBlanks, and the opt-out must reach intervals of 1. The verdict is `FLIPCAP PASS` or `FLIPCAP FAIL` on the TTY and the screen.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

```sh
make hello-flipcap-disc
```

Open `build/examples/mipsel-sony-psx/release/hello-flipcap.cue` in the emulator with its sibling BIN in the same directory. The [first-program walkthrough](@/docs/first-ps1-program.md#4-get-the-emulator) explains installation and loading.

`make hello-flipcap-gate FRONTEND=/path/to/frontend` runs the same disc headless and checks the verdict.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-flipcap" />}}
