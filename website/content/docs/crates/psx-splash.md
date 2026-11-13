+++
title = "psx-splash"
description = "The boot splash every PSoXide game opens with"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-splash` once, at boot, to show the studio mark and one line of text before the game starts. Every PSoXide game opens the same way, so the picture, the timing and the skip rule live here and are not parameters: the only thing a game chooses is the line, for example `Built with PSoXide`.

## How the crate is organized

`show` is the whole API. It takes the `Gpu`, a 320x240 `DoubleBuffer`, the line, the player's `Brightness` and a closure that returns the pad's `ButtonState`. The Bonnie Studios mark is stored once, run-length coded (4,692 bytes of the 8,192 a plain copy takes), and expanded into VRAM eight rows at a time, so no plain copy stays in RAM.

## Integration notes

The splash runs 150 frames, one per VBlank: 32 fading in, 74 held, 44 fading out. A fresh press of Cross, Circle or Start after the first 8 frames ends it. It never reads a pad itself; the closure is how a game hands it `psx_pad::Controllers`. The brightness step is applied over each frame and fades with the picture, so the fade ends black.

It uses VRAM columns the game overwrites afterwards: the mark at texture page x 896, its palette at x 768, y 256, and the 8x8 `BASIC` font atlas at x 320. A program that never calls `show` links none of the mark, the decoder or the drawing code, and there is no cargo feature.

## API, dependencies and source structure

{{<sdk_crate name="psx-splash" />}}
