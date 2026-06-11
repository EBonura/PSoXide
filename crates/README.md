# `crates/` (shared PSX primitives)

Workspace-shared, `no_std`-compatible building blocks consumed across the
emulator, SDK, and host tooling. These have no UI and no target-specific
runtime; they are the lowest common layer the rest of the repo builds on.

| Crate | Purpose |
|-------|---------|
| [`psx-hw`](psx-hw) | PlayStation 1 hardware model: register addresses, bitfield layouts, and command packet formats. Shared by emulator and SDK. |
| [`psx-iso`](psx-iso) | BIN/CUE and ISO9660 parsing for PS1 disc images. Shared by emulator, SDK, and disc-builder. |
| [`psx-trace`](psx-trace) | Instruction trace record format emitted by the emulator core (originally shared with the retired PCSX-Redux parity oracle). |

These three are the only members of the root `Cargo.toml` workspace. The
`sdk/`, `engine/`, `emu/`, `editor/`, and example trees are each their own
nested workspace. See the README in each.

## See also

- [Root README](../README.md). Project overview, quick start, build targets.
- [`docs/redux-oracle.md`](../docs/redux-oracle.md). The retired parity harness `psx-trace` originally fed.
