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
 * A program with sections stored in one place and run from another (the scene core's image,
 * psoxide-core.ld; boot-only code, psoxide-boot.ld) links an ELF instead and writes the EXE
 * from its load addresses with `psx-exe` (tools/psoxide-hazard; psoxide-pgo does it for the
 * programs it builds): ld.lld's `--oformat=binary` places every section at its run address.
 * The stored copies follow .data in the file, in this order: .data, .boot, the core's image.
 * .bss is laid over them, so they cost no RAM once their movers have run (`__core_boot`, then
 * `__boot_entry`, then `_start`).
 *
 * The image loads to 80010000h: KSEG0 (cached), the first address past the
 * 64 KiB the kernel keeps for itself (psx-spx "BIOS Memory Map").
 */

RAM_BASE    = 0x80000000;   /* KSEG0 cached RAM */
/* RAM_SIZE_OVERRIDE (bytes) at link time sets the RAM the program links for, as
   STACK_RESERVE_OVERRIDE does the stack: 8M for a dev kit, 2M (the default) for a retail
   console. Pass it the way STACK_RESERVE_OVERRIDE is passed (a `-T<file>` before this script,
   so the link map records it and psoxide-size reads it; `--defsym` works but the map does
   not carry it). The stack starts 100h below the top of RAM. */
RAM_SIZE    = DEFINED(RAM_SIZE_OVERRIDE) ? RAM_SIZE_OVERRIDE : 2M;
BIOS_SIZE   = 64K;          /* First 64 KiB is BIOS / kernel scratch */
/* LOAD_ADDR_OVERRIDE (an address) at link time moves where the image loads, the same way:
   a `-T<file>` before this script holding `LOAD_ADDR_OVERRIDE = 0x...;`. For a program that
   must sit above another one's image (a boot program, a chain loader); every game and every
   scene keeps the default. It must be at or above RAM_BASE + BIOS_SIZE (asserted below). */
LOAD_ADDR   = DEFINED(LOAD_ADDR_OVERRIDE) ? LOAD_ADDR_OVERRIDE : RAM_BASE + BIOS_SIZE;
STACK_INIT  = RAM_BASE + RAM_SIZE - 0x100;

/* Build-time RAM budget for the stack. The stack runs downward from
   STACK_INIT while .text/.data/.bss grow upward from LOAD_ADDR. We carve
   this reserve out of the RAM region the static sections may occupy, so a
   program that no longer leaves room for the stack overruns the region at
   LINK time (a clean rust-lld "section will not fit in region" error)
   instead of silently colliding $sp with .bss at runtime. 32 KiB is a
   generous floor for the call stack; the heap is unused while alloc is off.

   A program whose stack goes deeper sets STACK_RESERVE_OVERRIDE (bytes) at
   link time instead of patching this file. Either way works and the default
   is unchanged when nothing sets it:
     * `-Clink-arg=-T<file>` placed BEFORE this script, where <file> holds
       `STACK_RESERVE_OVERRIDE = 0x...;`. The link map then carries the line,
       so psoxide-size reports the heap against the reserve actually linked.
     * `-Clink-arg=--defsym=STACK_RESERVE_OVERRIDE=0x...`, which the link
       honours too but the map does not record. */
STACK_RESERVE = DEFINED(STACK_RESERVE_OVERRIDE) ? STACK_RESERVE_OVERRIDE : 0x8000; /* default 32 KiB */

/* Bytes in front of the image that the kernel reads and does not load. */
EXE_HEAD_BYTES = 0x800;

/* One region holds the header and everything the kernel loads after it.
   It starts EXE_HEAD_BYTES below LOAD_ADDR, so the header occupies the
   first 800h bytes of the flat file and `.text` lands on LOAD_ADDR. Its
   top is still the start of the stack reserve. */
MEMORY {
    IMAGE (rwx) : ORIGIN = LOAD_ADDR - EXE_HEAD_BYTES,
                  LENGTH = EXE_HEAD_BYTES + RAM_BASE + RAM_SIZE - LOAD_ADDR - STACK_RESERVE
    /* Run addresses of sections stored elsewhere (psoxide-core.ld, psoxide-boot.ld). They are
       counted where they are stored, by the assertions below and in their scripts, not here. */
    RELOCATED (rwx) : ORIGIN = RAM_BASE, LENGTH = 8M
}

ENTRY(_start)

/* Where psoxide-core.ld's `__core_boot` goes on to: the boot-only mover when psoxide-boot.ld is
   linked, else `_start`. Here, after both scripts, so the order they are given in cannot drop
   the mover (DEFINED() only sees symbols of the scripts before it). */
__core_next = DEFINED(__boot_initial) ? __boot_initial : _start;

SECTIONS {
    /* PS-X EXE header, field by field as psx-spx lists it. Offsets are
       from the start of the file. */
    .ps_exe_head : {
        LONG(0x582D5350);           /* 000h "PS-X" */
        LONG(0x45584520);           /* 004h " EXE" */
        LONG(0);                    /* 008h zero */
        LONG(0);                    /* 00Ch zero */
        /* 010h initial PC: `__initial_pc` when a script defines it (psoxide-core.ld: the core's
           mover), else `__boot_initial` (psoxide-boot.ld: the boot-only mover), else `_start`.
           With both, `__core_boot` runs first and goes on to `__boot_entry`. */
        LONG(DEFINED(__initial_pc) ? __initial_pc : DEFINED(__boot_initial) ? __boot_initial : _start);
        LONG(0);                    /* 014h initial GP: the runtime does not use $gp */
        LONG(LOAD_ADDR);            /* 018h destination address */
        LONG(__image_end - __text_start); /* 01Ch bytes stored after the header */
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
        /* The direct-access guard's body (psx-rt's `wait_idle`, section
           `.text.__psx_rt_wait_idle`) goes in a window of its own, so the script can tell
           whether it is in the image. Nothing but code that arms the guard keeps it there
           (`psx_io::gpu::arm_direct_access_guard` carries an R_MIPS_NONE reference to it,
           which section collection honours), and `wait_idle` callers that need it call it
           directly. See sdk/docs/PRESENT.md. */
        /* A program's own hot placement: functions it marks
           `#[link_section = ".psx_hot.<NN>_<name>"]` are placed here, in name order (so the
           numbers choose which sit side by side in the 4 KiB direct-mapped I-cache), ahead of
           the rest of the text. Nothing in it, nothing changes. A program that needs a layout
           the SDK's rules do not give it uses this instead of rewriting this script. */
        *(SORT_BY_NAME(.psx_hot .psx_hot.*))
        . = ALIGN(4);
        __psx_guard_window_start = .;
        *(.text.__psx_rt_wait_idle)
        __psx_guard_window_end = .;
        *(.text .text.*);
        /* Cold code, after everything else (`#[link_section = ".psx_cold.<name>"]`): code that
           runs once or rarely (a loader, a settings screen) kept apart from the hot text so it
           does not share I-cache sets or pages with it. */
        *(.psx_cold .psx_cold.*);
        __text_end = .;
    } > IMAGE

    /* The call target of psx-io's armed guard: `wait_idle`'s address when it is in the image,
       else 0. `run_armed_guard` runs only after an arm, and an arm puts `wait_idle` in the
       image, so a program that never arms never calls the 0. A program's call tree stays free
       of calls through a register, which a pointer to `wait_idle` could not give it (the
       stack proofs), and a program that never arms the guard links no wait (a pointer
       registered at start-up would keep one linked). */
    __psx_direct_access_guard = __psx_guard_window_end > __psx_guard_window_start ? __psx_guard_window_start : 0;

    /* Initialised data + read-only data. PS1 has no MMU so .rodata
       isn't actually read-only at runtime; keep it with .data so
       the linker can pack everything into one contiguous load. */
    .data : {
        __data_start = .;
        *(.data .data.*);
        *(.rodata .rodata.*);
        *(.got);
        . = ALIGN(4);
        __data_end = .;
    } > IMAGE

    /* The kernel loads whole sectors, so the header's size reaches the next
       800h boundary past the data. Those last bytes (under a sector) are
       zeros that `hazard-patch` appends to the flat file (psx-iso's
       `pad_to_payload`), and the loader copies them over the start of .bss,
       which `_start` clears before anything reads it. Starting .bss right
       after the data instead of after this padding gives the program the
       bytes back. */
    /* The stored bytes, each where .bss will be: .data, then the boot-only sections
       (psoxide-boot.ld), then the scene core's stored image (psoxide-core.ld), last so that a
       core switch, which loads a program up to its stored core and no further, loads .boot too.
       Decided here, after both scripts: DEFINED() only sees symbols of the scripts before it. */
    __psx_core_store_at = DEFINED(__boot_lma_end) ? __boot_lma_end : __data_end;
    __load_end = DEFINED(__psx_core_store_load_end) ? __psx_core_store_load_end
               : DEFINED(__boot_lma_end) ? __boot_lma_end : __data_end;
    __image_end = __text_start + ((__load_end - __text_start + EXE_HEAD_BYTES - 1) & ~(EXE_HEAD_BYTES - 1));

    /* Uninitialised data - zeroed by `_start`, not stored in file. It runs right after .data,
       over the stored copies; its load address (which nothing reads) follows them, so no two
       sections share load addresses and the link keeps its section checks. */
    .bss __data_end (NOLOAD) : AT(__load_end) {
        __bss_start = .;
        *(.bss .bss.*);
        *(COMMON);
        . = ALIGN(4);
        __bss_end = .;
        /* Heap starts right after BSS and runs up to just below the stack. Set here, inside .bss,
           so the link map prints its own address on its line: set after .bss, the map printed
           the location counter there, which a script INSERTed after .bss (psoxide-boot.ld,
           psoxide-overlay.ld) moves past .bss, and the tools that read free RAM from the map
           (psoxide-size, `psoxide-pgo ram`, heap-guard) read that instead. */
        __heap_start = .;
    } > IMAGE

    /* The heap runs up to just below the stack reserve. */
    __heap_end   = STACK_INIT - STACK_RESERVE;   /* keep heap below the reserved stack */

    /* The header above only describes the image if these hold. */
    ASSERT(__text_start == LOAD_ADDR, "the entry section must start at LOAD_ADDR")
    ASSERT(LOAD_ADDR >= RAM_BASE + BIOS_SIZE, "LOAD_ADDR_OVERRIDE is inside the kernel's 64 KiB")
    ASSERT((__image_end - __text_start) % EXE_HEAD_BYTES == 0, "the stored image must be whole 800h-byte sectors")
    ASSERT(__image_end - __load_end < EXE_HEAD_BYTES, "the stored image may reach past the data by under one sector")
    ASSERT(__load_end <= __heap_end, "the stored image reaches into the stack reserve")

    /* The scene core's stored image (psx-core with feature `link`, input section `.psx_core`)
       belongs to psoxide-core.ld's `.psx_core_store`, which takes it first when that script is
       linked. Linked without it, the image would be placed as an orphan without a word and the
       program would run with no core in its window; it lands here instead, and the assertion
       below fails the link. */
    .psx_core_unplaced : { KEEP(*(.psx_core)) } > RELOCATED
    ASSERT(SIZEOF(.psx_core_unplaced) == 0, "psx-core's image (feature link) is linked without psoxide-core.ld: link -T <ram>.ld -T psoxide-core.ld [-T psoxide-boot.ld] -T psoxide.ld")

    /* The symbol tables the link writes (non-allocated: in the ELF beside an EXE, for the
       tools), placed by name so the link can refuse every unplaced section
       (`--orphan-handling=error`). */
    .symtab 0 : { *(.symtab) }
    .strtab 0 : { *(.strtab) }
    .shstrtab 0 : { *(.shstrtab) }

    /* Discard MIPS ABI glue not needed on bare metal, and the MIPS debug records (procedure
       descriptors and the ABI marker) nothing reads. */
    /DISCARD/ : {
        *(.region); /* Discard legacy vendor markers from older object files. */
        *(.MIPS.abiflags)
        *(.reginfo)
        *(.pdr)
        *(.mdebug.*)
        *(.eh_frame_hdr)
        *(.eh_frame)
        *(.comment)
        *(.note.*)
    }
}
