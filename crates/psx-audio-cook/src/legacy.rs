//! Bit-exact copies of the pipelines the games used before this crate, kept
//! only so A/B measurements and tests can reproduce shipped banks.
//!
//! * `psxed-audio` (PSoXide-editor): two-tap linear resampler, peak
//!   normalisation in `f32`, greedy 5x13 search whose history is not clamped
//!   (the SPU clamps it, so loud blocks decode differently than the encoder
//!   assumed).
//! * hl-psx / cs-psx: a nearest-sample resampler in front of `psxed-audio`.

use crate::adpcm::{BLOCK_BYTES, BLOCK_SAMPLES, FILTERS};

/// hl-content's nearest-sample resampler.
pub fn resample_nearest(input: &[i16], from: u32, to: u32) -> Vec<i16> {
    if from == to {
        return input.to_vec();
    }
    let count = ((input.len() as u64 * to as u64) / from.max(1) as u64).max(1) as usize;
    (0..count)
        .map(|i| {
            let src =
                ((i as u64 * from as u64) / to as u64).min(input.len().saturating_sub(1) as u64);
            input[src as usize]
        })
        .collect()
}

/// psxed-audio's two-tap linear resampler.
pub fn resample_linear(samples: &[i16], from: u32, to: u32) -> Vec<i16> {
    if from == to || samples.len() < 2 {
        return samples.to_vec();
    }
    let out_len =
        (((samples.len() as u64 * to as u64) + (from as u64 / 2)) / from as u64).max(1) as usize;
    (0..out_len)
        .map(|i| {
            let pos = (i as f64) * (from as f64) / (to as f64);
            let lo = pos.floor() as usize;
            let hi = (lo + 1).min(samples.len() - 1);
            let t = pos - lo as f64;
            let (a, b) = (samples[lo] as f64, samples[hi] as f64);
            (a + (b - a) * t)
                .round()
                .clamp(i16::MIN as f64, i16::MAX as f64) as i16
        })
        .collect()
}

/// psxed-audio's peak normalisation.
pub fn normalize_to_peak(samples: &mut [i16], target: f32) {
    let peak = samples.iter().map(|&s| (s as i32).abs()).max().unwrap_or(0);
    if peak == 0 || target <= 0.0 {
        return;
    }
    let scale = (target.clamp(0.0, 1.0) * i16::MAX as f32).round() / peak as f32;
    for s in samples {
        *s = (*s as f32 * scale)
            .round()
            .clamp(i16::MIN as f32, i16::MAX as f32) as i16;
    }
}

fn round_div(num: i32, den: i32) -> i32 {
    if num >= 0 {
        (num + den / 2) / den
    } else {
        -((-num + den / 2) / den)
    }
}

/// psxed-audio's encoder: greedy, unclamped history, last block flagged END.
pub fn encode_psxed(samples: &[i16]) -> Vec<u8> {
    let blocks = samples.len().div_ceil(BLOCK_SAMPLES).max(1);
    let mut out = Vec::with_capacity(blocks * BLOCK_BYTES);
    let mut state = (0i32, 0i32);
    for b in 0..blocks {
        let mut x = [0i32; BLOCK_SAMPLES];
        for (i, v) in x.iter_mut().enumerate() {
            *v = samples.get(b * BLOCK_SAMPLES + i).copied().unwrap_or(0) as i32;
        }
        let mut best: Option<([u8; BLOCK_BYTES], (i32, i32), i64)> = None;
        for (filter, &(f1, f2)) in FILTERS.iter().enumerate() {
            for shift in 0..=12u32 {
                let (mut s1, mut s2) = state;
                let mut err = 0i64;
                let mut bytes = [0u8; BLOCK_BYTES];
                bytes[0] = ((filter as u8) << 4) | shift as u8;
                bytes[1] = if b + 1 == blocks { 0x01 } else { 0x00 };
                for (i, &sample) in x.iter().enumerate() {
                    let p = ((s1 * f1) >> 6) + ((s2 * f2) >> 6);
                    let q = round_div((sample - p) * (1 << shift), 4096).clamp(-8, 7);
                    let v = ((q << 12) >> shift) + p;
                    err += (sample as i64 - v as i64).pow(2);
                    let nib = (q as i8 as u8) & 0x0F;
                    bytes[2 + i / 2] |= if i & 1 == 0 { nib } else { nib << 4 };
                    s2 = s1;
                    s1 = v;
                }
                if best.is_none_or(|(_, _, e)| err < e) {
                    best = Some((bytes, (s1, s2), err));
                }
            }
        }
        let (bytes, end, _) = best.expect("65 candidates");
        out.extend_from_slice(&bytes);
        state = end;
    }
    out
}

/// The hl-psx cook of one sound: nearest resample, then psxed-audio at the
/// same rate (its linear pass is then a no-op), normalised to `peak`.
/// Returns the encoder's input and its ADPCM.
pub fn hl_cook(source: &[i16], source_rate: u32, rate: u32, peak: f32) -> (Vec<i16>, Vec<u8>) {
    let mut pcm = resample_nearest(source, source_rate, rate);
    normalize_to_peak(&mut pcm, peak);
    let adpcm = encode_psxed(&pcm);
    (pcm, adpcm)
}
