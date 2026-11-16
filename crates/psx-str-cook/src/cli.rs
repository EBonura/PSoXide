//! The command line; [`USAGE`] lists the commands and options.
//!
//! `compare` measures streams against the source they were made from: the
//! PSNR of every frame decoded the MDEC's way against the source picture at
//! the same time and against the closest one within three frames, and with
//! `--audio` the sound's SNR against the source WAV.

use std::io::BufRead;
use std::process::ExitCode;

use psx_audio_cook::xa::DriveSpeed;

use crate::stream::{self, Options, Sound};
use crate::{inspect, model};

/// The command line's usage text.
pub const USAGE: &str = "usage:
  psx-str-cook encode --video IN.y4m|- --audio IN.wav --out OUT.STR
                      [--size 320x240] [--speed 2] [--channels 1] [--xa-file 1]
                      [--xa-channel 0] [--audio-delay 0] [--max-rle-words N] [--frames N]
  psx-str-cook inspect STREAM [--frames]
  psx-str-cook compare --source IN.y4m [--audio IN.wav] STREAM...";

/// How to make the inputs from a movie file, for error messages.
const DECODE_HINT: &str = "make the inputs from the source movie first, for example:
  ffmpeg -i MOVIE -map 0:v:0 -f yuv4mpegpipe -pix_fmt yuv420p video.y4m
  ffmpeg -i MOVIE -map 0:a:0 -ac 1 -c:a pcm_s16le -f wav audio.wav";

/// Runs the command line `args` (without the program name).
pub fn run(args: &[String]) -> ExitCode {
    let result = match args.first().map(String::as_str) {
        Some("encode") => encode_command(&args[1..]),
        Some("inspect") => inspect_command(&args[1..]),
        Some("compare") => compare_command(&args[1..]),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("psx-str-cook: {message}");
            ExitCode::FAILURE
        }
    }
}

fn read(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("{path}: {e}"))
}

fn inspect_command(args: &[String]) -> Result<(), String> {
    let path = args.first().ok_or(USAGE)?;
    let list = args.iter().any(|a| a == "--frames");
    let raw = read(path)?;
    let layout = inspect::layout(&raw)?;
    if list {
        for f in &layout.frames {
            println!(
                "frame {:4} chunks {:2} bytes {:6} qscale {:2} sectors {:5}..={:5}",
                f.number, f.chunks, f.size, f.qscale, f.sectors.0, f.sectors.1
            );
        }
    }
    let chunks: usize = layout.frames.iter().map(|f| usize::from(f.chunks)).sum();
    let bytes: usize = layout.frames.iter().map(|f| f.size as usize).sum();
    println!(
        "{path}: {} sectors, {} frames in {chunks} video sectors ({bytes} bitstream bytes), {} audio, {} other, {} dropped, max {} chunks, max {} RLE words",
        layout.sectors,
        layout.frames.len(),
        layout.audio.len(),
        layout.other,
        layout.dropped,
        layout.frames.iter().map(|f| f.chunks).max().unwrap_or(0),
        layout.frames.iter().map(|f| f.rle_words).max().unwrap_or(0)
    );
    if let Ok((hz, pcm)) = inspect::audio(&raw, raw[1]) {
        println!(
            "sound: {} samples at {hz} Hz ({:.3} s)",
            pcm.len(),
            pcm.len() as f64 / f64::from(hz)
        );
    }
    Ok(())
}

/// Every source frame scaled to `size` and converted to RGB the way the
/// MDEC would show it at full precision.
fn source_frames(path: &str, size: (usize, usize)) -> Result<Vec<Vec<u8>>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let mut reader = crate::y4m::Reader::new(std::io::BufReader::new(file))
        .map_err(|e| format!("{path}: {e}"))?;
    let header = reader.header();
    let mut out = Vec::new();
    while let Some(frame) = reader.next_frame().map_err(|e| format!("{path}: {e}"))? {
        let planes = crate::picture::Planes::from_y4m(&header, &frame);
        out.push(planes.scaled(size.0, size.1).to_rgb());
    }
    Ok(out)
}

fn compare_command(args: &[String]) -> Result<(), String> {
    let (mut source, mut audio, mut streams) = (None, None, Vec::new());
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--source" => source = it.next().cloned(),
            "--audio" => audio = it.next().cloned(),
            other => streams.push(other.to_string()),
        }
    }
    let source = source.ok_or(USAGE)?;
    let verbose = std::env::var_os("PSX_STR_COOK_VERBOSE").is_some();
    for path in &streams {
        let layout = inspect::layout(&read(path)?)?;
        let first = layout.frames.first().ok_or(format!("{path}: no frames"))?;
        let size = (usize::from(first.size_px.0), usize::from(first.size_px.1));
        let reference = source_frames(&source, size)?;
        let samples = (reference[0].len() * layout.frames.len()) as f64;
        // Squared error against the source frame of the same time, and
        // against the closest source frame within three of it (coding
        // error alone, without timing error).
        let (mut timed, mut closest) = (0.0, 0.0);
        let mut worst = (f64::INFINITY, 0u32);
        let mut distinct = 0;
        for (i, f) in layout.frames.iter().enumerate() {
            if i == 0 || f.bitstream != layout.frames[i - 1].bitstream {
                distinct += 1;
            }
            let rgb = f.rgb()?;
            let at = (f.number as usize)
                .saturating_sub(1)
                .min(reference.len() - 1);
            let here = model::squared_error(&rgb, &reference[at]);
            let range = at.saturating_sub(3)..=(at + 3).min(reference.len() - 1);
            let (best, best_at) = range
                .map(|s| (model::squared_error(&rgb, &reference[s]), s))
                .fold((f64::INFINITY, at), |a, b| if b.0 < a.0 { b } else { a });
            let p = model::psnr_of(here / rgb.len() as f64);
            if verbose {
                println!(
                    "frame {:4} {p:6.2} dB, closest source frame {:4} at {:6.2} dB",
                    f.number,
                    best_at + 1,
                    model::psnr_of(best / rgb.len() as f64)
                );
            }
            timed += here;
            closest += best;
            if p < worst.0 {
                worst = (p, f.number);
            }
        }
        println!(
            "{path}: {} frames ({distinct} distinct bitstreams) against {} source frames: \
             PSNR {:.2} dB against the source at the same time, {:.2} dB against the closest \
             source frame, worst frame {} at {:.2} dB",
            layout.frames.len(),
            reference.len(),
            model::psnr_of(timed / samples),
            model::psnr_of(closest / samples),
            worst.1,
            worst.0,
        );
        if let Some(wav) = &audio {
            sound_report(path, &read(path)?, wav)?;
        }
    }
    Ok(())
}

fn parse_size(text: &str) -> Result<(u16, u16), String> {
    text.split_once('x')
        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
        .ok_or_else(|| format!("--size {text}: expected WIDTHxHEIGHT"))
}

fn parse_number<T: std::str::FromStr>(flag: &str, text: &str) -> Result<T, String> {
    text.parse()
        .map_err(|_| format!("{flag} {text}: not a number"))
}

fn encode_command(args: &[String]) -> Result<(), String> {
    let mut options = Options::default();
    let (mut video, mut audio, mut out) = (None, None, None);
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let value = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--video" => video = Some(value.clone()),
            "--audio" => audio = Some(value.clone()),
            "--out" => out = Some(value.clone()),
            "--size" => options.size = parse_size(value)?,
            "--speed" => {
                options.speed = match value.as_str() {
                    "1" => DriveSpeed::Single,
                    "2" => DriveSpeed::Double,
                    _ => return Err(format!("--speed {value}: 1 or 2")),
                }
            }
            "--channels" => {
                options.stereo = match value.as_str() {
                    "1" => false,
                    "2" => true,
                    _ => return Err(format!("--channels {value}: 1 or 2")),
                }
            }
            "--xa-file" => options.channel.file = parse_number(flag, value)?,
            "--xa-channel" => options.channel.channel = parse_number(flag, value)?,
            "--audio-delay" => options.audio_delay = parse_number(flag, value)?,
            "--max-rle-words" => options.max_rle_words = Some(parse_number(flag, value)?),
            "--frames" => options.max_frames = Some(parse_number(flag, value)?),
            other => return Err(format!("unknown option {other}\n{USAGE}")),
        }
    }
    let video = video
        .ok_or_else(|| format!("encode needs --video (a Y4M file, or - for stdin)\n{USAGE}"))?;
    let out = out.ok_or_else(|| format!("encode needs --out (the STR to write)\n{USAGE}"))?;
    for (what, path) in [("video", Some(&video)), ("audio", audio.as_ref())] {
        if let Some(path) = path.filter(|p| *p != "-") {
            if !std::path::Path::new(path).is_file() {
                return Err(format!(
                    "the {what} input {path} does not exist\n{DECODE_HINT}"
                ));
            }
        }
    }
    let sound = match audio {
        Some(path) => {
            let (rate, channels) = psx_audio_cook::wav::read_channels(&read(&path)?)
                .map_err(|e| format!("{path}: {e}"))?;
            Some(Sound { rate, channels })
        }
        None => None,
    };
    let input: Box<dyn BufRead> = if video == "-" {
        Box::new(std::io::stdin().lock())
    } else {
        let file = std::fs::File::open(&video).map_err(|e| format!("{video}: {e}"))?;
        Box::new(std::io::BufReader::new(file))
    };
    let reader = crate::y4m::Reader::new(input).map_err(|e| format!("{video}: {e}"))?;
    let (raw, report) = stream::encode(reader, sound.as_ref(), &options)?;
    std::fs::write(&out, &raw).map_err(|e| format!("{out}: {e}"))?;
    let mut histogram = std::collections::BTreeMap::new();
    for q in &report.qscales {
        *histogram.entry(q).or_insert(0u32) += 1;
    }
    eprintln!(
        "psx-str-cook: {out}: {} frames {}x{}, {} sectors ({} video, {} sound of {}), \
         max {} chunks, max {} RLE words, XA {:?}, {} of {} bitstream bytes used, qscale:frames {histogram:?}",
        report.frames,
        options.size.0,
        options.size.1,
        report.sectors,
        report.video_sectors,
        report.audio_sectors.0,
        report.audio_sectors.1,
        report.max_chunks,
        report.rle_words,
        report.xa,
        report.bytes.0,
        report.bytes.1,
    );
    Ok(())
}

/// The stream's sound against the WAV it was made from: the source taken to
/// 37.8 kHz mono with the same resampler, aligned to the lag (within 20 ms)
/// that matches best, and the signal-to-noise ratio over the common length.
fn sound_report(path: &str, raw: &[u8], wav: &str) -> Result<(), String> {
    use psx_audio_cook::xa::{prepare_pcm, Format, SampleRate};
    let (rate, channels) =
        psx_audio_cook::wav::read_channels(&read(wav)?).map_err(|e| format!("{wav}: {e}"))?;
    let format = Format {
        stereo: false,
        rate: SampleRate::Hz37800,
    };
    let source = prepare_pcm(format, rate, &channels, None).remove(0);
    let (hz, decoded) = inspect::audio(raw, raw[1])?;
    let snr = |lag: usize| {
        let n = decoded.len().saturating_sub(lag).min(source.len());
        let (mut signal, mut noise) = (0.0, 0.0);
        for i in 0..n {
            let (s, d) = (
                source[i].clamp(-32768.0, 32767.0),
                f64::from(decoded[i + lag]),
            );
            signal += s * s;
            noise += (s - d) * (s - d);
        }
        (10.0 * (signal / noise.max(1e-9)).log10(), n)
    };
    let (best, lag) =
        (0..756)
            .map(|lag| (snr(lag), lag))
            .fold(((f64::NEG_INFINITY, 0), 0), |a, b| {
                if b.0 .0 > a.0 .0 {
                    b
                } else {
                    a
                }
            });
    println!(
        "{path}: sound {} samples at {hz} Hz ({:.3} s), SNR {:.2} dB against the source over \
         {} samples at a lag of {lag} samples",
        decoded.len(),
        decoded.len() as f64 / f64::from(hz),
        best.0,
        best.1
    );
    Ok(())
}
