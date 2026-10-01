+++
title = "psx-font"
description = "Bitmap text and font atlases"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-font` to put text on screen through textured GPU primitives. Pick a bitmap font, upload its atlas and palette once, and draw with the returned `FontAtlas`.

## How the crate is organized

The root defines font descriptors, atlas upload, layout and drawing methods. `fonts` contains the supplied bitmap fonts; `hex` handles hexadecimal text helpers. Font source files and vendor provenance remain linked from the source tree.

## Integration notes

`draw_text` uses textured rectangles for axis-aligned text. Scaled, rotated, affine and gradient variants use different GPU packet paths. Pick the path that fits the effect, and reserve both atlas and CLUT space in your VRAM layout. The convenience upload has bounded stack scratch; use the caller-owned scratch API for larger atlases. Font assets carry their own licences, so check provenance before redistributing them.

The [hello-input source](@/docs/examples/hello-input.md) shows initialization and status text in a complete frame loop.

## API, dependencies and source structure

{{<sdk_crate name="psx-font" />}}
