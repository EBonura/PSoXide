# `emulator-core` examples (validation & diagnosis tools)

Durable host-side tools that run the emulator core headless. Each is a
standalone binary: `cargo run -p emulator-core --release --example <name>`.
One-off investigation probes are deleted once their hunt closes; git history
keeps the tools, so everything listed here is maintained.

## GTE accuracy (real-hardware oracles)

| Example | Purpose |
|---------|---------|
| `gte_fuzz_replay` | Replays JaCzekanski's real-console captured gte-fuzz log against `psx-gte-core`: 1100 tests, all 22 opcodes, all 64 registers including FLAG. The zero-burn GTE conformance gate. |
| `gte_skin_replay` | Replays a live capture (values transcribed from overlay photos taken off a real console) through the bit-exact GTE core, stage by stage. The tool that decoded the MTC2-commit hazard. |
| `gte_expected_values` | Computes the baked expected values for the `hardware-tests` disc through `psx-gte-core`, so no disc expectation is ever hand-invented. |

## Boot, disc, and commercial-game harnesses

| Example | Purpose |
|---------|---------|
| `boot_disc` | Disc boot harness entry point. |
| `verify_disc_reads` | Verifies disc sector delivery end to end. |
| `cdrom_probe` | CD-ROM command/state probe; used by the cortex preburn suite. |
| `probe_cdda_wav` | Captures CD-DA/SPU audio output to WAV; used by the preburn suite and the audio example targets. |
| `probe_disc_pad_trace` | Disc boot + pad input flow trace; used by the preburn boot-flow gate. |

## Performance and internals

| Example | Purpose |
|---------|---------|
| `bench_frame_paths` | Frame-path benchmark harness. |
| `cache_inspect`, `cache_diff` | I-cache model inspection and comparison. |
| `dma3_audit` | DMA channel 3 (CD-ROM) transfer audit. |
| `bios_syscall_probe` | BIOS A/B/C-table call instrumentation. |
| `smoke_draw` | Minimal first-instructions GPU smoke test. |
| `texwarp` | Measures affine texture warping in **texels**, per pixel, against an analytic perspective-correct ground truth, and ranks every mitigation (subdivision schemes, diagonal choice, UV scale) by error per primitive. See [`docs/texture-warping-2026-07-27.md`](../../../../docs/texture-warping-2026-07-27.md). |

Shared helpers live in [`support/`](support).
