# Demo-disc dependencies and repository separation

Snapshot: 5 September 2026. Inspected the demo-disc checkout at `76ea4195`,
its pinned game manifests and Makefiles, and the pinned Quake input. These
are dependency observations, not a rebuild or a migration. Other work may
advance the pins after this snapshot.

## Consumer matrix

Every guest below uses SDK code. Additional dependencies refer to build-time
source/tool requirements, not programs loaded alongside it on the PS1.

| Consumer | Inspected revision | Engine / host dependencies beyond SDK | Combined-disc form |
| --- | --- | --- | --- |
| VoXide | `5c119eca` | No direct engine dependency in game manifest; emulator invoked for run/smoke/profile | Whole image; streams assets |
| NitroXide | `c11b8a75` | `psx-engine`; arena/model cookers use `psxed-format`, `psxed-tex`, `psxed-gltf` | Whole image; streams assets |
| PSXcel | `ff9025e7` | `psx-engine`; emulator invoked for rendering/run | Bare EXE |
| Celeste collection | `66f900ea` | Host audio-capture tool directly links `emulator-core` and `psoxide-settings`; shared disc formats | Bare collection EXE |
| GH-PSX | `37b5b655` | `psx-engine`; emulator for run; game-specific chart tools | Whole image; demo build explicitly supplies an empty game-CDDA list |
| PSoXide Arcade | `3ddaf7f0` | Breakout, Invaders and Magikarp Pong use `psx-engine`; nested launcher/loader and its own packer | Whole collection image; nested chain-loads and CDDA |
| Quake shareware | `263f43f9` | `psx-engine`, BSP/render contracts, `psxed-audio` in cooker | Separately built and hash/provenance-verified image |
| Half-Life | `50778a1e` | `psx-engine`, render contracts, `psxed-audio`; game-specific asset cookers and external game data | Whole image in HL edition; runtime reads and 27 audio tracks |
| Cortex 0.4b | tools/game pin `b77a0547` | Full editor/cooker, engine and gameplay runtime; authored project and assets | Separately pinned full project image with world/UI packs and CDDA |
| Hardware tests | runtime pin `8df242b3` | `psx-engine` and SDK example `hello-memcard`; suite currently lives under `engine/examples` | Whole test image |
| Demo launcher and loader | disc `76ea4195` | Local `carousel`/`disc-toc`; launcher/loader link SDK directly | Top-level executable and embedded chain-loader |
| Demo packer and validation | disc `76ea4195` | `mkdisc` uses `psx-iso`; Python validators invoke a pinned emulator executable | Builds and verifies final layout |

The SDK also has transitive format dependencies currently stored under
`editor/`, so “no direct engine dependency” does not mean the current SDK
folder can already be copied out independently.

Evidence comes from each game's tracked `Cargo.toml` files and Makefile, not
from the size or contents of its hydrated `.psoxide` cache. For example,
Celeste's emulator dependency belongs to `tools/psx-audio-capture`, not its
PS1 executable. Nitro's editor dependencies belong to `tools/cook-arena` and
`tools/cook-models`; it does not need the editor UI to compile those tools.

## What the disc owns

The demo-disc repository remains a fourth, existing integration/release
repository alongside SDK, emulator, and editor + engine + Cortex. It owns:

- the carousel, launcher, chain-loader and disc table/layout;
- the exact source revisions and asset inputs selected for a build;
- shared overrides for ordinary games, plus explicit Cortex/Quake exceptions;
- the emulator executable used for release validation;
- receipts, relocation checks, audio mapping and final image hashes.

Today it has three PSoXide submodule paths: `games/PSoXide`,
`games/PSoXide-runtime`, and `games/PSoXide-cortex-current`. The normal runtime
source supplies SDK, engine, tools and emulator from one checkout. Ordinary
games receive `PSOXIDE_FROM`; HL's cooker gets the equivalent `--psoxide`.
Cortex builds from its own pinned editor/game source. Quake is consumed from
an independently pinned artifact with its own expected PSoXide revision.

After separation, one overloaded `PSOXIDE_FROM` cannot accurately name all
those inputs. Replace it with explicit component locations/revisions and keep
an adapter for existing consumers during migration. The release build should
fail on missing components or inconsistent pins rather than selecting a nearby
sibling checkout.

## Source ownership versus distribution

Keep Cortex with the editor, as requested. Engine and cooker libraries in
that repository must nevertheless be usable by other games without compiling
the editor UI or fetching the full Cortex asset collection.

Provide a slim, reproducible **engine/cooker source package** from the
editor repository's revision, with its SDK requirement and content hashes.
It is a distribution of that repository, not a fourth source repository for
engine code. This matters because merely using a Git dependency on an engine
crate still fetches its containing repository. The package build must include
workspace metadata, transitive dependencies and required fixtures, not just
copy a few crate directories.

Similarly, expose `emulator-core`/settings as libraries for capture tools,
and a pinned headless executable for smoke/replay checks. Ordinary disc
packing must not require opening the editor or emulator UI. Cortex authoring
and fresh asset cooking are separate build prerequisites.

The hardware suite should remain an explicitly owned integration fixture:
retain its engine/SDK dependencies and pin both. Moving it into an SDK-only
checkout without those dependencies would break the very validation intended
to protect the split.

## A disc-wide compatibility lock

Introduce one versioned release lock in the demo repository that records:

- SDK revision, toolchain/linker and SDK package hash;
- editor/engine/cooker revision and slim package hash;
- Cortex project identity and its tools/runtime/SDK tuple;
- emulator revision, features and executable hash;
- every game source revision, required component tuple and cooked-input hashes;
- packer/launcher/loader revision and build features;
- final EXE/BIN/CUE hashes, placement and relocated audio-track mapping.

This extends the existing checks rather than replacing them with an unverified
manifest. Keep standalone game pins for reproducibility. Disc overrides must
be explicit, compatibility-checked and recorded as the effective inputs.
Statically linked guests may retain separately validated component tuples;
Cortex and Quake already have separate lanes. Do not pretend that one SDK
revision automatically describes every guest or every host tool.

Compatibility includes the loader/disc-base protocol, executable placement,
shared data formats and CDDA relocation. On a PS1, the emulator and editor
are build/test tools, not runtime services required by the pressed game.

## Migration acceptance: both editions, every entry

1. Build a normal standalone candidate for every guest, without diagnostic
   features. Test SDK-only, engine-using and host-cooker consumers separately.
   Include Celeste's audio-capture tool and Nitro's model/arena cookers.
2. Rebuild the demo launcher, loader, packer and hardware suite against their
   explicit dependencies. Rebuild Cortex from the editor/game repository.
   Verify Quake and HL cooked provenance against their effective components.
3. Pack both standard and HL editions. Check each EXE entry and every embedded
   image boundary, data sector base, entrypoint and payload identity.
4. Exercise all carousel entries. Enter both Celeste games and all three
   Arcade games, including their nested loader paths. Repeat the existing
   Cortex, Quake, hardware and HL deterministic checks.
5. Verify runtime reads and every relocated CDDA track. HL's 27 source audio
   tracks and standalone offsets can remain unchanged while their combined
   physical track numbers change. Check the actual final layout and source
   bytes; also include menu music and Arcade's nested track mapping.
6. Compare relevant RAM/VRAM budgets, MIPS hazards, forbidden guest symbols,
   gameplay replays and performance to the pre-split baseline. Follow with
   original-hardware smoke checks before treating the new release as proven.

Preserve current normal-release feature sets; repository extraction is not an
opportunity to silently change runtime behavior, rendering or content.

## Source references

- [Demo Makefile at the inspected revision](https://github.com/EBonura/PSoXide-demo-disc/blob/76ea4195310b6800333dffa96338b80bccb80a69/Makefile)
- [Demo receipt verifier](https://github.com/EBonura/PSoXide-demo-disc/blob/76ea4195310b6800333dffa96338b80bccb80a69/tools/release_receipt.py)
- [Nitro cooker manifest](https://github.com/EBonura/nitroxide/blob/c11b8a751c878227e0146fe3be205df78d182320/tools/cook-models/Cargo.toml)
- [Celeste emulator capture tool](https://github.com/EBonura/celeste-collection-psx/blob/66f900eaef662513c286da72e9190ef1f289b75d/tools/psx-audio-capture/Cargo.toml)
- [Hardware-suite dependencies](../engine/examples/hardware-tests/Cargo.toml)
- [Cargo Git dependencies](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#specifying-dependencies-from-git-repositories)
