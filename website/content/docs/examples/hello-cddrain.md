+++
title = "hello-cddrain"
description = "Interrupt-popped sectors during a polled CD read"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Read a known file with the polled reader while the CD interrupt pops the sectors through `psx_cdstream::drain`, and check every byte. The disc carries `CDTEST.BIN`, the deterministic file `mkisopsx --cdtest-sectors` writes. Each check starts a read the way a polled-read program does, with `prepare` and `start_read`, hands the sectors to the interrupt with `drain::start`, spins on other work as a frame loop would, and ends the read with the reader's own `stop`. The checks are one read of 12 sectors with the foreground busy, a read whose last sector is only partly stored with the bytes past it intact, thirty reads back to back at spread offsets, where a dropped sector shows as a wrong byte, a read cancelled part-way followed by a good read, and a drain with no read behind it, which must end with the watchdog before a good read.

The program collects results during the run and prints them at the end, to the TTY and the screen. See [psx-cdstream](@/docs/crates/psx-cdstream.md) for the transport, and [hello-cdstream](@/docs/examples/hello-cdstream.md) for the interrupt-driven path.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

```sh
make hello-cddrain-disc
```

Open `build/examples/mipsel-sony-psx/release/hello-cddrain.cue` in the emulator with its sibling BIN in the same directory. The [first-program walkthrough](@/docs/first-ps1-program.md#4-get-the-emulator) explains installation and loading.

`make hello-cddrain-gate FRONTEND=/path/to/frontend` runs the same disc headless and prints the report.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-cddrain" />}}
