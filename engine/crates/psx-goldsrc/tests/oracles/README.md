Frozen test-only HSFX implementations: HL70072ce and CS988f308, before extraction. They have no production callers. The oracle rewires hardware access for recording; all parser, ownership, gain, upload ordering and sample configuration bodies are retained. Directory truncation is a known legacy panic, tested separately from tolerant invalid payloads.

## Chunk streamer

`legacy-cdstream.rs` is the exact public HL70072 source. CS988f differs only
in the persistent-entry capacity (32 versus16) and its matching comment;
`CDSTREAM-PROVENANCE.json` records both originals. The independent oracle
compiles the frozen algorithm for both configurations with a recording sector
transport. It compares cache/pump behavior and destination bytes, including
header entry straddling, cache invalidation, persistent collisions, fallback,
startup/read errors and recovery. Separate assertions require the first-pump
yield, one-sector service, and failure on the64th stalled pump. It does not
simulate optical seek time, FIFO loss, DMA or hardware acknowledgement timing.

Run from the editor root:

```sh
python3 engine/crates/psx-goldsrc/tests/chunk_stream_oracle.py
```
