+++
title = "psx-io"
description = "Hardware registers and CD commands"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-io` when you need to control a peripheral directly or write a subsystem driver. Most game code can start with `psx-gpu`, `psx-spu` and `psx-pad` instead.

## How the crate is organized

The root contains volatile reads and writes at 8-, 16- and 32-bit widths. Register addresses and bit layouts live in the shared `psx-hw` crate; the modules here wrap them for the GPU, DMA, interrupts, timers and the controller port. `periph` defines the zero-size ownership tokens (`GpuDma`, `Cd`, `ControllerPort`, `SpuDma`, `MdecDma`, `OrderingTableClearDma`) that `psx_rt::Peripherals::take()` hands out once. `cd` is the CD-ROM driver, with `cd::audio` for CD-DA, `cd::xa` for streaming XA-ADPCM music and `cd::reader` for polled data sectors; `disc_base` handles where a program's data sits on a shared disc. `cdrom` and `cdda` are the old names for `cd` and `cd::audio`, kept as deprecated forwarders.

## Integration notes

A volatile access is not a complete device protocol. The caller must use a valid mapped address and width, observe readiness and acknowledgements, and coordinate interrupts. DMA channels and the CD-ROM controller have owners, so an API that drives one takes its token by `&mut` and a second driver fails to compile. These functions can compile for a host without being safe to execute there: they still address PS1 hardware.

Read [CD audio](@/docs/examples/hello-cdda.md) and [XA music streaming](@/docs/examples/hello-xa.md) for real callers, and [CD command contention](@/docs/examples/cdda-read-contention.md) for a diagnostic that deliberately exercises an edge case.

## API, dependencies and source structure

{{<sdk_crate name="psx-io" />}}
