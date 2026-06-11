# `crates/` (shared PSX primitives)

Workspace-shared, `no_std`-compatible building blocks consumed across the
emulator, SDK, and host tooling. These have no UI and no target-specific
runtime; they are the lowest common layer the rest of the repo builds on.

| Crate | Purpose |
|-------|---------|
| [`psx-hw`](psx-hw) | PlayStation 1 hardware model: register addresses, bitfield layouts, and command packet formats. Shared by emulator and SDK. |
| [`psx-iso`](psx-iso) | BIN/CUE and ISO9660 parsing for PS1 disc images. Shared by emulator, SDK, and disc-builder. |
| [`psx-trace`](psx-trace) | Instruction trace record format emitted by the emulator core (originally shared with the retired PCSX-Redux parity oracle). |

These three are members of the repo-root HOST workspace (shared with
`emu/`, `editor/`, and `tools/`). The DEVICE side (`sdk/`, `engine/`:
`no_std`, `mipsel-sony-psx`) keeps its own workspaces; host and
bare-metal builds cannot share one cargo invocation. See the README in
each area.

## See also

- [Root README](../README.md). Project overview, quick start, build targets.
