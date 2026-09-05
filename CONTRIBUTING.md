# Contributing

Use the pinned Rust toolchain. Run `make fmt`, `make check`, `make test` and
`make lint`; for guest changes also build and run a relevant example and check
its MIPS hazards. Keep runtime changes separate from packaging changes.

Report the source revision, build features, reproduction steps and whether
results came from an emulator or original hardware. Do not commit retail BIOS,
commercial game data, credentials, build caches or local captures.

Preserve the license and source attribution. Engine/editor changes belong in
PSoXide-editor; emulator changes belong in PSoXide-emulator. Shared hardware
and binary contracts must keep their consumer tests in sync.
