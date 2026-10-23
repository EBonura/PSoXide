//! hudview: paints the sprite HUD on the host with the same composition code the guest uses, for
//! quick looks and for comparing with an emulator frame.
//!
//! hudview grid <out.ppm> [bg=r,g,b] [x0=n] [x1=n]
//!     the two-stance, four-level grid of the `hud-grid` frame; x0 and x1 are the left edges of the columns
//! hudview swap <out.ppm> [to=zenith|horizon] [bg=r,g,b] [x0=n]
//!     one column of the swap frames 0, 2, 4 and 6 (the `HUD_SWAP` build of the grid frame)
//! hudview bars <out.ppm> [bg=r,g,b]
//!     every bar at every size, alone
//! hudview size <out.ppm> size=n [bg=r,g,b]
//!     the grid of one size of the bar alone (the `HUD_SIZE` build of the grid frame)
//! hudview one <out.ppm> [horizon|zenith] [health] [other health] [energy] [swap] [bg=r,g,b]

#![allow(dead_code)]

#[path = "../../../../editor-playtest/src/stance_hud_sprites.rs"]
mod hud;

use hud::{compose, demo_state, AmmoFx, HudAnim, HudState, Op, Stance, REVEAL_MAX};

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

    /// A frame read from a binary PPM (the emulator's display dump), as the display shows it.
    fn from_ppm(path: &str) -> Self {
        let data = std::fs::read(path).expect("read ppm");
        let mut fields = Vec::new();
        let mut at = 0;
        while fields.len() < 4 {
            while data[at].is_ascii_whitespace() {
                at += 1;
            }
            let start = at;
            while !data[at].is_ascii_whitespace() {
                at += 1;
            }
            fields.push(String::from_utf8_lossy(&data[start..at]).into_owned());
        }
        at += 1;
        let (w, h): (usize, usize) = (fields[1].parse().unwrap(), fields[2].parse().unwrap());
        let mut f = Self::new(w, h, [0, 0, 0]);
        for (p, c) in f.px.iter_mut().zip(data[at..].chunks_exact(3)) {
            *p = shown([c[0], c[1], c[2]]);
        }
        f
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
    HudState::rest(stance, health, level)
}

/// The state of the `HUD_SWAP` frame: a swap to `stance` at frame `swap`.
fn swap_state(stance: Stance, swap: u8) -> HudState {
    HudState {
        swap,
        ..HudState::rest(stance, [7, 5], 8)
    }
}

/// The state of the `HUD_FOLD` frame at fold step `f` (1 to `REVEAL_MAX`): Horizon in front, the
/// bar behind two steps more open.
fn fold_state(f: u8) -> HudState {
    HudState {
        reveal: [f, (f + 2).min(REVEAL_MAX)],
        ammo_reveal: f,
        ..HudState::rest(Stance::Horizon, [7, 5], 8)
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
        "over" => {
            // The HUD over a frame of the game: `hudview over <out> <game.ppm> [x=n] [y=n] [stance=zenith]
            // [health=n] [other=n] [energy=n] [swap=n]`.
            let Some(game) = a.get(2) else {
                eprintln!("usage: hudview over <out.ppm> <game.ppm> [options]");
                std::process::exit(2);
            };
            let mut f = Frame::from_ppm(game);
            let stance = if a.iter().any(|s| s == "stance=zenith") {
                Stance::Zenith
            } else {
                Stance::Horizon
            };
            let mut health = [arg_num(&a, "other", 4) as u8; 2];
            health[stance.index()] = arg_num(&a, "health", 7) as u8;
            let energy = arg_num(&a, "energy", 6) as u8;
            let mut state = HudState {
                swap: arg_num(&a, "swap", 0) as u8,
                ..HudState::rest(stance, health, energy)
            };
            state.reveal = [arg_num(&a, "reveal", REVEAL_MAX as i32) as u8; 2];
            state.ammo_reveal = state.reveal[0];
            state.ammo_hint = arg_num(&a, "hint", 0) != 0;
            state.pulse = arg_num(&a, "pulse", 0) != 0;
            compose(
                (arg_num(&a, "x", 6), arg_num(&a, "y", 4)),
                &state,
                &mut |op| f.op(op),
            );
            f.write(path);
        }
        "fold" => {
            // The `HUD_FOLD` frame: fold steps 1 to 8, four down each column.
            let mut f = Frame::new(320, 240, background(&a));
            for step in 1..=REVEAL_MAX {
                let (col, row) = ((step - 1) as i32 / 4, (step - 1) as i32 % 4);
                compose((160 * col, 8 + 40 * row), &fold_state(step), &mut |op| f.op(op));
            }
            f.write(path);
        }
        "seq" => {
            // The `HUD_SEQ` frame: 12 frames of an animation demo (`demo=` 0 fold, 1 shot, 2 load, 3
            // reload, 4 jump; `page=` 0 or 1 for frames 0 to 11 or 12 to 23), two columns of six.
            let (demo, page) = (arg_num(&a, "demo", 0) as u8, arg_num(&a, "page", 0) as u16);
            let mut f = Frame::new(320, 240, background(&a));
            for i in 0..12u16 {
                let (col, row) = ((i / 6) as i32, (i % 6) as i32);
                let state = demo_state(demo, page * 12 + i);
                compose((160 * col, 8 + 40 * row), &state, &mut |op| f.op(op));
            }
            f.write(path);
        }
        "fxseq" => {
            // The ammunition effects frame by frame: `scenario=shot` (two shots), `load` (a pill
            // recharging a unit at a time), `reload` (all pills at once) or `jump` (three pills),
            // 12 frames down each of two columns.
            let scenario = a
                .iter()
                .find_map(|s| s.strip_prefix("scenario="))
                .unwrap_or("shot");
            let start = match scenario {
                "shot" => 100,
                "load" => 70,
                "reload" => 0,
                _ => 40,
            };
            let mut fx = AmmoFx::new(start);
            let mut f = Frame::new(320, 8 + 40 * 12, background(&a));
            for n in 0..24u16 {
                let energy = match scenario {
                    "shot" => match n {
                        0 => 100,
                        1..=8 => 80,
                        _ => 60,
                    },
                    "load" => 70 + n * 2 / 3,
                    "reload" => if n == 0 { 0 } else { 100 },
                    _ => if n == 0 { 40 } else { 70 },
                };
                fx.step(energy);
                let mut state = HudState::rest(Stance::Horizon, [7, 5], 0);
                fx.apply(&mut state);
                let (col, row) = ((n / 12) as i32, (n % 12) as i32);
                compose((160 * col, 8 + 40 * row), &state, &mut |op| f.op(op));
            }
            f.write(path);
        }
        "foldseq" => {
            // A fold and then an unfold, frame by frame: the fold down the left column, the unfold
            // down the right one, 12 frames each.
            let hint = arg_num(&a, "hint", 0) != 0;
            let mut f = Frame::new(320, 8 + 40 * 12, background(&a));
            let mut anim = HudAnim::open();
            for (col, open) in [(0, false), (1, true)] {
                for row in 0..12 {
                    anim.step(open, Stance::Horizon);
                    let mut state = HudState::rest(Stance::Horizon, [7, 5], 8);
                    state.ammo_hint = hint;
                    anim.apply(&mut state);
                    compose((160 * col, 8 + 40 * row), &state, &mut |op| f.op(op));
                }
            }
            f.write(path);
        }
        "swapall" => {
            // Every frame of a swap, 0 to 7: frames 0 to 3 down the left column, 4 to 7 down the
            // right one.
            let mut f = Frame::new(320, 240, background(&a));
            let stance = if a.iter().any(|s| s == "to=horizon") {
                Stance::Horizon
            } else {
                Stance::Zenith
            };
            for swap in 0..hud::SWAP_FRAMES {
                let origin = (
                    160 * (swap as i32 / 4) + arg_num(&a, "x0", 0),
                    8 + 40 * (swap as i32 % 4),
                );
                compose(origin, &swap_state(stance, swap), &mut |op| f.op(op));
            }
            f.write(path);
        }
        "bars" => {
            // Every bar at every size, on its own: each is a complete object.
            let mut f = Frame::new(176, 4 + 22 * 8, background(&a));
            let mut row = 0;
            for stance in [Stance::Horizon, Stance::Zenith] {
                for size in 0..hud::SIZES {
                    hud::bar_alone((8, 4 + 22 * row), stance, size, 7, &mut |op| f.op(op));
                    row += 1;
                }
            }
            f.write(path);
        }
        "size" => {
            // The `HUD_SIZE` frame: one size of the bar on its own, two stances by four levels.
            let size = arg_num(&a, "size", 0) as usize;
            let mut f = Frame::new(320, 240, background(&a));
            for (col, stance) in [Stance::Horizon, Stance::Zenith].into_iter().enumerate() {
                for (row, level) in [10u8, 6, 3, 0].into_iter().enumerate() {
                    let origin = (160 * col as i32, 8 + 40 * row as i32);
                    hud::bar_alone(origin, stance, size, level, &mut |op| f.op(op));
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
            let mut health = [num(4, 4); 2];
            health[stance.index()] = num(3, 7);
            let state = HudState {
                swap: num(6, 0),
                ..HudState::rest(stance, health, num(5, 6))
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
                            swap,
                            ..HudState::rest(stance, [health, 10 - health], energy)
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
                swap,
                ..HudState::rest(Stance::Zenith, [5, 5], 5)
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
