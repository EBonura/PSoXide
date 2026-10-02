# PSoXide website

The public site for the PSoXide SDK, editor, emulator and PlayStation projects:
[ebonura.github.io/PSoXide](https://ebonura.github.io/PSoXide/). It lives in the
SDK repository under `website/` and deploys from `.github/workflows/website.yml`
at the repository root. Run the commands below from this directory.

Built with **Zola 0.23.6**. The data importers, checks and builds are Rust tasks
in [`tools/xtask`](../tools/xtask): `cargo run -p xtask -- site <task>`, from
anywhere in the repository (`site --help` lists them).

Next work: [first-game tutorial, embedded emulator and measurements](ROADMAP.md).

```sh
zola serve
# Production build and deterministic checks:
zola build
cargo run -q -p xtask -- site check --base-url https://ebonura.github.io/PSoXide/
```

The Pages workflow checks generated data, builds with the actual Pages base URL,
checks internal links, assets, fragments and review flags, then deploys on pushes
to `main`. The browser example gallery hosts the pinned emulator and six SDK executables. The main Play button links to the demo disc on itch.io.

## Editing content

Follow the [writing guide and research notes](WRITING.md) for site copy.

- `content/`: guides and prose.
- `data/projects.toml`: project descriptions, public links, screenshots and
  individual games inside collections.
- `data/demo-disc.json`: verified public release contents, used for the contents
  list and all demo-disc badges. See [inventory evidence](data/demo-disc-source.md)
  before updating it; build options and old release descriptions can differ from
  the published image.
- `data/videos.toml`: published Bonnie Studios videos, verified on 29 September 2026.
- `data/compat-source.md`: snapshot of the emulator compatibility report; run
  `cargo run -q -p xtask -- site import-compat` after replacing it.
- `data/emu-bench/summary-final.csv`: recorded benchmark input; run
  `cargo run -q -p xtask -- site import-emu-bench` after replacing it. Figures describe the
  recorded builds, not an assertion about the latest emulator. Speed charts are
  disabled in `config.toml` until a new comparison is ready.
- `data/accuracy.toml`: no cross-emulator accuracy results yet. Placeholder rows
  remain out of the rendered site.
- `data/sdk.json`: the SDK crate and example index, generated from `../sdk/README.md`
  by `cargo run -q -p xtask -- site import-sdk`. Edit the README, not the JSON.
- `data/media-provenance.toml`: screenshot sources and fresh capture hashes.

Use original captures, label emulator versus console evidence, and verify that
links work without signing in. Do not publish planned video titles as working links.
Do not add BIOS files, commercial game data or disc images.

## Visual review

On macOS with Google Chrome installed (`CHROME=/path/to/chrome` elsewhere):

```sh
cargo run -q -p xtask -- site browser-check
cargo run -q -p xtask -- site review-shots
cargo run -q -p xtask -- site usability --set after --json review/usability.json
```

Each accepts `--zola /path/to/zola`. The usability routes are in
`scripts/usability_routes.toml`. Review outputs stay under ignored `review/`.
The browser check covers seven pages at four widths in both themes, broken
images, horizontal overflow, JavaScript errors, FAQ deep links, theme persistence
and the emulator page. The screenshot script produces 28 full-page captures
and a contact sheet. These are browser checks, not a new emulator-accuracy run.

## Media credits

Game and editor captures belong to the credited projects and their respective
asset creators. Celeste Classic Collection is an unofficial fan port; see the
site's about and licensing page for attribution. The VT323 font is by the VT323 Project Authors
and is distributed with its [SIL Open Font License](static/fonts/OFL.txt).
Videos link to [Bonnie Studios](https://www.youtube.com/@bonnie-studios-dev).

Earlier addresses (`ebonura.github.io/`, `/psoxide-site/` and the root-level
pages) redirect here from the `EBonura/ebonura.github.io` repository, which now
holds only those redirects.

## Runnable examples

`data/examples.toml` pins the SDK and emulator sources and lists the six programs
allowed in the browser gallery. Pages builds the player from the exact emulator
commit, compiles the examples from the exact SDK commit, then runs
`site stage-examples`. Generated WASM, EXEs and build records are ignored
by Git and included only in the deployed site. Both builds retain their source
links and licence notices. No demo-disc or commercial-game data is staged.

For a local preview, build the player from a checkout of the pinned emulator
with `cargo run -q -p xtask -- site player --emulator /path/to/pinned-emulator
--out /tmp/player`, build each listed SDK example with `make disc
EXAMPLE=<name>`, then run from `website/`:

```sh
cargo run -q -p xtask -- site stage-examples --player /tmp/player \
  --sdk /path/to/pinned-sdk \
  --examples /path/to/pinned-sdk/build/examples/mipsel-sony-psx/release
zola serve
```

Use `example_player` in a guide to place a player beside its explanation.
`static/js/examples.js` handles pause/resume, restart, fullscreen, offscreen
pausing, load failures and unloading the previous instance when a new one starts.
The iframe performs click-to-load, starts muted, and accepts commands only from
its same-origin parent. There are no touch controls or in-browser compiler.

Before publishing a new pin, check all six programs, keyboard focus, pause,
restart, switching examples, the missing-file fallback and mobile layout.
Physical gamepads and console behaviour require separate checks. The manual
website workflow can build a feature branch; only `main` deploys to Pages.

## SDK documentation

`content/docs/crates/` contains a usage and architecture guide for every SDK
workspace member; `content/docs/examples/` contains build notes and complete
source listings for every SDK example. `site import-sdk-reference` reads
the manifests and sources into `data/sdk-reference.json`, using the SDK revision
in `data/examples.toml`. It rejects source changes relative to that pin and
requires a guide for every crate and example. After updating the pin, regenerate
the data and review the prose. CI checks it with `--check` against the pinned
checkout.

Build the API files before running the normal site/link check:

```sh
cargo run -q -p xtask -- site import-sdk-reference --sdk-root /path/to/pinned-sdk
cargo run -q -p xtask -- site sdk-docs --sdk-root /path/to/pinned-sdk --target-dir /tmp/psoxide-sdk-api
zola build
cargo run -q -p xtask -- site check --base-url https://ebonura.github.io/PSoXide/
```

The API builder uses the pinned nightly to document all SDK crates for the PS1
target, with all Cargo features, and the GTE crates for the host. Shared repository dependency
documentation is included so local API links resolve. Generated rustdoc files
under `static/api/` are ignored by Git and published by Pages. This generates
documentation, not an execution test of every example or a console validation.
