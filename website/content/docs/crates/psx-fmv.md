+++
title = "psx-fmv"
description = "STR video, bitstream decoding and MDEC"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-fmv` to assemble a movie-playback pipeline from explicit pieces, or call `player::play` to stream one movie from the disc, decode it and show it. With the pieces, your program remains responsible for feeding sectors and presenting decoded frames.

## How the crate is organized

`stream` demultiplexes STR sectors and assembles frame chunks. `bitstream` decodes version-1 and version-2 bitstreams into MDEC run-length data, with `rle` and `idct` below it. `mdec`, available on the PS1 target, transfers input and output through DMA0/DMA1, either waiting for each column or starting it and polling, and holds the `MdecDma` token while it does. `iso` finds root-directory files in ISO9660 data. `player`, also PS1 only, runs the whole pipeline overlapped: the drive, the bitstream decode of the next frame, the MDEC and the VRAM upload all work at once, at 15 or 24 bits, with XA audio, and the caller lends the memory and a skip poll.

## Integration notes

The bitstream decoder accepts versions 1 and 2, which are coded alike; version 3 is rejected. Keep CD servicing frequent enough while decoding and uploading columns to VRAM. The caller must schedule CD, DMA, audio and presentation work and budget its buffers. The parser modules can be tested on the host; host success does not verify the hardware transfer path.

The complete [hello-fmv source](@/docs/examples/hello-fmv.md) includes its library and boot wrapper, explains the required `MOVIE.STR`, and distinguishes lost sectors, bad sectors and late frames.

## API, dependencies and source structure

{{<sdk_crate name="psx-fmv" />}}
