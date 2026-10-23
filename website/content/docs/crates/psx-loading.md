+++
title = "psx-loading"
description = "An animated loading indicator for blocking disc reads"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-loading` when a program blocks on the disc for seconds, during a chain-load, a pack read or a sound bank streamed to the SPU. A frozen picture reads as a hang. The crate draws the one thing such a picture needs: a small animation that keeps moving while the CPU waits.

## How the crate is organized

`Style` describes the look: font, scale, inks, backing colour and position. Its `const fn` builders let a game declare it as a `const`. `Tail` chooses the animation: `Spinner`, a ring of lights chasing round a square in the game's colours, `Dots`, a label with stepping trailing dots, or `Art`, which calls a function the game supplies to draw its own frames. `Indicator` holds the state between polls. `Sink` receives the GP0 packets, and `Gp0` is the sink that writes them to the GPU on the console.

## Integration notes

Every glyph and shape is a plain monochrome rectangle built from a `psx_font::BitmapFont`'s 1-bit source, so the indicator needs no texture, palette or atlas, and does not disturb texture state. One implementation therefore works in a chain-load blob with nothing in VRAM but the picture, in a game mid-boot, and in a menu with its own atlases.

The indicator is polled, never waiting. `begin(tick)` starts it, and `poll(&mut Gp0, tick)` is called between the steps of a blocking loop, after each CD sector or each spin of a seek wait. The tick advances once per VBlank. `poll` draws only when the picture would change, at most once per VBlank, and returns whether it did. Nothing runs in an interrupt, so a draw never overlaps the caller's own GPU traffic. `Gp0` waits once per packet for the GPU to be ready and then writes the words back to back, the pacing the silicon needs.

The default look is a small black square in the bottom-right corner, built with `Style::in_corner`, with the animation inside and no text. The caller owns the GPU state: the drawing area and offset must cover the box, and `prepare` sets the mask bit and draw mode to their defaults. Colours blend in integer steps and the breathing is a triangle wave over sixteen steps, so there is no float, 64-bit arithmetic or heap. The [adoption guide](https://github.com/EBonura/PSoXide/tree/main/sdk/docs/LOADING-INDICATOR.md) shows the setup call by call.

There is no standalone `psx-loading` example in the SDK set.

## API, dependencies and source structure

{{<sdk_crate name="psx-loading" />}}
