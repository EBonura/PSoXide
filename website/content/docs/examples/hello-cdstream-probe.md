+++
title = "hello-cdstream-probe"
description = "The console measurement disc for CD streaming: seek table, read cost, CD-DA next to data reads"
[extra]
kind = "Hardware probe"
eyebrow = "SDK example"
+++

## What this program does

Run a fixed sequence of drive measurements through [psx-cdstream](@/docs/crates/psx-cdstream.md), the production streaming transport, and show the results on the screen with no PC attached. It exists because the numbers a streaming design needs (seek times by distance, what a read costs the CPU, what CD-DA does next to a data read) are not the emulator's to give.

The sequence is a seek table (one sector at 1, 16, 128, 512, 2048 and 8192 sectors from the previous read, forward and back, eight repeats, min, median and max); the sustained read rate and the foreground CPU lost to the sector pops at double and at single speed, with the longest interrupt handler; the time a lease request takes to stop a read in flight and whether a Pause-parked motor winds down; CD-DA beside data reads (Pause-to-idle, time to the first data sector after audio, time to resume audio at its saved position, the recovery Pause with audio still playing, and a bare read over playing audio), with four questions the listener answers with X or O; what a Stop does to the next read; and the probe's own free RAM and stack depth.

Results appear as large text pages and as QR codes. Each QR carries a slice of one ASCII payload, `key=v,v;` records ending in a CRC-32, so a phone's reader returns text a person can read.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run this from the repository root.

```sh
make hello-cdstream-probe-disc
```

The disc carries `CDEXTRA.BIN`, 12 288 deterministic sectors for the seek table, and one CD-DA track, a rising scale of tones. Open `build/examples/mipsel-sony-psx/release/hello-cdstream-probe.cue` in the emulator, or burn it. `make hello-cdstream-probe-gate FRONTEND=/path/to/frontend` runs the sequence headless and checks that it reached its report with every read intact. An emulator run measures the emulator's model of the drive, not a console.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Source

`sdk/examples/hello-cdstream-probe` in the SDK repository.
