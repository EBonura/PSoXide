//! `fmv-test-movie`: build the synthetic STR movie the hello-fmv console
//! test streams.
//!
//! Everything is generated: an FFmpeg test pattern with temporal noise on
//! top (so every frame fills the whole 2x sector budget) and a stereo beep
//! track (left 440 Hz, right 660 Hz, 100 ms at the start of every second, in
//! step with the pattern's seconds counter). psxavenc encodes it as a
//! 320x240, 15 fps, BS v2 STR with interleaved 37.8 kHz stereo XA-ADPCM,
//! 2336-byte sectors, XA file 1 / channel 0.
//!
//! Then every video sector's STR header gets test fields in bytes 20..32 of
//! its 2048-byte payload (over the BS header copy, which players do not
//! need):
//!
//! ```text
//! 20..22  video sector ordinal (0-based, audio sectors skipped)
//! 22..24  total video sectors in the file
//! 24..28  sector index within the file (audio sectors counted)
//! 28..32  checksum of payload bytes 32..2048
//! ```
//!
//! The checksum is `h = rotl(h, 5) + w` over the 504 little-endian words,
//! seeded with `0x9E3779B9 ^ ordinal`; hello-fmv recomputes it for every
//! sector it reads, so a corrupt, shifted or misplaced PIO read shows up as
//! BAD and a skipped one as LOST.
//!
//! ```text
//! fmv-test-movie --psxavenc PATH --out MOVIE.STR [--seconds 75] [--noise 40]
//! ```
//!
//! Writes MOVIE.STR (2336-byte sectors, for `mkisopsx --xa-file`) and
//! MOVIE.STR.json with the counts.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::obj;
use crate::pyjson::dumps;

const XA_SECTOR: usize = 2336;
const SEED: u32 = 0x9E37_79B9;

/// The per-sector checksum hello-fmv recomputes.
pub fn checksum(payload: &[u8], ordinal: u32) -> u32 {
    payload[32..2048]
        .chunks_exact(4)
        .fold(SEED ^ ordinal, |h, w| {
            h.rotate_left(5)
                .wrapping_add(u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
        })
}

/// Stamp the test fields into every video sector and count what is there:
/// `(sectors, video sectors, audio sectors, distinct frames)`.
pub fn stamp(data: &mut [u8]) -> Result<(usize, usize, usize, usize), String> {
    if !data.len().is_multiple_of(XA_SECTOR) {
        return Err("psxavenc -t str writes 2336-byte sectors".into());
    }
    let sectors = data.len() / XA_SECTOR;
    let sector = |data: &[u8], i: usize| data[i * XA_SECTOR..(i + 1) * XA_SECTOR].to_vec();
    let video: Vec<usize> = (0..sectors)
        .filter(|&i| {
            let s = sector(data, i);
            s[2] & 0x04 == 0 && s[8..12] == [0x60, 0x01, 0x01, 0x80]
        })
        .collect();
    let audio = (0..sectors)
        .filter(|&i| data[i * XA_SECTOR + 2] & 0x04 != 0)
        .count();
    let mut frames = BTreeSet::new();
    for (ordinal, &i) in video.iter().enumerate() {
        let base = i * XA_SECTOR + 8;
        let payload = &mut data[base..base + 2048];
        frames.insert(u32::from_le_bytes(
            payload[8..12].try_into().expect("4 bytes"),
        ));
        payload[20..22].copy_from_slice(&(ordinal as u16).to_le_bytes());
        payload[22..24].copy_from_slice(&(video.len() as u16).to_le_bytes());
        payload[24..28].copy_from_slice(&(i as u32).to_le_bytes());
        let sum = checksum(payload, ordinal as u32);
        payload[28..32].copy_from_slice(&sum.to_le_bytes());
    }
    Ok((sectors, video.len(), audio, frames.len()))
}

fn run(command: &mut Command) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|e| format!("{:?}: {e}", command.get_program()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{:?} failed: {status}", command.get_program()))
    }
}

fn movie(psxavenc: &str, out: &Path, seconds: u32, noise: u32) -> Result<(), String> {
    let parent = match out.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let tmp = parent.join(format!(".fmv-test-movie-{}", std::process::id()));
    fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let encoded = (|| {
        let src = tmp.join("src.mkv");
        let raw = tmp.join("raw.str");
        let beep = |f: u32| format!("0.5*sin(2*PI*{f}*t)*lt(mod(t\\,1)\\,0.1)");
        run(Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!(
                "testsrc=size=320x240:rate=15:duration={seconds},noise=alls={noise}:allf=t+u"
            ))
            .args(["-f", "lavfi", "-i"])
            .arg(format!(
                "aevalsrc={}|{}:s=37800:d={seconds}",
                beep(440),
                beep(660)
            ))
            .args(["-c:v", "ffv1", "-c:a", "pcm_s16le", "-shortest"])
            .arg(&src))?;
        run(Command::new(psxavenc)
            .args([
                "-q", "-t", "str", "-v", "v2", "-s", "320x240", "-r", "15", "-x", "2", "-f",
                "37800", "-c", "2", "-b", "4", "-F", "1", "-C", "0",
            ])
            .arg(&src)
            .arg(&raw))?;
        fs::read(&raw).map_err(|e| e.to_string())
    })();
    let _ = fs::remove_dir_all(&tmp);
    let mut data = encoded?;
    let (sectors, video, audio, frames) = stamp(&mut data)?;
    fs::write(out, &data).map_err(|e| e.to_string())?;
    let info = obj! {
        "sectors" => sectors,
        "video_sectors" => video,
        "audio_sectors" => audio,
        "frames" => frames,
        "seconds" => i64::from(seconds),
    };
    let mut record = out.as_os_str().to_owned();
    record.push(".json");
    fs::write(&record, dumps(&info, Some(2), true) + "\n").map_err(|e| e.to_string())?;
    println!("{}", dumps(&info, None, true));
    Ok(())
}

/// Parse the arguments and build the movie.
pub fn main(args: &[String]) -> Result<(), String> {
    let (mut psxavenc, mut out, mut seconds, mut noise) = (None, None, 75, 40);
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .cloned()
                .ok_or_else(|| format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--psxavenc" => psxavenc = Some(value()?),
            "--out" => out = Some(value()?),
            "--seconds" => seconds = value()?.parse().map_err(|_| "--seconds wants an integer")?,
            "--noise" => noise = value()?.parse().map_err(|_| "--noise wants an integer")?,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let usage = "usage: fmv-test-movie --psxavenc PATH --out MOVIE.STR [--seconds 75] [--noise 40]";
    movie(
        &psxavenc.ok_or(usage)?,
        Path::new(&out.ok_or(usage)?),
        seconds,
        noise,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_video_sectors_and_skips_audio() {
        let mut data = vec![0u8; XA_SECTOR * 3];
        for i in [0, 2] {
            data[i * XA_SECTOR + 8..i * XA_SECTOR + 12].copy_from_slice(&[0x60, 1, 1, 0x80]);
            data[i * XA_SECTOR + 16] = i as u8; // frame number
        }
        data[XA_SECTOR + 2] = 0x04; // audio
        assert_eq!(stamp(&mut data).unwrap(), (3, 2, 1, 2));
        let second = &data[2 * XA_SECTOR + 8..2 * XA_SECTOR + 8 + 2048];
        assert_eq!(&second[20..28], &[1, 0, 2, 0, 2, 0, 0, 0]);
        assert_eq!(
            u32::from_le_bytes(second[28..32].try_into().unwrap()),
            checksum(second, 1)
        );
    }
}
