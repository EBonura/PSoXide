/*
 * The scene core, for psoxide.ld, opt-in: link with `-T <ram>.ld -T psoxide-core.ld -T psoxide.ld`
 * (these first), where <ram>.ld holds `RAM_SIZE_OVERRIDE = 0x1FE000;`, and depend on `psx-core`
 * with feature `link`. See sdk/crates/psx-core and the scene core ABI.
 *
 * The core's image (psx-core's `IMAGE`, input section `.psx_core`) runs at the window
 * 0x801FDF00..0x801FFF00 and is stored right after .data, where .bss will be, so it costs the
 * program no RAM once `_start` has cleared .bss. `__core_boot` is the entry: it copies the image
 * up before `_start` runs. Link with --no-check-sections as well (the store and .bss overlap).
 */
__initial_pc = __core_boot;
/* Where `__core_boot` goes next: the boot-overlay mover when psoxide-boot.ld is linked too (it
   moves .boot, runs the splash and goes on to `_start`), else psx-rt's `_start`. With both, the
   boot overlay is stored after the core's copy: BOOT_LMA = __psx_core_store_end. */
__core_next = DEFINED(__boot_entry) ? __boot_entry : _start;

/* The jump table's slot symbols come from psx-core (`link`), which takes them from psx-scene-abi. */

SECTIONS {
    /* The stored copy, right after .data. psoxide.ld lays .bss over it, so it costs the program no
       RAM; that overlap is on purpose, so these programs link with --no-check-sections. */
    .psx_core_store : {
        __psx_core_store = .;
        KEEP(*(.psx_core))
        . = ALIGN(4);
        __psx_core_store_end = .;
    } > IMAGE
}
/* As psoxide-boot.ld: INSERT must directly follow SECTIONS's closing brace. */
INSERT AFTER .data;

ASSERT(STACK_INIT == 0x801FDF00, "a program on the core links with RAM_SIZE_OVERRIDE = 0x1FE000")
ASSERT(__psx_core_store_end - __psx_core_store <= 0x2000, "the core image must fit the window")
ASSERT(__psx_core_store_end > __psx_core_store, "psx-core's IMAGE is missing: depend on psx-core with feature link")
