//! SPU-ADPCM block model: the filter table and the closed-loop quantiser, once.
//!
//! A 16-byte block holds 28 four-bit residuals, a predictor (one of five second-order filters,
//! weights in 1/64) and a shift. Every encoder, whatever its search, reconstructs exactly what
//! the SPU decodes: the prediction is two floored `>> 6` products, the sum is clamped to 16 bits
//! and the clamped value becomes the history. This module is that model for the host cooker
//! (`psx-audio-cook`) and for guest synthesisers that encode a sample at run time, with 32-bit
//! arithmetic only and no allocation: the error of a block is reported through a callback so each
//! caller accumulates it in the width it can afford (`i64` on the host, a saturating `u32` in
//! guest code).
//!
//! What stays with the caller is the search policy: which filters to try, how often to re-pick
//! them, how the shift is chosen. The five-filter table, the prediction, the rounding of a
//! residual to a nibble and the reconstruction of a block under a given filter and shift are
//! not policy and live here.

/// Bytes per ADPCM block.
pub const BLOCK_BYTES: usize = 16;
/// Samples per ADPCM block.
pub const BLOCK_SAMPLES: usize = 28;
/// The SPU's five prediction filters `(s1, s2)` weights in Q6 (nocash PSX-SPX). XA audio uses
/// the first four.
pub const FILTERS: [(i32, i32); 5] = [(0, 0), (60, 0), (115, -52), (98, -55), (122, -60)];
/// The filters XA-ADPCM allows (a prefix of [`FILTERS`]).
pub const XA_FILTER_COUNT: usize = 4;
/// The largest shift an encoder uses (the SPU treats 13 to 15 as 9).
pub const MAX_SHIFT: u32 = 12;

/// The SPU's prediction for filter weights `f` and the last two output samples.
#[inline(always)]
#[must_use]
pub const fn predict(f: (i32, i32), s1: i32, s2: i32) -> i32 {
    ((s1 * f.0) >> 6) + ((s2 * f.1) >> 6)
}

/// The nibble (`-8..=7`) nearest to `residual` at `shift`, rounding half away from zero:
/// the reconstruction step is `2^(12 - shift)`.
#[inline(always)]
#[must_use]
pub const fn nearest_nibble(residual: i32, shift: u32) -> i32 {
    let step_log = 12 - shift as i32;
    let q = if step_log == 0 {
        residual
    } else {
        let half = 1 << (step_log - 1);
        if residual >= 0 {
            (residual + half) >> step_log
        } else {
            -((-residual + half) >> step_log)
        }
    };
    if q < -8 {
        -8
    } else if q > 7 {
        7
    } else {
        q
    }
}

/// The sample the SPU produces from `nibble` (`-8..=7`) at `shift` after prediction `p`.
#[inline(always)]
#[must_use]
pub const fn reconstruct(nibble: i32, shift: u32, p: i32) -> i32 {
    let v = ((nibble << 12) >> shift) + p;
    if v < -0x8000 {
        -0x8000
    } else if v > 0x7fff {
        0x7fff
    } else {
        v
    }
}

/// The header byte of a block: filter in the high nibble, shift in the low one.
#[inline(always)]
#[must_use]
pub const fn header(filter: usize, shift: u32) -> u8 {
    ((filter as u8) << 4) | shift as u8
}

/// The closed-loop quantisation of one block: each sample takes the nibble nearest to its
/// residual against the prediction from the *reconstructed* history, so the encoder's error is the
/// error the console plays. `x` is the 28 target samples, `state` the history `(s1, s2)` before
/// the block, `filter` an index into [`FILTERS`], `shift` at most [`MAX_SHIFT`]. Returns the 28
/// nibbles (low four bits of each byte) and the history after the block; `error` is called once
/// per sample with `x[i] - reconstructed[i]`.
///
/// # Panics
///
/// If `filter` is not below five.
#[inline(always)]
pub fn quantise_block(
    x: &[i32; BLOCK_SAMPLES],
    state: (i32, i32),
    filter: usize,
    shift: u32,
    mut error: impl FnMut(i32),
) -> ([u8; BLOCK_SAMPLES], (i32, i32)) {
    let f = FILTERS[filter];
    let (mut s1, mut s2) = state;
    let mut nib = [0u8; BLOCK_SAMPLES];
    let mut i = 0;
    while i < BLOCK_SAMPLES {
        let p = predict(f, s1, s2);
        let q = nearest_nibble(x[i] - p, shift);
        let v = reconstruct(q, shift, p);
        error(x[i] - v);
        nib[i] = (q & 0xf) as u8;
        s2 = s1;
        s1 = v;
        i += 1;
    }
    (nib, (s1, s2))
}

/// Pack a block: `header`, `flags` and the 28 nibbles, two to a byte, the first in the low half.
#[must_use]
pub fn pack_block(header: u8, flags: u8, nib: &[u8; BLOCK_SAMPLES]) -> [u8; BLOCK_BYTES] {
    let mut b = [0u8; BLOCK_BYTES];
    b[0] = header;
    b[1] = flags;
    let mut i = 0;
    while i < 14 {
        b[2 + i] = (nib[2 * i] & 0xf) | (nib[2 * i + 1] << 4);
        i += 1;
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_the_published_one_and_xa_takes_its_prefix() {
        assert_eq!(
            FILTERS,
            [(0, 0), (60, 0), (115, -52), (98, -55), (122, -60)]
        );
        assert_eq!(XA_FILTER_COUNT, 4);
    }

    #[test]
    fn nibble_rounding_is_half_away_from_zero_and_clamped() {
        assert_eq!(nearest_nibble(0, 12), 0);
        assert_eq!(nearest_nibble(7, 12), 7);
        assert_eq!(nearest_nibble(100, 12), 7);
        assert_eq!(nearest_nibble(-100, 12), -8);
        // shift 8: step 16, half 8.
        assert_eq!(nearest_nibble(8, 8), 1);
        assert_eq!(nearest_nibble(7, 8), 0);
        assert_eq!(nearest_nibble(-8, 8), -1);
        assert_eq!(nearest_nibble(-7, 8), 0);
    }

    #[test]
    fn a_quantised_block_decodes_to_its_own_reconstruction() {
        let x: [i32; BLOCK_SAMPLES] = core::array::from_fn(|i| ((i as i32 * 977) % 4000) - 2000);
        for filter in 0..5 {
            for shift in 0..=MAX_SHIFT {
                let mut sum = 0i64;
                let (nib, end) = quantise_block(&x, (123, -45), filter, shift, |d| {
                    sum += i64::from(d) * i64::from(d);
                });
                // Decode the packed block the way the SPU does.
                let block = pack_block(header(filter, shift), 0, &nib);
                let f = FILTERS[(block[0] >> 4) as usize];
                let sh = u32::from(block[0] & 15);
                let (mut s1, mut s2) = (123, -45);
                let mut err = 0i64;
                for (i, &target) in x.iter().enumerate() {
                    let byte = i32::from(block[2 + i / 2]);
                    let n = if i & 1 == 0 { byte & 15 } else { byte >> 4 };
                    let n = (n << 28) >> 28;
                    let v = reconstruct(n, sh, predict(f, s1, s2));
                    err += i64::from(target - v) * i64::from(target - v);
                    s2 = s1;
                    s1 = v;
                }
                assert_eq!((s1, s2), end);
                assert_eq!(err, sum);
            }
        }
    }
}
