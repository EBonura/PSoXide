+++
title = "cdda-read-contention"
description = "CD data commands during audio playback"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Start CD-DA playback, then issue a data-read command and record the drive response. This intentionally probes command contention. It is not the recommended streaming pattern for a game and needs a disc with track 2 audio.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

Build the executable, then master a mixed-mode image with your own sector-aligned raw CD-DA audio. Replace `/path/to/track.raw` with a prepared audio file; the packer does not convert a WAV or MP3 in this command.

```sh
make example EXAMPLE=cdda-read-contention
cargo run --locked --release -p mkisopsx -- \
  --exe build/examples/mipsel-sony-psx/release/cdda-read-contention.exe \
  --out build/examples/mipsel-sony-psx/release/cdda-read-contention.bin \
  --cdda-track /path/to/track.raw
```

Open the resulting CUE with the BIN beside it. A generic `make disc` image has no audio track. See the [audio cooker's source and formats](https://github.com/EBonura/PSoXide/tree/e37dfe425a21b4475a131e094005622dfb98bc39/crates/psx-audio-cook) when preparing your source audio.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="cdda-read-contention" />}}
