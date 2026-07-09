// SPDX-License-Identifier: GPL-2.0-or-later
//! Integer-to-decimal ASCII without `core::fmt`.
//!
//! `core::fmt` drags kilobytes of formatting machinery into a guest binary,
//! so every project that draws a score, a coordinate, or a cell value with
//! `psx-font` grew its own digit loop (gh-psx `u32_dec`, PSXcel `write_dec` /
//! `write_u64`, hl-psx `fmt_i32`, VoXide `Decimal3`, the magikarp example).
//! This module is that loop, once.
//!
//! Each function renders right-to-left into a scratch tail of `buf`, then
//! returns the `&str` covering just the digits. Size `buf` with the matching
//! `*_DEC_MAX` constant and it can never fail; an undersized buffer panics
//! (slice bounds), which the guest panic handler reports with file:line.

/// Maximum bytes `u32_dec` can produce (`u32::MAX` = 10 digits).
pub const U32_DEC_MAX: usize = 10;
/// Maximum bytes `i32_dec` can produce (sign + 10 digits).
pub const I32_DEC_MAX: usize = 11;
/// Maximum bytes `u64_dec` can produce (`u64::MAX` = 20 digits).
pub const U64_DEC_MAX: usize = 20;

/// Format `v` as decimal ASCII into `buf`, returning the digits as `&str`.
///
/// `buf` must be at least [`U32_DEC_MAX`] bytes for the general case.
pub fn u32_dec(buf: &mut [u8], v: u32) -> &str {
    u64_dec(buf, v as u64)
}

/// Format `v` as decimal ASCII into `buf`, returning the digits as `&str`.
///
/// `buf` must be at least [`I32_DEC_MAX`] bytes for the general case.
/// Handles `i32::MIN` (no negate overflow: magnitude goes through u32).
pub fn i32_dec(buf: &mut [u8], v: i32) -> &str {
    if v >= 0 {
        return u64_dec(buf, v as u64);
    }
    // Sign first, magnitude right after it (u64_dec writes from the start
    // of the slice it is given).
    buf[0] = b'-';
    let n = 1 + u64_dec(&mut buf[1..], v.unsigned_abs() as u64).len();
    core::str::from_utf8(&buf[..n]).unwrap_or("?")
}

/// Format `v` as decimal ASCII into `buf`, returning the digits as `&str`.
///
/// `buf` must be at least [`U64_DEC_MAX`] bytes for the general case.
/// (64-bit math is slow on the PS1; fine for text, keep it out of hot loops.)
pub fn u64_dec(buf: &mut [u8], mut v: u64) -> &str {
    let mut tmp = [0u8; U64_DEC_MAX];
    let mut i = tmp.len();
    loop {
        i -= 1;
        tmp[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    let n = tmp.len() - i;
    buf[..n].copy_from_slice(&tmp[i..]);
    core::str::from_utf8(&buf[..n]).unwrap_or("?")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u32_basics() {
        let mut b = [0u8; U32_DEC_MAX];
        assert_eq!(u32_dec(&mut b, 0), "0");
        assert_eq!(u32_dec(&mut b, 7), "7");
        assert_eq!(u32_dec(&mut b, 240), "240");
        assert_eq!(u32_dec(&mut b, u32::MAX), "4294967295");
    }

    #[test]
    fn i32_signs_and_extremes() {
        let mut b = [0u8; I32_DEC_MAX];
        assert_eq!(i32_dec(&mut b, 0), "0");
        assert_eq!(i32_dec(&mut b, -1), "-1");
        assert_eq!(i32_dec(&mut b, 12345), "12345");
        assert_eq!(i32_dec(&mut b, i32::MIN), "-2147483648");
        assert_eq!(i32_dec(&mut b, i32::MAX), "2147483647");
    }

    #[test]
    fn u64_extremes() {
        let mut b = [0u8; U64_DEC_MAX];
        assert_eq!(u64_dec(&mut b, u64::MAX), "18446744073709551615");
        assert_eq!(u64_dec(&mut b, 10_000), "10000");
    }
}
