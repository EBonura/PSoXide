+++
title = "pad-engine-check"
description = "Interrupt-driven pad engine against the synchronous driver"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Measure the interrupt-driven pad engine against the synchronous driver on the same frames. Each phase lasts 120 frames, and the main loop does one unit of work per pass until the next VBlank, so the passes per frame show how much CPU the game keeps. The phases are: nothing polls, which is the ceiling; the synchronous driver polls both ports once per frame; and the engine polls in each of its configurations while the loop reads its snapshot. The program then exercises the lease, which is a memory card's turn on the port, and reports the engine's counters. Every result is a `padcheck:` line on the TTY.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

```sh
make disc EXAMPLE=pad-engine-check
```

Open `build/examples/mipsel-sony-psx/release/pad-engine-check.cue` in the emulator with its sibling BIN in the same directory. The [first-program walkthrough](@/docs/first-ps1-program.md#4-get-the-emulator) explains installation and loading. Run the executable on the headless frontend and read the `padcheck:` lines. A press on pad 1, sent with `--pad-pulses`, shows up as a `buttons=` line in the third phase.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="pad-engine-check" />}}
