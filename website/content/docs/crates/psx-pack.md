+++
title = "psx-pack"
description = "WORLD.PAK parsing, streaming and decompression"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-pack` to read chunked data produced by the repository's pack writer. It separates format parsing from the CD transfer machinery.

## How the crate is organized

The root defines pack headers, entries, checksums and HLZC/LZ4 decompression. `cd` provides the sector reader and chunk-loading operations; its hardware operations are target-dependent. `visibility` implements allocation-free visibility-row codecs with explicit strict and clamped policies.

## Integration notes

Match the writer's format revision and preserve sector alignment. Size destination buffers for both stored and decompressed payloads. Check missing IDs, bounds and checksums before consuming a chunk. Read `SectorReader::prepare` carefully: it changes interrupt masking to VBlank-only, which your program must coordinate with other device users.

The [hello-pack source](@/docs/examples/hello-pack.md) demonstrates the complete read/check/decompress path. Its historical fixture generator is absent from this SDK snapshot; the example page describes the expected fixture, because a plain data disc won't pass.

## API, dependencies and source structure

{{<sdk_crate name="psx-pack" />}}
