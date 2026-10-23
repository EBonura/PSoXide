+++
title = "hello-wide-div"
description = "Division of wide integers, checked and timed"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Run every `psx_math::wide_div` operation, which divides a `Wide` or `WideU`, on the target against a bit-by-bit restoring division over 32-bit words. The program checks the answers on edge values and a fixed pseudo-random stream and reports `WIDEDIV PASS` or `WIDEDIV FAIL`. It shows that the operations link no compiler helper, using a link map and `tools/guest_symbol_gate.sh` with no opt-out. It also prints the cost of each operation as `widediv: <op> cycles_per_call=N`, read from root counter 2 with the input shuffling subtracted. Those figures are the ones the [psx-math](@/docs/crates/psx-math.md) README quotes.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

```sh
make hello-wide-gate FRONTEND=/path/to/frontend
```

The same gate covers [hello-wide](@/docs/examples/hello-wide.md) and this example. Without `FRONTEND` it runs only the link-map checks.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-wide-div" />}}
