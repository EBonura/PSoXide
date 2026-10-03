# `sdk/` (PSX SDK)

Bare-metal PlayStation 1 SDK: typed wrappers over the hardware with no
engine framework on top. This is the layer you use to write a PS1 program
directly (`_start`, GPU/SPU/GTE access, controller polling) without the
Scene/App machinery in the separate engine component.

The SDK is its own Cargo workspace (`sdk/Cargo.toml`). Code here targets
MIPS (`mipsel-sony-psx`); the GTE crates additionally build for the host so
the editor and emulator share one simulation.

## Crates

| Crate | Purpose |
|-------|---------|
| [`psx-rt`](crates/psx-rt) | Bare-metal runtime: `_start`, BIOS calls, panic, heap, VBlank IRQ counter, corrected i64 builtins, scratchpad regions and scratchpad stacks. |
| [`psx-io`](crates/psx-io) | Volatile MMIO primitives for target code, plus the CD-ROM command surface. |
| [`psx-gpu`](crates/psx-gpu) | High-level GPU API: init, primitives, framebuffers, display-window shifting. |
| [`psx-vram`](crates/psx-vram) | Typed VRAM layout primitives: color, rect, tpage, CLUT, upload helpers. |
| [`psx-spu`](crates/psx-spu) | High-level SPU API: typed voices, volume, pitch, ADSR, ADPCM upload, key-on/off, loop chaining, noise. |
| [`psx-sfx`](crates/psx-sfx) | One-shot sample banks, voice allocation and playback cutoff for SPU effects. |
| [`psx-settings`](crates/psx-settings) | Versioned game preferences with optional memory-card persistence. |
| [`psx-gte`](crates/psx-gte) | GTE (COP2) wrappers. MIPS emits inline-asm coprocessor ops; host routes through `psx-gte-core`. |
| [`psx-gte-core`](crates/psx-gte-core) | Pure-Rust GTE state machine and fixed-point math. Shared by `psx-gte` and the emulator; bit-exact against a real-console conformance corpus. |
| [`psx-math`](crates/psx-math) | Fixed-point math: Q0.12 angles + sin/cos/atan2, int32 helpers, decimal text formatting. |
| [`psx-pad`](crates/psx-pad) | SIO0 controller polling: digital + DualShock analog, `PadTracker` edges/repeat, diagnostic pacing. |
| [`psx-tick`](crates/psx-tick) | Fixed-timestep game clock: per-game tick rate and catch-up policy, a render phase for interpolation, and consistency counters. Callers pass the VBlank count, so every rule is host-testable. |
| [`psx-font`](crates/psx-font) | Bitmap-font atlas: 1bpp source → 4bpp CLUT VRAM texture, GP0 textured-rect draw path. |
| [`psx-fx`](crates/psx-fx) | Arcade-style visual effects: particle pools, screen shake, deterministic RNG. |
| [`psx-asset`](crates/psx-asset) | Runtime parsers for cooked-asset blobs. Consumes `psxed-format` layouts produced by the editor. |
| [`psx-cache`](crates/psx-cache) | Generic no_std slot cache: keyed residency pool with LRU eviction and pinning. |
| [`psx-mc`](crates/psx-mc) | Memory-card driver: SIO0 transport, on-card filesystem format interoperable with the console's card manager, optional LZSS compression. |
| [`psx-osk`](crates/psx-osk) | On-screen keyboard for pad-driven text entry: QWERTY/symbols pages, shift, PS4-style boxed keys. |
| [`psx-pack`](crates/psx-pack) | Guest-side WORLD.PAK parsing + in-place HLZC/LZ4 decompression (reader half of `psx-iso`). |
| [`psx-telemetry`](crates/psx-telemetry) | Shared guest/host telemetry id tables for the emulator profiling hooks. |
| [`psx-fmv`](crates/psx-fmv) | FMV playback building blocks: STR sector demux, BS v2 bitstream to MDEC run-length decode, MDEC upload/decode via DMA0/DMA1 (guest only), ISO9660 root-file lookup. |

There is no umbrella crate: games depend on the subsystem crates they use.
A standalone game should use the pinned bootstrap workflow described in the
root README, rather than copying SDK directories by hand; several shared
dependencies still live outside `sdk/`.

## Examples

Bare-metal programs in [`examples/`](examples), each its own workspace.
Build and run them via the top-level `Makefile` (see the
[root README](../README.md#build-a-triangle)).

| Example | Shows |
|---------|-------|
| `hello-tri` | A single GPU triangle. |
| `hello-tex` | Textured primitives + CLUT upload. |
| `hello-ot` | Ordering-table depth sorting. |
| `hello-input` | Controller polling via `psx-pad`. |
| `hello-gte` | GTE-accelerated transforms. |
| `hello-audio` | SPU voice playback. |
| `hello-cdda` | CD-DA audio tracks. |
| `hello-spstack` | A call tree run on a scratchpad stack under VBlank IRQs, checked against the RAM stack; `stack-guard` (`tools/psoxide-hazard`) proves it fits. |
| `hello-gteirq` | RTPS run under VBlank IRQs, checking that psx-rt's handler never runs a GTE command twice. The emulator models this since its commit c7e3ea8; a console run remains the reference. |
| `hello-present` | psx-rt's queued display flip, held until the frame's closing GP0(1Fh) sets GPUSTAT bit 24 (`arm_draw_done`, `end_with_draw_done`, `signal_draw_done`). |
| `hello-present-queue` | psx-rt's VBlank-kicked present queue (`present-queue` feature): whole frames published as one chain each, preamble and HUD recorded with `psx_io::gpu::begin_recording_raw`, kicked and flipped by the VBlank handler. |
| `hello-fmv` | FMV console test: streams a 2x STR with XA audio, decodes it with `psx-fmv`, and shows sector counts (LOST/BAD) over the video. |
| `hello-memcard` | Non-destructive memory-card diagnostic: reads and hashes all 1024 frames, then (L1+R1+Cross) writes and verifies one test save. |
| `hello-memprobe` | Checks psx-rt's `memcpy`/`memset`/`memcmp` against reference loops for every size and alignment; prints `MEMPROBE PASS` or `FAIL`. |
| `hello-pack` | `psx_pack::cd` smoke test: streams raw and compressed WORLD.PAK chunks off the disc and checks them. |
| `hello-i64probe` | Runs software 64-bit multiply/divide/modulo on the target and checks the results; covers psx-rt's `__divdi3`/`__moddi3` overrides. |
| `cdda-read-contention` | CD-ROM conformance probe: issues a data read while CD-DA is playing and records which IRQ the drive raises. |

## See also

- [Root README](../README.md): builds, example discs and downstream use.
- [Editor, engine and Cortex](https://github.com/EBonura/PSoXide-editor).
- [Emulator](https://github.com/EBonura/PSoXide-emulator).
