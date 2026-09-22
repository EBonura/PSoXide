Frozen test-only HSFX implementations: HL 70072ce and CS 988f308, before extraction. They have no production callers. The oracle rewires hardware access for recording; all parser, ownership, gain, upload ordering and sample configuration bodies are retained. Directory truncation is a known legacy panic, tested separately from tolerant invalid payloads.

## Chunk streamer

`legacy-cdstream.rs` is the exact public HL 70072 source. CS 988f differs only
in the persistent-entry capacity (32 versus 16) and its matching comment;
`CDSTREAM-PROVENANCE.json` records both originals. The independent oracle
compiles the frozen algorithm for both configurations with a recording sector
transport. It compares cache/pump behavior and destination bytes, including
header entry straddling, cache invalidation, persistent collisions, fallback,
startup/read errors and recovery. Separate assertions require the first-pump
yield, one-sector service, and failure on the 64th stalled pump. It does not
simulate optical seek time, FIFO loss, DMA or hardware acknowledgement timing.

Run from the editor root:

```sh
python3 engine/crates/psx-goldsrc/tests/chunk_stream_oracle.py
```

## Renderer

`legacy-render.rs` is the exact HL 70072 renderer, including its original 17
unit tests. `RENDER-PROVENANCE.json` reconstructs the CS 988f original using
only its frozen viewport delta and verifies both original hashes. There is
one frozen legacy body, not two maintained implementations.

`render_oracle.py` compiles the current shared owner with both thin view
adapters and compares 30,000 full-screen and 120,000 split/windowed polygons
across six focal lengths. It compares every output coordinate, RGB/UV/depth,
clip count and order, guard/extent/refinement decision directly. It also runs
the 17 original unit tests against both old and shared implementations for
each game. Private helper tests live inside the temporary shared module so
production internals remain private. These are host release arithmetic gates;
MIPS layout, GPU output, state and timing are separate acceptance gates.

```sh
python3 engine/crates/psx-goldsrc/tests/render_oracle.py
```
