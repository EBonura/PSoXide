+++
title = "hello-memcard"
description = "Guarded memory-card diagnostic"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Read and hash all 1024 card frames before enabling a write. Holding L1 and R1 while pressing Cross performs another identity scan, creates a reserved one-block test save and verifies it. This does write persistent card data when requested. It does not format the card or overwrite an existing file; use a test card or an emulator card image when learning.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

```sh
make disc EXAMPLE=hello-memcard
```

Open `build/examples/mipsel-sony-psx/release/hello-memcard.cue` in the emulator with its sibling BIN in the same directory. The [first-program walkthrough](@/docs/first-ps1-program.md#4-get-the-emulator) explains installation and loading.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-memcard" />}}
