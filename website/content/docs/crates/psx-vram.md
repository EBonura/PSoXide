+++
title = "psx-vram"
description = "Texture pages, palettes and uploads"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-vram` to describe where textures and palettes live in the PS1's video memory. The important units are VRAM halfwords, texels at a chosen bit depth, and the packed handles placed in GPU commands.

## How the crate is organized

This crate has one source module. `Color555` represents packed colour, `VramRect` describes bounded VRAM rectangles, `Tpage` carries texture-page coordinates and depth, and `Clut` carries palette coordinates. Upload helpers move caller-owned data into those regions.

## Integration notes

A texture page has alignment requirements; a CLUT has its own. Typed constructors validate these rules, but the caller still owns the overall layout: texture data, fonts, palettes and framebuffers must not overlap. Indexed pixels are packed into halfwords before upload. Read each upload function's length and alignment contract; the byte-oriented helpers preserve an unaligned fallback.

Follow [textured sprites](@/docs/draw-textures.md), then read the complete [hello-tex program](@/docs/examples/hello-tex.md).

## API, dependencies and source structure

{{<sdk_crate name="psx-vram" />}}
