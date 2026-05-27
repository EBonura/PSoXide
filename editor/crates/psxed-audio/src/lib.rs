//! WAV → PSAU audio cooker.
//!
//! This crate is host-only. It turns licensed source WAVs into
//! deterministic mono PS1 SPU ADPCM blobs and writes enough provenance
//! to reproduce the cook later.

use psxed_format::audio::{self, AudioHeader, CODEC_SPU_ADPCM};
use psxed_format::AssetHeader;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

const SPU_SAMPLE_RATE_HZ: u32 = 44_100;
const ADPCM_BLOCK_BYTES: usize = 16;
const ADPCM_SAMPLES_PER_BLOCK: usize = 28;
const ADPCM_FILTERS: [(i32, i32); 5] = [(0, 0), (60, 0), (115, -52), (98, -55), (122, -60)];

/// Options for a manifest-based import.
#[derive(Clone, Debug)]
pub struct PackOptions {
    /// Write decoded preview WAVs next to the cooked PSAU files.
    pub write_preview_wav: bool,
}

impl Default for PackOptions {
    fn default() -> Self {
        Self {
            write_preview_wav: true,
        }
    }
}

/// JSON report emitted by a manifest import.
#[derive(Clone, Debug, Serialize)]
pub struct PackReport {
    /// Source pack display name.
    pub source_name: String,
    /// Source pack URL used for provenance.
    pub source_url: String,
    /// Source pack license identifier.
    pub license: String,
    /// SHA-256 of the imported source archive.
    pub archive_sha256: String,
    /// Target sample rate used for every cooked sound.
    pub target_sample_rate_hz: u32,
    /// Peak normalization target in `0.0..=1.0`.
    pub normalize_peak: f32,
    /// Per-sound cook results.
    pub sounds: Vec<SoundReport>,
}

/// JSON report for one cooked sound.
#[derive(Clone, Debug, Serialize)]
pub struct SoundReport {
    /// Stable asset id from the manifest.
    pub id: String,
    /// Path of the WAV inside the source archive.
    pub source_path: String,
    /// Relative cooked PSAU output path.
    pub output_psau: String,
    /// Relative decoded-preview WAV output path.
    pub preview_wav: Option<String>,
    /// SHA-256 of the source WAV bytes.
    pub source_sha256: String,
    /// SHA-256 of the cooked PSAU bytes.
    pub cooked_sha256: String,
    /// Source WAV sample rate.
    pub original_sample_rate_hz: u32,
    /// Source WAV channel count.
    pub original_channels: u16,
    /// Source frames before downmix/resample.
    pub input_sample_frames: u32,
    /// Audible mono sample count after cooking.
    pub cooked_sample_count: u32,
    /// Number of SPU ADPCM blocks.
    pub adpcm_blocks: u32,
    /// Cooked PSAU byte count.
    pub psau_bytes: usize,
    /// Peak before normalization in `0.0..=1.0`.
    pub peak_before: f32,
    /// Peak after normalization in `0.0..=1.0`.
    pub peak_after: f32,
    /// RMS error between normalized PCM and decoded ADPCM.
    pub decoded_rms_error: f32,
}

/// Errors surfaced by the audio cooker.
#[derive(Debug)]
pub enum Error {
    /// Filesystem operation failed.
    Io {
        /// Path being accessed.
        path: PathBuf,
        /// Underlying IO error.
        source: std::io::Error,
    },
    /// Manifest JSON could not be parsed or written.
    Json(serde_json::Error),
    /// Zip archive container could not be read.
    Zip(zip::result::ZipError),
    /// Required zip entry was not present.
    ZipEntry {
        /// Entry path from the manifest.
        path: String,
        /// Zip lookup error.
        source: zip::result::ZipError,
    },
    /// Archive hash differs from the manifest.
    ArchiveHashMismatch {
        /// Manifest hash.
        expected: String,
        /// Local archive hash.
        actual: String,
    },
    /// Manifest content is structurally invalid.
    InvalidManifest(String),
    /// WAV data is malformed or unsupported.
    Wav(String),
    /// ADPCM byte stream is malformed.
    Adpcm(String),
    /// Source audio had no samples after decoding.
    EmptyAudio,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Error::Json(e) => write!(f, "json: {e}"),
            Error::Zip(e) => write!(f, "zip: {e}"),
            Error::ZipEntry { path, source } => write!(f, "zip entry {path}: {source}"),
            Error::ArchiveHashMismatch { expected, actual } => {
                write!(
                    f,
                    "archive sha256 mismatch: expected {expected}, got {actual}"
                )
            }
            Error::InvalidManifest(msg) => write!(f, "manifest: {msg}"),
            Error::Wav(msg) => write!(f, "wav: {msg}"),
            Error::Adpcm(msg) => write!(f, "adpcm: {msg}"),
            Error::EmptyAudio => write!(f, "source audio is empty"),
        }
    }
}

impl std::error::Error for Error {}

impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl From<zip::result::ZipError> for Error {
    fn from(value: zip::result::ZipError) -> Self {
        Self::Zip(value)
    }
}

#[derive(Clone, Debug, Deserialize)]
struct PackManifest {
    source: SourceManifest,
    target_sample_rate_hz: Option<u32>,
    normalize_peak: Option<f32>,
    sounds: Vec<SoundManifest>,
}

#[derive(Clone, Debug, Deserialize)]
struct SourceManifest {
    name: String,
    url: String,
    license: String,
    archive_sha256: String,
}

#[derive(Clone, Debug, Deserialize)]
struct SoundManifest {
    id: String,
    path: String,
}

#[derive(Copy, Clone, Debug)]
struct CookConfig {
    sample_rate_hz: u32,
    normalize_peak: f32,
}

#[derive(Clone, Debug)]
struct CookedAudio {
    psau: Vec<u8>,
    decoded_preview: Vec<i16>,
    report: CookReport,
}

#[derive(Clone, Debug)]
struct CookReport {
    original_sample_rate_hz: u32,
    original_channels: u16,
    input_sample_frames: u32,
    cooked_sample_count: u32,
    adpcm_blocks: u32,
    peak_before: f32,
    peak_after: f32,
    decoded_rms_error: f32,
}

#[derive(Clone, Debug)]
struct WavPcm16 {
    sample_rate_hz: u32,
    channels: u16,
    samples: Vec<i16>,
}

/// Import a source zip using a checked-in JSON manifest.
pub fn import_pack(
    manifest_path: &Path,
    archive_path: &Path,
    out_dir: &Path,
    options: &PackOptions,
) -> Result<PackReport, Error> {
    let manifest_bytes = read_file(manifest_path)?;
    let archive_bytes = read_file(archive_path)?;
    let manifest: PackManifest = serde_json::from_slice(&manifest_bytes)?;
    validate_manifest(&manifest)?;

    let archive_sha256 = sha256_hex(&archive_bytes);
    if manifest.source.archive_sha256.to_ascii_lowercase() != archive_sha256 {
        return Err(Error::ArchiveHashMismatch {
            expected: manifest.source.archive_sha256,
            actual: archive_sha256,
        });
    }

    let sample_rate_hz = manifest.target_sample_rate_hz.unwrap_or(SPU_SAMPLE_RATE_HZ);
    if sample_rate_hz == 0 {
        return Err(Error::InvalidManifest(
            "target_sample_rate_hz must be non-zero".to_string(),
        ));
    }
    let normalize_peak = manifest.normalize_peak.unwrap_or(0.9).clamp(0.0, 1.0);
    let cfg = CookConfig {
        sample_rate_hz,
        normalize_peak,
    };

    let psau_dir = out_dir.join("psau");
    let preview_dir = out_dir.join("preview");
    create_dir(&psau_dir)?;
    if options.write_preview_wav {
        create_dir(&preview_dir)?;
    }

    let mut archive = zip::ZipArchive::new(Cursor::new(archive_bytes.as_slice()))?;
    let mut reports = Vec::with_capacity(manifest.sounds.len());

    for sound in &manifest.sounds {
        let id = checked_asset_id(&sound.id)?;
        let wav_bytes = read_zip_entry(&mut archive, &sound.path)?;
        let cooked = cook_wav(&wav_bytes, &cfg)?;

        let psau_rel = format!("psau/{id}.psau");
        let psau_path = out_dir.join(&psau_rel);
        write_file(&psau_path, &cooked.psau)?;

        let preview_rel = if options.write_preview_wav {
            let rel = format!("preview/{id}.wav");
            let wav = write_wav_mono_i16(cfg.sample_rate_hz, &cooked.decoded_preview);
            write_file(&out_dir.join(&rel), &wav)?;
            Some(rel)
        } else {
            None
        };

        reports.push(SoundReport {
            id,
            source_path: sound.path.clone(),
            output_psau: psau_rel,
            preview_wav: preview_rel,
            source_sha256: sha256_hex(&wav_bytes),
            cooked_sha256: sha256_hex(&cooked.psau),
            original_sample_rate_hz: cooked.report.original_sample_rate_hz,
            original_channels: cooked.report.original_channels,
            input_sample_frames: cooked.report.input_sample_frames,
            cooked_sample_count: cooked.report.cooked_sample_count,
            adpcm_blocks: cooked.report.adpcm_blocks,
            psau_bytes: cooked.psau.len(),
            peak_before: cooked.report.peak_before,
            peak_after: cooked.report.peak_after,
            decoded_rms_error: cooked.report.decoded_rms_error,
        });
    }

    let report = PackReport {
        source_name: manifest.source.name,
        source_url: manifest.source.url,
        license: manifest.source.license,
        archive_sha256,
        target_sample_rate_hz: cfg.sample_rate_hz,
        normalize_peak: cfg.normalize_peak,
        sounds: reports,
    };
    let report_json = serde_json::to_vec_pretty(&report)?;
    write_file(&out_dir.join("report.json"), &report_json)?;
    Ok(report)
}

fn validate_manifest(manifest: &PackManifest) -> Result<(), Error> {
    if manifest.sounds.is_empty() {
        return Err(Error::InvalidManifest(
            "sounds must not be empty".to_string(),
        ));
    }
    if manifest.source.archive_sha256.len() != 64 {
        return Err(Error::InvalidManifest(
            "source.archive_sha256 must be a 64-character hex string".to_string(),
        ));
    }
    Ok(())
}

fn cook_wav(src: &[u8], cfg: &CookConfig) -> Result<CookedAudio, Error> {
    let wav = parse_wav_pcm16(src)?;
    let input_sample_frames = wav.samples.len() / wav.channels as usize;
    let mono = downmix_to_mono(&wav.samples, wav.channels);
    if mono.is_empty() {
        return Err(Error::EmptyAudio);
    }

    let mut pcm = resample_linear(&mono, wav.sample_rate_hz, cfg.sample_rate_hz);
    let peak_before = peak_ratio(&pcm);
    normalize_to_peak(&mut pcm, cfg.normalize_peak);
    let peak_after = peak_ratio(&pcm);

    let adpcm = encode_spu_adpcm(&pcm);
    let decoded_preview = decode_spu_adpcm(&adpcm, pcm.len())?;
    let psau = assemble_psau(cfg.sample_rate_hz, pcm.len() as u32, &adpcm);
    let decoded_rms_error = rms_error(&pcm, &decoded_preview);

    Ok(CookedAudio {
        psau,
        decoded_preview,
        report: CookReport {
            original_sample_rate_hz: wav.sample_rate_hz,
            original_channels: wav.channels,
            input_sample_frames: input_sample_frames as u32,
            cooked_sample_count: pcm.len() as u32,
            adpcm_blocks: (adpcm.len() / ADPCM_BLOCK_BYTES) as u32,
            peak_before,
            peak_after,
            decoded_rms_error,
        },
    })
}

fn parse_wav_pcm16(bytes: &[u8]) -> Result<WavPcm16, Error> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(Error::Wav("expected RIFF/WAVE header".to_string()));
    }

    let mut pos = 12usize;
    let mut fmt: Option<(u16, u16, u32, u16)> = None;
    let mut data: Option<&[u8]> = None;

    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = read_u32(bytes, pos + 4)? as usize;
        let start = pos + 8;
        let end = start
            .checked_add(len)
            .ok_or_else(|| Error::Wav("chunk length overflow".to_string()))?;
        if end > bytes.len() {
            return Err(Error::Wav("chunk extends past EOF".to_string()));
        }

        match id {
            b"fmt " => {
                if len < 16 {
                    return Err(Error::Wav("fmt chunk too short".to_string()));
                }
                let audio_format = read_u16(bytes, start)?;
                let channels = read_u16(bytes, start + 2)?;
                let sample_rate_hz = read_u32(bytes, start + 4)?;
                let bits_per_sample = read_u16(bytes, start + 14)?;
                fmt = Some((audio_format, channels, sample_rate_hz, bits_per_sample));
            }
            b"data" => data = Some(&bytes[start..end]),
            _ => {}
        }

        pos = end + (len & 1);
    }

    let (audio_format, channels, sample_rate_hz, bits_per_sample) =
        fmt.ok_or_else(|| Error::Wav("missing fmt chunk".to_string()))?;
    if audio_format != 1 {
        return Err(Error::Wav(format!(
            "unsupported WAV format {audio_format}; expected PCM"
        )));
    }
    if channels == 0 {
        return Err(Error::Wav("channel count is zero".to_string()));
    }
    if bits_per_sample != 16 {
        return Err(Error::Wav(format!(
            "unsupported bit depth {bits_per_sample}; expected 16"
        )));
    }
    let data = data.ok_or_else(|| Error::Wav("missing data chunk".to_string()))?;
    if data.len() % 2 != 0 {
        return Err(Error::Wav("data chunk has odd byte length".to_string()));
    }

    let samples = data
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();

    Ok(WavPcm16 {
        sample_rate_hz,
        channels,
        samples,
    })
}

fn downmix_to_mono(samples: &[i16], channels: u16) -> Vec<i16> {
    let channels = channels as usize;
    samples
        .chunks_exact(channels)
        .map(|frame| {
            let sum: i32 = frame.iter().map(|&s| s as i32).sum();
            (sum / channels as i32).clamp(i16::MIN as i32, i16::MAX as i32) as i16
        })
        .collect()
}

fn resample_linear(samples: &[i16], source_rate: u32, target_rate: u32) -> Vec<i16> {
    if source_rate == target_rate || samples.len() < 2 {
        return samples.to_vec();
    }
    let out_len = ((samples.len() as u64 * target_rate as u64) + (source_rate as u64 / 2))
        / source_rate as u64;
    let out_len = out_len.max(1) as usize;
    let mut out = Vec::with_capacity(out_len);

    for i in 0..out_len {
        let pos = (i as f64) * (source_rate as f64) / (target_rate as f64);
        let lo = pos.floor() as usize;
        let hi = (lo + 1).min(samples.len() - 1);
        let t = pos - lo as f64;
        let a = samples[lo] as f64;
        let b = samples[hi] as f64;
        out.push(
            (a + (b - a) * t)
                .round()
                .clamp(i16::MIN as f64, i16::MAX as f64) as i16,
        );
    }
    out
}

fn normalize_to_peak(samples: &mut [i16], target: f32) {
    let peak = samples.iter().map(|&s| (s as i32).abs()).max().unwrap_or(0);
    if peak == 0 || target <= 0.0 {
        return;
    }
    let target_amp = (target.clamp(0.0, 1.0) * i16::MAX as f32).round();
    let scale = target_amp / peak as f32;
    for sample in samples {
        let scaled = (*sample as f32 * scale).round();
        *sample = scaled.clamp(i16::MIN as f32, i16::MAX as f32) as i16;
    }
}

fn peak_ratio(samples: &[i16]) -> f32 {
    let peak = samples.iter().map(|&s| (s as i32).abs()).max().unwrap_or(0);
    (peak as f32 / 32768.0).min(1.0)
}

fn encode_spu_adpcm(samples: &[i16]) -> Vec<u8> {
    let block_count = samples.len().div_ceil(ADPCM_SAMPLES_PER_BLOCK).max(1);
    let mut out = Vec::with_capacity(block_count * ADPCM_BLOCK_BYTES);
    let mut state = (0, 0);

    for block_index in 0..block_count {
        let start = block_index * ADPCM_SAMPLES_PER_BLOCK;
        let end = (start + ADPCM_SAMPLES_PER_BLOCK).min(samples.len());
        let mut block_samples = [0i32; ADPCM_SAMPLES_PER_BLOCK];
        for (dst, &src) in block_samples.iter_mut().zip(&samples[start..end]) {
            *dst = src as i32;
        }
        let flags = if block_index + 1 == block_count {
            0x01
        } else {
            0x00
        };
        let encoded = encode_adpcm_block(&block_samples, state, flags);
        out.extend_from_slice(&encoded.bytes);
        state = encoded.state;
    }

    out
}

#[derive(Copy, Clone, Debug)]
struct EncodedBlock {
    bytes: [u8; ADPCM_BLOCK_BYTES],
    state: (i32, i32),
    error: i64,
}

fn encode_adpcm_block(
    samples: &[i32; ADPCM_SAMPLES_PER_BLOCK],
    state: (i32, i32),
    flags: u8,
) -> EncodedBlock {
    let mut best: Option<EncodedBlock> = None;

    for filter in 0..ADPCM_FILTERS.len() {
        for shift in 0..=12 {
            let candidate = quantize_block(samples, state, filter, shift, flags);
            if best
                .as_ref()
                .is_none_or(|best| candidate.error < best.error)
            {
                best = Some(candidate);
            }
        }
    }

    best.expect("filter/shift search always yields a candidate")
}

fn quantize_block(
    samples: &[i32; ADPCM_SAMPLES_PER_BLOCK],
    state: (i32, i32),
    filter: usize,
    shift: u8,
    flags: u8,
) -> EncodedBlock {
    let (f1, f2) = ADPCM_FILTERS[filter];
    let mut s_1 = state.0;
    let mut s_2 = state.1;
    let mut nibbles = [0u8; ADPCM_SAMPLES_PER_BLOCK];
    let mut error = 0i64;

    for (i, &sample) in samples.iter().enumerate() {
        let prediction = ((s_1 * f1) >> 6) + ((s_2 * f2) >> 6);
        let residual = sample - prediction;
        let q = round_div(residual * (1 << shift), 4096).clamp(-8, 7);
        let raw = (q << 12) >> shift;
        let value = raw + prediction;
        let diff = sample as i64 - value as i64;
        error += diff * diff;
        nibbles[i] = (q as i8 as u8) & 0x0F;
        s_2 = s_1;
        s_1 = value;
    }

    let mut bytes = [0u8; ADPCM_BLOCK_BYTES];
    bytes[0] = ((filter as u8) << 4) | shift;
    bytes[1] = flags;
    for i in 0..14 {
        bytes[2 + i] = nibbles[i * 2] | (nibbles[i * 2 + 1] << 4);
    }

    EncodedBlock {
        bytes,
        state: (s_1, s_2),
        error,
    }
}

fn decode_spu_adpcm(adpcm: &[u8], sample_count: usize) -> Result<Vec<i16>, Error> {
    if adpcm.len() % ADPCM_BLOCK_BYTES != 0 {
        return Err(Error::Adpcm(
            "byte length is not a multiple of 16".to_string(),
        ));
    }

    let mut out = Vec::with_capacity(sample_count);
    let mut s_1 = 0i32;
    let mut s_2 = 0i32;

    for block in adpcm.chunks_exact(ADPCM_BLOCK_BYTES) {
        let predictor = ((block[0] >> 4) as usize).min(ADPCM_FILTERS.len() - 1);
        let shift = (block[0] & 0x0F).min(12) as u32;
        let (f1, f2) = ADPCM_FILTERS[predictor];

        for i in 0..ADPCM_SAMPLES_PER_BLOCK {
            let byte = block[2 + (i >> 1)] as i32;
            let nibble = if i & 1 == 0 {
                byte & 0x0F
            } else {
                (byte >> 4) & 0x0F
            };
            let signed = ((nibble << 28) >> 28) << 12;
            let raw = signed >> shift;
            let sample = raw + ((s_1 * f1) >> 6) + ((s_2 * f2) >> 6);
            s_2 = s_1;
            s_1 = sample;
            if out.len() < sample_count {
                out.push(sample.clamp(i16::MIN as i32, i16::MAX as i32) as i16);
            }
        }
    }

    Ok(out)
}

fn assemble_psau(sample_rate_hz: u32, sample_count: u32, adpcm: &[u8]) -> Vec<u8> {
    let adpcm_block_count = (adpcm.len() / ADPCM_BLOCK_BYTES) as u32;
    let payload_len = AudioHeader::SIZE as u32 + adpcm.len() as u32;
    let flags = audio::flags::MONO | audio::flags::ONE_SHOT;
    let mut out = Vec::with_capacity(AssetHeader::SIZE + payload_len as usize);

    out.extend_from_slice(&audio::MAGIC);
    out.extend_from_slice(&audio::VERSION.to_le_bytes());
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&payload_len.to_le_bytes());
    out.push(CODEC_SPU_ADPCM);
    out.push(1);
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&sample_rate_hz.to_le_bytes());
    out.extend_from_slice(&sample_count.to_le_bytes());
    out.extend_from_slice(&adpcm_block_count.to_le_bytes());
    out.extend_from_slice(&AudioHeader::NO_LOOP.to_le_bytes());
    out.extend_from_slice(adpcm);
    out
}

fn write_wav_mono_i16(sample_rate_hz: u32, samples: &[i16]) -> Vec<u8> {
    let data_bytes = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_bytes as usize);

    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&sample_rate_hz.to_le_bytes());
    out.extend_from_slice(&(sample_rate_hz * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());
    for &sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

fn rms_error(reference: &[i16], decoded: &[i16]) -> f32 {
    let n = reference.len().min(decoded.len());
    if n == 0 {
        return 0.0;
    }
    let sum = reference
        .iter()
        .zip(decoded.iter())
        .take(n)
        .map(|(&a, &b)| {
            let d = a as f64 - b as f64;
            d * d
        })
        .sum::<f64>();
    (sum / n as f64).sqrt() as f32
}

fn checked_asset_id(id: &str) -> Result<String, Error> {
    let id = id.trim();
    if id.is_empty() {
        return Err(Error::InvalidManifest("sound id is empty".to_string()));
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(Error::InvalidManifest(format!(
            "sound id {id:?} must use ASCII letters, digits, '_' or '-'"
        )));
    }
    Ok(id.to_string())
}

fn read_zip_entry<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    path: &str,
) -> Result<Vec<u8>, Error> {
    let mut file = archive.by_name(path).map_err(|source| Error::ZipEntry {
        path: path.to_string(),
        source,
    })?;
    let mut bytes = Vec::with_capacity(file.size() as usize);
    file.read_to_end(&mut bytes).map_err(|source| Error::Io {
        path: PathBuf::from(path),
        source,
    })?;
    Ok(bytes)
}

fn read_file(path: &Path) -> Result<Vec<u8>, Error> {
    std::fs::read(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    std::fs::write(path, bytes).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn create_dir(path: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        write!(&mut out, "{b:02x}").expect("write to String");
    }
    out
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, Error> {
    let pair = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| Error::Wav("unexpected EOF".to_string()))?;
    Ok(u16::from_le_bytes([pair[0], pair[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, Error> {
    let word = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| Error::Wav("unexpected EOF".to_string()))?;
    Ok(u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
}

fn round_div(num: i32, den: i32) -> i32 {
    if num >= 0 {
        (num + den / 2) / den
    } else {
        -((-num + den / 2) / den)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_wav_roundtrips_through_parser() {
        let src = [0i16, 1200, -1200, 3200];
        let wav = write_wav_mono_i16(22_050, &src);
        let parsed = parse_wav_pcm16(&wav).unwrap();

        assert_eq!(parsed.sample_rate_hz, 22_050);
        assert_eq!(parsed.channels, 1);
        assert_eq!(parsed.samples, src);
    }

    #[test]
    fn silence_encodes_to_silent_adpcm() {
        let src = vec![0i16; ADPCM_SAMPLES_PER_BLOCK * 2];
        let adpcm = encode_spu_adpcm(&src);
        let decoded = decode_spu_adpcm(&adpcm, src.len()).unwrap();

        assert_eq!(adpcm.len(), ADPCM_BLOCK_BYTES * 2);
        assert_eq!(adpcm[1], 0x00);
        assert_eq!(adpcm[17], 0x01);
        assert!(decoded.iter().all(|&s| s == 0));
    }

    #[test]
    fn cook_wav_emits_psau_header_and_preview() {
        let samples: Vec<i16> = (0..128)
            .map(|i| (((i % 32) as i32 - 16) * 512) as i16)
            .collect();
        let wav = write_wav_mono_i16(44_100, &samples);
        let cooked = cook_wav(
            &wav,
            &CookConfig {
                sample_rate_hz: 44_100,
                normalize_peak: 0.9,
            },
        )
        .unwrap();

        assert_eq!(&cooked.psau[0..4], b"PSAU");
        assert_eq!(u16::from_le_bytes([cooked.psau[4], cooked.psau[5]]), 1);
        assert_eq!(cooked.psau[12], CODEC_SPU_ADPCM);
        assert_eq!(cooked.psau[13], 1);
        assert_eq!(cooked.decoded_preview.len(), samples.len());
    }

    #[test]
    fn adpcm_encoder_is_deterministic() {
        let samples: Vec<i16> = (0..512)
            .map(|i| (((i * 17) % 97) as i32 * 300 - 12_000) as i16)
            .collect();
        let a = encode_spu_adpcm(&samples);
        let b = encode_spu_adpcm(&samples);

        assert_eq!(a, b);
    }
}
