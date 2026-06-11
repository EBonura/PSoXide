# `sdk/` (PSX SDK)

Bare-metal PlayStation 1 SDK: typed wrappers over the hardware with no
engine framework on top. This is the layer you use to write a PS1 program
directly (`_start`, GPU/SPU/GTE access, controller polling) without the
Scene/App machinery in [`engine/`](../engine).

The SDK is its own Cargo workspace (`sdk/Cargo.toml`). Code here targets
MIPS (`mipsel-sony-psx`); the GTE crates additionally build for the host so
the editor and emulator share one simulation.

## Crates

| Crate | Purpose |
|-------|---------|
| [`psx-sdk`](crates/psx-sdk) | Umbrella crate. Re-exports the per-subsystem crates. |
| [`psx-rt`](crates/psx-rt) | Bare-metal runtime: `_start`, BIOS calls, panic, heap. |
| [`psx-io`](crates/psx-io) | Volatile MMIO primitives for target code. |
| [`psx-gpu`](crates/psx-gpu) | High-level GPU API: init, primitives, framebuffers. |
| [`psx-vram`](crates/psx-vram) | Typed VRAM layout primitives: color, rect, tpage, CLUT, upload helpers. |
| [`psx-spu`](crates/psx-spu) | High-level SPU API: typed voices, volume, pitch, ADSR, ADPCM upload, key-on/off. |
| [`psx-gte`](crates/psx-gte) | GTE (COP2) wrappers. MIPS emits inline-asm coprocessor ops; host routes through `psx-gte-core`. |
| [`psx-gte-core`](crates/psx-gte-core) | Pure-Rust GTE state machine and fixed-point math. Shared by `psx-gte` and the emulator; bit-exact against a real-console conformance corpus. |
| [`psx-math`](crates/psx-math) | Fixed-point math: Q0.12 angles + sin/cos LUTs, plus Q3.12 / Q16.16 / vec / mat scaffolding. |
| [`psx-pad`](crates/psx-pad) | SIO0 controller-poll helpers (digital pad protocol). |
| [`psx-font`](crates/psx-font) | Bitmap-font atlas: 1bpp source → 4bpp CLUT VRAM texture, GP0 textured-rect draw path. |
| [`psx-fx`](crates/psx-fx) | Arcade-style visual effects: particle pools, screen shake, deterministic RNG. |
| [`psx-asset`](crates/psx-asset) | Runtime parsers for cooked-asset blobs. Consumes `psxed-format` layouts produced by the editor. |

## Examples

Bare-metal programs in [`examples/`](examples), each its own workspace.
Build and run them via the top-level `Makefile` (see the
[root README](../README.md#examples)).

| Example | Shows |
|---------|-------|
| `hello-tri` | A single GPU triangle. |
| `hello-tex` | Textured primitives + CLUT upload. |
| `hello-ot` | Ordering-table depth sorting. |
| `hello-input` | Controller polling via `psx-pad`. |
| `hello-gte` | GTE-accelerated transforms. |
| `hello-audio` | SPU voice playback. |
| `hello-cdda` | CD-DA audio tracks. |

## See also

- [Root README](../README.md). Project overview and build instructions.
- [`engine/`](../engine). The Scene/App framework built on top of this SDK.
- [`docs/hardware-refs/`](../docs/hardware-refs). Per-subsystem hardware notes (gpu, spu, dma, irq, timers).
