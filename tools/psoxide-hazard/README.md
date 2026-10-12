# Checked cold-image packing

`overlay-package` extracts one executable prefix and its declared cold data from
the same final MIPS ELF used for the resident executable. Keep `--emit-relocs`,
the matching map, compiler, features and Rust definitions. This is a private Rust
ABI boundary, not a stable ABI between independently compiled programs.

Example for a two-entry title module:

```sh
overlay-package game.elf raw.pak --id 3006 \
  --code .overlay.cs_title.text --data .overlay.cs_title.rodata \
  --resident .text --resident .data --resident .bss \
  --call cs_title_call_intro:cs_title_intro \
  --call cs_title_call_run:cs_title_run \
  --header .ps_exe_head --descriptor CS_TITLE_DESCRIPTOR \
  --sites CS_TITLE_CALLS --trap cs_title_unloaded
```

List every allocated resident section, every typed incoming call pair, and all
owned cold tables. Mutable gameplay state stays resident. The descriptor and
call array must be initialized writable resident symbols using `PackedDescriptor`
and `PackedCall`; their fixed capacities are checked before publication. Each
declared pair must have at least one call and all distinct sites are emitted.
Undeclared incoming references and data-held cold pointers are rejected.

The command produces `raw.pak`, a trapped runtime `raw.pak.exe`, a composite
`raw.pak.guard.exe`, and `raw.pak.guard-args`. Run the existing hazard patcher,
hazard scanner and stack guard on the composite image with its matching map and
all emitted module/code ranges. Preserve the consumer's stack budget and cache
flush exclusion. Then repeat the command with a new output name and
`--final-guard raw.pak.guard.exe`. Repeat the guards on that final composite image.
Ship only the final runtime executable and its final package together: the
descriptor checksum covers the hardened payload. Do not copy patched words or
repair checksums in a game-local extractor. Generated `.rs` is for host inspection,
not a second link that could move the qualified call sites.

The finalizer retains original typed calls in the guard image for the complete
call closure and traps them in the shipped resident image. It accounts for the
136-byte hazard trampoline reservation. Unsupported moved relocation operands
or rerouted import instructions fail closed; they require an explicit supported
binding trace before production use. Bundle cold dependencies in one image:
cross-cold imports are currently unsupported. Fixed resident services and
self J26/HI16/LO16/W32 relocations are supported within the checked placement
window; cross-image PC16 is rejected.

Transport remains consumer-owned. Admit into a disjoint portion of the existing
arena only after transfer completion. Keep generations across unload/re-admit,
pin dependencies through every call and data borrow, and quiesce callbacks,
rendering, dialogue and DMA before reuse. No module-owned reference may escape
its admission. Consumer production linking, RAM/residency, stack and focused
gameplay qualification remain required. The embedded `hello-overlay` example
qualifies the callable ABI and resident-service import, not CD transport or net
resident memory savings.
