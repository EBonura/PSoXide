+++
title = "psx-gpu"
description = "Drawing, command submission and framebuffers"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-gpu` to initialize the display, send drawing commands and manage framebuffers. A minimal frame clears the back buffer, emits primitives, waits for drawing and VBlank, then swaps buffers.

## How the crate is organized

`Gpu` is the handle for the GPU: it takes the GPU's DMA token from `psx_rt::Peripherals`, so one owner at a time drives GP0, and the drawing, draw-area and synchronization methods hang off it. `display` holds `DisplayConfig`, the video mode and resolution, and `DoubleBuffer`, whose methods take the `Gpu` to clear and swap the two framebuffers. `prim` describes primitive packets, `ot` builds depth-ordered DMA lists and `frame` wraps one frame of them (`OtFrame`, `PrimitiveArena`, `FrameStorage`) so the borrow checker tracks packet lifetimes. `ordered` preserves the caller's command order in a bounded command stream, and `chain` builds static DMA chains. `material` groups texture page, CLUT, tint and blend state. The older free functions and `framebuf::FrameBuffer` remain only as deprecated forwarders.

## Integration notes

Choose how to submit commands. An ordering table is useful when geometry needs depth ordering; an ordered stream suits commands already in painter order. DMA reads the command storage asynchronously, so keep it alive until completion. `OrderedCommandStream` uses caller-provided static storage and an explicit fence before reuse or separate VRAM work. Drawing completion and VBlank are different events: queued presentation waits for the frame's closing draw-done command.

Complete examples: [triangle](@/docs/examples/hello-tri.md), [ordering table](@/docs/examples/hello-ot.md), [queued presentation](@/docs/examples/hello-present.md) and the [VBlank-kicked present queue](@/docs/examples/hello-present-queue.md).

## API, dependencies and source structure

{{<sdk_crate name="psx-gpu" />}}
