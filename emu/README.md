# `emu/` (emulator & frontend)

The PlayStation 1 emulator and the desktop application that wraps it: a
pure-state-machine core, a wgpu/egui frontend, the hardware renderer, and
the PCSX-Redux parity harness used to validate the core against real
hardware behaviour.

`emu/Cargo.toml` is its own workspace. These crates run on the host.

## Crates

| Crate | Purpose |
|-------|---------|
| [`emulator-core`](crates/emulator-core) | CPU, bus, and peripherals. No UI, no window. Pure state machine. |
| [`frontend`](crates/frontend) | PSoXide desktop frontend (winit + wgpu + egui). Embeds the emulator and the editor. |
| [`psx-gpu-render`](crates/psx-gpu-render) | Hardware renderer: wgpu pipeline drawing each GP0 primitive at an internal-resolution multiple of native VRAM (free upscaling). |
| [`psx-gpu-compute`](crates/psx-gpu-compute) | GPU-side VRAM for the experimental compute-shader rasterizer. |
| [`psoxide-settings`](crates/psoxide-settings) | On-disk settings, library cache, and save-state formats. Pure logic, no GUI/wgpu deps, usable from headless CLIs. |
| [`psoxide-validation`](crates/psoxide-validation) | Manifest and exact-hash comparison primitives for validation. Runner-agnostic. |
| [`parity-oracle`](crates/parity-oracle) | PCSX-Redux harness: manages a headless Redux subprocess and collects traces to validate `emulator-core` against. |

## See also

- [Root README](../README.md#5-launch-the-frontend). Launching the frontend.
- [`docs/frontend.md`](../docs/frontend.md). Frontend architecture.
- [`docs/redux-oracle.md`](../docs/redux-oracle.md). Parity validation against PCSX-Redux.
- [`docs/commercial-parity-tracker.md`](../docs/commercial-parity-tracker.md). Retail-disc compatibility status.
- [`crates/psx-trace`](../crates/psx-trace). The trace format the oracle diffs against.
