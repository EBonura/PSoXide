# `emu/` (emulator & frontend)

The PlayStation 1 emulator and the desktop application that wraps it: a
pure-state-machine core, a wgpu/egui frontend, and the hardware renderer.
Accuracy is validated against real PS1 hardware: the GTE is bit-exact
against a real-console conformance corpus, the triangle rasterizer matches
silicon pixel coverage, and the core models silicon-measured GTE hazards found
during hardware validation.

These crates run on the host and are members of the repo-root HOST
workspace (one lockfile shared with `crates/`, `editor/`, and `tools/`).

## Crates

| Crate | Purpose |
|-------|---------|
| [`emulator-core`](crates/emulator-core) | CPU, bus, and peripherals. No UI, no window. Pure state machine. |
| [`frontend`](crates/frontend) | PSoXide desktop frontend (winit + wgpu + egui). Embeds the emulator and the editor. |
| [`psx-gpu-render`](crates/psx-gpu-render) | Hardware renderer: wgpu pipeline drawing GP0 primitives at an internal-resolution multiple of native VRAM (free upscaling). Lines/polylines draw as one-PSX-pixel quad bands (endpoint-inclusive, connected at any slope, upscale with the target); the software GPU stays the pixel oracle for exact Bresenham steps. |
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

- [Root README](../README.md#quick-start). Launching the frontend.
- [`docs/frontend.md`](../docs/frontend.md). Frontend architecture.
