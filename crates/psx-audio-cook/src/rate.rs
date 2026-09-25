//! Per-sound sample-rate choice and SPU RAM budget fitting.
//!
//! ADPCM costs a fixed 16 bytes per 28 samples, so the only size lever is
//! the sample rate, and what a lower rate costs depends on the sound: a deep
//! voice or a rumble loses almost nothing at 5 kHz, a hiss or a sibilant
//! voice loses a lot. [`band_loss`] measures that per sound from its own
//! spectrum: the predicted quality drop between the source and the source
//! band-limited to each candidate rate, on the scale of
//! [`crate::metrics::fw_snr_seg_db`], before any coding noise.
//!
//! [`allocate`] then fits a bank into a byte budget by minimising
//! `sum(weight * seconds * loss)`: time-weighted, so a sound heard for longer
//! counts for more, and weighted by class so dialogue outranks machinery.
//! Starting from every sound's best rate, it repeatedly takes the step down
//! that costs the least weighted loss per byte saved, then spends any bytes
//! left over on the steps up that buy the most. Because the byte cost of a
//! step is proportional to duration, this equalises the marginal loss per
//! hertz across sounds instead of starving long lines, which is what a
//! "largest saving first" rule does.

use crate::metrics::stft_power;
use crate::resample::ROLLOFF;

/// Default rate ladder, high to low.
pub const LADDER: [u32; 20] = [
    22_050, 18_900, 16_000, 13_000, 11_025, 10_000, 9_000, 8_000, 7_000, 6_000, 5_500, 5_000,
    4_500, 4_000, 3_600, 3_200, 2_800, 2_400, 2_000, 1_600,
];

/// Critical-band edges in Hz (Zwicker), as in [`crate::metrics::fw_snr_seg_db`].
const BARK_EDGES: [f64; 25] = [
    0.0, 100.0, 200.0, 300.0, 400.0, 510.0, 630.0, 770.0, 920.0, 1080.0, 1270.0, 1480.0, 1720.0,
    2000.0, 2320.0, 2700.0, 3150.0, 3700.0, 4400.0, 5300.0, 6400.0, 7700.0, 9500.0, 12000.0,
    15500.0,
];

/// Ceiling of the band SNR (the fwSNRseg clamp).
pub const LOSS_CEILING_DB: f64 = 35.0;

/// Predicted quality loss (dB of fwSNRseg, see [`crate::metrics`]) of
/// band-limiting `samples` (at `source_rate`) to each rate in `rates`,
/// computed from the source's own spectrum without encoding: per 23 ms frame
/// and critical band, the band SNR left after removing what the resampler's
/// low-pass removes, weighted as fwSNRseg weights it, ignoring bands more
/// than 50 dB under the frame's loudest band. Zero means nothing
/// audible is lost; rates at or above the source rate lose nothing.
pub fn band_loss(samples: &[f64], source_rate: u32, rates: &[u32]) -> Vec<f64> {
    let frame = (source_rate as f64 * 0.023).max(64.0).round() as usize;
    let frame = frame.next_power_of_two();
    let spectra = stft_power(samples, frame);
    let bins = frame / 2;
    let hz = |k: usize| k as f64 * source_rate as f64 / frame as f64;
    let bands: Vec<(usize, usize)> = BARK_EDGES
        .windows(2)
        .filter(|e| e[0] < source_rate as f64 / 2.0)
        .map(|e| {
            let lo = ((e[0] * frame as f64 / source_rate as f64).ceil() as usize).max(1);
            let hi = ((e[1] * frame as f64 / source_rate as f64).floor() as usize).clamp(lo, bins);
            (lo, hi)
        })
        .collect();
    let energy: Vec<f64> = spectra.iter().map(|f| f.iter().sum()).collect();
    let loudest = energy.iter().cloned().fold(0.0, f64::max);
    rates
        .iter()
        .map(|&rate| {
            if rate >= source_rate || loudest <= 0.0 {
                return 0.0;
            }
            let nyq = rate as f64 / 2.0;
            let pass = nyq * ROLLOFF;
            let h = |k: usize| {
                let f = hz(k);
                if f <= pass {
                    1.0
                } else if f >= nyq {
                    0.0
                } else {
                    (nyq - f) / (nyq - pass)
                }
            };
            let (mut sum, mut count) = (0.0, 0usize);
            for (fi, f) in spectra.iter().enumerate() {
                if energy[fi] < loudest * 1e-4 {
                    continue;
                }
                let level = |lo: usize, hi: usize| {
                    (f[lo..=hi].iter().sum::<f64>() / (hi - lo + 1) as f64).sqrt()
                };
                // Bands 50 dB under the frame's loudest band are inaudible
                // beside it (and in 8-bit sources are mostly quantisation
                // noise); losing them costs nothing.
                let audible = bands
                    .iter()
                    .map(|&(lo, hi)| level(lo, hi))
                    .fold(0.0, f64::max)
                    * 0.003_16;
                let (mut num, mut den) = (0.0, 0.0);
                for &(lo, hi) in &bands {
                    let width = (hi - lo + 1) as f64;
                    let x = level(lo, hi);
                    if x < audible {
                        continue;
                    }
                    let y = ((lo..=hi).map(|k| f[k] * h(k) * h(k)).sum::<f64>() / width).sqrt();
                    let snr = if x <= 0.0 {
                        continue;
                    } else if x - y <= x * 1e-4 {
                        LOSS_CEILING_DB
                    } else {
                        (20.0 * (x / (x - y)).log10()).clamp(-10.0, LOSS_CEILING_DB)
                    };
                    let w = x.powf(0.2);
                    num += w * snr;
                    den += w;
                }
                if den > 0.0 {
                    sum += LOSS_CEILING_DB - num / den;
                    count += 1;
                }
            }
            if count == 0 {
                0.0
            } else {
                sum / count as f64
            }
        })
        .collect()
}

/// One sound competing for a bank.
#[derive(Clone, Debug)]
pub struct Candidate {
    /// Bytes at each ladder step (non-increasing).
    pub bytes: Vec<usize>,
    /// Loss at each ladder step (made non-decreasing by [`allocate`]).
    pub loss: Vec<f64>,
    /// Class weight times duration in seconds.
    pub weight: f64,
    /// Lowest step this sound may take (inclusive).
    pub max_step: usize,
}

/// Choose a ladder step per candidate so the total bytes (plus
/// `overhead`) fit `budget`. Returns `None` when even every sound's lowest
/// allowed step does not fit.
pub fn allocate(cands: &[Candidate], overhead: usize, budget: usize) -> Option<Vec<usize>> {
    let loss: Vec<Vec<f64>> = cands
        .iter()
        .map(|c| {
            let mut run = 0.0f64;
            c.loss
                .iter()
                .map(|&l| {
                    run = run.max(l);
                    run
                })
                .collect()
        })
        .collect();
    let total = |steps: &[usize]| -> usize {
        overhead
            + cands
                .iter()
                .zip(steps)
                .map(|(c, &s)| c.bytes[s])
                .sum::<usize>()
    };
    let floor: Vec<usize> = cands
        .iter()
        .map(|c| c.max_step.min(c.bytes.len() - 1))
        .collect();
    if total(&floor) > budget {
        return None;
    }
    let mut steps = vec![0usize; cands.len()];
    let mut size = total(&steps);
    // Descend: cheapest weighted loss per byte saved.
    while size > budget {
        let mut best: Option<(f64, usize)> = None;
        for (i, c) in cands.iter().enumerate() {
            let s = steps[i];
            if s >= floor[i] {
                continue;
            }
            let saved = c.bytes[s].saturating_sub(c.bytes[s + 1]);
            if saved == 0 {
                steps[i] += 1; // free step
                best = None;
                break;
            }
            let cost = c.weight * (loss[i][s + 1] - loss[i][s]) / saved as f64;
            if best.is_none_or(|(b, _)| cost < b) {
                best = Some((cost, i));
            }
        }
        if let Some((_, i)) = best {
            steps[i] += 1;
        }
        size = total(&steps);
    }
    // Ascend: spend what is left on the most valuable steps up.
    loop {
        let spare = budget - size;
        let mut best: Option<(f64, usize)> = None;
        for (i, c) in cands.iter().enumerate() {
            let s = steps[i];
            if s == 0 {
                continue;
            }
            let extra = c.bytes[s - 1].saturating_sub(c.bytes[s]);
            if extra > spare {
                continue;
            }
            let gain = c.weight * (loss[i][s] - loss[i][s - 1]) / extra.max(1) as f64;
            if gain > 0.0 && best.is_none_or(|(b, _)| gain > b) {
                best = Some((gain, i));
            }
        }
        match best {
            Some((_, i)) => {
                steps[i] -= 1;
                size = total(&steps);
            }
            None => break,
        }
    }
    Some(steps)
}
