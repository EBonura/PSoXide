//! SPU-ADPCM: a decoder that matches the SPU, and a trellis encoder.
//!
//! Every 16-byte block holds 28 four-bit residuals, one of five fixed
//! second-order predictors and a shift. The file size depends only on the
//! sample count, so an encoder can only win on quality: which predictor,
//! shift and nibbles reconstruct the signal best *as the SPU decodes it*.
//!
//! The decoder here is the hardware's: the predictor is two floored `>> 6`
//! products, the sum is clamped to 16 bits and the clamped value becomes the
//! history (nocash PSX-SPX; PSoXide's emulator does the same), and shifts
//! 13..15 behave as 9. The encoder evaluates candidates with exactly this
//! model, so what it measures is what the console plays.
//!
//! Encoder, per block:
//! 1. Greedy closed-loop pass over all 5 filters x 13 shifts (nearest nibble
//!    per sample, clamped history). This alone is the classic encoder.
//! 2. Trellis (beam) search over the nibble sequence for the best few
//!    filter/shift pairs: each sample keeps the `beam` lowest-error paths,
//!    trying the nearest nibble and its two neighbours, because the
//!    reconstruction feeds the predictor and the locally nearest nibble is
//!    often not the best one for the rest of the block.
//! 3. Lookahead: the best few candidates are compared by their own error
//!    plus the best greedy error of the *next* block from their end state,
//!    so a block does not leave the predictor in a state the next block
//!    cannot recover from.
//!
//! Loops: the block where a hardware loop re-enters is forced to filter 0,
//! which ignores history, so it decodes identically on the first pass and
//! on every wrap (no click at the seam).

/// Bytes per ADPCM block.
pub const BLOCK_BYTES: usize = 16;
/// Samples per ADPCM block.
pub const BLOCK_SAMPLES: usize = 28;
/// SPU prediction filters `(s1, s2)` weights in Q6.
pub const FILTERS: [(i32, i32); 5] = [(0, 0), (60, 0), (115, -52), (98, -55), (122, -60)];

/// Block flag: end of sample (jump to the repeat address).
pub const FLAG_END: u8 = 0x01;
/// Block flag: repeat (with END, keep playing from the repeat address).
pub const FLAG_REPEAT: u8 = 0x02;
/// Block flag: loop start (latch this block as the repeat address).
pub const FLAG_LOOP_START: u8 = 0x04;

#[inline(always)]
fn predict(f: (i32, i32), s1: i32, s2: i32) -> i32 {
    ((s1 * f.0) >> 6) + ((s2 * f.1) >> 6)
}

/// Decode ADPCM blocks exactly as the SPU does (flags are ignored; blocks
/// decode in order). Returns 28 samples per block.
pub fn decode(adpcm: &[u8]) -> Vec<i16> {
    let mut out = Vec::with_capacity(adpcm.len() / BLOCK_BYTES * BLOCK_SAMPLES);
    let (mut s1, mut s2) = (0i32, 0i32);
    for block in adpcm.chunks_exact(BLOCK_BYTES) {
        let f = FILTERS[((block[0] >> 4) as usize).min(4)];
        let raw_shift = block[0] & 0x0F;
        let shift = if raw_shift > 12 { 9 } else { raw_shift } as u32;
        for i in 0..BLOCK_SAMPLES {
            let byte = block[2 + i / 2] as i32;
            let nib = if i & 1 == 0 { byte & 0xF } else { byte >> 4 };
            let raw = (((nib << 28) >> 28) << 12) >> shift;
            let v = (raw + predict(f, s1, s2)).clamp(-0x8000, 0x7FFF);
            out.push(v as i16);
            s2 = s1;
            s1 = v;
        }
    }
    out
}

/// Encoder effort.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Effort {
    /// Greedy closed-loop search of all 65 filter/shift pairs per block.
    Greedy,
    /// Greedy search, then a trellis over the best pairs, plus one block of
    /// lookahead. The default for cooking.
    Trellis,
}

/// Encoder settings.
#[derive(Copy, Clone, Debug)]
pub struct EncodeOptions {
    /// Search effort.
    pub effort: Effort,
    /// Paths kept per sample in the trellis.
    pub beam: usize,
    /// Filter/shift pairs refined by the trellis (best by greedy error).
    pub refine: usize,
    /// Candidates compared with one block of lookahead (1 disables it).
    pub lookahead: usize,
    /// Nibbles tried either side of the nearest one in the trellis.
    pub span: i32,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self {
            effort: Effort::Trellis,
            beam: 8,
            refine: 6,
            lookahead: 3,
            span: 1,
        }
    }
}

#[derive(Clone, Copy)]
struct Block {
    header: u8,
    nib: [u8; BLOCK_SAMPLES],
    end: (i32, i32),
    err: i64,
}

impl Block {
    fn bytes(&self, flags: u8) -> [u8; BLOCK_BYTES] {
        let mut b = [0u8; BLOCK_BYTES];
        b[0] = self.header;
        b[1] = flags;
        for i in 0..14 {
            b[2 + i] = (self.nib[2 * i] & 0xF) | (self.nib[2 * i + 1] << 4);
        }
        b
    }
}

#[inline(always)]
fn nearest(residual: i32, shift: u32) -> i32 {
    // Reconstruction step is 2^(12 - shift); round half away from zero.
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
    q.clamp(-8, 7)
}

fn greedy(x: &[i32; BLOCK_SAMPLES], state: (i32, i32), filter: usize, shift: u32) -> Block {
    let f = FILTERS[filter];
    let (mut s1, mut s2) = state;
    let mut nib = [0u8; BLOCK_SAMPLES];
    let mut err = 0i64;
    for i in 0..BLOCK_SAMPLES {
        let p = predict(f, s1, s2);
        let q = nearest(x[i] - p, shift);
        let v = (((q << 12) >> shift) + p).clamp(-0x8000, 0x7FFF);
        let d = (x[i] - v) as i64;
        err += d * d;
        nib[i] = (q & 0xF) as u8;
        s2 = s1;
        s1 = v;
    }
    Block {
        header: ((filter as u8) << 4) | shift as u8,
        nib,
        end: (s1, s2),
        err,
    }
}

#[derive(Clone, Copy)]
struct Path {
    s1: i32,
    s2: i32,
    err: i64,
    nib: [u8; BLOCK_SAMPLES],
}

fn trellis(
    x: &[i32; BLOCK_SAMPLES],
    state: (i32, i32),
    filter: usize,
    shift: u32,
    beam: usize,
    span: i32,
) -> Block {
    let f = FILTERS[filter];
    let mut paths = vec![Path {
        s1: state.0,
        s2: state.1,
        err: 0,
        nib: [0; BLOCK_SAMPLES],
    }];
    let mut next: Vec<Path> = Vec::with_capacity(beam * (2 * span as usize + 1));
    for i in 0..BLOCK_SAMPLES {
        next.clear();
        for p in &paths {
            let pred = predict(f, p.s1, p.s2);
            let q0 = nearest(x[i] - pred, shift);
            for q in (q0 - span)..=(q0 + span) {
                if !(-8..=7).contains(&q) {
                    continue;
                }
                let v = (((q << 12) >> shift) + pred).clamp(-0x8000, 0x7FFF);
                let d = (x[i] - v) as i64;
                let mut np = *p;
                np.err += d * d;
                np.nib[i] = (q & 0xF) as u8;
                np.s2 = p.s1;
                np.s1 = v;
                next.push(np);
            }
        }
        // Keep the best path per predictor state, then the best `beam`.
        next.sort_unstable_by_key(|p| (p.s1, p.s2, p.err));
        next.dedup_by_key(|p| (p.s1, p.s2));
        next.sort_by_key(|p| (p.err, p.s1, p.s2));
        next.truncate(beam.max(1));
        std::mem::swap(&mut paths, &mut next);
    }
    let best = paths[0];
    Block {
        header: ((filter as u8) << 4) | shift as u8,
        nib: best.nib,
        end: (best.s1, best.s2),
        err: best.err,
    }
}

fn best_greedy(x: &[i32; BLOCK_SAMPLES], state: (i32, i32), filters: &[usize]) -> Vec<Block> {
    let mut all = Vec::with_capacity(filters.len() * 13);
    for &filter in filters {
        for shift in 0..=12 {
            all.push(greedy(x, state, filter, shift));
        }
    }
    all.sort_by_key(|b| (b.err, b.header));
    all
}

fn block_at(samples: &[i32], index: usize) -> [i32; BLOCK_SAMPLES] {
    let mut x = [0i32; BLOCK_SAMPLES];
    let start = index * BLOCK_SAMPLES;
    for (i, v) in x.iter_mut().enumerate() {
        *v = samples.get(start + i).copied().unwrap_or(0);
    }
    x
}

/// Encode 16-bit samples to ADPCM blocks. `loop_block` is the block a
/// hardware loop re-enters (forced to filter 0); flags are written by
/// [`set_flags`], this returns blocks with zero flags.
pub fn encode(samples: &[i16], loop_block: Option<usize>, opts: &EncodeOptions) -> Vec<u8> {
    let x: Vec<i32> = samples.iter().map(|&s| s as i32).collect();
    let blocks = samples.len().div_ceil(BLOCK_SAMPLES).max(1);
    let all_filters = [0usize, 1, 2, 3, 4];
    let mut out = Vec::with_capacity(blocks * BLOCK_BYTES);
    let mut state = (0i32, 0i32);
    for b in 0..blocks {
        let target = block_at(&x, b);
        let filters: &[usize] = if Some(b) == loop_block {
            &[0]
        } else {
            &all_filters
        };
        let ranked = best_greedy(&target, state, filters);
        let chosen = match opts.effort {
            Effort::Greedy => ranked[0],
            Effort::Trellis => {
                let mut refined: Vec<Block> = ranked
                    .iter()
                    .take(opts.refine.max(1))
                    .map(|g| {
                        let t = trellis(
                            &target,
                            state,
                            (g.header >> 4) as usize,
                            (g.header & 0xF) as u32,
                            opts.beam,
                            opts.span,
                        );
                        if t.err <= g.err {
                            t
                        } else {
                            *g
                        }
                    })
                    .collect();
                refined.sort_by_key(|b| (b.err, b.header));
                if opts.lookahead > 1 && b + 1 < blocks {
                    let next = block_at(&x, b + 1);
                    let next_filters: &[usize] = if Some(b + 1) == loop_block {
                        &[0]
                    } else {
                        &all_filters
                    };
                    *refined
                        .iter()
                        .take(opts.lookahead)
                        .min_by_key(|c| {
                            (
                                c.err + best_greedy(&next, c.end, next_filters)[0].err,
                                c.err,
                                c.header,
                            )
                        })
                        .expect("at least one candidate")
                } else {
                    refined[0]
                }
            }
        };
        out.extend_from_slice(&chosen.bytes(0));
        state = chosen.end;
    }
    out
}

/// Write block flags: a one-shot ends with END on its last block; a loop
/// marks `loop_block` LOOP_START and ends with END|REPEAT.
pub fn set_flags(adpcm: &mut [u8], loop_block: Option<usize>) {
    let blocks = adpcm.len() / BLOCK_BYTES;
    for b in 0..blocks {
        adpcm[b * BLOCK_BYTES + 1] = 0;
    }
    if blocks == 0 {
        return;
    }
    let last = (blocks - 1) * BLOCK_BYTES + 1;
    match loop_block {
        Some(l) if l < blocks => {
            adpcm[l * BLOCK_BYTES + 1] |= FLAG_LOOP_START;
            adpcm[last] |= FLAG_END | FLAG_REPEAT;
        }
        _ => adpcm[last] |= FLAG_END,
    }
}
