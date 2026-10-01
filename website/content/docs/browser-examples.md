+++
title = "PS1 homebrew, running in your browser"
description = "Six actual PS1 programs running in the PSoXide emulator, right on this page. Try them, read the source and build your own."
weight = 2
[extra]
kind = "Interactive examples"
eyebrow = "Learn by trying"
+++

The SDK builds each example as a PS1 executable. The PSoXide emulator runs that
executable right here in the page, using the same emulation core as the desktop
app. Download the EXE below any player to run the same program locally.

Start with a triangle, then explore input, sprites, drawing order, 3D projection
and sound. Each example links to its source and a practical how-to. To change
one, edit and rebuild it locally, then run your new EXE or disc image in the
emulator.

**Click to run inside an example to start it.** Players load on demand and start
muted. Starting another example unloads the previous one; scrolling a running
example out of view pauses it. Use Resume to continue. Keyboard and browser
controller input are supported; touch controls are not included.

New to the SDK? Follow [Build and run your first PS1 program](@/docs/first-ps1-program.md)
for the tool installation and build steps. The downloadable EXEs are executables,
not disc images: use `make disc EXAMPLE=<name>` to create a BIN/CUE.

For deeper reference, browse [all 21 SDK crate guides](@/docs/crates/_index.md)
or the [16 complete example programs](@/docs/examples/_index.md), including their
Cargo manifests and every Rust source file.

## Triangle and frame loop

Learn how to initialize the GPU, clear a back buffer and draw a shaded triangle.
[Follow the first-program walkthrough](@/docs/first-ps1-program.md).

{{<example_player name="hello-tri" />}}

## Controller input

Change the background with the D-pad and draw coloured triangles with the face
buttons. [Read the input how-to](@/docs/controller-input.md), including how to
trigger an action only once when a button is pressed.

{{<example_player name="hello-input" />}}

## Textured sprites

Two sprites share a texture page and use separate palettes.
[Read the texture how-to](@/docs/draw-textures.md) to understand their VRAM layout
and change their movement.

{{<example_player name="hello-tex" />}}

## Drawing order

Three triangles overlap. Their ordering-table slots decide which one covers
which. [Read the drawing-order how-to](@/docs/drawing-order.md) and swap the slots
to see the effect.

{{<example_player name="hello-ot" />}}

## 3D projection

Rotate and project a wireframe cube with the GTE.
[Read the 3D how-to](@/docs/project-3d.md) to change its rotation and projection.

{{<example_player name="hello-gte" />}}

## Sound effects

Trigger one-shot samples with the face buttons, D-pad and Start. Enable sound
using the speaker button inside the player.
[Read the audio how-to](@/docs/sound-effects.md) to follow the path from a new
button press to an SPU voice.

{{<example_player name="hello-audio" />}}

## Builds and credits

The players and example programs are published together with a
[build manifest](../../examples/build.json). Each player's **Matching source** link
points to the exact SDK revision used for its executable. The manifest records
the emulator revision, SDK revision, toolchain and SHA-256 hashes. The
[emulator build record](../../player/psoxide-player-build.json) also identifies the
WebAssembly bundle and links to its source.

SDK and emulator code are GPL-2.0-or-later. Read the
[licence and third-party notices](../../player/THIRD-PARTY-NOTICES.txt) and the
[legal page](@/legal.md#code-and-licences) before redistributing builds.
The examples use the SDK's BASIC font, Pexels sample textures and Kronbits CC0
sound effects. Their individual guides and matching repositories carry the
asset provenance; these assets do not all share the code's licence.

These examples demonstrate SDK features in the browser. They are not new
measurements of emulator accuracy or proof of behaviour on original hardware.
For a console, build a disc image and read the
[hardware requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## If a player does not start

Use a browser with WebAssembly SIMD and WebGL 2 enabled. If loading fails, try
**Open player separately**, or download the EXE and use the desktop emulator.
The small examples do not require a Sony BIOS or a commercial game installation.
If input stops responding, click the game screen to restore focus. Starting a
different player resets the earlier one, so it cannot keep consuming memory or
playing audio in the background.
