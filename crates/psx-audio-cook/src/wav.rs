//! RIFF/WAVE reading and writing.
//!
//! Reads integer PCM (8, 16, 24 or 32 bit) and 32-bit float, mono or
//! multichannel (averaged to mono), plus the loop information game sources
//! carry: the first `smpl` loop, or the first `cue ` point as a loop start
//! (GoldSrc and Quake both mark loops that way).

use crate::Error;

/// A mono source sound at its authored rate.
#[derive(Clone, Debug, PartialEq)]
pub struct Wav {
    /// Sample rate in Hz.
    pub rate: u32,
    /// Mono samples scaled to the signed 16-bit range (not rounded, so
    /// 24-bit and float sources keep their precision until cooking).
    pub samples: Vec<f64>,
    /// Loop start sample, when the file declares a loop.
    pub loop_start: Option<usize>,
    /// Loop end (exclusive), when a `smpl` loop gives one. `None` with a
    /// loop start means "loop to the end of the sample".
    pub loop_end: Option<usize>,
    /// Bit depth of the source (for reporting).
    pub bits: u16,
}

fn u16le(b: &[u8], at: usize) -> Result<u16, Error> {
    b.get(at..at + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or_else(|| Error::Wav("unexpected end of file".into()))
}

fn u32le(b: &[u8], at: usize) -> Result<u32, Error> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| Error::Wav("unexpected end of file".into()))
}

/// Parse a WAV file into mono samples plus loop metadata.
pub fn read(bytes: &[u8]) -> Result<Wav, Error> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(Error::Wav("expected RIFF/WAVE header".into()));
    }
    let mut pos = 12usize;
    let mut fmt = None;
    let mut data: Option<&[u8]> = None;
    let mut smpl_loop = None;
    let mut cue_start = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32le(bytes, pos + 4)? as usize;
        let start = pos + 8;
        // Legacy game WAVs sometimes declare chunks longer than the file;
        // clamp instead of failing, as GoldSrc does.
        let end = start.saturating_add(len).min(bytes.len());
        let body = &bytes[start..end];
        match id {
            b"fmt " if body.len() >= 16 => {
                let mut format = u16le(body, 0)?;
                if format == 0xFFFE && body.len() >= 26 {
                    format = u16le(body, 24)?; // WAVE_FORMAT_EXTENSIBLE sub-format
                }
                fmt = Some((format, u16le(body, 2)?, u32le(body, 4)?, u16le(body, 14)?));
            }
            b"data" => data = Some(body),
            b"smpl" if body.len() >= 36 => {
                let loops = u32le(body, 28)?;
                if loops > 0 && body.len() >= 36 + 24 {
                    let s = u32le(body, 36 + 8)? as usize;
                    let e = u32le(body, 36 + 12)? as usize;
                    smpl_loop = Some((s, e + 1));
                }
            }
            b"cue " if body.len() >= 4 => {
                let count = u32le(body, 0)?;
                if count > 0 && body.len() >= 4 + 24 {
                    cue_start = Some(u32le(body, 4 + 20)? as usize);
                }
            }
            _ => {}
        }
        pos = start.saturating_add(len).saturating_add(len & 1);
    }
    let (format, channels, rate, bits) =
        fmt.ok_or_else(|| Error::Wav("missing fmt chunk".into()))?;
    let data = data.ok_or_else(|| Error::Wav("missing data chunk".into()))?;
    if channels == 0 || rate == 0 {
        return Err(Error::Wav("zero channels or rate".into()));
    }
    let width = match (format, bits) {
        (1, 8) => 1,
        (1, 16) => 2,
        (1, 24) => 3,
        (1, 32) | (3, 32) => 4,
        _ => {
            return Err(Error::Wav(format!(
                "unsupported WAV encoding {format} at {bits} bits"
            )))
        }
    };
    let frame = width * channels as usize;
    let mut samples = Vec::with_capacity(data.len() / frame);
    for f in data.chunks_exact(frame) {
        let mut sum = 0.0;
        for c in f.chunks_exact(width) {
            sum += match (format, width) {
                (1, 1) => (c[0] as f64 - 128.0) * 256.0,
                (1, 2) => i16::from_le_bytes([c[0], c[1]]) as f64,
                (1, 3) => (i32::from_le_bytes([0, c[0], c[1], c[2]]) >> 8) as f64 / 256.0,
                (1, 4) => i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f64 / 65536.0,
                _ => f32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f64 * 32768.0,
            };
        }
        samples.push(sum / channels as f64);
    }
    let n = samples.len();
    let (loop_start, loop_end) = match (smpl_loop, cue_start) {
        (Some((s, e)), _) if s < n && e > s => (Some(s), Some(e.min(n))),
        (None, Some(s)) if s < n => (Some(s), None),
        _ => (None, None),
    };
    Ok(Wav {
        rate,
        samples,
        loop_start,
        loop_end,
        bits,
    })
}

/// Encode mono 16-bit PCM as a WAV file.
pub fn write_mono16(rate: u32, samples: &[i16]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}
