# `crates/` (shared PSX primitives)

Workspace-shared, `no_std`-compatible building blocks consumed across the
emulator, SDK, and host tooling. These have no UI and no target-specific
runtime; they are the lowest common layer the rest of the repo builds on.

| Crate | Purpose |
|-------|---------|
| [`psx-hw`](psx-hw) | PlayStation 1 hardware model: register addresses, bitfield layouts, and command packet formats. Shared by emulator and SDK. |
| [`psx-iso`](psx-iso) | BIN/CUE and ISO9660 parsing for PS1 disc images. Shared by emulator, SDK, and disc-builder. |
| [`psx-trace`](psx-trace) | Instruction trace record format emitted by the emulator core (originally shared with the retired PCSX-Redux parity oracle). |
| [`psxed-format`](psxed-format) | Neutral cooked-asset formats shared by the SDK, editor cookers and engine. |

These crates are members of the SDK repo-root host workspace alongside its
host tools. The device-side `sdk/` keeps its own workspace. The engine and
editor live in [PSoXide-editor](https://github.com/EBonura/PSoXide-editor),
and the emulator in [PSoXide-emulator](https://github.com/EBonura/PSoXide-emulator);
both import the shared contracts from an exact SDK revision.

## See also

- [Root README](../README.md). Project overview, quick start, build targets.
