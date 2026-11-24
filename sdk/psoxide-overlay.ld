/*
 * Fixed-address overlays of a program on the scene core, for psoxide.ld, opt-in: link with
 * `-T <ram>.ld -T psoxide-core.ld [-T psoxide-boot.ld] -T psoxide-overlay.ld -T psoxide.ld`, where
 * <ram>.ld (or another `-T` file before this one) holds `OVERLAY_VMA = 0x...;` as well as
 * `RAM_SIZE_OVERRIDE`. Link an ELF, as for psoxide-core.ld.
 *
 * Code and read-only data a program needs only some of the time (a pre-play title menu) run at
 * OVERLAY_VMA, memory the program also uses for something else (its world, once play starts).
 * They are linked with the program, so they call its code and read its statics and the program
 * calls into them, but they are not in its EXE: `psx-exe` (and psoxide-pgo, which writes EXEs the
 * same way) leaves every `.overlay.N` section out of it, and
 *
 *     psx-exe --overlay .overlay.N game.elf menu.ovl
 *
 * writes overlay N as its own image file and prints its manifest options (`load=0x...`). The
 * scene disc's manifest then names it (mkisopsx --scenes):
 *
 *     overlay MENU.OVL menu.ovl owner=<the program> load=<as printed> class=volatile
 *
 * and mkisopsx writes its Overlay row (owner, load address, size, word FNV). The owner's own
 * `program` line needs `reserve=<lo>-<hi>`, a RAM range holding every overlay it owns
 * (OVERLAY_VMA to the largest one's sector-padded end), or mkisopsx refuses with `NoReserve`. An
 * overlay its owner reads right before WORLD.PAK's header (a title menu before its art) takes
 * `place=before-world`: its file then ends where the pack starts, so the two reads cost one seek
 * and a short hop instead of two long seeks. With it, `tail=1` makes that one read: the row also
 * loads the pack's header sector right after the overlay's padded end (checked with the overlay;
 * the reserve must hold it), and the program takes the header from there
 * (`psx_core::link::image_tail(image, __overlay_N_bytes, table)`) instead of reading it after the
 * core's stop. The program calls
 * `psx_core::link::load_image` for it before every call into it. `class=volatile` because the
 * program writes over that memory: the core then reads the overlay again at every load, so a
 * program that has used the memory for something else never jumps into stale bytes.
 *
 * Up to four overlays share OVERLAY_VMA, each loaded over the others: overlay N (1 to 4) is the
 * program's `.text.overlay.N.*` code and `.rodata.overlay.N.*` read-only data. Name each function
 * and table with `#[link_section = ".text.overlay.N.<name>"]` (or `.rodata.`) and keep the entry
 * points `#[inline(never)]`, so their code stays in the overlay. What the compiler places on its
 * own (string literals, constants it folds out, generic code it shares) stays resident, as do the
 * overlay's writable statics: an overlay keeps no state of its own across a reload. One overlay
 * never refers to another (NOCROSSREFS: the link fails if it does). Jump tables are among what
 * stays resident: LLVM puts a function's switch tables in a `.rodata` section of its own name,
 * not in the function's `link_section`, so an overlay function's tables sit in resident `.data`
 * (a few words each; they point into the overlay and are read only while it is loaded).
 *
 * OVERLAY_VMA may not lie over the program's own `.bss`: ld.lld refuses any two sections whose
 * run ranges overlap unless both are members of one OVERLAY, so an overlay over a static buffer
 * fails with "section .bss virtual address range overlaps with .overlay.N". Use memory outside
 * .text, .data and .bss (the heap or a pool the program manages by address). A buffer that is a
 * static cannot host an overlay yet (open item; see the scene-core plan).
 *
 * The overlay's code is not in the EXE, so psoxide-pgo's hazard patcher never sees it, and the
 * SDK's delay-slot flags can leave a load-delay hazard in it as anywhere else (Half-Life's menu
 * had two). Detection is the rule: run `hazard-scan --raw <OVERLAY_VMA> <file>` and `check-mfc0
 * --raw <OVERLAY_VMA> <file>` on each overlay file, as sdk/examples/hello-core-overlay/build.sh
 * does, and fail the build on a finding; fix it in the source (the usual cause is a small helper
 * inlined into a branch: keep it `#[inline(never)]`). The patcher does not fix overlays: its
 * trampolines go in the EXE's resident `HAZARD_TRAMPOLINES`, so patching an overlay would mean
 * patching the EXE and all its overlays in one pass (not built).
 */
SECTIONS {
    /* Every overlay runs at OVERLAY_VMA. The load addresses only keep the sections' file bytes
       apart in the ELF (OVERLAY places them one after another): the physical alias of the run
       address, where nothing else is stored. Nothing loads from them. */
    OVERLAY OVERLAY_VMA : NOCROSSREFS AT(OVERLAY_VMA & 0x1FFFFFFF) {
        .overlay.1 { *(.text.overlay.1.* .rodata.overlay.1.*) }
        .overlay.2 { *(.text.overlay.2.* .rodata.overlay.2.*) }
        .overlay.3 { *(.text.overlay.3.* .rodata.overlay.3.*) }
        .overlay.4 { *(.text.overlay.4.* .rodata.overlay.4.*) }
    } > RELOCATED
    /* OVERLAY leaves `.` at OVERLAY_VMA plus the largest overlay. */
    __overlay_end = .;
    /* Each overlay file's size, for psx_core::link::image_tail. */
    __overlay_1_bytes = SIZEOF(.overlay.1);
    __overlay_2_bytes = SIZEOF(.overlay.2);
    __overlay_3_bytes = SIZEOF(.overlay.3);
    __overlay_4_bytes = SIZEOF(.overlay.4);
}
/* As psoxide-boot.ld: INSERT must directly follow SECTIONS's closing brace. */
INSERT AFTER .bss;

ASSERT(OVERLAY_VMA % 4 == 0, "OVERLAY_VMA must be word aligned")
ASSERT(OVERLAY_VMA >= __data_end, "an overlay runs above the program's .text and .data (OVERLAY_VMA)")
/* The core loads whole sectors: the file is padded to one, and the padding lands in RAM too. */
ASSERT(OVERLAY_VMA + ((__overlay_end - OVERLAY_VMA + 0x7FF) & ~0x7FF) <= __heap_end, "an overlay, padded to whole sectors, must end below the stack reserve")
