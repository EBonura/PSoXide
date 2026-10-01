+++
title = "hello-spstack"
description = "Scratchpad stack under interrupts"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Compare a deterministic call tree on the RAM stack and scratchpad stack while VBlank interrupts arrive. The diagnostic checks the result, surrounding data and nested calls, and reports SPSTACK PASS or FAIL. Read the manifest for the scratchpad-stack-check, panic-on-stack and chained-vector feature choices.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

```sh
make disc EXAMPLE=hello-spstack
```

Open `build/examples/mipsel-sony-psx/release/hello-spstack.cue` in the emulator with its sibling BIN in the same directory. The [first-program walkthrough](@/docs/first-ps1-program.md#4-get-the-emulator) explains installation and loading.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-spstack" />}}
