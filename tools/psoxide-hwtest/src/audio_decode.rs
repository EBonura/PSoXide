//! Decode a hardware-test audio-link capture back into the capture payload.
//!
//! The disc streams its whole capture out of the SPU as binary FSK and loops it
//! forever, so a recording made through a capture card carries the payload
//! continuously instead of in photographed QR stills. This recovers it.
//!
//!     psoxide-hwtest audio-decode capture.wav --out payload.bin
//!
//! Each bit is one ADPCM block: exactly 28 samples at 44.1 kHz. The two tones
//! sit on exact DFT bins of a 28-sample window (1575 Hz = bin 1, 3150 Hz =
//! bin 2), so deciding a bit is a comparison of two bin magnitudes and needs no
//! filter design. Every repetition in the recording is tried until one
//! satisfies the CRC, which is the point of looping the transmission: a glitch
//! costs a repetition rather than another burn.

use std::io::Write;
use std::path::Path;

use crate::util::{base64_encode, crc32, Error, Result};

/// Samples per bit at each transmit rate. The disc can be dropped to a slower
/// rate on the console (SQUARE on the capture page) without a reburn, so the
/// decoder tries every rate rather than being told which one was used.
const RATE_SAMPLES_PER_BIT: [usize; 4] = [28, 56, 112, 224];
const SYNC_WORD: u32 = 0x1ACF;
const PREAMBLE_BITS: usize = 64;
/// Bin indices within one 28-sample window. Bit 1 is the lower tone.
const BIN_ONE: f64 = 1.0;
const BIN_ZERO: f64 = 2.0;
/// A window whose combined tone energy falls below this fraction of the TYPICAL
/// loud-window energy is silence rather than a bit.
///
/// Measured against a high percentile, not the peak: a single transient (a
/// CD-DA note, a pop when the console powers on) sets a peak far above the
/// readout tone, and a floor derived from it rejects the entire transmission.
/// A console recording was 97% "silence" by that rule while carrying a
/// perfectly good tone.
const SILENCE_FRACTION: f64 = 0.25;
/// Percentile of window energy taken as the reference "signal is present" level.
const SIGNAL_PERCENTILE: f64 = 90.0;
/// Preamble bits that must match. Not all of them: one bit error in 64 would
/// otherwise discard an entire repetition, and lossy audio produces exactly
/// that.
const PREAMBLE_MIN_MATCH: i16 = 54;
/// Bit errors tolerated in the sync word.
const SYNC_MAX_ERRORS: u32 = 3;
/// Calibration tones the disc sends ahead of each frame, in bit slots.
const CALIBRATION_BITS: usize = 128;
/// Samples of phase drift tolerated when grouping repetitions of one
/// transmission. The capture clock and the console clock are independent.
const PHASE_TOLERANCE: i64 = 4000;
/// The link's bit clock IS the console's 44.1 kHz sample rate. Capture chains
/// very often record at 48 kHz (OBS defaults to it), where a bit stops being a
/// whole number of samples and the tones no longer land on exact DFT bins.
/// Recordings are resampled to this rate before anything else.
pub const LINK_RATE: u32 = 44100;
const BASE64_CHARS_PER_PAGE: usize = 828;

/// Magnitude of DFT bins 1 and 2 for a window starting at EVERY sample.
///
/// A naive per-window DFT is O(n * span) and takes minutes on a couple of
/// minutes of audio, which is unusable for an operator decoding a console
/// recording. Multiplying the signal by the conjugate exponential and taking a
/// length-`span` moving sum yields the same bins in O(n) via one cumulative sum
/// per bin.
fn sliding_magnitudes(samples: &[f64], span: usize) -> (Vec<f64>, Vec<f64>) {
    let n = samples.len();
    let mut out = Vec::new();
    for bin_index in [BIN_ONE, BIN_ZERO] {
        let mut cumulative: Vec<(f64, f64)> = Vec::with_capacity(n + 1);
        cumulative.push((0.0, 0.0));
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (index, &sample) in samples.iter().enumerate() {
            // NumPy divides the complex angle by the integer span with Smith's
            // algorithm, which multiplies by the reciprocal; the same rounding
            // keeps the tie-breaks between equally good phases identical.
            let angle = (-2.0 * std::f64::consts::PI * bin_index * index as f64) * (1.0 / span as f64);
            re += sample * angle.cos();
            im += sample * angle.sin();
            cumulative.push((re, im));
        }
        // Window [i, i+span) sum = cumulative[i+span] - cumulative[i].
        let magnitudes: Vec<f64> = if n >= span {
            (0..=n - span)
                .map(|i| {
                    let (a, b) = (cumulative[i + span], cumulative[i]);
                    (a.0 - b.0).hypot(a.1 - b.1)
                })
                .collect()
        } else {
            Vec::new()
        };
        out.push(magnitudes);
    }
    let zero = out.pop().expect("two bins");
    let one = out.pop().expect("two bins");
    (one, zero)
}

/// `np.percentile(values, q)` with linear interpolation.
fn percentile(values: &[f64], q: f64) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("finite energies"));
    let virtual_index = (sorted.len() - 1) as f64 * (q / 100.0);
    let previous = virtual_index.floor();
    let gamma = virtual_index - previous;
    let a = sorted[previous as usize];
    let b = sorted[(previous as usize + 1).min(sorted.len() - 1)];
    let diff = b - a;
    if gamma >= 0.5 {
        b - diff * (1.0 - gamma)
    } else {
        a + diff * gamma
    }
}

/// Bit decisions at one transmit rate, precomputed for every offset.
pub struct RateDecoder {
    span: usize,
    /// -1 = silence / no signal, else the decided bit.
    bits: Vec<i8>,
    /// Signed discrimination toward bit 1, kept for phase refinement: a window
    /// straddling a tone boundary still DECIDES correctly on clean audio, so
    /// the bit array alone cannot tell an aligned lock from one half a bit off.
    /// This can.
    diff: Vec<f64>,
}

impl RateDecoder {
    pub fn new(samples: &[f64], span: usize) -> Self {
        let (one, zero) = sliding_magnitudes(samples, span);
        let energy: Vec<f64> = one.iter().zip(&zero).map(|(a, b)| a + b).collect();
        let reference = if energy.is_empty() {
            0.0
        } else {
            percentile(&energy, SIGNAL_PERCENTILE)
        };
        let floor = reference * SILENCE_FRACTION;
        let bits = energy
            .iter()
            .zip(one.iter().zip(&zero))
            .map(|(&e, (&a, &b))| if e < floor { -1 } else { i8::from(a > b) })
            .collect();
        let diff = one.iter().zip(&zero).map(|(a, b)| a - b).collect();
        RateDecoder { span, bits, diff }
    }

    fn bit(&self, start: i64) -> i8 {
        if start < 0 || start as usize >= self.bits.len() {
            -1
        } else {
            self.bits[start as usize]
        }
    }

    /// Aggregate discrimination of the alternating preamble at `start`.
    ///
    /// Maximised when block windows align with transmitted block boundaries: a
    /// misaligned window mixes both tones and its discrimination collapses even
    /// while the decision stays right, so this peaks sharply at the true phase
    /// where the bit-match score alone plateaus half a bit wide.
    fn preamble_quality(&self, start: usize) -> f64 {
        let mut total = 0.0;
        for i in 0..PREAMBLE_BITS {
            let at = start + i * self.span;
            if at < self.diff.len() {
                let d = self.diff[at];
                total += if i % 2 == 0 { d } else { -d };
            }
        }
        total
    }

    fn read(&self, start: i64, count: usize) -> Vec<i8> {
        (0..count)
            .map(|i| self.bit(start + (i * self.span) as i64))
            .collect()
    }
}

fn bits_to_int(bits: &[i8], start: usize, count: usize) -> u64 {
    let mut value = 0u64;
    for offset in 0..count {
        value = (value << 1) | bits[start + offset] as u8 as u64;
    }
    value
}

/// Sample offsets where a preamble-then-sync sequence begins.
///
/// Matching is deliberately fuzzy, and scored across the WHOLE preamble rather
/// than gated on its first few bits. Requiring any bits to be exact means one
/// error discards a whole repetition, and lossy capture audio produces single
/// bit errors constantly: a console recording yielded 2 usable frames under
/// exact matching and dozens under this.
fn find_frames(decoder: &RateDecoder, limit: usize) -> Vec<usize> {
    let span = decoder.span;
    let bits = &decoder.bits;
    let reach = span * (PREAMBLE_BITS + 16);
    if bits.len() <= reach {
        return Vec::new();
    }
    let count = bits.len() - reach;
    let mut score = vec![0i16; count];
    for i in 0..PREAMBLE_BITS {
        let expected = i8::from(i % 2 == 0);
        for (offset, slot) in score.iter_mut().enumerate() {
            *slot += i16::from(bits[i * span + offset] == expected);
        }
    }
    let positions: Vec<usize> = score
        .iter()
        .enumerate()
        .filter(|(_, &s)| s >= PREAMBLE_MIN_MATCH)
        .map(|(i, _)| i)
        .collect();
    let mut starts = Vec::new();
    let mut last: i64 = -(10i64.pow(9));
    let mut index = 0;
    while index < positions.len() && starts.len() < limit {
        // One preamble produces a run of adjacent hits. The first hit is the
        // WORST lock, not the best: on clean audio the fuzzy score crosses the
        // threshold about half a bit early, where every tone-transition window
        // still decides correctly but at a margin any real noise flips. That
        // mislock cost the chain matrix its noise_25pct tier while every clean
        // decode passed. Pick the phase that discriminates best instead.
        let mut end = index;
        while end + 1 < positions.len() && positions[end + 1] - positions[end] <= span {
            end += 1;
        }
        let run = &positions[index..=end];
        index = end + 1;
        // Python's `max(run, key=...)` keeps the first of equal keys.
        let mut best = run[0];
        let mut best_quality = decoder.preamble_quality(best);
        for &candidate in &run[1..] {
            let quality = decoder.preamble_quality(candidate);
            if quality > best_quality {
                best = candidate;
                best_quality = quality;
            }
        }
        if (best as i64) - last < (span * PREAMBLE_BITS) as i64 {
            continue;
        }
        let window = decoder.read(best as i64, PREAMBLE_BITS + 16);
        let clamped: Vec<i8> = window.iter().map(|&b| b.max(0)).collect();
        let sync = bits_to_int(&clamped, PREAMBLE_BITS, 16) as u32;
        if (sync ^ SYNC_WORD).count_ones() > SYNC_MAX_ERRORS {
            continue;
        }
        starts.push(best);
        last = best as i64;
    }
    starts
}

fn frame_length(decoder: &RateDecoder, start: usize) -> Option<usize> {
    let bits = decoder.read(
        (start + decoder.span * (PREAMBLE_BITS + 16)) as i64,
        16,
    );
    if bits.contains(&-1) {
        return None;
    }
    let length = bits_to_int(&bits, 0, 16) as usize;
    (1..=65535).contains(&length).then_some(length)
}

fn payload_bits(decoder: &RateDecoder, start: usize, length: usize) -> Vec<i8> {
    let cursor = start + decoder.span * (PREAMBLE_BITS + 16 + 16);
    decoder.read(cursor as i64, length * 8 + 32)
}

fn decode_frame(decoder: &RateDecoder, start: usize) -> Option<Vec<u8>> {
    let length = frame_length(decoder, start)?;
    let bits = payload_bits(decoder, start, length);
    if bits.contains(&-1) {
        return None;
    }
    check_payload(&bits, length)
}

fn check_payload(bits: &[i8], length: usize) -> Option<Vec<u8>> {
    let payload: Vec<u8> = (0..length)
        .map(|i| bits_to_int(bits, i * 8, 8) as u8)
        .collect();
    let claimed = bits_to_int(bits, length * 8, 32) as u32;
    (claimed == crc32(&payload)).then_some(payload)
}

/// Re-encode a payload as the page lines the report tool reads.
pub fn emit_pages(payload: &[u8]) -> String {
    // The payload's own magic (b"PX7B", b"PX8B", ...) names the schema. The
    // page prefix must match it, or the report applies the wrong page rules:
    // this once emitted a hardcoded PX7 prefix on a PX8 payload and the report
    // refused the single page a PX8 conformance capture costs.
    let prefix = if payload.len() >= 4 && &payload[..2] == b"PX" && payload[3] == b'B' {
        String::from_utf8_lossy(&payload[..3]).into_owned()
    } else {
        "PX8".to_string()
    };
    let encoded = base64_encode(payload);
    let chunks: Vec<&str> = encoded
        .as_bytes()
        .chunks(BASE64_CHARS_PER_PAGE)
        .map(|chunk| std::str::from_utf8(chunk).expect("base64 is ASCII"))
        .collect();
    let mut lines = Vec::new();
    for (number, chunk) in chunks.iter().enumerate() {
        lines.push(format!(
            "{prefix}/{:02X}{:02X}/{chunk}/C:{:08X}",
            number + 1,
            chunks.len(),
            crc32(chunk.as_bytes())
        ));
    }
    lines.join("\n") + "\n"
}

/// Group frame offsets that belong to the same uploaded payload.
///
/// One transmission repeats with an exact period, so its repetitions share a
/// phase modulo that period. A rerun uploads a different payload and starts at
/// an unrelated offset, so it lands in a different group. Groups are returned
/// largest first, because more repetitions means a stronger vote.
fn transmission_groups(decoder: &RateDecoder, starts: &[usize]) -> Vec<(usize, Vec<usize>)> {
    let mut by_length: Vec<(usize, Vec<usize>)> = Vec::new();
    for &start in starts {
        if let Some(length) = frame_length(decoder, start) {
            match by_length.iter_mut().find(|(l, _)| *l == length) {
                Some((_, offsets)) => offsets.push(start),
                None => by_length.push((length, vec![start])),
            }
        }
    }
    let mut groups: Vec<(usize, Vec<usize>)> = Vec::new();
    for (length, offsets) in &by_length {
        // Total framed bits for this payload, hence the repetition period.
        let period = (decoder.span
            * (CALIBRATION_BITS + PREAMBLE_BITS + 16 + 16 + length * 8 + 32)) as i64;
        let mut buckets: Vec<(i64, Vec<usize>)> = Vec::new();
        for &offset in offsets {
            let phase = offset as i64 % period;
            // Tolerate drift: a capture clock is not the console's clock.
            let slot = buckets.iter_mut().find(|(known, _)| {
                let d = (phase - known).abs();
                d.min(period - d) <= PHASE_TOLERANCE
            });
            match slot {
                Some((_, members)) => members.push(offset),
                None => buckets.push((phase, vec![offset])),
            }
        }
        for (_, mut members) in buckets {
            members.sort_unstable();
            groups.push((*length, members));
        }
    }
    groups.sort_by_key(|(_, members)| std::cmp::Reverse(members.len()));
    groups
}

/// Per-bit majority vote across repetitions.
///
/// The transmission loops forever, so a marginal chain typically corrupts a
/// DIFFERENT bit in each repetition. Decoding repetitions independently needs
/// one flawless pass and can fail on all of them; voting per bit recovers a
/// payload that no single repetition carried cleanly.
fn vote_payload(frames: &[Vec<i8>], length: usize) -> Option<Vec<u8>> {
    if frames.is_empty() {
        return None;
    }
    let needed = length * 8 + 32;
    let mut voted = Vec::with_capacity(needed);
    for index in 0..needed {
        let (mut ones, mut valid) = (0i32, 0i32);
        for frame in frames {
            if index < frame.len() && frame[index] >= 0 {
                valid += 1;
                ones += i32::from(frame[index]);
            }
        }
        if valid == 0 {
            return None;
        }
        voted.push(i8::from(ones * 2 > valid));
    }
    check_payload(&voted, length)
}

/// Linear-resample a recording to the link's 44.1 kHz bit clock.
///
/// Linear interpolation is sufficient here because the decision is which of two
/// tones dominates a window, not a faithful waveform reconstruction, and both
/// tones sit far below Nyquist at any plausible capture rate.
pub fn resample(samples: &[f64], source: u32, target: u32) -> Vec<f64> {
    if source == target || samples.is_empty() {
        return samples.to_vec();
    }
    let count = (samples.len() as f64 * f64::from(target) / f64::from(source)) as usize;
    let ratio = f64::from(source) / f64::from(target);
    let last = samples.len() - 1;
    (0..count)
        .map(|i| {
            let x = i as f64 * ratio;
            if x >= last as f64 {
                return samples[last];
            }
            let j = x.floor() as usize;
            let slope = samples[j + 1] - samples[j];
            slope * (x - j as f64) + samples[j]
        })
        .collect()
}

/// A decoded 16-bit PCM WAV file: interleaved samples and the format.
pub struct Wav {
    pub samples: Vec<i16>,
    pub rate: u32,
    pub channels: u16,
}

pub fn read_wav_file(path: &Path) -> Result<Wav> {
    let data = std::fs::read(path).map_err(|e| Error(format!("{}: {e}", path.display())))?;
    ensure!(
        data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WAVE",
        "file does not start with RIFF id"
    );
    let (mut rate, mut channels, mut bits) = (0u32, 0u16, 0u16);
    let mut at = 12;
    let mut pcm: Option<&[u8]> = None;
    while at + 8 <= data.len() {
        let id = &data[at..at + 4];
        let size = u32::from_le_bytes(data[at + 4..at + 8].try_into().expect("4")) as usize;
        let body = &data[at + 8..(at + 8 + size).min(data.len())];
        if id == b"fmt " && body.len() >= 16 {
            channels = u16::from_le_bytes([body[2], body[3]]);
            rate = u32::from_le_bytes(body[4..8].try_into().expect("4"));
            bits = u16::from_le_bytes([body[14], body[15]]);
        } else if id == b"data" {
            pcm = Some(body);
        }
        at += 8 + size + (size & 1);
    }
    ensure!(bits != 0, "fmt chunk missing");
    ensure!(bits == 16, "expected 16-bit PCM");
    let pcm = pcm.ok_or_else(|| Error("data chunk missing".into()))?;
    let samples = pcm
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    Ok(Wav {
        samples,
        rate,
        channels,
    })
}

/// Mono samples, channels summed: the link is mono, and summing recovers it
/// whether the capture chain carried it on one side or both.
fn read_wav(path: &Path) -> Result<(Vec<f64>, u32)> {
    let wav = read_wav_file(path)?;
    let channels = usize::from(wav.channels.max(1));
    let values: Vec<f64> = wav.samples.iter().map(|&s| f64::from(s)).collect();
    if channels > 1 {
        let usable = values.len() - values.len() % channels;
        let summed = values[..usable]
            .chunks(channels)
            .map(|frame| frame.iter().sum())
            .collect();
        return Ok((summed, wav.rate));
    }
    Ok((values, wav.rate))
}

struct Options {
    wav: String,
    out: Option<String>,
    emit_pages: Option<String>,
    max_frames: usize,
}

fn parse_args(args: &[String]) -> Result<Options> {
    let mut wav = None;
    let (mut out, mut emit_pages) = (None, None);
    let mut max_frames = 6;
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let value = |i: &mut usize| -> Result<String> {
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| Error(format!("{flag} needs a value")))
        };
        match flag {
            "--out" => out = Some(value(&mut i)?),
            "--emit-pages" => emit_pages = Some(value(&mut i)?),
            "--max-frames" => {
                max_frames = value(&mut i)?
                    .parse()
                    .map_err(|_| Error("--max-frames must be an integer".into()))?;
            }
            other if other.starts_with("--") => bail!("unrecognized arguments: {other}"),
            path if wav.is_none() => wav = Some(path.to_string()),
            extra => bail!("unrecognized arguments: {extra}"),
        }
        i += 1;
    }
    Ok(Options {
        wav: wav.ok_or_else(|| Error("the following arguments are required: wav".into()))?,
        out,
        emit_pages,
        max_frames,
    })
}

fn emit(payload: &[u8], options: &Options, out: &mut dyn Write) -> Result<i32> {
    writeln!(out, "# recovered {} bytes", payload.len())?;
    writeln!(out, "# crc32={:08X}", crc32(payload))?;
    if let Some(path) = &options.out {
        std::fs::write(path, payload)?;
        writeln!(out, "# wrote {path}")?;
    }
    if let Some(path) = &options.emit_pages {
        std::fs::write(path, emit_pages(payload))?;
        writeln!(out, "# wrote {path}")?;
    }
    Ok(0)
}

/// `psoxide-hwtest audio-decode <wav> [--out FILE] [--emit-pages FILE] [--max-frames N]`
pub fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> Result<i32> {
    let options = parse_args(args)?;
    let (mut samples, rate) = read_wav(Path::new(&options.wav))?;
    let duration = samples.len() as f64 / f64::from(rate.max(1));
    if rate != LINK_RATE {
        writeln!(out, "# resampling {rate} Hz -> {LINK_RATE} Hz")?;
        samples = resample(&samples, rate, LINK_RATE);
    }
    writeln!(out, "# samples={} rate={rate} ({duration:.1}s)", samples.len())?;

    for span in RATE_SAMPLES_PER_BIT {
        let decoder = RateDecoder::new(&samples, span);
        let starts = find_frames(&decoder, options.max_frames);
        if starts.is_empty() {
            continue;
        }
        writeln!(
            out,
            "# rate {span} samples/bit: {} frame(s) at {starts:?}",
            starts.len()
        )?;
        // A clean repetition needs no voting, so try each on its own first.
        for &start in &starts {
            if let Some(payload) = decode_frame(&decoder, start) {
                return emit(&payload, &options, out);
            }
        }
        // Otherwise vote. Frames must be grouped by TRANSMISSION first: a
        // recording spans reruns, each re-uploading a different payload under
        // the same framing, and voting across two payloads yields neither.
        // Repetitions of one transmission are an exact period apart, so their
        // start offsets share a phase.
        for (length, group) in transmission_groups(&decoder, &starts) {
            if group.len() < 3 {
                continue;
            }
            writeln!(
                out,
                "# voting across {} repetitions of one transmission",
                group.len()
            )?;
            let frames: Vec<Vec<i8>> = group
                .iter()
                .map(|&start| payload_bits(&decoder, start, length))
                .collect();
            if let Some(voted) = vote_payload(&frames, length) {
                return emit(&voted, &options, out);
            }
        }
    }
    writeln!(err, "FAIL: no frame recovered at any rate")?;
    Ok(1)
}
