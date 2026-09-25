//! `psx-audio-cook` command line; see [`psx_audio_cook::cli`].

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    psx_audio_cook::cli::run(&args)
}
