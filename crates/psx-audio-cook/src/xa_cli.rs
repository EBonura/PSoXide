//! The `xa-encode`, `xa-decode` and `xa-score` subcommands of `psx-audio-cook`.

use crate::xa::{self, DriveSpeed, Format, SampleRate};
use crate::{metrics, wav};
use std::path::Path;
use std::process::ExitCode;

/// Usage text of the XA subcommands.
pub const USAGE: &str = "  psx-audio-cook xa-encode OUT.XA SONG.wav... [--rate 37800|18900] [--mono] [--speed 1|2] [--file N] [--peak F] [--manifest OUT.json]\n  psx-audio-cook xa-decode IN.XA CHANNEL OUT.wav\n  psx-audio-cook xa-score SRC.wav IN.XA CHANNEL\n  psx-audio-cook xa-peaks CAPTURE.wav";

fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("{message}");
    ExitCode::FAILURE
}

struct Options {
    format: Format,
    speed: DriveSpeed,
    file: u8,
    peak: Option<f64>,
    manifest: Option<String>,
    songs: Vec<String>,
}

fn parse(flags: &[String]) -> Result<Options, String> {
    let mut o = Options {
        format: Format {
            stereo: true,
            rate: SampleRate::Hz37800,
        },
        speed: DriveSpeed::Single,
        file: 1,
        peak: None,
        manifest: None,
        songs: Vec::new(),
    };
    let mut i = 0;
    while i < flags.len() {
        let value = flags.get(i + 1).map(String::as_str);
        match (flags[i].as_str(), value) {
            ("--rate", Some("37800")) => o.format.rate = SampleRate::Hz37800,
            ("--rate", Some("18900")) => o.format.rate = SampleRate::Hz18900,
            ("--speed", Some("1")) => o.speed = DriveSpeed::Single,
            ("--speed", Some("2")) => o.speed = DriveSpeed::Double,
            ("--file", Some(v)) => o.file = v.parse().map_err(|_| format!("bad --file {v}"))?,
            ("--peak", Some(v)) => o.peak = Some(v.parse().map_err(|_| format!("bad --peak {v}"))?),
            ("--manifest", Some(v)) => o.manifest = Some(v.to_string()),
            ("--mono", _) => {
                o.format.stereo = false;
                i += 1;
                continue;
            }
            (flag, _) if !flag.starts_with("--") => {
                o.songs.push(flag.to_string());
                i += 1;
                continue;
            }
            (flag, _) => return Err(format!("unknown or incomplete option {flag}")),
        }
        i += 2;
    }
    if o.songs.is_empty() {
        return Err("no song WAVs given".into());
    }
    Ok(o)
}

fn stem(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .map_or_else(|| path.to_string(), |s| s.to_string_lossy().into_owned())
}

/// `xa-encode OUT.XA SONG.wav... [options]`.
pub fn encode(output: &str, flags: &[String]) -> ExitCode {
    let o = match parse(flags) {
        Ok(o) => o,
        Err(e) => return fail(e),
    };
    let mut pcm = Vec::new();
    for path in &o.songs {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => return fail(format!("{path}: {e}")),
        };
        match wav::read_channels(&bytes) {
            Ok((rate, channels)) => pcm.push(xa::prepare_pcm(o.format, rate, &channels, o.peak)),
            Err(e) => return fail(format!("{path}: {e}")),
        }
    }
    let frames = |p: &Vec<Vec<f64>>| p.iter().map(Vec::len).max().unwrap_or(0);
    let sectors = pcm
        .iter()
        .map(|p| xa::sector_count(o.format, frames(p)))
        .max()
        .unwrap_or(1);
    let encoded: Vec<_> = pcm
        .iter()
        .map(|p| xa::encode(o.format, p, sectors))
        .collect();
    let file = match xa::interleave(o.format, o.speed, o.file, &encoded) {
        Ok(f) => f,
        Err(e) => return fail(e),
    };
    if let Err(e) = std::fs::write(output, &file.bytes) {
        return fail(format!("{output}: {e}"));
    }
    let manifest = manifest_json(&o, &file, sectors, &pcm);
    if let Some(path) = &o.manifest {
        if let Err(e) = std::fs::write(path, &manifest) {
            return fail(format!("{path}: {e}"));
        }
    }
    println!("{manifest}");
    if o.songs.len() < file.stride {
        eprintln!(
            "note: {} of {} interleave slots used; the rest is filler that still takes disc space",
            o.songs.len(),
            file.stride
        );
    }
    ExitCode::SUCCESS
}

fn manifest_json(
    o: &Options,
    file: &xa::Interleaved,
    sectors: usize,
    pcm: &[Vec<Vec<f64>>],
) -> String {
    let (num, den) = o.format.sectors_per_second_ratio();
    let songs: Vec<String> = o
        .songs
        .iter()
        .zip(pcm)
        .enumerate()
        .map(|(channel, (path, p))| {
            let samples = p.iter().map(Vec::len).max().unwrap_or(0);
            format!(
                "{{\"name\":\"{}\",\"channel\":{channel},\"samples\":{samples},\"seconds\":{:.3}}}",
                stem(path).replace(['\\', '"'], "_"),
                samples as f64 / o.format.rate.hz() as f64
            )
        })
        .collect();
    format!(
        "{{\"file\":{},\"stereo\":{},\"sample_rate\":{},\"drive_speed\":{},\"stride\":{},\"song_sectors\":{sectors},\"sectors\":{},\"sectors_per_second\":\"{num}/{den}\",\"songs\":[{}]}}",
        o.file,
        o.format.stereo,
        o.format.rate.hz(),
        o.speed.sectors_per_second() / 75,
        file.stride,
        file.sector_count,
        songs.join(",")
    )
}

fn load_file(path: &str, channel: &str) -> Result<(Format, Vec<Vec<i16>>), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let channel = channel
        .parse()
        .map_err(|_| format!("bad channel {channel}"))?;
    xa::decode_channel(&bytes, channel)
}

/// `xa-decode IN.XA CHANNEL OUT.wav`: the reference decoder's output at the
/// file's own sample rate.
pub fn decode(input: &str, channel: &str, output: &str) -> ExitCode {
    let (format, pcm) = match load_file(input, channel) {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    let planes: Vec<&[i16]> = pcm.iter().map(Vec::as_slice).collect();
    match std::fs::write(output, wav::write_pcm16(format.rate.hz(), &planes)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(format!("{output}: {e}")),
    }
}

/// `xa-score SRC.wav IN.XA CHANNEL`: SNR in dB of the decoded channel against
/// the source resampled to the file's rate, per channel, as one JSON line.
pub fn score(source: &str, input: &str, channel: &str) -> ExitCode {
    let (format, decoded) = match load_file(input, channel) {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    let bytes = match std::fs::read(source) {
        Ok(b) => b,
        Err(e) => return fail(format!("{source}: {e}")),
    };
    let (rate, channels) = match wav::read_channels(&bytes) {
        Ok(r) => r,
        Err(e) => return fail(format!("{source}: {e}")),
    };
    let reference = xa::prepare_pcm(format, rate, &channels, None);
    let snr: Vec<String> = reference
        .iter()
        .zip(&decoded)
        .map(|(r, d)| {
            let n = r.len().min(d.len());
            format!(
                "{:.2}",
                metrics::snr_db(&crate::resample::to_i16(&r[..n]), &d[..n])
            )
        })
        .collect();
    println!("{{\"snr_db\":[{}]}}", snr.join(","));
    ExitCode::SUCCESS
}

/// `xa-peaks CAPTURE.wav`: for every half second of the left channel, the
/// start time, RMS level and strongest frequency, as tab-separated lines.
/// The gate that plays `hello-xa` headless reads an emulator audio capture
/// with it.
pub fn peaks(input: &str) -> ExitCode {
    const FRAME: usize = 16_384;
    let bytes = match std::fs::read(input) {
        Ok(b) => b,
        Err(e) => return fail(format!("{input}: {e}")),
    };
    let (rate, channels) = match wav::read_channels(&bytes) {
        Ok(r) => r,
        Err(e) => return fail(format!("{input}: {e}")),
    };
    let left = &channels[0];
    let hop = (rate / 2) as usize;
    let mut at = 0;
    while at + FRAME <= left.len() {
        let window = &left[at..at + FRAME];
        let rms = (window.iter().map(|v| v * v).sum::<f64>() / FRAME as f64).sqrt();
        let mut re: Vec<f64> = window
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let hann = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / FRAME as f64).cos();
                v * hann
            })
            .collect();
        let mut im = vec![0.0; FRAME];
        metrics::fft(&mut re, &mut im);
        let bin = (1..FRAME / 2)
            .max_by(|&a, &b| {
                let (pa, pb) = (re[a].hypot(im[a]), re[b].hypot(im[b]));
                pa.total_cmp(&pb)
            })
            .unwrap_or(0);
        let hz = bin as f64 * rate as f64 / FRAME as f64;
        println!("{:.1}\t{:.0}\t{:.0}", at as f64 / rate as f64, rms, hz);
        at += hop;
    }
    ExitCode::SUCCESS
}
