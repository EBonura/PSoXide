+++
title = "hello-cdstream"
description = "Interrupt-driven CD streaming checked byte for byte"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Stream a known file off the disc through [psx-cdstream](@/docs/crates/psx-cdstream.md) and compare every byte with the pattern the disc builder wrote. The disc carries `CDTEST.BIN`, the deterministic file `mkisopsx --cdtest-sectors` writes. The program finds it by name with the polled reader, hands the CD controller to the interrupt-driven transport, and runs eight checks: one read, two chained reads, priority order, an abort followed by a resume, a sustained stream through four buffers, the main loop's speed while a long read runs, an audio lease taken in the middle of a read, and a read the disc cannot satisfy followed by a good one.

The results are collected while the transport runs and printed afterwards, to the TTY and to the screen, because TTY output goes through the BIOS.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

```sh
make hello-cdstream-disc
```

Open `build/examples/mipsel-sony-psx/release/hello-cdstream.cue` in the emulator with its sibling BIN in the same directory. The [first-program walkthrough](@/docs/first-ps1-program.md#4-get-the-emulator) explains installation and loading. The screen turns green with `ALL PASS` when every check holds.

`make hello-cdstream-gate FRONTEND=/path/to/frontend` runs the same disc headless and prints the report.

## Emulator run

On the headless PSoXide frontend, all eight checks pass. A sustained stream of 640 sectors took 270 VBlanks, 142.2 sectors per second including the first seek and the program verifying each 32-sector buffer, with 19 of the 20 requests chained and no sectors dropped. While a 256-sector read ran, the foreground spin loop kept 85% of its idle speed; the longest interrupt-handler call was 41,576 cycles, about 1.2 ms for one sector. These are emulator numbers, not a console measurement.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-cdstream" />}}
