+++
title = "psx-fmv"
description = "STR video, bitstream decoding and MDEC"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-fmv` to assemble a movie-playback pipeline from explicit pieces. Your program remains responsible for feeding sectors and presenting decoded frames.

## How the crate is organized

`stream` demultiplexes STR sectors and assembles frame chunks. `bitstream` decodes version-2 bitstreams into MDEC run-length data, with `rle` and `idct` below it. `mdec`, available on the PS1 target, transfers input and output through DMA0/DMA1 and holds the `MdecDma` token while it does. `iso` finds root-directory files in ISO9660 data.

## Integration notes

The bitstream decoder currently accepts version 2; version 3 is rejected. Keep CD servicing frequent enough while decoding and uploading columns to VRAM. The caller must schedule CD, DMA, audio and presentation work and budget its buffers. The parser modules can be tested on the host; host success does not verify the hardware transfer path.

The complete [hello-fmv source](@/docs/examples/hello-fmv.md) includes its library and boot wrapper, explains the required `MOVIE.STR`, and distinguishes lost sectors, bad sectors and late frames.

## API, dependencies and source structure

{{<sdk_crate name="psx-fmv" />}}
