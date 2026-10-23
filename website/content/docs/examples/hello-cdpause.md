+++
title = "hello-cdpause"
description = "The drive's two-answer Pause against the emulator model"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Play the console's Pause command against the emulator's model of it. Pause answers twice, and on the console the second answer, which says the drive has stopped, arrives 1.2 to 3.9 million system clocks after the first. The program checks four things: `stop()` returns at the first response with the second still to come; `stop_proved()` returns only when the drive says it has stopped; a Pause sent while the read is still seeking is refused and `stop_proved()` waits the seek out and sends it again; and a read started after any of these still delivers its sector. The disc carries `CDTEST.BIN`, which the program finds by name. The last TTY line is `hello-cdpause: ALL PASS` or `FAIL`.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

```sh
make hello-cdpause-disc
```

This masters the image with `mkisopsx --cdtest-sectors 960`. Open `build/examples/mipsel-sony-psx/release/hello-cdpause.cue` in the emulator with its sibling BIN in the same directory. The [first-program walkthrough](@/docs/first-ps1-program.md#4-get-the-emulator) explains installation and loading.

`make hello-cdpause-gate FRONTEND=/path/to/frontend` runs the disc headless and checks the verdict.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-cdpause" />}}
