// SPDX-License-Identifier: GPL-2.0-or-later
//! `hello-wide` -- every `psx_math::Wide` / `WideU` / `Mask64` operation, run
//! on the target against a reference built from 32-bit arithmetic only.
//!
//! Two jobs:
//!
//! 1. Prove the operations compute the right answer on the R3000 (edge values
//!    and a fixed pseudo-random stream; the verdict goes to the TTY as
//!    `WIDE PASS` / `WIDE FAIL`).
//! 2. Prove they link no compiler helper: the image is built with a link map
//!    and `tools/guest_symbol_gate.sh` must pass on it (the gate fails on
//!    `__divdi3`, `__udivdi3`, `__moddi3`, `__umoddi3`, `__muldi3`, the 64-bit
//!    shift helpers, `u64_div_rem`, the 128-bit helpers and soft-float).
//!
//! Nothing here names `i64` or `u64`. Operands go through `black_box`, so
//! nothing const-folds and every operation is real code.

#![no_std]
#![no_main]

extern crate psx_rt;

use core::cmp::Ordering;
use core::hint::black_box;
use psx_math::{Mask64, Wide, WideU};
use psx_rt::tty;

/// A 64-bit value as two words, with just enough 32-bit arithmetic to check
/// the library: the reference.
#[derive(Clone, Copy, PartialEq, Eq)]
struct R {
    hi: u32,
    lo: u32,
}

impl R {
    const fn new(hi: u32, lo: u32) -> R {
        R { hi, lo }
    }
    fn of(a: i32) -> R {
        R::new(if a < 0 { u32::MAX } else { 0 }, a as u32)
    }
    fn add(self, o: R) -> R {
        let lo = self.lo.wrapping_add(o.lo);
        let carry = (lo < self.lo) as u32;
        R::new(self.hi.wrapping_add(o.hi).wrapping_add(carry), lo)
    }
    fn neg(self) -> R {
        R::new(!self.hi, !self.lo).add(R::new(0, 1))
    }
    fn sub(self, o: R) -> R {
        self.add(o.neg())
    }
    fn is_neg(self) -> bool {
        self.hi >> 31 != 0
    }
    fn abs(self) -> R {
        if self.is_neg() {
            self.neg()
        } else {
            self
        }
    }
    /// The full unsigned product of two words, from 16-bit limbs.
    fn mulu(a: u32, b: u32) -> R {
        let (a0, a1, b0, b1) = (a & 0xffff, a >> 16, b & 0xffff, b >> 16);
        let (p00, p01, p10, p11) = (a0 * b0, a0 * b1, a1 * b0, a1 * b1);
        let mid = (p00 >> 16) + (p01 & 0xffff) + (p10 & 0xffff);
        R::new(
            p11 + (p01 >> 16) + (p10 >> 16) + (mid >> 16),
            (p00 & 0xffff) | (mid << 16),
        )
    }
    /// The full signed product of two words.
    fn muls(a: i32, b: i32) -> R {
        let p = R::mulu(a as u32, b as u32);
        let mut hi = p.hi;
        if a < 0 {
            hi = hi.wrapping_sub(b as u32);
        }
        if b < 0 {
            hi = hi.wrapping_sub(a as u32);
        }
        R::new(hi, p.lo)
    }
    /// The low 64 bits of the product of two 64-bit values.
    fn mul_low(self, o: R) -> R {
        let p = R::mulu(self.lo, o.lo);
        R::new(
            p.hi.wrapping_add(self.lo.wrapping_mul(o.hi))
                .wrapping_add(self.hi.wrapping_mul(o.lo)),
            p.lo,
        )
    }
    fn shl(self, n: u32) -> R {
        let n = n & 63;
        if n == 0 {
            self
        } else if n < 32 {
            R::new((self.hi << n) | (self.lo >> (32 - n)), self.lo << n)
        } else {
            R::new(self.lo << (n - 32), 0)
        }
    }
    /// Shift right; `fill` is the word shifted in (0, or all ones to keep a sign).
    fn shr(self, n: u32, fill: u32) -> R {
        let n = n & 63;
        if n == 0 {
            self
        } else if n < 32 {
            R::new(
                (self.hi >> n) | (fill << (32 - n)),
                (self.lo >> n) | (self.hi << (32 - n)),
            )
        } else if n == 32 {
            R::new(fill, self.hi)
        } else {
            R::new(fill, (self.hi >> (n - 32)) | (fill << (64 - n)))
        }
    }
    fn sar(self, n: u32) -> R {
        self.shr(n, if self.is_neg() { u32::MAX } else { 0 })
    }
    fn cmp_signed(self, o: R) -> Ordering {
        (self.hi as i32)
            .cmp(&(o.hi as i32))
            .then(self.lo.cmp(&o.lo))
    }
    fn cmp_unsigned(self, o: R) -> Ordering {
        self.hi.cmp(&o.hi).then(self.lo.cmp(&o.lo))
    }
}

/// xorshift32, the same stream on every run.
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
    /// A word of random magnitude, so small, Q12-sized and full-range values all occur.
    fn word(&mut self) -> u32 {
        let v = self.next();
        let w = self.next();
        match v & 3 {
            0 => w,
            1 => ((w as i32) >> 16) as u32,
            2 => ((w as i32) >> 8) as u32,
            _ => ((w as i32) >> 24) as u32,
        }
    }
    fn wide(&mut self) -> R {
        let v = self.next();
        let (hi, lo) = (self.word(), self.next());
        match v & 3 {
            0 => R::new(hi, lo),
            1 => R::of(self.word() as i32),
            2 => R::new(hi >> 12, lo),
            _ => R::new(hi >> 28, lo),
        }
    }
}

struct Run {
    cases: u32,
    failures: u32,
}

impl Run {
    fn check(&mut self, name: &str, ok: bool) {
        self.cases += 1;
        if !ok {
            self.failures += 1;
            if self.failures <= 16 {
                tty::print("  FAIL ");
                tty::println(name);
            }
        }
    }
    fn wide(&mut self, name: &str, got: Wide, want: R) {
        self.check(
            name,
            got.high() as u32 == want.hi && got.low_u32() == want.lo,
        );
    }
    fn wide_u(&mut self, name: &str, got: WideU, want: R) {
        self.check(name, got.high() == want.hi && got.low() == want.lo);
    }
}

fn to_wide(r: R) -> Wide {
    Wide::from_parts(black_box(r.hi) as i32, black_box(r.lo))
}

fn to_wide_u(r: R) -> WideU {
    WideU::from_parts(black_box(r.hi), black_box(r.lo))
}

const EDGE_WORDS: [u32; 16] = [
    0,
    1,
    2,
    4095,
    4096,
    0xffff,
    0x1_0000,
    0x7fff_ffff,
    0x8000_0000,
    0x8000_0001,
    0xffff_f000,
    0xffff_ffff,
    0xffff_fffe,
    0x1234_5678,
    0xdead_beef,
    0xffff_0000,
];

/// Every value the checks run through: the edge words as both halves, and a
/// run of random ones.
fn wide_values(rng: &mut Rng, out: &mut [R; 320]) {
    let mut n = 0;
    for &hi in &EDGE_WORDS[..8] {
        for &lo in &EDGE_WORDS {
            out[n] = R::new(hi, lo);
            n += 1;
        }
    }
    for &hi in &EDGE_WORDS[8..] {
        for &lo in &EDGE_WORDS[..8] {
            out[n] = R::new(hi, lo);
            n += 1;
        }
    }
    while n < out.len() {
        out[n] = rng.wide();
        n += 1;
    }
}

fn products(run: &mut Run, rng: &mut Rng) {
    for i in 0..EDGE_WORDS.len() * EDGE_WORDS.len() + 3000 {
        let (a, b) = if i < EDGE_WORDS.len() * EDGE_WORDS.len() {
            (
                EDGE_WORDS[i / EDGE_WORDS.len()],
                EDGE_WORDS[i % EDGE_WORDS.len()],
            )
        } else {
            (rng.word(), rng.word())
        };
        let (sa, sb) = (black_box(a) as i32, black_box(b) as i32);
        let acc = rng.wide();
        let p = R::muls(sa, sb);
        run.wide("mul", Wide::mul(sa, sb), p);
        run.wide("mul_add", to_wide(acc).mul_add(sa, sb), acc.add(p));
        run.wide("mul_sub", to_wide(acc).mul_sub(sa, sb), acc.sub(p));
        run.wide("of", Wide::of(sa), R::of(sa));
        run.wide("of_u", Wide::of_u(black_box(a)), R::new(0, a));
        run.wide(
            "from_parts",
            Wide::from_parts(sa, black_box(b)),
            R::new(a, b),
        );
        let pu = R::mulu(a, b);
        run.wide_u("mulu", WideU::mul(black_box(a), black_box(b)), pu);
        run.wide_u(
            "mulu_add",
            to_wide_u(acc).mul_add(black_box(a), black_box(b)),
            acc.add(pu),
        );
        run.wide_u("of_unsigned", WideU::of(black_box(a)), R::new(0, a));
    }
    let v = [black_box(3), black_box(-4096), black_box(i32::MIN)];
    let w = [black_box(i32::MAX), black_box(7), black_box(i32::MIN)];
    let want = R::muls(v[0], w[0])
        .add(R::muls(v[1], w[1]))
        .add(R::muls(v[2], w[2]));
    run.wide("dot3", Wide::dot3(v, w), want);
}

fn arithmetic(run: &mut Run, values: &[R; 320]) {
    for (i, &a) in values.iter().enumerate() {
        let wa = to_wide(a);
        run.wide("neg", -wa, a.neg());
        run.wide("negate", wa.negate(), a.neg());
        run.wide("abs", wa.abs(), a.abs());
        run.check("is_negative", wa.is_negative() == a.is_neg());
        run.check(
            "is_positive",
            wa.is_positive() == (!a.is_neg() && (a.hi | a.lo) != 0),
        );
        run.check("is_zero", wa.is_zero() == (a.hi | a.lo == 0));
        run.check("low", wa.low() == a.lo as i32 && wa.low_u32() == a.lo);
        run.check("high", wa.high() as u32 == a.hi);
        let wu = to_wide_u(a);
        run.check("wide_u low/high", wu.low() == a.lo && wu.high() == a.hi);
        run.check("wide_u is_zero", wu.is_zero() == (a.hi | a.lo == 0));
        // 15 partners per value keeps the pair count bounded.
        for j in 0..15 {
            let b = values[(i * 7 + j * 19 + 1) % values.len()];
            let wb = to_wide(b);
            run.wide("add", wa + wb, a.add(b));
            run.wide("sub", wa - wb, a.sub(b));
            run.wide("add_w", wa.add_w(wb), a.add(b));
            run.wide("sub_w", wa.sub_w(wb), a.sub(b));
            run.wide("mul_w", wa.mul_w(wb), a.mul_low(b));
            run.check("cmp", wa.cmp(&wb) == a.cmp_signed(b));
            run.check("lt/ge", (wa < wb) == (a.cmp_signed(b) == Ordering::Less));
            run.check(
                "max",
                wa.max(wb)
                    == if a.cmp_signed(b) == Ordering::Less {
                        wb
                    } else {
                        wa
                    },
            );
            run.check(
                "min",
                wa.min(wb)
                    == if a.cmp_signed(b) == Ordering::Greater {
                        wb
                    } else {
                        wa
                    },
            );
            let (ua, ub) = (to_wide_u(a), to_wide_u(b));
            run.wide_u("wide_u add", ua + ub, a.add(b));
            run.wide_u("wide_u sub", ua - ub, a.sub(b));
            run.wide_u("wide_u add()", WideU::add(ua, ub), a.add(b));
            run.wide_u("wide_u sub()", WideU::sub(ua, ub), a.sub(b));
            let sum = a.add(b);
            let overflow = R::cmp_unsigned(sum, a) == Ordering::Less;
            run.wide_u(
                "wide_u saturating_add",
                ua.saturating_add(ub),
                if overflow {
                    R::new(u32::MAX, u32::MAX)
                } else {
                    sum
                },
            );
            run.check("wide_u cmp", ua.cmp(&ub) == a.cmp_unsigned(b));
        }
        for &k in &EDGE_WORDS {
            let k = black_box(k) as i32;
            run.wide("times", wa.times(k), a.mul_low(R::of(k)));
            run.wide("plus", wa.plus(k), a.add(R::of(k)));
            run.check("cmp_i32", wa.cmp_i32(k) == a.cmp_signed(R::of(k)));
            run.wide_u(
                "wide_u times",
                wu.times(k as u32),
                a.mul_low(R::new(0, k as u32)),
            );
        }
    }
}

fn shifts(run: &mut Run, values: &[R; 320]) {
    for &a in values.iter().step_by(3) {
        let (wa, wu) = (to_wide(a), to_wide_u(a));
        // Every count 0..64 plus counts past 64 (taken modulo 64).
        for n in 0..72u32 {
            let n = black_box(n);
            run.wide("sar", wa.sar(n), a.sar(n));
            run.wide(">>", wa >> n, a.sar(n));
            run.wide("shl_by", wa.shl_by(n), a.shl(n));
            run.wide("<<", wa << n, a.shl(n));
            run.wide("lshr", wa.lshr(n), a.shr(n, 0));
            run.check("shr_i32", wa.shr_i32(n) == a.sar(n).lo as i32);
            run.wide_u("wide_u >>", wu >> n, a.shr(n, 0));
            run.wide_u("wide_u <<", wu << n, a.shl(n));
            run.wide_u("wide_u shr_by", wu.shr_by(n), a.shr(n, 0));
            run.wide_u("wide_u shl_by", wu.shl_by(n), a.shl(n));
            run.check("wide_u shr_u32", wu.shr_u32(n) == a.shr(n, 0).lo);
        }
    }
}

fn extraction(run: &mut Run, values: &[R; 320]) {
    for &a in values {
        let (wa, wu) = (to_wide(a), to_wide_u(a));
        let fits_i32 = a.hi == (a.lo as i32 >> 31) as u32;
        run.check(
            "to_i32",
            wa.to_i32() == if fits_i32 { Some(a.lo as i32) } else { None },
        );
        let sat = if fits_i32 {
            a.lo as i32
        } else if a.is_neg() {
            i32::MIN
        } else {
            i32::MAX
        };
        run.check("saturate_i32", wa.saturate_i32() == sat);
        let fits_u32 = a.hi == 0;
        run.check(
            "to_u32",
            wa.to_u32() == if fits_u32 { Some(a.lo) } else { None },
        );
        let sat_u = if a.is_neg() {
            0
        } else if a.hi != 0 {
            u32::MAX
        } else {
            a.lo
        };
        run.check("saturate_u32", wa.saturate_u32() == sat_u);
        run.check(
            "wide_u to_u32",
            wu.to_u32() == if fits_u32 { Some(a.lo) } else { None },
        );
        run.check(
            "wide_u saturate_u32",
            wu.saturate_u32() == if a.hi != 0 { u32::MAX } else { a.lo },
        );
    }
}

fn roots(run: &mut Run, values: &[R; 320]) {
    for &a in values {
        // The floor root r satisfies r*r <= v < (r+1)*(r+1) (r+1 is 2^32 at the top).
        let unsigned_root = to_wide_u(a).isqrt();
        let square = R::mulu(unsigned_root, unsigned_root);
        let below = R::cmp_unsigned(square, a) != Ordering::Greater;
        let next = R::mulu(unsigned_root.wrapping_add(1), unsigned_root.wrapping_add(1));
        let above = unsigned_root == u32::MAX || R::cmp_unsigned(next, a) == Ordering::Greater;
        run.check("wide_u isqrt", below && above);
        let signed_root = to_wide(a).isqrt();
        if a.is_neg() || (a.hi | a.lo) == 0 {
            run.check("isqrt of <= 0", signed_root == 0);
        } else {
            run.check("isqrt", signed_root == unsigned_root);
        }
    }
    run.check(
        "isqrt 13",
        Wide::dot3(black_box([3, 4, 12]), [3, 4, 12]).isqrt() == 13,
    );
}

fn masks(run: &mut Run, rng: &mut Rng) {
    let mut mask = black_box(Mask64::EMPTY);
    let (mut lo, mut hi) = (0u32, 0u32);
    for _ in 0..2000 {
        let bit = (rng.next() % 64) as usize;
        let on = rng.next() & 1 != 0;
        let word = if bit < 32 { &mut lo } else { &mut hi };
        match rng.next() % 3 {
            0 if on => {
                mask.set(bit);
                *word |= 1 << (bit & 31);
            }
            1 => {
                mask.clear(bit);
                *word &= !(1 << (bit & 31));
            }
            _ => {
                mask.assign(bit, on);
                if on {
                    *word |= 1 << (bit & 31);
                } else {
                    *word &= !(1 << (bit & 31));
                }
            }
        }
        let probe = (rng.next() % 72) as usize;
        let want = probe < 64 && (if probe < 32 { lo } else { hi }) >> (probe & 31) & 1 != 0;
        run.check("mask test", mask.test(probe) == want);
    }
}

#[no_mangle]
fn main() {
    let mut run = Run {
        cases: 0,
        failures: 0,
    };
    let mut rng = Rng(0x9e37_79b9);
    let mut values = [R::new(0, 0); 320];
    wide_values(&mut rng, &mut values);
    products(&mut run, &mut rng);
    arithmetic(&mut run, &values);
    shifts(&mut run, &values);
    extraction(&mut run, &values);
    roots(&mut run, &values);
    masks(&mut run, &mut rng);
    if run.failures == 0 {
        tty::print("WIDE PASS cases=");
        tty::print_hex_u32(run.cases);
    } else {
        tty::print("WIDE FAIL failures=");
        tty::print_hex_u32(run.failures);
        tty::print(" of=");
        tty::print_hex_u32(run.cases);
    }
    tty::println("");
    tty::println("WIDE DONE");
    loop {}
}
