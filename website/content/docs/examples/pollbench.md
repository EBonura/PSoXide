+++
title = "pollbench"
description = "Cycle cost of one controller poll"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Measure what one controller poll costs. The program polls port 1, then port 2, then both back to back, many times each, and prints the cycles per poll for every phase. A game that polls both ports every frame pays the third figure per frame. Root counter 2 runs at the system clock divided by eight and is read around each poll. An empty port 2 is the headless default, and the cost of an absent socket is the figure this program exists to watch.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

```sh
make disc EXAMPLE=pollbench
```

Open `build/examples/mipsel-sony-psx/release/pollbench.cue` in the emulator with its sibling BIN in the same directory. The [first-program walkthrough](@/docs/first-ps1-program.md#4-get-the-emulator) explains installation and loading. Run the executable on the headless frontend with `--steps 400000000` and read the `pollbench:` lines on the TTY.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="pollbench" />}}
