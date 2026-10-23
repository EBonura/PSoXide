//! hudview: paints the sprite HUD on the host with the same composition code the guest uses, for
//! quick looks and for comparing with an emulator frame.
//!
//! hudview grid <out.ppm> [bg=r,g,b] [x0=n] [x1=n]
//!     the two-stance, four-level grid of the `hud-grid` frame; x0 and x1 are the left edges of the columns
//! hudview swap <out.ppm> [to=zenith|horizon] [bg=r,g,b] [x0=n]
//!     one column of the swap frames 0, 2, 4 and 6 (the `HUD_SWAP` build of the grid frame)
//! hudview bars <out.ppm> [bg=r,g,b]
//!     every bar in every look, alone
//! hudview one <out.ppm> [horizon|zenith] [health] [other health] [energy] [swap] [bg=r,g,b]

#![allow(dead_code)]

#[path = "../../../../editor-playtest/src/stance_hud_sprites.rs"]
mod hud;

use hud::{compose, HudState, Op, Stance};

const BG: [u8; 3] = [16, 24, 40];

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

/// A frame as the GPU keeps it: five bits a channel and the mask bit.
struct Frame {
    w: usize,
    h: usize,
    px: Vec<[u8; 3]>,
    mask: Vec<bool>,
    set_mask: bool,
    check_mask: bool,
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
            mask: vec![false; w * h],
            set_mask: false,
            check_mask: false,
        }
    }

    /// Draw one pixel of 5-bit colour `c5`: opaque, or averaged with the scene when `glass`.
    fn plot(&mut self, x: i32, y: i32, c5: [u16; 3], glass: bool, stp: bool) {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return;
        }
        let i = y as usize * self.w + x as usize;
        if self.check_mask && self.mask[i] {
            return;
        }
        for (c, f) in self.px[i].iter_mut().zip(c5) {
            *c = if glass {
                // The GPU's semi-transparency mode 0: half the scene and half the colour.
                expand(((*c as u16 >> 3) + f) >> 1)
            } else {
                expand(f)
            };
        }
        // A translucent texel keeps its own bit 15 in the frame, which is the mask bit.
        if self.set_mask || stp {
            self.mask[i] = true;
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
                let c5 = rgb.map(|v| (v >> 3) as u16);
                for yy in y..y + h {
                    for xx in x..x + w {
                        self.plot(xx, yy, c5, glass, false);
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
                        let translucent = glass && entry & 0x8000 != 0;
                        self.plot(x + tx as i32, y + ty as i32, c5, translucent, translucent);
                    }
                }
            }
            Op::Mask { set, check } => {
                self.set_mask = set;
                self.check_mask = check;
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

/// The state of the HUD at `level` of 10 in the `hud-grid` frame: the active stance at `level`,
/// the other one at 4, the gems at `level`.
fn level_state(stance: Stance, level: u8) -> HudState {
    let mut health = [4, 4];
    health[stance.index()] = level;
    HudState {
        stance,
        health,
        energy: [level, level],
        swap: 0,
    }
}

/// The state of the `HUD_SWAP` frame: a swap to `stance` at frame `swap`.
fn swap_state(stance: Stance, swap: u8) -> HudState {
    HudState {
        stance,
        health: [7, 5],
        energy: [8, 3],
        swap,
    }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (Some(mode), Some(path)) = (a.first(), a.get(1)) else {
        eprintln!("usage: hudview grid|swap|one <out.ppm> [options]");
        std::process::exit(2);
    };
    match mode.as_str() {
        "grid" => {
            let mut f = Frame::new(320, 240, background(&a));
            // Left edge of the Horizon and Zenith columns (a column can be put off screen).
            let cols = [arg_num(&a, "x0", 0), arg_num(&a, "x1", hud::CANVAS_W)];
            for (col, stance) in [Stance::Horizon, Stance::Zenith].into_iter().enumerate() {
                for (row, level) in [10u8, 6, 3, 0].into_iter().enumerate() {
                    let state = level_state(stance, level);
                    compose((cols[col], 8 + 40 * row as i32), &state, &mut |op| f.op(op));
                }
            }
            f.write(path);
        }
        "swap" => {
            let mut f = Frame::new(320, 240, background(&a));
            let stance = if a.iter().any(|s| s == "to=horizon") {
                Stance::Horizon
            } else {
                Stance::Zenith
            };
            let x0 = arg_num(&a, "x0", 0);
            for (row, swap) in [0u8, 2, 4, 6].into_iter().enumerate() {
                let state = swap_state(stance, swap);
                compose((x0, 8 + 40 * row as i32), &state, &mut |op| f.op(op));
            }
            f.write(path);
        }
        "bars" => {
            // Every bar in every look, on its own: each is a complete object.
            let mut f = Frame::new(176, 150, background(&a));
            for (row, (stance, look)) in [
                (Stance::Horizon, 2),
                (Stance::Horizon, 1),
                (Stance::Horizon, 0),
                (Stance::Zenith, 2),
                (Stance::Zenith, 1),
                (Stance::Zenith, 0),
            ]
            .into_iter()
            .enumerate()
            {
                hud::bar_alone((8, 4 + 24 * row as i32), stance, look, 7, &mut |op| {
                    f.op(op)
                });
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
            let mut health = [num(4, 4); 2];
            health[stance.index()] = num(3, 7);
            let state = HudState {
                stance,
                health,
                energy: [num(5, 6); 2],
                swap: num(6, 0),
            };
            let mut f = Frame::new(hud::CANVAS_W as usize + 8, 48, background(&a));
            compose((0, 4), &state, &mut |op| f.op(op));
            f.write(path);
        }
        _ => std::process::exit(2),
    }
}

#[cfg(test)]
mod tests {
    use super::hud::{
        compose, HudState, Op, Stance, ATLAS_H, ATLAS_HW_W, CANVAS_H, CANVAS_W, MAX_OPS, SWAP_BOW,
        SWAP_FRAMES,
    };

    /// Every state, including every frame of a swap, fits the buffers, and paints inside the
    /// canvas (give or take the bow of a swap) and the atlas.
    #[test]
    fn every_state_stays_inside_its_bounds() {
        for stance in [Stance::Horizon, Stance::Zenith] {
            for swap in 0..SWAP_FRAMES {
                for health in [0u8, 4, 10] {
                    for energy in [0u8, 3, 10] {
                        let state = HudState {
                            stance,
                            health: [health, 10 - health],
                            energy: [energy, 10 - energy],
                            swap,
                        };
                        let mut n = 0;
                        compose((0, 0), &state, &mut |op| {
                            n += 1;
                            match op {
                                Op::Rect { x, y, w, h, .. } => {
                                    assert!(x >= -SWAP_BOW && y >= 0 && w > 0 && h > 0);
                                    assert!(x + w <= CANVAS_W + SWAP_BOW && y + h <= CANVAS_H);
                                }
                                Op::Sprite { x, y, src, .. } => {
                                    assert!(x >= -SWAP_BOW && y >= 0);
                                    assert!(x + src.w as i32 <= CANVAS_W + SWAP_BOW);
                                    assert!(y + src.h as i32 <= CANVAS_H);
                                    assert!(src.u as usize + src.w as usize <= ATLAS_HW_W * 4);
                                    assert!(src.v as usize + src.h as usize <= ATLAS_H);
                                }
                                Op::Mask { .. } => {}
                            }
                        });
                        assert!(n <= MAX_OPS, "{n} ops, room for {MAX_OPS}");
                    }
                }
            }
        }
    }

    /// Every frame of a swap looks different from the one before it and from the rest state it
    /// ends in, in both directions: no wasted frames.
    #[test]
    fn swap_frames_all_differ() {
        for stance in [Stance::Horizon, Stance::Zenith] {
            let frames: Vec<Vec<[u8; 3]>> = (1..=super::hud::SWAP_FRAMES)
                .map(|swap| {
                    let mut f = super::Frame::new(176, 48, super::BG);
                    super::compose((0, 4), &super::swap_state(stance, swap), &mut |op| f.op(op));
                    f.px
                })
                .collect();
            for pair in frames.windows(2) {
                assert!(pair[0] != pair[1]);
            }
        }
    }

    /// Mask changes come in the same order every time and end with the mask off, so the GPU is left
    /// as it was found.
    #[test]
    fn mask_is_left_off() {
        for swap in 0..SWAP_FRAMES {
            let state = HudState {
                stance: Stance::Zenith,
                health: [5, 5],
                energy: [5, 5],
                swap,
            };
            let mut last = None;
            compose((0, 0), &state, &mut |op| {
                if let Op::Mask { set, check } = op {
                    last = Some((set, check));
                }
            });
            assert_eq!(last, Some((false, false)));
        }
    }
}
