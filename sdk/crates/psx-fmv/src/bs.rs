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
//! | 6..8  | bitstream version (2 or 3) |
//!
//! The body is read as little-endian 16-bit words, most significant bit
//! first. Macroblocks come in column-major order (top to bottom, then left
//! to right), six 8x8 blocks each: Cr, Cb, Y0..Y3. In version 2 every
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
pub enum BsError {
    /// Shorter than the 8-byte header.
    Truncated,
    /// Bitstream version other than 2.
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
    pub fn parse(frame: &[u8]) -> Result<Self, BsError> {
        if frame.len() < HEADER_BYTES {
            return Err(BsError::Truncated);
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
            let significant = 16 - (code as u16).leading_zeros(); // bits from the top one
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

/// MSB-first reader over little-endian 16-bit words.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    /// Pending bits, left-aligned.
    buf: u32,
    /// Valid bits in `buf`.
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

    /// Top up to at least 17 valid bits. Reads past the end yield zeros;
    /// [`Self::overrun`] reports it.
    #[inline(always)]
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

    fn overrun(&self) -> bool {
        // Two words of zero fill are the refill's own look-ahead.
        self.pos > self.data.len() + 4
    }
}

/// Decode one version-2 BS frame (header included) into MDEC halfwords.
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
) -> Result<usize, BsError> {
    let header = Header::parse(frame)?;
    if header.version != 2 {
        return Err(BsError::Version(header.version));
    }
    let qscale = ((header.qscale as u32) & 0x3F) << 10;
    let mut bits = Bits::new(&frame[HEADER_BYTES..]);
    let mut n = 0usize;
    let mut mb = 0u32;
    let mut since_pump = 0u32;

    'frame: while mb < max_macroblocks {
        for block in 0..6 {
            let dc = bits.read(10);
            if dc == V2_END_OF_FRAME && block == 0 {
                break 'frame;
            }
            if n >= out.len() {
                return Err(BsError::OutputFull);
            }
            out[n] = (qscale | dc) as u16;
            n += 1;
            loop {
                let e = SHORT[(bits.buf >> 24) as usize];
                let (len, hw) = match e >> 24 {
                    KIND_EOB => {
                        bits.skip(2);
                        if n >= out.len() {
                            return Err(BsError::OutputFull);
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
                            return Err(BsError::OutputFull);
                        }
                        out[n] = raw as u16;
                        n += 1;
                        continue;
                    }
                    KIND_LONG => {
                        let lz = bits.buf.leading_zeros();
                        if !(6..=11).contains(&lz) {
                            return Err(BsError::BadCode);
                        }
                        let next4 = (bits.buf << (lz + 1)) >> 28;
                        let e2 = LONG[((lz - 6) * 16 + next4) as usize];
                        if e2 >> 24 != KIND_CODE {
                            return Err(BsError::BadCode);
                        }
                        ((e2 >> 16) & 0x1F, e2 & 0xFFFF)
                    }
                    _ => return Err(BsError::BadCode),
                };
                bits.skip(len);
                let negative = bits.read(1) != 0;
                let hw = if negative {
                    (hw & 0xFC00) | ((0u32.wrapping_sub(hw & 0x3FF)) & 0x3FF)
                } else {
                    hw
                };
                if n >= out.len() {
                    return Err(BsError::OutputFull);
                }
                out[n] = hw as u16;
                n += 1;
            }
            if bits.overrun() {
                return Err(BsError::Overrun);
            }
        }
        mb += 1;
        since_pump += 1;
        if since_pump == pump_every {
            since_pump = 0;
            pump();
        }
    }

    let announced = header.mdec_words as usize * 2;
    let mut padded = (n + 63) & !63;
    if padded < announced {
        padded = (announced + 63) & !63;
    }
    if padded > out.len() {
        return Err(BsError::OutputFull);
    }
    for slot in &mut out[n..padded] {
        *slot = END_OF_BLOCK;
    }
    Ok(padded / 2)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn table_is_prefix_free_and_complete() {
        // Every AC code must land in exactly the slots it owns.
        let mut seen = 0;
        for &(len, code, run, level) in AC_CODES.iter() {
            let hw = ((run as u32) << 10) | level as u32;
            let e = if len <= 8 {
                SHORT[((code as usize) << (8 - len)) as usize]
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
        assert_eq!(Header::parse(&[0; 4]), Err(BsError::Truncated));
        let mut f = [0u8; 16];
        f[6] = 3;
        let mut out = [0u16; 64];
        assert_eq!(
            decode_frame(&f, &mut out, 1, 1, &mut || {}),
            Err(BsError::Version(3))
        );
    }
}
