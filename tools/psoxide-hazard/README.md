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

## Independent images from one final link

`overlay-compose` owns the complete publication when two or more independently
admitted images share one resident executable. Do not merge outputs from separate
`overlay-package` invocations. All images still use one source-matched ELF and
map, and each image owns its code, tables, descriptor and call array.

```sh
overlay-compose game.elf raw-generation \
  --header .ps_exe_head --resident .text --resident .data --resident .bss \
  --image forest --id 1 --code .overlay.forest.text \
  --data .overlay.forest.rodata --descriptor FOREST_DESCRIPTOR \
  --sites FOREST_CALLS --trap overlay_unloaded \
  --call forest_wrapper:forest_entry \
  --image shop --id 2 --code .overlay.shop.text \
  --data .overlay.shop.rodata --descriptor SHOP_DESCRIPTOR \
  --sites SHOP_CALLS --trap overlay_unloaded \
  --call shop_wrapper:shop_entry
```

The new generation directory contains `runtime.exe`, one `NAME.pak` per image,
`guard.exe`, `guard-args` with every module/code range, and `manifest.txt` with
all call sites and final checksums. Its union of cold sections is excluded from
resident bytes. Identities, section ownership, metadata ranges and call sites
must be distinct across images. Independently linked addresses are supported;
references between cold images remain rejected.

Harden and scan the single composite `raw-generation/guard.exe` with all emitted
`--module` and `--code` ranges. For stack-guard, pass every `--module` range and
the exact map, without `--code`; retain `--forbid __psx_rt_flush_i_cache`. Repeat the complete batch command using a new destination directory and
`--final-guard raw-generation/guard.exe`. Check the resulting final composite
again, then ship its runtime executable and all matching packages as one bound
generation. The finalizer refreshes every payload, relocation list, checksum and
metadata table before publishing any output. It preserves typed guard calls and
traps every shipped resident call site. An existing destination is rejected;
incomplete I/O staging is retained separately and never becomes the published
generation. There is no second source link or consumer-side checksum/byte merge.

### Existing pinned compact consumers

Legacy consumers with an existing eight-byte import table may use
`--compact-sites SYMBOL --compact-ranges SYMBOL --compact-trap SYMBOL` instead
of per-image descriptor/site/trap tables. Each image may additionally declare
`--pinned OWNER:SITE_OFFSET:KIND:TARGET_OFFSET`; offsets accept decimal or `0x`
notation. Kinds 2/3 are the exact resident HI16/LO16 pair addressing cold data,
and kind 4 is an exact initialized resident callback word addressing cold code.
The packer verifies the original symbol, relocation, instruction and destination;
unused, ambiguous or changed declarations fail. These bindings retain the
consumer's pre-existing pinned lifetime discipline. They provide no new lease
or callback lifetime guarantee. The ordinary descriptor/leased API rejects them.
Finalization preserves these bindings and traps them in the runtime image while
restoring their original values in the complete guard image.

Module guard ranges include the complete 136-byte trampoline reserve, beginning
at its eight-byte magic/count header. This is required for hazard-tool discovery;
the reserved size and executable entry offsets do not change.

A resident guest that exceeds psx-rt's default 96 trampoline words can enable
`psx-rt/hazard-trampolines-192` (+384 bytes of initialized data), or the existing
`psx-rt/hazard-trampolines-256` (+640 bytes). If both are enabled, 256 wins.
These features change only the resident array's capacity. Each packed cold
image retains its separate 136-byte reservation: an eight-byte header and
32 instruction words.
