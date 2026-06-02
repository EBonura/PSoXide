# `tools/` (standalone utilities)

Small host-side command-line utilities used by the build and disc-mastering
flow. Each is its own workspace.

| Tool | Purpose |
|------|---------|
| [`mkisopsx`](mkisopsx) | Wrap a PSX-EXE into a bootable PS1 disc image (BIN/CUE). Used by the example and project-disc build targets. |
| [`psx-exe-pack`](psx-exe-pack) | `psx-exe-info`: validate a PSX-EXE and print its header. The linker script already emits a PSX-EXE header; this verifies it. |

## See also

- [Root README](../README.md#7-export-an-authored-project-disc). Disc export flow.
- [`crates/psx-iso`](../crates/psx-iso). The BIN/CUE / ISO9660 parsing these build on.
