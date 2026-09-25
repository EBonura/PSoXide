//! Shared PS1 SPU-ADPCM cooker (host side).
//!
//! One encoder for every game, so a quality fix lands everywhere:
//!
//! * [`wav`]: reads 8/16/24/32-bit and float WAVs with their `smpl`/`cue `
//!   loop points.
//! * [`resample`]: Kaiser-windowed sinc; low-passes before it downsamples.
//! * [`adpcm`]: a decoder that matches the SPU (clamped history) and a
//!   trellis encoder with one block of lookahead, evaluated against that
//!   decoder.
//! * [`rate`]: per-sound band loss for each candidate rate and a
//!   distortion-aware allocator that fits a bank into a byte budget.
//! * [`metrics`], [`spu_play`]: SNR, log-spectral distance, and a model of
//!   the voice's Gaussian interpolation so measurements reflect playback.
//! * [`legacy`]: the previous pipelines, bit-exact, for A/B only.
//! * [`cli`]: the command line, callable from any workspace.
//!
//! [`cook`] runs the whole chain for one sound and [`psau`] wraps the result
//! in the PSAU container every runtime already parses.

#![allow(clippy::needless_range_loop)]

pub mod adpcm;
pub mod cli;
pub mod legacy;
pub mod metrics;
pub mod rate;
pub mod resample;
pub mod spu_play;
pub mod wav;

pub use adpcm::{Effort, EncodeOptions};
pub use wav::Wav;

use adpcm::BLOCK_SAMPLES;
use psxed_format::audio;
use resample::{Edge, Sinc};

/// Errors from parsing sources.
#[derive(Debug)]
pub enum Error {
    /// The WAV could not be parsed.
    Wav(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Wav(m) => write!(f, "WAV: {m}"),
        }
    }
}

impl std::error::Error for Error {}

/// How the cooked sample repeats.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Looping {
    /// One-shot: the last block ends the voice.
    None,
    /// The whole sample repeats. Its length is rounded to whole ADPCM blocks
    /// (a pitch change under half a block over the sample) so no zero padding
    /// plays at the seam.
    Whole,
    /// Repeat from the source's own loop start (`smpl` or `cue `), or play
    /// once when the source declares none. The pre-roll is padded at the
    /// front by under one block so the loop starts on a block boundary, and
    /// the loop is stretched to whole blocks.
    Source,
}

/// Settings for [`cook`].
#[derive(Clone, Debug)]
pub struct CookOptions {
    /// Output sample rate in Hz.
    pub rate: u32,
    /// Scale so the peak reaches this fraction of full scale (`None` keeps
    /// the source level).
    pub normalize_peak: Option<f64>,
    /// Loop handling.
    pub looping: Looping,
    /// Pre-emphasise the top of the band so the SPU's Gaussian interpolation
    /// plays it back flat ([`resample::compensate_gauss`]).
    pub compensate_gauss: bool,
    /// Encoder settings.
    pub encode: EncodeOptions,
}

impl CookOptions {
    /// One-shot at `rate`, normalised to 0.9 like the previous pipeline.
    pub fn one_shot(rate: u32) -> Self {
        Self {
            rate,
            normalize_peak: Some(0.9),
            looping: Looping::None,
            compensate_gauss: true,
            encode: EncodeOptions::default(),
        }
    }
}

/// One cooked sound.
#[derive(Clone, Debug)]
pub struct Cooked {
    /// Playback rate in Hz.
    pub rate: u32,
    /// The 16-bit signal the encoder was given.
    pub pcm: Vec<i16>,
    /// ADPCM blocks with flags set.
    pub adpcm: Vec<u8>,
    /// Block the hardware loop re-enters.
    pub loop_block: Option<usize>,
}

impl Cooked {
    /// ADPCM decoded as the SPU plays it (whole blocks).
    pub fn decoded(&self) -> Vec<i16> {
        adpcm::decode(&self.adpcm)
    }
}

/// Number of output samples for `len` source samples.
pub fn resampled_len(len: usize, from: u32, to: u32) -> usize {
    ((len as u64 * to as u64 + from as u64 / 2) / from as u64).max(1) as usize
}

/// Bytes of ADPCM for `samples` output samples.
pub fn adpcm_bytes(samples: usize) -> usize {
    samples.div_ceil(BLOCK_SAMPLES).max(1) * adpcm::BLOCK_BYTES
}

/// Output sample count [`cook`] produces for `wav` at `rate` with `looping`.
pub fn cooked_len(wav: &Wav, rate: u32, looping: Looping) -> usize {
    plan(wav, rate, looping).total
}

struct Plan {
    pad: usize,
    pre: usize,
    body: usize,
    total: usize,
    loop_block: Option<usize>,
    loop_start: Option<usize>,
    end: usize,
}

fn round_blocks(n: usize) -> usize {
    (((n + BLOCK_SAMPLES / 2) / BLOCK_SAMPLES).max(1)) * BLOCK_SAMPLES
}

fn plan(wav: &Wav, rate: u32, looping: Looping) -> Plan {
    let n = wav.samples.len();
    let source_loop = match looping {
        Looping::None => None,
        Looping::Whole => Some((0, n)),
        Looping::Source => wav.loop_start.map(|s| (s, wav.loop_end.unwrap_or(n))),
    };
    match source_loop {
        None => {
            let total = resampled_len(n, wav.rate, rate);
            Plan {
                pad: 0,
                pre: total,
                body: 0,
                total,
                loop_block: None,
                loop_start: None,
                end: n,
            }
        }
        Some((ls, le)) => {
            let pre = if ls == 0 {
                0
            } else {
                resampled_len(ls, wav.rate, rate)
            };
            let pad = (BLOCK_SAMPLES - pre % BLOCK_SAMPLES) % BLOCK_SAMPLES;
            let body = round_blocks(resampled_len(le - ls, wav.rate, rate));
            Plan {
                pad,
                pre,
                body,
                total: pad + pre + body,
                loop_block: Some((pad + pre) / BLOCK_SAMPLES),
                loop_start: Some(ls),
                end: le,
            }
        }
    }
}

/// Resample, normalise, encode and flag one sound.
pub fn cook(wav: &Wav, opts: &CookOptions) -> Cooked {
    let sinc = Sinc::new();
    let p = plan(wav, opts.rate, opts.looping);
    let src = &wav.samples[..p.end];
    let mut pcm = vec![0.0f64; p.pad];
    match p.loop_start {
        None => pcm.extend(sinc.resample(src, wav.rate, opts.rate)),
        Some(ls) => {
            let edge = Edge::Loop { start: ls };
            if p.pre > 0 {
                pcm.extend(
                    sinc.resample_span(src, edge, 0.0, ls as f64, p.pre, wav.rate, opts.rate),
                );
            }
            pcm.extend(sinc.resample_span(
                src,
                edge,
                ls as f64,
                src.len() as f64,
                p.body,
                wav.rate,
                opts.rate,
            ));
        }
    }
    // The normalising gain comes from the source's own peak, as the old
    // pipelines' did in effect (a nearest or linear resample keeps the
    // source's samples, so its peak). The band-limited resample's peak is
    // lower whenever the sound has energy above the new Nyquist, and
    // normalising that would turn up what is left: a flashlight click at
    // 5 kHz came out 8 dB louder. The pre-emphasis does not change the gain
    // either: the few samples it pushes past full scale are clamped
    // (to_i16). Lowering the gain to fit them cost loud, bright sounds up to
    // 7.5 dB of playback level (Counter-Strike's gunshots) and measured no
    // better.
    let peak_of = |v: &[f64]| v.iter().fold(0.0f64, |m, x| m.max(x.abs()));
    let gain = match opts.normalize_peak {
        Some(target) if peak_of(src) > 0.0 => target.clamp(0.0, 1.0) * 32767.0 / peak_of(src),
        _ => 1.0,
    };
    if opts.compensate_gauss {
        let loop_start = p.loop_block.map(|b| b * BLOCK_SAMPLES);
        pcm = resample::compensate_gauss(&pcm, loop_start);
    }
    pcm.iter_mut().for_each(|v| *v *= gain);
    let pcm = resample::to_i16(&pcm);
    let mut adpcm = adpcm::encode(&pcm, p.loop_block, &opts.encode);
    adpcm::set_flags(&mut adpcm, p.loop_block);
    Cooked {
        rate: opts.rate,
        pcm,
        adpcm,
        loop_block: p.loop_block,
    }
}

/// Wrap ADPCM in the PSAU container (version 1, mono, one-shot header; loops
/// live in the ADPCM block flags, which is what the runtimes parse today).
pub fn psau(rate: u32, sample_count: usize, adpcm: &[u8]) -> Vec<u8> {
    let blocks = (adpcm.len() / adpcm::BLOCK_BYTES) as u32;
    let payload = (audio::AudioHeader::SIZE + adpcm.len()) as u32;
    let flags = audio::flags::MONO | audio::flags::ONE_SHOT;
    let mut out = Vec::with_capacity(12 + payload as usize);
    out.extend_from_slice(&audio::MAGIC);
    out.extend_from_slice(&audio::VERSION.to_le_bytes());
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&payload.to_le_bytes());
    out.push(audio::CODEC_SPU_ADPCM);
    out.push(1);
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(sample_count as u32).to_le_bytes());
    out.extend_from_slice(&blocks.to_le_bytes());
    out.extend_from_slice(&audio::AudioHeader::NO_LOOP.to_le_bytes());
    out.extend_from_slice(adpcm);
    out
}

/// Playback of a cooked sound at 44.1 kHz through the SPU interpolator,
/// as `f64`, trimmed so sample `n` lines up with source time `n / 44100`.
pub fn playback(c: &Cooked) -> Vec<f64> {
    spu_play::play(&c.decoded(), c.rate)
        .iter()
        .map(|&v| v as f64)
        .collect()
}

/// The source band-limited and resampled to 44.1 kHz, the reference for
/// end-to-end measurements.
pub fn reference_44k(wav: &Wav) -> Vec<f64> {
    Sinc::new().resample(&wav.samples, wav.rate, 44_100)
}

/// End-to-end quality of ADPCM that plays at `rate` Hz, against its source.
#[derive(Clone, Debug)]
pub struct Score {
    /// fwSNRseg (dB, higher is better) of the playback against
    /// [`reference_44k`], over the band up to half the source rate, capped
    /// at 11,025 Hz: the measure every rollout comparison uses.
    pub fw_snr_seg_db: f64,
    /// Scale-invariant SNR (dB) over the same span.
    pub si_snr_db: f64,
    /// The playback at 44.1 kHz through the SPU interpolator model.
    pub played: Vec<i16>,
}

/// Score any ADPCM (a shipped bank entry, or a cook) against its source:
/// decode it as the SPU does, drop `skip` leading samples (at `rate`, e.g.
/// the pre-roll padding a source loop adds), play it through the voice's
/// Gaussian interpolator, and measure against the band-limited source. Flags
/// are ignored, so a loop is measured over one pass; a loop stretched to
/// whole blocks drifts against its source by under half a block.
pub fn score(source: &Wav, adpcm: &[u8], rate: u32, skip: usize) -> Score {
    let decoded = adpcm::decode(adpcm);
    let played = spu_play::play(&decoded[skip.min(decoded.len())..], rate);
    let reference = reference_44k(source);
    let test: Vec<f64> = played.iter().map(|&v| v as f64).collect();
    let n = reference.len().min(test.len());
    let max_hz = (source.rate as f64 / 2.0).min(11_025.0);
    Score {
        fw_snr_seg_db: metrics::fw_snr_seg_db(&reference[..n], &test[..n], max_hz),
        si_snr_db: metrics::si_snr_db(&reference[..n], &test[..n]),
        played,
    }
}
