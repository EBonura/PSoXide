# PSoXide website

The public site for the PSoXide SDK, editor, emulator and PlayStation projects:
[ebonura.github.io](https://ebonura.github.io/).

Built with **Zola 0.23.6**. Python scripts require Python 3.11 or newer.

Next work: [first-game tutorial, embedded emulator and measurements](ROADMAP.md).

```sh
zola serve
# Production build and deterministic checks:
zola build
python3 scripts/check_site.py --base-url https://ebonura.github.io/
```

The Pages workflow checks generated data, builds with the actual Pages base URL,
checks internal links, assets, fragments and review flags, then deploys on pushes
to `main`. No browser player or game data is hosted here; Play links to itch.io.

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
  `python3 scripts/import_compat.py` after replacing it.
- `data/emu-bench/summary-final.csv`: recorded benchmark input; run
  `python3 scripts/import_emu_bench.py` after replacing it. Figures describe the
  recorded builds, not an assertion about the latest emulator. Speed charts are
  disabled in `config.toml` until a new comparison is ready.
- `data/accuracy.toml`: no cross-emulator accuracy results yet. Placeholder rows
  remain out of the rendered site.
- `data/media-provenance.toml`: screenshot sources and fresh capture hashes.

Use original captures, label emulator versus console evidence, and verify that
links work without signing in. Do not publish planned video titles as working links.
Do not add BIOS files, commercial game data or disc images.

## Visual review

On macOS with Google Chrome and Pillow installed:

```sh
python3 scripts/browser_check.py
python3 scripts/review_shots.py
python3 scripts/usability_check.py --set after --json review/usability.json
```

Each accepts `--zola /path/to/zola`. Review outputs stay under ignored `review/`.
The browser check covers seven pages at four widths in both themes, broken
images, horizontal overflow, JavaScript errors, FAQ deep links, theme persistence
and the emulator and legacy URL redirects. The screenshot script produces 28 full-page captures
and a contact sheet. These are browser checks, not a new emulator-accuracy run.

## Media credits

Game and editor captures belong to the credited projects and their respective
asset creators. Celeste Classic Collection is an unofficial fan port; see the
site's about and licensing page for attribution. The VT323 font is by the VT323 Project Authors
and is distributed with its [SIL Open Font License](static/fonts/OFL.txt).
Videos link to [Bonnie Studios](https://www.youtube.com/@bonnie-studios-dev).

The former `/psoxide-site/` routes redirect to the root site. Their small HTML
files in `static/psoxide-site/` retain query strings and anchors when JavaScript
is enabled, with a meta-refresh and link fallback. Keep these for shared links.
