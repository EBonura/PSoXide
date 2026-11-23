/*
 * The scene core's own link: everything at the window 0x801FDF00..0x801FFF00, the jump table
 * first. The flat output (`--oformat=binary`) is text and data; .bss is the core's statics, zeroed
 * by a program's `__core_boot` on a cold entry. Stack: none in the window (a switch runs at the
 * top of the outgoing program's stack reserve).
 */
MEMORY {
    WINDOW (rwx) : ORIGIN = 0x801FDF00, LENGTH = 0x2000
}

ENTRY(__psx_core_table)

SECTIONS {
    .text : {
        __core_text_start = .;
        KEEP(*(.text.__psx_core_table))
        *(.text .text.*)
        . = ALIGN(4);
        __core_text_end = .;
    } > WINDOW

    .data : {
        *(.data .data.*)
        *(.rodata .rodata.*)
        *(.sdata .sdata.*)
        *(.got)
        . = ALIGN(4);
        __core_image_end = .;
    } > WINDOW

    .bss (NOLOAD) : {
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        *(COMMON)
        . = ALIGN(4);
        __core_bss_end = .;
    } > WINDOW

    ASSERT(__psx_core_table == 0x801FDF00, "the jump table must open the window")
    ASSERT(__core_bss_end <= 0x801FFF00, "the core and its statics must fit the window")

    /DISCARD/ : {
        *(.MIPS.abiflags)
        *(.reginfo)
        *(.eh_frame_hdr)
        *(.eh_frame)
        *(.comment)
        *(.note.*)
    }
}
