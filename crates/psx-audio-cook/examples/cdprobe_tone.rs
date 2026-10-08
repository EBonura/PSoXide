//! Writes the CD-DA track `hello-cdstream-probe` plays, as raw 16-bit stereo
//! little-endian 44.1 kHz PCM padded to whole 2352-byte sectors, ready for
//! `mkisopsx --cdda-track`:
//! `cargo run -p psx-audio-cook --example cdprobe_tone -- OUT.pcm [SECONDS]`.
//!
//! A two-octave major scale, 0.4 s a note, repeating every 5.6 s. Rising
//! pitch tells a listener where in the track they are: a stop, a restart
//! from the top and a resume at the saved position all sound different.
//! Synthesised here, nothing sampled.

use std::f64::consts::PI;

const RATE: u32 = 44_100;
const SECTOR_BYTES: usize = 2352;
const NOTE_SECONDS: f64 = 0.4;
/// Semitones above C4: a major scale over two octaves.
const SCALE: [i32; 14] = [0, 2, 4, 5, 7, 9, 11, 12, 14, 16, 17, 19, 21, 23];

fn sample(t: f64) -> f64 {
    let index = (t / NOTE_SECONDS) as usize % SCALE.len();
    let into_note = t % NOTE_SECONDS;
    let freq = 261.63 * 2f64.powf(f64::from(SCALE[index]) / 12.0);
    // 20 ms fades in and out of every note so the steps do not click.
    let envelope = (into_note / 0.02)
        .min((NOTE_SECONDS - into_note) / 0.02)
        .clamp(0.0, 1.0);
    (2.0 * PI * freq * t).sin() * envelope * 0.5
}

fn main() {
    let mut args = std::env::args().skip(1);
    let out = args.next().expect("usage: cdprobe_tone OUT.pcm [SECONDS]");
    let seconds: f64 = args
        .next()
        .map_or(30.0, |s| s.parse().expect("SECONDS is a number"));
    let frames = (f64::from(RATE) * seconds) as usize;
    let mut bytes = Vec::with_capacity(frames * 4);
    for i in 0..frames {
        let value = (sample(i as f64 / f64::from(RATE)) * 32767.0).round() as i16;
        bytes.extend_from_slice(&value.to_le_bytes());
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.resize(bytes.len().div_ceil(SECTOR_BYTES) * SECTOR_BYTES, 0);
    std::fs::write(&out, &bytes).expect("write the track");
    println!("{out}: {} sectors", bytes.len() / SECTOR_BYTES);
}
