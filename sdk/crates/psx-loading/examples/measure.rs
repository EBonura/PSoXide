//! Measures the widget in a captured frame: the bounding box of everything lit in the
//! bottom-right corner, in framebuffer pixels and in display-corrected pixels (what a 4:3
//! television shows), for a capture on a black ground.
//!
//! ```text
//! cargo run -p psx-loading --example measure -- FRAME.ppm [--mode 320x240] [--region 64] [--floor 12]
//! ```
//!
//! A pixel counts when its brightest channel is above `--floor`; the search covers the
//! `--region` pixels (tall) at the corner, widened by the pixel aspect. Prints
//! `ring WxH px, on screen W.WxH.H height-pixels, ratio R` and exits non-zero when
//! nothing is lit.

use std::fs;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: measure FRAME.ppm [options]");
    let (mut mode, mut region, mut floor) = ((320usize, 240usize), 64usize, 12u8);
    while let Some(flag) = args.next() {
        let mut value = || args.next().expect("a value after the flag");
        match flag.as_str() {
            "--mode" => {
                let v = value();
                let (w, h) = v.split_once('x').expect("WxH");
                mode = (w.parse().unwrap(), h.parse().unwrap());
            }
            "--region" => region = value().parse().unwrap(),
            "--floor" => floor = value().parse().unwrap(),
            other => panic!("unknown flag {other}"),
        }
    }
    let bytes = fs::read(&path).expect("read the frame");
    let mut at = 0;
    let mut fields = Vec::new();
    while fields.len() < 4 {
        while bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        let start = at;
        while !bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        fields.push(String::from_utf8_lossy(&bytes[start..at]).into_owned());
    }
    at += 1;
    let (w, h): (usize, usize) = (fields[1].parse().unwrap(), fields[2].parse().unwrap());
    let par = 4.0 * mode.1 as f64 / (3.0 * mode.0 as f64);
    let region_w = ((region as f64 / par).round() as usize).min(w);
    let (mut left, mut top, mut right, mut bottom) = (usize::MAX, usize::MAX, 0, 0);
    for y in h.saturating_sub(region)..h {
        for x in w - region_w..w {
            let p = &bytes[at + (y * w + x) * 3..][..3];
            if p.iter().any(|&c| c > floor) {
                left = left.min(x);
                right = right.max(x + 1);
                top = top.min(y);
                bottom = bottom.max(y + 1);
            }
        }
    }
    if right == 0 {
        eprintln!("nothing lit in the corner");
        return ExitCode::FAILURE;
    }
    let (bw, bh) = (right - left, bottom - top);
    let on_screen = bw as f64 * par;
    println!(
        "ring {bw}x{bh} px at ({left},{top}), pixel aspect {par:.3}, on screen {on_screen:.1}x{bh}.0 height-pixels, ratio {:.3}",
        on_screen / bh as f64
    );
    ExitCode::SUCCESS
}
