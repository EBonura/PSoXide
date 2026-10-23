+++
title = "hello-asmprobe"
description = "Hand-scheduled kernels against a plain Rust reference"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Compare the SDK's hand-scheduled kernels with a portable Rust reference. Every kernel runs on random and hand-picked edge inputs, from the same xorshift32 stream on every run, and must match the reference bit for bit. Each kernel is then timed on a fixed workload. The verdict is `ASMPROBE PASS` or `ASMPROBE FAIL` on the TTY, and per-function cycles come from the emulator's attribution logs over the link map. The source needs the nightly `asm_experimental_arch` feature, which the pinned toolchain provides.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

```sh
make disc EXAMPLE=hello-asmprobe
```

Open `build/examples/mipsel-sony-psx/release/hello-asmprobe.cue` in the emulator with its sibling BIN in the same directory. The [first-program walkthrough](@/docs/first-ps1-program.md#4-get-the-emulator) explains installation and loading. Run it on the headless frontend to read the verdict.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-asmprobe" />}}
