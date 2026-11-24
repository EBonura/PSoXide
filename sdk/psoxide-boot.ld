/*
 * Boot-only memory for psoxide.ld, opt-in: link with `-T psoxide-boot.ld -T psoxide.ld` (this
 * script first; after psoxide-core.ld when the program is on the scene core too), as an ELF (no
 * `--oformat=binary`), and write the EXE from its load addresses (`psx-exe`, or psoxide-pgo,
 * which does the same); see psoxide.ld.
 *
 * Code and data a program needs only once, at boot, before its heap fills (the boot splash, the
 * film player, anything it puts in a `.boot_only` section) run from memory the program reuses
 * afterwards: by default the start of the heap (BOOT_VMA = __bss_end, rounded up to 16), so the
 * program's first heap blocks lie over them. In the EXE file they follow .data (the scene core's
 * stored image follows them), where .bss will be. psx-rt's `__boot_entry` moves them up before
 * `_start` clears .bss, zeroes `.boot_bss` and flushes the I-cache over them. The program must be
 * done with them before anything writes over their run range (its first heap allocation, with
 * the default).
 *
 * A program that reuses other memory names it with a `-T` file before this script:
 *   BOOT_VMA_OVERRIDE      where .boot runs (default: __bss_end rounded up to 16);
 *   BOOT_RUN_END_OVERRIDE  the end of the memory it reuses (default: __heap_end);
 *   BOOT_LMA_OVERRIDE      where it is stored (default: right after .data; not on the core).
 * The assertions below refuse a run range outside __bss_end..BOOT_RUN_END (so over .text, .data
 * or the .bss `_start` clears, or over the stack reserve, the core's window or the handoff block
 * above it), and a BOOT_RUN_END past __heap_end.
 *
 * Programs add their own sections with a further script that names the same output section,
 * or place them in `.boot_only`.
 */
/* psoxide.ld (its header's PC, or `__core_next` for psoxide-core.ld) reads `__boot_initial`; a
   program without this script never references `__boot_entry` (ld.lld keeps what a script
   expression names, taken or not). */
__boot_initial = __boot_entry;
BOOT_VMA = DEFINED(BOOT_VMA_OVERRIDE) ? BOOT_VMA_OVERRIDE : ALIGN(__bss_end, 16);
BOOT_RUN_END = DEFINED(BOOT_RUN_END_OVERRIDE) ? BOOT_RUN_END_OVERRIDE : __heap_end;
BOOT_LMA = DEFINED(BOOT_LMA_OVERRIDE) ? BOOT_LMA_OVERRIDE : __data_end;
SECTIONS {
    /* Boot-only code resident code may call while it boots (before its heap reaches this
       memory): the program's own `.boot_only` sections and the boot crates' entry points, each
       placed by its own crate (psx-splash's `show`, `show_with_partner`). Code takes `.text.`
       names, data `.rodata.` ones, so the hazard tools read them as what they are and a program
       without this script keeps them in .text and .data. */
    .boot BOOT_VMA : AT(BOOT_LMA) {
        __boot_start = .;
        *(.boot_only .boot_only.* .text.boot_only.* .rodata.boot_only.*)
        . = ALIGN(4);
    } > RELOCATED
    /* Boot-only code and data only boot code reaches: psx-splash's own functions and statics
       (placed by name in the crate: never a generic instance of another crate's code, which
       resident code may share), and the film player, still taken by crate name. Resident code
       must never reach into it: NOCROSSREFS_TO below fails the link if it does (a generic the
       player shares with resident code, say). Right after .boot in RAM and in the file (named: the
       region's own count is past the core's window when the program has one). */
    .boot_inner ADDR(.boot) + SIZEOF(.boot) : AT(LOADADDR(.boot) + SIZEOF(.boot)) {
        *(.boot_inner .boot_inner.* .text.boot_inner.* .rodata.boot_inner.*)
        *(.text.*psx_fmv* .rodata.*psx_fmv* .data.*psx_fmv*)
        . = ALIGN(4);
        __boot_load_end = .;
    } > RELOCATED
    /* Right after .boot (named: the region's own count is past the core's window when the
       program has one). Its load address (which nothing reads) follows .bss's, which follows the
       stored bytes (psoxide.ld), so no two sections share load addresses. */
    .boot_bss __boot_load_end (NOLOAD) : AT(__load_end + SIZEOF(.bss)) {
        *(.bss.*psx_fmv*)
        . = ALIGN(4);
        __boot_end = .;
    } > RELOCATED
    __boot_lma = LOADADDR(.boot);
    __boot_lma_end = __boot_lma + SIZEOF(.boot) + SIZEOF(.boot_inner);
}
/* ld.lld takes a script with no INSERT for the main one; this is the addition. INSERT must
   directly follow SECTIONS's closing brace. */
INSERT AFTER .bss;

ASSERT(__boot_start >= __bss_end, ".boot must run above .bss (_start clears .bss after the move)")
ASSERT(__boot_end <= BOOT_RUN_END, ".boot does not fit the memory it reuses (BOOT_RUN_END)")
ASSERT(BOOT_RUN_END <= __heap_end, ".boot must run below the stack reserve, the core's window and the handoff block")
ASSERT(__boot_lma >= __data_end, ".boot must be stored after .data")
/* On the core, .boot is stored right after .data and the core's image right after it: a core
   switch loads the program up to its stored core, so it loads .boot too. */
ASSERT(!DEFINED(__psx_core_store_lma) || __boot_lma == __data_end, "on the scene core, .boot is stored right after .data (no BOOT_LMA_OVERRIDE)")
ASSERT(__boot_start % 4 == 0 && __boot_lma % 4 == 0, ".boot's run and load addresses must be word aligned")

/* Resident code and data never reference boot-only code or data that is not an entry point: once
   the heap has the memory, it is gone. ld.lld fails the link on any such reference. */
NOCROSSREFS_TO(.boot_inner .text .data)
NOCROSSREFS_TO(.boot_bss .text .data)
