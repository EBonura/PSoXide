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
and arena/relocation API live in the SDK tools and `psx-residency`. Production
games use the generic `overlay-compose` recipe below rather than copying the
sample-specific extractor.

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

## Generic same-compilation load and call

Compile resident typed wrappers and their cold Rust functions together, using
identical private state/service types and compiler settings. Emit code first in
`.overlay.actor.text`, and all owned tables/jump tables in
`.overlay.actor.rodata`. Initialize `ACTOR_DESCRIPTOR: PackedDescriptor` and
`ACTOR_CALLS: [PackedCall; N]` in resident `.data`; use volatile reads after the
final-link patch. Keep a resident `actor_unloaded` trap. The packer rejects an
unlisted resident pointer into cold bytes, cross-cold references, and table
capacity overflow. `--call` order supplies selectors; every actual callsite is
emitted, including multiple sites for the same pair.

```sh
overlay-compose game.elf raw --header .ps_exe_head \
  --resident .text --resident .data --resident .bss \
  --image actor --id 85 --code .overlay.actor.text \
  --data .overlay.actor.rodata --descriptor ACTOR_DESCRIPTOR \
  --sites ACTOR_CALLS --trap actor_unloaded \
  --call actor_call_update:actor_update
```

Add further images to the same command; never merge game-local EXE patches.
Run hazard-patch and hazard-scan against `raw/guard.exe`, passing **all**
generated `--module` and `--code` ranges from `raw/guard-args`. Run stack-guard
with every generated `--module` range and the exact map; it does not accept
`--code`. Retain `--forbid __psx_rt_flush_i_cache` for stack-guard. Then repeat the identical
composition command with a new output directory and
`--final-guard hardened-guard.exe`. Repeat check/scan/stack on its final guard.
Ship only that generation's `runtime.exe` and packages: their metadata and
checksums describe the final hardened bytes. The raw ELF stays immutable.

After the game's CD transport finishes, pass the unpadded package words and its
trusted descriptor to the existing-arena API. This minimal one-site example
uses the same typed wrapper that was compiled with its cold target:

```rust
use psx_residency::arena::{CallerArena, Descriptor, Registry, StaticImport};

// Unsafe application boundary: `site` is the final patched resident instruction,
// `offset` its checked cold export, and `typed_call` calls that exact Rust entry.
let mut arena = CallerArena::new(existing_scene_tail, actual_tail_address)?;
let handle = registry.install(&mut arena, completed_cd_words, Descriptor {
    id: packed.id, linked: packed.linked, checksum: packed.checksum,
})?;
let dependencies = [handle];
{
    let admission = registry.admit(&mut arena, &dependencies)?;
    let mut imports = [unsafe {
        StaticImport::new(&mut *(site as *mut u32), site, handle, offset)
    }];
    let result = unsafe {
        admission.invoke(&mut imports, resident_trap_address, || {
            typed_call(&mut resident_actor_state, &mut resident_services, frames)
        })?
    };
    // Consume result here. Do not retain cold references or callbacks.
}
// Imports are trapped and the caller cache is flushed on invoke scope exit.
// Complete all external users before unload or gameplay overwrite.
registry.unload(handle);
```

For multiple entries/sites construct the complete fixed-capacity import list
from `packed.calls` and each `PackedCall`, with unique instruction words; do not
bind only the selected wrapper's first site. Keep `Registry` resident across
unload/re-admit so an old handle never becomes valid again. Borrow module-owned
data only through `admission.words(handle)` and copy anything needed afterward.

For OoT's requested boundary: all 96 actor slots and save state remain resident.
Profile callback pointers must be resident veneers whose entire call, including
`ActorSystem::Drop` destruction, runs under admission, or be copied into resident
records without cold pointers. Run destroy/dialogue/CPU render completion before
unload; finish DMA and other asynchronous users explicitly. Player, collision,
render, math, save and message services remain fixed resident imports. Module
views stay borrowed or are copied. Preserve nearest-hit selection and pending AC
state as resident slot IDs, not cold references. The SDK borrows a disjoint tail
of the existing 1,105,920-byte arena and does not alter the 32,768-byte stack.
These are application integration obligations, not automatic game parity claims.
