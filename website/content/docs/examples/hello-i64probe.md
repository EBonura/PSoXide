+++
title = "hello-i64probe"
description = "Software 64-bit arithmetic checks"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Exercise multiplication, division and remainder on the target and compare the answers with expected results. This covers the runtime replacements for signed division and remainder as well as compiler-generated arithmetic calls.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

```sh
make disc EXAMPLE=hello-i64probe
```

Open `build/examples/mipsel-sony-psx/release/hello-i64probe.cue` in the emulator with its sibling BIN in the same directory. The [first-program walkthrough](@/docs/first-ps1-program.md#4-get-the-emulator) explains installation and loading.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-i64probe" />}}
