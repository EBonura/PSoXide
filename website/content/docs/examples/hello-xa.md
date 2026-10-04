+++
title = "hello-xa"
description = "Streaming XA-ADPCM music"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Play XA-ADPCM songs from one interleaved file through `psx_io::cd::xa`. The disc carries `SONGS.XA`: four generated test songs as the channels of one 37.8 kHz stereo file, interleaved for single speed, so every fourth sector belongs to one song. CROSS plays the next song from the top, SQUARE restarts the current one, CIRCLE stops or restarts it, and UP and DOWN change the volume. Songs loop.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

```sh
make hello-xa-disc
```

This generates the test songs, encodes them with `psx-audio-cook xa-encode` and masters the image with `mkisopsx --xa-file`. Open `build/examples/mipsel-sony-psx/release/hello-xa.cue` in the emulator with its sibling BIN in the same directory. The [first-program walkthrough](@/docs/first-ps1-program.md#4-get-the-emulator) explains installation and loading.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Source

This example is newer than the SDK revision the site embeds sources from. Read the [complete source on GitHub](https://github.com/EBonura/PSoXide/tree/main/sdk/examples/hello-xa).
