+++
title = "hello-icache"
description = "Flushing the instruction cache"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Check that `psx_rt::cache::flush_instruction_cache` makes the CPU fetch code that was just rewritten in RAM. A three-word function in cached RAM returns a constant; the probe writes it, flushes and calls it, rewrites the constant with a plain store and calls it again without a flush, then flushes and calls once more. It prints `ICACHE PASS`, `INCONCLUSIVE` (nothing was stale) or `FAIL`.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

```sh
make disc EXAMPLE=hello-icache
```

Open `build/examples/mipsel-sony-psx/release/hello-icache.cue` in the emulator with its sibling BIN in the same directory. The [first-program walkthrough](@/docs/first-ps1-program.md#4-get-the-emulator) explains installation and loading.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-icache" />}}
