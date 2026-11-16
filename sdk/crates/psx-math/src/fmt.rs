// SPDX-License-Identifier: GPL-2.0-or-later
//! Integer-to-decimal ASCII without `core::fmt`.
//!
//! `core::fmt` drags kilobytes of formatting machinery into a guest binary,
//! so every project that draws a score, a coordinate, or a cell value with
//! `psx-font` grew its own digit loop (gh-psx `u32_dec`, PSXcel `write_dec` /
//! `write_u64`, hl-psx `fmt_i32`, VoXide `Decimal3`, the magikarp example).
//! This module is that loop, once.
//!
//! Each function renders right-to-left at the end of `buf` and returns the
//! `&str` covering just the digits (so use the return value, not `buf[..n]`;
//! `u32_dec_padded` is the one that starts at the front). Size `buf` with the matching
//! `*_DEC_MAX` constant and it can never fail; an undersized buffer panics
//! (slice bounds), which the guest panic handler reports with file:line.
//!
//! ```
//! use psx_math::fmt::{clock_dec, u32_dec, CLOCK_DEC_MAX, U32_DEC_MAX};
//!
//! let mut buf = [0u8; U32_DEC_MAX];
//! // Draw the returned text. The digits are buf's tail: `buf[..2]` is NOT "42".
//! assert_eq!(u32_dec(&mut buf, 42), "42");
//!
//! let mut buf = [0u8; CLOCK_DEC_MAX];
//! assert_eq!(clock_dec(&mut buf, 75), "1:15");
//! ```

/// Maximum bytes `u32_dec` can produce (`u32::MAX` = 10 digits).
pub const U32_DEC_MAX: usize = 10;
/// Maximum bytes `i32_dec` can produce (sign + 10 digits).
pub const I32_DEC_MAX: usize = 11;
/// Maximum bytes `u64_dec` can produce (`u64::MAX` = 20 digits).
pub const U64_DEC_MAX: usize = 20;

/// The formatted bytes as `&str`. Only this module's writers reach it, and
/// they write ASCII digits and `-` alone, so UTF-8 validation is skipped: it
/// was 280 of the 500 cycles a 32-bit `u32_dec` call took on the emulator
/// microbench (the old `u64` route took 757).
#[inline(always)]
fn ascii(bytes: &[u8]) -> &str {
    debug_assert!(bytes.is_ascii());
    // SAFETY: every byte is an ASCII digit or '-', so the slice is UTF-8.
    unsafe { core::str::from_utf8_unchecked(bytes) }
}

/// Format `v` as decimal ASCII into `buf`, returning the digits as `&str`.
///
/// The digits are written right to left at the END of `buf`, so the returned
/// `&str` is `buf`'s tail; use the return value, not `buf[..n]`. `buf` must be
/// at least [`U32_DEC_MAX`] bytes for the general case; a longer one is fine.
/// 32-bit only: the constant `/ 10` becomes a `multu` by its reciprocal and
/// the digit comes from `v - q * 10`, so no divide and no 64-bit helper runs.
///
/// Built for size and kept out of line on the console: the loop LLVM unrolls
/// at full speed is 708 bytes, this is under a sixth of that, and a digit
/// costs the same one multiply either way.
#[cfg_attr(target_arch = "mips", optimize(size), inline(never))]
pub fn u32_dec(buf: &mut [u8], mut v: u32) -> &str {
    let mut i = buf.len();
    loop {
        let q = v / 10;
        i -= 1;
        buf[i] = b'0' + (v - q * 10) as u8;
        v = q;
        if v == 0 {
            break;
        }
    }
    ascii(&buf[i..])
}

/// Maximum bytes `u32_hex` can produce (eight digits).
pub const U32_HEX_MAX: usize = 8;

/// Format the low `digits` hex digits of `v` (0 to 8) as upper-case ASCII at the START of `buf`
/// and return them as `&str`. Fixed width, zero-padded, no `0x` prefix: a register, an address
/// or a checksum as every diagnostic prints it. `buf` must hold `digits` bytes (an undersized
/// one panics, like the decimal writers).
///
/// Built for size and kept out of line on the console, like [`u32_dec`].
///
/// ```
/// use psx_math::fmt::{u32_hex, U32_HEX_MAX};
/// let mut buf = [0u8; U32_HEX_MAX];
/// assert_eq!(u32_hex(&mut buf, 0xBEEF, 8), "0000BEEF");
/// assert_eq!(u32_hex(&mut buf, 0x1234_ABCD, 4), "ABCD");
/// ```
#[cfg_attr(target_arch = "mips", optimize(size), inline(never))]
pub fn u32_hex(buf: &mut [u8], v: u32, digits: usize) -> &str {
    let mut i = digits;
    while i > 0 {
        i -= 1;
        let nibble = ((v >> (i * 4)) & 15) as u8;
        buf[digits - 1 - i] = if nibble < 10 {
            b'0' + nibble
        } else {
            b'A' + nibble - 10
        };
    }
    ascii(&buf[..digits])
}

/// Format `v` as decimal ASCII into `buf`, returning the digits as `&str`.
///
/// Right-aligned like [`u32_dec`]. `buf` must be at least [`I32_DEC_MAX`] bytes
/// for the general case. Handles `i32::MIN` (no negate overflow: magnitude
/// goes through u32).
pub fn i32_dec(buf: &mut [u8], v: i32) -> &str {
    let len = buf.len();
    let digits = u32_dec(&mut buf[..], v.unsigned_abs()).len();
    if v < 0 {
        buf[len - digits - 1] = b'-';
        return ascii(&buf[len - digits - 1..]);
    }
    ascii(&buf[len - digits..])
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
    ascii(&buf[..n])
}

/// Maximum bytes `clock_dec` can produce (`u32::MAX / 60` minutes, `:`, two digits).
pub const CLOCK_DEC_MAX: usize = U32_DEC_MAX + 3;
/// Maximum bytes `tenths_dec` can produce (ten digits, `.`, one digit).
pub const TENTHS_DEC_MAX: usize = U32_DEC_MAX + 2;

/// Format `v` as at least `width` decimal digits, zero-padded on the left.
/// A value with more digits than `width` is written whole.
///
/// `buf` must be at least `max(width, U32_DEC_MAX)` bytes. A fixed-width
/// counter such as `007` is `u32_dec_padded(buf, 7, 3)`: it comes back at the
/// start of `buf`. A value wider than `width` comes back like [`u32_dec`]'s.
#[inline]
pub fn u32_dec_padded(buf: &mut [u8], v: u32, width: usize) -> &str {
    let mut rest = v;
    let mut i = width;
    while i != 0 {
        let q = rest / 10;
        i -= 1;
        buf[i] = b'0' + (rest - q * 10) as u8;
        rest = q;
    }
    if rest != 0 {
        return u32_dec(buf, v);
    }
    ascii(&buf[..width])
}

/// Format a count of seconds as a clock, `M:SS`: minutes in as many digits as
/// they take, seconds always two.
///
/// Right-aligned like [`u32_dec`]: the clock is `buf`'s tail. `buf` must be at
/// least [`CLOCK_DEC_MAX`] bytes (4 when `secs` is under 600).
#[cfg_attr(target_arch = "mips", optimize(size), inline(never))]
pub fn clock_dec(buf: &mut [u8], secs: u32) -> &str {
    let len = buf.len();
    let m = secs / 60;
    let s = secs - m * 60;
    let t = s / 10;
    buf[len - 1] = b'0' + (s - t * 10) as u8;
    buf[len - 2] = b'0' + t as u8;
    buf[len - 3] = b':';
    let minutes = u32_dec(&mut buf[..len - 3], m).len();
    ascii(&buf[len - 3 - minutes..])
}

/// Format `tenths` (a value in tenths of a unit) as `W.T`: `1234` is `123.4`.
///
/// Right-aligned like [`u32_dec`]. `buf` must be at least [`TENTHS_DEC_MAX`]
/// bytes.
#[cfg_attr(target_arch = "mips", optimize(size), inline(never))]
pub fn tenths_dec(buf: &mut [u8], tenths: u32) -> &str {
    let len = buf.len();
    let whole = tenths / 10;
    buf[len - 1] = b'0' + (tenths - whole * 10) as u8;
    buf[len - 2] = b'.';
    let digits = u32_dec(&mut buf[..len - 2], whole).len();
    ascii(&buf[len - 2 - digits..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u32_hex_matches_the_format_macro_for_every_width() {
        extern crate std;
        use std::format;
        let mut b = [0u8; U32_HEX_MAX];
        for v in [
            0u32,
            1,
            9,
            10,
            15,
            16,
            0xbeef,
            0x1234_abcd,
            u32::MAX,
            0x8000_0000,
        ] {
            for digits in 0..=8usize {
                let want = if digits == 0 {
                    std::string::String::new()
                } else {
                    let full = format!("{v:08X}");
                    full[8 - digits..].into()
                };
                assert_eq!(u32_hex(&mut b, v, digits), want, "{v:#x} x{digits}");
            }
        }
    }

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

    /// `u32_dec` against the 64-bit formatter it replaced, at every digit-count
    /// boundary and on random values.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "exhaustive sweep, minutes under Miri; make test runs it natively"
    )]
    fn u32_matches_u64_formatter() {
        let check = |v: u32| {
            let mut a = [0u8; U32_DEC_MAX];
            let mut b = [0u8; U64_DEC_MAX];
            assert_eq!(u32_dec(&mut a, v), u64_dec(&mut b, u64::from(v)), "{v}");
        };
        let mut power = 1u32;
        loop {
            for v in [
                power - 1,
                power,
                power + 1,
                power.wrapping_mul(2) - 1,
                power.wrapping_mul(9),
            ] {
                check(v);
            }
            match power.checked_mul(10) {
                Some(next) => power = next,
                None => break,
            }
        }
        for v in (0..100_000).chain(u32::MAX - 100_000..=u32::MAX) {
            check(v);
        }
        let mut seed = 0x1234_5678u32;
        for _ in 0..1_000_000 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            check(seed);
            check(seed >> (seed % 32));
        }
    }

    /// Every u32, against the 64-bit formatter. Several minutes in a debug
    /// build: `cargo test --release -p psx-math -- --ignored`.
    #[test]
    #[ignore]
    fn u32_matches_u64_formatter_exhaustively() {
        let mut a = [0u8; U32_DEC_MAX];
        let mut b = [0u8; U64_DEC_MAX];
        for v in 0..=u32::MAX {
            assert_eq!(u32_dec(&mut a, v), u64_dec(&mut b, u64::from(v)), "{v}");
        }
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "exhaustive sweep, minutes under Miri; make test runs it natively"
    )]
    fn i32_matches_u64_formatter() {
        let mut seed = 0x9e37_79b9u32;
        for _ in 0..200_000 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let v = seed as i32 >> (seed % 32);
            let mut a = [0u8; I32_DEC_MAX];
            let mut b = [0u8; U64_DEC_MAX];
            let magnitude = u64_dec(&mut b, u64::from(v.unsigned_abs()));
            let sign = if v < 0 { "-" } else { "" };
            assert_eq!(i32_dec(&mut a, v), [sign, magnitude].concat(), "{v}");
        }
    }

    #[test]
    fn u64_extremes() {
        let mut b = [0u8; U64_DEC_MAX];
        assert_eq!(u64_dec(&mut b, u64::MAX), "18446744073709551615");
        assert_eq!(u64_dec(&mut b, 10_000), "10000");
    }

    // The games' copies, kept as the oracle each formatter must equal.

    // VoXide main.rs `decimal3` (clamped to 999, three digits).
    fn voxide_decimal3(v: u16) -> [u8; 3] {
        let n = if v > 999 { 999 } else { v };
        [
            b'0' + ((n / 100) as u8),
            b'0' + (((n / 10) % 10) as u8),
            b'0' + ((n % 10) as u8),
        ]
    }

    // VoXide inv.rs `number` and `tenths`.
    fn voxide_number(v: u16, buf: &mut [u8; 5]) -> &str {
        let mut i = 5;
        let mut v = v;
        loop {
            i -= 1;
            buf[i] = b'0' + (v % 10) as u8;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        core::str::from_utf8(&buf[i..]).unwrap()
    }
    fn voxide_tenths(v: i32, buf: &mut [u8; 8]) -> usize {
        let mut n = [0u8; 5];
        let whole = voxide_number((v / 10) as u16, &mut n);
        let w = whole.len();
        buf[..w].copy_from_slice(whole.as_bytes());
        buf[w] = b'.';
        buf[w + 1] = b'0' + (v % 10) as u8;
        w + 2
    }

    // NitroXide main.rs and pause_menu.rs clock: minutes are one digit because the
    // callers clamp to 599 seconds.
    fn nitro_clock(secs: u32) -> [u8; 4] {
        [
            b'0' + (secs / 60) as u8,
            b':',
            b'0' + ((secs % 60) / 10) as u8,
            b'0' + ((secs % 60) % 10) as u8,
        ]
    }

    // NitroXide victory.rs clock: `u32_dec` minutes, then the two seconds digits.
    fn nitro_victory_clock(v: u32) -> ([u8; 14], usize) {
        let mut out = [0u8; 14];
        let mut dec = [0u8; U32_DEC_MAX];
        let mut n = 0;
        for &c in u32_dec(&mut dec, v / 60).as_bytes() {
            out[n] = c;
            n += 1;
        }
        out[n] = b':';
        out[n + 1] = b'0' + (v % 60 / 10) as u8;
        out[n + 2] = b'0' + (v % 10) as u8;
        (out, n + 3)
    }

    #[test]
    fn padded_equals_voxide_decimal3_over_its_range() {
        let mut b = [0u8; U32_DEC_MAX];
        for v in 0..=u16::MAX {
            assert_eq!(
                u32_dec_padded(&mut b, u32::from(v.min(999)), 3).as_bytes(),
                voxide_decimal3(v)
            );
        }
    }

    #[test]
    fn padded_widths_and_overflowing_values() {
        let mut b = [0u8; 16];
        assert_eq!(u32_dec_padded(&mut b, 0, 1), "0");
        assert_eq!(u32_dec_padded(&mut b, 0, 4), "0000");
        assert_eq!(u32_dec_padded(&mut b, 42, 2), "42");
        assert_eq!(u32_dec_padded(&mut b, 1234, 2), "1234");
        assert_eq!(u32_dec_padded(&mut b, u32::MAX, 12), "004294967295");
    }

    #[test]
    fn tenths_equals_voxide_tenths_over_its_range() {
        let mut b = [0u8; TENTHS_DEC_MAX];
        for v in 0..=65_535 * 10 + 9 {
            let mut old = [0u8; 8];
            let len = voxide_tenths(v, &mut old);
            assert_eq!(tenths_dec(&mut b, v as u32).as_bytes(), &old[..len], "{v}");
        }
        assert_eq!(tenths_dec(&mut b, u32::MAX), "429496729.5");
    }

    #[test]
    fn a_longer_buffer_changes_where_the_text_sits_not_what_it_says() {
        let mut exact = [0u8; CLOCK_DEC_MAX];
        let mut long = [b'x'; 40];
        for secs in (0..5_000).chain([u32::MAX]) {
            assert_eq!(clock_dec(&mut long, secs), clock_dec(&mut exact, secs));
        }
        let mut exact = [0u8; TENTHS_DEC_MAX];
        for v in (0..5_000).chain([u32::MAX]) {
            assert_eq!(tenths_dec(&mut long, v), tenths_dec(&mut exact, v));
        }
        let mut exact = [0u8; I32_DEC_MAX];
        for v in [0, 7, -7, 12345, -12345, i32::MIN, i32::MAX] {
            assert_eq!(i32_dec(&mut long, v), i32_dec(&mut exact, v));
        }
        let mut exact = [0u8; U32_DEC_MAX];
        for v in [0, 9, 10, 99, 100, u32::MAX] {
            assert_eq!(u32_dec(&mut long, v), u32_dec(&mut exact, v));
        }
        assert!(u32_dec(&mut long, 4242).as_ptr() == long[36..].as_ptr());
    }

    #[test]
    fn clock_equals_the_nitroxide_clocks() {
        let mut b = [0u8; CLOCK_DEC_MAX];
        for secs in 0..=599 {
            assert_eq!(clock_dec(&mut b, secs).as_bytes(), nitro_clock(secs));
        }
        for secs in (0..200_000).chain([u32::MAX - 1, u32::MAX]) {
            let (old, len) = nitro_victory_clock(secs);
            assert_eq!(clock_dec(&mut b, secs).as_bytes(), &old[..len], "{secs}");
        }
        assert_eq!(clock_dec(&mut b, 61), "1:01");
    }
}
