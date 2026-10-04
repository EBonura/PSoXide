//! Writes the four generated test songs `hello-xa` plays as 37.8 kHz stereo
//! WAV files: `cargo run -p psx-audio-cook --example xa_demo_songs -- OUT_DIR [SECONDS]`.
//! Plain synthesised tones, nothing sampled.

use psx_audio_cook::wav;
use std::f64::consts::PI;

const RATE: u32 = 37_800;
const DEFAULT_SECONDS: f64 = 4.0;

/// Fade in and out over 50 ms so a loop restart does not click.
fn fade(t: f64) -> f64 {
    (t / 0.05)
        .min(1.0)
        .min((SECONDS - t) / 0.05)
        .clamp(0.0, 1.0)
}

fn tone(freq: f64, t: f64) -> f64 {
    (2.0 * PI * freq * t).sin()
}

/// `(left, right)` at time `t` for song `index`.
fn sample(index: usize, t: f64, seconds: f64) -> (f64, f64) {
    let swell = 0.6 + 0.4 * (2.0 * PI * 0.5 * t).sin();
    let (l, r) = match index {
        0 => {
            let chord = tone(220.0, t) + tone(277.18, t) + tone(329.63, t);
            (chord * swell * 0.30, chord * swell * 0.26)
        }
        1 => {
            let vibrato = 1.0 + 0.004 * (2.0 * PI * 5.0 * t).sin();
            let chord =
                tone(392.0 * vibrato, t) + tone(493.88 * vibrato, t) + tone(587.33 * vibrato, t);
            (chord * 0.26, chord * 0.30)
        }
        2 => {
            let gate = if (t % 0.5) < 0.1 { 1.0 } else { 0.0 };
            let blip = gate * tone(880.0, t);
            (blip * 0.7, blip * 0.7)
        }
        _ => {
            // A glide from 1000 to 1800 Hz every second; 1400 whole cycles
            // pass per second, so the phase stays continuous at the restart.
            let tau = t % 1.0;
            let phase = 2.0 * PI * (1400.0 * t.floor() + 1000.0 * tau + 400.0 * tau * tau);
            let w = phase.sin();
            (w * 0.6, w * 0.6)
        }
    };
    let f = fade(t, seconds);
    (l * f * 20_000.0, r * f * 20_000.0)
}

fn main() {
    let dir = std::env::args()
        .nth(1)
        .expect("usage: xa_demo_songs OUT_DIR");
    std::fs::create_dir_all(&dir).expect("create output directory");
    for (index, name) in ["song0_pad", "song1_high", "song2_blips", "song3_whistle"]
        .iter()
        .enumerate()
    {
        let frames = (RATE as f64 * seconds) as usize;
        let (mut left, mut right) = (Vec::with_capacity(frames), Vec::with_capacity(frames));
        for i in 0..frames {
            let (l, r) = sample(index, i as f64 / RATE as f64, seconds);
            left.push(l.round() as i16);
            right.push(r.round() as i16);
        }
        let path = format!("{dir}/{name}.wav");
        std::fs::write(&path, wav::write_pcm16(RATE, &[&left, &right])).expect("write WAV");
        println!("{path}");
    }
}
