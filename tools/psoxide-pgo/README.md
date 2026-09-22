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
(build flags, the ELF twin, replays, conversion, renaming, the hazard patcher
and scanner) lives here, so a fix reaches every game with its next SDK pin.

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
plus the hazard patcher and scanner, so a game can route its build through
`apply` before it has a profile.

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
2. Hazard-patches and scans the image, and runs `--pack` if given, with
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
variant's flags, and runs the hazard patcher and scanner. Their output is
never piped, and a non-zero exit stops the build: a swallowed failure once
shipped an unpatched hl-psx exe.

Variants, joined with `+` to combine (`accurate+hot=1000`):

| variant    | extra flags                                   |
|------------|-----------------------------------------------|
| `off`      | none, and no profile: the plain build         |
| `default`  | the profile alone                             |
| `accurate` | `-Cllvm-args=-profile-sample-accurate`: code the profile never saw is treated as cold |
| `hot=N`    | `-Cllvm-args=-hot-callsite-threshold=N`: the inline budget of a call the profile calls hot (LLVM's default is 3000) |
| `noreplay` | `-Cllvm-args=-disable-sample-loader-inlining`: do not replay the profiled build's inlining; inlinee samples merge into their own functions |
| `nopgso`   | `-Cllvm-args=-pgso=false`: do not optimise profile-cold code for size |
| `llvm=-F`  | `-Cllvm-args=-F`, any other LLVM option (`llvm=-sample-profile-inline-size`) |

A variant that fails to build shows as a failed row in `choose` instead of
stopping it.

### choose

Builds each `--variant` in turn (default: `off`, `default`, `accurate`), packs
it if `--pack` is given, and runs `--gate` with `PSOXIDE_PGO_VARIANT`,
`PSOXIDE_PGO_EXE`, `PSOXIDE_PGO_IMAGE` (the disc when packed, else the exe) and
`PSOXIDE_PGO` (this tool, for `measure`). The gate's exit status is pass or
fail; every `key=value` line it prints (no spaces in the value) becomes a
column. On a CPU-bound SDK guest (bus cycles to reach poll 1,400 of each tape):

```text
variant   gate  train.cycles  unseen.cycles
off       pass  514724511     533018552
default   pass  509023853     527305124
```

The gate is the game's own judgement, so it should replay a training tape
*and* one the profile never saw, and check correctness (hashes, poll-bound
state) as well as speed. PSoXide-editor's
`docs/measuring-guest-performance-2026-09-17.md` explains why final-frame
hashes alone mislead. Commit the
winner as the game's `PGO_VARIANT`, `off` included.

### measure

One replay of `--image`, printing totals over the ticks that ran wholly
inside the `--polls` window, as `NAME.key=value` lines for a gate:

| key      | meaning |
|----------|---------|
| `ticks`  | route ticks (vblanks) the window took: lower is faster for a guest that never waits on vblank |
| `flips`  | ticks in which the display start changed: rendered frames, for a guest that renders at most once per tick |
| `cycles` | bus cycles in those ticks |
| `icache` | I-cache refill stall cycles in those ticks |
| `vram`, `display` | the frontend's `--dump-hash` at the stop: equal across builds only when the guest's simulation does not depend on its own speed (VoXide's `lockstep` feature, for example) |

The resolution is one route tick at each end of the window. The frontend's
own output goes to stderr so it cannot land in the table.

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
