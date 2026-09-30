+++
title = "hello-fmv"
description = "Streaming video with XA audio"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Stream MOVIE.STR while decoding video through the CPU and MDEC. Read the library and its boot wrapper together. The overlay tracks lost sectors, corrupt sectors and late frames; those counters distinguish data loss from decoding that cannot keep up.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

This program needs `MOVIE.STR` with the synthetic test pattern and sector metadata expected by the diagnostic. Install FFmpeg and build `psxavenc` separately, then supply its executable path:

```sh
make example EXAMPLE=hello-fmv
mkdir -p build/fmv-docs
python3 tools/fmv_test_movie.py \
  --psxavenc /path/to/psxavenc --out build/fmv-docs/MOVIE.STR
cargo run --locked --release -p mkisopsx -- \
  --exe build/examples/mipsel-sony-psx/release/hello-fmv.exe \
  --out build/examples/mipsel-sony-psx/release/hello-fmv.bin \
  --xa-file build/fmv-docs/MOVIE.STR
```

Open the resulting CUE. Passing an ordinary movie file directly to the executable will not satisfy the test. These asset-generation steps are separate from the six browser-player builds.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-fmv" />}}
