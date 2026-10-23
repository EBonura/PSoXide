//! hudview: paints the sprite HUD on the host with the same composition code the guest uses, for
//! quick looks and for comparing with an emulator frame.
//!
//! hudview grid <out.ppm> [bg=r,g,b] [x0=n] [x1=n]
//!     the two-stance, four-level grid of the `hud-grid` frame; x0 and x1 are the left edges of the columns
//! hudview one <out.ppm> [horizon|zenith] [health] [back] [energy] [bg=r,g,b]

#![allow(dead_code)]

#[path = "../../../../editor-playtest/src/stance_hud_sprites.rs"]
mod hud;

use hud::{compose, HudState, Op, Stance};

const BG: [u8; 3] = [16, 24, 40];

/// A background given as `bg=r,g,b` on the command line.
/// A number given as `key=n` on the command line.
fn arg_num(args: &[String], key: &str, default: i32) -> i32 {
    args.iter()
        .find_map(|a| a.strip_prefix(key)?.strip_prefix('='))
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn background(args: &[String]) -> [u8; 3] {
    args.iter()
        .find_map(|a| a.strip_prefix("bg="))
        .and_then(|v| {
            let n: Vec<u8> = v.split(',').filter_map(|s| s.parse().ok()).collect();
            (n.len() == 3).then(|| [n[0], n[1], n[2]])
        })
        .unwrap_or(BG)
}

struct Frame {
    w: usize,
    h: usize,
    px: Vec<[u8; 3]>,
}

/// The 5-bit colour as the display shows it.
fn expand(c5: u16) -> u8 {
    let v = (c5 & 31) as u8;
    (v << 3) | (v >> 2)
}

fn shown(c: [u8; 3]) -> [u8; 3] {
    c.map(|v| expand((v >> 3) as u16))
}

impl Frame {
    fn new(w: usize, h: usize, bg: [u8; 3]) -> Self {
        Self {
            w,
            h,
            px: vec![shown(bg); w * h],
        }
    }

    fn put(&mut self, x: i32, y: i32, c: [u8; 3]) {
        if x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h {
            self.px[y as usize * self.w + x as usize] = c;
        }
    }

    /// Average the scene with a 5-bit colour, the GPU's semi-transparency mode 0.
    fn blend(&mut self, x: i32, y: i32, f5: [u16; 3]) {
        if x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h {
            let p = &mut self.px[y as usize * self.w + x as usize];
            for (c, f) in p.iter_mut().zip(f5) {
                *c = expand(((*c as u16 >> 3) + f) >> 1);
            }
        }
    }

    fn op(&mut self, op: Op) {
        match op {
            Op::Rect {
                x,
                y,
                w,
                h,
                rgb,
                glass,
            } => {
                for yy in y..y + h {
                    for xx in x..x + w {
                        if glass {
                            self.blend(xx, yy, rgb.map(|v| (v >> 3) as u16));
                        } else {
                            self.put(xx, yy, shown(rgb));
                        }
                    }
                }
            }
            Op::Sprite {
                x,
                y,
                src,
                clut,
                glass,
            } => {
                for ty in 0..src.h as usize {
                    for tx in 0..src.w as usize {
                        let (u, v) = (src.u as usize + tx, src.v as usize + ty);
                        let hw = hud::ATLAS[v * hud::ATLAS_HW_W + u / 4];
                        let idx = ((hw >> ((u % 4) * 4)) & 15) as usize;
                        let entry = hud::CLUTS[clut * 16 + idx];
                        if entry == 0 {
                            continue;
                        }
                        let c5 = [entry & 31, (entry >> 5) & 31, (entry >> 10) & 31];
                        if glass && entry & 0x8000 != 0 {
                            self.blend(x + tx as i32, y + ty as i32, c5);
                        } else {
                            self.put(x + tx as i32, y + ty as i32, c5.map(expand));
                        }
                    }
                }
            }
        }
    }

    fn write(&self, path: &str) {
        let mut out = format!("P6\n{} {}\n255\n", self.w, self.h).into_bytes();
        for p in &self.px {
            out.extend_from_slice(p);
        }
        std::fs::write(path, out).expect("write ppm");
    }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (Some(mode), Some(path)) = (a.first(), a.get(1)) else {
        eprintln!("usage: hudview grid|one <out.ppm> [stance health back energy]");
        std::process::exit(2);
    };
    match mode.as_str() {
        "grid" => {
            let mut f = Frame::new(320, 240, background(&a));
            // Left edge of the Horizon and Zenith columns (a column can be put off screen).
            let cols = [arg_num(&a, "x0", 0), arg_num(&a, "x1", hud::CANVAS_W)];
            for (col, stance) in [Stance::Horizon, Stance::Zenith].into_iter().enumerate() {
                for (row, level) in [10u8, 6, 3, 0].into_iter().enumerate() {
                    let state = HudState {
                        stance,
                        health: level,
                        back_health: 4,
                        energy: level,
                    };
                    compose((cols[col], 8 + 40 * row as i32), &state, &mut |op| f.op(op));
                }
            }
            f.write(path);
        }
        "one" => {
            let num = |i: usize, d: u8| a.get(i).and_then(|s| s.parse().ok()).unwrap_or(d);
            let stance = if a.get(2).map(String::as_str) == Some("zenith") {
                Stance::Zenith
            } else {
                Stance::Horizon
            };
            let state = HudState {
                stance,
                health: num(3, 7),
                back_health: num(4, 4),
                energy: num(5, 6),
            };
            let mut f = Frame::new(160, 40, background(&a));
            compose((0, 4), &state, &mut |op| f.op(op));
            f.write(path);
        }
        _ => std::process::exit(2),
    }
}
#[cfg(test)]
mod tests {
    use super::hud::{
        compose, HudState, Op, Stance, ATLAS_H, ATLAS_HW_W, CANVAS_H, CANVAS_W, MAX_OPS,
    };

    /// Every state fits the buffers, and paints inside the canvas and the atlas.
    #[test]
    fn every_state_stays_inside_its_bounds() {
        for stance in [Stance::Horizon, Stance::Zenith] {
            for health in 0..=10u8 {
                for back_health in [0u8, 4, 10] {
                    for energy in [0u8, 3, 10] {
                        let state = HudState {
                            stance,
                            health,
                            back_health,
                            energy,
                        };
                        let mut n = 0;
                        compose((0, 0), &state, &mut |op| {
                            n += 1;
                            match op {
                                Op::Rect { x, y, w, h, .. } => {
                                    assert!(x >= 0 && y >= 0 && w > 0 && h > 0);
                                    assert!(x + w <= CANVAS_W && y + h <= CANVAS_H);
                                }
                                Op::Sprite { x, y, src, .. } => {
                                    assert!(x >= 0 && y >= 0);
                                    assert!(x + src.w as i32 <= CANVAS_W);
                                    assert!(y + src.h as i32 <= CANVAS_H);
                                    assert!(src.u as usize + src.w as usize <= ATLAS_HW_W * 4);
                                    assert!(src.v as usize + src.h as usize <= ATLAS_H);
                                }
                            }
                        });
                        assert!(n <= MAX_OPS, "{n} ops, room for {MAX_OPS}");
                    }
                }
            }
        }
    }
}
