+++
title = "psx-gpu"
description = "Drawing, command submission and framebuffers"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-gpu` to initialize the display, send drawing commands and manage framebuffers. A minimal frame clears the back buffer, emits primitives, waits for drawing and VBlank, then swaps buffers.

## How the crate is organized

The root exposes display modes, drawing helpers and synchronization. `framebuf::FrameBuffer` manages the two framebuffers. `prim` describes primitive packets, `ot` builds depth-ordered DMA lists, and `ordered` preserves the caller's command order in a bounded command stream. `material` groups texture page, CLUT, tint and blend state.

## Integration notes

Pick submission order deliberately. An ordering table is useful when geometry needs depth ordering; an ordered stream suits commands already in painter order. DMA reads the command storage asynchronously, so keep it alive until completion. `OrderedCommandStream` uses caller-provided static storage and an explicit fence before reuse or separate VRAM work. Drawing completion and VBlank are different events: queued presentation waits for the frame's closing draw-done command.

Complete examples: [triangle](@/docs/examples/hello-tri.md), [ordering table](@/docs/examples/hello-ot.md) and [queued presentation](@/docs/examples/hello-present.md).

## API, dependencies and source structure

{{<sdk_crate name="psx-gpu" />}}
