//! TTY output for debug printing.
//!
//! Off unless the `host-tty` feature is on: without it [`print`], [`println`] and
//! [`print_hex_u32`] compile to nothing, so a shipping program neither spends
//! cycles on them nor touches the debug port, and a call in a frame loop costs
//! no more than an empty one. Arguments are still evaluated, so keep any
//! formatting work behind the call's own condition.
//!
//! With `host-tty` each byte is one store to the Expansion 2 debug console
//! ([`psx_hw::memory::expansion2::DEBUG_CONSOLE`]). That is where PCSX-Redux,
//! OpenBIOS's `putchar` and our emulator print to the host terminal, and a
//! retail console ignores the store. It never calls into the BIOS and reads no
//! kernel state, so a diagnostic build behaves the same under every BIOS.

/// Write a string to TTY.
///
/// Out of line so a literal is not unrolled into one store per byte at every
/// call site.
#[cfg(feature = "host-tty")]
#[inline(never)]
pub fn print(s: &str) {
    for b in s.bytes() {
        write_byte(b);
    }
}

/// Write a string to TTY. A no-op without the `host-tty` feature.
#[cfg(not(feature = "host-tty"))]
#[inline(always)]
pub fn print(_s: &str) {}

/// [`print`] + a trailing `\n`.
#[cfg(feature = "host-tty")]
#[inline(never)]
pub fn println(s: &str) {
    print(s);
    write_byte(b'\n');
}

/// [`print`] + a trailing `\n`. A no-op without the `host-tty` feature.
#[cfg(not(feature = "host-tty"))]
#[inline(always)]
pub fn println(_s: &str) {}

/// Write a single u32 as 8 hex digits, no prefix, uppercase.
#[cfg(feature = "host-tty")]
pub fn print_hex_u32(v: u32) {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    for shift in (0..8).rev() {
        let nib = ((v >> (shift * 4)) & 0xf) as usize;
        write_byte(DIGITS[nib]);
    }
}

/// Write a single u32 as 8 hex digits. A no-op without the `host-tty` feature.
#[cfg(not(feature = "host-tty"))]
#[inline(always)]
pub fn print_hex_u32(_v: u32) {}

/// Whether this build prints at all (the `host-tty` feature).
pub const ENABLED: bool = cfg!(feature = "host-tty");

/// One byte to the debug console.
#[cfg(feature = "host-tty")]
#[inline(always)]
fn write_byte(byte: u8) {
    use psx_hw::memory::{expansion2, to_kseg1};
    // SAFETY: the address is the debug console's one-byte register in the
    // expansion window: a store there has no effect on a retail console and
    // prints on the hosts that implement it. It is not RAM, so no Rust object
    // aliases it.
    unsafe {
        core::ptr::write_volatile(to_kseg1(expansion2::DEBUG_CONSOLE) as *mut u8, byte);
    }
}
