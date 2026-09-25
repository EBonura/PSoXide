//! `psx-audio-cook`: command-line front end for cookers that are not Rust
//! (hk-psx, voxide and pico8-psx cook in Python).
//!
//! ```text
//! psx-audio-cook encode IN.wav OUT --rate HZ [--format psau|raw]
//!                [--loop none|whole|source] [--peak F|--no-normalize]
//!                [--no-gauss-comp] [--greedy]
//! psx-audio-cook rates IN.wav            predicted loss per candidate rate
//! ```
//!
//! `raw` writes bare ADPCM blocks (flags set); `psau` wraps them in the PSAU
//! container. One line of JSON describing the result goes to stdout.

use psx_audio_cook::{adpcm, cook, psau, rate, wav, CookOptions, Looping};
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!(
        "usage:\n  psx-audio-cook encode IN.wav OUT --rate HZ [--format psau|raw] [--loop none|whole|source] [--peak F|--no-normalize] [--no-gauss-comp] [--greedy]\n  psx-audio-cook rates IN.wav"
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("encode") if args.len() >= 3 => encode(&args[1], &args[2], &args[3..]),
        Some("rates") if args.len() == 2 => rates(&args[1]),
        _ => usage(),
    }
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
