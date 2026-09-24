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

`order` is a second lever: it lays functions out for the R3000's 4 KB
direct-mapped I-cache from exact per-word counts, and any variant takes it
as `+order` (see "order"). Its gains are small and per game, so it is one
more candidate for `choose`, never a default.

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

A third applies only to a game that tries `+order`:

- **The link order.** An `+order` variant relinks with an ordering file the
  driver names in `PSOXIDE_LINK_ORDER`, set only for that link. It changes
  no rustflags, so only the final crate relinks, and the guest's `build.rs`
  passes it to ld.lld, in place of any ordering file of its own (ld.lld
  keeps only the last):

  ```rust
  println!("cargo:rerun-if-env-changed=PSOXIDE_LINK_ORDER");
  if let Some(order) = std::env::var_os("PSOXIDE_LINK_ORDER") {
      println!("cargo:rustc-link-arg=--symbol-ordering-file={}", order.to_string_lossy());
  }
  ```

  The file's name holds a hash of its contents, so a new order is a new
  value and cargo reruns the build script. The driver reads the relinked
  map and stops if the order was not followed, which is what a guest
  without the hook gets.

Nothing needs ignoring in git: the work directory is
`<target>/mipsel-sony-psx/release/psoxide-pgo` unless `--work` says otherwise.

## Modes

```text
psoxide-pgo collect [GUEST] --frontend PATH [--tape PATH [--polls A..B]]...
                    [--launch-arg ARG]... [--pack CMD] --out PROFILE -- CARGO-ARGS...
psoxide-pgo order   [GUEST] --frontend PATH (--tape PATH --polls A..B)... [--launch-arg ARG]...
                    [--pack CMD] [--profile PROFILE] [--variant V] --out LAYOUT -- CARGO-ARGS...
psoxide-pgo apply   [GUEST] [--profile PROFILE] [--layout LAYOUT] [--variant V] -- CARGO-ARGS...
psoxide-pgo choose  [GUEST] --profile PROFILE [--layout LAYOUT] --gate CMD [--variant V]...
                    [--pack CMD] [--frame-budget VBLANKS] [--rank deadline|work] -- CARGO-ARGS...
psoxide-pgo measure --frontend PATH --image PATH [--tape PATH] --polls A..B
                    [--launch-arg ARG]... [--name NAME] [--wait-range START..END]...
GUEST: [--crate DIR] [--work DIR] [--patcher PATH] [--scanner PATH] [--stack-guard PATH]
       [--linker-script PATH]
```

`CARGO-ARGS` is what follows `cargo` in the guest's own build, starting with
`build`. `--crate` is where cargo runs (default: the current directory).
`--patcher`, `--scanner` and `--stack-guard` default to this SDK's
`tools/hazard_patch.py`, `tools/hazard_scan.py` and `tools/stack_guard.py`; a
game with its own copy, or a wrapper that hands the tool the map its own
`build.rs` asks the link for, passes it here. Every tool refuses a map that
does not match the image. `--linker-script` (default: this SDK's
`sdk/psoxide.ld`, which is the one a game's `.psoxide` links) tells `+order`
which sections the script places ahead of its catch-all `*(.text .text.*)`,
where no ordering file can move them. Paths may contain spaces; `--pack` and `--gate` are shell commands, quoted by the caller.

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

### order

Collects the layout profile an `+order` variant places from, on the variant
it will join:

1. Builds `--variant` as `apply` does, without any `+order`, with its link
   map, and packs it if `--pack` is given.
2. Replays each `--tape` over its `--polls` window with a count for every
   word (`--pc-log-words --pc-line-log`), located as `measure` locates its
   window, and sums the counts. The frontend must list `--pc-log-words`.
3. Writes `--out` (commit it as `pgo/<game>.layout`): the features and
   variant it was collected on, then per function that ran its portable name
   (the one `rebind` uses, free of crate hashes), a hash of its code, its
   size, the count of every word that ran, and its direct calls: each
   `jal` or `j` word that ran and lands in another function (through a hazard
   trampoline, too) with its count. A `jalr` calls through a register, so
   indirect calls are invisible; the summary line counts how many ran.

The code hash masks what the linker fills in (`j`/`jal` targets, `lui`
halves and the low halves added to them), so linking in another order keeps
every hash, and a change to an instruction's opcode, registers, branch
offset or other immediates changes it.

Placement (`src/layout.rs`, ported from the study's conflict model): a
caller keeps the lines of the innermost loop around its call sites live
across every call, and each callee line is live for its own count over the
calls. Where the two share an I-cache set they refill each other; two
callees of one caller conflict the same way. Functions in the most conflict
go first, each at the word offset within the next 4 KB that costs least
against the ones already placed, the gap in front filled with the largest
cold functions that fit. Then the executed functions without call edges,
densest first, then the cold ones. Naive hot-first order and a `.text.hot`
linker-script rule both measured slower than the plain link, so neither is
offered.

```make
PGO_LAYOUT = pgo/mygame.layout
# Regenerate after code, feature, SDK or PGO_VARIANT changes (apply refuses a
# stale one), then run pgo-choose with --layout.
pgo-order:
	$(PGO) order --crate game --frontend "$(FRONTEND)" \
		--tape tapes/route-a.pxtape --polls $(TRAIN_POLLS) \
		--pack 'make pack EXE="$$PSOXIDE_PGO_EXE" OUT="$$PSOXIDE_PGO_DISC"' \
		--profile "$(PGO_PROFILE)" --variant "$(PGO_VARIANT)" --out "$(PGO_LAYOUT)" -- $(GAME_CARGO)
```

`choose` with `--layout` then decides per game, on the training polls and
on unseen ones, and `PGO_VARIANT` takes `+order` only where it wins. Give
`compile` `--layout "$(PGO_LAYOUT)"` as well; only an `+order` variant reads
it. What it did in validation, work cycles from
`measure` (frozen frontend dc3e8352, emulator d7686e6), display and VRAM
hashes equal within each game:

| game, gate polls | variant | work cycles, train | second window |
|------------------|---------|-------------------:|--------------:|
| VoXide 72ea127, `lockstep`; train and unseen tapes, polls 252..1200 | `hot=500+profi` | 896,748,060 | 711,274,599 |
| | `hot=500+profi+order` | 888,685,594 (-0.90%) | 703,015,745 (-1.16%) |
| | `off+order` | refused: 0.9% of the profile bound | |
| NitroXide b1bdd75; train tape polls 396..1200, then held-out polls 1200..1600 | `off` | 222,433,080 | 57,822,030 |
| | `hot=500+profi` | 220,804,984 | 57,236,508 |
| | `hot=500+profi+order` | 219,303,086 (-0.68%) | 57,008,541 (-0.40%) |

Each layout profile came from the training window alone and bound 100% of
its instructions; every placed function (102 on VoXide, 62 on NitroXide)
linked where the model put it. The ordered images are the size of their
variant's plain link (491,520 and 550,912 bytes) and scan clean. The
study's prototype measured -0.92%/-1.20% and -0.66%/-0.40% with the older
frontend. I-cache refill stalls were 4.8% to 11% of the study's window
cycles, so the ceiling is low.

A layout profile does not carry across builds whose code differs. In the
study, Quake's order trained on E1M1 alone measured +0.2% and +0.8% work on
E1M1 and E1M2, and its chain-route order reused on the monster-route feature
build +1.2%. The chain-route profile binds 94.4% of its instructions onto
that build (13 functions changed), so `apply` refuses it; the E1M1 one binds
fully, and only the gate can reject it (`choose` ranks it below its variant).

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

`+order` joins any variant, `off` included (`hot=500+profi+order`,
`off+order`), and needs `--layout`. It links the variant as above, then:

1. Binds the layout profile (see "order") onto that link by portable name
   and code hash, and prints how much bound. A function whose code differs
   gets no counts: they belong to other instructions. Below 98% of the
   profile's instructions the build stops, because a changed function is
   placed as cold, anywhere, a gap in the hot code included. Regenerate the
   profile with `order` after code, feature or variant changes.
2. Places the functions (see "order"), writes the ordering file to the work
   directory and relinks with it (`PSOXIDE_LINK_ORDER`, above).
3. Reads the new map and stops unless every listed function is linked in the
   listed order. ld.lld ignores a name it cannot find, silently under
   `--no-warn-symbol-ordering`, and a guest without the hook links as before,
   so the map is the only proof. It also reports how many placed functions
   start where the model put them.
4. Runs the hazard patcher, scanner and stack guard on the relinked image
   with its own map.

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
`-pgso=false` and `hot=1500` on Cortex (see "Troubleshooting"). With
`--layout` every variant can take `+order`; with no `--variant` the
candidates add the variant the layout was collected on and that variant
with `+order`.

When the gate prints what `measure` does, `choose` ranks the passing rows
by the frames that miss their deadline. A game locked to the display shows
every frame for a whole number of vblanks, and a frame whose work overruns
the budget stays up a vblank longer, so the average work per frame is the
wrong objective: a variant can lower it and still slow the heavy frames,
which are the ones that miss. The deadline ranking orders the rows by

1. `missed`: the frames each `NAME.vblanks` column shows for longer than the
   frame budget, as a share of the frames presented;
2. `p95`, then `p99`: the gate's `frame_work_p95` and `frame_work_p99`, the
   work of the heavy frames;
3. `work`: the gate's `work_cycles`, the average.

The relative columns are against the `off` row (or the first passing row
with every ranked column), and each is averaged over the gate's replays so
every tape counts the same. The frame budget is the vblanks a frame may
take: `--frame-budget 1` for 60 fps, 2 for 30. Without it each replay's
budget is the `off` row's most common pacing, which is the rate the game is
built for, and `choose` says which it used. A game that never misses (every
row's `missed` equal) is ranked by its heavy frames, and only rows that tie
on those fall to the average.

`--rank work` orders the table by `work` alone, the ranking `choose` used
before, and `choose` prints both rankings under the table either way.
Failed rows and rows missing a ranked column go last, unranked. A gate
without `vblanks` is ranked by `work`, and one with neither stays in build
order.

NitroXide 13f7c0e shows why (its `pgo-choose`: the six variants, the train
tape over polls 396..1200, discs with the four CD-DA songs; the frame budget
came out as 1 vblank from `off`'s pacing):

| variant                    | frames at 60 | `missed` | frame work p95 | p99     | `work`  |
|----------------------------|-------------:|---------:|---------------:|--------:|--------:|
| `off`                      | 748 of 775   | 3.48%    | 551,458        | 626,608 | 323,407,463 cycles |
| `hot=500+profi`            | 676 of 739   | 8.53%    | 561,066        | 617,028 | -4.74%  |
| `accurate+nopgso+hot=1000` | 670 of 736   | 8.97%    | 561,789        | 613,121 | -5.90%  |
| `accurate+nopgso+hot=1500` | 650 of 726   | 10.47%   | 562,868        | 617,027 | -6.75%  |
| `hot=500`                  | 646 of 724   | 10.77%   | 565,134        | 619,411 | -6.55%  |
| `default`                  | 606 of 704   | 13.92%   | 569,268        | 632,913 | -8.22%  |

Ranked by average work, `default` won by 8.22%, and it shows the fewest
frames at 60. Every profiled variant does less work on average and more on
the frames at p95, and those decide which frames make the vblank (at p99,
past the frames that miss, most do a little less).

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
| `vblanks` | how many route ticks each frame stayed on screen, as `vblanks:frames` pairs (`2:937,3:6` is 937 frames at 30 fps and 6 at 20): the deadline `choose` ranks by |
| `vram`, `display` | the frontend's `--dump-hash` at the stop: equal across builds only when the guest's simulation does not depend on its own speed (VoXide's `lockstep` feature, for example) |

A game locked to the display (every frame two vblanks, like VoXide or
NitroXide) spends its slack spinning in `wait_vblank`, `draw_sync` or a DMA
poll, so `ticks` and `cycles` come out the same for a faster and a slower
build. The second group subtracts the waiting. It covers everything from the
start of the window's first tick to the stop (the first flip after poll `TO`):

| key      | meaning |
|----------|---------|
| `work_cycles` | bus cycles spent outside wait loops: the average `choose` breaks its last ties with |
| `work_instr` | instructions retired outside wait loops |
| `wait_cycles` | cycles inside wait loops: their instructions plus every I-cache, RAM-load and MMIO stall charged to them |
| `wait_share` | `wait_cycles` as a share of all cycles in that span |
| `work_per_frame` | `work_cycles` over the frames presented in that span |
| `frame_work_p50`, `frame_work_p95`, `frame_work_p99` | work cycles of the frames the window presented (the median, 95th and 99th percentile): what a frame costs before its wait, the heavy ones deciding which frames miss |

A frame's work is the work of the route ticks after one flip up to and
including the tick of the next. A flip lands late in its route tick, so the
little that follows it there is the start of the next frame: in NitroXide's
30 fps stretches a flip tick is about 15% work and the tick after it about
94%. Waiting is split between ticks by PC samples, one every
61 instructions, each standing for 61 instructions at its address and
waiting what the exact counts say an instruction there waits on average; a
spell of waiting is one run, so a tick is off by at most 61 instructions a
spell. stderr compares the samples' total with the exact `wait_cycles`. On
NitroXide 13f7c0e every frame that stayed up two vblanks had more than one
vblank of work, and every other frame less.

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

The rule reads a site the hazard patcher rerouted as the instruction it
replaced: `j TRAMP` into `bXX +3 ; nop ; j NEXT ; nop ; j T ; nop` is the
branch `bXX T`, and `j`/`jal` into `nop ; j T ; nop` is `j`/`jal T`. The
trampoline's words run once per iteration and count as wait with the loop.
Read as written, a loop whose exit branch had its slot load rerouted leaves
its span and never comes back: NitroXide's flip wait in its PGO build at
SDK 8adf4b14f went unseen, and `measure` put `hot=500+profi` at +36.6% work
against `off` on the training polls. Seen through, the same builds measure
-4.9% (315,733,754 against 332,039,002 work cycles, display hashes equal).

Some waits are beyond the rule. HK's present loop (the `loop` after
`presentation::begin` in its `main.rs`) spins until the next vblank calling
`input::checkpoint` twice and `presentation::service`, keeps its clock in a
stack slot and branches on a flag it clears, so it counts as work. Name such
a loop with `--wait-range START..END` (hex, end exclusive, repeatable: a PGO
layout can move a loop's pieces apart, so pass each). The addresses belong to
one build, so read them from that build's disassembly; a range that ran
nothing is refused. A range counts what it ran itself as waiting, except a
word (or line) holding a store outside the stack, a coprocessor write or a
GTE command, and stderr lists each range's share.

A range's calls count only with per-word counts, which `measure` asks for
(`--pc-log-words`) whenever `frontend launch --help` lists it. A callee is
usually shared (`checkpoint` also runs from inside rendering), and the counts
cannot say whose call ran an instruction, so `measure` counts a lower bound.
If a callee was entered `N` times, `n` of them from the range, an instruction
on a quiet path that ran `c` times ran at least `c - (N - n)` times for the
range, because one call runs it at most once. A quiet path goes from the
entry to `jr ra` with no store outside the stack, no coprocessor or GTE work,
no loop, and only calls with quiet paths of their own, which get the same
bound in turn; code that any other executed branch or call also jumps into
is left out. Whatever only some calls do stays work: on HK that is the pad
poll `checkpoint` makes once a vblank, a DMA kick, an audio refill. Stalls
on a partly waiting word are split in proportion to its count. Without
per-word counts the calls stay work and stderr says so. The rule and its
tests are in `src/work.rs` (`split`, `attribute`).

On HK's kings climb (polls 100..4790, a frontend with `--pc-log-words`),
naming the present loop's pieces moved it out of work:

| build                | spins a frame | wait share, rule only | named  | work per frame, rule only | named     |
|----------------------|--------------:|----------------------:|-------:|--------------------------:|----------:|
| plain                | 403           | 1.37%                 | 10.80% | 1,584,777                 | 1,433,206 |
| PGO (a later commit) | 503           | 1.37%                 | 19.08% | 1,359,079                 | 1,114,999 |

The loop itself was 22,344 and 34,903 cycles a frame; its calls added
129,318 and 209,303, against at most 133,909 and 214,150 had every
instruction those calls could have run in `checkpoint` and `service` been
waiting. The pad poll (22,480 and 19,303 cycles a frame) stays work. Without
per-word counts only the loop's own instructions move (wait share 2.77%
and 3.90%).

This costs a second replay: the per-line logs can only start at a route tick,
so a short first replay (to 30 polls past `FROM`) finds the tick in which
poll `FROM` lands, and the full one logs every retired instruction per
16-byte I-cache line (`--pc-line-log`), or per word, with the I-cache,
RAM-load and MMIO stalls on the same key and a PC sample every 61
instructions per route tick, then dumps RAM for the code. A frontend without
`--icache-stall-line-log` leaves the wait loops' I-cache stalls in work and
says so. `measure`
stops if the two replays disagree at that tick. The frontend's own output
goes to stderr so it cannot land in the table.

One approximation, small next to the differences a variant makes: per line,
a line the loop touches counts as wait in full (the instructions sharing it
run once per call, not once per iteration; per word it is gone). The GTE,
multiply and DMA-contention stalls have no per-line log, so they stay work
wherever they fall; a spin loop does no GTE or multiply work.

A spin loop's I-cache refills are waiting, too. A layout can put a wait loop
in the same I-cache set as the hazard trampoline it jumps through, and then
every spin refills both lines, as the flip wait at 0x80017d18 and its
trampoline at 0x8008fd14 did in NitroXide builds made while tuning its
60 fps path. Those refills end when the vblank comes, like the spin, so they
are waiting. Two of those builds (36e9fa5 with two different `draw.rs`
experiments, train polls 396..1200) kept the same pacing, 574 frames at 60
and 114 at 30:

| build                      | I-cache stalls | work per frame, refills as work | refills as waiting |
|----------------------------|---------------:|--------------------------------:|-------------------:|
| wait loop and trampoline apart | 13,232,661 | 420,929 | 420,881 |
| in one I-cache set         | 50,701,974     | 535,552                         | 437,503            |

A guest that
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
  to the display; `vblanks` says how many frames missed, and `choose` ranks
  by those, then by the heavy frames' work, then by `work_cycles`.
- **`choose` picks a variant with more work on average.** It lost fewer
  frames: the variant with the lower average made the heavy frames slower
  (see "choose"). `--rank work` gives the old order.
- **`measure` reports a wait loop that is not one, or misses one.** Its
  addresses are on stderr; look them up in the link map. The rule is in
  `src/work.rs` with a test per pattern it accepts or rejects; add the new
  shape there. A loop that calls out, stores or carries state (HK's present
  loop) can be named with `--wait-range` instead.

## Lower-level commands

```text
psoxide-pgo <elf-with-dwarf> <pc.csv>... <out.prof>        convert (sums several logs)
psoxide-pgo portable <in.prof> <out.prof>                  strip disambiguators
psoxide-pgo rebind <in.prof> <target-elf-with-dwarf> <out.prof>
psoxide-pgo layout <link.map> <image.exe> <words.csv>... <out.layout>   layout profile from per-word logs
psoxide-pgo place <in.layout> <link.map> <image.exe> <linker-script> <out.order>
```

`layout` reads `--pc-log-words --pc-line-log` logs of replays of that
image; `place` binds, places and writes the ordering file without linking.

The same hl-psx commit built from two checkout paths differed by about 0.4%
in FPS, because the path changes the code layout. Build every candidate from
one directory, and treat smaller differences as unproven without a cycle
breakdown.
