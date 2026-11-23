/*
 * Boot-only memory for psoxide.ld, opt-in: link with `-T psoxide-boot.ld -T psoxide.ld` (this
 * script first), --defsym=BOOT_VMA=..., --defsym=BOOT_LMA=..., -u __boot_entry.
 *
 * Code and data a program needs only once, at boot, before its heap fills (the film player, the
 * boot splash, anything it puts in a `.boot_only` section) are placed at the start of the heap
 * (BOOT_VMA = __bss_end), so the program's heap block lies over them and takes their memory once
 * it grows that far. In the EXE file they follow .data (BOOT_LMA = __data_end), where .bss will
 * be; the program's `__boot_entry` (the initial PC) moves them up before `_start` clears .bss.
 * The program must be done with them before its heap use reaches them.
 *
 * The two addresses come from a first link with placeholder values (the sections here keep their
 * sizes, so .data and .bss end where they did), then a second with the real ones.
 *
 * Programs add their own sections with a further script that names the same output section,
 * or place them in `.boot_only`.
 */
/* psoxide.ld reads `__initial_pc`; a program without this script never references
   `__boot_entry` (ld.lld keeps what a script expression names, taken or not). */
/* PROVIDE: with psoxide-core.ld the entry is `__core_boot`, which runs first and then jumps here. */
PROVIDE(__initial_pc = __boot_entry);
SECTIONS {
    .boot BOOT_VMA : AT(BOOT_LMA) {
        __boot_start = .;
        *(.boot_only .boot_only.*)
        *(.text.*psx_fmv* .rodata.*psx_fmv* .data.*psx_fmv*)
        *(.text.*psx_splash* .rodata.*psx_splash*)
        . = ALIGN(4);
        __boot_load_end = .;
    }
    .boot_bss (NOLOAD) : {
        *(.bss.*psx_fmv*)
        . = ALIGN(4);
        __boot_end = .;
    }
    __boot_lma = BOOT_LMA;
}
/* ld.lld takes a script with no INSERT for the main one; this is the addition. INSERT must
   directly follow SECTIONS's closing brace. */
INSERT AFTER .bss;
