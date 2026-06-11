# `emu/` (emulator & frontend)

The PlayStation 1 emulator and the desktop application that wraps it: a
pure-state-machine core, a wgpu/egui frontend, and the hardware renderer.
Accuracy is validated against real PS1 hardware: the GTE is bit-exact
against a real-console conformance corpus, the triangle rasterizer matches
silicon pixel coverage, and the core models silicon-measured GTE hazards no
other public emulator does (see `docs/hardware-burn-ledger.md` for the
burn-by-burn evidence).

`emu/Cargo.toml` is its own workspace. These crates run on the host.

## Crates

| Crate | Purpose |
|-------|---------|
| [`emulator-core`](crates/emulator-core) | CPU, bus, and peripherals. No UI, no window. Pure state machine. |
| [`frontend`](crates/frontend) | PSoXide desktop frontend (winit + wgpu + egui). Embeds the emulator and the editor. |
| [`psx-gpu-render`](crates/psx-gpu-render) | Hardware renderer: wgpu pipeline drawing each GP0 primitive at an internal-resolution multiple of native VRAM (free upscaling). |
| [`psoxide-settings`](crates/psoxide-settings) | On-disk settings, library cache, and save-state formats. Pure logic, no GUI/wgpu deps, usable from headless CLIs. |
| [`psoxide-validation`](crates/psoxide-validation) | Manifest and exact-hash comparison primitives for validation. Runner-agnostic. |

## Accuracy tooling

[`crates/emulator-core/examples/`](crates/emulator-core/examples) holds the
durable validation and diagnosis tools (see its
[README](crates/emulator-core/examples/README.md)), including
`gte_fuzz_replay` (replays a real-console captured GTE test corpus) and
`gte_skin_replay` (replays live values photographed off a real console
through the bit-exact GTE core).

## See also

- [Root README](../README.md#5-launch-the-frontend). Launching the frontend.
- [`docs/frontend.md`](../docs/frontend.md). Frontend architecture.
- [`docs/hardware-burn-ledger.md`](../docs/hardware-burn-ledger.md). Real-hardware findings and the fixes they produced.
- [`docs/commercial-parity-tracker.md`](../docs/commercial-parity-tracker.md). Retail-disc compatibility status.
