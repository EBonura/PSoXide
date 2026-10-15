//! VADPCM, the 9-byte-per-16-sample predictive codec the N64 sample banks
//! use.
//!
//! A frame is one header byte (high nibble: residual shift, low nibble:
//! predictor page) and eight bytes of sixteen signed 4-bit residuals. Each
//! page of the codebook holds `order` vectors of eight coefficients; decoding
//! two groups of eight samples per frame, each sample is the shifted residual
//! plus an inner product of the page's coefficients with the previous
//! `order` samples and the residuals so far in the group, scaled by 2^-11.

use crate::rom::{be_u16, be_u32};
use crate::Error;

/// A codebook expanded into the per-row filters the decoder multiplies with.
pub struct Book {
    order: usize,
    /// `table[page][row]` is `order + 8` taps.
    table: Vec<Vec<Vec<i32>>>,
}

impl Book {
    /// Read the codebook at `off` in `font` (order, page count, coefficients).
    pub fn parse(font: &[u8], off: usize) -> Result<Self, Error> {
        let order = be_u32(font, off)? as usize;
        let npred = be_u32(font, off + 4)? as usize;
        if order == 0 || order > 8 || npred == 0 || npred > 16 {
            return Err(Error::Format(format!(
                "codebook order {order} pages {npred}"
            )));
        }
        let mut at = off + 8;
        let mut table = Vec::with_capacity(npred);
        for _ in 0..npred {
            let mut page = vec![vec![0i32; order + 8]; 8];
            for col in 0..order {
                for row in page.iter_mut() {
                    row[col] = be_u16(font, at)? as i16 as i32;
                    at += 2;
                }
            }
            for k in 1..8 {
                page[k][order] = page[k - 1][order - 1];
            }
            page[0][order] = 1 << 11;
            for k in 1..8 {
                for j in 0..8 {
                    page[j][order + k] = if j < k { 0 } else { page[j - k][order] };
                }
            }
            table.push(page);
        }
        Ok(Self { order, table })
    }
}

fn inner(len: usize, a: &[i32], b: &[i32]) -> i32 {
    let sum: i32 = a[..len].iter().zip(&b[..len]).map(|(x, y)| x * y).sum();
    sum.div_euclid(1 << 11)
}

/// Decode `data` (whole frames) to 16-bit PCM.
pub fn decode(data: &[u8], book: &Book) -> Vec<i16> {
    let order = book.order;
    let mut out = Vec::with_capacity(data.len() / 9 * 16);
    let mut state = [0i32; 16];
    for frame in data.chunks_exact(9) {
        let scale = 1i32 << (frame[0] >> 4);
        let page = &book.table[((frame[0] & 15) as usize).min(book.table.len() - 1)];
        let mut ix = [0i32; 16];
        for i in 0..16 {
            let byte = frame[1 + i / 2];
            let n = if i % 2 == 0 { byte >> 4 } else { byte & 15 } as i32;
            ix[i] = (if n >= 8 { n - 16 } else { n }) * scale;
        }
        for half in 0..2 {
            let mut in_vec = [0i32; 16];
            for i in 0..order {
                in_vec[i] = state[(2 - half) * 8 - order + i];
            }
            for i in 0..8 {
                let at = half * 8 + i;
                in_vec[order + i] = ix[at];
                let v = inner(order + i, &page[i], &in_vec) + ix[at];
                state[at] = v.clamp(-32768, 32767);
            }
        }
        out.extend(state.iter().map(|&v| v as i16));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A codebook of order 2 with one all-zero page: no prediction at all.
    fn flat_book() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&2u32.to_be_bytes());
        b.extend_from_slice(&1u32.to_be_bytes());
        b.extend_from_slice(&[0u8; 2 * 8 * 2]);
        b
    }

    #[test]
    fn without_prediction_a_frame_is_its_scaled_residuals() {
        let book = Book::parse(&flat_book(), 0).unwrap();
        // shift 2, page 0; residual nibbles 1, 2, 3, ... 8, then -1 ..
        let mut frame = [0u8; 9];
        frame[0] = 2 << 4;
        for i in 0..8 {
            frame[1 + i] = (((2 * i + 1) as u8 & 15) << 4) | ((2 * i + 2) as u8 & 15);
        }
        let pcm = decode(&frame, &book);
        assert_eq!(pcm.len(), 16);
        // nibble values 1..=15 sign-extended: 1..7, -8..-1; each << 2
        let want: Vec<i16> = (1..=16i32)
            .map(|n| {
                let n = n & 15;
                let s = if n >= 8 { n - 16 } else { n };
                (s << 2) as i16
            })
            .collect();
        assert_eq!(pcm, want);
    }

    #[test]
    fn a_codebook_column_weights_earlier_residuals_within_the_frame() {
        // order 1, one page whose only coefficient is 1.0 (2048). The page
        // expands so each sample takes the previous sample's residual too:
        // with every residual 1 the output is 1, 2, 2, 2, ...
        let mut b = Vec::new();
        b.extend_from_slice(&1u32.to_be_bytes());
        b.extend_from_slice(&1u32.to_be_bytes());
        b.extend_from_slice(&2048i16.to_be_bytes());
        b.extend_from_slice(&[0u8; 14]);
        let book = Book::parse(&b, 0).unwrap();
        let mut frame = [0u8; 9];
        frame[1..].fill(0x11);
        let pcm = decode(&frame, &book);
        assert_eq!(&pcm[..4], &[1, 2, 2, 2]);
    }

    #[test]
    fn a_bad_codebook_is_rejected() {
        assert!(Book::parse(&[0u8; 8], 0).is_err());
    }
}
