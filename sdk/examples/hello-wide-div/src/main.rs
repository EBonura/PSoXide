// SPDX-License-Identifier: GPL-2.0-or-later
//! `hello-wide-div` -- every `psx_math::wide_div` operation (division of a `Wide` /
//! `WideU`), run on the target against a reference built from 32-bit arithmetic only,
//! then timed.
//!
//! Three jobs:
//!
//! 1. Prove the operations compute the right answer on the R3000: edge values and a
//!    fixed pseudo-random stream, against a bit-by-bit restoring division over 32-bit
//!    words (the verdict goes to the TTY as `WIDEDIV PASS` / `WIDEDIV FAIL`).
//! 2. Prove they link no compiler helper: the image is built with a link map and
//!    `tools/guest_symbol_gate.sh` must pass on it, with no opt-out.
//! 3. Print the cost of each operation in CPU cycles (`widediv: <op> cycles_per_call=N`)
//!    from root counter 2, with the input shuffling subtracted. The figures are what the
//!    table in the `psx-math` README quotes.
//!
//! Nothing here names `i64` or `u64`. Operands go through `black_box`, so nothing
//! const-folds and every operation is real code.

#![no_std]
#![no_main]

extern crate psx_rt;

use core::cmp::Ordering;
use core::hint::black_box;
use core::ptr::{read_volatile, write_volatile};
use psx_math::{Rounding, Wide, WideU};
use psx_rt::tty;

/// A 64-bit value as two words, with just enough 32-bit arithmetic to check the library.
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
    fn is_zero(self) -> bool {
        self.hi | self.lo == 0
    }
    fn abs(self) -> R {
        if self.is_neg() {
            self.neg()
        } else {
            self
        }
    }
    fn cmp(self, o: R) -> Ordering {
        self.hi.cmp(&o.hi).then(self.lo.cmp(&o.lo))
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
}

/// The exact 128-bit product of two unsigned 64-bit values, as four words, low first.
fn mul_full(a: R, b: R) -> [u32; 4] {
    let (p00, p01, p10, p11) = (
        R::mulu(a.lo, b.lo),
        R::mulu(a.lo, b.hi),
        R::mulu(a.hi, b.lo),
        R::mulu(a.hi, b.hi),
    );
    // Column sums with carries, in R so no 32-bit add can wrap unseen.
    let c1 = R::new(0, p00.hi)
        .add(R::new(0, p01.lo))
        .add(R::new(0, p10.lo));
    let c2 = R::new(0, p01.hi)
        .add(R::new(0, p10.hi))
        .add(R::new(0, p11.lo))
        .add(R::new(0, c1.hi));
    let w3 = p11.hi.wrapping_add(c2.hi);
    [p00.lo, c1.lo, c2.lo, w3]
}

fn cmp_words(a: [u32; 4], b: [u32; 4]) -> Ordering {
    for i in (0..4).rev() {
        let c = a[i].cmp(&b[i]);
        if c != Ordering::Equal {
            return c;
        }
    }
    Ordering::Equal
}

/// Restoring long division of a numerator of `bits` bits (words low first) by a nonzero
/// 64-bit divisor: quotient words and the remainder. One bit at a time, 32-bit only.
fn udivrem(n: [u32; 4], d: R, bits: usize) -> ([u32; 4], R) {
    let mut q = [0u32; 4];
    let mut r = R::new(0, 0);
    for i in (0..bits).rev() {
        let carry = r.hi >> 31;
        r = R::new(
            (r.hi << 1) | (r.lo >> 31),
            (r.lo << 1) | ((n[i / 32] >> (i % 32)) & 1),
        );
        if carry != 0 || r.cmp(d) != Ordering::Less {
            r = r.sub(d);
            q[i / 32] |= 1 << (i % 32);
        }
    }
    (q, r)
}

/// The signed quotient of `n / d` rounded as asked: its sign and magnitude (a 64-bit
/// value; the quotient of two 64-bit values always fits).
fn signed_quotient(n: R, d: R, round: Rounding) -> (bool, R) {
    let (an, ad) = (n.abs(), d.abs());
    let (q, rest) = udivrem([an.lo, an.hi, 0, 0], ad, 64);
    let negative = n.is_neg() != d.is_neg();
    let up = match round {
        Rounding::TowardZero => false,
        Rounding::Floor => negative && !rest.is_zero(),
        Rounding::Nearest => rest.cmp(ad.sub(rest)) != Ordering::Less,
    };
    let mag = R::new(q[1], q[0]);
    (negative, if up { mag.add(R::new(0, 1)) } else { mag })
}

fn wrap(negative: bool, mag: R) -> i32 {
    if negative {
        mag.lo.wrapping_neg() as i32
    } else {
        mag.lo as i32
    }
}

fn saturate(negative: bool, mag: R) -> i32 {
    if negative {
        if mag.hi != 0 || mag.lo > 0x8000_0000 {
            i32::MIN
        } else {
            mag.lo.wrapping_neg() as i32
        }
    } else if mag.hi != 0 || mag.lo > 0x7fff_ffff {
        i32::MAX
    } else {
        mag.lo as i32
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
        w >> (v % 32)
    }
    /// A 64-bit value of random magnitude and sign.
    fn wide(&mut self) -> R {
        let v = self.next();
        let (hi, lo) = (self.next(), self.next());
        let r = match v & 3 {
            0 => R::new(hi, lo),
            1 => R::of(self.word() as i32),
            2 => R::new(hi >> (self.next() % 32), lo),
            _ => R::new(0, lo >> (self.next() % 32)),
        };
        if v & 4 != 0 {
            r.neg()
        } else {
            r
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
}

fn to_wide(r: R) -> Wide {
    Wide::from_parts(black_box(r.hi) as i32, black_box(r.lo))
}

fn to_wide_u(r: R) -> WideU {
    WideU::from_parts(black_box(r.hi), black_box(r.lo))
}

const EDGE_WORDS: [u32; 12] = [
    0,
    1,
    2,
    3,
    0xffff,
    0x1_0000,
    0x7fff_ffff,
    0x8000_0000,
    0x8000_0001,
    0xffff_ffff,
    0xffff_fffe,
    0x1234_5678,
];

const ROUNDS: [Rounding; 3] = [Rounding::TowardZero, Rounding::Floor, Rounding::Nearest];

/// Every 64-bit value the checks run through: the edge words as both halves, then random.
fn values(rng: &mut Rng, out: &mut [R; 200]) {
    let mut n = 0;
    for &hi in &EDGE_WORDS {
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

fn unsigned_ops(run: &mut Run, vals: &[R; 200]) {
    for (i, &a) in vals.iter().enumerate() {
        let wa = to_wide_u(a);
        for j in 0..12 {
            let b = vals[(i * 5 + j * 17 + 3) % vals.len()];
            // 128-bit product.
            let (high, low) = wa.mul_full(to_wide_u(b));
            let want = mul_full(a, b);
            run.check(
                "mul_full",
                [low.low(), low.high(), high.low(), high.high()] == want,
            );
            if b.is_zero() {
                continue;
            }
            // Division by a word and by a wide value.
            let (qw, rest) = udivrem([a.lo, a.hi, 0, 0], b, 64);
            let wb = to_wide_u(b);
            run.check(
                "quotient",
                wa.quotient(wb) == if qw[1] != 0 { u32::MAX } else { qw[0] },
            );
            if b.hi == 0 {
                let (q, r) = wa.div_rem_u32(black_box(b.lo));
                run.check(
                    "div_rem_u32",
                    q.low() == qw[0] && q.high() == qw[1] && R::new(0, r) == rest,
                );
            }
            // Ratio order: a/b against the next two values.
            let c = vals[(i * 3 + j * 11 + 1) % vals.len()];
            let d = vals[(i * 7 + j * 13 + 5) % vals.len()];
            if !d.is_zero() {
                let want = cmp_words(mul_full(a, d), mul_full(c, b));
                run.check(
                    "cmp_ratio",
                    WideU::cmp_ratio(wa, wb, to_wide_u(c), to_wide_u(d)) == want,
                );
            }
        }
    }
}

fn scale_ops(run: &mut Run, rng: &mut Rng, vals: &[R; 200]) {
    for i in 0..1500usize {
        let den = vals[i % vals.len()];
        if den.is_zero() {
            continue;
        }
        // A fraction t <= den: den itself, zero, or den scaled down by a random shift.
        let t = match rng.next() % 4 {
            0 => den,
            1 => R::new(0, 0),
            _ => {
                let shifted = shr(den, rng.next() % 64);
                shifted
            }
        };
        let a = rng.word();
        let product = mul_full(R::new(0, a), t);
        let (q, _) = udivrem(product, den, 96);
        run.check(
            "scale_by_ratio",
            to_wide_u(t).scale_by_ratio(black_box(a), to_wide_u(den)) == q[0],
        );
    }
}

fn shr(r: R, n: u32) -> R {
    if n == 0 {
        r
    } else if n < 32 {
        R::new(r.hi >> n, (r.lo >> n) | (r.hi << (32 - n)))
    } else {
        R::new(0, r.hi >> (n - 32))
    }
}

fn signed_ops(run: &mut Run, rng: &mut Rng, vals: &[R; 200]) {
    for (i, &n) in vals.iter().enumerate() {
        let wn = to_wide(n);
        // Word divisors: the edge words as signed values, and random ones.
        for j in 0..14usize {
            let d32 = if j < EDGE_WORDS.len() {
                EDGE_WORDS[j]
            } else {
                rng.word()
            };
            if d32 == 0 {
                continue;
            }
            let d32 = d32 as i32;
            for round in ROUNDS {
                let (neg, mag) = signed_quotient(n, R::of(d32), round);
                run.check(
                    "div_i32_wrap",
                    wn.div_i32_wrap(black_box(d32), round) == wrap(neg, mag),
                );
                run.check(
                    "div_i32_sat",
                    wn.div_i32_sat(black_box(d32), round) == saturate(neg, mag),
                );
            }
        }
        // Wide divisors.
        for j in 0..8usize {
            let d = vals[(i * 5 + j * 23 + 1) % vals.len()];
            if d.is_zero() {
                continue;
            }
            for round in ROUNDS {
                let (neg, mag) = signed_quotient(n, d, round);
                run.check(
                    "div_wide_sat",
                    wn.div_wide_sat(to_wide(d), round) == saturate(neg, mag),
                );
            }
        }
        // Division by a power of two truncates toward zero.
        for s in [0u32, 1, 12, 31, 32, 33, 62] {
            let s = black_box(s);
            let (neg, mag) = signed_quotient(n, R::new(0, 1).shl(s), Rounding::TowardZero);
            let want = if neg { mag.neg() } else { mag };
            let got = wn.div_pow2(s);
            run.check(
                "div_pow2",
                got.high() as u32 == want.hi && got.low_u32() == want.lo,
            );
        }
    }
}

impl R {
    fn shl(self, n: u32) -> R {
        if n == 0 {
            self
        } else if n < 32 {
            R::new((self.hi << n) | (self.lo >> (32 - n)), self.lo << n)
        } else {
            R::new(self.lo << (n - 32), 0)
        }
    }
}

// ---------------------------------------------------------------------------------------
// Timing.

/// Calls per batch. 64 calls of at most a few thousand cycles stay well inside the 16-bit
/// counter (524,288 cycles).
const BATCH: u32 = 64;
/// Batches averaged per figure.
const REPS: u32 = 8;

const T2_COUNTER: *const u32 = (psx_hw::timers::block(2) + psx_hw::timers::COUNTER) as *const u32;
const T2_MODE: *mut u32 = (psx_hw::timers::block(2) + psx_hw::timers::MODE) as *mut u32;
const T2_SYSCLK_DIV8: u32 = 1 << 9;

fn now() -> u32 {
    // SAFETY: a read of root counter 2's current-value register.
    unsafe { read_volatile(T2_COUNTER) & 0xffff }
}

/// Counter ticks (8 CPU cycles each) for `REPS` batches of `BATCH` calls of `body(i)`.
fn ticks(mut body: impl FnMut(usize)) -> u32 {
    let mut total = 0u32;
    for _ in 0..REPS {
        let start = now();
        for i in 0..BATCH as usize {
            body(i);
        }
        total += now().wrapping_sub(start) & 0xffff;
    }
    total
}

fn print_dec(mut v: u32) {
    let mut buf = [0u8; 10];
    let mut n = 0;
    if v == 0 {
        buf[0] = b'0';
        n = 1;
    }
    while v > 0 {
        buf[n] = b'0' + (v % 10) as u8;
        v /= 10;
        n += 1;
    }
    let mut out = [0u8; 10];
    for i in 0..n {
        out[i] = buf[n - 1 - i];
    }
    // SAFETY: only ASCII digits were written.
    tty::print(unsafe { core::str::from_utf8_unchecked(&out[..n]) });
}

fn report(label: &str, total_ticks: u32, base_ticks: u32) {
    let cycles = total_ticks.saturating_sub(base_ticks) * 8 / (REPS * BATCH);
    tty::print("widediv: ");
    tty::print(label);
    tty::print(" cycles_per_call=");
    print_dec(cycles);
    tty::print("\n");
}

/// Operand sets for the figures. Each set has 16 entries, indexed round-robin.
struct Sets {
    /// Numerators that fit 32 bits.
    small: [(u32, u32); 16],
    /// Numerators of every magnitude up to 64 bits (high word nonzero).
    big: [(u32, u32); 16],
    /// Divisors of every magnitude that fit 32 bits.
    word: [u32; 16],
    /// Divisors from 2^32 up.
    wide: [(u32, u32); 16],
}

fn sets(rng: &mut Rng) -> Sets {
    let mut s = Sets {
        small: [(0, 0); 16],
        big: [(0, 0); 16],
        word: [1; 16],
        wide: [(1, 1); 16],
    };
    for i in 0..16 {
        s.small[i] = (0, rng.word() | 1);
        s.big[i] = (rng.word() | 1, rng.next());
        s.word[i] = rng.word() | 1;
        s.wide[i] = (rng.word() | 1, rng.next());
    }
    s
}

fn bench(rng: &mut Rng) {
    // SAFETY: counter 2's mode register; nothing else uses the counter here.
    unsafe { write_volatile(T2_MODE, T2_SYSCLK_DIV8) };
    let s = sets(rng);
    let mut sink = 0u32;
    let base = ticks(|i| {
        let a = black_box(s.big[i & 15]);
        let d = black_box(s.word[i & 15]);
        sink = sink.wrapping_add(a.0 ^ a.1 ^ d);
    });
    let wu = |p: (u32, u32)| WideU::from_parts(black_box(p.0), black_box(p.1));
    let ws = |p: (u32, u32)| Wide::from_parts(black_box(p.0) as i32, black_box(p.1));
    let mut bench_u = |label: &str, f: &mut dyn FnMut(usize) -> u32| {
        let t = ticks(|i| sink = sink.wrapping_add(f(i)));
        report(label, t, base);
    };
    bench_u("div_rem_u32 (numerator < 2^32)", &mut |i| {
        wu(s.small[i & 15]).div_rem_u32(black_box(s.word[i & 15])).1
    });
    bench_u("div_rem_u32 (64-bit numerator)", &mut |i| {
        wu(s.big[i & 15]).div_rem_u32(black_box(s.word[i & 15])).1
    });
    bench_u("quotient (word divisor)", &mut |i| {
        wu(s.big[i & 15]).quotient(WideU::of(black_box(s.word[i & 15])))
    });
    bench_u("quotient (wide divisor)", &mut |i| {
        wu(s.big[i & 15]).quotient(wu(s.wide[i & 15]))
    });
    bench_u("mul_full", &mut |i| {
        wu(s.big[i & 15]).mul_full(wu(s.wide[i & 15])).1.low()
    });
    bench_u("cmp_ratio", &mut |i| {
        WideU::cmp_ratio(
            wu(s.big[i & 15]),
            wu(s.wide[i & 15]),
            wu(s.wide[(i + 1) & 15]),
            wu(s.big[(i + 5) & 15]),
        ) as u32
    });
    bench_u("scale_by_ratio (wide denominator)", &mut |i| {
        // t <= den: the numerator word pair shifted down against the full denominator.
        let den = wu(s.wide[i & 15]);
        let t = den.shr_by(black_box(1));
        t.scale_by_ratio(black_box(s.word[i & 15]), den)
    });
    bench_u("div_i32_wrap TowardZero (numerator < 2^32)", &mut |i| {
        ws(s.small[i & 15]).div_i32_wrap(black_box(s.word[i & 15]) as i32, Rounding::TowardZero)
            as u32
    });
    bench_u("div_i32_wrap TowardZero (64-bit numerator)", &mut |i| {
        ws(s.big[i & 15]).div_i32_wrap(black_box(s.word[i & 15]) as i32, Rounding::TowardZero)
            as u32
    });
    bench_u("div_i32_sat Nearest (64-bit numerator)", &mut |i| {
        ws(s.big[i & 15]).div_i32_sat(black_box(s.word[i & 15]) as i32, Rounding::Nearest) as u32
    });
    bench_u("div_i32_sat Floor (numerator < 2^32)", &mut |i| {
        ws(s.small[i & 15]).div_i32_sat(black_box(s.word[i & 15]) as i32, Rounding::Floor) as u32
    });
    bench_u("div_wide_sat Nearest (wide divisor)", &mut |i| {
        ws(s.big[i & 15]).div_wide_sat(ws(s.wide[i & 15]), Rounding::Nearest) as u32
    });
    bench_u("div_pow2", &mut |i| {
        ws(s.big[i & 15]).div_pow2(black_box(12)).low() as u32
    });
    black_box(sink);
}

#[no_mangle]
fn main() {
    let mut run = Run {
        cases: 0,
        failures: 0,
    };
    let mut rng = Rng(0x9e37_79b9);
    let mut vals = [R::new(0, 0); 200];
    values(&mut rng, &mut vals);
    unsigned_ops(&mut run, &vals);
    scale_ops(&mut run, &mut rng, &vals);
    signed_ops(&mut run, &mut rng, &vals);
    if run.failures == 0 {
        tty::print("WIDEDIV PASS cases=");
        tty::print_hex_u32(run.cases);
    } else {
        tty::print("WIDEDIV FAIL failures=");
        tty::print_hex_u32(run.failures);
        tty::print(" of=");
        tty::print_hex_u32(run.cases);
    }
    tty::println("");
    bench(&mut rng);
    tty::println("WIDEDIV DONE");
    loop {}
}
