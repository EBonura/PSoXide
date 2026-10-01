+++
title = "hello-cdda"
description = "Playing a disc audio track"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Play CD-DA track 2 and expose playback controls through the pad. This requires a mixed-mode disc with an audio track; a bare EXE cannot supply that audio.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

Build the executable, then master a mixed-mode image with your own sector-aligned raw CD-DA audio. Replace `/path/to/track.raw` with a prepared audio file; the packer does not convert a WAV or MP3 in this command.

```sh
make example EXAMPLE=hello-cdda
cargo run --locked --release -p mkisopsx -- \
  --exe build/examples/mipsel-sony-psx/release/hello-cdda.exe \
  --out build/examples/mipsel-sony-psx/release/hello-cdda.bin \
  --cdda-track /path/to/track.raw
```

Open the resulting CUE with the BIN beside it. A generic `make disc` image has no audio track. See the [audio cooker's source and formats](https://github.com/EBonura/PSoXide/tree/df69b8946f71e81e96d3f8b49a46c5cd856d9ea1/crates/psx-audio-cook) when preparing your source audio.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-cdda" />}}
