# pgo-search-flow

One script, `search-flow.sh`, that gives a game a `psoxide-pgo search` layout (the one
`psoxide-test install` accepts: its header carries `# psoxide-pgo searched: rounds N seed N`),
rebuilds the shipping link with it, and proves the shipping map follows it. Host only: it lives
outside the guest paths, so it moves no game's guest tree, and it runs the game's own pinned
psoxide-pgo (`.psoxide/tools/psoxide-pgo`), so the layout it writes is the format that pin reads.

```text
search-flow.sh --config GAME.conf --repo GAME_CLONE --work DIR --frontend FRONTEND
               [--from first|seed|trace|search|ship|also|check] [--reseed] [--threads N] [--plan]
```

`--plan` prints the resolved config (and flags a missing tape) and runs nothing. `--work` must be
outside the repository. `FLOW_EMU_WRAP` prefixes every emulator run (a machine slot wrapper,
say); `FLOW_PGO` replaces the pinned psoxide-pgo. `--from` resumes at a step, reusing what the
earlier steps left in `--work`.

Run it on a clone hydrated in place (the game's `make psoxide` or equivalent). Do not copy
`.psoxide/target` from another checkout: the host tools built there keep that checkout as their
SDK root, so the architecture gate in the first build refuses every SDK crate as game code.
Give the clone's `origin` the game's own repository URL (the gate names the program
`repo/package` from it).

## Steps

1. **first**: the unordered link of the shipping code (`FIRST_BUILD`), packed as a disc. Its map
   and patched image are what `place` sees at the start of the ship build, so the search scores
   placements on them. Exe and map are copied to `DIR/first`; for a glob, the newest file the
   build wrote wins (a game may keep one map per configuration in one directory).
2. **seed**: the layout the search starts from. The committed layout serves when it binds to the
   first link (`psoxide-pgo place`); otherwise, or with `--reseed`, `SEED_BUILD` re-collects the
   greedy counts. A copy goes to `DIR/seed.layout`.
3. **trace**: one fetch trace per window, on the first-link disc (a trace binds by portable name
   and code hash, so the unordered disc of the same code serves). A short replay finds the route
   tick of the window's first poll, then the traced replay runs to its last poll. Windows run one
   at a time, one emulator.
4. **search**: `psoxide-pgo search` over every window and weight, `ROUNDS` rounds (default 600)
   at `SEED`, written over `LAYOUT` in the repository. No early stop: the search runs every round,
   and the flow refuses `ROUNDS` under `MIN_ROUNDS` (600), because a 1-round search writes a stamp
   the install check accepts while its placement can measure no better than no layout. The flow
   then checks the stamp names exactly these rounds and seed.
5. **ship**: the game's shipping build (`SHIP_BUILD`, the harness's `[[build]] ship` command),
   now linking the searched layout.
6. **also**: every other build that links the same layout (`ALSO_BUILDS`: a gate or test disc
   with other features) must still link. A searched layout records exact sections, and `place`
   refuses one whose code differs in that build, where a greedy layout only bound a little lower.
7. **check**: `psoxide-pgo place` writes the order the searched layout gives the first link,
   independently of the game's build, and `psoxide-pgo followed` checks the ship map against it:
   every ordered function in order, and at most `MAX_DRIFT` (default 0) placed functions away
   from the address the search scored. A build that falls back to the default order when `place`
   refuses, a build that never checks its relink, and a `LINKER_SCRIPT` other than the one the
   ship link uses all fail here, where a stamped layout would otherwise claim a placement the
   disc does not have.

On success it prints what comes next; it commits and blesses nothing:

```text
git add <LAYOUT> && git commit
psoxide-test run --tier quick      speed against the current baseline
psoxide-test bless                 the searched layout's baseline
psoxide-test run --tier full       then install
```

The traces in `DIR/trace` are working files (100 MB or more per window of 1,000 polls): delete
them once the layout is committed.

## A game's config

A shell file the script sources; keep it in the game's repository or beside the lane that runs
it. Build commands run in the repository with `FLOW_OUT` (where to put a disc), `FLOW_FRONTEND`,
`FLOW_REPO` and `FLOW_WORK` set. Paths are repository-relative; `*_EXE` and `*_MAP` may be globs;
`*_DISC` names the `.cue` and may use `$FLOW_OUT`.

| key | what |
| --- | --- |
| `GAME` | name, for the log |
| `LAYOUT` | the committed layout profile the ship build links |
| `LINKER_SCRIPT` | the script the ship link uses (checked after the first build) |
| `FIRST_BUILD`, `FIRST_EXE`, `FIRST_MAP`, `FIRST_DISC` | the unordered link of the ship code, packed |
| `SHIP_BUILD`, `SHIP_EXE`, `SHIP_MAP`, `SHIP_DISC` | the shipping build, as the harness runs it |
| `SEED_BUILD` | optional: re-collects greedy counts into `LAYOUT` |
| `ALSO_BUILDS` | optional, one per line: `name command`, every other build linking `LAYOUT` |
| `LAUNCH_ARGS` | frontend arguments of every replay (the speed windows' `launch_args`) |
| `WINDOWS` | one per line: `name tape from-poll to-poll weight [extra launch args]`; tape `-` for none |
| `ROUNDS`, `SEED`, `BATCH`, `THREADS`, `START` | search settings (600, 1, 24, 4, greedy) |
| `MAX_DRIFT` | placed functions allowed away from the searched address (0) |

Take the windows from the game's `psoxide-test.toml` speed rows, so the layout is fitted to what
the install check measures, and use poll-bound tapes only, so every build replays the same
gameplay.

For a game that builds through `psoxide-pgo apply` with an `+order` variant (the Makefile pattern
in `tools/psoxide-pgo/README.md`):

```sh
GAME=mygame
LAYOUT=pgo/mygame.layout
LINKER_SCRIPT=.psoxide/sdk/psoxide.ld      # what build.rs passes with -T
# The compile part of the shipping variant (here off+order), so its code is what `place` sees.
FIRST_BUILD='make compile PGO_VARIANT=off && make pack PACK_OUT="$FLOW_OUT/mygame.bin"'
FIRST_EXE=game/target/mipsel-sony-psx/release/mygame.exe
FIRST_MAP='game/target/mipsel-sony-psx/psoxide-pgo-maps/*.map'
FIRST_DISC='$FLOW_OUT/mygame.cue'
SHIP_BUILD='make compile && make pack PACK_OUT="$FLOW_OUT/mygame.bin"'
SHIP_EXE=game/target/mipsel-sony-psx/release/mygame.exe
SHIP_MAP='game/target/mipsel-sony-psx/psoxide-pgo-maps/*.map'
SHIP_DISC='$FLOW_OUT/mygame.cue'
SEED_BUILD='make pgo-order FRONTEND="$FLOW_FRONTEND"'
LAUNCH_ARGS='--embedded-playtest'
WINDOWS='
train  pgo/train.pxtape  400 1200 1
unseen pgo/unseen.pxtape 400 1200 2
'
```

A game with no layout yet needs the `+order` hooks first (`PSOXIDE_LINK_ORDER` in `build.rs`,
`--layout` on `apply`, an `order` target to seed from), and its free RAM checked with the ordered
link.
