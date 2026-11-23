/* An overlay of hello-scene: a raw image at 0x80100000, inside the base program's heap region,
   entered through the two words at its start. The core's slot symbols come from slots.ld. */
MEMORY { OVERLAY (rwx) : ORIGIN = 0x80100000, LENGTH = 0x10000 }
ENTRY(__overlay_table)
SECTIONS {
    .text : { KEEP(*(.text.__overlay_table)) *(.text .text.*) . = ALIGN(4); } > OVERLAY
    .data : { *(.data .data.*) *(.rodata .rodata.*) *(.sdata*) . = ALIGN(4); } > OVERLAY
    .bss (NOLOAD) : { *(.bss .bss.* .sbss* COMMON) . = ALIGN(4); } > OVERLAY
    /DISCARD/ : { *(.MIPS.abiflags) *(.reginfo) *(.eh_frame*) *(.comment) *(.note.*) }
}
psx_core_switch = 0x801FDF08;
