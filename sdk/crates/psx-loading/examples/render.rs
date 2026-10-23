//! Renders the corner widget on the host, the way the GPU would draw its packets, so the
//! look can be judged (and measured) without an emulator.
//!
//! ```text
//! cargo run -p psx-loading --example render -- OUT.ppm [--mode 320x240] [--size 24]
//!     [--margin 12] [--scale 1] [--ring 2,4] [--blend opaque|average|additive|glow]
//!     [--tick 0] [--bg FRAME.ppm] [--legacy]
//! ```
//!
//! `OUT.ppm` is the framebuffer of the mode (P6, one pixel per framebuffer pixel; stretch it to
//! 4:3 for display). The report on stdout gives the box and the ring in pixels and in
//! display-corrected pixels, which is what a 4:3 television shows. `--legacy` draws the 2 x 3
//! dot spinner the widget used before the ring, for before and after pictures.

use std::fs;

use psx_font::fonts::BASIC_8X16;
use psx_loading::{Blend, Indicator, Rgb, Sink, Style, Tail};

struct Frame {
    width: usize,
    height: usize,
    pixels: Vec<[u8; 3]>,
    /// Semi-transparency mode field of the last GP0(E1h).
    mode: u32,
}

impl Sink for Frame {
    fn packet(&mut self, words: &[u32]) {
        match words[0] >> 24 {
            0xe1 => self.mode = words[0] >> 5 & 3,
            0x60 | 0x62 => {
                let blended = words[0] >> 24 == 0x62;
                let colour = [
                    words[0] as u8,
                    (words[0] >> 8) as u8,
                    (words[0] >> 16) as u8,
                ];
                let (x, y) = (words[1] as i16 as i32, (words[1] >> 16) as i16 as i32);
                let (w, h) = ((words[2] & 0xffff) as i32, (words[2] >> 16) as i32);
                for py in y.max(0)..(y + h).min(self.height as i32) {
                    for px in x.max(0)..(x + w).min(self.width as i32) {
                        let at = py as usize * self.width + px as usize;
                        self.pixels[at] = if blended {
                            blend(self.pixels[at], colour, self.mode)
                        } else {
                            colour
                        };
                    }
                }
            }
            _ => {}
        }
    }
}

/// The GPU's four semi-transparency modes for one pixel, per channel, clamped.
fn blend(back: [u8; 3], front: [u8; 3], mode: u32) -> [u8; 3] {
    let mut out = [0u8; 3];
    for c in 0..3 {
        let (b, f) = (back[c] as i32, front[c] as i32);
        out[c] = match mode {
            0 => (b + f) / 2,
            1 => (b + f).min(255),
            2 => (b - f).max(0),
            _ => (b + f / 4).min(255),
        } as u8;
    }
    out
}

fn read_ppm(path: &str) -> (usize, usize, Vec<[u8; 3]>) {
    let bytes = fs::read(path).expect("read the background");
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
    let pixels = bytes[at..at + w * h * 3]
        .chunks(3)
        .map(|p| [p[0], p[1], p[2]])
        .collect();
    (w, h, pixels)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let out = args.next().expect("usage: render OUT.ppm [options]");
    let (mut mode, mut size, mut margin, mut scale) = ((320usize, 240usize), 24i16, 12i16, 1u8);
    let (mut ring, mut blend, mut tick, mut bg, mut legacy) =
        ((2i16, 4i16), Blend::Additive, 0u32, None::<String>, false);
    while let Some(flag) = args.next() {
        let mut value = || args.next().expect("a value after the flag");
        match flag.as_str() {
            "--mode" => {
                let v = value();
                let (w, h) = v.split_once('x').expect("WxH");
                mode = (w.parse().unwrap(), h.parse().unwrap());
            }
            "--size" => size = value().parse().unwrap(),
            "--margin" => margin = value().parse().unwrap(),
            "--scale" => scale = value().parse().unwrap(),
            "--ring" => {
                let v = value();
                let (d, p) = v.split_once(',').expect("DOT,PITCH");
                ring = (d.parse().unwrap(), p.parse().unwrap());
            }
            "--blend" => {
                blend = match value().as_str() {
                    "opaque" => Blend::Opaque,
                    "average" => Blend::Average,
                    "additive" => Blend::Additive,
                    "glow" => Blend::Glow,
                    other => panic!("unknown blend {other}"),
                }
            }
            "--tick" => tick = value().parse().unwrap(),
            "--bg" => bg = Some(value()),
            "--legacy" => legacy = true,
            other => panic!("unknown flag {other}"),
        }
    }
    let lit = Rgb(255, 214, 96);
    let unlit = Rgb(40, 34, 20);
    let style = Style::new(&BASIC_8X16, "", Rgb(255, 170, 0), Rgb(0, 0, 0))
        .with_tail(Tail::Spinner { lit, unlit })
        .scaled(scale)
        .in_corner(size, mode.0 as i16, mode.1 as i16, margin)
        .with_ring(ring.0, ring.1)
        .with_blend(blend)
        .timed(2, 64);

    let (width, height, pixels) = match &bg {
        Some(path) => read_ppm(path),
        None => (mode.0, mode.1, vec![[0, 0, 0]; mode.0 * mode.1]),
    };
    let mut frame = Frame {
        width,
        height,
        pixels,
        mode: 0,
    };
    let indicator = Indicator::new(style);
    if legacy {
        legacy_frame(&mut frame, &style, (tick / 2 % 10) as u8);
    } else {
        indicator.draw(&mut frame, style.look(tick));
    }

    let (bx, by, bw, bh) = style.bounds();
    let (rw, rh) = style.ring_size();
    // Pixel aspect on a 4:3 set: width of a pixel over its height.
    let par = 4.0 * mode.1 as f64 / (3.0 * mode.0 as f64);
    println!("mode {}x{} pixel aspect {par:.3}", mode.0, mode.1);
    println!(
        "box   {bw}x{bh} px at ({bx},{by}); on screen {:.1}x{:.1} height-pixels",
        bw as f64 * par,
        bh as f64
    );
    println!(
        "ring  {rw}x{rh} px; on screen {:.1}x{:.1} height-pixels",
        rw as f64 * par,
        rh as f64
    );
    let mut ppm = format!("P6\n{} {}\n255\n", frame.width, frame.height).into_bytes();
    for p in &frame.pixels {
        ppm.extend_from_slice(p);
    }
    fs::write(out, ppm).expect("write the frame");
}

/// The widget as it was: a 2 x 3 dot spinner (braille "dots") in a square box, for before pictures.
fn legacy_frame(frame: &mut Frame, style: &Style, step: u8) {
    const FRAMES: [u8; 10] = [0x0b, 0x19, 0x39, 0x38, 0x3c, 0x34, 0x26, 0x27, 0x07, 0x0f];
    let (x, y, w, h) = style.bounds();
    psx_loading::rect(frame, x, y, w, h, Rgb(0, 0, 0));
    let scale = style.scale as i16;
    let (gw, gh) = (8 * scale, 13 * scale);
    let (x0, y0) = (x + (w - gw) / 2, y + (h - gh) / 2);
    for dot in 0..6i16 {
        let on = FRAMES[step as usize % 10] & (1 << dot) != 0;
        let colour = if on {
            Rgb(255, 214, 96)
        } else {
            Rgb(40, 34, 20)
        };
        psx_loading::rect(
            frame,
            x0 + dot / 3 * 5 * scale,
            y0 + dot % 3 * 5 * scale,
            3 * scale,
            3 * scale,
            colour,
        );
    }
}
