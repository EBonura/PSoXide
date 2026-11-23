//! `__boot_entry`: the mover for boot-only memory (sdk/psoxide-boot.ld).
//!
//! A program that links psoxide-boot.ld stores its `.boot` section right after `.data` (and the
//! scene core's stored image), where `.bss` will be, and runs it from memory it reuses later (by
//! default the start of the heap). The script makes this the program's entry, or the scene
//! core's `__core_boot` goes on to it. It runs before `_start`:
//!
//! 1. it keeps `$a0..$a2` and moves `$sp` to `STACK_INIT` (a BIOS hands over its own stack);
//! 2. it copies `.boot` from its load address to its run address (the two ranges may overlap:
//!    the copy is a `memmove`), zeroes `.boot_bss` after it, and invalidates the I-cache lines
//!    over the run range once the DMA channels are idle (a chain loader, the core or a previous
//!    program may have run code at those addresses);
//! 3. it jumps to `_start` with `$a0..$a2` as it found them, and `_start` clears `.bss`, which
//!    holds the stored copy.
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
#[link_section = ".text.__boot_entry_rs"]
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
    // `.boot_bss` follows the run copy. No code runs from either range yet. The ranges may
    // overlap, so the copy runs away from the overlap, a word at a time: `ptr::copy` would not
    // do, because LLVM takes two distinct symbols for two objects and makes it a `memcpy`.
    unsafe {
        if (to as usize) < (from as usize) {
            for i in 0..words {
                core::ptr::write_volatile(to.add(i), core::ptr::read_volatile(from.add(i)));
            }
        } else {
            for i in (0..words).rev() {
                core::ptr::write_volatile(to.add(i), core::ptr::read_volatile(from.add(i)));
            }
        }
        core::ptr::write_bytes(load_end, 0, end as usize - load_end as usize);
    }
    crate::cache::flush_instruction_cache_range(run as u32, load_end as u32);
}

#[cfg(target_arch = "mips")]
core::arch::global_asm!(
    ".section .text.__boot_entry,\"ax\",@progbits",
    ".globl __boot_entry",
    ".type __boot_entry,@function",
    ".set noreorder",
    "__boot_entry:",
    "move  $s0, $a0",
    "move  $s1, $a1",
    "move  $s2, $a2",
    "lui   $sp, %hi(STACK_INIT)",
    "addiu $sp, $sp, %lo(STACK_INIT)",
    "jal   __boot_entry_rs",
    "nop",
    "move  $a0, $s0",
    "move  $a1, $s1",
    "j     _start",
    "move  $a2, $s2",
    ".set reorder",
    ".size __boot_entry, . - __boot_entry",
);
