/*
 * PSoXide linker script.
 *
 * The link writes a flat binary (`--oformat=binary`) that is already a
 * PS-X EXE: 800h bytes of header, then the program image the kernel copies
 * to the header's destination address. The header is described in psx-spx,
 * "CDROM File Formats", "Executables and Debug Files". Every field is filled
 * in here from link-time symbols, so no post-link tool has to touch the
 * file.
 *
 * The image loads to 80010000h: KSEG0 (cached), the first address past the
 * 64 KiB the kernel keeps for itself (psx-spx "BIOS Memory Map").
 */

RAM_BASE    = 0x80000000;   /* KSEG0 cached RAM */
RAM_SIZE    = 2M;
BIOS_SIZE   = 64K;          /* First 64 KiB is BIOS / kernel scratch */
LOAD_ADDR   = RAM_BASE + BIOS_SIZE;
STACK_INIT  = RAM_BASE + 0x001FFF00;
STACK_INIT  = RAM_BASE + 0x001FFF00;

/* Build-time RAM budget for the stack. The stack runs downward from
   STACK_INIT while .text/.data/.bss grow upward from LOAD_ADDR. We carve
   this reserve out of the RAM region the static sections may occupy, so a
   program that no longer leaves room for the stack overruns the region at
   LINK time (a clean rust-lld "section will not fit in region" error)
   instead of silently colliding $sp with .bss at runtime. 32 KiB is a
   generous floor for the call stack; the heap is unused while alloc is off. */
STACK_RESERVE = 0x8000;     /* 32 KiB reserved for the runtime stack */

/* Bytes in front of the image that the kernel reads and does not load. */
EXE_HEAD_BYTES = 0x800;

/* One region holds the header and everything the kernel loads after it.
   It starts EXE_HEAD_BYTES below LOAD_ADDR, so the header occupies the
   first 800h bytes of the flat file and `.text` lands on LOAD_ADDR. Its
   top is still the start of the stack reserve. */
MEMORY {
    IMAGE (rwx) : ORIGIN = LOAD_ADDR - EXE_HEAD_BYTES,
                  LENGTH = EXE_HEAD_BYTES + RAM_SIZE - BIOS_SIZE - STACK_RESERVE
}

ENTRY(_start)

SECTIONS {
    /* PS-X EXE header, field by field as psx-spx lists it. Offsets are
       from the start of the file. */
    .ps_exe_head : {
        LONG(0x582D5350);           /* 000h "PS-X" */
        LONG(0x45584520);           /* 004h " EXE" */
        LONG(0);                    /* 008h zero */
        LONG(0);                    /* 00Ch zero */
        LONG(_start);               /* 010h initial PC */
        LONG(0);                    /* 014h initial GP: the runtime does not use $gp */
        LONG(LOAD_ADDR);            /* 018h destination address */
        LONG(__data_end - __text_start); /* 01Ch bytes stored after the header */
        LONG(0);                    /* 020h data start: nothing for the kernel to do */
        LONG(0);                    /* 024h data size */
        LONG(0);                    /* 028h BSS start: `_start` clears .bss itself */
        LONG(0);                    /* 02Ch BSS size */
        LONG(STACK_INIT);           /* 030h SP/FP base */
        LONG(0);                    /* 034h SP/FP offset */
        /* 038h-04Bh is scratch for A(43h), 04Ch on would hold a region
           string the BIOS ignores. Both stay zero, as does everything up
           to LOAD_ADDR, where the region puts the end of the header. */
        . = LOAD_ADDR;
    } > IMAGE

    /* Program code. `.text._start` is forced first so the entry
       point lives at `LOAD_ADDR` exactly. */
    .text : {
        __text_start = .;
        KEEP(*(.text._start));
        /* Instruction-cache affinity: the R3000 I-cache is 4 KiB and
           direct-mapped, so a hot leaf that happens to land 4 KiB below
           its caller's inner loop evicts that loop on every call. A refill
           profile of the Cortex whole-level tape (2026-09-02) showed
           materialize_pxbsp_face and draw_pxbsp_faces evicting each other
           for 10% of all refills, the animation pose sampler and the model
           submitter for 9%, and the collision trace kernels for 4%. Placing
           each caller directly before its leaves keeps those pairs in
           disjoint cache sets. Name fragments match the mangled
           function-section names regardless of crate hash; a renamed
           function silently falls back to default placement, which is
           safe. Shared engine code only, so every guest benefits. */
        *(.text.*draw_pxbsp_faces*)
        *(.text.*materialize_pxbsp_face*)
        *(.text.*collect_marked_faces_ascending*)
        *(.text.*submit_textured_model_geometry_impl*)
        *(.text.*AnimationPoseSample*4pose*)
        *(.text.*pose_looped_q12*)
        *(.text.*flush_blended_model_vertex_chunk*)
        *(.text.*shade_model_material_at_bounds*)
        *(.text.*point_light_weight*)
        *(.text.*CollisionHull*trace_into*)
        *(.text.*point_contents_from*)
        *(.text.*plane_contact*)
        *(.text.*model_collision_hull*)
        *(.text .text.*);
        __text_end = .;
    } > IMAGE

    /* Initialised data + read-only data. PS1 has no MMU so .rodata
       isn't actually read-only at runtime; keep it with .data so
       the linker can pack everything into one contiguous load. */
    .data : {
        __data_start = .;
        *(.data .data.*);
        *(.rodata .rodata.*);
        *(.got);
        . = ALIGN(2048);   /* 2 KiB-align for ISO sector loading later */
        __data_end = .;
    } > IMAGE

    /* Uninitialised data - zeroed by `_start`, not stored in file. */
    .bss (NOLOAD) : {
        __bss_start = .;
        *(.bss .bss.*);
        *(COMMON);
        . = ALIGN(4);
        __bss_end = .;
    } > IMAGE

    /* Heap starts right after BSS and runs up to just below the stack. */
    . = ALIGN(4);
    __heap_start = .;
    __heap_end   = STACK_INIT - STACK_RESERVE;   /* keep heap below the reserved stack */

    /* The header above only describes the image if these hold. */
    ASSERT(__text_start == LOAD_ADDR, "the entry section must start at LOAD_ADDR")
    ASSERT((__data_end - __text_start) % EXE_HEAD_BYTES == 0, "the stored image must be whole 800h-byte sectors")

    /* Discard MIPS ABI glue not needed on bare metal. */
    /DISCARD/ : {
        *(.region); /* Discard legacy vendor markers from older object files. */
        *(.MIPS.abiflags)
        *(.reginfo)
        *(.eh_frame_hdr)
        *(.eh_frame)
        *(.comment)
        *(.note.*)
    }
}
