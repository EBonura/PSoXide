+++
title = "hello-diag"
description = "The console stop panel, triggered on purpose"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Show the stop panel that `psx_rt::diag` draws when a program halts. The program draws the [hello-tri](@/docs/examples/hello-tri.md) triangle for a while, then stops in the way the `HELLO_DIAG_MODE` build variable names: `stall` spins forever with interrupts on, `fault` executes a `BREAK`, `wild` jumps to an address no code lives at to show the panel's wild-PC row, `panic` raises a panic with a message, `ll` sends the GPU a linked-list node of 41 words, longer than the 16 the hardware is known to take, and `cd` asks the drive for a sector that is not there to put its error counters on the panel before stalling. With any other value it keeps drawing. The crate enables the runtime's `console-diag` feature.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

Pick a mode when building.

```sh
HELLO_DIAG_MODE=stall make disc EXAMPLE=hello-diag
```

Open `build/examples/mipsel-sony-psx/release/hello-diag.cue` in the emulator with its sibling BIN in the same directory. The [first-program walkthrough](@/docs/first-ps1-program.md#4-get-the-emulator) explains installation and loading. The panel takes over the screen after the triangle has been drawn for a while.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-diag" />}}
