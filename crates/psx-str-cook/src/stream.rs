//! The whole encode: which sector carries what, and how much each frame may
//! spend.
//!
//! The drive reads `75 * speed` sectors a second, so a movie at `fps` frames a
//! second has `75 * speed / fps` sectors of time per frame (10 for 15 frames
//! a second at double speed). Frame `k` (from 0) owns the sectors from
//! `floor(k * 75 * speed / fps)` up to frame `k + 1`'s first. The sound takes
//! one sector in every `stride` (16 for mono 37.8 kHz at double speed, so the
//! drive delivers 37,800 samples a second), from sector `audio_delay` on,
//! for as long as it lasts; every other sector of a frame's window carries
//! that frame's picture, so the window's video sectors are the frame's byte
//! budget (2016 bytes each) and [`rate`] picks the finest quantisation scale
//! whose frame fits it. A frame is complete when its window has been read,
//! which is when the player may show it.
//!
//! With `audio_delay` 0 this is the layout psxavenc writes (sound first, then
//! picture). A player that shows each frame some time after its last sector
//! arrives moves the sound later by the same time with `audio_delay`, so
//! that the two play together.

use std::io::BufRead;

use psx_audio_cook::xa::{self, DriveSpeed, Format, SampleRate};

use crate::frame::{Encoded, Transformed};
use crate::picture::Planes;
use crate::sector::{self, Channel, RECORD_BYTES};
use crate::y4m;

/// Bitstream bytes a video sector carries.
pub const CHUNK_BYTES: usize = psx_fmv::stream::CHUNK_PAYLOAD_BYTES;

/// How to encode.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Options {
    /// Picture size, multiples of 16. Default 320x240.
    pub size: (u16, u16),
    /// Frame rate as numerator and denominator; `None` keeps the source's.
    pub fps: Option<(u32, u32)>,
    /// Drive speed the stream is read at. Default double.
    pub speed: DriveSpeed,
    /// XA file and channel of every sector. Default file 1, channel 0.
    pub channel: Channel,
    /// Stereo rather than mono sound. Default mono.
    pub stereo: bool,
    /// Sectors before the first sound sector. Default 0.
    pub audio_delay: usize,
    /// Most run-length words a frame may decode to (the player's MDEC
    /// buffer), `None` for no limit beyond the sector budget.
    pub max_rle_words: Option<usize>,
    /// Stop after this many frames, `None` for the whole source.
    pub max_frames: Option<u32>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            size: (320, 240),
            fps: None,
            speed: DriveSpeed::Double,
            channel: Channel {
                file: 1,
                channel: 0,
            },
            stereo: false,
            audio_delay: 0,
            max_rle_words: None,
            max_frames: None,
        }
    }
}

/// The sound to interleave: its sample rate and one sample vector per
/// channel, in the signed 16-bit range (as `psx_audio_cook::wav` reads it).
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Sound {
    /// Sample rate in Hz.
    pub rate: u32,
    /// Samples per channel.
    pub channels: Vec<Vec<f64>>,
}

impl Sound {
    /// Sound at `rate` Hz, one sample vector per channel.
    #[must_use]
    pub fn new(rate: u32, channels: Vec<Vec<f64>>) -> Sound {
        Sound { rate, channels }
    }
}

/// What the encode did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Report {
    /// Frames written.
    pub frames: u32,
    /// Sectors in the stream.
    pub sectors: usize,
    /// Video sectors.
    pub video_sectors: usize,
    /// Sound sectors written, and how many the sound had (the rest was past
    /// the last frame and is not kept).
    pub audio_sectors: (usize, usize),
    /// Quantisation scale of every frame.
    pub qscales: Vec<u16>,
    /// Most video sectors a frame had.
    pub max_chunks: u16,
    /// Bitstream bytes written and bytes the video sectors could carry.
    pub bytes: (usize, usize),
    /// Picture width and height.
    pub size: (u16, u16),
    /// Most run-length words a frame decodes to: the MDEC buffer a player
    /// needs (the SDK decoder's padded count, as `decode_frame` returns it).
    pub rle_words: usize,
    /// XA file and channel of the sound, `None` for a silent movie.
    pub xa: Option<(u8, u8)>,
}

/// The finest quantisation scale whose frame fits `budget` bytes, or `None`
/// when not even the coarsest does.
#[must_use]
pub fn rate(picture: &Transformed, budget: usize, max_rle_words: Option<usize>) -> Option<Encoded> {
    let fits = |q: u16| {
        Some(picture.encode(q)).filter(|e| {
            e.bytes.len() <= budget && max_rle_words.is_none_or(|most| e.rle_words <= most)
        })
    };
    if let Some(e) = fits(1) {
        return Some(e);
    }
    // Size falls with the scale; search for the first that fits, then step
    // down while a finer one still does (the fall is not strictly monotone).
    let (mut lo, mut hi) = (1u16, 63u16);
    let mut best = fits(hi)?;
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        match fits(mid) {
            Some(e) => {
                hi = mid;
                best = e;
            }
            None => lo = mid,
        }
    }
    Some(best)
}

/// First sector of frame `k`'s window.
fn window_start(k: u64, sectors_per_second: u64, fps: (u32, u32)) -> usize {
    (k * sectors_per_second * u64::from(fps.1) / u64::from(fps.0)) as usize
}

/// Encodes the pictures of a Y4M stream and `sound` into sector records:
/// [`encode_frames`] over the stream's frames at its own frame rate. The
/// reader can sit on a file, on memory or on a decoder's piped output.
///
/// # Errors
///
/// As [`encode_frames`], and when the video cannot be read.
pub fn encode<R: BufRead>(
    mut video: y4m::Reader<R>,
    sound: Option<&Sound>,
    options: &Options,
) -> Result<(Vec<u8>, Report), String> {
    let header = video.header();
    let next = move || -> Result<Option<Planes>, String> {
        let frame = video.next_frame().map_err(|e| format!("video: {e}"))?;
        Ok(frame.map(|frame| Planes::from_y4m(&header, &frame)))
    };
    encode_frames(next, header.fps, sound, options)
}

/// Encodes pictures and `sound` into sector records: the STR's bytes (raw
/// 2336-byte records for `mkisopsx --xa-file`) and a [`Report`] carrying the
/// figures a player sizes itself by (frames, size, chunks, run-length words,
/// XA file and channel).
///
/// `next_frame` hands out the source pictures in order, `None` at the end;
/// they come at `source_fps` (numerator, denominator) and may be any size
/// (each is scaled to [`Options::size`]).
///
/// # Errors
///
/// `next_frame` failed, a size is not whole macroblocks, a frame's window has
/// no video sector, or a frame does not fit its window at the coarsest
/// quantisation (or under [`Options::max_rle_words`]).
///
/// # Panics
///
/// Only on an internal inconsistency: a window with fewer video sectors
/// than it counted.
pub fn encode_frames(
    mut next_frame: impl FnMut() -> Result<Option<Planes>, String>,
    source_fps: (u32, u32),
    sound: Option<&Sound>,
    options: &Options,
) -> Result<(Vec<u8>, Report), String> {
    let (width, height) = options.size;
    if width == 0 || height == 0 || width % 16 != 0 || height % 16 != 0 {
        return Err(format!("{width}x{height} is not whole 16x16 macroblocks"));
    }
    if source_fps.0 == 0 || source_fps.1 == 0 {
        return Err("the source frame rate is zero".into());
    }
    let fps = options.fps.unwrap_or(source_fps);
    if fps.0 == 0 || fps.1 == 0 {
        return Err("the frame rate is zero".into());
    }
    let sectors_per_second = u64::from(options.speed.sectors_per_second());
    let format = Format {
        stereo: options.stereo,
        rate: SampleRate::Hz37800,
    };
    let stride = format.stride(options.speed);
    let audio: Vec<xa::SectorData> = match sound {
        Some(s) => xa::encode(
            format,
            &xa::prepare_pcm(format, s.rate, &s.channels, None),
            0,
        ),
        None => Vec::new(),
    };
    let coding = format.coding_info();

    let mut out = Vec::new();
    let mut report = Report {
        audio_sectors: (0, audio.len()),
        size: options.size,
        xa: sound.map(|_| (options.channel.file, options.channel.channel)),
        ..Report::default()
    };
    let mut next_audio = audio.iter();
    let mut pending_audio = next_audio.next();
    let mut source: Option<(u64, Planes)> = None;
    let mut source_index = 0u64;
    let mut k = 0u64;
    while options.max_frames.is_none_or(|most| k < u64::from(most)) {
        // The source frame shown at this frame's time.
        let wanted = k * u64::from(source_fps.0) * u64::from(fps.1)
            / (u64::from(source_fps.1) * u64::from(fps.0));
        while source.as_ref().is_none_or(|(at, _)| *at < wanted) {
            match next_frame()? {
                Some(planes) => {
                    source = Some((source_index, planes));
                    source_index += 1;
                }
                None => {
                    source = None;
                    break;
                }
            }
        }
        let Some((_, planes)) = source.as_ref() else {
            break;
        };
        let (first, end) = (
            window_start(k, sectors_per_second, fps),
            window_start(k + 1, sectors_per_second, fps),
        );
        let is_audio = |s: usize, left: bool| {
            left && s >= options.audio_delay && (s - options.audio_delay).is_multiple_of(stride)
        };
        // Sound sectors are taken in order, so count this window's ahead.
        let mut left = audio.len() - report.audio_sectors.0;
        let mut chunks = 0u16;
        for s in first..end {
            if is_audio(s, left > 0) {
                left -= 1;
            } else {
                chunks += 1;
            }
        }
        let number = u32::try_from(k + 1).map_err(|_| "too many frames")?;
        if chunks == 0 || usize::from(chunks) > usize::from(psx_fmv::stream::MAX_CHUNKS) {
            return Err(format!(
                "frame {number}: {chunks} video sectors in its window"
            ));
        }
        let budget = usize::from(chunks) * CHUNK_BYTES;
        let transformed = Transformed::new(&planes.scaled(usize::from(width), usize::from(height)));
        let encoded = rate(&transformed, budget, options.max_rle_words)
            .ok_or_else(|| format!("frame {number} does not fit {budget} bytes at qscale 63"))?;
        let mut video_sectors = sector::video_sectors(
            options.channel,
            number,
            options.size,
            &encoded.bytes,
            chunks,
        )
        .into_iter();
        for s in first..end {
            let audio_here = pending_audio.filter(|_| is_audio(s, true));
            match audio_here {
                Some(data) => {
                    out.extend_from_slice(&sector::audio_sector(options.channel, coding, data));
                    report.audio_sectors.0 += 1;
                    pending_audio = next_audio.next();
                }
                None => {
                    let record = video_sectors.next().expect("one video sector per slot");
                    out.extend_from_slice(&record);
                    report.video_sectors += 1;
                }
            }
        }
        report.frames += 1;
        report.qscales.push(encoded.qscale);
        report.max_chunks = report.max_chunks.max(chunks);
        report.rle_words = report.rle_words.max(encoded.rle_words);
        report.bytes.0 += encoded.bytes.len();
        report.bytes.1 += budget;
        k += 1;
    }
    if report.frames == 0 {
        return Err("the video has no frames".into());
    }
    report.sectors = out.len() / RECORD_BYTES;
    Ok((out, report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inspect;
    use crate::y4m::{header_line, Range};

    /// A small synthetic clip: moving bars over a gradient, `frames` frames
    /// of `w` x `h` at 15 frames a second, full range.
    fn clip(w: usize, h: usize, frames: usize) -> Vec<u8> {
        let mut data = header_line(w, h, (15, 1), Range::Full).into_bytes();
        for f in 0..frames {
            data.extend_from_slice(b"FRAME\n");
            for y in 0..h {
                for x in 0..w {
                    let bar = if (x + 4 * f) % 32 < 16 { 60 } else { 0 };
                    data.push(((x + y) / 2 % 160 + bar) as u8);
                }
            }
            for plane in [90u8, 170] {
                for i in 0..(w / 2) * (h / 2) {
                    data.push(plane.wrapping_add((i % 17) as u8));
                }
            }
        }
        data
    }

    fn tone(seconds: f64) -> Sound {
        let n = (48_000.0 * seconds) as usize;
        Sound {
            rate: 48_000,
            channels: vec![(0..n)
                .map(|i| 8000.0 * (i as f64 * 440.0 * std::f64::consts::TAU / 48_000.0).sin())
                .collect()],
        }
    }

    #[test]
    fn a_synthetic_clip_round_trips_through_the_sdk_decoder() {
        let data = clip(64, 48, 6);
        let options = Options {
            size: (64, 48),
            ..Options::default()
        };
        let (raw, report) = encode(
            y4m::Reader::new(&data[..]).unwrap(),
            Some(&tone(1.0)),
            &options,
        )
        .unwrap();
        // 6 frames of 10 sectors at double speed and 15 frames a second.
        assert_eq!((report.frames, report.sectors), (6, 60));
        // Sound at sectors 0, 16, 32, 48.
        let layout = inspect::layout(&raw).unwrap();
        assert_eq!(layout.audio, vec![0, 16, 32, 48]);
        assert_eq!(layout.frames.len(), 6);
        assert_eq!(layout.dropped, 0);
        // The report's player figures are what the SDK decoder measures.
        let rle = layout.frames.iter().map(|f| f.rle_words).max().unwrap();
        assert_eq!(report.rle_words, rle);
        assert_eq!((report.size, report.xa), ((64, 48), Some((1, 0))));
        let chunks = layout.frames.iter().map(|f| f.chunks).max().unwrap();
        assert_eq!(report.max_chunks, chunks);
        assert_eq!(
            layout.frames.iter().map(|f| f.number).collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5, 6]
        );
        // Frame k's sectors lie inside its own window.
        for (k, f) in layout.frames.iter().enumerate() {
            assert!(f.sectors.0 >= 10 * k && f.sectors.1 < 10 * (k + 1), "{f:?}");
        }
        // Every frame decodes and looks like its source.
        let mut reader = y4m::Reader::new(&data[..]).unwrap();
        let header = reader.header();
        for f in &layout.frames {
            let source = Planes::from_y4m(&header, &reader.next_frame().unwrap().unwrap());
            let quality = crate::model::psnr(&f.rgb().unwrap(), &source.to_rgb());
            assert!(quality > 35.0, "frame {}: {quality} dB", f.number);
        }
        // The sound decodes to the tone, delayed by nothing.
        let (hz, pcm) = inspect::audio(&raw, 0).unwrap();
        assert_eq!(hz, 37_800);
        assert_eq!(pcm.len(), 4 * 4032);
        let peak = pcm[4032..].iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!((7000..=9000).contains(&peak), "peak {peak}");
    }

    #[test]
    fn the_audio_delay_moves_every_sound_sector() {
        let data = clip(32, 32, 4);
        let options = Options {
            size: (32, 32),
            audio_delay: 5,
            ..Options::default()
        };
        let (raw, report) = encode(
            y4m::Reader::new(&data[..]).unwrap(),
            Some(&tone(1.0)),
            &options,
        )
        .unwrap();
        let layout = inspect::layout(&raw).unwrap();
        assert_eq!(layout.audio, vec![5, 21, 37]);
        // The sound outlasts the movie: the rest is not kept.
        assert_eq!(report.audio_sectors, (3, 10));
        assert_eq!(report.max_chunks, 10);
    }

    #[test]
    fn a_busy_frame_is_held_to_its_window() {
        // Noise needs a coarse scale to fit one sector at single speed and
        // 15 frames a second (5 sectors a frame, minus sound).
        let (w, h) = (128, 96);
        let mut data = header_line(w, h, (15, 1), Range::Full).into_bytes();
        let mut x = 0x1234_5678u32;
        for _ in 0..2 {
            data.extend_from_slice(b"FRAME\n");
            for _ in 0..w * h + 2 * (w / 2) * (h / 2) {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                data.push(x as u8);
            }
        }
        let options = Options {
            size: (128, 96),
            speed: DriveSpeed::Single,
            ..Options::default()
        };
        let (raw, report) = encode(y4m::Reader::new(&data[..]).unwrap(), None, &options).unwrap();
        assert!(
            report.qscales.iter().all(|&q| q > 1),
            "{:?}",
            report.qscales
        );
        let layout = inspect::layout(&raw).unwrap();
        for f in &layout.frames {
            assert!(f.size as usize <= usize::from(f.chunks) * CHUNK_BYTES);
            assert_eq!(f.chunks, 5);
        }
    }

    #[test]
    fn a_lower_frame_rate_takes_every_other_source_frame() {
        let data = clip(32, 32, 6);
        let options = Options {
            size: (32, 32),
            fps: Some((15, 2)),
            ..Options::default()
        };
        let (_, report) = encode(y4m::Reader::new(&data[..]).unwrap(), None, &options).unwrap();
        assert_eq!((report.frames, report.sectors), (3, 60));
    }
}
