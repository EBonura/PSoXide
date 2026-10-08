// SPDX-License-Identifier: GPL-2.0-or-later
//! BS ("bitstream") frame decode: variable-length codes to MDEC run-length
//! halfwords.
//!
//! A BS frame starts with an 8-byte header:
//!
//! | Bytes | Field |
//! |-------|-------|
//! | 0..2  | MDEC data size in 32-bit words (low half of the MDEC command) |
//! | 2..4  | `0x3800` (high half of a 15bpp decode command) |
//! | 4..6  | quantization scale, 1..63 |
//! | 6..8  | bitstream version (1, 2 or 3) |
//!
//! The body is read as little-endian 16-bit words, most significant bit
//! first. Macroblocks come in column-major order (top to bottom, then left
//! to right), six 8x8 blocks each: Cr, Cb, Y0..Y3. In versions 1 and 2 every
//! block starts with a raw 10-bit signed DC value (`0x1FF` there ends the
//! frame), followed by the AC run/level codes of the MPEG-1 DCT
//! coefficient table (ISO 11172-2 table B.5c, with the sign bit after the
//! code), a 6-bit escape `000001` followed by a raw 16-bit MDEC halfword,
//! and `10` for end of block.
//!
//! The MDEC wants, per block, `(qscale << 10) | dc` then one halfword per
//! non-zero AC coefficient (`run << 10 | level`, level 10-bit signed) and
//! `0xFE00` to end the block. The same `0xFE00` pads the tail.
//!
//! The code table is the one every BS encoder uses; the listing here was
//! cross-checked against psxavenc's encoder table (zlib license) and the
//! PSX-SPX description.

/// Fixed BS frame header size in bytes.
pub const HEADER_BYTES: usize = 8;
/// MDEC end-of-block / padding halfword.
pub const END_OF_BLOCK: u16 = 0xFE00;
/// Version-2 DC value that ends the frame.
const V2_END_OF_FRAME: u32 = 0x1FF;

/// Why a frame could not be decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Shorter than the 8-byte header.
    Truncated,
    /// Bitstream version other than 1 or 2.
    Version(u16),
    /// A bit pattern that is not in the code table.
    BadCode,
    /// The output buffer is too small for the frame.
    OutputFull,
    /// The decoder ran past the end of the input.
    Overrun,
}

/// Parsed frame header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// MDEC data size the encoder announced, in 32-bit words.
    pub mdec_words: u16,
    /// Quantization scale.
    pub qscale: u16,
    /// Bitstream version.
    pub version: u16,
}

impl Header {
    /// Parse the 8-byte header.
    pub fn parse(frame: &[u8]) -> Result<Self, DecodeError> {
        if frame.len() < HEADER_BYTES {
            return Err(DecodeError::Truncated);
        }
        let h = |i: usize| u16::from_le_bytes([frame[i], frame[i + 1]]);
        Ok(Header {
            mdec_words: h(0),
            qscale: h(4),
            version: h(6),
        })
    }
}

// Table entry layout: bits 0..16 MDEC halfword (positive level), 16..21 code
// length (without the sign bit), 24..27 kind.
const KIND_INVALID: u32 = 0;
const KIND_CODE: u32 = 1;
const KIND_ESCAPE: u32 = 2;
const KIND_EOB: u32 = 3;
const KIND_LONG: u32 = 4;

const fn entry(kind: u32, len: u32, hw: u32) -> u32 {
    (kind << 24) | (len << 16) | hw
}

/// (code length without sign, code, run, level) for every AC code.
/// `11` (run 0, level 1) and the end-of-block `10` share the top level.
const AC_CODES: [(u8, u16, u8, u8); 111] = [
    (2, 0x3, 0, 1),
    (3, 0x3, 1, 1),
    (4, 0x4, 0, 2),
    (4, 0x5, 2, 1),
    (5, 0x05, 0, 3),
    (5, 0x06, 4, 1),
    (5, 0x07, 3, 1),
    (6, 0x04, 7, 1),
    (6, 0x05, 6, 1),
    (6, 0x06, 1, 2),
    (6, 0x07, 5, 1),
    (7, 0x04, 2, 2),
    (7, 0x05, 9, 1),
    (7, 0x06, 0, 4),
    (7, 0x07, 8, 1),
    (8, 0x20, 13, 1),
    (8, 0x21, 0, 6),
    (8, 0x22, 12, 1),
    (8, 0x23, 11, 1),
    (8, 0x24, 3, 2),
    (8, 0x25, 1, 3),
    (8, 0x26, 0, 5),
    (8, 0x27, 10, 1),
    (10, 0x008, 16, 1),
    (10, 0x009, 5, 2),
    (10, 0x00A, 0, 7),
    (10, 0x00B, 2, 3),
    (10, 0x00C, 1, 4),
    (10, 0x00D, 15, 1),
    (10, 0x00E, 14, 1),
    (10, 0x00F, 4, 2),
    (12, 0x010, 0, 11),
    (12, 0x011, 8, 2),
    (12, 0x012, 4, 3),
    (12, 0x013, 0, 10),
    (12, 0x014, 2, 4),
    (12, 0x015, 7, 2),
    (12, 0x016, 21, 1),
    (12, 0x017, 20, 1),
    (12, 0x018, 0, 9),
    (12, 0x019, 19, 1),
    (12, 0x01A, 18, 1),
    (12, 0x01B, 1, 5),
    (12, 0x01C, 3, 3),
    (12, 0x01D, 0, 8),
    (12, 0x01E, 6, 2),
    (12, 0x01F, 17, 1),
    (13, 0x0010, 10, 2),
    (13, 0x0011, 9, 2),
    (13, 0x0012, 5, 3),
    (13, 0x0013, 3, 4),
    (13, 0x0014, 2, 5),
    (13, 0x0015, 1, 7),
    (13, 0x0016, 1, 6),
    (13, 0x0017, 0, 15),
    (13, 0x0018, 0, 14),
    (13, 0x0019, 0, 13),
    (13, 0x001A, 0, 12),
    (13, 0x001B, 26, 1),
    (13, 0x001C, 25, 1),
    (13, 0x001D, 24, 1),
    (13, 0x001E, 23, 1),
    (13, 0x001F, 22, 1),
    (14, 0x0010, 0, 31),
    (14, 0x0011, 0, 30),
    (14, 0x0012, 0, 29),
    (14, 0x0013, 0, 28),
    (14, 0x0014, 0, 27),
    (14, 0x0015, 0, 26),
    (14, 0x0016, 0, 25),
    (14, 0x0017, 0, 24),
    (14, 0x0018, 0, 23),
    (14, 0x0019, 0, 22),
    (14, 0x001A, 0, 21),
    (14, 0x001B, 0, 20),
    (14, 0x001C, 0, 19),
    (14, 0x001D, 0, 18),
    (14, 0x001E, 0, 17),
    (14, 0x001F, 0, 16),
    (15, 0x0010, 0, 40),
    (15, 0x0011, 0, 39),
    (15, 0x0012, 0, 38),
    (15, 0x0013, 0, 37),
    (15, 0x0014, 0, 36),
    (15, 0x0015, 0, 35),
    (15, 0x0016, 0, 34),
    (15, 0x0017, 0, 33),
    (15, 0x0018, 0, 32),
    (15, 0x0019, 1, 14),
    (15, 0x001A, 1, 13),
    (15, 0x001B, 1, 12),
    (15, 0x001C, 1, 11),
    (15, 0x001D, 1, 10),
    (15, 0x001E, 1, 9),
    (15, 0x001F, 1, 8),
    (16, 0x0010, 1, 18),
    (16, 0x0011, 1, 17),
    (16, 0x0012, 1, 16),
    (16, 0x0013, 1, 15),
    (16, 0x0014, 6, 3),
    (16, 0x0015, 16, 2),
    (16, 0x0016, 15, 2),
    (16, 0x0017, 14, 2),
    (16, 0x0018, 13, 2),
    (16, 0x0019, 12, 2),
    (16, 0x001A, 11, 2),
    (16, 0x001B, 31, 1),
    (16, 0x001C, 30, 1),
    (16, 0x001D, 29, 1),
    (16, 0x001E, 28, 1),
    (16, 0x001F, 27, 1),
];

/// First-level table, indexed by the next 8 bits. Covers every code of up
/// to 8 bits, the escape prefix, end of block, and routes codes with 6 or
/// more leading zeros to [`LONG`].
static SHORT: [u32; 256] = build_short();
/// Second level for codes with `lz` = 6..=11 leading zeros, indexed by
/// `(lz - 6) * 16` plus the 4 bits after the first one bit (lz 6 codes
/// have 3 such bits and fill two slots each).
static LONG: [u32; 96] = build_long();

const fn build_short() -> [u32; 256] {
    let mut t = [entry(KIND_INVALID, 0, 0); 256];
    // End of block `10`.
    let mut i = 0b1000_0000;
    while i < 0b1100_0000 {
        t[i] = entry(KIND_EOB, 2, 0);
        i += 1;
    }
    // Escape `000001`.
    let mut i = 0b0000_0100;
    while i < 0b0000_1000 {
        t[i] = entry(KIND_ESCAPE, 6, 0);
        i += 1;
    }
    // Six or more leading zeros: second level.
    let mut i = 0;
    while i < 0b0000_0100 {
        t[i] = entry(KIND_LONG, 0, 0);
        i += 1;
    }
    let mut c = 0;
    while c < AC_CODES.len() {
        let (len, code, run, level) = AC_CODES[c];
        if len <= 8 {
            let shift = 8 - len as usize;
            let base = (code as usize) << shift;
            let mut k = 0;
            while k < (1 << shift) {
                t[base + k] = entry(KIND_CODE, len as u32, ((run as u32) << 10) | level as u32);
                k += 1;
            }
        }
        c += 1;
    }
    t
}

const fn build_long() -> [u32; 96] {
    let mut t = [entry(KIND_INVALID, 0, 0); 96];
    let mut c = 0;
    while c < AC_CODES.len() {
        let (len, code, run, level) = AC_CODES[c];
        if len >= 10 {
            let significant = 16 - code.leading_zeros(); // bits from the top one
            let lz = len as u32 - significant;
            let suffix_bits = significant - 1;
            let suffix = (code as u32) & ((1 << suffix_bits) - 1);
            let base = ((lz - 6) * 16) as usize;
            let hw = ((run as u32) << 10) | level as u32;
            // Index by the 4 bits after the one bit; shorter suffixes fill
            // every slot that shares their prefix.
            let pad = 4 - suffix_bits;
            let first = (suffix << pad) as usize;
            let mut k = 0;
            while k < (1 << pad) {
                t[base + first + k] = entry(KIND_CODE, len as u32, hw);
                k += 1;
            }
        }
        c += 1;
    }
    t
}

/// Entry of [`FAST`], the table the decode loop uses: bit 31 set routes to
/// [`SHORT`]'s slower kinds (end of block, escape, long codes, invalid);
/// clear, bits 0..16 are the MDEC halfword for a positive level and bits
/// 16..21 the code length plus its sign bit.
static FAST: [u32; 256] = build_fast();

const fn build_fast() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let e = SHORT[i];
        t[i] = if e >> 24 == KIND_CODE {
            ((((e >> 16) & 0x1F) + 1) << 16) | (e & 0xFFFF)
        } else {
            0x8000_0000
        };
        i += 1;
    }
    t
}

/// MSB-first reader over the frame body's little-endian 16-bit words, as
/// plain values so the decode loop keeps all of it in registers.
///
/// `ALIGNED` reads whole halfwords (one `lhu` on the R3000); a streaming
/// player's frame buffers are always word aligned, and the byte path only
/// exists so a caller's odd-addressed slice still decodes.
#[derive(Clone, Copy)]
struct Bits<const ALIGNED: bool> {
    /// Next halfword to load, and one past the last whole halfword (a
    /// trailing odd byte reads as zero).
    p: *const u8,
    end: *const u8,
    /// Pending bits, left-aligned.
    buf: u32,
    /// Valid bits in `buf`.
    avail: u32,
}

impl<const ALIGNED: bool> Bits<ALIGNED> {
    fn new(data: &[u8]) -> Self {
        let p = data.as_ptr();
        let mut b = Bits {
            p,
            end: p.wrapping_add(data.len() & !1),
            buf: 0,
            avail: 0,
        };
        b.refill();
        b
    }

    /// Top up to at least 17 valid bits. Reads past the end yield zeros;
    /// the caller's overrun check reports it.
    #[inline(always)]
    fn refill(&mut self) {
        while self.avail <= 16 {
            let hw = if self.p < self.end {
                // SAFETY: `p` is below `end`, so both bytes are in the slice;
                // the aligned path is taken only for a 2-aligned start.
                unsafe {
                    if ALIGNED {
                        u32::from(u16::from_le((self.p as *const u16).read()))
                    } else {
                        u32::from(*self.p) | u32::from(*self.p.add(1)) << 8
                    }
                }
            } else {
                0
            };
            self.p = self.p.wrapping_add(2);
            self.buf |= hw << (16 - self.avail);
            self.avail += 16;
        }
    }

    #[inline(always)]
    fn skip(&mut self, n: u32) {
        self.buf <<= n;
        self.avail -= n;
        self.refill();
    }

    /// Read `n` (1..=16) bits.
    #[inline(always)]
    fn read(&mut self, n: u32) -> u32 {
        let v = self.buf >> (32 - n);
        self.skip(n);
        v
    }
}

/// Decode one version-1 or version-2 BS frame (header included) into MDEC
/// halfwords. The two versions code the bitstream the same way: version 1
/// is what the earliest (1995) Sony encoder wrote, WipEout's intro among
/// them.
///
/// Stops at the end-of-frame code or after `max_macroblocks`, whichever
/// comes first, then pads the output with [`END_OF_BLOCK`] to a multiple of
/// 64 halfwords (32-word DMA blocks) and at least up to the size the header
/// announced. Returns the number of 32-bit words to send.
///
/// `pump` runs after every `pump_every` macroblocks (one column when set to
/// `height / 16`). A streaming player drains CD sectors there so the drive
/// never runs ahead of the decoder.
pub fn decode_frame(
    frame: &[u8],
    out: &mut [u16],
    max_macroblocks: u32,
    pump_every: u32,
    pump: &mut impl FnMut(),
) -> Result<usize, DecodeError> {
    let header = Header::parse(frame)?;
    if !(1..=2).contains(&header.version) {
        return Err(DecodeError::Version(header.version));
    }
    let body = &frame[HEADER_BYTES..];
    let mut pump = || cold_pump(pump);
    let n = if (body.as_ptr() as usize).is_multiple_of(2) {
        decode_body::<true>(
            body,
            header.qscale,
            out,
            max_macroblocks,
            pump_every,
            &mut pump,
        )?
    } else {
        decode_body::<false>(
            body,
            header.qscale,
            out,
            max_macroblocks,
            pump_every,
            &mut pump,
        )?
    };

    let announced = header.mdec_words as usize * 2;
    let mut padded = (n + 63) & !63;
    if padded < announced {
        padded = (announced + 63) & !63;
    }
    if padded > out.len() {
        return Err(DecodeError::OutputFull);
    }
    for slot in &mut out[n..padded] {
        *slot = END_OF_BLOCK;
    }
    Ok(padded / 2)
}

/// The caller's pump, kept out of line so its code and registers stay out
/// of the decode loop.
#[cold]
#[inline(never)]
fn cold_pump(pump: &mut impl FnMut()) {
    pump();
}

/// The macroblock loop of [`decode_frame`]; returns the halfwords written.
///
/// This is the per-frame hot path of a player on a 33 MHz R3000 without a
/// data cache, where every spilled register costs a RAM access. So the state
/// is a handful of plain locals (bit buffer, input and output cursors), the
/// common code costs one table load with its sign folded into the same
/// shift, and the output has one bound check per halfword.
#[inline(never)]
fn decode_body<const ALIGNED: bool>(
    body: &[u8],
    qscale: u16,
    out: &mut [u16],
    max_macroblocks: u32,
    pump_every: u32,
    pump: &mut impl FnMut(),
) -> Result<usize, DecodeError> {
    let qscale = ((qscale as u32) & 0x3F) << 10;
    let mut bits = Bits::<ALIGNED>::new(body);
    // Past this, the reader has run more than the two look-ahead words off
    // the end of the input.
    let overrun = body.as_ptr().wrapping_add((body.len() + 4) & !1);
    let o0 = out.as_mut_ptr();
    let oend = o0.wrapping_add(out.len());
    let mut o = o0;
    let mut mb = 0u32;
    let mut since_pump = 0u32;

    // SAFETY (every write below): `o` only advances after a check that it is
    // still below `oend`, one past the end of `out`.
    'frame: while mb < max_macroblocks {
        for block in 0..6 {
            let dc = bits.read(10);
            if dc == V2_END_OF_FRAME && block == 0 {
                break 'frame;
            }
            if o == oend {
                return Err(DecodeError::OutputFull);
            }
            // SAFETY: `o` is below `oend` (checked just above).
            unsafe { o.write((qscale | dc) as u16) };
            o = o.wrapping_add(1);
            loop {
                let e = FAST[(bits.buf >> 24) as usize];
                let hw = if (e as i32) >= 0 {
                    // A code of up to 8 bits, then its sign bit.
                    let used = e >> 16;
                    let sign = ((bits.buf << (used - 1)) as i32 >> 31) as u32;
                    bits.skip(used);
                    let level = ((e & 0x3FF) ^ sign).wrapping_sub(sign) & 0x3FF;
                    (e & 0xFC00) | level
                } else {
                    match slow_code(&mut bits) {
                        Ok(Some(hw)) => hw,
                        Ok(None) => {
                            if o == oend {
                                return Err(DecodeError::OutputFull);
                            }
                            // SAFETY: `o` is below `oend` (checked just above).
                            unsafe { o.write(END_OF_BLOCK) };
                            o = o.wrapping_add(1);
                            break;
                        }
                        Err(e) => return Err(e),
                    }
                };
                if o == oend {
                    return Err(DecodeError::OutputFull);
                }
                // SAFETY: `o` is below `oend` (checked just above).
                unsafe { o.write(hw as u16) };
                o = o.wrapping_add(1);
            }
            if bits.p > overrun {
                return Err(DecodeError::Overrun);
            }
        }
        mb += 1;
        since_pump += 1;
        if since_pump == pump_every {
            since_pump = 0;
            pump();
        }
    }
    // SAFETY: `o` walked forward from `o0` inside `out`.
    Ok(unsafe { o.offset_from(o0) } as usize)
}

/// The codes [`FAST`] does not cover: end of block (`None`), the escape,
/// codes of 10 bits and more, and invalid ones. Inlined: an out-of-line call
/// would take the bit reader's address and pin it to the stack.
#[inline(always)]
fn slow_code<const ALIGNED: bool>(bits: &mut Bits<ALIGNED>) -> Result<Option<u32>, DecodeError> {
    match SHORT[(bits.buf >> 24) as usize] >> 24 {
        KIND_EOB => {
            bits.skip(2);
            Ok(None)
        }
        KIND_ESCAPE => {
            bits.skip(6);
            Ok(Some(bits.read(16)))
        }
        KIND_LONG => {
            let lz = bits.buf.leading_zeros();
            if !(6..=11).contains(&lz) {
                return Err(DecodeError::BadCode);
            }
            let next4 = (bits.buf << (lz + 1)) >> 28;
            let e2 = LONG[((lz - 6) * 16 + next4) as usize];
            if e2 >> 24 != KIND_CODE {
                return Err(DecodeError::BadCode);
            }
            bits.skip((e2 >> 16) & 0x1F);
            let negative = bits.read(1) != 0;
            let hw = e2 & 0xFFFF;
            Ok(Some(if negative {
                (hw & 0xFC00) | ((0u32.wrapping_sub(hw & 0x3FF)) & 0x3FF)
            } else {
                hw
            }))
        }
        _ => Err(DecodeError::BadCode),
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    /// The decoder as it was before the R3000 rewrite, kept as the oracle
    /// the rewrite must match halfword for halfword.
    mod reference {
        use super::super::*;

        struct Bits<'a> {
            data: &'a [u8],
            pos: usize,
            buf: u32,
            avail: u32,
        }

        impl<'a> Bits<'a> {
            fn new(data: &'a [u8]) -> Self {
                let mut b = Bits {
                    data,
                    pos: 0,
                    buf: 0,
                    avail: 0,
                };
                b.refill();
                b
            }
            fn refill(&mut self) {
                while self.avail <= 16 {
                    let hw = if self.pos + 1 < self.data.len() {
                        self.data[self.pos] as u32 | (self.data[self.pos + 1] as u32) << 8
                    } else {
                        0
                    };
                    self.pos += 2;
                    self.buf |= hw << (16 - self.avail);
                    self.avail += 16;
                }
            }
            fn skip(&mut self, n: u32) {
                self.buf <<= n;
                self.avail -= n;
                self.refill();
            }
            fn read(&mut self, n: u32) -> u32 {
                let v = self.buf >> (32 - n);
                self.skip(n);
                v
            }
            fn overrun(&self) -> bool {
                self.pos > self.data.len() + 4
            }
        }

        pub fn decode_frame(
            frame: &[u8],
            out: &mut [u16],
            max_macroblocks: u32,
        ) -> Result<usize, DecodeError> {
            let header = Header::parse(frame)?;
            if !(1..=2).contains(&header.version) {
                return Err(DecodeError::Version(header.version));
            }
            let qscale = ((header.qscale as u32) & 0x3F) << 10;
            let mut bits = Bits::new(&frame[HEADER_BYTES..]);
            let mut n = 0usize;
            let mut mb = 0u32;
            'frame: while mb < max_macroblocks {
                for block in 0..6 {
                    let dc = bits.read(10);
                    if dc == V2_END_OF_FRAME && block == 0 {
                        break 'frame;
                    }
                    if n >= out.len() {
                        return Err(DecodeError::OutputFull);
                    }
                    out[n] = (qscale | dc) as u16;
                    n += 1;
                    loop {
                        let e = SHORT[(bits.buf >> 24) as usize];
                        let (len, hw) = match e >> 24 {
                            KIND_EOB => {
                                bits.skip(2);
                                if n >= out.len() {
                                    return Err(DecodeError::OutputFull);
                                }
                                out[n] = END_OF_BLOCK;
                                n += 1;
                                break;
                            }
                            KIND_CODE => ((e >> 16) & 0x1F, e & 0xFFFF),
                            KIND_ESCAPE => {
                                bits.skip(6);
                                let raw = bits.read(16);
                                if n >= out.len() {
                                    return Err(DecodeError::OutputFull);
                                }
                                out[n] = raw as u16;
                                n += 1;
                                continue;
                            }
                            KIND_LONG => {
                                let lz = bits.buf.leading_zeros();
                                if !(6..=11).contains(&lz) {
                                    return Err(DecodeError::BadCode);
                                }
                                let next4 = (bits.buf << (lz + 1)) >> 28;
                                let e2 = LONG[((lz - 6) * 16 + next4) as usize];
                                if e2 >> 24 != KIND_CODE {
                                    return Err(DecodeError::BadCode);
                                }
                                ((e2 >> 16) & 0x1F, e2 & 0xFFFF)
                            }
                            _ => return Err(DecodeError::BadCode),
                        };
                        bits.skip(len);
                        let negative = bits.read(1) != 0;
                        let hw = if negative {
                            (hw & 0xFC00) | ((0u32.wrapping_sub(hw & 0x3FF)) & 0x3FF)
                        } else {
                            hw
                        };
                        if n >= out.len() {
                            return Err(DecodeError::OutputFull);
                        }
                        out[n] = hw as u16;
                        n += 1;
                    }
                    if bits.overrun() {
                        return Err(DecodeError::Overrun);
                    }
                }
                mb += 1;
            }
            let announced = header.mdec_words as usize * 2;
            let mut padded = (n + 63) & !63;
            if padded < announced {
                padded = (announced + 63) & !63;
            }
            if padded > out.len() {
                return Err(DecodeError::OutputFull);
            }
            for slot in &mut out[n..padded] {
                *slot = END_OF_BLOCK;
            }
            Ok(padded / 2)
        }
    }

    /// Both decoders on one input: same result, same halfwords.
    fn same_as_reference(frame: &[u8], out_len: usize, mbs: u32) {
        let mut a = std::vec![0u16; out_len];
        let mut b = std::vec![0u16; out_len];
        let ra = reference::decode_frame(frame, &mut a, mbs);
        let rb = decode_frame(frame, &mut b, mbs, 1, &mut || {});
        assert_eq!(ra, rb);
        if let Ok(words) = ra {
            assert_eq!(a[..words * 2], b[..words * 2]);
        }
    }

    #[test]
    fn rewrite_matches_reference_on_random_streams() {
        // Random bodies hit every code, escape, long code, bad code, output
        // bound and overrun; small xorshift so the test needs no crate.
        let mut x = 0x2545_F491u32;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x
        };
        for case in 0..4000 {
            let body = 8 + (next() % 400) as usize;
            let mut frame = std::vec![0u8; HEADER_BYTES + body];
            frame[0..2].copy_from_slice(&((next() % 64) as u16).to_le_bytes());
            frame[2..4].copy_from_slice(&0x3800u16.to_le_bytes());
            frame[4..6].copy_from_slice(&((next() % 64) as u16).to_le_bytes());
            frame[6..8].copy_from_slice(&2u16.to_le_bytes());
            for byte in &mut frame[HEADER_BYTES..] {
                // Bias towards zero bits so long codes and escapes show up.
                *byte = (next() & next()) as u8;
            }
            let out_len = [64, 256, 4096][case % 3];
            same_as_reference(&frame, out_len, 1 + next() % 8);
            // The byte path, from an odd address.
            let mut shifted = std::vec![0u8; frame.len() + 1];
            shifted[1..].copy_from_slice(&frame);
            same_as_reference(&shifted[1..], out_len, 4);
        }
    }

    /// Every frame of a real movie, when `PSX_FMV_TEST_STR` names one: a
    /// 2048-byte-sector `.str` (psxavenc `-t strv`) or one with 2336-byte
    /// XA sectors (`-t str`, what `tools/fmv_test_movie.py` writes).
    #[test]
    fn rewrite_matches_reference_on_a_movie() {
        let Some(path) = std::env::var_os("PSX_FMV_TEST_STR") else {
            return;
        };
        let data = std::fs::read(path).unwrap();
        let (size, skip) = if data.len() % 2336 == 0 && data.len() % 2048 != 0 {
            (2336, 8)
        } else {
            (2048, 0)
        };
        let mut asm = crate::stream::FrameAssembler::new();
        let mut buf = std::vec![0u32; 16 * 1024];
        let mut frames = 0;
        for sector in data.chunks_exact(size) {
            let payload = &sector[skip..skip + 2048];
            // SAFETY: a u32 buffer viewed as bytes, for the word alignment
            // the device's slots have.
            let bytes = unsafe {
                core::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 4)
            };
            if let Some(frame) = asm.add(payload, bytes) {
                same_as_reference(&bytes[..frame.size as usize], 32 * 1024, 300);
                frames += 1;
            }
        }
        assert!(frames > 0, "no frames in the movie");
        std::eprintln!("{frames} frames match");
    }

    /// MSB-first bit writer producing little-endian 16-bit words, the
    /// layout the encoder emits.
    struct Writer {
        words: [u16; 64],
        n: usize,
        acc: u32,
        used: u32,
    }

    impl Writer {
        fn new() -> Self {
            Writer {
                words: [0; 64],
                n: 0,
                acc: 0,
                used: 0,
            }
        }
        fn put(&mut self, len: u32, value: u32) {
            for i in (0..len).rev() {
                self.acc = (self.acc << 1) | ((value >> i) & 1);
                self.used += 1;
                if self.used == 16 {
                    self.words[self.n] = self.acc as u16;
                    self.n += 1;
                    self.acc = 0;
                    self.used = 0;
                }
            }
        }
        fn frame(mut self, qscale: u16) -> ([u8; 136], usize) {
            if self.used > 0 {
                let pad = 16 - self.used;
                self.put(pad, 0);
            }
            let mut out = [0u8; 136];
            out[0..2].copy_from_slice(&0x0020u16.to_le_bytes());
            out[2..4].copy_from_slice(&0x3800u16.to_le_bytes());
            out[4..6].copy_from_slice(&qscale.to_le_bytes());
            out[6..8].copy_from_slice(&2u16.to_le_bytes());
            for i in 0..self.n {
                out[8 + 2 * i..10 + 2 * i].copy_from_slice(&self.words[i].to_le_bytes());
            }
            (out, 8 + 2 * self.n)
        }
    }

    fn decode(frame: &[u8], mbs: u32) -> ([u16; 256], usize) {
        let mut out = [0u16; 256];
        let words = decode_frame(frame, &mut out, mbs, 1, &mut || {}).unwrap();
        (out, words)
    }

    #[test]
    fn every_table_code_round_trips() {
        // One macroblock: block 0 carries every code once (positive, then
        // negative for odd entries), blocks 1..5 are DC + EOB.
        let mut w = Writer::new();
        w.put(10, 0x123);
        for (i, &(len, code, _, _)) in AC_CODES.iter().enumerate().take(20) {
            w.put(len as u32, code as u32);
            w.put(1, (i & 1) as u32);
        }
        w.put(2, 0b10);
        for _ in 1..6 {
            w.put(10, 0x3FE); // DC -2
            w.put(2, 0b10);
        }
        w.put(10, 0x1FF);
        let (frame, len) = w.frame(5);
        let (out, words) = decode(&frame[..len], 16);
        assert_eq!(out[0], (5 << 10) | 0x123);
        for (i, &(_, _, run, level)) in AC_CODES.iter().enumerate().take(20) {
            let lv = if i & 1 == 1 {
                (-(level as i32)) as u32 & 0x3FF
            } else {
                level as u32
            };
            assert_eq!(out[1 + i] as u32, ((run as u32) << 10) | lv, "code {i}");
        }
        assert_eq!(out[21], END_OF_BLOCK);
        assert_eq!(out[22], (5 << 10) | 0x3FE);
        assert_eq!(words, 32);
        assert!(out[33..64].iter().all(|&h| h == END_OF_BLOCK));
    }

    #[test]
    fn long_codes_and_escape_decode() {
        let mut w = Writer::new();
        w.put(10, 0);
        // Every code of 10+ bits, alternating sign.
        let long: [usize; 4] = [23, 31, 63, 110];
        for (k, &i) in long.iter().enumerate() {
            let (len, code, _, _) = AC_CODES[i];
            w.put(len as u32, code as u32);
            w.put(1, (k & 1) as u32);
        }
        w.put(6, 0b000001);
        w.put(16, (40 << 10) | 0x155);
        w.put(2, 0b10);
        for _ in 1..6 {
            w.put(10, 0);
            w.put(2, 0b10);
        }
        let (frame, len) = w.frame(1);
        let (out, _) = decode(&frame[..len], 1);
        for (k, &i) in long.iter().enumerate() {
            let (_, _, run, level) = AC_CODES[i];
            let lv = if k & 1 == 1 {
                (-(level as i32)) as u32 & 0x3FF
            } else {
                level as u32
            };
            assert_eq!(out[1 + k] as u32, ((run as u32) << 10) | lv, "entry {i}");
        }
        assert_eq!(out[5], (40 << 10) | 0x155);
        assert_eq!(out[6], END_OF_BLOCK);
    }

    #[test]
    fn version_1_decodes_like_version_2() {
        let mut w = Writer::new();
        for block in 0..6u32 {
            w.put(10, 0x1F0 + block);
            let (len, code, _, _) = AC_CODES[block as usize * 7];
            w.put(len as u32, code as u32);
            w.put(1, block & 1);
            w.put(2, 0b10);
        }
        let (mut frame, len) = w.frame(3);
        let v2 = decode(&frame[..len], 1);
        frame[6..8].copy_from_slice(&1u16.to_le_bytes());
        assert_eq!(decode(&frame[..len], 1), v2);
    }

    #[test]
    fn table_is_prefix_free_and_complete() {
        // Every AC code must land in exactly the slots it owns.
        let mut seen = 0;
        for &(len, code, run, level) in AC_CODES.iter() {
            let hw = ((run as u32) << 10) | level as u32;
            let e = if len <= 8 {
                SHORT[(code as usize) << (8 - len)]
            } else {
                let significant = 16 - code.leading_zeros();
                let lz = len as u32 - significant;
                let suffix_bits = significant - 1;
                let suffix = code as u32 & ((1 << suffix_bits) - 1);
                LONG[((lz - 6) * 16 + (suffix << (4 - suffix_bits))) as usize]
            };
            assert_eq!(e & 0xFFFF, hw, "code len {len} value {code:#x}");
            assert_eq!((e >> 16) & 0x1F, len as u32);
            seen += 1;
        }
        assert_eq!(seen, 111);
    }

    #[test]
    fn rejects_v3_and_short_input() {
        assert_eq!(Header::parse(&[0; 4]), Err(DecodeError::Truncated));
        let mut f = [0u8; 16];
        f[6] = 3;
        let mut out = [0u16; 64];
        assert_eq!(
            decode_frame(&f, &mut out, 1, 1, &mut || {}),
            Err(DecodeError::Version(3))
        );
    }
}
