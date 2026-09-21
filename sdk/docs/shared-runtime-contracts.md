# Shared runtime contracts and adopters

This inventory records the actual migration callers, including deliberately distinct numerical and scheduling policies.

| API | Adopters | Preserved contract and checks |
| --- | --- | --- |
| `psx_gpu::ordered::OrderedCommandStream` | Celeste Collection (both carts) | Complete packets, at most 15 payload words plus one DMA tag; painter order; small A0 uploads; static DMA storage; async submission and explicit draw fence. Host transport tests cover actual linked nodes, delayed completion, exact capacity, reuse, moves/drop, and the captured 264-command frame. |
| `psx_font::FontAtlas::emit_text_packets` | VoXide | Atlas-owned glyph layout and padding, E1/E2 before sprites, callback cancellation. Host padded-atlas and bounded-sink tests. |
| `psx_pack::visibility` | HL, CS, Quake; engine compatibility reexport | One allocation-free RLE kernel with explicit strict or clamped decode/merge policies. Core-only consumers do not acquire an allocator. |
| `psx_math::attributed_clip` | engine reexport, VoXide, HL, CS | Existing allocation-free traversal moved unchanged; caller adapters retain distance arithmetic, interpolation, deduplication and vertex order. No dynamic dispatch. |
| `int32::{isqrt_u32,isqrt_u64}` | engine lighting, HL, CS, HK | Exact floor square root, including full unsigned ranges; boundary tests. Existing signed helper retains nonpositive-to-zero policy. |
| `Mat3I16::{rotate_x_q12,rotate_y_q12,transform_i32}` | Nitroxide | Interpolated 4096-unit rotations and wrapping i32 dot products before arithmetic shift; exhaustive angle test. Existing 256-unit constructors remain distinct. |
| `color::{scale_rgb,lerp_rgb,lerp_rgb_q8}` | GH, Nitro, PSXcel, demo | Saturating channels, truncating signed ratio and flooring Q8 interpolation are separate contracts. Descending-channel tests prevent a one-level rounding change. |
| `ButtonState::pressed_since`, `aim_curve_symmetric` | VoXide, HL, CS | Group edge versus per-button tracker; inverted axes may include +128 without changing existing clamped curve. |
| `cdrom::poll_data_sector` | HL, CS | Nonblocking probe leaves INT1 for the sector reader; bounded response drain, error reset, unrelated ACK and FIFO fallback. Protocol-order tests. |
| `CddaStarter::{begin_after,with_retry_ticks}` | Arcade Pong | Existing SetMode/Demute/Play state machine, caller pacing, shared post-command settle. Defaults unchanged; wrap/retry/single-Play tests. |
| `gpu::{try_wait_cmd_ready,try_wait_dma_ready}` | demo and Arcade loaders | Nonrecovering bounded readiness probe. Callers explicitly retain best-effort timeout handling; this helper never resets the GPU. |
| `psx_vram::upload_bytes_aligned` | HK | Packed word loads only when alignment and pixel count allow; unaligned and odd pixel uploads retain byte packing. |
| `psx_spu::{set_irq_address,enable_irq,irq_pending}` | HK | Shared typed SPU register access; CPU IRQ acknowledgement and refill scheduling remain caller-owned. |
| `telemetry::emit::cycles` | VoXide | Emulator cycle MMIO only for MIPS with emit enabled; zero otherwise. |

The ordered list uses a conservative 16-word **total** merged primitive contract from Sony's Run-Time Library Overview 4.6 (printed page 8-13), including the DMA tag. This is not a claim that every hardware command outside that bound is illegal. Large uploads remain separate VRAM operations after an explicit stream fence. The caller supplies static RAM; moving or forgetting the stream cannot release storage still read by DMA. Dropping waits for DMA but deliberately discards unsent packets. The common submission boundary uses compiler-only memory barriers before starting DMA and after its completion wait: volatile register access alone does not order unrelated ordinary stores.

Game-specific animation, PICO-8 palette/synth semantics, voxel visibility, clipping numeric adapters, authored asset formats and streaming budgets remain caller policy. Consumer replay evidence lives with the harmonisation audit rather than in source assets.
