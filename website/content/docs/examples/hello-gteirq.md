+++
title = "hello-gteirq"
description = "Geometry commands interrupted by VBlank"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Exercise RTPS while VBlank interrupts arrive and check that the runtime exception path does not execute a geometry command twice. This is a runtime regression probe; a passing emulator run alone is not a new hardware conformance result.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

```sh
make disc EXAMPLE=hello-gteirq
```

Open `build/examples/mipsel-sony-psx/release/hello-gteirq.cue` in the emulator with its sibling BIN in the same directory. The [first-program walkthrough](@/docs/first-ps1-program.md#4-get-the-emulator) explains installation and loading.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-gteirq" />}}
