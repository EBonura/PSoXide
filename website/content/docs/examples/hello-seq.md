+++
title = "hello-seq"
description = "Sequenced music through psx-seq"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Play a cooked song through [psx-seq](@/docs/crates/psx-seq.md) and report what the sequencer cost. The song and its sound bank are cooked from the player's own cartridge by `psx-seq-cook cook` and are never committed. Every update, at 180 Hz, is timed on root counter 2, so the cost figures come from the program itself. The program runs the music for 40 seconds, then reports.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

Cook a song and bank first, then point `PSX_SEQ_DIR` at the directory that holds `song.psq` and `bank.psqb`.

```sh
PSX_SEQ_DIR=/path/to/cooked make disc EXAMPLE=hello-seq
```

Open `build/examples/mipsel-sony-psx/release/hello-seq.cue` in the emulator with its sibling BIN in the same directory. The [first-program walkthrough](@/docs/first-ps1-program.md#4-get-the-emulator) explains installation and loading. To capture the audio and read the cost report, run the executable on the headless frontend with `--dump-audio out.wav` and read the `hello-seq:` lines on the TTY. The program does not build without the cooked input.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-seq" />}}
