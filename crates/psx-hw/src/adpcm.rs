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
pub fn quantise_block<S: Copy + Into<i32>>(
    x: &[S; BLOCK_SAMPLES],
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
        let target: i32 = x[i].into();
        let q = nearest_nibble(target - p, shift);
        let v = reconstruct(q, shift, p);
        error(target - v);
        nib[i] = (q & 0xf) as u8;
        s2 = s1;
        s1 = v;
        i += 1;
    }
    (nib, (s1, s2))
}

/// The nibble (`-8..=7`) for `residual` at `shift` when ties round up: `(residual + half) >> step`,
/// floored, then clamped. This is the rounding the run-time guest encoders ship; it differs from
/// [`nearest_nibble`] only when a negative residual sits exactly halfway between two steps (which
/// goes to the larger nibble here, away from zero there).
#[inline(always)]
#[must_use]
pub const fn nibble_half_up(residual: i32, shift: u32) -> i32 {
    let step_log = 12 - shift as i32;
    let q = if step_log == 0 {
        residual
    } else {
        (residual + (1 << (step_log - 1))) >> step_log
    };
    if q < -8 {
        -8
    } else if q > 7 {
        7
    } else {
        q
    }
}

/// The largest absolute open-loop residual of the block under `filter`: each sample against the
/// prediction from the *source* history (the previous two target samples), the cheap measure a
/// run-time encoder picks its shift from.
#[inline(always)]
#[must_use]
pub fn residual_peak<S: Copy + Into<i32>>(
    x: &[S; BLOCK_SAMPLES],
    state: (i32, i32),
    filter: usize,
) -> i32 {
    let f = FILTERS[filter];
    let (mut s1, mut s2) = state;
    let mut peak = 0;
    let mut i = 0;
    while i < BLOCK_SAMPLES {
        let target: i32 = x[i].into();
        let r = target - predict(f, s1, s2);
        peak = if r.abs() > peak { r.abs() } else { peak };
        s2 = s1;
        s1 = target;
        i += 1;
    }
    peak
}

/// [`residual_peak`] for all five filters in one pass, with the table folded in as constants.
#[inline(always)]
#[must_use]
pub fn filter_peaks<S: Copy + Into<i32>>(x: &[S; BLOCK_SAMPLES], state: (i32, i32)) -> [i32; 5] {
    let (mut s1, mut s2) = state;
    let mut peak = [0i32; 5];
    let mut i = 0;
    while i < BLOCK_SAMPLES {
        let t: i32 = x[i].into();
        let r = [
            t,
            t - predict(FILTERS[1], s1, s2),
            t - predict(FILTERS[2], s1, s2),
            t - predict(FILTERS[3], s1, s2),
            t - predict(FILTERS[4], s1, s2),
        ];
        let mut f = 0;
        while f < 5 {
            let a = r[f].abs();
            peak[f] = if a > peak[f] { a } else { peak[f] };
            f += 1;
        }
        s2 = s1;
        s1 = t;
        i += 1;
    }
    peak
}

/// The filter with the smallest peak (the lowest index on a tie) and that peak.
#[inline(always)]
#[must_use]
pub const fn best_filter(peaks: &[i32; 5]) -> (usize, i32) {
    let mut best = 0;
    let mut f = 1;
    while f < 5 {
        if peaks[f] < peaks[best] {
            best = f;
        }
        f += 1;
    }
    (best, peaks[best])
}

/// The finest shift (at most [`MAX_SHIFT`]) whose nibble range still covers a residual `peak`.
#[inline(always)]
#[must_use]
pub const fn shift_for_peak(peak: i32) -> u32 {
    let mut shift = MAX_SHIFT;
    while shift > 0 && peak > (7 << (12 - shift)) {
        shift -= 1;
    }
    shift
}

/// Quantise one block closed-loop under `filter` and `shift` ([`nibble_half_up`] rounding) and
/// write the whole 16 bytes into `out`: header, `flags` and the 28 nibbles. Returns the history
/// after the block. The run-time encoders' form: no error accounting, nibbles stored in place.
///
/// # Panics
///
/// If `filter` is not below five.
#[inline(always)]
pub fn encode_block<S: Copy + Into<i32>>(
    x: &[S; BLOCK_SAMPLES],
    state: (i32, i32),
    filter: usize,
    shift: u32,
    flags: u8,
    out: &mut [u8; BLOCK_BYTES],
) -> (i32, i32) {
    let f = FILTERS[filter];
    let (mut s1, mut s2) = state;
    out[0] = header(filter, shift);
    out[1] = flags;
    let mut i = 0;
    while i < BLOCK_SAMPLES {
        let p = predict(f, s1, s2);
        let target: i32 = x[i].into();
        let q = nibble_half_up(target - p, shift);
        let v = reconstruct(q, shift, p);
        s2 = s1;
        s1 = v;
        let nib = (q & 0xf) as u8;
        if i & 1 == 0 {
            out[2 + i / 2] = nib;
        } else {
            out[2 + i / 2] |= nib << 4;
        }
        i += 1;
    }
    (s1, s2)
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
#[allow(clippy::needless_range_loop)] // the oracles are the games' own loops, verbatim
mod tests {
    extern crate std;
    use super::*;
    use std::eprintln;

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

    /// A deterministic stream of blocks the way the synths make them: tones, noise, silence,
    /// full-scale clipping and steps, so every filter and shift gets picked.
    fn block(rng: &mut u32, n: u32) -> [i16; BLOCK_SAMPLES] {
        let mut next = || {
            *rng ^= *rng << 13;
            *rng ^= *rng >> 17;
            *rng ^= *rng << 5;
            *rng
        };
        let kind = next() % 6;
        let amp = (next() % 32768) as i32 + 1;
        let period = (next() % 60 + 2) as i32;
        core::array::from_fn(|i| {
            let ph = ((i as u32 + n * 28) as i32 % period) * 2 - period;
            let v = match kind {
                0 => 0,
                1 => (next() as i32 >> 16) * amp / 32768,
                2 => ph * amp / period,
                3 => {
                    if ph < 0 {
                        -amp
                    } else {
                        amp
                    }
                }
                4 => (ph * ph / period - period / 3) * amp / period,
                _ => (next() as i32 >> 20) * (1 + (next() % 8) as i32),
            };
            v.clamp(-32768, 32767) as i16
        })
    }

    /// Celeste's shipped encoder (`shared/src/synth.rs encode_block`, `shift_for`, `quantize`),
    /// verbatim apart from the statics being fields.
    struct OldCeleste {
        s1: i32,
        s2: i32,
        filter: usize,
        count: u32,
    }

    impl OldCeleste {
        fn encode(&mut self, pcm: &[i16; BLOCK_SAMPLES], flags: u8, out: &mut [u8; BLOCK_BYTES]) {
            let (s1, s2) = (self.s1, self.s2);
            let search = self.count & 3 == 0;
            let mut best_f = self.filter;
            let mut best_max = 0i32;
            let (mut p1, mut p2) = (s1, s2);
            if search {
                let mut rmax = [0i32; 5];
                for &x in pcm.iter() {
                    let x = x as i32;
                    let r = [
                        x,
                        x - ((p1 * 60) >> 6),
                        x - ((p1 * 115) >> 6) - ((p2 * -52) >> 6),
                        x - ((p1 * 98) >> 6) - ((p2 * -55) >> 6),
                        x - ((p1 * 122) >> 6) - ((p2 * -60) >> 6),
                    ];
                    for f in 0..5 {
                        rmax[f] = rmax[f].max(r[f].abs());
                    }
                    p2 = p1;
                    p1 = x;
                }
                best_f = 0;
                best_max = rmax[0];
                for f in 1..5 {
                    if rmax[f] < best_max {
                        best_max = rmax[f];
                        best_f = f;
                    }
                }
            } else {
                let (k0, k1) = FILTERS[best_f];
                for &x in pcm.iter() {
                    let x = x as i32;
                    best_max = best_max.max((x - ((p1 * k0) >> 6) - ((p2 * k1) >> 6)).abs());
                    p2 = p1;
                    p1 = x;
                }
            }
            self.filter = best_f;
            self.count = self.count.wrapping_add(1);
            let mut shift = 12u32;
            while shift > 0 && best_max > (7 << (12 - shift)) {
                shift -= 1;
            }
            let (k0, k1) = FILTERS[best_f];
            let (mut p1, mut p2) = (s1, s2);
            let rs = 12 - shift;
            for i in 0..BLOCK_SAMPLES {
                let pred = ((p1 * k0) >> 6) + ((p2 * k1) >> 6);
                let r = pcm[i] as i32 - pred;
                let q = (if rs > 0 {
                    (r + (1 << (rs - 1))) >> rs
                } else {
                    r
                })
                .clamp(-8, 7);
                let dec = (((q << 12) >> shift) + pred).clamp(-0x8000, 0x7fff);
                p2 = p1;
                p1 = dec;
                let n = (q & 0xf) as u8;
                if i & 1 == 0 {
                    out[2 + i / 2] = n;
                } else {
                    out[2 + i / 2] |= n << 4;
                }
            }
            self.s1 = p1;
            self.s2 = p2;
            out[0] = ((best_f as u8) << 4) | shift as u8;
            out[1] = flags;
        }
    }

    /// The same encoder through this module: what the game calls after adopting it.
    struct NewCeleste {
        state: (i32, i32),
        filter: usize,
        count: u32,
    }

    impl NewCeleste {
        fn encode(&mut self, pcm: &[i16; BLOCK_SAMPLES], flags: u8, out: &mut [u8; BLOCK_BYTES]) {
            let peak = if self.count & 3 == 0 {
                let (f, peak) = best_filter(&filter_peaks(pcm, self.state));
                self.filter = f;
                peak
            } else {
                residual_peak(pcm, self.state, self.filter)
            };
            self.count = self.count.wrapping_add(1);
            self.state = encode_block(
                pcm,
                self.state,
                self.filter,
                shift_for_peak(peak),
                flags,
                out,
            );
        }
    }

    #[test]
    fn the_celeste_encoder_through_this_module_writes_the_same_bytes() {
        let mut rng = 0x1234_5678u32;
        let mut old = OldCeleste {
            s1: 0,
            s2: 0,
            filter: 0,
            count: 0,
        };
        let mut new = NewCeleste {
            state: (0, 0),
            filter: 0,
            count: 0,
        };
        let (mut blocks, mut filters_seen, mut shifts_seen) = (0u32, [false; 5], [false; 13]);
        for n in 0..600_000u32 {
            if n % 5000 == 0 {
                // A stream restart, as reset_encoder does.
                old = OldCeleste {
                    s1: 0,
                    s2: 0,
                    filter: 0,
                    count: 0,
                };
                new = NewCeleste {
                    state: (0, 0),
                    filter: 0,
                    count: 0,
                };
            }
            let pcm = block(&mut rng, n);
            let flags = (n % 8) as u8;
            let (mut a, mut b) = ([0u8; BLOCK_BYTES], [0xaau8; BLOCK_BYTES]);
            old.encode(&pcm, flags, &mut a);
            new.encode(&pcm, flags, &mut b);
            assert_eq!(a, b, "block {n}");
            assert_eq!((old.s1, old.s2), new.state, "history after block {n}");
            filters_seen[(a[0] >> 4) as usize] = true;
            shifts_seen[(a[0] & 15) as usize] = true;
            blocks += 1;
        }
        assert_eq!(blocks, 600_000);
        assert!(filters_seen.iter().all(|&s| s), "{filters_seen:?}");
        assert!(shifts_seen[..=12].iter().all(|&s| s), "{shifts_seen:?}");
    }

    /// VoXide's shipped `adpcm_block`, with the prediction either as it ships (one `>> 6` of
    /// the sum of products) or as the SPU decodes (two floored products).
    fn old_voxide(s: &[i32; 28], p1: &mut i32, p2: &mut i32, out: &mut [u8; 16], spu_exact: bool) {
        let pred = |a: i32, b: i32, k: (i32, i32)| {
            if spu_exact {
                ((a * k.0) >> 6) + ((b * k.1) >> 6)
            } else {
                (a * k.0 + b * k.1) >> 6
            }
        };
        let mut best = (i32::MAX, 0usize);
        let mut f = 0;
        while f < 5 {
            let (mut a, mut b) = (*p1, *p2);
            let mut m = 0;
            let mut i = 0;
            while i < 28 {
                let r = s[i] - pred(a, b, FILTERS[f]);
                m = m.max(r.abs());
                b = a;
                a = s[i];
                i += 1;
            }
            if m < best.0 {
                best = (m, f);
            }
            f += 1;
        }
        let f = best.1;
        let mut range = 12;
        while range > 0 && (7 << (12 - range)) < best.0 {
            range -= 1;
        }
        let shift = range;
        out[0] = (shift as u8) | ((f as u8) << 4);
        out[1] = 0;
        let step = 12 - shift;
        let mut i = 0;
        while i < 28 {
            let pr = pred(*p1, *p2, FILTERS[f]);
            let r = s[i] - pr;
            let q = if step > 0 {
                ((r + (1 << (step - 1))) >> step).clamp(-8, 7)
            } else {
                r.clamp(-8, 7)
            };
            let dec = ((q << step) + pr).clamp(-32_768, 32_767);
            *p2 = *p1;
            *p1 = dec;
            let nib = (q & 0xf) as u8;
            if i % 2 == 0 {
                out[2 + i / 2] = nib;
            } else {
                out[2 + i / 2] |= nib << 4;
            }
            i += 1;
        }
    }

    /// VoXide's encoder through this module: every block searches all five filters.
    fn new_voxide(s: &[i32; 28], state: &mut (i32, i32), out: &mut [u8; 16]) {
        let (f, peak) = best_filter(&filter_peaks(s, *state));
        *state = encode_block(s, *state, f, shift_for_peak(peak), 0, out);
    }

    #[test]
    fn the_voxide_encoder_matches_with_the_spu_prediction_and_differs_without_it() {
        let mut rng = 0x9e37_79b9u32;
        let (mut exact_old, mut shipped_old) = ((0, 0), (0, 0));
        let mut new = (0, 0);
        let (mut differing, mut total, mut worst) = (0u32, 0u32, 0i32);
        for n in 0..400_000u32 {
            let s16 = block(&mut rng, n);
            let s: [i32; 28] = core::array::from_fn(|i| i32::from(s16[i]));
            let (mut a, mut b, mut c) = ([0u8; 16], [0u8; 16], [0xaau8; 16]);
            old_voxide(&s, &mut exact_old.0, &mut exact_old.1, &mut a, true);
            old_voxide(&s, &mut shipped_old.0, &mut shipped_old.1, &mut b, false);
            new_voxide(&s, &mut new, &mut c);
            assert_eq!(
                a, c,
                "block {n}: the SPU-exact old form must equal the module"
            );
            assert_eq!(exact_old, new);
            total += 1;
            differing += u32::from(a != b);
            worst = worst.max((shipped_old.0 - new.0).abs());
        }
        // The shipped form drifts from what the SPU plays; the module does not.
        assert!(differing > 0, "{differing} of {total}");
        eprintln!("voxide shipped vs spu-exact: {differing} of {total} blocks differ, worst history gap {worst}");
    }

    #[test]
    fn half_up_differs_from_nearest_only_on_negative_ties() {
        for shift in 0..=MAX_SHIFT {
            for r in -70_000..70_000 {
                let (a, b) = (nibble_half_up(r, shift), nearest_nibble(r, shift));
                if a != b {
                    let step = 1 << (12 - shift);
                    assert!(
                        r < 0 && step > 1 && (-r) % step == step / 2,
                        "{r} at {shift}: {a} {b}"
                    );
                    assert_eq!(a, b + 1);
                }
            }
        }
    }
}
