+++
title = "hello-wide"
description = "64-bit arithmetic without 64-bit types"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Run every operation of `psx_math::Wide`, `WideU` and `Mask64` on the target and compare it with a reference built from 32-bit arithmetic only. The program has two jobs. It checks the answers on the R3000 using edge values and a fixed pseudo-random stream, and reports `WIDE PASS` or `WIDE FAIL` on the TTY. It also shows that the operations link no compiler helper: the image is built with a link map, and `tools/guest_symbol_gate.sh` must pass on it, which rules out the 64-bit multiply, divide and shift helpers, the 128-bit helpers and soft-float. Nothing in the source names `i64` or `u64`, and operands pass through `black_box` so no operation is folded away.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

```sh
make hello-wide-gate FRONTEND=/path/to/frontend
```

The gate builds this example and [hello-wide-div](@/docs/examples/hello-wide-div.md) with link maps, fails on any forbidden helper symbol, then runs each image headless and requires its pass line. Without `FRONTEND` it runs only the link-map checks. It needs the pinned nightly toolchain and its `rust-src` component.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-wide" />}}
