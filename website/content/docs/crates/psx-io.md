+++
title = "psx-io"
description = "Hardware registers and CD commands"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-io` when you need to control a peripheral directly or write a subsystem driver. Most game code can start with `psx-gpu`, `psx-spu` and `psx-pad` instead.

## How the crate is organized

The root contains volatile reads and writes at 8-, 16- and 32-bit widths. Peripheral modules cover GPU, SPU, GTE-related registers, SIO, DMA, interrupts and timers. `cdrom` provides the CD command and sector surface; `cdda` adds paced CD-audio control; `disc_base` handles disc-base information.

## Integration notes

A volatile access is not a complete device protocol. The caller must use a valid mapped address and width, observe readiness and acknowledgements, and coordinate ownership of DMA channels and interrupts. These functions can compile for a host without being safe to execute there: they still address PS1 hardware.

Read [CD audio](@/docs/examples/hello-cdda.md) for a real caller and [CD command contention](@/docs/examples/cdda-read-contention.md) for a diagnostic that deliberately exercises an edge case.

## API, dependencies and source structure

{{<sdk_crate name="psx-io" />}}
