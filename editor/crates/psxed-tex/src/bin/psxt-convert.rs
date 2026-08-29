//! Headless PNG/image -> `.psxt` converter for scripted asset work
//! (the editor GUI import uses the same `psxed_tex::convert` path).
//!
//! Usage:
//!   psxt-convert <src> <out.psxt> <width> <height> [depth] [--transparent-zero]
//!
//! `depth` is 4, 8, or 15 (default 4). `--transparent-zero` reserves
//! indexed palette entry 0 for alpha-transparent pixels (UI sprites).

use psxed_tex::{convert, Config, CropMode, PsxtDepth as Depth, Resampler};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!(
            "usage: {} <src> <out.psxt> <width> <height> [depth 4|8|15] [--transparent-zero]",
            args[0]
        );
        std::process::exit(2);
    }
    let src = &args[1];
    let out = &args[2];
    let width: u16 = args[3].parse().expect("width");
    let height: u16 = args[4].parse().expect("height");
    let mut depth = Depth::Bit4;
    let mut transparent_index_zero = false;
    for arg in &args[5..] {
        match arg.as_str() {
            "4" => depth = Depth::Bit4,
            "8" => depth = Depth::Bit8,
            "15" => depth = Depth::Bit15,
            "--transparent-zero" => transparent_index_zero = true,
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    let bytes = std::fs::read(src).expect("read source image");
    let cfg = Config {
        width,
        height,
        depth,
        crop: CropMode::None,
        resampler: Resampler::Lanczos3,
        transparent_index_zero,
        clut_rows: 1,
    };
    let texture = convert(&bytes, &cfg).expect("convert");
    std::fs::write(out, &texture).expect("write psxt");
    println!("wrote {out} ({} bytes, {width}x{height})", texture.len());
}
