/*
 * The scene core, for psoxide.ld, opt-in: link with `-T <ram>.ld -T psoxide-core.ld -T psoxide.ld`
 * (these first), where <ram>.ld holds `RAM_SIZE_OVERRIDE = 0x1FE000;`, and depend on `psx-core`
 * with feature `link`. See sdk/crates/psx-core and the scene core ABI.
 *
 * The core's image (psx-core's `IMAGE`, input section `.psx_core`) runs at the window
 * 0x801FDF00..0x801FFF00 and is stored after .data and .boot, last, where .bss will be, so it
 * costs the program no RAM once `_start` has cleared .bss, and a core switch (which loads a
 * program up to its stored core) loads .boot too. `__core_boot` is the entry: it copies the image
 * from its load address up into the window, then goes on to `__core_next` (psoxide.ld:
 * `__boot_entry` with psoxide-boot.ld, else `_start`, whatever the order of the scripts).
 *
 * The run address and the load address differ, so the program links an ELF (no
 * `--oformat=binary`) and its EXE is written from the load addresses (`psx-exe`, or psoxide-pgo,
 * which does the same); see psoxide.ld.
 */
/* rustc links a dependency only when some code names it: a program whose own code never names
   psx_core has no `__core_boot`. Said by name here, not as lld's "symbol not found". */
ASSERT(DEFINED(__core_boot), "__core_boot is missing: no code names the psx_core crate, so rustc left it out of the link; add `use psx_core as _;` to the program's main.rs (or call psx_core::link)")
__initial_pc = DEFINED(__core_boot) ? __core_boot : 0;
/* `__core_next` is psoxide.ld's: here, before psoxide-boot.ld, DEFINED(__boot_initial) would be
   false and the chain would skip the boot mover. */

/* The jump table's slot symbols come from psx-core (`link`), which takes them from psx-scene-abi. */

SECTIONS {
    /* Run at the window, stored after .data and .boot (psoxide.ld's `__psx_core_store_at`).
       psoxide.ld lays .bss over the stored copy. */
    .psx_core_store STACK_INIT : AT(__psx_core_store_at) {
        __psx_core_store = .;
        KEEP(*(.psx_core))
        . = ALIGN(4);
        __psx_core_store_end = .;
    } > RELOCATED
    __psx_core_store_lma = LOADADDR(.psx_core_store);
    __psx_core_store_load_end = __psx_core_store_lma + SIZEOF(.psx_core_store);
}
/* As psoxide-boot.ld: INSERT must directly follow SECTIONS's closing brace. */
INSERT AFTER .data;

ASSERT(STACK_INIT == 0x801FDF00, "a program on the core links with RAM_SIZE_OVERRIDE = 0x1FE000")
ASSERT(__psx_core_store_end - __psx_core_store <= 0x2000, "the core image must fit the window")
ASSERT(__psx_core_store_end > __psx_core_store, "psx-core's IMAGE is missing: depend on psx-core with feature link, and name the crate in code (`use psx_core as _;`)")
/* The entry code is placed by the program's main script (psoxide.ld: `.text_entry.*`, last in
   .text). A forked script that leaves that line out links it as an orphan section outside .text:
   refused here, whatever the orphan-handling option. */
ASSERT(!DEFINED(__core_boot) || (__core_boot >= __text_start && __core_boot < __text_end), "__core_boot is outside .text: the program's link script must place .text_entry.* in .text (see psoxide.ld)")
