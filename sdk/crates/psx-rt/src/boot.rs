//! `__boot_entry`: the mover for boot-only memory (sdk/psoxide-boot.ld).
//!
//! A program that links psoxide-boot.ld stores its `.boot` section right after `.data` (and the
//! scene core's stored image), where `.bss` will be, and runs it from memory it reuses later (by
//! default the start of the heap). The script makes this the program's entry, or the scene
//! core's `__core_boot` goes on to it. It runs before `_start`:
//!
//! 1. it keeps `$a0..$a2` and moves `$sp` to `STACK_INIT` (a BIOS hands over its own stack);
//! 2. it copies `.boot` from its load address up to its run address (at or above it; the two
//!    ranges may overlap, so the copy runs from the top down) and zeroes `.boot_bss` after it;
//! 3. it jumps to `_start` with `$a0..$a2` as it found them, and `_start` clears `.bss`, which
//!    holds the stored copy, and flushes the whole instruction cache before `main` (a chain
//!    loader, the core or a previous program may have run code at the run addresses), so the
//!    mover flushes nothing itself.
//!
//! A program that does not link psoxide-boot.ld never references `__boot_entry`, and the link
//! drops it.

#[cfg(target_arch = "mips")]
extern "C" {
    static __boot_start: u8;
    static __boot_load_end: u8;
    static __boot_end: u8;
    static __boot_lma: u8;
}

/// The Rust half of `__boot_entry`. No statics: `.bss` is not cleared yet, and it holds the
/// stored copy this reads.
#[cfg(target_arch = "mips")]
#[no_mangle]
#[link_section = ".text_entry.__boot_entry_rs"]
extern "C" fn __boot_entry_rs() {
    let run = &raw const __boot_start as *mut u8;
    let load_end = &raw const __boot_load_end as *mut u8;
    let end = &raw const __boot_end as *mut u8;
    let stored = &raw const __boot_lma;
    let words = (load_end as usize - run as usize) / 4;
    let (from, to) = (stored as *const u32, run as *mut u32);
    // SAFETY: psoxide-boot.ld lays these out, word aligned (asserted there): `words` words stored
    // at `__boot_lma` (inside the image the loader wrote) and the same number at `__boot_start`,
    // inside memory its assertions prove is the program's and holds nothing live yet;
    // `.boot_bss` follows the run copy. No code runs from either range yet. The run copy is at or
    // above the stored one (asserted there too) and the two may overlap, so the copy runs from
    // the top down, a word at a time: `ptr::copy` would not do, because LLVM takes two distinct
    // symbols for two objects and makes it a `memcpy`.
    unsafe {
        for i in (0..words).rev() {
            core::ptr::write_volatile(to.add(i), core::ptr::read_volatile(from.add(i)));
        }
        core::ptr::write_bytes(load_end, 0, end as usize - load_end as usize);
    }
    // No I-cache flush here: `_start` runs next, and its `entry::set_entry_state` flushes the
    // whole instruction cache (once the DMA channels are idle) before `main`, the only way into
    // boot code.
}

#[cfg(target_arch = "mips")]
core::arch::global_asm!(
    ".section .text_entry.__boot_entry,\"ax\",@progbits",
    ".globl __boot_entry",
    ".type __boot_entry,@function",
    ".set noreorder",
    "__boot_entry:",
    "move  $s0, $a0",
    "move  $s1, $a1",
    "move  $s2, $a2",
    // $sp written once (the delay slot), never the half-built %hi value (0x80200000).
    "lui   $t0, %hi(STACK_INIT)",
    "addiu $t0, $t0, %lo(STACK_INIT)",
    "jal   __boot_entry_rs",
    "move  $sp, $t0",
    "move  $a0, $s0",
    "move  $a1, $s1",
    "j     _start",
    "move  $a2, $s2",
    ".set reorder",
    ".size __boot_entry, . - __boot_entry",
);
