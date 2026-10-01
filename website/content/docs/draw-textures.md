+++
title = "Draw textured sprites"
description = "Upload a texture and palette to VRAM, draw a sprite, and avoid overlapping the display buffers."
weight = 4
[extra]
kind = "How-to"
eyebrow = "How-to · SDK"
+++

{{<example_player name="hello-tex" />}}

## Build and try it

With the [SDK tools installed](@/docs/first-ps1-program.md#1-install-the-tools), run:

```sh
make disc EXAMPLE=hello-tex
```

Open `build/examples/mipsel-sony-psx/release/hello-tex.cue`. You should see two textured squares moving across a dark background. This example already includes cooked texture files, so you do not need the editor to build it.

## Load a cooked texture

The program embeds `.psxt` data into its executable with `include_bytes!`. At startup it parses the header and gets separate pixel and palette slices:

```rust
let brick = Texture::from_bytes(BRICK_BLOB).expect("brick.psxt");
```

Do this once before the render loop. Uploading the same pixels every frame wastes time and can overwrite memory the GPU is reading.

## Reserve video memory

The PS1's VRAM is 1024 by 512 16-bit words. This example reserves two 320 by 240 display buffers at `(0, 0)` and `(0, 240)`. Textures live elsewhere:

| Data | Location | Meaning |
|---|---|---|
| Shared texture page | `(640, 0)` | 4-bit indexed texture data |
| Brick palette | `(0, 480)` | 16 palette entries |
| Floor palette | `(0, 481)` | Another 16 entries |

A 64-pixel-wide 4-bit texture occupies **16 VRAM words per row**, because each 16-bit word holds four texels. Screen pixels, texture coordinates and VRAM words are not interchangeable units.

```rust
let rect = VramRect::new(
    SHARED_TPAGE.x(), SHARED_TPAGE.y(),
    brick.halfwords_per_row(), brick.height(),
);
upload_bytes(rect, brick.pixel_bytes());
upload_bytes(
    VramRect::new(BRICK_CLUT.x(), BRICK_CLUT.y(), brick.clut_entries(), 1),
    brick.clut_bytes(),
);
```

Keep your texture and palette rectangles clear of both display buffers and of each other. Texture-page X coordinates have alignment requirements; reuse the layout in the example until you need a larger atlas.

## Draw the sprite

The example's `draw_sprite` helper emits a textured rectangle packet. It takes a screen position, size, texture coordinates and palette reference. The brick begins at texture U=0; the floor begins at U=64 on the same page. Both are 64 by 64 texels, but they use different palettes.

`SHARED_TPAGE.apply_as_draw_mode()` selects the page used by those sprite packets. Each frame clears the back buffer, draws both rectangles, waits for drawing and vertical blank, then swaps the buffers.

## Try a change

Change the amplitudes passed to `drift` to make the sprites move farther or less far. To make a sprite stationary, replace its calculated `bx` and `by` with constants. Rebuild after each change. Keep the texture width, height and UV range within the uploaded image.

## Troubleshooting and credits

Wrong colours usually mean the wrong palette, palette position or texture depth. A damaged background suggests that an upload overlaps a framebuffer. A chopped sprite suggests a size or UV mismatch.

The sample images come from Pexels according to the [recorded asset provenance](https://github.com/EBonura/PSoXide/blob/main/docs/asset-provenance.md); the original image URLs remain unrecorded. Credit and licence requirements belong to the assets, separately from the SDK code. Use your own images or assets with documented permission when making a new project.
