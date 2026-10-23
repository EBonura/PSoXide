+++
title = "hello-xa-resume"
description = "A music stream that survives a data read"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Check that XA music survives a read of disc data. The program plays one channel of `SONGS.XA`, suspends it, reads sixteen data sectors through the polled reader, which rewrites the drive mode and leaves the head at the data, and resumes. It then checks that the song streams and its clock stays with the head readings, that the data read returns every sector asked for, that `resume` is accepted, and that the head returns to the song file within five seconds, near where the pause left it, and keeps pace afterwards. The channel and the drive mode are checked from outside, using the drive command log and the audio capture. The verdict is `hello-xa-resume: ALL PASS` or a failure, on the TTY and the screen.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

```sh
make hello-xa-resume-disc
```

This generates the test songs, encodes them with `psx-audio-cook xa-encode` and masters the image with `mkisopsx --xa-file`. Open `build/examples/mipsel-sony-psx/release/hello-xa-resume.cue` in the emulator with its sibling BIN in the same directory. The [first-program walkthrough](@/docs/first-ps1-program.md#4-get-the-emulator) explains installation and loading.

`make hello-xa-resume-gate FRONTEND=/path/to/frontend` runs the disc headless and applies the outside checks. See [hello-xa](@/docs/examples/hello-xa.md) for the plain streaming player.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-xa-resume" />}}
