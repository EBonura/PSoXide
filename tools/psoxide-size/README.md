# psoxide-size

Where the bytes of a PS1 guest image go, and the panic policy every guest build shares.

```
psoxide-size audit MAP [--exe EXE] [--top N] [--json] [--list CLASS] [--budget CLASS=BYTES]...
psoxide-size diff BEFORE.map AFTER.map
psoxide-size panic-flags [--toml]
psoxide-size faults MAP RAM.bin
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
