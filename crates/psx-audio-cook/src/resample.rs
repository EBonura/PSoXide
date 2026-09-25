//! Band-limited resampling (Kaiser-windowed sinc).
//!
//! Downsampling must low-pass below the new Nyquist first; the nearest-sample
//! and two-tap linear resamplers the games used before fold everything above
//! it back into the audible band as aliasing, which is most of what made
//! low-rate samples sound harsh.

/// Zero crossings of the sinc kept on each side of the centre tap.
const ZEROS: f64 = 20.0;
/// Kaiser window shape (about 80 dB stop band).
const BETA: f64 = 8.0;
/// Pass band edge as a fraction of the lower of the two Nyquist rates.
pub const ROLLOFF: f64 = 0.92;
const TABLE: usize = 4096;

/// What the signal holds outside `0..len`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Edge {
    /// Silence before the start and after the end (one-shots).
    Zero,
    /// Silence before the start; past the end the signal wraps to `start`
    /// (a hardware loop), so the filter sees the loop seam as the SPU plays it.
    Loop {
        /// First sample of the loop.
        start: usize,
    },
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..64 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// A windowed-sinc interpolator with a precomputed Kaiser window.
pub struct Sinc {
    window: Vec<f64>,
}

impl Default for Sinc {
    fn default() -> Self {
        Self::new()
    }
}

impl Sinc {
    /// Build the window table.
    pub fn new() -> Self {
        let norm = bessel_i0(BETA);
        let window = (0..=TABLE + 1)
            .map(|i| {
                let u = (i as f64 / TABLE as f64).min(1.0);
                bessel_i0(BETA * (1.0 - u * u).max(0.0).sqrt()) / norm
            })
            .collect();
        Self { window }
    }

    #[inline]
    fn win(&self, u: f64) -> f64 {
        let p = u.abs() * TABLE as f64;
        let i = p as usize;
        if i >= TABLE {
            return 0.0;
        }
        let f = p - i as f64;
        self.window[i] * (1.0 - f) + self.window[i + 1] * f
    }

    /// Value of the band-limited signal at fractional input position `t`,
    /// low-passed at `fc` cycles per input sample (at most 0.5).
    pub fn eval(&self, x: &[f64], edge: Edge, t: f64, fc: f64) -> f64 {
        let n = x.len() as isize;
        let half = ZEROS / (2.0 * fc);
        let lo = (t - half).ceil() as isize;
        let hi = (t + half).floor() as isize;
        let mut acc = 0.0;
        let mut wsum = 0.0;
        for i in lo..=hi {
            let d = t - i as f64;
            let arg = 2.0 * fc * d;
            let s = if arg.abs() < 1e-12 {
                1.0
            } else {
                (std::f64::consts::PI * arg).sin() / (std::f64::consts::PI * arg)
            };
            let w = s * self.win(d / half);
            wsum += w;
            let v = if i < 0 {
                0.0
            } else if i < n {
                x[i as usize]
            } else {
                match edge {
                    Edge::Zero => 0.0,
                    Edge::Loop { start } => {
                        let start = start as isize;
                        let len = n - start;
                        if len <= 0 {
                            0.0
                        } else {
                            x[(start + (i - n) % len) as usize]
                        }
                    }
                }
            };
            acc += v * w;
        }
        if wsum.abs() > 1e-9 {
            acc / wsum
        } else {
            0.0
        }
    }

    /// Resample `x` from `from` Hz to `to` Hz; the output has
    /// `round(len * to / from)` samples.
    pub fn resample(&self, x: &[f64], from: u32, to: u32) -> Vec<f64> {
        let out_len =
            ((x.len() as u64 * to as u64 + from as u64 / 2) / from as u64).max(1) as usize;
        // Exact rate ratio (not the span's), so long sounds do not drift
        // against a reference by the output length's rounding.
        let end = out_len as f64 * from as f64 / to as f64;
        self.resample_span(x, Edge::Zero, 0.0, end, out_len, from, to)
    }

    /// Resample the input span `[start, end)` (fractional input positions)
    /// into exactly `out_len` samples. `from` and `to` set the anti-alias
    /// cutoff; the span's own ratio sets the positions, so a loop can be
    /// stretched by a fraction of a sample to fill whole ADPCM blocks.
    pub fn resample_span(
        &self,
        x: &[f64],
        edge: Edge,
        start: f64,
        end: f64,
        out_len: usize,
        from: u32,
        to: u32,
    ) -> Vec<f64> {
        let fc = 0.5 * ROLLOFF * (to as f64 / from as f64).min(1.0);
        let step = (end - start) / out_len.max(1) as f64;
        (0..out_len)
            .map(|j| self.eval(x, edge, start + j as f64 * step, fc))
            .collect()
    }
}

/// Round and clamp to the signed 16-bit range.
pub fn to_i16(x: &[f64]) -> Vec<i16> {
    x.iter()
        .map(|&v| v.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16)
        .collect()
}

/// Taps either side of the centre of the Gaussian pre-compensation filter.
const COMP_HALF: usize = 6;
/// Largest boost the compensation may apply (dB).
const COMP_MAX_BOOST_DB: f64 = 9.0;

/// Symmetric FIR that pre-emphasises the top of the band so that, after the
/// SPU's Gaussian interpolation (about -3 dB at a quarter of the sample
/// rate and -8 dB at 0.4), the played response is flat up to the resampler's
/// pass band. Designed by least squares against the interpolator's own
/// response; the boost is capped at 9 dB.
pub fn gauss_compensation_taps() -> Vec<f64> {
    let n = COMP_HALF + 1;
    let grid: Vec<f64> = (0..=200).map(|i| i as f64 / 200.0 * 0.5).collect();
    let cap = 10f64.powf(COMP_MAX_BOOST_DB / 20.0);
    // Normal equations for c: minimise sum w |C(u) G(u) - T(u)|^2.
    let mut a = vec![vec![0.0; n]; n];
    let mut b = vec![0.0; n];
    for &u in &grid {
        let g = crate::spu_play::gauss_response(u);
        let pass = u <= 0.5 * ROLLOFF;
        let target = if pass { (1.0 / g).min(cap) * g } else { 0.0 };
        let weight = if pass { 1.0 } else { 0.05 };
        let basis: Vec<f64> = (0..n)
            .map(|k| if k == 0 { 1.0 } else { 2.0 * (2.0 * std::f64::consts::PI * u * k as f64).cos() } * g)
            .collect();
        for i in 0..n {
            b[i] += weight * basis[i] * target;
            for j in 0..n {
                a[i][j] += weight * basis[i] * basis[j];
            }
        }
    }
    // Gaussian elimination.
    for col in 0..n {
        let piv = (col..n)
            .max_by(|&x, &y| a[x][col].abs().total_cmp(&a[y][col].abs()))
            .unwrap();
        a.swap(col, piv);
        b.swap(col, piv);
        for row in 0..n {
            if row != col {
                let f = a[row][col] / a[col][col];
                for k in col..n {
                    a[row][k] -= f * a[col][k];
                }
                b[row] -= f * b[col];
            }
        }
    }
    let c: Vec<f64> = (0..n).map(|i| b[i] / a[i][i]).collect();
    let mut taps = vec![0.0; 2 * COMP_HALF + 1];
    for k in 0..n {
        taps[COMP_HALF + k] = c[k];
        taps[COMP_HALF - k] = c[k];
    }
    taps
}

/// Apply [`gauss_compensation_taps`] to a band-limited signal. `loop_start`
/// makes the filter wrap across a hardware loop's seam.
pub fn compensate_gauss(x: &[f64], loop_start: Option<usize>) -> Vec<f64> {
    let taps = gauss_compensation_taps();
    let n = x.len() as isize;
    let at = |i: isize| -> f64 {
        if i < 0 {
            0.0
        } else if i < n {
            x[i as usize]
        } else {
            match loop_start {
                Some(s) if (s as isize) < n => {
                    let s = s as isize;
                    x[(s + (i - n) % (n - s)) as usize]
                }
                _ => 0.0,
            }
        }
    };
    (0..n)
        .map(|i| {
            taps.iter()
                .enumerate()
                .map(|(k, t)| t * at(i + k as isize - COMP_HALF as isize))
                .sum()
        })
        .collect()
}
