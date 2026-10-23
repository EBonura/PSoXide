+++
title = "hello-fault"
description = "A deliberate panic for the abort gate"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Panic on purpose, so the panic gate has something to check. The example is built with `-Cpanic=immediate-abort`, the SDK's shared policy, so the out-of-range index in its source becomes a bare `break` at the faulting instruction. The runtime's exception handler records Cause, EPC and BadVAddr and halts, and `psoxide-size faults` reads them from a RAM dump and names the function around EPC. The program draws nothing.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

```sh
make disc EXAMPLE=hello-fault
```

The check is `sh tools/abort_gate.sh /path/to/frontend`. It runs this program and fails unless the fault was recorded, runs [hello-present](@/docs/examples/hello-present.md) and requires zero CPU exceptions, and compares the size of a build with and without panic messages.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-fault" />}}
