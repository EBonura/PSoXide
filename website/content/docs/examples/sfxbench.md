+++
title = "sfxbench"
description = "Cycle cost of the psx-sfx bookkeeping"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Measure what [psx-sfx](@/docs/crates/psx-sfx.md) costs on the CPU. The program times the per-emitter placement chain, the mixer's per-voice level update, its idle tick and a stealing `play`, each over many calls against a hardware stand-in that does nothing, and prints cycles per call. Root counter 2 runs at the system clock divided by eight and is read around each batch, so the numbers come from the program itself and need no profiler. An empty batch with the same input shuffling is measured first and subtracted. SPU register writes are not part of the figures; the real per-voice cost is these numbers plus the SPU writes.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

```sh
make disc EXAMPLE=sfxbench
```

Open `build/examples/mipsel-sony-psx/release/sfxbench.cue` in the emulator with its sibling BIN in the same directory. The [first-program walkthrough](@/docs/first-ps1-program.md#4-get-the-emulator) explains installation and loading. The program draws nothing. Run it on the headless frontend with `frontend launch --path build/examples/mipsel-sony-psx/release/sfxbench.exe --steps 400000000` and read the `sfxbench:` lines it prints to the TTY.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="sfxbench" />}}
