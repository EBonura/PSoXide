//! cellcheck: verify that the health cells and energy gems of a HUD grid frame are
//! pixel-identical shapes at a constant step.
//!
//! cellcheck <frame.ppm> [--cols 2,162] [--y0 8] [--pitch 40] [--levels 10,6,3,0]
//!           [--back-lit 4] [--no-back] [--bg-row 9] [--gem-y 22,32]
//!
//! For every HUD in the grid it extracts the connected pixel components of the cell band
//! (and of the back bar's visible rows, and of the gem band), then checks:
//!   * shape: every component has the same mask (same pixels relative to its corner);
//!   * colour: all lit components have the same pixels, all empty ones likewise;
//!   * step: the corners of consecutive components differ by one constant (dx, 0).

use std::collections::HashMap;
use std::process::ExitCode;

struct Frame {
    w: usize,
    h: usize,
    px: Vec<[u8; 3]>,
}

fn read_ppm(path: &str) -> Result<Frame, String> {
    let d = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let mut pos = 0;
    let mut tok = || -> String {
        while d[pos].is_ascii_whitespace() {
            pos += 1;
        }
        let s = pos;
        while !d[pos].is_ascii_whitespace() {
            pos += 1;
        }
        String::from_utf8_lossy(&d[s..pos]).into_owned()
    };
    if tok() != "P6" {
        return Err("not a P6 ppm".into());
    }
    let w: usize = tok().parse().map_err(|_| "bad width")?;
    let h: usize = tok().parse().map_err(|_| "bad height")?;
    let _max = tok();
    pos += 1;
    let px = d[pos..pos + w * h * 3]
        .chunks(3)
        .map(|c| [c[0], c[1], c[2]])
        .collect();
    Ok(Frame { w, h, px })
}

impl Frame {
    fn at(&self, x: i32, y: i32) -> [u8; 3] {
        self.px[y as usize * self.w + x as usize]
    }
}

#[derive(Clone)]
struct Comp {
    x0: i32,
    y0: i32,
    pixels: Vec<(i32, i32, [u8; 3])>, // relative to (x0, y0)
}

/// Connected components (8-neighbour) of pixels in `rect` not in `bg`, skipping any that touch
/// the left or right edge of the rect. Sorted by x0.
fn components(f: &Frame, rect: (i32, i32, i32, i32), bg: &[[u8; 3]]) -> Vec<Comp> {
    let (rx0, ry0, rx1, ry1) = rect;
    let (w, h) = ((rx1 - rx0) as usize, (ry1 - ry0) as usize);
    let ink = |x: i32, y: i32| !bg.contains(&f.at(x, y));
    let mut seen = vec![false; w * h];
    let mut out = Vec::new();
    for sy in ry0..ry1 {
        for sx in rx0..rx1 {
            let i = (sy - ry0) as usize * w + (sx - rx0) as usize;
            if seen[i] || !ink(sx, sy) {
                continue;
            }
            let mut stack = vec![(sx, sy)];
            seen[i] = true;
            let mut pts = Vec::new();
            while let Some((x, y)) = stack.pop() {
                pts.push((x, y));
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        let (nx, ny) = (x + dx, y + dy);
                        if nx < rx0 || nx >= rx1 || ny < ry0 || ny >= ry1 {
                            continue;
                        }
                        let j = (ny - ry0) as usize * w + (nx - rx0) as usize;
                        if !seen[j] && ink(nx, ny) {
                            seen[j] = true;
                            stack.push((nx, ny));
                        }
                    }
                }
            }
            let (minx, maxx) = (
                pts.iter().map(|p| p.0).min().unwrap(),
                pts.iter().map(|p| p.0).max().unwrap(),
            );
            if minx == rx0 || maxx == rx1 - 1 || pts.len() < 6 {
                continue;
            }
            let miny = pts.iter().map(|p| p.1).min().unwrap();
            let mut pixels: Vec<_> = pts
                .iter()
                .map(|&(x, y)| (x - minx, y - miny, f.at(x, y)))
                .collect();
            pixels.sort();
            out.push(Comp {
                x0: minx,
                y0: miny,
                pixels,
            });
        }
    }
    out.sort_by_key(|c| c.x0);
    out
}

fn mask(c: &Comp) -> Vec<(i32, i32)> {
    c.pixels.iter().map(|p| (p.0, p.1)).collect()
}

/// Most common colours of `rect`, top `n`.
fn top_colours(f: &Frame, rect: (i32, i32, i32, i32), n: usize) -> Vec<[u8; 3]> {
    let mut h: HashMap<[u8; 3], usize> = HashMap::new();
    for y in rect.1..rect.3 {
        for x in rect.0..rect.2 {
            *h.entry(f.at(x, y)).or_default() += 1;
        }
    }
    let mut v: Vec<_> = h.into_iter().collect();
    v.sort_by_key(|e| std::cmp::Reverse(e.1));
    v.into_iter().take(n).map(|e| e.0).collect()
}

struct Verdict {
    ok: bool,
    text: String,
}

fn check(name: &str, comps: &[Comp], expect: usize, lit: usize) -> Verdict {
    let mut ok = true;
    let mut t = format!("  {name}: {} components", comps.len());
    if comps.len() != expect {
        ok = false;
        t += &format!(" (expected {expect})");
    }
    if comps.is_empty() {
        return Verdict { ok: false, text: t };
    }
    let m0 = mask(&comps[0]);
    let same_mask = comps.iter().all(|c| mask(c) == m0);
    let lit_set = &comps[..lit.min(comps.len())];
    let off_set = &comps[lit.min(comps.len())..];
    let same_lit = lit_set.windows(2).all(|w| w[0].pixels == w[1].pixels);
    let same_off = off_set.windows(2).all(|w| w[0].pixels == w[1].pixels);
    let steps: Vec<i32> = comps.windows(2).map(|w| w[1].x0 - w[0].x0).collect();
    let same_step = steps.windows(2).all(|w| w[0] == w[1]);
    let same_y = comps.iter().all(|c| c.y0 == comps[0].y0);
    ok &= same_mask && same_lit && same_off && same_step && same_y;
    t += &format!(
        ", size {}x{} ({} px), mask identical: {}, lit identical: {}, empty identical: {}, step {:?} constant: {}, same row: {}",
        comps[0].pixels.iter().map(|p| p.0).max().unwrap() + 1,
        comps[0].pixels.iter().map(|p| p.1).max().unwrap() + 1,
        comps[0].pixels.len(),
        same_mask,
        same_lit,
        same_off,
        steps.first(),
        same_step,
        same_y
    );
    if std::env::var("CELLCHECK_DUMP").is_ok() {
        for c in comps {
            println!(
                "    {} {} {:?}",
                c.x0,
                c.y0,
                c.pixels.iter().take(8).map(|p| p.2).collect::<Vec<_>>()
            );
        }
    }
    if !same_mask {
        let mut sizes: Vec<usize> = comps.iter().map(|c| c.pixels.len()).collect();
        sizes.dedup();
        t += &format!(
            " [pixel counts {:?}]",
            comps.iter().map(|c| c.pixels.len()).collect::<Vec<_>>()
        );
    }
    Verdict { ok, text: t }
}

fn list(a: &[String], key: &str, default: &str) -> Vec<i32> {
    let v = a
        .iter()
        .position(|s| s == key)
        .and_then(|i| a.get(i + 1))
        .map(|s| s.as_str())
        .unwrap_or(default);
    v.split(',').filter_map(|s| s.parse().ok()).collect()
}

fn main() -> ExitCode {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = a.first() else {
        eprintln!("usage: cellcheck <frame.ppm> [options]");
        return ExitCode::from(2);
    };
    let f = match read_ppm(path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let cols = list(&a, "--cols", "2,162");
    let y0 = list(&a, "--y0", "8")[0];
    let pitch = list(&a, "--pitch", "40")[0];
    let levels = list(&a, "--levels", "10,6,3,0");
    let bg_row = list(&a, "--bg-row", "9")[0];
    let gem_y = list(&a, "--gem-y", "22,32");
    let back_lit = list(&a, "--back-lit", "4")[0] as usize;
    let no_back = a.iter().any(|s| s == "--no-back");
    let mut all_ok = true;
    for (ci, &ox) in cols.iter().enumerate() {
        for (ri, &lvl) in levels.iter().enumerate() {
            let oy = y0 + pitch * ri as i32;
            println!("HUD col {ci} row {ri} (origin {ox},{oy}), lit {lvl}/10");
            let strip = top_colours(&f, (ox + 60, oy + bg_row, ox + 120, oy + bg_row + 1), 1);
            let cells = components(&f, (ox + 24, oy + 10, ox + 162, oy + 18), &strip);
            let v = check("cells", &cells, 10, lvl as usize);
            println!("{}  -> {}", v.text, if v.ok { "PASS" } else { "FAIL" });
            all_ok &= v.ok;
            if !no_back {
                let bstrip = top_colours(
                    &f,
                    (ox + 60, oy - 7 + bg_row, ox + 120, oy - 7 + bg_row + 1),
                    1,
                );
                let back = components(&f, (ox + 24, oy + 3, ox + 162, oy + 8), &bstrip);
                let v = check("back cells", &back, 10, back_lit);
                println!("{}  -> {}", v.text, if v.ok { "PASS" } else { "FAIL" });
                all_ok &= v.ok;
            }
            let gband = (ox + 26, oy + gem_y[0], ox + 170, oy + gem_y[1]);
            let bg = top_colours(&f, gband, 2);
            let gems = components(&f, gband, &bg);
            let v = check("gems", &gems, 10, lvl as usize);
            println!("{}  -> {}", v.text, if v.ok { "PASS" } else { "FAIL" });
            all_ok &= v.ok;
        }
    }
    println!("{}", if all_ok { "ALL PASS" } else { "SOME FAILED" });
    if all_ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
