//! The runtime's one remaining BIOS entry point: TTY `putchar`.
//!
//! Normal operation talks straight to the hardware; the i-cache flush
//! that used to go through `A(44h) FlushCache` is now
//! [`crate::cache::flush_i_cache`]. The single service still routed
//! through the BIOS dispatch convention is `A(3Ch) putchar`, kept
//! because it is the canonical PS1 debug channel: the emulator's HLE
//! prints it to the host terminal, and on a real console the BIOS TTY
//! device swallows it harmlessly. It is reached only from panic
//! reporting and debug prints ([`crate::tty`]), never from a frame
//! loop.
//!
//! The trampoline is a 3-instruction stub against the publicly
//! documented A-table dispatch convention: load the table address into
//! `$t0`, jump to it, put the function index into `$t1` in the branch
//! delay slot.

core::arch::global_asm!(
    ".set noreorder",
    ".section .text.bios.putchar",
    ".globl __bios_putchar",
    "__bios_putchar:",
    "  la $8, 0xA0",
    "  jr $8",
    "  li $9, 0x3C",
    ".set reorder",
);

extern "C" {
    fn __bios_putchar(ch: u32);
}

/// Write one byte to TTY via BIOS `putchar`.
#[inline(always)]
pub fn putchar(ch: u8) {
    unsafe { __bios_putchar(ch as u32) }
}
