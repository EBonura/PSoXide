//! Test the audio link against simulated capture-card damage.
//!
//! The emulator's SPU output is a clean digital path, so decoding it proves the
//! encoding and framing but says nothing about surviving a real capture chain.
//! That chain resamples, band-limits, applies AGC, adds noise and sometimes
//! clips. This applies each of those to a known-good recording and checks the
//! decoder still recovers the payload, which converts "untested until we burn a
//! disc" into a concrete pass/fail matrix.
//!
//!     psoxide-hwtest audio-chaintest build/hwtest-audio.wav
//!
//! It cannot prove the real chain works. It can prove the decoder is not brittle
//! against the specific degradations a capture chain is known to introduce,
//! which is the part that would otherwise cost a burn to discover.
//!
//! The noise tiers use a fixed-seed generator of this crate's own, so a run is
//! repeatable but its noise is not NumPy's: the matrix tests a noise level, not
//! one particular noise sequence.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::audio_decode::{read_wav_file, resample};
use crate::util::{Error, Result};

/// Mono samples, channels averaged.
fn read_wav(path: &Path) -> Result<(Vec<f64>, u32)> {
    let wav = read_wav_file(path)?;
    let channels = usize::from(wav.channels.max(1));
    let values: Vec<f64> = wav.samples.iter().map(|&s| f64::from(s)).collect();
    if channels > 1 {
        let usable = values.len() - values.len() % channels;
        let mean = values[..usable]
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f64>() / channels as f64)
            .collect();
        return Ok((mean, wav.rate));
    }
    Ok((values, wav.rate))
}

fn write_wav(path: &Path, samples: &[f64], rate: u32) -> Result<()> {
    let pcm: Vec<u8> = samples
        .iter()
        .flat_map(|&s| (s.clamp(-32768.0, 32767.0) as i16).to_le_bytes())
        .collect();
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend(b"RIFF");
    out.extend((36 + pcm.len() as u32).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes()); // PCM
    out.extend(1u16.to_le_bytes()); // mono
    out.extend(rate.to_le_bytes());
    out.extend((rate * 2).to_le_bytes());
    out.extend(2u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend((pcm.len() as u32).to_le_bytes());
    out.extend(pcm);
    std::fs::write(path, out)?;
    Ok(())
}

/// Crude low-pass: a moving average. Models a chain that rolls off the upper
/// tone harder than the lower one, which is the asymmetry most likely to bias
/// an FSK decision.
fn band_limit(samples: &[f64], taps: usize) -> Vec<f64> {
    let half = taps / 2;
    (0..samples.len())
        .map(|i| {
            let from = i.saturating_sub(half);
            let to = (i + half + 1).min(samples.len());
            samples[from..to].iter().sum::<f64>() / taps as f64
        })
        .collect()
}

/// A repeatable normal-noise source (SplitMix64 with Box-Muller).
struct Noise(u64);

impl Noise {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn uniform(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }

    fn normal(&mut self, sigma: f64) -> f64 {
        let (u, v) = (self.uniform(), self.uniform());
        sigma * (-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos()
    }
}

fn peak(samples: &[f64]) -> f64 {
    samples.iter().fold(0.0f64, |m, s| m.max(s.abs()))
}

fn noisy(samples: &[f64], seed: u64, fraction: f64) -> Vec<f64> {
    let mut noise = Noise(seed);
    let sigma = fraction * peak(samples);
    samples.iter().map(|s| s + noise.normal(sigma)).collect()
}

type Transform = fn(&[f64], u32) -> (Vec<f64>, u32);

const CASES: [(&str, Transform); 12] = [
    ("baseline", |s, r| (s.to_vec(), r)),
    // The most likely real failure: OBS and most capture cards default to 48k.
    ("resample_48k", |s, r| (resample(s, r, 48000), 48000)),
    ("resample_32k", |s, r| (resample(s, r, 32000), 32000)),
    ("resample_96k", |s, r| (resample(s, r, 96000), 96000)),
    // Gain extremes: FSK should be immune, since it decides on which tone wins.
    ("gain_x0.05", |s, r| (s.iter().map(|v| v * 0.05).collect(), r)),
    ("gain_x8_clipped", |s, r| {
        (s.iter().map(|v| (v * 8.0).clamp(-32768.0, 32767.0)).collect(), r)
    }),
    ("dc_offset", |s, r| (s.iter().map(|v| v + 3000.0).collect(), r)),
    ("band_limit_5tap", |s, r| (band_limit(s, 5), r)),
    ("band_limit_9tap", |s, r| (band_limit(s, 9), r)),
    ("noise_10pct", |s, r| (noisy(s, 1, 0.10), r)),
    ("noise_25pct", |s, r| (noisy(s, 2, 0.25), r)),
    ("worst_case_stack", |s, r| {
        let resampled = resample(s, r, 48000);
        let mut noise = Noise(3);
        let sigma = 0.15 * peak(s);
        let stacked: Vec<f64> = resampled.iter().map(|v| v + noise.normal(sigma)).collect();
        (band_limit(&stacked, 5).iter().map(|v| v * 0.3).collect(), 48000)
    }),
];

/// `psoxide-hwtest audio-chaintest <wav> [--workdir DIR]`
pub fn run(args: &[String], out: &mut dyn Write) -> Result<i32> {
    let mut wav = None;
    let mut workdir: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--workdir" => {
                i += 1;
                workdir = Some(PathBuf::from(
                    args.get(i).ok_or_else(|| Error("--workdir needs a value".into()))?,
                ));
            }
            flag if flag.starts_with("--") => bail!("unrecognized arguments: {flag}"),
            path if wav.is_none() => wav = Some(PathBuf::from(path)),
            extra => bail!("unrecognized arguments: {extra}"),
        }
        i += 1;
    }
    let source = wav.ok_or_else(|| Error("the following arguments are required: wav".into()))?;
    let (samples, rate) = read_wav(&source)?;
    let workdir = workdir.unwrap_or_else(|| {
        source.parent().unwrap_or(Path::new(".")).join("chaintest")
    });
    std::fs::create_dir_all(&workdir)?;
    writeln!(
        out,
        "# source {} ({:.1}s at {rate} Hz)",
        source.display(),
        samples.len() as f64 / f64::from(rate)
    )?;
    let this = std::env::current_exe()?;
    let mut failures = 0;
    for (name, transform) in CASES {
        let (degraded, out_rate) = transform(&samples, rate);
        let path = workdir.join(format!("{name}.wav"));
        write_wav(&path, &degraded, out_rate)?;
        let result = Command::new(&this).arg("audio-decode").arg(&path).output()?;
        let ok = result.status.success();
        let stdout = String::from_utf8_lossy(&result.stdout);
        let mut detail = "";
        for line in stdout.lines() {
            if let Some(crc) = line.strip_prefix("# crc32=") {
                detail = crc;
            }
        }
        if !ok {
            failures += 1;
        }
        writeln!(out, "{}  {name:22} {detail}", if ok { "PASS" } else { "FAIL" })?;
    }
    writeln!(out, "# {}/{} degradations decoded", CASES.len() - failures, CASES.len())?;
    Ok(i32::from(failures != 0))
}
