/*
 * Boot-only memory for psoxide.ld, opt-in: link with `-T psoxide-boot.ld -T psoxide.ld` (this
 * script first; after psoxide-core.ld when the program is on the scene core too), as an ELF (no
 * `--oformat=binary`), and write the EXE from its load addresses (`psx-exe`, or psoxide-pgo,
 * which does the same); see psoxide.ld.
 *
 * Code and data a program needs only once, at boot, before its heap fills (the boot splash, the
 * film player, anything it puts in a `.text.boot_only.*` section) run from memory the program
 * reuses afterwards: by default the start of the heap (BOOT_VMA = __bss_end, rounded up to 16),
 * so the program's first heap blocks lie over them. In the EXE file they follow .data (the core's
 * stored image follows them), where .bss will be. psx-rt's `__boot_entry` moves them up before
 * `_start` clears .bss and zeroes `.boot_bss` (`_start` flushes the whole I-cache before `main`).
 * The program must be done with them before anything writes over their run range (its first heap
 * allocation, with the default).
 *
 * A program that reuses other memory names it with a `-T` file before this script:
 *   BOOT_VMA_OVERRIDE      where .boot runs (default: __bss_end rounded up to 16);
 *   BOOT_RUN_END_OVERRIDE  the end of the memory it reuses (default: __heap_end, or
 *                          STACK_INIT - BOOT_STACK_OVERRIDE when that is set);
 *   BOOT_STACK_OVERRIDE    the stack its boot code needs, in bytes below STACK_INIT (default:
 *                          the whole STACK_RESERVE): while boot code is live the stack never
 *                          goes deeper, so boot code may run in the rest of the reserve;
 *   BOOT_LMA_OVERRIDE      where it is stored (default: right after .data; not on the core).
 * The assertions below refuse a run range outside __bss_end..BOOT_RUN_END (so over .text, .data
 * or the .bss `_start` clears), a BOOT_RUN_END closer than BOOT_STACK to STACK_INIT (so into the
 * stack the program says boot needs, the core's window or the handoff block above it), and a
 * BOOT_STACK smaller than the boot crates' own deepest stack (psx-splash's
 * `__psx_splash_stack_bytes`, below `show`'s caller). A program with no heap to spare (Ocarina)
 * sets only `BOOT_STACK_OVERRIDE` and runs .boot from the end of .bss into the reserve; one that
 * runs it from its own pool (Hollow Knight) sets BOOT_VMA_OVERRIDE and BOOT_RUN_END_OVERRIDE up to
 * the reserve's floor. BOOT_STACK is the program's statement of its deepest stack while boot code
 * runs: its own frames down to the boot call, the deepest boot code, and any callback boot code
 * makes into the program (the splash's pad closure). The link checks the boot crates' part; the
 * program's own stack proof covers the rest.
 *
 * Programs place their own boot-only code and data by section name: `.text.boot_only.*` (entry
 * points resident code calls; data `.rodata.boot_only.*`, `.data.boot_only.*`) and
 * `.text.boot_inner.*` (code only boot code calls). A further script naming `.boot` or
 * `.boot_inner` again does not add to these: ld.lld makes a second output section of that name.
 */
/* psoxide.ld (its header's PC, or `__core_next` for psoxide-core.ld) reads `__boot_initial`; a
   program without this script never references `__boot_entry` (ld.lld keeps what a script
   expression names, taken or not). */
__boot_initial = __boot_entry;
BOOT_VMA = DEFINED(BOOT_VMA_OVERRIDE) ? BOOT_VMA_OVERRIDE : ALIGN(__bss_end, 16);
BOOT_STACK = DEFINED(BOOT_STACK_OVERRIDE) ? BOOT_STACK_OVERRIDE : STACK_RESERVE;
BOOT_RUN_END = DEFINED(BOOT_RUN_END_OVERRIDE) ? BOOT_RUN_END_OVERRIDE
             : DEFINED(BOOT_STACK_OVERRIDE) ? STACK_INIT - BOOT_STACK_OVERRIDE : __heap_end;
/* The boot crates' own deepest stack (0 without them): see the assertion below. */
BOOT_STACK_FLOOR = DEFINED(__psx_splash_stack_bytes) ? __psx_splash_stack_bytes : 0;
BOOT_LMA = DEFINED(BOOT_LMA_OVERRIDE) ? BOOT_LMA_OVERRIDE : __data_end;
SECTIONS {
    /* What resident code may refer to while it boots (before its heap reaches this memory):
       - entry points: the program's own `.text.boot_only.*` code and the boot crates' public calls
         (psx-splash's `show` and `show_with_partner`, psx-fmv's `player::play`,
         `find_root_file`, `memory_words`), each placed by its own crate;
       - the boot crates' statics (psx-splash's font and mark, psx-fmv's tables and its reader,
         decoder and sector buffer). With one caller, LTO folds a static's address into the
         shared resident code that caller uses (psx-io's `SectorReader` methods; psx-splash
         uploads through boot-only copies of its own, so no font upload does that any more).
         Folding happens only when every call passes that static, and only the boot crate can
         name it, so that resident code runs only from boot code.
       Code takes `.text.` names and data `.rodata.`/`.data.` ones, so the hazard tools read them
       as what they are and a program without this script keeps them in .text and .data. */
    .boot BOOT_VMA : AT(BOOT_LMA) {
        __boot_start = .;
        *(.text.boot_only.* .rodata.boot_only.* .data.boot_only.*)
        . = ALIGN(4);
    } > RELOCATED
    /* Boot-only code only boot code calls: psx-splash's and psx-fmv's own functions, placed by
       name in each crate, never a generic instance of another crate's code (resident code may
       share that one, so it stays resident). Resident code must never call into it:
       NOCROSSREFS_TO below fails the link if it does. Right after .boot in RAM and in the file
       (named: the region's own count is past the core's window when the program has one). */
    .boot_inner ADDR(.boot) + SIZEOF(.boot) : AT(BOOT_LMA + SIZEOF(.boot)) {
        *(.text.boot_inner.* .rodata.boot_inner.*)
        . = ALIGN(4);
        __boot_load_end = .;
    } > RELOCATED
    /* The boot crates' zeroed statics, right after .boot_inner (named, as above). Its load
       address (which nothing reads) follows .bss's, which follows the stored bytes (psoxide.ld),
       so no two sections share load addresses. */
    .boot_bss __boot_load_end (NOLOAD) : AT(__load_end + SIZEOF(.bss)) {
        *(.bss.boot_only.*)
        . = ALIGN(4);
        __boot_end = .;
    } > RELOCATED
    /* Boot-only sections without a `.text.`, `.rodata.`, `.data.` or `.bss.` prefix: the hazard
       tools would not know code from data in them (code there is never patched), and ld.lld
       would place a misspelt one as an orphan without a word. Nothing may land here (asserted
       below): name boot-only code `.text.boot_only.*` or `.text.boot_inner.*`. */
    .boot_unprefixed : { *(.boot_only .boot_only.* .boot_inner .boot_inner.*) } > RELOCATED
    /* BOOT_LMA, not LOADADDR(.boot): an empty .boot (LTO dropped the splash, or a build that
       never calls it) has no load address of its own, and the stored size came out as
       0x7fff2000. */
    __boot_lma = BOOT_LMA;
    __boot_lma_end = __boot_lma + SIZEOF(.boot) + SIZEOF(.boot_inner);
}
/* ld.lld takes a script with no INSERT for the main one; this is the addition. INSERT must
   directly follow SECTIONS's closing brace. */
INSERT AFTER .bss;

ASSERT(__boot_start >= __bss_end, ".boot must run above .bss (_start clears .bss after the move)")
ASSERT(__boot_end <= BOOT_RUN_END, ".boot does not fit the memory it reuses (BOOT_RUN_END)")
ASSERT(BOOT_RUN_END <= STACK_INIT - BOOT_STACK, ".boot must run below the stack its boot code needs (BOOT_STACK_OVERRIDE, default the whole stack reserve), the core's window and the handoff block")
ASSERT(BOOT_STACK >= BOOT_STACK_FLOOR, "BOOT_STACK_OVERRIDE is less than the boot crates' own stack (__psx_splash_stack_bytes): the stack would reach .boot")
ASSERT(BOOT_STACK <= STACK_INIT - __bss_end, "BOOT_STACK_OVERRIDE reaches below .bss")
ASSERT(__boot_lma >= __data_end, ".boot must be stored after .data")
ASSERT(__boot_start >= __boot_lma, ".boot must run at or above where it is stored (psx-rt's mover copies from the top down)")
/* On the core, .boot is stored right after .data and the core's image right after it: a core
   switch loads the program up to its stored core, so it loads .boot too. */
ASSERT(!DEFINED(__psx_core_store_lma) || __boot_lma == __data_end, "on the scene core, .boot is stored right after .data (no BOOT_LMA_OVERRIDE)")
ASSERT(__boot_start % 4 == 0 && __boot_lma % 4 == 0, ".boot's run and load addresses must be word aligned")

/* Resident code and data never refer to boot-only code that is not an entry point: once the heap
   has the memory, it is gone. ld.lld fails the link on any such reference. */
NOCROSSREFS_TO(.boot_inner .text .data)
ASSERT(SIZEOF(.boot_unprefixed) == 0, "a boot-only section without a .text./.rodata./.data. prefix (`.boot_only.x`): name code `.text.boot_only.x` so the hazard tools patch it")
