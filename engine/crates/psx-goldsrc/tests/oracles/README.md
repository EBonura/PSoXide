Frozen test-only HSFX implementations: HL 70072ce and CS 988f308, before extraction. They have no production callers. The oracle rewires hardware access for recording; all parser, ownership, gain, upload ordering and sample configuration bodies are retained. Directory truncation is a known legacy panic, tested separately from tolerant invalid payloads.

Map-loop voice allocation is no longer compared: the shared runtime now takes a
free voice first, evicts the quietest loop and re-levels authored loops as the
listener moves (73ff8eaa, fe95bd46), where the originals were round-robin. The
scenario leaves map-loop starts and stops out, and the `hsfx.rs` host tests pin
the new allocation. `stop_map_loops` and `stop_all` are still compared.

## Chunk streamer

Retired. The frozen HL 70072 / CS 988f chunk streamer (`legacy-cdstream.rs`) and its oracle
were removed when the owner stopped being equivalent to it on purpose: a short pack table is
never cached, failed reads are tried again from a fresh seek, and the polled pump with its
64-call stall bound gave way to the interrupt-serviced session. `tests/chunk_stream.rs`
pins the new contract against a scripted drive.

## Renderer

`legacy-render.rs` is the exact HL 70072 renderer, including its original 17
unit tests. `RENDER-PROVENANCE.json` reconstructs the CS 988f original using
only its frozen viewport delta and verifies both original hashes. There is
one frozen legacy body, not two maintained implementations.

`render_oracle.rs` compiles the current shared owner with both thin view
adapters and compares 30,000 full-screen and 210,000 split/windowed polygons
across six focal lengths, including both split orientations and return to full screen. It compares every output coordinate, RGB/UV/depth,
clip count and order, guard/extent/refinement decision directly. It also runs
the 17 original unit tests against both old and shared implementations for
each game. Private helper tests live inside the temporary shared module so
production internals remain private. These are host release arithmetic gates;
MIPS layout, GPU output, state and timing are separate acceptance gates.

```sh
cargo test --manifest-path engine/Cargo.toml -p psx-goldsrc --test render_oracle
```
