# Downstream game projects: canonical structure

How a game repo built on PSoXide should be laid out and wired. This is the
written-down version of the layout that six sibling projects converged on
(oot-psx defined it; hl-psx, voxide, psxcel, gh-psx, zelda3-psx/alttp-psx and
pico8-psx each follow it to varying degrees), captured during the 2026-07
cross-project audit so the next project starts consistent instead of
converging by copy-paste.

## Repo layout

```
<game>/
  README.md              what it is, how to build, current status
  LICENSE                see "Licensing" below
  Makefile               the whole build surface (verbs below)
  rust-toolchain.toml    nightly + rust-src/llvm-tools/rustfmt/clippy,
                         kept in sync with PSoXide's own toolchain file
  .gitignore             see template below
  game/                  the PSX-EXE crate (its own workspace, see below)
    Cargo.toml  Cargo.lock  build.rs  .cargo/config.toml  src/
  tools/                 host-side cookers/extractors (own workspaces)
  docs/                  design notes, feasibility, screenshots
  dist/                  build output (gitignored)
  psoxide-pin/           host bootstrap crate + committed Cargo.lock
  .psoxide/             generated source cache (gitignored)
```

## Consuming PSoXide

For new projects, use a **Cargo-pinned bootstrap**. A small host crate (commonly
`psoxide-pin/`) depends on `psoxide-link` at a full Git revision and commits its
`Cargo.lock`. Before building guest crates, it calls
`psoxide_link::hydrate_pinned` to populate a local `.psoxide/` source cache.
Guest manifests then use paths into that cache. The manifest revision,
bootstrap revision argument and lockfile must agree.

The implementation is in [psoxide-link](../tools/psoxide-link/src/lib.rs).
[Half-Life](https://github.com/EBonura/hl-psx) and
[VoXide](https://github.com/EBonura/voxide) are downstream examples of this
workflow. Inspect their current Makefiles before adopting game-specific paths.

For co-development, the game's `PSOXIDE_FROM` override can hydrate a local
checkout instead. This is also how the demo disc builds its ordinary programs
against one tested SDK revision. Local overrides must be explicit; record
which source was actually used when comparing or distributing builds.

A pinned Git submodule remains valid for existing consumers. Initialize the
recorded commit, never advance it implicitly with `git submodule update
--remote` during a release build. Do not advertise a submodule pin while
silently compiling against a different sibling checkout.

Bootstrap is a separate step because Cargo resolves path dependencies before
running `build.rs`. Keep `.psoxide/` ignored and do not manually edit it: the
next hydration may replace it.

## The `game/` crate

- `[workspace]` (empty table): a standalone workspace, deliberately NOT the
  repo root, so the PSX target config cannot leak onto host tools and cargo
  does not adopt the SDK path-deps as implicit members.
- `game/.cargo/config.toml`:

  ```toml
  [build]
  target = "mipsel-sony-psx"

  [unstable]
  build-std = ["core"]
  build-std-features = ["compiler-builtins-mem"]
  ```

- `game/build.rs`: inject the linker script by absolute path plus
  `--oformat=binary` (flat PSX-EXE). Copy it from any sibling project; it is
  byte-identical across all of them (resolve the PSoXide root per your
  consumption mode, then `-T<psoxide>/sdk/psoxide.ld`).
- Release profile: `panic = "abort"`, `codegen-units = 1`, `lto = true`.
  Exception: a bin that links several game lib crates (the Celeste
  collection shape) needs `lto = "thin"`; fat LTO garbage-collected the
  cross-crate `run()` entry points and shipped a 23 KiB EXE with the games
  missing.
- Features:
  - `emulator-telemetry` (default off): gates the PSoXide-only MMIO
    telemetry writes so `frontend launch` can stop on guest frames and dump
    profile CSVs. Off for burns; calls compile to no-ops.
  - Debug boot shortcuts: ONE feature (e.g. `debug-map-boot`) plus a
    scenario id read at boot, hl-psx style. Do not grow one cargo feature
    per QA scenario; features are additive by design and these are mutually
    exclusive switches (zelda3-psx reached 48 features this way, since
    flagged for consolidation).

## Makefile verbs

Same names everywhere, so muscle memory transfers between projects:

| Verb | Does |
| --- | --- |
| `help` | list targets (default goal or first target) |
| `bootstrap` | resolve and hydrate the pinned SDK, or the explicit local override |
| `build` | cargo build the game EXE |
| `disc` | `build` + mkisopsx pack into `dist/<game>.bin/.cue` |
| `install` | copy the disc into the PSoXide game library (`$HOME/Downloads/ps1 games/`, destination overridable) |
| `run` | `install` + boot in the PSoXide frontend (or alias of install) |
| `clean` | remove build output; do NOT also delete capture archives |
| `release` | see "Releasing" |

mkisopsx runs from the PSoXide checkout
(`tools/mkisopsx && cargo run --release -- --exe <exe> --out dist/<game>.bin
--volume <VOLUME>`), plus `--world-pack-rooms-dir` / `--cdda-track-list` /
`--world-pack-compress-rooms` as the game needs.

## Releasing (historical pico8-psx example)

This describes the earlier committed-artifact workflow. Check the target
repository's current release instructions before adopting it.

The one released project's flow, reusable as-is:

1. `VERSION` file at the root is the version of record (player-facing
   changelog lives in `release/README.txt`).
2. `make release` builds + packs + copies `dist/*.bin/.cue` into `release/`,
   which IS tracked: `.gitignore` ignores `*.bin`/`*.cue` globally, then
   un-ignores `!/release/*.bin` etc. with a comment saying why.
3. Committing `release/**` triggers `.github/workflows/deploy.yml`, which
   pushes `./release` to itch.io via butler with
   `--userversion "$(cat VERSION)"`.
4. No PS1 toolchain on CI. The disc is built and verified locally (emulator
   + hardware); CI only ships the committed artifact.

Pin the nightly by date in `rust-toolchain.toml` when you cut a release, so
the shipped binary is reproducible.

## .gitignore template

```gitignore
/dist/
/.psoxide/
/game/target/
/captures/
.DS_Store
# Agent/editor context stays local
CLAUDE.md
AGENTS.md
.claude/
.codex/
.cursor/
```

Plus per-project entries (ROMs and extracted assets are never committed:
`rom/*` + `!rom/.gitkeep`, `data/`, `*.sfc`, ...). If the asset cook scripts
in `tools/` are required by Makefile targets, track them; an ignore pattern
that leaves `make assets` broken on a fresh clone is a bug.

## Licensing

PSoXide is GPL-2.0-or-later; a distributed game binary links SDK/engine code,
so GPL obligations apply to that code and your modifications
([downstream-licensing.md](downstream-licensing.md) has the full picture).
Public game repos should carry a LICENSE file saying what the game's own
code/content is under and pointing at the PSoXide obligation. Note also
per-asset requirements: e.g. embedding `psx-font`'s Spleen face requires the
BSD-2-Clause notice (`sdk/crates/psx-font/vendor/spleen-LICENSE`) in binary
distributions.

## Use the SDK, don't re-derive it

The 2026-07 audit found the same helpers rewritten in up to five projects.
They are SDK API now; new projects should reach for these before writing a
local version:

| Need | API |
| --- | --- |
| just-pressed / released edges, menu auto-repeat, handoff suppression | `psx_pad::PadTracker` |
| numbers as text without `core::fmt` | `psx_math::fmt` |
| angle of a vector (facing/steering) | `psx_math::atan2_q12` |
| TV screen-position option | `psx_gpu::set_display_offset` |
| true vblank wait in a raw loop | `psx_rt::interrupts::wait_vblank` |
| WORLD.PAK parsing + HLZC decode | `psx-pack` |
| memory-card saves (+ compression) | `psx-mc` |
| streamed SPU audio loop chaining | `psx_spu::Voice::set_loop_addr` |
| noise SFX | `psx_spu::Voice::set_noise_mask` + `set_noise_clock` |
| deterministic RNG, particles, screen shake | `psx-fx` |
| keyed residency pools with LRU + pinning | `psx-cache` |
| SIO0 register bits (custom peripherals) | `psx_hw::sio::sio0` |

The engine's `Ctx` covers the input needs when you adopt the App/Scene loop;
`PadTracker` exists for raw-loop games.

If you find yourself writing something a second project will also want,
propose it for the SDK instead: the flow that produced `psx-fx` (extracted
from breakout + invaders) and everything in the table above.
