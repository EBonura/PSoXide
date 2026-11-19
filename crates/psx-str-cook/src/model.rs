//! A host model of the MDEC: run-length halfwords to 24-bit RGB.
//!
//! The encoder quantises against [`dequant_dc`] and [`dequant_ac`], and the
//! measurements decode with [`decode_rgb`], so both sides use the same
//! arithmetic: the inverse quantisation, the two rounded passes of the
//! matrix IDCT and the colour conversion as the PSoXide emulator's MDEC
//! performs them (psx-spx "MDEC Decompression"). The IDCT matrix and the
//! quantisation table are the ones the SDK's driver uploads
//! ([`psx_fmv::idct::MATRIX`], [`psx_fmv::mdec::QUANT_ROW_MAJOR`]), so a
//! change there moves the encoder and the measurement together.

use psx_fmv::bitstream::{self, DecodeError, END_OF_BLOCK};
use psx_fmv::idct::MATRIX;
use psx_fmv::mdec::{QUANT_ROW_MAJOR, ZIGZAG_TO_ROW_MAJOR};

/// Coefficient range the MDEC keeps after inverse quantisation.
const COEFFICIENT_MIN: i32 = -0x4000;
const COEFFICIENT_MAX: i32 = 0x3fff;

/// Quantisation table entry for zigzag position `i`.
pub(crate) fn quant(i: usize) -> i32 {
    i32::from(QUANT_ROW_MAJOR[usize::from(ZIGZAG_TO_ROW_MAJOR[i])])
}

/// Row-major index of zigzag position `i`.
pub(crate) fn zigzag(i: usize) -> usize {
    usize::from(ZIGZAG_TO_ROW_MAJOR[i])
}

/// Sign-extends the 10-bit value of a run-length halfword.
pub(crate) fn level_of(halfword: u16) -> i32 {
    (i32::from(halfword & 0x3ff) << 22) >> 22
}

/// The MDEC's rounding step towards zero after scaling.
fn toward_zero(value: i32) -> i32 {
    match value.signum() {
        1 => -8,
        -1 => 8,
        _ => 0,
    }
}

/// The coefficient a block's DC value becomes.
pub(crate) fn dequant_dc(dc: i32) -> i32 {
    (((dc * quant(0)) << 4) + toward_zero(dc)).clamp(COEFFICIENT_MIN, COEFFICIENT_MAX)
}

/// The coefficient an AC level becomes at zigzag position `i` and `qscale`.
pub(crate) fn dequant_ac(level: i32, i: usize, qscale: i32) -> i32 {
    let scale = qscale * quant(i);
    let value = if scale == 0 {
        level << 5
    } else {
        (((level * scale) >> 3) << 4) + toward_zero(level)
    };
    value.clamp(COEFFICIENT_MIN, COEFFICIENT_MAX)
}

/// One 8x8 block through the MDEC's matrix IDCT: `coefficients` row-major
/// (vertical frequency major), samples clamped to the signed byte range.
pub(crate) fn idct(coefficients: &[i32; 64]) -> [i32; 64] {
    // The driver uploads the matrix in 1.15; the MDEC keeps 1.12 of it.
    let m = |k: usize, n: usize| i64::from(MATRIX[8 * k + n] >> 3);
    // Vertical pass first, rounded to 16 bits, as the MDEC orders them (the
    // order is visible at one least significant bit).
    let mut columns = [0i64; 64];
    for y in 0..8 {
        for u in 0..8 {
            let sum: i64 = (0..8)
                .map(|v| i64::from(coefficients[8 * v + u]) * m(v, y))
                .sum();
            columns[8 * y + u] = i64::from(((sum + 0x4000) >> 15) as i16);
        }
    }
    let mut out = [0i32; 64];
    for y in 0..8 {
        for x in 0..8 {
            let sum: i64 = (0..8).map(|u| columns[8 * y + u] * m(u, x)).sum();
            out[8 * y + x] = (((sum + 0x4000) >> 15) as i32).clamp(-128, 127);
        }
    }
    out
}

/// Signed YCbCr to RGB bytes with the MDEC's fixed-point weights.
pub(crate) fn rgb(y: i32, cb: i32, cr: i32) -> [u8; 3] {
    const SHIFT: u32 = 16;
    let y = y << SHIFT;
    [
        y + 91_881 * cr,
        y - 22_525 * cb - 46_813 * cr,
        y + 116_130 * cb,
    ]
    .map(|c| {
        let c = c.clamp(-128 << SHIFT, 127 << SHIFT);
        (((c + (1 << (SHIFT - 1))) >> SHIFT) + 128) as u8
    })
}

/// Reads one block from `rle` at `at`: the coefficients (row-major) and the
/// position after it, or `None` when the halfwords run out.
fn read_block(rle: &[u16], mut at: usize, qscale_hint: &mut i32) -> Option<([i32; 64], usize)> {
    // Padding before a block is skipped.
    while *rle.get(at)? == END_OF_BLOCK {
        at += 1;
    }
    let head = rle[at];
    at += 1;
    let qscale = i32::from(head >> 10);
    *qscale_hint = qscale;
    let mut block = [0i32; 64];
    block[0] = dequant_dc(level_of(head));
    let mut index = 0usize;
    // The end-of-block halfword reads as run 63, which ends the loop.
    while index < 63 {
        let halfword = *rle.get(at)?;
        at += 1;
        index += usize::from(halfword >> 10) + 1;
        if index < 64 {
            block[zigzag(index)] = dequant_ac(level_of(halfword), index, qscale);
        }
    }
    Some((block, at))
}

/// Decodes one bitstream v1/v2 frame (header included) to `width * height`
/// RGB pixels, three bytes each, the way the MDEC renders it at 24 bits.
///
/// # Errors
///
/// The SDK's bitstream decoder refused the frame.
pub fn decode_rgb(frame: &[u8], width: usize, height: usize) -> Result<Vec<u8>, DecodeError> {
    let (columns, rows) = (width.div_ceil(16), height.div_ceil(16));
    let macroblocks = (columns * rows) as u32;
    let mut rle = vec![0u16; macroblocks as usize * 6 * 65 + 64];
    let words = bitstream::decode_frame(frame, &mut rle, macroblocks, rows as u32, &mut || {})?;
    let rle = &rle[..words * 2];
    let mut out = vec![0u8; width * height * 3];
    let (mut at, mut qscale) = (0usize, 0i32);
    'frame: for mb in 0..columns * rows {
        let mut blocks = [[0i32; 64]; 6];
        for block in &mut blocks {
            let Some((coefficients, next)) = read_block(rle, at, &mut qscale) else {
                break 'frame;
            };
            *block = idct(&coefficients);
            at = next;
        }
        let (x0, y0) = ((mb / rows) * 16, (mb % rows) * 16);
        for y in 0..16 {
            for x in 0..16 {
                let (px, py) = (x0 + x, y0 + y);
                if px >= width || py >= height {
                    continue;
                }
                let luma = blocks[2 + (y / 8) * 2 + x / 8][(y % 8) * 8 + x % 8];
                let c = (y / 2) * 8 + x / 2;
                let pixel = rgb(luma, blocks[1][c], blocks[0][c]);
                out[(py * width + px) * 3..][..3].copy_from_slice(&pixel);
            }
        }
    }
    Ok(out)
}

/// Sum of squared differences of two 8-bit pictures.
///
/// # Panics
///
/// If they differ in size.
#[must_use]
pub fn squared_error(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len(), "pictures of different sizes");
    a.iter()
        .zip(b)
        .map(|(&x, &y)| (f64::from(x) - f64::from(y)).powi(2))
        .sum()
}

/// Peak signal-to-noise ratio in dB for a mean squared error of 8-bit
/// samples; infinite for no error.
#[must_use]
pub fn psnr_of(mse: f64) -> f64 {
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0 * 255.0 / mse).log10()
    }
}

/// Peak signal-to-noise ratio of `a` against `b` (8-bit samples), in dB;
/// infinite for identical inputs.
///
/// # Panics
///
/// If they differ in size.
#[must_use]
pub fn psnr(a: &[u8], b: &[u8]) -> f64 {
    psnr_of(squared_error(a, b) / a.len() as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flat_block_comes_back_flat() {
        // DC 4v gives 32 * 4v - 8 = 128v - 8 in the coefficient, which the
        // two IDCT passes divide by 128: v, give or take the rounding.
        for v in [-100, -1, 0, 1, 37, 120] {
            let mut c = [0i32; 64];
            c[0] = dequant_dc(4 * v);
            let out = idct(&c);
            assert!(out.iter().all(|&s| (s - v).abs() <= 1), "{v}: {out:?}");
        }
    }

    #[test]
    fn colour_weights_match_the_mdec() {
        assert_eq!(rgb(0, 0, 0), [128, 128, 128]);
        assert_eq!(rgb(127, 0, 0), [255, 255, 255]);
        assert_eq!(rgb(-128, 0, 0), [0, 0, 0]);
        // Pure red difference raises R by 1.402 and lowers G by 0.714.
        let [r, g, b] = rgb(0, 0, 50);
        assert_eq!((r, g, b), (128 + 70, 128 - 36, 128));
    }

    #[test]
    fn dequantisation_rounds_toward_zero_by_half_a_step() {
        assert_eq!(dequant_dc(0), 0);
        assert_eq!(dequant_dc(1), 24);
        assert_eq!(dequant_dc(-1), -24);
        // Zigzag 1 has quant 16: level 1 at qscale 1 is (16 >> 3) << 4 - 8.
        assert_eq!(dequant_ac(1, 1, 1), 24);
        assert_eq!(dequant_ac(-1, 1, 1), -24);
    }

    #[test]
    fn psnr_of_identical_and_off_by_one() {
        assert!(psnr(&[1, 2, 3], &[1, 2, 3]).is_infinite());
        let p = psnr(&[0; 4], &[1; 4]);
        assert!((p - 48.13).abs() < 0.01, "{p}");
    }
}
