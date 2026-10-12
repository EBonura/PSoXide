# Borrowed arena callable probe

This example compiles its private Rust state, typed resident caller, cold entry
and resident floating-point service in one compiler invocation. The SDK borrows
the caller's existing workspace. It does not allocate another arena. Admission
exclusively borrows the registry and storage through the call; module data views
cannot outlive that borrow. Static entry rebinding is unsafe: the application
must preserve the exact same-compilation ABI and finish asynchronous DMA,
rendering and callbacks before unloading or overwriting the arena.

The probe checks a first call (count 7, value 3.25), trapping the entry on return,
unload, gameplay overwrite, same-slot re-admission, rejection of the old handle,
and a second call (count 12, value 5.125). Its cold entry calls a fixed resident
Rust service, including floating-point helpers. Those resident targets remain
unchanged; self references receive checked relocation.

The embedded package is a completed-transport fixture. It proves neither CD
reads nor net RAM recovery. A game supplies these same checked words after its
own CD transport completes, and borrows a disjoint existing scene/title tail.
Do not overwrite title code until all admitted calls and external users finish.
Keep the registry across gameplay and title restoration so generations do not
reset. Original module-to-module cold dependencies and their import binding are
not supported by this example; resident service imports are qualified.

`overlay-example-pack` is a sample-specific recipe (fixed symbols, one cold
export and link base), not a generic game packer. The reusable checked ELF reader
and arena/relocation API live in the SDK tools and `psx-residency`. Future game
packaging should reuse those shared checks rather than copy this recipe.

From this directory, with the repository's pinned toolchain:

```sh
make OUT=/absolute/private/output
```

The recipe excludes `.overlay` from the resident executable and traps its typed
callsite until admission. The `.guard` file contains identical cold image bytes
after the resident payload, with the original typed call restored for static
dependency analysis. This validates caller, module and resident service together;
the guard file is never booted. The 136-byte module trampoline reservation is
retained even when the scanner reports zero hazards. The runtime package and
guard copies must remain identical; a future hazard patch must be repackaged and
checksummed before use, rather than patching only one copy.

Run the produced CUE in a retained headless emulator and require
`CALLER ARENA OVERLAY PASS`, both count/float values, zero MIPS hazards and the
typed call closure's stack proof. These results are emulator qualification,
not original hardware qualification.
