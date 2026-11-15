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
/// `buf` must be at least [`U32_DEC_MAX`] bytes for the general case.
/// 32-bit only: the constant `/ 10` becomes a `multu` by its reciprocal and
/// the digit comes from `v - q * 10`, so no divide and no 64-bit helper runs.
pub fn u32_dec(buf: &mut [u8], mut v: u32) -> &str {
    let mut tmp = [0u8; U32_DEC_MAX];
    let mut i = tmp.len();
    loop {
        let q = v / 10;
        i -= 1;
        tmp[i] = b'0' + (v - q * 10) as u8;
        v = q;
        if v == 0 {
            break;
        }
    }
    let n = tmp.len() - i;
    buf[..n].copy_from_slice(&tmp[i..]);
    ascii(&buf[..n])
}

/// Format `v` as decimal ASCII into `buf`, returning the digits as `&str`.
///
/// `buf` must be at least [`I32_DEC_MAX`] bytes for the general case.
/// Handles `i32::MIN` (no negate overflow: magnitude goes through u32).
pub fn i32_dec(buf: &mut [u8], v: i32) -> &str {
    if v >= 0 {
        return u32_dec(buf, v as u32);
    }
    // Sign first, magnitude right after it (u32_dec writes from the start
    // of the slice it is given).
    buf[0] = b'-';
    let n = 1 + u32_dec(&mut buf[1..], v.unsigned_abs()).len();
    ascii(&buf[..n])
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
/// counter such as `007` is `u32_dec_padded(buf, 7, 3)`.
pub fn u32_dec_padded(buf: &mut [u8], v: u32, width: usize) -> &str {
    let digits = u32_dec(&mut buf[..], v).len();
    if digits >= width {
        return ascii(&buf[..digits]);
    }
    let pad = width - digits;
    buf.copy_within(..digits, pad);
    buf[..pad].fill(b'0');
    ascii(&buf[..width])
}

/// Format a count of seconds as a clock, `M:SS`: minutes in as many digits as
/// they take, seconds always two.
///
/// `buf` must be at least [`CLOCK_DEC_MAX`] bytes.
pub fn clock_dec(buf: &mut [u8], secs: u32) -> &str {
    let minutes = u32_dec(&mut buf[..], secs / 60).len();
    let secs = secs % 60;
    buf[minutes] = b':';
    buf[minutes + 1] = b'0' + (secs / 10) as u8;
    buf[minutes + 2] = b'0' + (secs % 10) as u8;
    ascii(&buf[..minutes + 3])
}

/// Format `tenths` (a value in tenths of a unit) as `W.T`: `1234` is `123.4`.
///
/// `buf` must be at least [`TENTHS_DEC_MAX`] bytes.
pub fn tenths_dec(buf: &mut [u8], tenths: u32) -> &str {
    let whole = u32_dec(&mut buf[..], tenths / 10).len();
    buf[whole] = b'.';
    buf[whole + 1] = b'0' + (tenths % 10) as u8;
    ascii(&buf[..whole + 2])
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
