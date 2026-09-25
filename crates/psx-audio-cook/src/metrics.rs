//! Objective quality measures.
//!
//! * [`snr_db`]: plain SNR of a decode against the signal the encoder was
//!   given (same rate, same length). Measures the ADPCM coder alone.
//! * [`si_snr_db`]: scale-invariant SNR (least-squares gain removed), for
//!   end-to-end comparisons where pipelines normalise differently.
//! * [`lsd_db`]: log-spectral distance, a common perceptual-ish proxy. Per
//!   23 ms frame at 44.1 kHz, the RMS difference in dB between the two power
//!   spectra over the reference's band, with a floor 50 dB under the frame's
//!   loudest bin so inaudible bins do not dominate; averaged over frames
//!   within 40 dB of the loudest frame. Lower is better; it rises both with
//!   coding noise and with high frequencies lost to a low sample rate.

use std::f64::consts::PI;

/// In-place radix-2 complex FFT (`re.len()` must be a power of two).
pub fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * PI / len as f64;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (s, c) = (ang * k as f64).sin_cos();
                let a = start + k;
                let b = a + len / 2;
                let tr = re[b] * c - im[b] * s;
                let ti = re[b] * s + im[b] * c;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len <<= 1;
    }
}

/// Power spectra of Hann-windowed frames (`frame` a power of two, 50%
/// overlap). Each entry holds `frame / 2 + 1` bins.
pub fn stft_power(x: &[f64], frame: usize) -> Vec<Vec<f64>> {
    let hop = frame / 2;
    let win: Vec<f64> = (0..frame)
        .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f64 / frame as f64).cos())
        .collect();
    let mut out = Vec::new();
    let mut start = 0;
    while start < x.len().max(1) {
        let mut re: Vec<f64> = (0..frame)
            .map(|i| x.get(start + i).copied().unwrap_or(0.0) * win[i])
            .collect();
        let mut im = vec![0.0; frame];
        fft(&mut re, &mut im);
        out.push(
            (0..=frame / 2)
                .map(|k| re[k] * re[k] + im[k] * im[k])
                .collect(),
        );
        start += hop;
    }
    out
}

/// SNR in dB of `test` against `reference` (common length only).
pub fn snr_db(reference: &[i16], test: &[i16]) -> f64 {
    let (mut sig, mut noise) = (0.0f64, 0.0f64);
    for (&r, &t) in reference.iter().zip(test) {
        sig += (r as f64).powi(2);
        noise += (r as f64 - t as f64).powi(2);
    }
    10.0 * (sig.max(1e-9) / noise.max(1e-9)).log10()
}

/// Scale-invariant SNR in dB: `test` is scaled by the least-squares gain
/// before comparison.
pub fn si_snr_db(reference: &[f64], test: &[f64]) -> f64 {
    let n = reference.len().min(test.len());
    let (mut rt, mut tt) = (0.0, 0.0);
    for i in 0..n {
        rt += reference[i] * test[i];
        tt += test[i] * test[i];
    }
    let g = if tt > 0.0 { rt / tt } else { 0.0 };
    let (mut sig, mut noise) = (0.0, 0.0);
    for i in 0..n {
        sig += reference[i] * reference[i];
        noise += (reference[i] - g * test[i]).powi(2);
    }
    10.0 * (sig.max(1e-9) / noise.max(1e-9)).log10()
}

/// Log-spectral distance in dB between two 44.1 kHz signals over
/// `0..=max_hz` (see the module notes). `test` is gain-matched first.
pub fn lsd_db(reference: &[f64], test: &[f64], max_hz: f64) -> f64 {
    const FRAME: usize = 1024;
    let n = reference.len().min(test.len());
    let t = gain_matched(&reference[..n], &test[..n]);
    let a = stft_power(&reference[..n], FRAME);
    let b = stft_power(&t, FRAME);
    let top = ((max_hz / 44_100.0 * FRAME as f64) as usize).clamp(1, FRAME / 2);
    let energy: Vec<f64> = a.iter().map(|f| f[1..=top].iter().sum::<f64>()).collect();
    let loudest = energy.iter().cloned().fold(0.0, f64::max);
    let (mut sum, mut count) = (0.0, 0usize);
    for (fi, (fa, fb)) in a.iter().zip(&b).enumerate() {
        if energy[fi] < loudest * 1e-4 || energy[fi] <= 0.0 {
            continue;
        }
        let peak = fa[1..=top].iter().cloned().fold(0.0, f64::max);
        let floor = peak * 1e-5;
        let mut acc = 0.0;
        for k in 1..=top {
            let d = 10.0 * ((fa[k] + floor) / (fb[k] + floor)).log10();
            acc += d * d;
        }
        sum += (acc / top as f64).sqrt();
        count += 1;
    }
    if count == 0 {
        0.0
    } else {
        sum / count as f64
    }
}

/// `test` scaled by the least-squares gain against `reference`. The signals
/// must be time-aligned (a stretched loop drifting against its reference
/// scores near silence); measure loops as one-shots.
fn gain_matched(reference: &[f64], test: &[f64]) -> Vec<f64> {
    let (mut rt, mut tt) = (0.0, 0.0);
    for i in 0..reference.len().min(test.len()) {
        rt += reference[i] * test[i];
        tt += test[i] * test[i];
    }
    let g = if tt > 0.0 { rt / tt } else { 1.0 };
    test.iter().map(|v| v * g).collect()
}

/// Critical-band edges in Hz (Zwicker).
const BARK_EDGES: [f64; 25] = [
    0.0, 100.0, 200.0, 300.0, 400.0, 510.0, 630.0, 770.0, 920.0, 1080.0, 1270.0, 1480.0, 1720.0,
    2000.0, 2320.0, 2700.0, 3150.0, 3700.0, 4400.0, 5300.0, 6400.0, 7700.0, 9500.0, 12000.0,
    15500.0,
];

/// Frequency-weighted segmental SNR in dB (Hu and Loizou's fwSNRseg), a
/// standard objective speech-quality measure that tracks listening tests
/// better than plain SNR. Per 23 ms frame (44.1 kHz) and critical band up to
/// `max_hz`: `10 log10(|X|^2 / (|X| - |Y|)^2)` on band magnitudes, clamped to
/// -10..35 dB, weighted by `|X|^0.2`. Because it compares magnitudes, both
/// missing high frequencies and aliasing that adds energy where the source
/// has none lower it. Higher is better. `test` is gain-matched first.
pub fn fw_snr_seg_db(reference: &[f64], test: &[f64], max_hz: f64) -> f64 {
    const FRAME: usize = 1024;
    let n = reference.len().min(test.len());
    let t = gain_matched(&reference[..n], &test[..n]);
    let a = stft_power(&reference[..n], FRAME);
    let b = stft_power(&t, FRAME);
    let bands: Vec<(usize, usize)> = BARK_EDGES
        .windows(2)
        .filter(|e| e[0] < max_hz)
        .map(|e| {
            let lo = ((e[0] / 44_100.0 * FRAME as f64).ceil() as usize).max(1);
            let hi = ((e[1].min(max_hz) / 44_100.0 * FRAME as f64).floor() as usize).max(lo);
            (lo, hi.min(FRAME / 2))
        })
        .collect();
    let energy: Vec<f64> = a.iter().map(|f| f.iter().sum::<f64>()).collect();
    let loudest = energy.iter().cloned().fold(0.0, f64::max);
    let (mut sum, mut count) = (0.0, 0usize);
    for (fi, (fa, fb)) in a.iter().zip(&b).enumerate() {
        if energy[fi] < loudest * 1e-4 || energy[fi] <= 0.0 {
            continue;
        }
        let (mut num, mut den) = (0.0, 0.0);
        for &(lo, hi) in &bands {
            let x = (fa[lo..=hi].iter().sum::<f64>() / (hi - lo + 1) as f64).sqrt();
            let y = (fb[lo..=hi].iter().sum::<f64>() / (hi - lo + 1) as f64).sqrt();
            let w = x.powf(0.2);
            let snr = if x <= 0.0 {
                -10.0
            } else {
                (10.0 * (x * x / ((x - y).powi(2)).max(1e-12)).log10()).clamp(-10.0, 35.0)
            };
            num += w * snr;
            den += w;
        }
        if den > 0.0 {
            sum += num / den;
            count += 1;
        }
    }
    if count == 0 {
        0.0
    } else {
        sum / count as f64
    }
}
