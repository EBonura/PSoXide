# psoxide-test

One test harness for every program on the SDK. A repo declares what to build and check in
`psoxide-test.toml` at its root; the harness decides how: stage order, content receipts, the
emulator budget, pass and fail against the committed baseline (`psoxide-test.baseline.toml`), one
result file per run, the clean-clone proof and the install.

## The one command

```
psoxide-test run --tier full      # everything a landing needs; exit 0 only on LANDING: COMPLETE PASS
psoxide-test run                  # the quick pre-check (build, size, one window, precheck journeys)
psoxide-test run --stage journeys # one stage (the build always runs first, usually as a receipt)
psoxide-test bless                # measure the reference build, write the baseline
psoxide-test lint                 # load the file and every journey; pads and open-loop counts
psoxide-test install              # copy bytes a complete full PASS judged into the library
psoxide-test install-check ...    # the install guarantee for a script's install (see Gate profiles)
psoxide-test pin [--write]        # the SDK pin rule for this repo
```

Machine-local paths (gate, frontend, ReARMed core, Redux, library, original game files, the
budget) live in the user config, never in the repo: `~/.config/psoxide-test/config.toml` or
`PSOXIDE_TEST_CONFIG`; `config.example.toml` lists every key.

## What a repo declares

| Section | Says |
|---|---|
| `[[build]]` | a build: `commands` (the repo's own entry points, `{out}` is its output dir), `collect`ed files, the `disc` cue, the link `map`; `env` with `{input:NAME}` original files and `{tool:NAME}`; `inputs` path prefixes for a build keyed on its sources alone (a cook); `members` for a disc image's manifest |
| `[[size]]` | the build whose map is judged: text, data, bss and free RAM against the baseline (tolerance 0 unless a `reason` is given) |
| `[[speed]]` | a window on a poll-bound tape (`tape` + `polls`) or between two game-state conditions (`from` + `until`), measured at phase `offsets`; `models = ["mister"]` adds a row per model |
| `[journeys]` | gate journeys (`files`), the quick tier's `precheck`, enforced conformance rules, `hw`, `only_on`, `instances` (templates played once per required member) |
| `[cross]` | smoke journeys replayed on PCSX-ReARMed and PCSX-Redux, RAM checkpoints only; `boot` of a shipping disc with no input |
| `[[host]]` | host checks: a command that exits 0, optionally a `metric`; `inputs` and `uses` key it on what it reads |
| `[[clean]]` | a fresh clone of the pushed commit rebuilds these files byte for byte |
| `[[install]]` | the library entry a build becomes |
| `[[requires]]` | results this verdict stands on (a disc image's members): each a complete full PASS of the bytes the image carries |

Unknown keys are errors. A tolerance or an exception needs its reason in the file (a human changes
a bar by editing the file, never the baseline). Journeys say `pad` explicitly and wait on game
state, not on time: open-loop steps are a ratchet in the baseline that only goes down.

## Reading the verdict

```
NitroXide at 1d85d7a618c1 (SDK aaf06b0ffa5d, guest tree equals main 297f088666fe, gate f280c6e711d9, tier full)
  sdk       PASS        1 pass   0 fail   0 skip  run             1 s
  build     PASS        4 pass   0 fail   0 skip  receipt         0 s
  journeys  PASS      166 pass   0 fail   8 skip  receipt         0 s
      pass journey rows  164 row(s): 0 gate process(es) ..., 164 harness receipt(s), 0 carried ...
  ...
  skips: 8 (journeys 8)
VERDICT PASS
LANDING: COMPLETE PASS (every required tier ran on these bytes and passed)
```

- One line per stage: verdict, check counts, `run` / `receipt` / `run+receipt` (where the answer
  came from) and wall time. A red stage prints every check with the file, the condition and the
  value it read.
- `RECEIPT HIT` lines above the table name each stage or row answered from a receipt: same bytes,
  same verdict, nothing started. `journey rows` says how many gate processes ran and how many rows
  came from receipts or were carried from an older gate (carried rows are never a fresh PASS).
- `STANDS ON REAL` lines and the `standing on real` check list the rows of another model that were
  not played: the journey's `real` row passed and its gate's `SIGNALS` line shows none of the events
  that model treats differently (no GP0(1Fh) executed or written for `mister` and `gpu-irq`, no
  broken CD acknowledge for `cd-ack=strict`, no MDEC command for `mdec-req=hidden`), and the journey
  names no `golden_on`. The emulator's docs/conformance.md ("Model equivalence") gives the reason;
  `run --all-models` plays every model of every journey (the audit).
- Every skip is a check with its reason, counted per stage on the `skips:` line.
- `LANDING: COMPLETE PASS` needs `--tier full` on one set of bytes with every declared section;
  otherwise the last line lists what is missing.
- The result is `<work>/<game>/results/<sha256>.json`, named by its content; wall times are in
  `<sha256>.timing.json` beside it. `install` and `[[requires]]` name results by that hash.

## Gate profiles and the install guarantee

**Needs the owner's approval before any lane uses `--profile landing`.** Without `--profile` a full
tier is exactly what it was before profiles existed (the result file is byte-identical: an
install-profile result writes no `profile` key).

```
psoxide-test run --tier full                     # = --profile install: the strict full tier
psoxide-test run --tier full --profile landing   # lighter, opt-in, never installable
```

| | `install` (default) | `landing` |
|---|---|---|
| journeys | every model in `FULL_MODELS` | `real` only; every other model is a skip row naming the landing profile (never a pass) |
| speed: work per frame, frames presented | mean worse-by over the offsets <= the blessed phase spread | the same, plus `landing_work_slack_permille` thousandths of the blessed mean (per `[[speed]]` window, default 50 = 5%): work may rise and flips may drop by that much |
| speed: p99 | judged as above | reported as a skip (value and baseline kept), not judged |
| frames by vblank count | reported, not judged | the same |
| result JSON | no `profile` key | `"profile": "landing"`; the verdict line reads `LANDING-PROFILE PASS ... NOT an install pass` |

`--profile landing` is refused outside `run --tier full`, and with `--all-models`. A disc image run
on the install profile refuses a `[[requires]]` member whose result is landing-profile.

The install guarantee (`install`, and `install-check` for installs made by a script) refuses
unless the result is a complete, clean, full PASS on the install profile, and refuses a result
slower than the build installed before it: for every speed row the installed build was judged
on, over the offsets both measured, mean work per frame not above and mean frames presented not
below the installed build's, within its own phase spread (the strict tolerance, no slack). A row
the installed build had and the new result lacks is refused. The facts live beside the installed
disc as `<Name>.speed-facts.json`:

```
{"schema": 1, "result_sha256": "<hex>",
 "windows": {"<row>": {"offsets": [-12, 0, 12], "flips": [...], "work_per_frame": [...]}}}
```

No facts file means the first install under this rule: accepted, and the facts are written with
the disc. `psoxide-test install-check --result R.json --facts INSTALLED.json --repo GAME_CLONE
[--write-facts OUT.json]` applies the same check (exit 0 = may install) and writes R's facts for
the caller to store. `--repo` is a clone of the game holding R's commit: the layouts are read
there, at that commit.

The install also needs a searched layout. Every committed psoxide-pgo layout profile of the
installed builds must carry the search stamp that `psoxide-pgo search` writes as its second
line (format defined in tools/psoxide-pgo, README "The search stamp"):

```
# psoxide-pgo layout 1
# psoxide-pgo searched: rounds <N> seed <N>
```

The layouts are read at the result's commit: a `[[build]]`'s `layout_files` (paths in the repo),
or when it names none every tracked `*.layout` file whose first line is `# psoxide-pgo layout
1`. A game with no layout profile passes; a build with `layout = false` has none. A layout from
`order` or `layout` (greedy), or one searched before the stamp existed, is refused: run the
game's layout search on the commit (`psoxide-pgo search ... --start recorded` continues from the
committed placement), commit the layout, bless, and run the full tier on that commit.

## Receipts and the SDK pin

Every stage keys a receipt on what it reads (artifacts, journey files, tapes, tool binaries,
declared inputs), never on paths or the clock, so a pin move that leaves the bytes identical
re-runs only the build and the clean clone. A repo may pin any SDK commit whose guest tree equals
SDK main's: the `sdk` stage checks it first (`psoxide-test pin`).

Original game files (`{input:NAME}`) enter keys by content (a git checkout by commit plus its
dirty files). Every child the harness starts leads its own process group and is killed when its
job returns, on a signal, or by a small reaper if the harness itself is killed.
