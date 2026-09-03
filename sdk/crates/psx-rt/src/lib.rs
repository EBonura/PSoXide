// SPDX-License-Identifier: GPL-2.0-or-later
//! PS1 bare-metal runtime.
//!
//! Provides the `_start` entry point that the PSX-EXE loader jumps to,
//! a panic handler, direct i-cache control, a BIOS TTY debug
//! trampoline, and optional heap initialisation. Homebrew crates
//! depend on this crate to get a working `main()` environment without
//! thinking about linker symbols or cache-flushing.
//!
//! # Entry sequence
//!
//! 1. The loader (our emulator or the real BIOS) copies the EXE
//!    payload to `LOAD_ADDR` and jumps to [`_start`].
//! 2. [`_start`] zeroes the `.bss` section using the linker-defined
//!    `__bss_start` / `__bss_end` symbols.
//! 3. With the `alloc` feature, the bump allocator is seeded from
//!    `__heap_start..__heap_end`.
//! 4. `main()` is called. When it returns, we halt.

#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]
// Required for MIPS `global_asm!` blocks (BIOS trampolines). Suppress
// the "feature is declared but not used" warning that shows on the
// host target where only the panic handler + heap are live.
#![cfg_attr(target_arch = "mips", feature(asm_experimental_arch))]

// Corrected signed 64-bit divide/mod (compiler-builtins' are broken here).
#[cfg(target_arch = "mips")]
mod builtins;
// Hand-scheduled memcpy/memset/memcmp (compiler-builtins' are generic loops).
#[cfg(target_arch = "mips")]
mod mem;

#[cfg(target_arch = "mips")]
pub mod bios;
pub mod cache;
#[cfg(target_arch = "mips")]
pub mod interrupts;
#[cfg(target_arch = "mips")]
pub mod tty;

#[cfg(feature = "alloc")]
pub mod heap;

/// Post-link home for R3000 load-delay hazard trampolines.
///
/// LLVM's MIPS delay-slot filler leaves loads in branch delay slots whose
/// consumer runs one instruction later, inside the load delay, and so reads
/// the stale register. `tools/hazard_patch.py` rewrites each such branch to
/// jump through a short trampoline it writes here after the link (magic,
/// capacity, then code words), which the staged guest build runs and verifies
/// instead of disabling the filler's backward search at a cost of tens of
/// kilobytes of nops. Every guest that links psx-rt therefore carries this
/// 392-byte array in `.data`; nothing reads it at runtime except the CPU.
#[no_mangle]
#[used]
pub static mut HAZARD_TRAMPOLINES: [u32; 2 + 96] = {
    let mut words = [0u32; 2 + 96];
    words[0] = 0x4841_5a54;
    words[1] = 96;
    words
};

// Symbols emitted by `psoxide.ld`.
extern "C" {
    static mut __bss_start: u8;
    static mut __bss_end: u8;
    #[cfg(feature = "alloc")]
    static __heap_start: u8;
    #[cfg(feature = "alloc")]
    static __heap_end: u8;
}

#[cfg(target_arch = "mips")]
extern "Rust" {
    fn main();
}

#[cfg(target_arch = "mips")]
#[used]
#[link_section = ".region"]
static PSX_EXE_REGION: [u8; 55] = *b"PSoXide SDK homebrew executable                        ";

/// Entry point the PSX-EXE loader jumps to.
///
/// The arguments are the MIPS ABI's first three registers as the loader left
/// them. A BIOS boot leaves garbage there, which is why
/// [`psx_io::disc_base::install`] gates on a magic value rather than trusting
/// them; a multi-program loader passes
/// (`HANDOFF_MAGIC`, disc LBA offset, CD-DA track base) so the SDK can find
/// this program's data on a disc it does not own.
///
/// # Safety
/// Called exactly once at boot by the loader. The caller must
/// have set up a valid stack pointer before branching here -- the
/// PSX-EXE header's `initial_sp_base` + `initial_sp_offset` fields
/// guarantee this.
#[cfg(target_arch = "mips")]
#[no_mangle]
#[link_section = ".text._start"]
pub unsafe extern "C" fn _start(a0: u32, a1: u32, a2: u32) -> ! {
    #[cfg(feature = "boot-trace")]
    tty::println("psx-rt: start");

    // Zero BSS.
    let bss_start = &raw mut __bss_start as *mut u8;
    let bss_end = &raw const __bss_end as *const u8;
    let bss_len = bss_end as usize - bss_start as usize;
    if bss_len > 0 {
        unsafe { core::ptr::write_bytes(bss_start, 0, bss_len) };
    }
    #[cfg(feature = "boot-trace")]
    tty::println("psx-rt: bss ok");

    // After the zeroing, not before: the bases live in statics.
    // SAFETY: once, at boot, before anything can observe them.
    unsafe { psx_io::disc_base::install(a0, a1, a2) };

    #[cfg(feature = "alloc")]
    {
        let heap_start = &raw const __heap_start as *const u8 as usize;
        let heap_end = &raw const __heap_end as *const u8 as usize;
        unsafe { heap::init(heap_start, heap_end - heap_start) };
    }

    #[cfg(feature = "boot-trace")]
    tty::println("psx-rt: main");
    unsafe { main() };
    #[cfg(feature = "boot-trace")]
    tty::println("psx-rt: main returned");
    halt();
}

/// Current MIPS stack pointer (`$sp`). No memory or stack side effects.
#[cfg(target_arch = "mips")]
#[inline(always)]
fn stack_pointer() -> usize {
    let sp: usize;
    // SAFETY: a single register move out of `$sp`.
    unsafe {
        core::arch::asm!("move {}, $sp", out(reg) sp, options(nomem, nostack, preserves_flags));
    }
    sp
}

/// Trap (loudly, via TTY) if the live stack pointer has descended into
/// the static data region.
///
/// The linker lays out `.text/.data/.bss` from the load address upward
/// and runs the stack downward from the top of RAM. The two are only
/// safe while `$sp` stays above the top of `.bss`. A large
/// stack-resident object (e.g. a multi-hundred-KB struct held in a
/// `main` local) or very deep call frames push `$sp` *below* `__bss_end`;
/// from there a stack write and a write to some `static` alias the same
/// RAM and silently corrupt each other -- the corruption surfaces far
/// from the cause and is brutal to debug. Call this once the deepest
/// expected stack frame is live (e.g. right after constructing the main
/// scene object) to turn that into an immediate, located halt instead.
///
/// Note: this checks against the *actual* top of static data, not the
/// linker's `__heap_end` stack floor, which currently reserves only a
/// nominal slice and would false-positive on any real stack.
#[cfg(target_arch = "mips")]
pub fn assert_stack_headroom() {
    /// Bytes of slack required between `$sp` and the top of `.bss`.
    const GUARD: usize = 0x800;
    let data_top = &raw const __bss_end as *const u8 as usize;
    let sp = stack_pointer();
    if sp <= data_top.saturating_add(GUARD) {
        tty::print("\nSTACK/DATA COLLISION: $sp=0x");
        tty::print_hex_u32(sp as u32);
        tty::print(" is inside static data (top=0x");
        tty::print_hex_u32(data_top as u32);
        tty::print(").\n  A large stack object or deep call frames overran the stack and\n  now alias `static` memory -- this silently corrupts both. Move big\n  objects into .bss, shrink static buffers, or enlarge the stack.\n");
        halt();
    }
}

/// Infinite loop with no useful side effects. Used after `main()`
/// returns or from panic / reset paths.
#[inline(never)]
pub fn halt() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

/// Panic handler. Tries to write the message to TTY so PCSX-Redux
/// (and our emulator's future console hook) shows it, then halts.
///
/// Only registered when targeting PS1 hardware. Host builds of the
/// SDK use `std`'s panic handler -- this avoids the lang-item conflict
/// that would otherwise fire when something on host transitively
/// depends on both `psx-rt` and `std`.
#[cfg(target_arch = "mips")]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    tty::print("PANIC: ");
    if let Some(msg) = info.message().as_str() {
        tty::print(msg);
    }
    tty::print("\n");
    if let Some(loc) = info.location() {
        tty::print("  at ");
        tty::print(loc.file());
        tty::print(":");
        // Print the line number (no fmt machinery in no_std): up to 8 digits.
        let mut line = loc.line();
        let mut buf = [0u8; 8];
        let mut i = buf.len();
        if line == 0 {
            i -= 1;
            buf[i] = b'0';
        }
        while line > 0 && i > 0 {
            i -= 1;
            buf[i] = b'0' + (line % 10) as u8;
            line /= 10;
        }
        if let Ok(s) = core::str::from_utf8(&buf[i..]) {
            tty::print(s);
        }
        tty::print("\n");
    }
    halt()
}
