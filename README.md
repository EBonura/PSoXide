# PSoXide SDK

Bare-metal Rust tools and libraries for the original PlayStation. This is the
SDK repository at the original **EBonura/PSoXide** URL. It provides the runtime,
GPU/GTE, audio, input, disc and memory-card APIs, fixed-point math, shared data
formats, a disc packer and small homebrew examples.

The editor, engine and Cortex Ignition live together in
[PSoXide-editor](https://github.com/EBonura/PSoXide-editor). The emulator is in
[PSoXide-emulator](https://github.com/EBonura/PSoXide-emulator). Their source
history and existing pre-split Git revisions remain available.

## Build a triangle

Install Rust through rustup and a C/C++ build toolchain for your host. The
checked-in `rust-toolchain.toml` selects the required nightly and components.
Python 3 and `mipsel-none-elf-objdump` are used by the instruction-hazard check.

```sh
git clone https://github.com/EBonura/PSoXide.git
cd PSoXide
make check
make test
make hello-tri-disc
```

The result is `build/examples/mipsel-sony-psx/release/hello-tri.{exe,bin,cue}`.
Keep the BIN and CUE together. It runs on a PS1 or in an emulator; no editor is
needed to compile or package it. Launch with an existing emulator executable:

```sh
make run-tri FRONTEND=/absolute/path/to/frontend
```

`make disc EXAMPLE=hello-input` selects another self-contained example.
Examples that use CD audio or WORLD.PAK need their own pack inputs; the generic
`disc` target only creates a data-only image.

## Layout

- [sdk/](sdk/README.md): 19 device crates, linker script and examples.
- `crates/`: shared hardware, disc, trace and cooked-asset format contracts.
- `tools/mkisopsx`: host-side BIN/CUE mastering.
- `tools/psoxide-link`: source hydration for pinned downstream builds.
- `tools/hazard_scan.py`, `hazard_patch.py`, `guest_symbol_gate.sh`: guest checks.

The root host workspace and `sdk/` device workspace intentionally remain
separate. Existing `sdk/crates/*` and `sdk/psoxide.ld` paths are retained.
The dependency-free `psxed-format` package now lives in `crates/psxed-format`;
its package name and binary layouts are unchanged.

## Downstream games

Pin a full Git revision and commit lockfiles. SDK-only hydration contains this
repository; engine-based games additionally pin the editor/runtime component.
The demo-disc repository owns the tested component combination for each disc.
See `components.json` for the SDK package layout and extraction provenance.

The project is pre-1.0. Format/API changes require coordinated consumer updates.
Device code uses bounded memory and 32-bit fixed-point arithmetic. Emulator
checks complement rather than replace original-hardware testing.

## License

Code remains [GPL-2.0-or-later](LICENSE). Preserve existing attribution and
provenance; extraction does not change licensing. Example assets have their
own [provenance records](docs/asset-provenance.md). See
[downstream licensing](docs/downstream-licensing.md) before distribution.

## Recent changes

Source snapshot **2026.09.05**: Split the SDK from the editor and emulator; existing Git revisions still resolve.
See the [changelog](CHANGELOG.md) for the remaining changes.
