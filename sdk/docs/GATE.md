# The architecture gate

A program that breaks the SDK's architecture does not build, and an image that did not pass the
gate does not become a disc. The rules are the three below. What is allowed is
`sdk/profile.toml` (the feature tables, the probe programs, the deprecated symbols, and the
exceptions of the programs that live in this repository). What is still tolerated in any other
program is listed by that program, in a `psx-gate.toml` at the root of its own repository; the
SDK does not name its users.

## The rules

The messages cite these. They are the SDK's standard for a program that links it:

1. **One implementation of each job.** The SDK has one implementation of every job (GPU, pad, CD,
   audio, timing, ...). A program does not carry its own copy of an SDK crate.
2. **No feature selects a different implementation.** A cargo feature of an SDK crate may switch
   on a development tap (`[diag]`, which makes a build a diagnostic one that cannot become a disc)
   or a host-only tool (`[host]`), or a capability every build may have (`[shipping]`). It does not
   choose another implementation of an SDK job.
3. **A program reaches the hardware through the SDK.** It does not write a register address,
   install an exception-vector handler or run COP0 code itself, and it does not call a legacy
   entry point the SDK has replaced.

## What is refused

| Kind | Meaning | Rule |
|---|---|---|
| `feature` | A program's graph enables a feature of an SDK or engine crate that `profile.toml` lists nowhere (or only under `[host]`). | 2 |
| `register-literal` | A hardware-window address literal (`0x1F80`, `0xBF80`, `0x9F80`, `1F80_`, `1F80xxxxh`) in code a program owns (comments excluded). | 3 |
| `vector-write` | `0x8000_0080` / `0xA000_0080` or `declare_stack_safe_handler` in a program's own code. | 3 |
| `cop0-asm` | `mfc0`, `mtc0`, `rfe`, `cfc0`, `ctc0` in a program's own code. | 3 |
| `deprecated-use` | A call of a legacy SDK entry point (`[deprecated]` in `sdk/profile.toml`) in code a program owns. Matched by name (`\bsymbol\b`), so a function of another crate with the same name is a hit too; list it. A symbol written `crate::name` in the table matches only the item of that crate: the path `crate::name`, an import of it (`use crate::{..., name}`) and, in a file that imports it or `crate::*`, a bare call; a function the program defines itself and a method call of that name pass. | 1 |
| `private-copy` | A package named `psx-*`, `psxed*` or `psoxide*` in the program's graph whose manifest is not under the SDK. | 1, 3 |
| `no-receipt` | A disc builder is given an executable that did not come through `psoxide-pgo apply`. | 2, 3 |

A refusal prints what was found, where (`file:line`, or `crate/feature`, or the package), the
rule and what to use instead:

```
psx-gate: acme/rover: game/src/irq.rs:36: (0x8000_0080 as *mut u32).write_volatile(...) (vector-write); sdk/docs/GATE.md rule 3 (a game does not wrap the exception vector); register with psx_rt::interrupts (the dispatcher) instead
psx-gate: psx-gpu: feature `asm-kernels` is not in sdk/profile.toml and the program being built has no exception for it; sdk/docs/GATE.md rule 2 (...). Who enables it: `cargo tree -e features -i psx-gpu`.
```

## Three places, one file

**G1, the build scripts (`crates/psx-gate/src/features.rs`).** Every SDK or engine crate that has a
`[features]` table has a six-line `build.rs` that compiles the shared `profile.rs` and
`features.rs` in with `#[path]` (no build dependency, so no game's `Cargo.lock` changes). On a
MIPS build it refuses any enabled feature that no table of `sdk/profile.toml` lists and that the
program being built holds no exception for. Host builds (tests, tools) are not checked, and
neither is a `cargo clippy` run (the SDK's `make lint` turns every feature on for the guest and
links no program; the build script sees `RUSTC_WORKSPACE_WRAPPER` is clippy-driver). A new
feature added to a crate is refused the first time anything enables it.

A build script sees its own crate's features and not the program, so it learns the program from
the environment:

* `PSX_GATE_FILE`, the path of the program's own `psx-gate.toml`: its `feature` entries count.
  `psoxide-pgo` sets it for every cargo build it runs. A program built by plain cargo sets it in
  its `.cargo/config.toml`:

  ```toml
  [env]
  PSX_GATE_FILE = { value = "psx-gate.toml", relative = true }
  ```

  (`relative` is to the directory that holds `.cargo`.)
* `PSX_GATE_PROGRAM`, the name of a program inside this repository, whose `feature` entries in
  `sdk/profile.toml` count. `psoxide-pgo` sets it for those programs.

A recipe or script of this repository that builds a program with feature exceptions by plain
cargo (not through `psoxide-pgo`) sets `PSX_GATE_PROGRAM=<its name>` on the cargo command; a test
(`crates/psx-gate/tests/profile.rs`) reads the make files, scripts and workflows and fails when
one does not.

With neither set only the tables count: a program that holds a feature exception and is built by
plain cargo without naming its file is refused, loudly, until it does. There is no silent way
through G1 for an unlisted feature. What G1 does not do, so nobody reads it as the whole gate: it
trusts the file it is handed (it does not count, find a stale entry, or tell that the file belongs
to the program being built), it does not look at program sources at all, and it only runs when
cargo builds the SDK crate, so a program that does not link a crate is not asked about it. Those
are G2.

**G2, `psx-gate` (`tools/psoxide-hazard`).** Run by `psoxide-pgo apply`, `collect`, `order`,
`choose` and `ladder` before the first compile (and runnable by hand:
`psx-gate --sdk .psoxide --crate game -- build --release`). It reads `cargo metadata` for the
program (with its own feature flags, for the PS1 target), takes the packages reachable from the
root that are not the SDK's, scans their `lib`, `bin` and `build.rs` sources (not `tests`,
`benches`, `examples`), and checks the resolved features of every SDK package in the graph. It
reads the program's own file from `psx-gate.toml` at the root of the program's repository (or
from `--program-file`). A program outside this repository is named `<repository>/<root package>`
in its receipt (two repositories may call their root package alike); a program inside this
repository is named by its package. Items are paths from the repository root.

**G3, the receipt (`crates/psx-iso/src/receipt.rs`, `crates/psx-gate/src/press.rs`).** `apply`
writes a receipt into the PSX-EXE header (offset 0x400, in the reserved region the BIOS ignores):
SHA-256 of the executable with the receipt zeroed, the program name, and shipping or diag. `mkisopsx`
refuses an executable whose receipt does not match (patched or relinked after the gate), a
`diag` receipt unless `--diag` is given (a diag image has a `[diag]` feature of an SDK crate on:
`host-tty`, `emulator-telemetry`, ...), and an executable with no receipt unless a `no-receipt`
entry lists it: in `sdk/profile.toml` for a program of this repository, or in the program's own
`psx-gate.toml`, which the recipe that calls `mkisopsx` passes with `--gate-file`. The SDK's own
disc recipes pass `--diag`; a program's lineup recipe does not.

Identifying a program that has no receipt: the executable carries no name, so `mkisopsx` uses the
executable's file stem. A determined bypass is to rename the file to a listed stem, or to call
`psx_iso::receipt::write` oneself; the receipt stops a program built the wrong way by accident
and any change made after the gate ran, not sabotage by someone who has the SDK. Disc builders
that call `psx_iso` directly are not covered.

The receipt is written in one place, `psx_gate::press::stamp`. `psoxide-pgo apply` calls it at the end
of its build; `tools/build_guest_staged.sh` (the editor's playtest program, which does not go
through `apply`) runs `psx-gate` before it compiles anything and `psx-gate --receipt EXE` after
the hazard patch, which runs the same check and stamps the same receipt (diag when a `[diag]`
feature is on). A program built by plain cargo through some other script has no receipt until it
does the same.

What a caller of `psx_iso` must do to be covered: read the executable, call
`psx_gate::press::check(&exe, label, allow_diag, &sdk_profile, own.as_ref())` before it hands the
bytes to `IsoBuilder`, and stop on `Err` (print the message, exit non-zero). `label` is the
program's name (the executable's file stem is what `mkisopsx` uses), `sdk_profile` is
`Profile::read("sdk/profile.toml")` of the SDK it links, `own` is `Profile::parse_program_file`
of the program's `psx-gate.toml` (or `None`). Never swallow the error, and never write the
executable into an image through another route.

## The ratchet

A program's exceptions list what exists today, by item. In `psx-gate.toml` at the root of the
program's repository (the entries of a program inside this repository are the `[[exception]]`
tables at the end of `sdk/profile.toml`, with the same fields):

```
[[exception]]
program = "rover"              # the root package; may be left out when the repository has one program
kind = "register-literal"
item = "src/telemetry.rs"
count = 3                      # hit lines; the line kinds only
remove = "W4"
```

* A violation without an entry is refused.
* An entry whose program no longer does it fails ("delete the entry").
* A count that is now lower fails ("lower the count to N"); one that is higher fails and the
  message prints every hit line of the file with the listed count.
* `remove` is a short tag for what removes it: a workstream (`W3`), `unscheduled` or `decision`.
  What the tags mean is the program's business, not the SDK's.
* `no-receipt` (see G3) lives in the same file: `kind = "no-receipt"`, `item = "-"`, `program` the
  executable's file stem.

Nothing is added to a list without review. `psx-gate --propose` prints the entries that would
list today's violations; use it to see what a change adds, not to silence the gate. Each program
takes its file when it repins to an SDK with the gate.

## Probe programs

`[probe]` in `sdk/profile.toml` names programs of this repository that measure the hardware: they
must not depend on the accessors they measure, so they read registers and COP0 by raw address on
purpose. A probe program is exempt from the `register-literal`, `vector-write` and `cop0-asm`
rules by rule, not by counted entries (a new hardware test is not a ratchet edit). It is still
checked for features, private copies and deprecated calls. The class is closed to programs of
other repositories, and a probe that keeps a line-kind entry fails ("delete the entry").

## Deprecated symbols

`[deprecated]` in `sdk/profile.toml` lists the legacy entry points a program may still call. A
program lists its remaining calls per file with a count (`deprecated-use`); a new call is refused
and the count only goes down. The match is by name.

## Tests

`cargo test -p psx-gate -p psoxide-hazard -p psx-iso`: each check has a failing and a passing
fixture (`tools/psoxide-hazard/src/gate.rs`, `crates/psx-gate`, `crates/psx-iso/src/receipt.rs`),
and `crates/psx-gate/tests/profile.rs` fails when a crate with features has no gate script or the
profile lists a feature that does not exist.
