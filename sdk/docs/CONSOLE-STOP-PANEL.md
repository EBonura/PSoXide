# Console stop-panel: seeing where a program stopped

A freeze on a console gives you a still picture and nothing else. The stop-panel
turns it into one photo: when a program faults, panics or stops making progress,
`psx-rt` takes over the display and prints where the program was and what the
hardware was doing. It exists for diagnostic discs and is **off in every normal
build**: with the feature off the code is not linked, and every hook in
`psx-io` compiles to nothing, so a shipping image is the same bytes with or
without this crate version.

## Turning it on

The feature is `console-diag` on `psx-rt`. It switches on the matching feature
in `psx-io` (the counters the panel prints), so the game needs only the one line:

```toml
[features]
console-diag = ["psx-rt/console-diag"]   # a game's own, off by default
```

Build the disc with `--features console-diag` and give it a name that cannot be
mistaken for the release (a DIAG disc). Never enable it unconditionally in the
dependency line: the release build must stay byte-identical to a build that
does not know about the feature. `sdk/examples/hello-diag` is the reference
user (it turns the feature on in its manifest because it only exists to show
the panel).

Optional: set `PSX_DIAG_TAG` at compile time (any short string) to put a build
name in the panel header.

## What brings the panel up

| Trigger | When |
| --- | --- |
| Fault | The exception handler reached its halt: a `BREAK` (what `panic=immediate-abort` leaves behind), an instruction-side fault, a fault in a delay slot |
| Panic | A panic with a message (builds without `panic=immediate-abort`) |
| Stall | The VBlank handler saw no sign of life for `psx_rt::diag::STALL_VBLANKS` frames (900, 15 s at 60 Hz): no GP1 write, no GPU DMA kick, no CD sector or command |

A program with steps that are legitimately silent for longer than that (a long
measurement, a silent test) calls `psx_rt::diag::set_stall_limit(vblanks)` once.
Faults and panics are not affected by it.

The panel resets the GPU, shows its own 320x240 screen and keeps refreshing its
frame counters, so a photo shows the machine is alive and the program is not. It
prints the interrupted program counter, return address and stack, a guess at the
call chain, the GPU, DMA and interrupt registers, the SDK's wait and CD
counters, the last GPU list that held a node over 16 words (the silicon-only
freeze class), and any panic message. Resolve the addresses against the link
map of the same build.

## Naming the step

The program can say where it is, so the photo names the step as well as the
address:

- `psx_io::diag::step(id, "TEXT")`: an id and up to 24 ASCII bytes.
- `psx_io::diag::step_hex(id, "tag", a, b)`: a tag and two numbers in hex, for a
  failure site that has to say which file or chunk and which result.
- `psx_io::diag::stop()`: halt into the panel on purpose, with the last step.

All three are plain calls that do nothing when the feature is off. A call is
still a call, though (`step` and `stop` are not inlined), so keep them behind
the game's own `console-diag` feature (`#[cfg(feature = "console-diag")]`) and
the shipping image does not carry them.

## Cache flushes

The panel also shows what the last instruction-cache flush found (DMA busy mask, cache control
register, status register), a `WILD PC` row when the program counter is outside any code, and
whether a fault came with the cache isolated; see [CACHE-CONTROL](CACHE-CONTROL.md).

## Seeing it

`sdk/examples/hello-diag` draws a triangle, then stops in the way the
`HELLO_DIAG_MODE` build variable names: `stall`, `fault`, `panic`, `cd` (a
sector that is not on the disc) or `ll` (a 41-word GPU list node). Build the
example the way `hello-tri` is built (see the root README) with the variable set.
