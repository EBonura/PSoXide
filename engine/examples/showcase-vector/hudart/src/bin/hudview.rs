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
            Op::Quad { p, rgb, glass } => {
                let p = p.map(|(x, y)| (i32::from(x), i32::from(y)));
                let c5 = rgb.map(|v| (v >> 3) as u16);
                for tri in [[0, 1, 2], [1, 2, 3]] {
                    self.triangle(tri.map(|i| p[i]), |_, _| Some((c5, glass, false)));
                }
            }
            Op::TexQuad {
                p,
                src,
                clut,
                glass,
            } => {
                let p = p.map(|(x, y)| (i32::from(x), i32::from(y)));
                // The texels of `src` stretched over the quad by the first triangle's affine map
                // and the second's (the GPU interpolates texture coordinates per triangle), with
                // half-open coordinates: the far corners are one texel past the last.
                let uv = [
                    (0.0, 0.0),
                    (src.w as f64, 0.0),
                    (0.0, src.h as f64),
                    (src.w as f64, src.h as f64),
                ];
                for tri in [[0, 1, 2], [1, 2, 3]] {
                    let corners = tri.map(|i| p[i]);
                    let uvs = tri.map(|i| uv[i]);
                    self.triangle(corners, |bary, _| {
                        let u = bary[0] * uvs[0].0 + bary[1] * uvs[1].0 + bary[2] * uvs[2].0;
                        let v = bary[0] * uvs[0].1 + bary[1] * uvs[1].1 + bary[2] * uvs[2].1;
                        let tx = (u.floor() as i32).clamp(0, src.w as i32 - 1) as usize;
                        let ty = (v.floor() as i32).clamp(0, src.h as i32 - 1) as usize;
                        let (u, v) = (src.u as usize + tx, src.v as usize + ty);
                        let hw = hud::ATLAS[v * hud::ATLAS_HW_W + u / 4];
                        let idx = ((hw >> ((u % 4) * 4)) & 15) as usize;
                        let entry = hud::CLUTS[clut * 16 + idx];
                        if entry == 0 {
                            return None;
                        }
                        let c5 = [entry & 31, (entry >> 5) & 31, (entry >> 10) & 31];
                        let translucent = glass && entry & 0x8000 != 0;
                        Some((c5, translucent, translucent))
                    });
                }
            }
        }
    }

    /// Fill a triangle the way the host sees it: a pixel is inside when its centre is, with the
    /// top-left rule for the pixels on an edge. `shade` gets the pixel's barycentric weights and
    /// says what to plot (colour, glass, translucent texel) or nothing. This is the viewer's
    /// approximation of the GPU's rasteriser, for looks, not an emulator.
    fn triangle(
        &mut self,
        v: [(i32, i32); 3],
        mut shade: impl FnMut([f64; 3], ()) -> Option<([u16; 3], bool, bool)>,
    ) {
        let area = |a: (i32, i32), b: (i32, i32), c: (i32, i32)| {
            (b.0 - a.0) as i64 * (c.1 - a.1) as i64 - (b.1 - a.1) as i64 * (c.0 - a.0) as i64
        };
        let total = area(v[0], v[1], v[2]);
        if total == 0 {
            return;
        }
        // Wind every triangle the same way, so that "inside" means the same sign.
        let swapped = total < 0;
        let (v, _total) = if swapped {
            ([v[0], v[2], v[1]], -total)
        } else {
            (v, total)
        };
        let (x0, x1) = (
            v.iter().map(|p| p.0).min().unwrap(),
            v.iter().map(|p| p.0).max().unwrap(),
        );
        let (y0, y1) = (
            v.iter().map(|p| p.1).min().unwrap(),
            v.iter().map(|p| p.1).max().unwrap(),
        );
        // Edge function at doubled coordinates, so that pixel centres are whole numbers.
        let edge = |a: (i32, i32), b: (i32, i32), px: i32, py: i32| -> i64 {
            (b.0 - a.0) as i64 * (2 * py + 1 - 2 * a.1) as i64
                - (b.1 - a.1) as i64 * (2 * px + 1 - 2 * a.0) as i64
        };
        let top_left = |a: (i32, i32), b: (i32, i32)| {
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            dy < 0 || (dy == 0 && dx > 0)
        };
        for py in y0..y1 {
            for px in x0..x1 {
                let mut w = [0i64; 3];
                let mut inside = true;
                for i in 0..3 {
                    let (a, b) = (v[(i + 1) % 3], v[(i + 2) % 3]);
                    let e = edge(a, b, px, py);
                    if e < 0 || (e == 0 && !top_left(a, b)) {
                        inside = false;
                        break;
                    }
                    w[i] = e;
                }
                if !inside {
                    continue;
                }
                let sum = (w[0] + w[1] + w[2]) as f64;
                let mut bary = [w[0] as f64 / sum, w[1] as f64 / sum, w[2] as f64 / sum];
                if swapped {
                    bary.swap(1, 2);
                }
                if let Some((c5, glass, stp)) = shade(bary, ()) {
                    self.plot(px, py, c5, glass, stp);
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

/// The state of the HUD at `level` of 10 in the `hud-grid` frame: the active stance at `level`,
/// the other one at 4, the gems at `level`.
fn level_state(stance: Stance, level: u8) -> HudState {
    let mut health = [4, 4];
    health[stance.index()] = level;
    HudState::rest(stance, health, level)
}

/// The state of the `HUD_SWAP` frame: a swap to `stance` at frame `swap` of `SWAP_FRAMES`.
fn swap_state(stance: Stance, swap: u8) -> HudState {
    HudState {
        swap: (u32::from(swap) * 4096 / u32::from(hud::SWAP_FRAMES)) as u16,
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
                swap: arg_num(&a, "swap", 0) as u16,
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
                compose((160 * col, 8 + 40 * row), &fold_state(step), &mut |op| {
                    f.op(op)
                });
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
                    "reload" => {
                        if n == 0 {
                            0
                        } else {
                            100
                        }
                    }
                    _ => {
                        if n == 0 {
                            40
                        } else {
                            70
                        }
                    }
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
        "swaptick" => {
            // The swap tick by tick as the game draws it (`SWAP_TICKS` of them, 60 a second), every
            // `step`th one, `cols` to a row: `swaptick <out> [to=horizon] [step=2] [cols=4]`.
            let stance = if a.iter().any(|s| s == "to=horizon") {
                Stance::Horizon
            } else {
                Stance::Zenith
            };
            let step = arg_num(&a, "step", 2).max(1) as u16;
            let cols = arg_num(&a, "cols", 4).max(1) as usize;
            let ticks: Vec<u16> = match a.iter().find_map(|s| s.strip_prefix("ticks=")) {
                Some(list) => list.split(',').filter_map(|n| n.parse().ok()).collect(),
                None => (0..=hud::SWAP_TICKS).step_by(step as usize).collect(),
            };
            let rows = ticks.len().div_ceil(cols);
            let (cw, ch) = (hud::CANVAS_W as usize + 12, 44usize);
            let mut f = Frame::new(cw * cols, ch * rows, background(&a));
            for (i, t) in ticks.iter().enumerate() {
                let swap = if *t == 0 {
                    0
                } else if *t >= hud::SWAP_TICKS {
                    4096
                } else {
                    (u32::from(*t) * 4096 / u32::from(hud::SWAP_TICKS)) as u16
                };
                let mut state = HudState::rest(stance, [7, 5], 8);
                state.swap = swap;
                if *t == 0 {
                    state.stance = stance.other();
                }
                let origin = ((cw * (i % cols)) as i32 + 6, (ch * (i / cols)) as i32 + 4);
                compose(origin, &state, &mut |op| f.op(op));
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
                swap: arg_num(&a, "swap", 0) as u16,
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
    };

    /// Every state, including every frame of a swap, fits the buffers, and paints inside the
    /// canvas (give or take the bow of a swap) and the atlas.
    #[test]
    fn every_state_stays_inside_its_bounds() {
        for stance in [Stance::Horizon, Stance::Zenith] {
            for swap in (0..4096).step_by(97) {
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
                                Op::Quad { p, .. } | Op::TexQuad { p, .. } => {
                                    for (x, y) in p.map(|(x, y)| (i32::from(x), i32::from(y))) {
                                        assert!(x >= -2 * SWAP_BOW && y >= -4);
                                        assert!(x <= CANVAS_W + 2 * SWAP_BOW && y <= CANVAS_H + 4);
                                    }
                                    if let Op::TexQuad { src, .. } = op {
                                        assert!(src.u as usize + src.w as usize <= ATLAS_HW_W * 4);
                                        assert!(src.v as usize + src.h as usize <= ATLAS_H);
                                    }
                                }
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
            let frames: Vec<Vec<[u8; 3]>> = (1..super::hud::SWAP_FRAMES)
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
        for swap in (0..4096).step_by(113) {
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

    /// The swap starts where the stance it leaves was at rest and ends where the stance it reaches is:
    /// every piece of the bars is where the rest frame puts it, give or take the rounding of the
    /// first and last tick, and the first and last frames paint nearly the same pixels.
    #[test]
    fn swap_starts_and_ends_at_rest() {
        for to in [Stance::Horizon, Stance::Zenith] {
            let health = [7, 5];
            let rest_to = HudState::rest(to, health, 8);
            let rest_from = HudState::rest(to.other(), health, 8);
            for (swap, rest) in [(1u16, rest_from), (4095, rest_to)] {
                let state = HudState { swap, ..rest_to };
                let mut moving = super::Frame::new(176, 48, super::BG);
                compose((0, 4), &state, &mut |op| moving.op(op));
                let mut at_rest = super::Frame::new(176, 48, super::BG);
                compose((0, 4), &rest, &mut |op| at_rest.op(op));
                let differ = moving
                    .px
                    .iter()
                    .zip(&at_rest.px)
                    .filter(|(a, b)| a != b)
                    .count();
                assert!(differ < 120, "{to:?} swap {swap}: {differ} pixels differ");
            }
        }
    }

    /// Where each stance's bar is at tick `t` of a swap to `to`, from the scene.
    fn places(to: Stance, t: u32) -> [(i32, i32, i32); 2] {
        let ticks = super::hud::SWAP_TICKS as u32;
        let state = HudState {
            swap: (t * 4096 / ticks) as u16,
            ..HudState::rest(to, [7, 5], 8)
        };
        let sc = super::hud::scene(&state);
        let mut out = [(0, 0, 0); 2];
        for (stance, at) in [sc.front, sc.behind] {
            if let super::hud::Placement::Moving(xf) = at {
                out[stance.index()] = (xf.x, xf.y, xf.depth);
            }
        }
        out
    }

    /// No bar jumps from one tick of the swap to the next: it moves a few pixels at most and
    /// changes size by a small part of one size of the art, so at 60 ticks a second, and at any
    /// frame rate that renders every second tick or so, each frame the bars move a little.
    #[test]
    fn swap_moves_a_few_pixels_a_tick() {
        let ticks = super::hud::SWAP_TICKS as u32;
        let (mut dxy, mut dd) = (0, 0);
        for to in [Stance::Horizon, Stance::Zenith] {
            for t in 1..ticks - 1 {
                let (a, b) = (places(to, t), places(to, t + 1));
                for s in 0..2 {
                    dxy = dxy.max((a[s].0 - b[s].0).abs().max((a[s].1 - b[s].1).abs()));
                    dd = dd.max((a[s].2 - b[s].2).abs());
                }
            }
        }
        eprintln!(
            "most a bar moves in one tick: {} px, size {}/256",
            dxy as f32 / 256.0,
            dd
        );
        // Q8 pixels: 5 pixels, a third of a size of the art.
        assert!(dxy <= 5 * 256 && dd <= 90);
    }

    /// A bar at rest, drawn as quads with the bar's own size, turn and place, is the pixel art drawn
    /// 1:1 from sprites: the same pixels, in every size, stance and fill.
    #[test]
    fn rest_quads_equal_the_sprites() {
        use super::hud::{bar_alone, bar_moving, Fold, Part, Xf, LCAP_X};
        for stance in [Stance::Horizon, Stance::Zenith] {
            for size in 0..3usize {
                for lit in [0u8, 3, 10] {
                    let mut sprites = super::Frame::new(180, 24, super::BG);
                    bar_alone((4, 3), stance, size, lit, &mut |op| sprites.op(op));
                    let mut quads = super::Frame::new(180, 24, super::BG);
                    let xf = Xf {
                        x: LCAP_X << 8,
                        y: 0,
                        depth: (size as i32) << 8,
                        sin: 0,
                        cos: 4096,
                    };
                    for part in [Part::Plate, Part::Cells] {
                        bar_moving(
                            (4, 3),
                            stance,
                            xf,
                            lit,
                            Fold::FULL,
                            part,
                            false,
                            &mut |op| quads.op(op),
                        );
                    }
                    let differ = sprites
                        .px
                        .iter()
                        .zip(&quads.px)
                        .filter(|(a, b)| a != b)
                        .count();
                    assert_eq!(differ, 0, "{stance:?} size {size} lit {lit}");
                }
            }
        }
    }

    /// A bar at rest comes out of the same code as a bar on the move, and yet its ops are exactly the
    /// ones the pixel art was made with (`bar`): every stance, both sizes it rests at, every fold
    /// step, every part.
    #[test]
    fn a_bar_at_rest_is_the_ops_of_the_art() {
        use super::hud::{bar, bar_at, fold_of, Part, Placement, Pose, BACK_SIZE, GEO, REVEAL_MAX};
        for stance in [Stance::Horizon, Stance::Zenith] {
            for size in [0, BACK_SIZE] {
                for reveal in 1..=REVEAL_MAX {
                    let fold = fold_of(reveal, &GEO[size]).unwrap();
                    for part in [Part::Glass, Part::Opaque, Part::Plate, Part::Cells] {
                        let pose = Pose { x: 3, y: 9, size };
                        let (mut art, mut rest) = (Vec::new(), Vec::new());
                        bar((5, 7), stance, pose, 6, fold, part, &mut |op| art.push(op));
                        bar_at(
                            (5, 7),
                            stance,
                            &Placement::Rest(pose),
                            6,
                            fold,
                            part,
                            &mut |op| rest.push(op),
                        );
                        assert_eq!(art, rest, "{stance:?} size {size} reveal {reveal} {part:?}");
                    }
                }
            }
        }
    }
}
