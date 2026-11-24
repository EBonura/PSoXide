# Service images and what each scene needs

A program on the scene core keeps cold or scene-scoped code (a title menu, a level's script engine, an
SDK service only some scenes use) out of its resident image as **overlays**, and says in its scene
table which of them each scene needs. The core loads them: a scene switch inside the program loads
the scene's needed images before the scene runs, and the program asks once at its start for what its
start scene needs. Nothing is decided at run time: the table, written when the disc is built, is the
one declaration. The example is `sdk/examples/hello-modular`, built by its `build.sh`.

## 1. Put the code in an overlay

Link with psoxide-overlay.ld (see its header) and name the code and its read-only data:

```rust
#[inline(never)]
#[link_section = ".text.overlay.1.menu"]
fn menu(x: u32) -> u32 { /* ... */ }

#[link_section = ".rodata.overlay.1.strings"]
static STRINGS: [u8; 64] = [/* ... */];
```

Up to four overlays share `OVERLAY_VMA` (or the program's `.bss.overlay_host` buffer). Hot per-frame
code stays resident: an overlay is for code a scene enters, not code every frame calls.
`psx-exe --overlay .overlay.1 game.elf menu.ovl` writes the image and prints its `load=` option; run
`hazard-scan --raw` and `check-mfc0 --raw` on it (the EXE's hazard patcher never sees it).

## 2. Declare it in the disc manifest

```text
program PSX.EXE   -         reserve=0x80100000-0x80140000 carries=CARRIED.OVL
overlay SVC.OVL   svc.ovl   owner=PSX.EXE load=0x80100000 class=swap
overlay VOL.OVL   vol.ovl   owner=PSX.EXE load=0x80100000 class=volatile
scene   START     PSX.EXE   start needs=SVC.OVL
scene   PLAY      PSX.EXE   entry=0x80012a44 needs=VOL.OVL
```

`needs=` lists the images the scene needs in RAM when it runs: overlays of the scene's own program
that overlap neither each other nor the scene's own overlay (mkisopsx refuses anything else). The
other options (`class=volatile` for memory the program reuses, `carries=` for bytes the exe already
holds, `place=`, `tail=`, `host`) are mkisopsx's (tools/mkisopsx/src/scenes.rs).

## 3. Ask for the start scene's images

```rust
static mut TABLE: [u32; 512] = [0; 512];
// ... at the start of main, before the first call into a needed image:
psx_core::link::require_or_fatal(table); // or `require`, which returns the code
```

`require` finds the program's entry scene (its table's boot scene after a BIOS boot or a launcher's
switch, else the scene the switch entered) and loads each needed image not in RAM. In-program scenes
need no call: `psx_core::link::switch` to one loads what it needs before it runs.

## 4. Keep in-program scene entries linked

Nothing calls an in-program scene's entry but the core, so the linker drops it unless something keeps
it: a `#[used]` static holding the entry functions (hello-modular's `ENTRIES`).

## Rules

- A switch inside a program only adds images; an image leaves RAM only when a later load's range
  covers it (its evicts word) or at a program switch.
- The idle hook runs while these loads wait on the drive (so a program keeps its pad and music
  going), never while `psx_core_cd` sends a short command.
- Every program on a disc is on one core (CORE_HASH); every disc is pressed with the SDK that built
  its programs (table version 3).
