//! `psx-audio-cook`: command-line front end for cookers that are not Rust
//! (hk-psx, voxide and pico8-psx cook in Python).
//!
//! ```text
//! psx-audio-cook encode IN.wav OUT --rate HZ [--format psau|raw]
//!                [--loop none|whole|source] [--peak F|--no-normalize]
//!                [--no-gauss-comp] [--greedy]
//! psx-audio-cook rates IN.wav            predicted loss per candidate rate
//! psx-audio-cook score SRC.wav ADPCM [--rate HZ] [--skip N]
//!                [--play OUT.wav] [--original OUT.wav]
//! ```
//!
//! `raw` writes bare ADPCM blocks (flags set); `psau` wraps them in the PSAU
//! container. One line of JSON describing the result goes to stdout.
//!
//! `score` measures any ADPCM (raw blocks, or a PSAU whose header supplies
//! the rate) against the source it was cooked from, as the SPU plays it:
//! fwSNRseg and SI-SNR in dB, higher is better. `--skip` drops leading
//! samples (at the playback rate) before comparing, `--play` writes the
//! playback and `--original` the band-limited source, both at 44.1 kHz.

use psx_audio_cook::{adpcm, cook, psau, rate, score, wav, CookOptions, Looping};
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!(
        "usage:\n  psx-audio-cook encode IN.wav OUT --rate HZ [--format psau|raw] [--loop none|whole|source] [--peak F|--no-normalize] [--no-gauss-comp] [--greedy]\n  psx-audio-cook rates IN.wav\n  psx-audio-cook score SRC.wav ADPCM [--rate HZ] [--skip N] [--play OUT.wav] [--original OUT.wav]"
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("encode") if args.len() >= 3 => encode(&args[1], &args[2], &args[3..]),
        Some("rates") if args.len() == 2 => rates(&args[1]),
        Some("score") if args.len() >= 3 => score_cmd(&args[1], &args[2], &args[3..]),
        _ => usage(),
    }
}

fn score_cmd(source: &str, input: &str, flags: &[String]) -> ExitCode {
    let src = match load(source) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let bytes = match std::fs::read(input) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{input}: {e}");
            return ExitCode::FAILURE;
        }
    };
    // A PSAU carries its rate at byte 16 and its blocks after 32 bytes.
    let (mut rate, blocks) = if bytes.len() >= 32 && &bytes[..4] == b"PSAU" {
        (
            u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]),
            &bytes[32..],
        )
    } else {
        (0, &bytes[..])
    };
    let (mut skip, mut play_out, mut original_out) = (0usize, None, None);
    let mut i = 0;
    while i < flags.len() {
        let value = flags.get(i + 1).cloned();
        match (flags[i].as_str(), value) {
            ("--rate", Some(v)) => rate = v.parse().unwrap_or(0),
            ("--skip", Some(v)) => skip = v.parse().unwrap_or(0),
            ("--play", Some(v)) => play_out = Some(v),
            ("--original", Some(v)) => original_out = Some(v),
            _ => return usage(),
        }
        i += 2;
    }
    if rate == 0 || blocks.len() < adpcm::BLOCK_BYTES {
        return usage();
    }
    let s = score(&src, blocks, rate, skip);
    let written = |path: &Option<String>, samples: &[i16]| -> bool {
        match path {
            Some(p) => match std::fs::write(p, wav::write_mono16(44_100, samples)) {
                Ok(()) => true,
                Err(e) => {
                    eprintln!("{p}: {e}");
                    false
                }
            },
            None => true,
        }
    };
    let original = psx_audio_cook::resample::to_i16(&psx_audio_cook::reference_44k(&src));
    if !written(&play_out, &s.played) || !written(&original_out, &original) {
        return ExitCode::FAILURE;
    }
    println!(
        "{{\"fwsnrseg\":{:.2},\"si_snr\":{:.2},\"rate\":{},\"adpcm_bytes\":{},\"seconds\":{:.3}}}",
        s.fw_snr_seg_db,
        s.si_snr_db,
        rate,
        blocks.len() / adpcm::BLOCK_BYTES * adpcm::BLOCK_BYTES,
        src.samples.len() as f64 / src.rate as f64
    );
    ExitCode::SUCCESS
}

fn load(path: &str) -> Result<wav::Wav, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    wav::read(&bytes).map_err(|e| format!("{path}: {e}"))
}

fn encode(input: &str, output: &str, flags: &[String]) -> ExitCode {
    let source = match load(input) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let mut opts = CookOptions::one_shot(0);
    let mut raw = false;
    let mut i = 0;
    while i < flags.len() {
        let value = flags.get(i + 1).map(String::as_str);
        match (flags[i].as_str(), value) {
            ("--rate", Some(v)) => {
                opts.rate = v.parse().unwrap_or(0);
                i += 1;
            }
            ("--format", Some(v)) => {
                raw = v == "raw";
                i += 1;
            }
            ("--loop", Some(v)) => {
                opts.looping = match v {
                    "whole" => Looping::Whole,
                    "source" => Looping::Source,
                    _ => Looping::None,
                };
                i += 1;
            }
            ("--peak", Some(v)) => {
                opts.normalize_peak = v.parse().ok();
                i += 1;
            }
            ("--no-normalize", _) => opts.normalize_peak = None,
            ("--no-gauss-comp", _) => opts.compensate_gauss = false,
            ("--greedy", _) => opts.encode.effort = adpcm::Effort::Greedy,
            _ => return usage(),
        }
        i += 1;
    }
    if opts.rate == 0 {
        return usage();
    }
    let cooked = cook(&source, &opts);
    let bytes = if raw {
        cooked.adpcm.clone()
    } else {
        psau(cooked.rate, cooked.pcm.len(), &cooked.adpcm)
    };
    if let Err(e) = std::fs::write(output, &bytes) {
        eprintln!("{output}: {e}");
        return ExitCode::FAILURE;
    }
    let loop_block = cooked
        .loop_block
        .map(|b| b.to_string())
        .unwrap_or_else(|| "null".into());
    println!(
        "{{\"rate\":{},\"samples\":{},\"adpcm_bytes\":{},\"file_bytes\":{},\"loop_block\":{}}}",
        cooked.rate,
        cooked.pcm.len(),
        cooked.adpcm.len(),
        bytes.len(),
        loop_block
    );
    ExitCode::SUCCESS
}

fn rates(input: &str) -> ExitCode {
    let source = match load(input) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let ladder: Vec<u32> = rate::LADDER
        .iter()
        .copied()
        .filter(|&r| r <= source.rate)
        .collect();
    let loss = rate::band_loss(&source.samples, source.rate, &ladder);
    for (r, l) in ladder.iter().zip(loss) {
        let bytes = psx_audio_cook::adpcm_bytes(psx_audio_cook::resampled_len(
            source.samples.len(),
            source.rate,
            *r,
        ));
        println!("{r}\t{bytes}\t{l:.2}");
    }
    ExitCode::SUCCESS
}
