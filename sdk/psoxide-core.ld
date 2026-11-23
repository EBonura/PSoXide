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

/* The core's jump table: one slot per call, fixed by the ABI. */
__psx_core_attach       = 0x801FDF00;
psx_core_switch         = 0x801FDF08;
psx_core_read           = 0x801FDF10;
psx_core_pause          = 0x801FDF18;
psx_core_client_attach  = 0x801FDF20;
psx_core_client_detach  = 0x801FDF28;
psx_core_fatal          = 0x801FDF30;
psx_core_music_yield    = 0x801FDF38;
psx_core_stream_open    = 0x801FDF40;
psx_core_stream_sector  = 0x801FDF48;
psx_core_cd             = 0x801FDF50;
psx_core_set_idle_hook  = 0x801FDF58;

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
