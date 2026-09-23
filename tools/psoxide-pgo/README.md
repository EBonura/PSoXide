# psoxide-pgo

Profile-guided optimisation for PS1 guests, driven by the emulator instead of
an instrumented build. PSoXide counts every guest instruction, so a replay of
an input tape gives an exact PC histogram; this tool maps it through the
guest's DWARF into an LLVM sample profile (AutoFDO text) and rebuilds the
guest with `-Zprofile-sample-use`.

hl-psx measured +9.3% rendered FPS on the route that trained the profile and
+5.9% on one that shared no map with it. VoXide measured 2-3% *more* work per
frame, so PGO is not a free win: every game picks its variant with `choose`,
and `off` is always one of the candidates.

## What a game adds

One profile file in the repository and three Makefile targets. Everything else
(build flags, the ELF twin, replays, conversion, renaming, the link map, the
hazard patcher and scanner, the stack guard) lives here, so a fix reaches
every game with its next SDK pin.

```make
# The guest's cargo arguments, exactly as its normal build passes them.
GAME_CARGO  = build --release --features "$(FEATURES)"
PGO         = cargo run -q --release --locked --manifest-path "$(PSOXIDE)/Cargo.toml" -p psoxide-pgo --
# Committed, portable.
PGO_PROFILE = pgo/mygame.prof
# The winner from `make pgo-choose`, or off.
PGO_VARIANT = default
# Gameplay windows in port-1 polls, read once from a --route-log of each replay.
TRAIN_POLLS  = <from>..<to>
SECOND_POLLS = <from>..<to>
UNSEEN_POLLS = <from>..<to>

# Every build: CI, itch, demo disc.
compile:
	$(PGO) apply --crate game --profile "$(PGO_PROFILE)" --variant "$(PGO_VARIANT)" -- $(GAME_CARGO)

# Regenerate the committed profile.
pgo-collect:
	$(PGO) collect --crate game --frontend "$(FRONTEND)" \
		--tape tapes/route-a.pxtape --polls $(TRAIN_POLLS) \
		--tape tapes/route-b.pxtape --polls $(SECOND_POLLS) \
		--pack 'make pack EXE="$$PSOXIDE_PGO_EXE" OUT="$$PSOXIDE_PGO_DISC"' \
		--launch-arg --embedded-playtest \
		--out "$(PGO_PROFILE)" -- $(GAME_CARGO)

# Build every variant and gate it.
pgo-choose:
	$(PGO) choose --crate game --profile "$(PGO_PROFILE)" \
		--variant off --variant default --variant accurate --variant hot=1000 \
		--pack 'make pack EXE="$$PSOXIDE_PGO_EXE" OUT="$$PSOXIDE_PGO_DISC"' \
		--gate '"$$PSOXIDE_PGO" measure --frontend "$(FRONTEND)" --image "$$PSOXIDE_PGO_IMAGE" \
			--launch-arg --embedded-playtest --tape tapes/route-a.pxtape --polls $(TRAIN_POLLS) --name train \
		&& "$$PSOXIDE_PGO" measure --frontend "$(FRONTEND)" --image "$$PSOXIDE_PGO_IMAGE" \
			--launch-arg --embedded-playtest --tape tapes/unseen.pxtape --polls $(UNSEEN_POLLS) --name unseen' \
		-- $(GAME_CARGO)
```

**Judge gameplay, not loading.** Every tape above carries a `--polls FROM..TO`
window: the port-1 polls between the end of the loads and the end of the
route, found once from a `--route-log` of the replay (its `port1_polls`
column against the CD activity or the screen). A poll is one simulation tick,
so the same window covers the same gameplay in builds of any speed, which a
route-tick window would not. Training on it keeps CD polling loops and menus
out of the profile; gating on it keeps load times out of the verdict.

`apply` leaves the patched executable where cargo always puts it, so the
game's pack step does not change. With `--variant off` it is the plain build
plus the post-link tools, so a game can route its build through `apply`
before it has a profile.

Every link the driver makes writes its ld.lld map (`-Clink-arg=-Map`, which
does not change the emitted bytes) to
`<target>/mipsel-sony-psx/psoxide-pgo-maps/<hash>.map`, the hash naming the
crate, the cargo arguments and the rustflags, so a build cargo finds fresh
still has the map of its own link. The patcher and scanner get it as
`--map`, which proves every jump table instead of guessing from the
dispatch's block, and `tools/stack_guard.py` gets it to prove every
scratchpad stack call tree fits its region. A `-Map` the guest's own
`build.rs` adds comes later on the link line and wins; the driver then warns
and runs the tools without a map (and the stack guard refuses an image that
switches to a scratchpad stack).

Two conventions the guest must follow:

- **Rustflags live in `[target.mipsel-sony-psx] rustflags`** (in
  `.cargo/config.toml`, or passed with `--config` in the cargo arguments).
  The driver appends its flags with `--config`, which joins that list.
  `RUSTFLAGS` replaces every config list, so the driver refuses to run when it
  is set, and `build.rustflags` is ignored whenever a target list exists, so a
  guest that keeps its flags there loses them. If the profiling flags never
  reach rustc, the driver stops: the twin has no line tables.
- **The ELF twin.** The driver links one build as an ELF to read its DWARF.
  It appends `-Clink-arg=--oformat=elf` and sets `PSOXIDE_LINK_ELF=1`. If the
  guest's `--oformat=binary` comes from rustflags, the later flag wins and
  nothing is needed. Cargo passes build-script link arguments after rustflags,
  so a `build.rs` that adds it must skip it when the variable is set:

  ```rust
  println!("cargo:rerun-if-env-changed=PSOXIDE_LINK_ELF");
  if std::env::var_os("PSOXIDE_LINK_ELF").is_none() {
      println!("cargo:rustc-link-arg=--oformat=binary");
  }
  ```

  The driver checks the output and says so if the guest forgot.

Nothing needs ignoring in git: the work directory is
`<target>/mipsel-sony-psx/release/psoxide-pgo` unless `--work` says otherwise.

## Modes

```text
psoxide-pgo collect [GUEST] --frontend PATH [--tape PATH [--polls A..B]]...
                    [--launch-arg ARG]... [--pack CMD] --out PROFILE -- CARGO-ARGS...
psoxide-pgo apply   [GUEST] [--profile PROFILE] [--variant V] -- CARGO-ARGS...
psoxide-pgo choose  [GUEST] --profile PROFILE --gate CMD [--variant V]... [--pack CMD]
                    -- CARGO-ARGS...
psoxide-pgo measure --frontend PATH --image PATH [--tape PATH] --polls A..B
                    [--launch-arg ARG]... [--name NAME]
GUEST: [--crate DIR] [--work DIR] [--patcher PATH] [--scanner PATH]
```

`CARGO-ARGS` is what follows `cargo` in the guest's own build, starting with
`build`. `--crate` is where cargo runs (default: the current directory).
`--patcher` and `--scanner` default to this SDK's `tools/hazard_patch.py` and
`tools/hazard_scan.py`; a game with its own patcher passes it here. Paths may
contain spaces; `--pack` and `--gate` are shell commands, quoted by the caller.

### collect

1. Builds the guest once with `-Cdebuginfo=1 -Zdebug-info-for-profiling
   -Cstrip=none` as an ELF, keeps it as `<work>/<name>.elf`, and cuts the flat
   PSX-EXE from it (the same bytes `ld.lld --oformat=binary` writes, so every
   sampled PC is an address in the ELF by construction). The profiling flags
   do not change the code; the flat image runs and measures like a plain
   build.
2. Hazard-patches and scans the image and runs the stack guard, all with the
   twin's link map, and runs `--pack` if given, with
   `PSOXIDE_PGO_EXE` (the image) and `PSOXIDE_PGO_DISC` (a `.bin` path to
   write; the driver launches its `.cue` sibling when there is one).
3. Replays each `--tape` with `frontend launch --pc-sample-log
   --pc-sample-instructions 61` (a prime interval, so the sampler cannot fall
   into step with a loop) plus every `--launch-arg`. A tape replay stops when
   the tape runs out; the driver adds `--steps 40000000000` as a cap unless a
   launch argument sets `--steps`. With no tape, one run uses the launch
   arguments alone.

   A `--polls FROM..TO` after a tape (or on its own, for the tapeless run)
   keeps only gameplay samples. The frontend cannot start `--pc-sample-log`
   late, so the driver samples in 30-route-tick windows
   (`--pc-sample-window-log`), maps ticks to polls through a `--route-log` of
   the same replay, keeps the windows wholly inside the poll range, and stops
   the replay at `TO`. Up to one window at each end is lost to the rounding.
4. Sums the histograms, converts them, writes the portable profile to `--out`,
   and deletes the PC and route logs, the collect image and the disc.

After `collect` the exe at cargo's path is the ELF twin, which does not boot;
run `apply` (or the game's normal build) next.

### apply

Builds the ELF twin in *this* checkout, rebinds the portable profile onto its
symbols (`rebind` prints how many names bound and how many are missing), then
builds with the collect flags, `-Zprofile-sample-use=<rebound>` and the
variant's flags, and runs the hazard patcher, scanner and stack guard with
the link map. Their output is never piped, and a non-zero exit stops the
build: a swallowed failure once shipped an unpatched hl-psx exe.

Variants, joined with `+` to combine (`accurate+hot=1000`):

| variant    | extra flags                                   |
|------------|-----------------------------------------------|
| `off`      | none, and no profile: the plain build         |
| `default`  | the profile alone                             |
| `accurate` | `-Cllvm-args=-profile-sample-accurate`: code the profile never saw is treated as cold |
| `hot=N`    | `-Cllvm-args=-hot-callsite-threshold=N`: the inline budget of a call the profile calls hot (LLVM's default is 3000) |
| `noreplay` | `-Cllvm-args=-disable-sample-loader-inlining`: do not replay the profiled build's inlining; inlinee samples merge into their own functions |
| `nopgso`   | `-Cllvm-args=-pgso=false`: do not optimise profile-cold code for size |
| `profi`    | `-Cllvm-args=-sample-profile-use-profi`: infer block counts where samples are missing |
| `llvm=-F`  | `-Cllvm-args=-F`, any other LLVM option (`llvm=-sample-profile-inline-size`) |

A variant that fails to build shows as a failed row in `choose` instead of
stopping it.

### choose

Builds each `--variant` in turn, packs it if `--pack` is given, and runs
`--gate` with `PSOXIDE_PGO_VARIANT`, `PSOXIDE_PGO_EXE`, `PSOXIDE_PGO_IMAGE`
(the disc when packed, else the exe) and `PSOXIDE_PGO` (this tool, for
`measure`). The gate's exit status is pass or fail; every `key=value` line it
prints (no spaces in the value) becomes a column.

With no `--variant`, the candidates are `off`, `default`, `hot=500`,
`hot=500+profi`, `accurate+nopgso+hot=1000` and `accurate+nopgso+hot=1500`:
the winners so far were `hot=500+profi` on VoXide 895cb60 (see the table
under `measure`), `hot=1000` on hl-psx and cs-psx, and `accurate` with
`-pgso=false` and `hot=1500` on Cortex (see "Troubleshooting").

When the gate prints `work_cycles` (as `measure` does), `choose` ranks the
passing rows by them, fastest first, and adds a `work` column: each row's
work cycles against the `off` row, averaged over the gate's replays so every
tape counts the same. Failed rows go last, unranked. Without `work_cycles` the
rows stay in build order.

The gate is the game's own judgement, so it should replay a training tape
*and* one the profile never saw, and check correctness (hashes, poll-bound
state) as well as speed. PSoXide-editor's
`docs/measuring-guest-performance-2026-09-17.md` explains why final-frame
hashes alone mislead. Commit the winner as the game's `PGO_VARIANT`, `off`
included.

### measure

Replays `--image` and prints `NAME.key=value` lines for a gate. The first
group covers the route ticks that ran wholly inside the `--polls` window
(one route tick of resolution at each end):

| key      | meaning |
|----------|---------|
| `ticks`  | route ticks (vblanks) the window took: lower is faster for a guest that never waits on vblank |
| `flips`  | ticks in which the display start changed: rendered frames, for a guest that renders at most once per tick |
| `cycles` | bus cycles in those ticks |
| `icache` | I-cache refill stall cycles in those ticks |
| `frame_p50`, `frame_p95` | bus cycles from one flip to the next (the median and 95th percentile): how long each frame stayed on screen, which is what a player sees |
| `vblanks` | how many route ticks each frame stayed on screen, as `vblanks:frames` pairs (`2:937,3:6` is 937 frames at 30 fps and 6 at 20) |
| `vram`, `display` | the frontend's `--dump-hash` at the stop: equal across builds only when the guest's simulation does not depend on its own speed (VoXide's `lockstep` feature, for example) |

A game locked to the display (every frame two vblanks, like VoXide or
NitroXide) spends its slack spinning in `wait_vblank`, `draw_sync` or a DMA
poll, so `ticks` and `cycles` come out the same for a faster and a slower
build. The second group subtracts the waiting. It covers everything from the
start of the window's first tick to the stop (the first flip after poll `TO`):

| key      | meaning |
|----------|---------|
| `work_cycles` | bus cycles spent outside wait loops: the number `choose` ranks by |
| `work_instr` | instructions retired outside wait loops |
| `wait_cycles` | cycles inside wait loops: their instructions plus the MMIO and RAM-load stalls charged to them |
| `wait_share` | `wait_cycles` as a share of all cycles in that span |
| `work_per_frame` | `work_cycles` over the frames presented in that span |

Wait loops are found in the code itself, not by name, so psx-rt's waits, a
game's own (Quake's `gpu_end_frame`, VoXide's `frame_present`, HL's `play`)
and every PGO layout of either are covered by one rule: a small loop, closed
by a backward branch or a `j`, with no store, call or GTE work in it, whose
loads all read an address the loop never changes (LLVM would have hoisted a
plain load, so these are volatile: a hardware register or a counter an
interrupt writes), and whose branches depend only on those loads, on values
fixed for the loop or on a spin counter. A loop that is a piece of a larger
one, holds an inner loop, or branches on a register it carries round in any
other way is work. The rule and its tests are in `src/work.rs`. Everything
else is work, interrupt handlers included. Each wait loop above 0.1% of the
span's instructions is listed on stderr, so a new game's first run can be
checked against its source.

This costs a second replay: the per-line logs can only start at a route tick,
so a short first replay (to 30 polls past `FROM`) finds the tick in which
poll `FROM` lands, and the full one logs every retired instruction per
16-byte I-cache line (`--pc-line-log`) with the MMIO and RAM-load stalls per
line, then dumps RAM for the code. `measure` stops if the two replays
disagree at that tick. The frontend's own output goes to stderr so it cannot
land in the table.

Two approximations, both small next to the differences a variant makes:
a line the loop touches counts as wait in full (the instructions sharing it
run once per call, not once per iteration), and a wait loop's I-cache and
other stalls stay in `work_cycles` (a spin loop stays cached). A guest that
renders a different number of frames per build (NitroXide without a
lockstep build drew 394 to 396 in the same polls) does more work for the
extra frames; compare `work_per_frame` there too.

On VoXide 895cb60 (lockstep, polls 252..1200 of both tapes), `measure` ranked
the five variants of the hand-built loop-body harness (telemetry builds,
`frame_present`'s waits excluded) in the same order on both tapes:

| variant           | work cycles, train | loop body, train | work cycles, unseen | loop body, unseen |
|-------------------|-------------------:|-----------------:|--------------------:|------------------:|
| off               | 918,203,048        | 974,573          | 729,358,977         | 778,546           |
| default           | +0.60%             | +0.65%           | +1.68%              | +1.87%            |
| hot=500           | -0.28%             | -0.33%           | +0.33%              | +0.35%            |
| hot=500+profi     | -2.35%             | -2.59%           | -2.50%              | -2.69%            |
| accurate+hot=500  | -0.82%             | -0.86%           | -0.44%              | -0.42%            |

`ticks` and `cycles` could not tell them apart on the training tape (1,913
or 1,914 ticks each).

### What the knobs did on VoXide

VoXide at 29117ac (delay-slot flags on) with its `lockstep` feature, so every
build reaches the same state at every poll (display hashes matched in every
row). Profile from the recorded tape's gameplay polls 252..1200; work
instructions are everything executed after route tick 600 except
`frame_present`'s vblank wait, counted exactly (`--pc-sample-instructions 1`).
The unseen tape is a different walk the profile never saw.

| variant         | work instr, train | work instr, unseen | I-cache stalls, train | exe bytes |
|-----------------|------------------:|-------------------:|----------------------:|----------:|
| off             | 489,506,085       | 484,544,541        | 84,176,815            | 483,328   |
| default         | 504,901,379       | 499,651,874        | 79,727,602            | 516,096   |
| hot=225         | 502,387,481       | 496,702,726        | 76,157,506            | 471,040   |
| hot=500         | 501,930,820       | 496,213,814        | 76,905,048            | 479,232   |
| hot=1000        | 506,490,617       | 501,107,607        | 77,352,944            | 485,376   |
| nopgso+hot=225  | 501,775,828       | 496,093,825        | 80,650,454            | 473,088   |
| hot=500+profi   | 498,891,678       | 493,996,539        | (not recorded)        | 491,520   |

LLVM's hot-callsite budget of 3000 is what grows the image (+32 KB here; on
Cortex it overflowed RAM), and 225-500 takes all of that back and cuts
I-cache stalls below the plain build's. It does not take back the extra
executed instructions: every profiled variant still ran 1.9-3.1% more. In
the face loop (a third of the frame) the profiled build executed the same
ALU work plus 6.7M more nops, 6.0M more stack loads and stores and 1.6M more
jumps: layout and register allocation spending the profile's block counts
badly. `profi` recovers about a third of it. memcpy calls rose from 3,822 to
5,642 in the window, about two per frame, and are not the cost. On VoXide
`off` still wins, and `choose` exists to say so per game.

## Why the committed profile is portable

Profile names are the build's mangled symbols, and every Rust symbol carries
its crate's disambiguator. Cargo derives that from the absolute path of each
path dependency outside the workspace (a game's `.psoxide/sdk` crates), so the
same commit checked out somewhere else names every function differently and
LLVM would match none of a raw profile. `collect` therefore writes names
without disambiguators (`hello_gte::step_entity`, with ` @crate` appended for a
generic instance), and `apply` maps them back onto the local build. That is
what lets CI, itch and the demo disc (which rebuilds every game against one
SDK with `psoxide-link --from`) apply the profile a developer committed.

Cargo features change the disambiguators too (they feed the same hash), and
portable names drop them the same way: a profile collected without a feature
binds every name in a build with it. The code behind those names can differ,
though, so `collect` writes the features it trained with as the profile's
first line (`# psoxide-pgo features: ...`, a comment to LLVM), and `apply`
warns when the build's features differ. Keep one committed profile per
shipped feature set (`pgo/mygame.prof`, `pgo/mygame-monsters.prof`).

A profile goes stale as code changes: renamed or removed functions show up as
`missing` in `rebind`'s count, and LLVM ignores lines that moved. Regenerate
after large changes, and whenever the game repins onto a different SDK.

## Profile quality notes

- **Head samples** (a function's entry count) come from the samples at its
  first instruction. Until 2026-09-22 the line-table lookup gave that address
  to the end of the previous function's sequence, so every head was 0.
- **Unrolled copies.** A loop body the unroller copied N times carries
  duplication factor N in its discriminator, and each copy runs N times less
  often than its source line. Counts are scaled by it, as AutoFDO does; the
  summary line reports how many samples that touched (none on VoXide or the
  SDK bench, whose builds unroll nothing hot).
- **Unmapped samples** are reported by cause, with the functions they sit in,
  and none of them can reach LLVM usefully:
  - *Line 0* is code the compiler made up or merged (loop counters, hoisted
    common code). On a CPU-bound SDK guest it was 4.8% of samples, on VoXide
    15.5%, mostly inside its face loop. Emitting it under its own key built a
    byte-identical bench exe; carrying each line-0 sample forward to the
    previous line made VoXide execute 1.7% *more* instructions than dropping
    them, so they stay dropped.
  - *No DWARF* is psx-rt's assembly `memcpy`/`memset` and the hazard
    trampolines (2.8% on the bench, 0.2% on VoXide). The sample loader only
    annotates functions it compiles from IR; adding records for the assembly
    built a byte-identical exe.

## Troubleshooting

- **A profiled build loses, and it calls memcpy more.** With a profile, LLVM
  optimises the code the profile calls cold for size (profile-guided size
  optimisation, PGSO), and one thing it does there is turn fixed-size struct
  copies into `memcpy` calls. On Cortex, under `accurate` (which calls
  everything the profile never saw cold), that made 346 memcpy call sites
  against 115 with no profile, for copies of 20 to 96 bytes. Only `nopgso`
  (`-pgso=false`) took them back out; `-pgso-cold-code-only` and the PGSO
  cutoff options changed nothing. Cortex's winner was
  `accurate+nopgso+hot=1500` trained on gameplay polls: +1.9% fps, 1.2% fewer
  work instructions and 15% fewer I-cache stalls. Count `jal` to memcpy in the
  disassembly of the two builds before blaming the profile.
- **`ticks` and `cycles` are the same for every variant.** The game is locked
  to the display; rank by `work_cycles` (which `choose` does when the gate
  prints it) and check `vblanks` for frames that got slower.
- **`measure` reports a wait loop that is not one, or misses one.** Its
  addresses are on stderr; look them up in the link map. The rule is in
  `src/work.rs` with a test per pattern it accepts or rejects; add the new
  shape there.

## Lower-level commands

```text
psoxide-pgo <elf-with-dwarf> <pc.csv>... <out.prof>        convert (sums several logs)
psoxide-pgo portable <in.prof> <out.prof>                  strip disambiguators
psoxide-pgo rebind <in.prof> <target-elf-with-dwarf> <out.prof>
```

The same hl-psx commit built from two checkout paths differed by about 0.4%
in FPS, because the path changes the code layout. Build every candidate from
one directory, and treat smaller differences as unproven without a cycle
breakdown.
