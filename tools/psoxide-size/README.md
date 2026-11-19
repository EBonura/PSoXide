# psoxide-size

Where the bytes of a PS1 guest image go, and the panic policy every guest build shares.

```
psoxide-size audit MAP [--exe EXE] [--top N] [--json] [--list CLASS] [--budget CLASS=BYTES]...
psoxide-size diff BEFORE.map AFTER.map
psoxide-size panic-flags [--toml]
psoxide-size faults MAP RAM.bin
psoxide-size ram MAP [--items] [--floor BYTES|FILE]
psoxide-size block EXE MAGIC:VERSION:LEN:NAME...
```

`audit` reads the lld link map a guest build writes (and the PS-X EXE when given, for strings,
`core::panic::Location` records and duplicated constants) and prints bytes per class and section kind:

| class | what it is |
| --- | --- |
| `fmt`, `panic`, `str_char` | `core::fmt`, `core::panicking`, `unwrap_failed`, bounds-check panics, `from_utf8` |
| `panic_location`, `file_name` | 16-byte `Location` records and the `src/foo.rs` strings they point at (needs `--exe`) |
| `string`, `anon_const` | other string literals; promoted constants, jump tables, vtables |
| `debug_impl` | `Debug`/`Display` impls the game or SDK derived |
| `alloc_grow` | `Vec`/`RawVec` growth paths |
| `sdk`, `game`, `builtins` | `psx_*` crates, the game's own code and statics, `compiler_builtins` |

It also lists the largest input sections and statics, bytes per crate, alignment padding between input
sections, generic functions that exist in 2 or more instantiations, duplicated read-only constants and
all-zero `.data`. `heap_free` is `STACK_INIT - STACK_RESERVE - __heap_start`, the figure builders report.
`--budget panic_machinery=2048` (or any class name) exits 1 when the class is over, for a CI size line.
The classifier works on a map alone; `--exe` only sharpens strings, locations and duplicates.

## Panic policy

A shipping guest has nowhere to print a panic message, so it is built with `-Cpanic=immediate-abort`:
every panic, bounds check and overflow check is a bare `break` at the faulting instruction, and
`core::fmt`, the panic locations and their file names stay out of the image (12.8 KB on Quake).
psx-rt's exception handler (`install_vblank_counter`) records Cause, EPC and BadVAddr in
`__psx_rt_fault_*`, so a fault is still located: `faults` reads them from a `--dump-ram` image, exits 1
when `__psx_rt_fault_count` is not zero and names the function around EPC. A gate that runs a shipping
image should call it. `PSOXIDE_PANIC_MESSAGES=1` restores the formatted panic. `panic-flags` prints the
rustflags for the chosen policy (`--toml` for a cargo `--config` array body).

`tools/sdk-examples.mk` builds every example this way, and `tools/abort_gate.sh FRONTEND` proves the
path: `hello-fault` panics on purpose and must be recorded as a break in `main`, `hello-present` must run
clean and be smaller than its message build.

## Free RAM and the budget gate

`ram` is the number a game's RAM budget gate reads and the table that says where the rest went.
The first line is `heap region N B`: `STACK_INIT - STACK_RESERVE - __heap_start`, with the
`STACK_RESERVE` the map itself names, so a game that links a smaller reserve is measured against
its own (`audit --json` carries the same `heap_free` and `stack_reserve`). Below it: stack reserve,
the image split into `.text`, `.data` (with the read-only data) and `.bss`, and per output section
the bytes by crate, by module (the first two path segments of the symbol; the link is fat-LTO, so
the symbol path, not the object file, says where code came from) and the 30 largest symbols.
`.overlay.*` sections are listed apart, since they are stored on the disc and do not occupy the
image. `--items` prints every placed input section as `kind size crate name`, largest first, for
diffing two links. `--floor` takes a byte count or a file whose first number on its first
non-comment line is the floor, prints the margin and exits 1 when the link is under it.

`psoxide-size ram` is the measure of free RAM, and the only one: the floor a game commits is
compared with this number, exactly at the floor fits and one byte under does not
(`psoxide_size::ram::fits`). The figure stops at `STACK_INIT - STACK_RESERVE`, the lowest address
the stack may reach and the one the link asserts against. A RAM report a game wrote itself has
over-reported by 256 B, because the linker script's `RAM` region ends 256 B above that address, so
a build it called fine could be under the floor. A game's budget gate and its link hook for
`psoxide-pgo ladder` must call `psoxide-size ram MAP --floor BYTES` (or `psoxide-pgo ram MAP
--floor BYTES`, which runs the same function and prints `free N B`), or link against the library
function `psoxide_size::ram::free_ram`, and print nothing computed by itself. `psoxide-pgo
ladder` measures the map a `--link` hook leaves with that function and ignores a different `free
N` the hook printed (it says so).

`block` finds the one statically initialised state block a gate reads from emulator RAM: four
magic bytes, a version word, the block's own length and zero padding. Each `MAGIC:VERSION:LEN:NAME`
prints `0xADDRESS NAME`; it exits 1 when a block is missing, ambiguous or the file is not a PS-X EXE
with its whole declared payload.
