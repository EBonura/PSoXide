# PSoXide SDK

> **Built with agentic coding.** AI coding agents write most of the code in PSoXide. I direct them, review what they produce, and test the results in the emulator and on a real PlayStation.

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
The post-link hazard and stack checks are Rust (`tools/psoxide-hazard`): no
Python or MIPS binutils needed to build.

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

- [sdk/](sdk/README.md): the device crates, linker script and examples.
- `crates/`: shared hardware, disc, trace and cooked-asset format contracts.
- `tools/mkisopsx`: host-side BIN/CUE mastering.
- `tools/psoxide-link`: source hydration for pinned downstream builds, and
  `psoxide-components`, which brings a consumer's imported paths to its
  `components.lock.json` and records a content receipt.
- `tools/psoxide-pgo`: emulator PC histogram to LLVM sample profile, for
  profile-guided guest builds with no instrumentation. `portable` writes a
  profile that can be committed, and `rebind` applies it to a build made in
  any other checkout. `order` and the `+order` variants link functions in an
  I-cache-aware order from exact per-word counts, gated per game by `choose`.
- `tools/psoxide-hazard` (`hazard-patch`, `hazard-scan`, `stack-guard`) and
  `guest_symbol_gate.sh` (a link-map grep for 64-bit helpers): guest checks.
  psoxide-hazard decodes the image with `crates/psx-disasm`, no objdump. The
  patcher and scanner share one detector; given the link's `-Map` (`--map`),
  both bound each switch's jump table to its own function, and `--text-only`
  limits them to the map's `.text`. `stack-guard` proves every
  `psx_rt::scratchpad::ScratchpadStack` call tree in a linked exe fits its
  scratchpad region, from the exe and its ld.lld `-Map`. psoxide-pgo runs all
  three in-process.

- `tools/xtask`: repository tasks, `cargo run -p xtask -- <task>`:
  `check-mfc0` (`make lint`), `material-audit` (CI), `fmv-test-movie`
  (hello-fmv's movie) and the website's `site` tasks. The repository runs no
  Python.

- `tools/check-register-literals.sh` (`make lint`): fails when a hardware-window
  address literal appears anywhere outside `crates/psx-hw`.

The root host workspace and `sdk/` device workspace intentionally remain
separate. Existing `sdk/crates/*` and `sdk/psoxide.ld` paths are retained.
The dependency-free `psxed-format` package now lives in `crates/psxed-format`;
its package name and binary layouts are unchanged.

## Downstream games

Pin a full Git revision and commit lockfiles. SDK-only hydration contains this
repository; engine-based games additionally pin the editor/runtime component.
The demo-disc repository owns the tested component combination for each disc.
See `components.json` for the SDK package layout and extraction provenance.

### Repinning a game onto the Rust tools

From `ceabbac8f` the post-link checks are Rust, and from `8a7252089` the
component bootstrap is too; this repository no longer ships the Python
versions. The Rust tools patch, scan and report exactly as the Python ones
did (checked on the WipEout, Half-Life and Quake builds), so after a repin any
difference in a game's image comes from other SDK changes in the range. A game
moving its SDK pin past them changes four things:

1. Its build driver runs `hazard-patch`, `hazard-scan` and `stack-guard`,
   built from the hydrated tree (`cargo build --release -p psoxide-hazard` in
   `.psoxide`), where it ran `python3 .psoxide/tools/hazard_patch.py`,
   `hazard_scan.py` and `stack_guard.py`. The arguments are unchanged.
   psoxide-pgo runs them in-process, so a PGO build needs nothing.
2. Its `components.lock.json` lists `tools/psoxide-hazard` for the SDK
   component in place of the three `.py` paths (`crates` already brings
   `crates/psx-disasm`).
3. It builds against an editor revision whose `Cargo.lock` includes
   `psoxide-hazard` and `psx-disasm`, because games build `--locked` against
   the editor's lock.
4. Its emulator component is a revision that compiles against the new SDK:
   since `3a7c21a05` (psx-iso reads tracks through a `TrackSource`), emulator
   `3d49e63` or later.

It also replaces its copy of `tools/bootstrap-components.py` with
`psoxide-components` (`tools/psoxide-link`): same arguments, lock, receipt
and files. A Rust build driver that already depends on `psoxide-link` can
call `psoxide_link::components::materialize` instead; otherwise install the
binary from the pinned revision with `cargo install --locked --git
https://github.com/EBonura/PSoXide --rev REV psoxide-link`.

A driver that extracts SDK files itself (for example by untarring `git
archive`) must give them a fresh modification time, or clean its guest
target directory when the pin changes. An archive dates files at their
commit, and Cargo, which compares mtimes, can otherwise link objects built
from the previous SDK. `psoxide-link` and `psoxide-components` always write
fresh files.

The project is pre-1.0. Format/API changes require coordinated consumer updates.
Device code uses bounded memory and 32-bit fixed-point arithmetic. Emulator
checks complement rather than replace original-hardware testing.

## License

Code remains [GPL-2.0-or-later](LICENSE). Preserve existing attribution and
provenance; extraction does not change licensing. Example assets have their
own [provenance records](docs/asset-provenance.md). See
[downstream licensing](docs/downstream-licensing.md) before distribution.

## How This Was Built

AI coding agents wrote nearly all of the code in PSoXide. A human directs the
architecture, reviews every change and verifies the results on hardware.
[How PSoXide is built](https://ebonura.github.io/PSoXide/how-its-built/) lists
the rules the agents work to and the checks each change passes.

This is not a clean-room implementation. An LLM is trained on large amounts of
existing code, so AI-written code can carry influence from its training data
that neither the tool nor the author can fully audit. Disclosing AI assistance
is therefore not a warranty of clean-room provenance or of non-infringement.
Parts of the emulator core are derived from PCSX-Redux (GPL-2.0-or-later), and
those derivations are tracked file by file. See
[downstream licensing](docs/downstream-licensing.md) and the
[license audit](docs/license-audit.md).

## Recent changes

Source snapshot **2026.09.05**: Split the SDK from the editor and emulator; existing Git revisions still resolve.
See the [changelog](CHANGELOG.md) for the remaining changes.

## Firmware policy

PSoXide does not bundle or load external console firmware. Homebrew runs
through the built-in emulator runtime. See the [cleanup audit](docs/firmware-cleanup.md)
for the source, binary-header and history checks.
