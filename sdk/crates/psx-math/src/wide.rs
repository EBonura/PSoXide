// SPDX-License-Identifier: GPL-2.0-or-later
//! The one sanctioned wide-arithmetic type for guest code.
//!
//! The R3000 has `mult` and `multu`, which write a 64-bit product to the
//! HI:LO pair in one instruction. A 64-bit add or subtract is a few 32-bit
//! instructions with a carry, a compare is two word compares, and a shift is
//! a few word shifts. None of that calls a compiler helper. The expensive
//! half of 64-bit arithmetic is division and remainder (`__divdi3`,
//! `__udivdi3`, `__moddi3`, `__umoddi3`, with `u64_div_rem` behind them),
//! and the 128-bit and soft-float routines; the project's guest symbol gate
//! (`tools/guest_symbol_gate.sh`) fails a link that contains any of them.
//!
//! Game and engine code that has a quantity this wide (a dot product of two
//! world-sized vectors, a squared distance, the cross product of two screen
//! edges, a CORDIC accumulator) holds a [`Wide`] or a [`WideU`] and never
//! names `i64` or `u64`, so a per-project source guard can fail the build on
//! every `i64`/`u64` token outside this file. This file is where the rule is
//! enforced: its 64-bit tokens carry `psx-numeric-allow` markers.
//!
//! # What exists, and what deliberately does not
//!
//! Only operations that compile to native R3000 instructions exist:
//!
//! - full products of two 32-bit words ([`Wide::mul`], [`WideU::mul`]), and
//!   multiply-accumulate forms of them;
//! - 64-bit add, subtract, negate, absolute value, compare;
//! - shifts, with the count taken modulo 64;
//! - the low 64 bits of a 64-by-64 product ([`Wide::mul_w`]) and of a
//!   64-by-32 product ([`Wide::times`]), which LLVM expands to a few
//!   `multu`/`mul` and adds;
//! - extraction to 32 bits in three explicit flavours: truncating
//!   (`low`, `shr_i32`), checked (`to_i32`, `to_u32`) and saturating
//!   (`saturate_i32`, `saturate_u32`);
//! - a square root ([`Wide::isqrt`], [`WideU::isqrt`]), which is
//!   [`crate::int32::isqrt_u64`] and uses `multu` only.
//!
//! There is **no division and no remainder** on a wide value, and no way to
//! ask for one. To divide a product by a 32-bit value use
//! [`crate::int32::mul_div_i32`] / [`crate::int32::mul_div_u32`] (a native
//! `multu` then `divu`), or restructure so that the divide happens on 32
//! bits.
//!
//! Arithmetic wraps like the release-mode `i64` / `u64` it stands in for.
//! Every function is `#[inline(always)]` (or `const`), so a call site
//! compiles to the same instructions as the bare 64-bit expression.
//!
//! The representation is private. Outside tests the only 64-bit tokens in
//! this module are the handful in the private `widen*` / `raw` helpers, each
//! with a marker.

use core::cmp::Ordering;
use core::fmt;
use core::ops::{Add, Neg, Shl, Shr, Sub};

/// Sign-extend a word: one `sra` for the high word.
#[inline(always)]
// psx-numeric-allow-next-line: the sanctioned wide word; sign extension is one sra, no helper
const fn widen(a: i32) -> i64 {
    // psx-numeric-allow-next-line: sign extension, no helper
    a as i64
}

/// Zero-extend a word: free.
#[inline(always)]
// psx-numeric-allow-next-line: the sanctioned wide word; zero extension is free, no helper
const fn widen_unsigned(a: u32) -> u64 {
    // psx-numeric-allow-next-line: zero extension, no helper
    a as u64
}

/// The same 64 bits read as signed: free.
#[inline(always)]
// psx-numeric-allow-next-line: the sanctioned wide word; a reinterpretation, no instructions
const fn signed(a: u64) -> i64 {
    // psx-numeric-allow-next-line: reinterpretation, no instructions
    a as i64
}

/// The same 64 bits read as unsigned: free.
#[inline(always)]
// psx-numeric-allow-next-line: the sanctioned wide word; a reinterpretation, no instructions
const fn unsigned(a: i64) -> u64 {
    // psx-numeric-allow-next-line: reinterpretation, no instructions
    a as u64
}

/// A signed 64-bit value, built from native 32-bit multiplies and adds.
///
/// See the [module documentation](self) for the operations it offers and the
/// ones it leaves out on purpose.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Wide(
    // psx-numeric-allow-next-line: the one signed wide word; every method is a native HI:LO operation
    i64,
);

impl Wide {
    /// Zero.
    pub const ZERO: Wide = Wide(0);
    /// One.
    pub const ONE: Wide = Wide(1);
    /// The largest value, `2^63 - 1`.
    // psx-numeric-allow-next-line: the largest wide value
    pub const MAX: Wide = Wide(i64::MAX);
    /// The smallest value, `-2^63`.
    // psx-numeric-allow-next-line: the smallest wide value
    pub const MIN: Wide = Wide(i64::MIN);

    /// A 32-bit value widened (sign-extending).
    #[inline(always)]
    pub const fn of(a: i32) -> Wide {
        Wide(widen(a))
    }

    /// A 32-bit unsigned value widened (zero-extending).
    #[inline(always)]
    pub const fn of_u(a: u32) -> Wide {
        Wide(signed(widen_unsigned(a)))
    }

    /// The value with signed high word `hi` and low word `lo`, the pair a
    /// `mult` leaves in HI:LO.
    #[inline(always)]
    pub const fn from_parts(hi: i32, lo: u32) -> Wide {
        Wide(widen(hi).wrapping_shl(32) | signed(widen_unsigned(lo)))
    }

    /// The full signed product of two words: one `mult`.
    #[inline(always)]
    pub const fn mul(a: i32, b: i32) -> Wide {
        Wide(widen(a).wrapping_mul(widen(b)))
    }

    /// `self + a * b`: one `mult` and a 64-bit add.
    #[inline(always)]
    pub const fn mul_add(self, a: i32, b: i32) -> Wide {
        Wide(self.0.wrapping_add(widen(a).wrapping_mul(widen(b))))
    }

    /// `self - a * b`: one `mult` and a 64-bit subtract.
    #[inline(always)]
    pub const fn mul_sub(self, a: i32, b: i32) -> Wide {
        Wide(self.0.wrapping_sub(widen(a).wrapping_mul(widen(b))))
    }

    /// `self + o`, usable in `const fn`.
    #[inline(always)]
    pub const fn add_w(self, o: Wide) -> Wide {
        Wide(self.0.wrapping_add(o.0))
    }

    /// `self - o`, usable in `const fn`.
    #[inline(always)]
    pub const fn sub_w(self, o: Wide) -> Wide {
        Wide(self.0.wrapping_sub(o.0))
    }

    /// `self * o`, the low 64 bits of the product (three multiplies).
    #[inline(always)]
    pub const fn mul_w(self, o: Wide) -> Wide {
        Wide(self.0.wrapping_mul(o.0))
    }

    /// `self * k`, the low 64 bits of the product.
    #[inline(always)]
    pub const fn times(self, k: i32) -> Wide {
        Wide(self.0.wrapping_mul(widen(k)))
    }

    /// `self + k` for a 32-bit `k`.
    #[inline(always)]
    pub const fn plus(self, k: i32) -> Wide {
        Wide(self.0.wrapping_add(widen(k)))
    }

    /// `-self`, wrapping at [`Wide::MIN`].
    #[inline(always)]
    pub const fn negate(self) -> Wide {
        Wide(self.0.wrapping_neg())
    }

    /// `|self|`, wrapping at [`Wide::MIN`].
    #[inline(always)]
    pub const fn abs(self) -> Wide {
        Wide(self.0.wrapping_abs())
    }

    /// The dot product of two 3-vectors, three `mult`s and two 64-bit adds.
    /// With `a == b` it is the squared length.
    #[inline(always)]
    pub const fn dot3(a: [i32; 3], b: [i32; 3]) -> Wide {
        Wide(
            widen(a[0])
                .wrapping_mul(widen(b[0]))
                .wrapping_add(widen(a[1]).wrapping_mul(widen(b[1])))
                .wrapping_add(widen(a[2]).wrapping_mul(widen(b[2]))),
        )
    }

    /// Arithmetic shift left; the count is taken modulo 64.
    #[inline(always)]
    pub const fn shl_by(self, n: u32) -> Wide {
        Wide(self.0.wrapping_shl(n))
    }

    /// Arithmetic shift right (the sign is kept); the count is taken modulo 64.
    #[inline(always)]
    pub const fn sar(self, n: u32) -> Wide {
        Wide(self.0.wrapping_shr(n))
    }

    /// Logical shift right of the 64-bit pattern (zeros come in at the top);
    /// the count is taken modulo 64.
    #[inline(always)]
    pub const fn lshr(self, n: u32) -> Wide {
        Wide(signed(unsigned(self.0).wrapping_shr(n)))
    }

    /// The low 32 bits as `i32` (truncating, like `as i32`).
    #[inline(always)]
    pub const fn low(self) -> i32 {
        self.0 as i32
    }

    /// The low 32 bits as `u32` (the LO register).
    #[inline(always)]
    pub const fn low_u32(self) -> u32 {
        self.0 as u32
    }

    /// The high 32 bits as `i32` (the HI register).
    #[inline(always)]
    pub const fn high(self) -> i32 {
        self.0.wrapping_shr(32) as i32
    }

    /// `self >> n` (arithmetic, count modulo 64) truncated to its low 32
    /// bits: the fixed-point "multiply, then drop the fraction" step.
    #[inline(always)]
    pub const fn shr_i32(self, n: u32) -> i32 {
        self.0.wrapping_shr(n) as i32
    }

    /// The value clamped into `i32`.
    #[inline(always)]
    pub const fn saturate_i32(self) -> i32 {
        if self.0 > widen(i32::MAX) {
            i32::MAX
        } else if self.0 < widen(i32::MIN) {
            i32::MIN
        } else {
            self.0 as i32
        }
    }

    /// The value as `i32` if it fits.
    #[inline(always)]
    pub const fn to_i32(self) -> Option<i32> {
        if self.0 >= widen(i32::MIN) && self.0 <= widen(i32::MAX) {
            Some(self.0 as i32)
        } else {
            None
        }
    }

    /// The value clamped into `u32` (a negative value gives 0).
    #[inline(always)]
    pub const fn saturate_u32(self) -> u32 {
        if self.0 < 0 {
            0
        } else if self.0 > signed(widen_unsigned(u32::MAX)) {
            u32::MAX
        } else {
            self.0 as u32
        }
    }

    /// The value as `u32` if it fits.
    #[inline(always)]
    pub const fn to_u32(self) -> Option<u32> {
        if self.0 >= 0 && self.0 <= signed(widen_unsigned(u32::MAX)) {
            Some(self.0 as u32)
        } else {
            None
        }
    }

    /// Is the value below zero?
    #[inline(always)]
    pub const fn is_negative(self) -> bool {
        self.0 < 0
    }

    /// Is the value above zero?
    #[inline(always)]
    pub const fn is_positive(self) -> bool {
        self.0 > 0
    }

    /// Is the value zero?
    #[inline(always)]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// Compare with a 32-bit value.
    #[inline(always)]
    pub const fn cmp_i32(self, k: i32) -> Ordering {
        let k = widen(k);
        if self.0 < k {
            Ordering::Less
        } else if self.0 > k {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    }

    /// `floor(sqrt(self))`, 0 for a negative value ([`crate::int32::isqrt_u64`]).
    #[inline]
    pub const fn isqrt(self) -> u32 {
        if self.0 <= 0 {
            0
        } else {
            crate::int32::isqrt_u64(unsigned(self.0))
        }
    }
}

impl Add for Wide {
    type Output = Wide;
    #[inline(always)]
    fn add(self, o: Wide) -> Wide {
        self.add_w(o)
    }
}

impl Sub for Wide {
    type Output = Wide;
    #[inline(always)]
    fn sub(self, o: Wide) -> Wide {
        self.sub_w(o)
    }
}

impl Neg for Wide {
    type Output = Wide;
    #[inline(always)]
    fn neg(self) -> Wide {
        self.negate()
    }
}

/// Arithmetic shift right; the count is taken modulo 64.
impl Shr<u32> for Wide {
    type Output = Wide;
    #[inline(always)]
    fn shr(self, n: u32) -> Wide {
        self.sar(n)
    }
}

/// Shift left; the count is taken modulo 64.
impl Shl<u32> for Wide {
    type Output = Wide;
    #[inline(always)]
    fn shl(self, n: u32) -> Wide {
        self.shl_by(n)
    }
}

/// Prints the two words, `Wide(HI:LO)` in hex. (Decimal formatting of 64 bits
/// would pull in a software divide.)
impl fmt::Debug for Wide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Wide({:08x}:{:08x})", self.high() as u32, self.low_u32())
    }
}

/// An unsigned 64-bit value: a `u32 * u32` product needs all 64 bits.
///
/// Same rules as [`Wide`]: native operations only, no division.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct WideU(
    // psx-numeric-allow-next-line: the one unsigned wide word; every method is a native HI:LO operation
    u64,
);

impl WideU {
    /// Zero.
    pub const ZERO: WideU = WideU(0);
    /// The largest value, `2^64 - 1`.
    // psx-numeric-allow-next-line: the largest unsigned wide value
    pub const MAX: WideU = WideU(u64::MAX);

    /// A 32-bit value widened.
    #[inline(always)]
    pub const fn of(a: u32) -> WideU {
        WideU(widen_unsigned(a))
    }

    /// The value with high word `hi` and low word `lo`, the pair a `multu`
    /// leaves in HI:LO.
    #[inline(always)]
    pub const fn from_parts(hi: u32, lo: u32) -> WideU {
        WideU((widen_unsigned(hi).wrapping_shl(32)) | widen_unsigned(lo))
    }

    /// The full unsigned product of two words: one `multu`.
    #[inline(always)]
    pub const fn mul(a: u32, b: u32) -> WideU {
        WideU(widen_unsigned(a).wrapping_mul(widen_unsigned(b)))
    }

    /// `self + a * b`: one `multu` and a 64-bit add (wrapping).
    #[inline(always)]
    pub const fn mul_add(self, a: u32, b: u32) -> WideU {
        WideU(
            self.0
                .wrapping_add(widen_unsigned(a).wrapping_mul(widen_unsigned(b))),
        )
    }

    /// `self + o`, wrapping.
    #[inline(always)]
    pub const fn add(self, o: WideU) -> WideU {
        WideU(self.0.wrapping_add(o.0))
    }

    /// `self - o`, wrapping.
    #[inline(always)]
    pub const fn sub(self, o: WideU) -> WideU {
        WideU(self.0.wrapping_sub(o.0))
    }

    /// `self + o`, stopping at [`WideU::MAX`].
    #[inline(always)]
    pub const fn saturating_add(self, o: WideU) -> WideU {
        WideU(self.0.saturating_add(o.0))
    }

    /// `self * k`, the low 64 bits of the product.
    #[inline(always)]
    pub const fn times(self, k: u32) -> WideU {
        WideU(self.0.wrapping_mul(widen_unsigned(k)))
    }

    /// Shift left; the count is taken modulo 64.
    #[inline(always)]
    pub const fn shl_by(self, n: u32) -> WideU {
        WideU(self.0.wrapping_shl(n))
    }

    /// Shift right (zeros come in at the top); the count is taken modulo 64.
    #[inline(always)]
    pub const fn shr_by(self, n: u32) -> WideU {
        WideU(self.0.wrapping_shr(n))
    }

    /// The low 32 bits (the LO register).
    #[inline(always)]
    pub const fn low(self) -> u32 {
        self.0 as u32
    }

    /// The high 32 bits (the HI register).
    #[inline(always)]
    pub const fn high(self) -> u32 {
        self.0.wrapping_shr(32) as u32
    }

    /// `self >> n` (count modulo 64) truncated to its low 32 bits.
    #[inline(always)]
    pub const fn shr_u32(self, n: u32) -> u32 {
        self.0.wrapping_shr(n) as u32
    }

    /// The value clamped into `u32`.
    #[inline(always)]
    pub const fn saturate_u32(self) -> u32 {
        if self.0 > widen_unsigned(u32::MAX) {
            u32::MAX
        } else {
            self.0 as u32
        }
    }

    /// The value as `u32` if it fits.
    #[inline(always)]
    pub const fn to_u32(self) -> Option<u32> {
        if self.0 <= widen_unsigned(u32::MAX) {
            Some(self.0 as u32)
        } else {
            None
        }
    }

    /// Is the value zero?
    #[inline(always)]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// `floor(sqrt(self))` ([`crate::int32::isqrt_u64`]).
    #[inline]
    pub const fn isqrt(self) -> u32 {
        crate::int32::isqrt_u64(self.0)
    }
}

impl Add for WideU {
    type Output = WideU;
    #[inline(always)]
    fn add(self, o: WideU) -> WideU {
        WideU::add(self, o)
    }
}

impl Sub for WideU {
    type Output = WideU;
    #[inline(always)]
    fn sub(self, o: WideU) -> WideU {
        WideU::sub(self, o)
    }
}

/// Shift right (logical); the count is taken modulo 64.
impl Shr<u32> for WideU {
    type Output = WideU;
    #[inline(always)]
    fn shr(self, n: u32) -> WideU {
        self.shr_by(n)
    }
}

/// Shift left; the count is taken modulo 64.
impl Shl<u32> for WideU {
    type Output = WideU;
    #[inline(always)]
    fn shl(self, n: u32) -> WideU {
        self.shl_by(n)
    }
}

/// Prints the two words, `WideU(HI:LO)` in hex.
impl fmt::Debug for WideU {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WideU({:08x}:{:08x})", self.high(), self.low())
    }
}

/// A set of up to 64 flags in two words: what a `u64` bit mask was used for.
/// It does no arithmetic, so it needs no wide integer at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Mask64([u32; 2]);

impl Mask64 {
    /// No bit set.
    pub const EMPTY: Mask64 = Mask64([0; 2]);

    /// Set `bit` (0..64).
    #[inline(always)]
    pub fn set(&mut self, bit: usize) {
        self.0[bit >> 5] |= 1 << (bit & 31);
    }

    /// Clear `bit` (0..64).
    #[inline(always)]
    pub fn clear(&mut self, bit: usize) {
        self.0[bit >> 5] &= !(1 << (bit & 31));
    }

    /// Set or clear `bit` (0..64).
    #[inline(always)]
    pub fn assign(&mut self, bit: usize, value: bool) {
        if value {
            self.set(bit);
        } else {
            self.clear(bit);
        }
    }

    /// Is `bit` set? A bit past 63 is not.
    #[inline(always)]
    pub fn test(&self, bit: usize) -> bool {
        bit < 64 && self.0[bit >> 5] >> (bit & 31) & 1 != 0
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;
    use std::vec::Vec;

    /// xorshift64*, the same stream on every run.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }
        fn i32(&mut self) -> i32 {
            // A mix of magnitudes: full range, small, and Q12-sized values.
            let v = self.next();
            match v & 3 {
                0 => (v >> 8) as i32,
                1 => ((v >> 8) as i32) >> 16,
                2 => ((v >> 8) as i32) >> 8,
                _ => ((v >> 8) as i32) >> 24,
            }
        }
        fn wide(&mut self) -> i128 {
            let v = self.next();
            let w = self.next() as i64 as i128;
            match v & 3 {
                0 => w,
                1 => w >> 20,
                2 => w >> 40,
                _ => (w >> 50) * 4096,
            }
        }
        fn shift(&mut self) -> u32 {
            (self.next() >> 20) as u32 % 96
        }
    }

    const I32_EDGES: [i32; 17] = [
        i32::MIN,
        i32::MIN + 1,
        -65_537,
        -65_536,
        -4097,
        -4096,
        -1,
        0,
        1,
        4095,
        4096,
        65_535,
        65_536,
        65_537,
        i32::MAX - 1,
        i32::MAX,
        0x5555_5555,
    ];
    const U32_EDGES: [u32; 12] = [
        0,
        1,
        2,
        4095,
        4096,
        65_535,
        65_536,
        0x7fff_ffff,
        0x8000_0000,
        0x8000_0001,
        u32::MAX - 1,
        u32::MAX,
    ];
    const I64_MIN: i128 = -(1 << 63);
    const I64_MAX: i128 = (1 << 63) - 1;

    /// Every signed value the oracle can hold, as the public words see it.
    fn val(w: Wide) -> i128 {
        ((w.high() as i128) << 32) | w.low_u32() as i128
    }
    fn valu(w: WideU) -> u128 {
        ((w.high() as u128) << 32) | w.low() as u128
    }
    /// An oracle value wrapped to 64 bits, as a `Wide`.
    fn wrap(v: i128) -> Wide {
        Wide::from_parts((v >> 32) as i32, v as u32)
    }
    fn wrap_i64(v: i128) -> i128 {
        v as i64 as i128
    }
    fn wrap_u64(v: u128) -> u128 {
        v as u64 as u128
    }

    /// Wide values that exercise every carry and sign boundary.
    fn wide_edges() -> Vec<i128> {
        let mut v = vec![
            0,
            1,
            -1,
            2,
            -2,
            0x7fff_ffff,
            0x8000_0000,
            0xffff_ffff,
            0x1_0000_0000,
            0x1_0000_0001,
            -0x7fff_ffff,
            -0x8000_0000,
            -0x8000_0001,
            -0xffff_ffff,
            -0x1_0000_0000,
            -0x1_0000_0001,
            I64_MAX,
            I64_MAX - 1,
            I64_MIN,
            I64_MIN + 1,
            1 << 40,
            -(1 << 40),
            0x1234_5678_9abc_def0,
            -0x1234_5678_9abc_def0,
        ];
        for a in I32_EDGES {
            for b in I32_EDGES {
                v.push(a as i128 * b as i128);
            }
        }
        v
    }

    fn isqrt_oracle(v: u128) -> u32 {
        let (mut lo, mut hi) = (0u128, 1u128 << 32);
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if mid * mid <= v {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo as u32
    }

    #[test]
    fn construction_and_extraction_round_trip() {
        for v in wide_edges() {
            let w = wrap(v);
            assert_eq!(val(w), wrap_i64(v), "{v}");
            assert_eq!(w.low(), v as i32);
            assert_eq!(w.low_u32(), v as u32);
            assert_eq!(w.high(), (wrap_i64(v) >> 32) as i32);
            assert_eq!(Wide::from_parts(w.high(), w.low_u32()), w);
        }
        for a in I32_EDGES {
            assert_eq!(val(Wide::of(a)), a as i128);
        }
        for a in U32_EDGES {
            assert_eq!(val(Wide::of_u(a)), a as i128);
            assert_eq!(valu(WideU::of(a)), a as u128);
        }
        assert_eq!(val(Wide::ZERO), 0);
        assert_eq!(val(Wide::ONE), 1);
        assert_eq!(val(Wide::MAX), I64_MAX);
        assert_eq!(val(Wide::MIN), I64_MIN);
        assert_eq!(Wide::default(), Wide::ZERO);
        assert_eq!(valu(WideU::ZERO), 0);
        assert_eq!(valu(WideU::MAX), u64::MAX as u128);
    }

    #[test]
    fn signed_products_match_the_oracle() {
        for a in I32_EDGES {
            for b in I32_EDGES {
                let p = a as i128 * b as i128;
                assert_eq!(val(Wide::mul(a, b)), p, "{a} * {b}");
                for acc in [0i128, 1, -1, I64_MAX, I64_MIN, 0x1_0000_0000, -0x8000_0000] {
                    let w = wrap(acc);
                    assert_eq!(val(w.mul_add(a, b)), wrap_i64(acc + p), "{acc} + {a} * {b}");
                    assert_eq!(val(w.mul_sub(a, b)), wrap_i64(acc - p), "{acc} - {a} * {b}");
                }
            }
        }
    }

    #[test]
    fn unsigned_products_match_the_oracle() {
        for a in U32_EDGES {
            for b in U32_EDGES {
                let p = a as u128 * b as u128;
                assert_eq!(valu(WideU::mul(a, b)), p, "{a} * {b}");
                for acc in [
                    0u128,
                    1,
                    u64::MAX as u128,
                    0x8000_0000_0000_0000,
                    0x1_0000_0000,
                ] {
                    let w = WideU::from_parts((acc >> 32) as u32, acc as u32);
                    assert_eq!(
                        valu(w.mul_add(a, b)),
                        wrap_u64(acc + p),
                        "{acc} + {a} * {b}"
                    );
                }
            }
        }
    }

    #[test]
    fn add_sub_neg_abs_match_the_oracle() {
        let edges = wide_edges();
        for &a in &edges {
            let (wa, a64) = (wrap(a), wrap_i64(a));
            assert_eq!(val(-wa), wrap_i64(-a64));
            assert_eq!(val(wa.negate()), wrap_i64(-a64));
            assert_eq!(val(wa.abs()), wrap_i64(a64.abs()));
            for &b in &edges {
                let (wb, b64) = (wrap(b), wrap_i64(b));
                assert_eq!(val(wa + wb), wrap_i64(a64 + b64), "{a} + {b}");
                assert_eq!(val(wa - wb), wrap_i64(a64 - b64), "{a} - {b}");
                assert_eq!(val(wa.add_w(wb)), wrap_i64(a64 + b64));
                assert_eq!(val(wa.sub_w(wb)), wrap_i64(a64 - b64));
                assert_eq!(
                    val(wa.mul_w(wb)),
                    wrap_i64(a64.wrapping_mul(b64)),
                    "{a} * {b}"
                );
            }
        }
    }

    #[test]
    fn small_operand_forms_match_the_oracle() {
        for &a in &wide_edges() {
            let (wa, a64) = (wrap(a), wrap_i64(a));
            for k in I32_EDGES {
                assert_eq!(
                    val(wa.times(k)),
                    wrap_i64(a64.wrapping_mul(k as i128)),
                    "{a} * {k}"
                );
                assert_eq!(val(wa.plus(k)), wrap_i64(a64 + k as i128), "{a} + {k}");
                assert_eq!(wa.cmp_i32(k), a64.cmp(&(k as i128)), "{a} vs {k}");
            }
        }
    }

    #[test]
    fn dot_products_match_the_oracle() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for _ in 0..20_000 {
            let a = [rng.i32(), rng.i32(), rng.i32()];
            let b = [rng.i32(), rng.i32(), rng.i32()];
            let want: i128 = (0..3).map(|i| a[i] as i128 * b[i] as i128).sum();
            assert_eq!(val(Wide::dot3(a, b)), wrap_i64(want), "{a:?} . {b:?}");
        }
        let m = [i32::MIN; 3];
        assert_eq!(val(Wide::dot3(m, m)), wrap_i64(3 * (1i128 << 62)));
        assert_eq!(Wide::dot3([3, 4, 12], [3, 4, 12]).isqrt(), 13);
    }

    #[test]
    fn shifts_match_the_oracle_for_every_count() {
        for &a in &wide_edges() {
            let (wa, a64) = (wrap(a), wrap_i64(a));
            for n in 0..64u32 {
                assert_eq!(val(wa >> n), a64 >> n, "{a} >> {n}");
                assert_eq!(val(wa.sar(n)), a64 >> n);
                assert_eq!(val(wa << n), wrap_i64(a64 << n), "{a} << {n}");
                assert_eq!(val(wa.shl_by(n)), wrap_i64(a64 << n));
                assert_eq!(
                    val(wa.lshr(n)),
                    wrap_i64(((a64 as u128 & u64::MAX as u128) >> n) as i128)
                );
                assert_eq!(wa.shr_i32(n), (a64 >> n) as i32);
            }
            // The count is taken modulo 64.
            for n in [64u32, 65, 95, 127, 128] {
                assert_eq!(val(wa >> n), a64 >> (n & 63));
                assert_eq!(val(wa << n), wrap_i64(a64 << (n & 63)));
                assert_eq!(val(wa.lshr(n)), val(wa.lshr(n & 63)));
            }
        }
        for &a in &wide_edges() {
            let u = wrap_u64(a as u128);
            let w = WideU::from_parts((u >> 32) as u32, u as u32);
            for n in 0..64u32 {
                assert_eq!(valu(w >> n), u >> n, "{u} >> {n}");
                assert_eq!(valu(w << n), wrap_u64(u << n), "{u} << {n}");
                assert_eq!(w.shr_u32(n), (u >> n) as u32);
            }
            assert_eq!(valu(w >> 64), u);
            assert_eq!(valu(w << 70), wrap_u64(u << 6));
        }
    }

    #[test]
    fn extraction_variants_agree_with_the_oracle() {
        for &a in &wide_edges() {
            let (w, a64) = (wrap(a), wrap_i64(a));
            assert_eq!(
                w.saturate_i32() as i128,
                a64.clamp(i32::MIN as i128, i32::MAX as i128)
            );
            assert_eq!(
                w.to_i32(),
                (a64 >= i32::MIN as i128 && a64 <= i32::MAX as i128).then_some(a64 as i32)
            );
            assert_eq!(w.saturate_u32() as i128, a64.clamp(0, u32::MAX as i128));
            assert_eq!(
                w.to_u32(),
                (a64 >= 0 && a64 <= u32::MAX as i128).then_some(a64 as u32)
            );
            let u = wrap_u64(a as u128);
            let wu = WideU::from_parts((u >> 32) as u32, u as u32);
            assert_eq!(wu.saturate_u32() as u128, u.min(u32::MAX as u128));
            assert_eq!(wu.to_u32(), (u <= u32::MAX as u128).then_some(u as u32));
        }
    }

    #[test]
    fn comparisons_and_predicates_match_the_oracle() {
        let edges = wide_edges();
        for &a in &edges {
            let (wa, a64) = (wrap(a), wrap_i64(a));
            assert_eq!(wa.is_negative(), a64 < 0);
            assert_eq!(wa.is_positive(), a64 > 0);
            assert_eq!(wa.is_zero(), a64 == 0);
            for &b in &edges {
                let (wb, b64) = (wrap(b), wrap_i64(b));
                assert_eq!(wa.cmp(&wb), a64.cmp(&b64), "{a} vs {b}");
                assert_eq!(wa == wb, a64 == b64);
                assert_eq!(val(wa.max(wb)), a64.max(b64));
                assert_eq!(val(wa.min(wb)), a64.min(b64));
            }
        }
        let unsigned: Vec<u128> = edges.iter().map(|&v| wrap_u64(v as u128)).collect();
        for &a in &unsigned {
            let wa = WideU::from_parts((a >> 32) as u32, a as u32);
            assert_eq!(wa.is_zero(), a == 0);
            for &b in &unsigned {
                let wb = WideU::from_parts((b >> 32) as u32, b as u32);
                assert_eq!(wa.cmp(&wb), a.cmp(&b), "{a} vs {b}");
                assert_eq!(valu(wa + wb), wrap_u64(a + b));
                assert_eq!(valu(wa - wb), wrap_u64(a.wrapping_sub(b)));
                assert_eq!(valu(wa.saturating_add(wb)), (a + b).min(u64::MAX as u128));
            }
            for k in U32_EDGES {
                assert_eq!(
                    valu(wa.times(k)),
                    wrap_u64(a.wrapping_mul(k as u128)),
                    "{a} * {k}"
                );
            }
        }
    }

    #[test]
    fn isqrt_is_the_floor_root() {
        for &a in &wide_edges() {
            let w = wrap(a);
            let a64 = wrap_i64(a);
            let want = if a64 <= 0 {
                0
            } else {
                isqrt_oracle(a64 as u128)
            };
            assert_eq!(w.isqrt(), want, "isqrt {a}");
            let u = wrap_u64(a as u128);
            assert_eq!(
                WideU::from_parts((u >> 32) as u32, u as u32).isqrt(),
                isqrt_oracle(u)
            );
        }
        assert_eq!(Wide::of(-5).isqrt(), 0);
        assert_eq!(Wide::mul(i32::MAX, i32::MAX).isqrt(), i32::MAX as u32);
        assert_eq!(WideU::MAX.isqrt(), u32::MAX);
        assert_eq!(WideU::mul(u32::MAX, u32::MAX).isqrt(), u32::MAX);
    }

    #[test]
    fn random_properties_against_the_oracle() {
        let mut rng = Rng(0x0123_4567_89ab_cdef);
        for _ in 0..200_000 {
            let (a, b) = (rng.i32(), rng.i32());
            let (x, y) = (rng.wide(), rng.wide());
            let (wx, wy) = (wrap(x), wrap(y));
            let (x64, y64) = (wrap_i64(x), wrap_i64(y));
            assert_eq!(val(Wide::mul(a, b)), a as i128 * b as i128);
            assert_eq!(val(wx.mul_add(a, b)), wrap_i64(x64 + a as i128 * b as i128));
            assert_eq!(val(wx.mul_sub(a, b)), wrap_i64(x64 - a as i128 * b as i128));
            assert_eq!(val(wx + wy), wrap_i64(x64 + y64));
            assert_eq!(val(wx - wy), wrap_i64(x64 - y64));
            assert_eq!(val(wx.mul_w(wy)), wrap_i64(x64.wrapping_mul(y64)));
            assert_eq!(val(wx.times(a)), wrap_i64(x64.wrapping_mul(a as i128)));
            assert_eq!(wx.cmp(&wy), x64.cmp(&y64));
            assert_eq!(wx.cmp_i32(a), x64.cmp(&(a as i128)));
            let n = rng.shift();
            assert_eq!(val(wx >> n), x64 >> (n & 63));
            assert_eq!(val(wx << n), wrap_i64(x64 << (n & 63)));
            assert_eq!(wx.shr_i32(n), (x64 >> (n & 63)) as i32);
            assert_eq!(
                wx.saturate_i32() as i128,
                x64.clamp(i32::MIN as i128, i32::MAX as i128)
            );
            let (ua, ub) = (rng.next() as u32, rng.next() as u32);
            assert_eq!(valu(WideU::mul(ua, ub)), ua as u128 * ub as u128);
            let ux = wrap_u64(x as u128);
            let wux = WideU::from_parts((ux >> 32) as u32, ux as u32);
            assert_eq!(
                valu(wux.mul_add(ua, ub)),
                wrap_u64(ux + ua as u128 * ub as u128)
            );
            assert_eq!(valu(wux >> n), ux >> (n & 63));
            assert_eq!(wux.isqrt(), isqrt_oracle(ux));
        }
    }

    #[test]
    fn the_games_call_shapes_still_compute_what_they_did() {
        // hl-psx proj_logic: Wide::mul(v, 819).mul_add(dir, along).plus(2048) >> 12.
        let (v, dir, along) = (-123_456, 7_777, 31_415);
        let want = (v as i128 * 819 + dir as i128 * along as i128 + 2048) >> 12;
        assert_eq!(
            Wide::mul(v, 819).mul_add(dir, along).plus(2048).shr_i32(12),
            want as i32
        );
        // wipeout fx::atan2: headroom compare and shifts on a Wide.
        let limit = Wide::of(1) << 40;
        assert_eq!(val(limit), 1 << 40);
        assert!(Wide::of(1_000_000_000).shl_by(11).abs() > limit);
        assert!(Wide::of(1_000_000_000).shl_by(9).abs() <= limit);
        // cs-psx view_space_facing: the cross terms and the final 64x64 dot.
        let n = [
            Wide::mul(3, 5).mul_sub(7, 11),
            Wide::mul(-3, 5),
            Wide::mul(i32::MAX, i32::MAX),
        ];
        let sum = Wide::of(2).mul_w(n[0]) + Wide::of(-4).mul_w(n[1]) + Wide::of(1).mul_w(n[2]);
        assert_eq!(
            val(sum),
            2 * (15 - 77) + (-4) * (-15) + (i32::MAX as i128).pow(2)
        );
    }

    #[test]
    fn mask64_sets_and_clears_each_bit() {
        let mut mask = Mask64::EMPTY;
        for bit in [0usize, 1, 31, 32, 33, 63] {
            mask.set(bit);
            assert!(mask.test(bit));
        }
        assert!(!mask.test(2) && !mask.test(64) && !mask.test(200));
        mask.assign(32, false);
        mask.clear(0);
        mask.assign(5, true);
        assert!(!mask.test(32) && !mask.test(0) && mask.test(33) && mask.test(63) && mask.test(5));
        for bit in 0..64 {
            let mut one = Mask64::EMPTY;
            one.set(bit);
            assert!((0..64).all(|other| one.test(other) == (other == bit)));
            one.clear(bit);
            assert_eq!(one, Mask64::EMPTY);
        }
    }

    #[test]
    fn debug_prints_the_two_words() {
        use core::fmt::Write;
        struct Buf([u8; 48], usize);
        impl Write for Buf {
            fn write_str(&mut self, s: &str) -> core::fmt::Result {
                self.0[self.1..self.1 + s.len()].copy_from_slice(s.as_bytes());
                self.1 += s.len();
                Ok(())
            }
        }
        let mut b = Buf([0; 48], 0);
        write!(b, "{:?} {:?}", Wide::from_parts(-1, 0x1234), WideU::MAX).unwrap();
        assert_eq!(
            &b.0[..b.1],
            b"Wide(ffffffff:00001234) WideU(ffffffff:ffffffff)"
        );
    }
}
