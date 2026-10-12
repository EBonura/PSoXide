/* The cold function is linked in the same ELF and compiler invocation as
   its typed resident caller. The packaging tool excludes this section from
   the resident EXE and includes it only in the checked module payload. */
MEMORY { COLD (rx) : ORIGIN = 0x80408000, LENGTH = 64K }
SECTIONS {
    .overlay : { KEEP(*(.overlay.text)); } > COLD
} INSERT BEFORE .text;
