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
psoxide-test bless [--keep-speed] # measure the reference build, write the baseline (speed rows
                                  # record their layouts' sha256 and search stamp and the window
                                  # identity; --keep-speed keeps them, only when every row came
                                  # from searched layouts or none and its identity is unchanged)
psoxide-test lint                 # load the file and every journey; pads and open-loop counts
psoxide-test install [--rebase-facts] # copy bytes a complete full PASS judged into the library
psoxide-test install-check ...    # the install guarantee for a script's install (see Gate profiles)
psoxide-test pin [--write]        # the SDK pin rule for this repo
```

Machine-local paths (gate, frontend, ReARMed core, Redux, library, original game files, the
budget) live in the user config, never in the repo: `~/.config/psoxide-test/config.toml` or
`PSOXIDE_TEST_CONFIG`; `config.example.toml` lists every key.

## What a repo declares

| Section | Says |
|---|---|
| `[[build]]` | a build: `commands` (the repo's own entry points, `{out}` is its output dir), `collect`ed files, the `disc` cue, the link `map`; `env` with `{input:NAME}` original files and `{tool:NAME}`; `inputs` path prefixes for a build keyed on its sources alone (a cook); `members` for a disc image's manifest; `images` for other programs it links beside its own map (the scene core, a boot scene), whose symbols the harness writes as `<image>:<name>` into `images.sym`; `exe` names the executable `map` is of on a disc with more than one, and the install records it (`artifacts.exe`) with every image's map and executable (`artifacts.images`) |
| `[[size]]` | the build whose map is judged: text, data, bss and free RAM against the baseline (tolerance 0 unless a `reason` is given); `image` judges one of its `images` instead (baseline entry `<build>:<image>`); `max_bytes` is an absolute ceiling on text + data + bss |
| `[[speed]]` | a window on a poll-bound tape (`tape` + `polls`) or between two game-state conditions (`from` + `until`), measured at phase `offsets`; `models = ["mister"]` adds a row per model; `anchor` (RAM conditions) with `anchor_max_polls` counts the tape and the window from the poll the game's own state first holds at instead of from power-on, so a splash or boot core in front moves neither (`psoxide-test tape-anchor` re-anchors a power-on tape) |
| `[[load]]` | load time on the PSoXide frontend, in VBlanks (and bus cycles): from power-on (no `from`) or a game state `from` to the state `until` (the game's first own frame for a boot, the next scene's first state for a scene load), one input-free or taped replay stopped at `max_polls`; never more VBlanks than the baseline (`max_growth` needs a `reason`) and, at install, never more than the installed build's facts (`--accept-slower` with the owner's words, or `--rebase-facts` for a row that measures something else). Measured in the speed stage |
| `[journeys]` | gate journeys (`files`), the quick tier's `precheck`, enforced conformance rules, `hw`, `only_on`, `instances` (templates played once per required member) |
| `[cross]` | smoke journeys replayed on PCSX-ReARMed and PCSX-Redux, RAM checkpoints only; `boot` of a shipping disc with no input. `boot.until` (RAM conditions on the build's map) with `boot.after` proves the game itself started: the run waits up to `frames` for them, reports the frame they held at, and counts `min_distinct` over the `after` frames only, so a splash or boot core in front cannot pass it alone |
| `[[host]]` | host checks: a command that exits 0, optionally a `metric`; `inputs` and `uses` key it on what it reads |
| `[[clean]]` | a fresh clone of the pushed commit rebuilds these files byte for byte |
| `[[install]]` | the library entry a build becomes |
| `[[requires]]` | results this verdict stands on (a disc image's members): each a complete full PASS of the bytes the image carries |

Unknown keys are errors. A tolerance or an exception needs its reason in the file (a human changes
a bar by editing the file, never the baseline). Journeys say `pad` explicitly and wait on game
state, not on time: open-loop steps are a ratchet in the baseline that only goes down.

## RAM checks count only once the program has started

Until psx-rt's `_start` has cleared `.bss`, a `.bss` symbol holds whatever RAM held before the
program: on a program of the scene core, the bytes of its stored `.boot` and core image, which the
EXE puts where `.bss` will be. A check such as `wait_until = { sym = "HITS", op = "ge", value = 1 }`
read in those first instructions can hold before the program has run a line. psx-rt therefore keeps
`__psx_rt_started` in `.data`: `"pre_"` (0x5F657270) as the EXE stores it, `"run_"` (0x5F6E7572) once
`.bss` is clear. Every condition set psoxide-test resolves carries `__psx_rt_started eq run_` beside
its own when the build's map has the symbol (`journey::with_started`): every journey step
(`journey::plan`, the `[cross]` smokes), `[[speed]]` window ends and anchors, `[[load]]` `from` (a
non-empty one; an empty `from` is power-on) and `until`, and `[cross]` boot `until`. So no sample taken
before the start can satisfy one. A run whose maps give the marker two addresses (two program maps
listed for one run) is refused with the maps named: list the map of the program the run checks.

Not covered here: `[journeys]` handed to the PSoXide emulator's `psoxide-gate` (its own runner; the
change it needs is an emulator-repo item), and any tool of a game that reads a route log or RAM itself
(a tape anchor, a replay cue): each applies the same rule, a row before the marker reads `run_` is not
the program's state. Until psoxide-gate has it, a journey adds the condition by hand: first step
`wait_until = { kind = "ram", sym = "__psx_rt_started", op = "eq", value = 0x5F6E7572 }`.

A program that starts through another runtime (the psyq layer of 360X and QD Up) has the rule only if
it defines the same marker: a `u32` named `__psx_rt_started`, `#[no_mangle]`, in `.data` (never
`.bss`), stored as 0x5F657270 and written 0x5F6E7572 right after its own `.bss` clear, before anything
else runs. Without it psoxide-test cannot tell its tick-0 rows from its own state.

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
| speed rows | every row: the window (the console model, `real`) and each `models` row | the window's `real` row only; each `<name>@<model>` row is a skip naming the landing profile, not measured |
| speed: work per frame, frames presented | mean worse-by over the offsets <= the blessed phase spread | the same, plus `landing_work_slack_permille` thousandths of the blessed mean (per `[[speed]]` window, default 50 = 5%): work may rise and flips may drop by that much |
| speed: p99 | judged as above | reported as a skip (value and baseline kept), not judged |
| frames by vblank count | reported, not judged | the same |
| result JSON | `"schema": 1`, no `profile` key (byte-identical to results before profiles) | `"schema": 2`, `"profile": "landing"`; the verdict line reads `LANDING-PROFILE PASS ... NOT an install pass` |

`--profile landing` is refused outside `run --tier full`, and with `--all-models`. A disc image run
on the install profile refuses a `[[requires]]` member whose result is landing-profile.

The install guarantee (`install`, and `install-check` for installs made by a script) refuses
unless the result is a complete, clean, full PASS on the install profile, with a result
`schema` this harness knows (a newer one is refused), and refuses a result slower than the build
installed before it. Only a psoxide-test from this revision on may install: older binaries
ignore the `profile` key and would install a landing result.

Never slower: for every speed row the installed build was judged on whose identity matches, over
the offsets both measured, mean work per frame and p99 not above and mean frames presented not
below the installed build's, within its own phase spread (the strict tolerance, no slack). Such
a row the new result lacks is refused. The facts live beside the installed disc as
`<Name>.speed-facts.json`, bound to the disc they describe:

```
{"schema": 2, "result_sha256": "<hex>", "bin_sha256": "<hex>", "map_sha256": "<hex>"|null,
 "windows": {"<row>": {"offsets": [-12, 0, 12], "flips": [...], "work_per_frame": [...],
   "frame_work_p99": [...],
   "identity": {"tape": "<path>", "tape_sha256": "<hex>", "window": "polls A..B",
                "offsets": [-12, 0, 12], "launch_args": [...], "gate_revision": "<sha>"}}}}
```

`window` is `polls A..B`, or `located from <cond> until <cond> max_polls N` for a window found by
conditions; the tape is hashed at the result's commit. A result does not carry the frontend's or
psoxide-pgo's sha256, so the emulator is named by the gate revision. The facts are compared only
when they describe the library's bin (`bin_sha256`) and the row's identity is unchanged. When they
cannot be compared (a disc in the library without facts, schema-1 facts, facts of another bin, a
moved window, a new tape or gate) the install is refused; `--rebase-facts` takes the strict PASS
as the new reference instead and says so loudly. It never waives a comparison that can be made.

`install` reads the library's own facts: none and no disc of that name is the first install;
a disc without facts is refused. A script's install runs `install-check`:

```
psoxide-test install-check --result RESULT.json --repo GAME_CLONE --bin BIN [--map MAP]
    (--facts LIBRARY/<Name>.speed-facts.json --library-bin LIBRARY/<Name>.bin | --first-install)
    [--rebase-facts] [--write-facts OUT.json] [--stamp OUT.json]
```

- `--result`: a result file named `<sha256 of its content>.json`, as `run` writes it; any other
  name is refused.
- `--repo`: a clone of the game holding the result's commit; the game file, layouts and tapes are
  read there, at that commit.
- `--bin`, `--map`: the files about to be installed. The bin must be an `[[install]]` build's bin
  as the result's build stage recorded it, the map that build's recorded map (`--map` is needed
  exactly when the build declares a map).
- `--facts`, `--library-bin`: the library's facts and bin. Missing facts are refused (a typo must
  not disable never-slower) unless `--first-install` says there are none; `--first-install` is
  itself refused when either path exists.
- `--write-facts` writes the new facts; `--stamp` writes, on success only, the marker a deploy
  script checks before it copies a bin (it copies only a bin whose sha256 is the stamp's):

```
{"schema": 1, "result_sha256": "<hex>", "bin_sha256": "<hex>", "map_sha256": "<hex>"|null,
 "profile": "install", "checked_at": "2026-11-22T12:34:56Z", "facts": {<the new facts>}}
```

Exit 0 only on PASS.

A build slower than the installed one goes in only with the owner's approval in words:
`install --accept-slower "<who approved, and what>"` (and the same flag on `install-check`).
It waives only the never-slower comparisons that were made and failed; each one, with its
numbers, goes into the install record with the text (`owner_approval` in the provenance and the
stamp) and is printed loudly. Every other refusal stands, and an approval nothing needed is not
recorded. With `--rebase-facts` on facts that cannot be compared, the text is recorded with why
nothing was measured.

The install also needs a searched layout. Every committed psoxide-pgo layout profile of the
installed builds must carry the search stamp that `psoxide-pgo search` writes as its second
line (format defined in tools/psoxide-pgo, README "The search stamp"):

```
# psoxide-pgo layout 1
# psoxide-pgo searched: rounds <N> seed <N>
```

with at least 600 rounds (`MIN_SEARCH_ROUNDS`, the psoxide-pgo README's recipe): a search
stopped after a round or two carries the same stamp and places like no search.

The layouts are read at the result's commit: a `[[build]]`'s `layout_files` (paths in the repo),
or when it names none every tracked `*.layout` file whose first line is `# psoxide-pgo layout
1`. A game with no layout profile passes; a build with `layout = false` has none. A layout from
`order` or `layout` (greedy), or one searched before the stamp existed, is refused: run the
game's layout search on the commit (`psoxide-pgo search ... --start recorded` continues from the
committed placement), commit the layout, bless, and run the full tier on that commit.

The stamp says where the search placed the code; the build stage proves the ship link put it
there. A `[[build]]` may name `order`, the ordering file its link followed, inside `out`, with
the `<order>.placed` sidecar `psoxide-pgo place` writes collected beside it. On every run the
build stage runs `psoxide-pgo followed <order> <map>` (the psoxide-pgo of the game's pin) and
fails unless every ordered function is in order; the check reports how many placed functions
start where the model put them. The install build (`[[install]] build`) of a game whose
committed layouts include a searched one must name `order`: a driver that links by itself and
falls back to plain order would otherwise ship a stamp the disc does not follow.

## Receipts and the SDK pin

Every stage keys a receipt on what it reads (artifacts, journey files, tapes, tool binaries,
declared inputs), never on paths or the clock, so a pin move that leaves the bytes identical
re-runs only the build and the clean clone. A repo may pin any SDK commit whose guest tree equals
SDK main's: the `sdk` stage checks it first (`psoxide-test pin`).

Original game files (`{input:NAME}`) enter keys by content (a git checkout by commit plus its
dirty files). Every child the harness starts leads its own process group and is killed when its
job returns, on a signal, or by a small reaper if the harness itself is killed.
