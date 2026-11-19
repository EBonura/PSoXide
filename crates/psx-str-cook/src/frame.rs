//! One picture to one bitstream version 2 frame.
//!
//! [`Transformed::new`] cuts a [`Planes`] picture into macroblocks (column
//! by column, top to bottom, as the player reads them; each Cr, Cb and the
//! four luma blocks), and takes every block's forward DCT once.
//! [`Transformed::encode`] then quantises at one quantisation scale and
//! writes the bitstream, so rate control can try scales cheaply.
//!
//! Quantisation picks, for every coefficient, the level whose value after
//! the MDEC's own inverse quantisation ([`crate::model`]) is nearest the
//! wanted coefficient; the variable-length codes are the SDK decoder's table
//! ([`psx_fmv::bitstream::AC_CODES`]), a level the table lacks takes the
//! escape.
//!
//! Scale of the transform: the MDEC's two IDCT passes each compute a quarter
//! of the orthonormal inverse DCT (the matrix is the DCT-II basis at 1.12,
//! halved again by the DC row's `1/sqrt(2)`), so the coefficient that
//! reproduces a block is 16 times its orthonormal DCT.

use psx_fmv::bitstream::{AC_CODES, HEADER_BYTES, V2_END_OF_FRAME};

use crate::model::{dequant_ac, dequant_dc, zigzag};
use crate::picture::Planes;

/// MDEC decode command's high half (15 bits per pixel), as BS headers carry it.
const HEADER_COMMAND: u16 = 0x3800;
/// Coefficient scale the MDEC's IDCT expects over the orthonormal DCT.
const MDEC_GAIN: f64 = 16.0;
/// Largest and smallest 10-bit levels.
const LEVEL_MAX: i32 = 511;
const LEVEL_MIN: i32 = -512;
/// Largest DC value: `0x1FF` in the first block of a macroblock ends the frame.
const DC_MAX: i32 = V2_END_OF_FRAME as i32 - 1;

/// Every block of a picture after the forward DCT, in MDEC scale.
pub struct Transformed {
    width: u16,
    height: u16,
    /// Six blocks per macroblock (Cr, Cb, Y0..Y3), macroblocks column-major,
    /// coefficients row-major.
    blocks: Vec<[f64; 64]>,
}

/// One encoded frame.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Encoded {
    /// The frame: 8-byte header and the bitstream, a multiple of 4 bytes.
    pub bytes: Vec<u8>,
    /// Quantisation scale used.
    pub qscale: u16,
    /// Run-length words the decoder produces (the header's MDEC size).
    pub rle_words: usize,
}

/// The 8-point orthonormal DCT-II basis, `BASIS[k][n]`.
fn basis() -> [[f64; 8]; 8] {
    let mut b = [[0.0; 8]; 8];
    for (k, row) in b.iter_mut().enumerate() {
        let scale = if k == 0 { (0.125f64).sqrt() } else { 0.5 };
        for (n, v) in row.iter_mut().enumerate() {
            *v = scale * (std::f64::consts::PI * ((2 * n + 1) * k) as f64 / 16.0).cos();
        }
    }
    b
}

/// Forward DCT of a block of samples (row-major), in MDEC scale.
fn forward_dct(samples: &[f64; 64], b: &[[f64; 8]; 8]) -> [f64; 64] {
    let mut rows = [0.0; 64];
    for y in 0..8 {
        for u in 0..8 {
            rows[8 * y + u] = (0..8).map(|x| samples[8 * y + x] * b[u][x]).sum();
        }
    }
    let mut out = [0.0; 64];
    for v in 0..8 {
        for u in 0..8 {
            out[8 * v + u] = MDEC_GAIN * (0..8).map(|y| rows[8 * y + u] * b[v][y]).sum::<f64>();
        }
    }
    out
}

/// The 8x8 block at (`x0`, `y0`) of a `width`-wide plane, edges repeated.
fn cut(plane: &[f64], width: usize, height: usize, x0: usize, y0: usize) -> [f64; 64] {
    let mut block = [0.0; 64];
    for y in 0..8 {
        for x in 0..8 {
            let (px, py) = ((x0 + x).min(width - 1), (y0 + y).min(height - 1));
            block[8 * y + x] = plane[py * width + px];
        }
    }
    block
}

/// MSB-first bit writer over little-endian 16-bit words, the BS layout.
struct Bits {
    bytes: Vec<u8>,
    acc: u32,
    used: u32,
}

impl Bits {
    fn put(&mut self, len: u32, value: u32) {
        debug_assert!(len <= 16);
        self.acc = (self.acc << len) | (value & ((1 << len) - 1));
        self.used += len;
        while self.used >= 16 {
            self.used -= 16;
            let word = (self.acc >> self.used) as u16;
            self.bytes.extend_from_slice(&word.to_le_bytes());
        }
        self.acc &= (1 << self.used) - 1;
    }

    /// Pads to a whole halfword with zeros.
    fn flush(&mut self) {
        if self.used > 0 {
            self.put(16 - self.used, 0);
        }
    }
}

/// `(code length, code)` by run (0..32) and level (1..41), zero length
/// where the table has no code.
struct Codes([[(u8, u16); 41]; 32]);

impl Codes {
    fn new() -> Codes {
        let mut t = [[(0u8, 0u16); 41]; 32];
        for &(len, code, run, level) in AC_CODES.iter() {
            t[usize::from(run)][usize::from(level)] = (len, code);
        }
        Codes(t)
    }

    fn write(&self, bits: &mut Bits, run: usize, level: i32) {
        let magnitude = level.unsigned_abs() as usize;
        match self.0.get(run).and_then(|r| r.get(magnitude)) {
            Some(&(len, code)) if len > 0 => {
                bits.put(u32::from(len), u32::from(code));
                bits.put(1, u32::from(level < 0));
            }
            _ => {
                // Escape `000001` and the raw MDEC halfword.
                bits.put(6, 0b00_0001);
                bits.put(16, ((run as u32) << 10) | (level as u32 & 0x3ff));
            }
        }
    }
}

/// The level among those around `target / step` whose dequantised value is
/// nearest `target`; `dequant` gives a level's value.
fn nearest_level(target: f64, step: f64, lo: i32, hi: i32, dequant: impl Fn(i32) -> i32) -> i32 {
    let guess = (target / step).floor() as i32;
    let mut best = 0i32;
    let mut best_error = target.abs();
    for level in [guess, guess + 1] {
        let level = level.clamp(lo, hi);
        let error = (f64::from(dequant(level)) - target).abs();
        // Ties go to the smaller magnitude: fewer bits.
        if error < best_error || (error == best_error && level.abs() < best.abs()) {
            best = level;
            best_error = error;
        }
    }
    best
}

impl Transformed {
    /// The forward DCT of every block of `picture`, whose size must be a
    /// multiple of 16 in both directions.
    ///
    /// # Panics
    ///
    /// If the size is not a multiple of 16 or does not fit 16 bits.
    #[must_use]
    pub fn new(picture: &Planes) -> Transformed {
        let (w, h) = (picture.width, picture.height);
        assert!(
            w % 16 == 0 && h % 16 == 0 && w > 0 && h > 0,
            "{w}x{h} is not whole macroblocks"
        );
        let (cw, ch) = (picture.chroma_width(), picture.chroma_height());
        let b = basis();
        let mut blocks = Vec::with_capacity(w / 16 * h / 16 * 6);
        for mx in (0..w).step_by(16) {
            for my in (0..h).step_by(16) {
                blocks.push(forward_dct(&cut(&picture.cr, cw, ch, mx / 2, my / 2), &b));
                blocks.push(forward_dct(&cut(&picture.cb, cw, ch, mx / 2, my / 2), &b));
                for (dx, dy) in [(0, 0), (8, 0), (0, 8), (8, 8)] {
                    let block = cut(&picture.y, w, h, mx + dx, my + dy);
                    blocks.push(forward_dct(&block, &b));
                }
            }
        }
        Transformed {
            width: u16::try_from(w).expect("width fits 16 bits"),
            height: u16::try_from(h).expect("height fits 16 bits"),
            blocks,
        }
    }

    /// Picture width.
    #[must_use]
    pub fn width(&self) -> u16 {
        self.width
    }

    /// Picture height.
    #[must_use]
    pub fn height(&self) -> u16 {
        self.height
    }

    /// The frame at quantisation scale `qscale` (1..=63).
    ///
    /// # Panics
    ///
    /// If `qscale` is outside 1..=63.
    pub fn encode(&self, qscale: u16) -> Encoded {
        assert!((1..=63).contains(&qscale), "qscale {qscale}");
        let q = i32::from(qscale);
        let codes = Codes::new();
        let mut bits = Bits {
            bytes: vec![0; HEADER_BYTES],
            acc: 0,
            used: 0,
        };
        // Run-length halfwords the decoder will produce: per block the DC,
        // one per non-zero AC level and the end of block.
        let mut halfwords = 0usize;
        for block in &self.blocks {
            let dc = nearest_level(block[0], 32.0, LEVEL_MIN, DC_MAX, dequant_dc);
            bits.put(10, dc as u32 & 0x3ff);
            halfwords += 2;
            let mut run = 0usize;
            for i in 1..64 {
                let step = 2.0 * f64::from(q * crate::model::quant(i));
                let target = block[zigzag(i)];
                let level =
                    nearest_level(target, step, LEVEL_MIN, LEVEL_MAX, |l| dequant_ac(l, i, q));
                if level == 0 {
                    run += 1;
                } else {
                    codes.write(&mut bits, run, level);
                    halfwords += 1;
                    run = 0;
                }
            }
            bits.put(2, 0b10);
        }
        bits.put(10, V2_END_OF_FRAME);
        bits.flush();
        let mut bytes = bits.bytes;
        while !bytes.len().is_multiple_of(4) {
            bytes.push(0);
        }
        // The decoder pads its output to 32-word DMA blocks; announce that.
        let mdec_words = halfwords.div_ceil(64) * 32;
        bytes[0..2].copy_from_slice(&(mdec_words as u16).to_le_bytes());
        bytes[2..4].copy_from_slice(&HEADER_COMMAND.to_le_bytes());
        bytes[4..6].copy_from_slice(&qscale.to_le_bytes());
        bytes[6..8].copy_from_slice(&2u16.to_le_bytes());
        Encoded {
            bytes,
            qscale,
            rle_words: mdec_words,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{decode_rgb, psnr};

    fn picture(width: usize, height: usize, f: impl Fn(usize, usize) -> (f64, f64, f64)) -> Planes {
        let (cw, ch) = (width / 2, height / 2);
        let mut p = Planes {
            width,
            height,
            y: vec![0.0; width * height],
            cb: vec![0.0; cw * ch],
            cr: vec![0.0; cw * ch],
        };
        for y in 0..height {
            for x in 0..width {
                let (l, cb, cr) = f(x, y);
                p.y[y * width + x] = l;
                if x % 2 == 0 && y % 2 == 0 {
                    p.cb[(y / 2) * cw + x / 2] = cb;
                    p.cr[(y / 2) * cw + x / 2] = cr;
                }
            }
        }
        p
    }

    fn gradient() -> Planes {
        picture(64, 48, |x, y| {
            (
                (x as f64 * 3.7 + y as f64 * 1.3) % 255.0 - 128.0,
                (x as f64 - 32.0) * 2.0,
                (y as f64 - 24.0) * 3.0,
            )
        })
    }

    #[test]
    fn a_frame_decodes_through_the_sdk_decoder_to_the_announced_size() {
        let t = Transformed::new(&gradient());
        let e = t.encode(4);
        let mut rle = vec![0u16; 64 * 1024];
        let words =
            psx_fmv::bitstream::decode_frame(&e.bytes, &mut rle, 12, 3, &mut || {}).unwrap();
        let header = psx_fmv::bitstream::Header::parse(&e.bytes).unwrap();
        assert_eq!(words, usize::from(header.mdec_words));
        assert_eq!((header.qscale, header.version), (4, 2));
        assert_eq!(e.bytes.len() % 4, 0);
    }

    #[test]
    fn quality_falls_and_size_shrinks_as_the_scale_rises() {
        let p = gradient();
        let reference = p.to_rgb();
        let t = Transformed::new(&p);
        let mut last = (f64::INFINITY, usize::MAX);
        for q in [1, 2, 4, 8, 16, 32, 63] {
            let e = t.encode(q);
            let quality = psnr(&decode_rgb(&e.bytes, 64, 48).unwrap(), &reference);
            assert!(
                quality <= last.0 + 0.2,
                "qscale {q}: {quality} after {}",
                last.0
            );
            assert!(e.bytes.len() <= last.1, "qscale {q}");
            last = (quality, e.bytes.len());
        }
        let fine = psnr(&decode_rgb(&t.encode(1).bytes, 64, 48).unwrap(), &reference);
        assert!(fine > 38.0, "qscale 1 gives {fine} dB");
    }

    #[test]
    fn flat_colours_survive_with_their_hue() {
        // A flat orange: the transposition or sign of a chroma block would
        // turn it another colour.
        let p = picture(32, 32, |_, _| (10.0, -40.0, 60.0));
        let rgb = decode_rgb(&Transformed::new(&p).encode(1).bytes, 32, 32).unwrap();
        let want = p.to_rgb();
        for (got, want) in rgb.chunks_exact(3).zip(want.chunks_exact(3)) {
            for c in 0..3 {
                assert!((i32::from(got[c]) - i32::from(want[c])).abs() <= 2);
            }
        }
    }

    #[test]
    fn levels_beyond_the_table_take_the_escape() {
        // A hard vertical edge at qscale 1 needs large levels.
        let p = picture(16, 16, |x, _| {
            if x < 8 {
                (-128.0, 0.0, 0.0)
            } else {
                (127.0, 0.0, 0.0)
            }
        });
        let e = Transformed::new(&p).encode(1);
        let rgb = decode_rgb(&e.bytes, 16, 16).unwrap();
        assert!(psnr(&rgb, &p.to_rgb()) > 30.0);
    }
}
