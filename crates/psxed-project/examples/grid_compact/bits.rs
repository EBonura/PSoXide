//! Entropy-coding backends for the prototype codec.
//!
//! One traversal (`codec::code_room`) runs against any [`Coder`]: the range
//! coder (adaptive binary contexts, LZMA style) or the bit packer (fixed
//! widths, contexts ignored). Encoding and decoding share the traversal, so
//! the decoder cannot drift from the encoder.

pub const MAX_LABELS: usize = 16;

pub trait Coder {
    fn encoding(&self) -> bool;
    /// Code one binary decision under context `ctx`; returns the real bit.
    fn bit(&mut self, ctx: usize, v: bool) -> bool;
    /// Code `n` (at most 24) equiprobable bits.
    fn raw(&mut self, n: u32, v: u32) -> u32;
    fn label(&mut self, l: usize);
    /// Forget adaptive state (independent units); the byte stream goes on.
    fn reset(&mut self) {}
}

/// Decode-side work counters (what an R3000 decoder would execute).
#[derive(Clone, Copy, Debug, Default)]
pub struct Ops {
    pub decisions: u64,
    pub raw_bits: u64,
}

// -------------------------------------------------------------------------
// Range coder.
// -------------------------------------------------------------------------

const PROB_BITS: u32 = 11;
const PROB_INIT: u16 = 1 << (PROB_BITS - 1);

pub struct RcEnc {
    low: u64,
    range: u32,
    cache: u8,
    cache_size: u64,
    pub out: Vec<u8>,
    probs: Vec<u16>,
    shift: u32,
    label: usize,
    pub cost: [f64; MAX_LABELS],
    pub ops: Ops,
}

impl RcEnc {
    pub fn new(shift: u32) -> Self {
        Self {
            low: 0,
            range: 0xFFFF_FFFF,
            cache: 0,
            cache_size: 1,
            out: Vec::new(),
            probs: Vec::new(),
            shift,
            label: 0,
            cost: [0.0; MAX_LABELS],
            ops: Ops::default(),
        }
    }

    fn shift_low(&mut self) {
        if (self.low as u32) < 0xFF00_0000 || (self.low >> 32) != 0 {
            let carry = (self.low >> 32) as u8;
            let mut temp = self.cache;
            loop {
                self.out.push(temp.wrapping_add(carry));
                temp = 0xFF;
                self.cache_size -= 1;
                if self.cache_size == 0 {
                    break;
                }
            }
            self.cache = ((self.low >> 24) & 0xFF) as u8;
        }
        self.cache_size += 1;
        self.low = (self.low & 0x00FF_FFFF) << 8;
    }

    pub fn finish(mut self) -> (Vec<u8>, [f64; MAX_LABELS], Ops) {
        for _ in 0..5 {
            self.shift_low();
        }
        (self.out, self.cost, self.ops)
    }
}

impl Coder for RcEnc {
    fn encoding(&self) -> bool {
        true
    }
    fn bit(&mut self, ctx: usize, v: bool) -> bool {
        if ctx >= self.probs.len() {
            self.probs.resize(ctx + 1, PROB_INIT);
        }
        self.ops.decisions += 1;
        let p = self.probs[ctx];
        let bound = (self.range >> PROB_BITS) * u32::from(p);
        let total = f64::from(1u32 << PROB_BITS);
        if !v {
            self.range = bound;
            self.probs[ctx] = p + (((1 << PROB_BITS) - p) >> self.shift);
            self.cost[self.label] -= (f64::from(p) / total).log2();
        } else {
            self.low += u64::from(bound);
            self.range -= bound;
            self.probs[ctx] = p - (p >> self.shift);
            self.cost[self.label] -= ((total - f64::from(p)) / total).log2();
        }
        while self.range < (1 << 24) {
            self.range <<= 8;
            self.shift_low();
        }
        v
    }
    fn raw(&mut self, n: u32, v: u32) -> u32 {
        self.ops.raw_bits += u64::from(n);
        self.cost[self.label] += f64::from(n);
        for i in (0..n).rev() {
            self.range >>= 1;
            if (v >> i) & 1 != 0 {
                self.low += u64::from(self.range);
            }
            while self.range < (1 << 24) {
                self.range <<= 8;
                self.shift_low();
            }
        }
        v
    }
    fn label(&mut self, l: usize) {
        self.label = l;
    }
    fn reset(&mut self) {
        self.probs.clear();
    }
}

pub struct RcDec<'a> {
    data: &'a [u8],
    pos: usize,
    range: u32,
    code: u32,
    probs: Vec<u16>,
    shift: u32,
    pub ops: Ops,
}

impl<'a> RcDec<'a> {
    pub fn new(data: &'a [u8], shift: u32) -> Self {
        let mut d = Self {
            data,
            pos: 1,
            range: 0xFFFF_FFFF,
            code: 0,
            probs: Vec::new(),
            shift,
            ops: Ops::default(),
        };
        for _ in 0..4 {
            d.code = (d.code << 8) | u32::from(d.next());
        }
        d
    }
    fn next(&mut self) -> u8 {
        let b = self.data.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        b
    }
}

impl Coder for RcDec<'_> {
    fn encoding(&self) -> bool {
        false
    }
    fn bit(&mut self, ctx: usize, _v: bool) -> bool {
        if ctx >= self.probs.len() {
            self.probs.resize(ctx + 1, PROB_INIT);
        }
        self.ops.decisions += 1;
        let p = self.probs[ctx];
        let bound = (self.range >> PROB_BITS) * u32::from(p);
        let bit;
        if self.code < bound {
            self.range = bound;
            self.probs[ctx] = p + (((1 << PROB_BITS) - p) >> self.shift);
            bit = false;
        } else {
            self.code -= bound;
            self.range -= bound;
            self.probs[ctx] = p - (p >> self.shift);
            bit = true;
        }
        while self.range < (1 << 24) {
            self.range <<= 8;
            self.code = (self.code << 8) | u32::from(self.next());
        }
        bit
    }
    fn raw(&mut self, n: u32, _v: u32) -> u32 {
        self.ops.raw_bits += u64::from(n);
        let mut out = 0;
        for _ in 0..n {
            self.range >>= 1;
            let bit = if self.code >= self.range {
                self.code -= self.range;
                1
            } else {
                0
            };
            out = (out << 1) | bit;
            while self.range < (1 << 24) {
                self.range <<= 8;
                self.code = (self.code << 8) | u32::from(self.next());
            }
        }
        out
    }
    fn label(&mut self, _l: usize) {}
}

// -------------------------------------------------------------------------
// Bit packer: every decision costs one bit, contexts are ignored.
// -------------------------------------------------------------------------

#[derive(Default)]
pub struct PackEnc {
    pub bits: Vec<bool>,
    label: usize,
    pub cost: [f64; MAX_LABELS],
    pub ops: Ops,
}

impl Coder for PackEnc {
    fn encoding(&self) -> bool {
        true
    }
    fn bit(&mut self, _ctx: usize, v: bool) -> bool {
        self.ops.decisions += 1;
        self.bits.push(v);
        self.cost[self.label] += 1.0;
        v
    }
    fn raw(&mut self, n: u32, v: u32) -> u32 {
        self.ops.raw_bits += u64::from(n);
        for i in (0..n).rev() {
            self.bits.push((v >> i) & 1 != 0);
        }
        self.cost[self.label] += f64::from(n);
        v
    }
    fn label(&mut self, l: usize) {
        self.label = l;
    }
}

pub struct PackDec<'a> {
    bits: &'a [bool],
    pos: usize,
    pub ops: Ops,
}

impl<'a> PackDec<'a> {
    pub fn new(bits: &'a [bool]) -> Self {
        Self {
            bits,
            pos: 0,
            ops: Ops::default(),
        }
    }
}

impl Coder for PackDec<'_> {
    fn encoding(&self) -> bool {
        false
    }
    fn bit(&mut self, _ctx: usize, _v: bool) -> bool {
        self.ops.decisions += 1;
        self.pos += 1;
        self.bits[self.pos - 1]
    }
    fn raw(&mut self, n: u32, _v: u32) -> u32 {
        self.ops.raw_bits += u64::from(n);
        let mut out = 0;
        for _ in 0..n {
            out = (out << 1) | u32::from(self.bits[self.pos]);
            self.pos += 1;
        }
        out
    }
    fn label(&mut self, _l: usize) {}
}

// -------------------------------------------------------------------------
// Symbol helpers shared by both backends. `base` is a context offset the
// caller allocates with [`Alloc`]; the same allocations happen on both sides.
// -------------------------------------------------------------------------

#[derive(Default)]
pub struct Alloc(pub usize);

impl Alloc {
    pub fn take(&mut self, n: usize) -> usize {
        let at = self.0;
        self.0 += n;
        at
    }
}

/// An `nbits` wide symbol through a binary tree of contexts at `base`
/// (needs `1 << nbits` contexts).
pub fn tree<C: Coder>(c: &mut C, base: usize, nbits: u32, v: u32) -> u32 {
    let mut node = 1usize;
    for i in (0..nbits).rev() {
        let b = c.bit(base + node, (v >> i) & 1 != 0);
        node = (node << 1) | usize::from(b);
    }
    (node - (1usize << nbits)) as u32
}

/// Unsigned Exp-Golomb with adaptive prefix bits (needs 20 contexts).
pub fn eg<C: Coder>(c: &mut C, base: usize, v: u32) -> u32 {
    let enc = c.encoding();
    let vv = v + 1;
    let k = if enc { 31 - vv.leading_zeros() } else { 0 };
    let mut n = 0u32;
    loop {
        let more = c.bit(base + (n as usize).min(15), enc && n < k);
        if !more {
            break;
        }
        n += 1;
    }
    let mant = c.raw(n, if enc { vv & ((1u32 << n) - 1) } else { 0 });
    ((1u32 << n) | mant) - 1
}

/// Signed value: zero flag, sign, magnitude minus one (needs 24 contexts).
pub fn seg<C: Coder>(c: &mut C, base: usize, v: i32) -> i32 {
    let nz = c.bit(base, v != 0);
    if !nz {
        return 0;
    }
    let neg = c.bit(base + 1, v < 0);
    let m = eg(c, base + 2, v.unsigned_abs().wrapping_sub(1));
    let m = m as i32 + 1;
    if neg {
        -m
    } else {
        m
    }
}

pub const EG_CTX: usize = 16;
pub const SEG_CTX: usize = 2 + EG_CTX;
