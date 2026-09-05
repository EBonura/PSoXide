# Contributing to PSoXide

Start with the [quick start](README.md#quick-start), then the
[architecture guide](docs/repository-architecture.md) for the area you want to
change. The project is pre-1.0; small, reproducible changes are easiest to review.

## Development setup

Use the toolchain in `rust-toolchain.toml`. The repository has three Cargo
workspaces: the root host workspace, `sdk/`, and `engine/`. Guest examples have
separate workspaces and target settings. Run commands from the documented
folder so host and MIPS settings do not get mixed.

From the repository root:

```sh
make fmt
make check
make cook-playtest
make test
make lint
```

`make cook-playtest` prepares fixtures needed by the full test suite. For a
small change, start with the affected crate's tests, then run the checks above
before merging code. CI also checks dependency policy, instruction hazards
and the web publishing contracts. See [CI](.github/workflows/ci.yml) for the
exact commands. Documentation-only edits need link and command checks rather
than a fresh game-disc build.

## Pull requests

Describe the problem, the resulting behavior, and how you verified it. Include
screenshots for visible UI changes. For rendering or performance work, retain
a baseline and use the same scene, replay, executable settings and measurement
method on both sides. State what was tested on an emulator and what was tested
on original hardware; they are different evidence.

Guest hot paths use bounded memory and 32-bit fixed-point arithmetic. Keep
host-only conveniences out of device code, and run the numeric and MIPS hazard
checks when changing runtime math or build flags. Avoid combining behavioral
changes with unrelated formatting or generated-asset updates.

## Reporting a bug

Include the commit or version, host OS, build command and features, and the
steps needed to reproduce it. For a game, include its source revision and the
relevant project or map name. Attach a small log, screenshot or input tape when
useful. Explain the expected result as well as what happened.

For a hardware-only problem, include the console model, loading method and
whether the same image reproduces in an emulator. Do not attach retail BIOS
images, commercial game data or credentials to an issue.

## Assets and generated files

Keep build outputs, local captures and downloaded game data out of source
commits. Small fixtures needed for a regression are welcome when their source
and regeneration steps are documented. Large game assets and historical
captures belong with the game or release evidence, rather than in SDK crates.

Preserve source provenance and attribution. See [LICENSE](LICENSE),
[downstream licensing](docs/downstream-licensing.md), and the
[asset provenance records](docs/asset-provenance.md).
