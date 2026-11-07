//! `psx-svg-cook`: cook an SVG to a PSVG asset, rasterise one on the host, or
//! diff two PPM frames.
//!
//! ```text
//! psx-svg-cook cook <in.svg> -o <out.psvg> [--tolerance PX] [--budget total=N]
//!                   [--budget GROUP=N] [--budget-per-group N] [--quiet]
//! psx-svg-cook raster <in.psvg> -o <out.ppm> [--groups a,b] [--origin X,Y]
//!                   [--size W,H] [--bg RRGGBB] [--scale N]   (.png or .ppm by extension)
//! psx-svg-cook diff <a.ppm> <b.ppm> [--threshold N] [--region X0,Y0,X1,Y1]
//! psx-svg-cook png <in.ppm> -o <out.png|out.ppm> [--scale N] [--crop X0,Y0,X1,Y1]
//! psx-svg-cook meas <ram.bin>     (print the PSVGMEAS cycle table of a showcase run)
//! psx-svg-cook profile <nm.txt> <pc.csv> [top]   (attribute an emulator pc-line log to symbols)
//! ```

use std::process::ExitCode;

use psx_vector::{Placement, Style, VectorAsset};
use psx_vector_cook::raster::{compare, draw_group, AlignedAsset, Image};
use psx_vector_cook::{cook, Budget, CookOptions};

fn pair(s: &str) -> Result<Vec<i64>, String> {
    s.split(',')
        .map(|v| {
            v.trim()
                .parse::<i64>()
                .map_err(|_| format!("bad number `{v}`"))
        })
        .collect()
}

/// Print the cycle table the showcase guest writes into RAM: an 8-byte tag
/// `PSVGMEAS`, `rows_used` and `frames` as little-endian words, then rows of
/// an 8-byte name, triangles and cycles.
fn print_measurements(ram: &[u8]) -> Result<(), String> {
    let at = ram
        .windows(8)
        .position(|w| w == b"PSVGMEAS")
        .ok_or("no PSVGMEAS block in this RAM image")?;
    let word = |o: usize| -> Result<u32, String> {
        ram.get(at + o..at + o + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .ok_or_else(|| "truncated PSVGMEAS block".to_string())
    };
    let (rows, frames) = (word(8)?, word(12)?);
    println!("frames {frames}, rows {rows}");
    println!(
        "{:8} {:>5} {:>8} {:>10}",
        "row", "tris", "cycles", "per tri"
    );
    for r in 0..rows as usize {
        let base = 16 + r * 16;
        let name = ram.get(at + base..at + base + 8).ok_or("truncated row")?;
        let name = String::from_utf8_lossy(name);
        let (tris, cycles) = (word(base + 8)?, word(base + 12)?);
        let per = if tris == 0 {
            0.0
        } else {
            cycles as f64 / tris as f64
        };
        println!(
            "{:8} {tris:>5} {cycles:>8} {per:>10.1}",
            name.trim_end_matches('\0')
        );
    }
    Ok(())
}

/// Attribute the instruction counts of an emulator `--pc-line-log --pc-log-words`
/// CSV to the symbols of `llvm-nm -n` output and print the heaviest.
fn profile(nm: &str, pc: &str, top: usize) -> Result<(), String> {
    let mut syms: Vec<(u32, String)> = std::fs::read_to_string(nm)
        .map_err(|e| format!("{nm}: {e}"))?
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let addr = u32::from_str_radix(it.next()?, 16).ok()?;
            let kind = it.next()?;
            let name = it.next()?;
            (kind.eq_ignore_ascii_case("t")).then(|| (addr, name.to_string()))
        })
        .collect();
    syms.sort();
    let mut totals: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    let mut all = 0u64;
    for line in std::fs::read_to_string(pc)
        .map_err(|e| format!("{pc}: {e}"))?
        .lines()
        .skip(1)
    {
        let mut it = line.split(',');
        let (Some(addr), Some(n)) = (it.next(), it.next()) else {
            continue;
        };
        let addr =
            u32::from_str_radix(addr.trim_start_matches("0x"), 16).map_err(|e| e.to_string())?;
        let n: u64 = n.parse().map_err(|_| "bad count")?;
        let i = syms.partition_point(|(a, _)| *a <= addr);
        let name = if i == 0 {
            "?".to_string()
        } else {
            syms[i - 1].1.clone()
        };
        *totals.entry(name).or_default() += n;
        all += n;
    }
    let mut rows: Vec<_> = totals.into_iter().collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1));
    for (name, n) in rows.into_iter().take(top) {
        let short: String = name.chars().take(100).collect();
        println!("{n:>11} {:>5.1}%  {short}", 100.0 * n as f64 / all as f64);
    }
    Ok(())
}

fn run(args: &[String]) -> Result<(), String> {
    let cmd = args
        .first()
        .map(String::as_str)
        .ok_or("missing command (cook, raster, diff)")?;
    let mut positional = Vec::new();
    let mut flags: Vec<(String, String)> = Vec::new();
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        if let Some(name) = a.strip_prefix("--").or_else(|| a.strip_prefix('-')) {
            if name == "quiet" {
                flags.push((name.to_string(), String::new()));
            } else {
                let v = it.next().ok_or(format!("{a} needs a value"))?;
                flags.push((name.to_string(), v.clone()));
            }
        } else {
            positional.push(a.clone());
        }
    }
    let flag = |n: &str| {
        flags
            .iter()
            .rev()
            .find(|(k, _)| k == n)
            .map(|(_, v)| v.as_str())
    };
    match cmd {
        "cook" => {
            let input = positional.first().ok_or("missing input.svg")?;
            let out = flag("o").ok_or("missing -o <out.psvg>")?;
            let svg = std::fs::read_to_string(input).map_err(|e| format!("{input}: {e}"))?;
            let mut opts = CookOptions::default();
            if let Some(t) = flag("tolerance") {
                opts.tolerance_px = t.parse().map_err(|_| "bad --tolerance")?;
            }
            let mut budget = Budget::default();
            for (k, v) in &flags {
                match k.as_str() {
                    "budget" => {
                        let (name, n) = v.split_once('=').ok_or("--budget wants NAME=N")?;
                        let n: usize = n.parse().map_err(|_| "bad budget number")?;
                        if name == "total" {
                            budget.total = Some(n);
                        } else {
                            budget.groups.insert(name.to_string(), n);
                        }
                    }
                    "budget-per-group" => {
                        budget.per_group = Some(v.parse().map_err(|_| "bad budget number")?)
                    }
                    _ => {}
                }
            }
            opts.budget = budget;
            let cooked = cook(&svg, &opts).map_err(|e| e.to_string())?;
            std::fs::write(out, &cooked.bytes).map_err(|e| format!("{out}: {e}"))?;
            if flag("quiet").is_none() {
                println!("{}", cooked.report);
            }
            Ok(())
        }
        "raster" => {
            let input = positional.first().ok_or("missing input.psvg")?;
            let out = flag("o").ok_or("missing -o <out.ppm>")?;
            let bytes = std::fs::read(input).map_err(|e| format!("{input}: {e}"))?;
            let aligned = AlignedAsset::new(&bytes);
            let asset = VectorAsset::parse(aligned.bytes()).map_err(|e| format!("{e:?}"))?;
            let canvas = asset.canvas_q4();
            let size = match flag("size") {
                Some(s) => pair(s)?,
                None => vec![(canvas[0] as i64 + 15) / 16, (canvas[1] as i64 + 15) / 16],
            };
            let origin = flag("origin")
                .map(pair)
                .transpose()?
                .unwrap_or_else(|| vec![0, 0]);
            let bg = flag("bg").unwrap_or("000000");
            let bgv = u32::from_str_radix(bg, 16).map_err(|_| "bad --bg")?;
            let mut img = Image::new(
                size[0] as usize,
                size[1] as usize,
                [(bgv >> 16) as u8, (bgv >> 8) as u8, bgv as u8],
            );
            let wanted: Option<Vec<&str>> = flag("groups").map(|g| g.split(',').collect());
            for id in asset.groups() {
                if wanted
                    .as_ref()
                    .is_some_and(|w| !w.contains(&asset.group_name(id)))
                {
                    continue;
                }
                draw_group(
                    &mut img,
                    &asset,
                    id,
                    &Placement::IDENTITY,
                    &Style::DEFAULT,
                    (origin[0] as i32, origin[1] as i32),
                )
                .map_err(|e| format!("{e:?}"))?;
            }
            let scale: usize =
                flag("scale").map_or(Ok(1), |s| s.parse().map_err(|_| "bad --scale"))?;
            let img = if scale > 1 { img.upscaled(scale) } else { img };
            let bytes = if out.ends_with(".png") {
                img.to_png()
            } else {
                img.to_ppm()
            };
            std::fs::write(out, bytes).map_err(|e| format!("{out}: {e}"))
        }
        "diff" => {
            let (a, b) = (
                positional.first().ok_or("missing a.ppm")?,
                positional.get(1).ok_or("missing b.ppm")?,
            );
            let load = |p: &String| -> Result<Image, String> {
                Image::from_ppm(&std::fs::read(p).map_err(|e| format!("{p}: {e}"))?)
            };
            let (ia, ib) = (load(a)?, load(b)?);
            let threshold: u8 =
                flag("threshold").map_or(Ok(16), |t| t.parse().map_err(|_| "bad --threshold"))?;
            let region = match flag("region") {
                Some(r) => pair(r)?.iter().map(|&v| v as usize).collect::<Vec<_>>(),
                None => vec![0, 0, ia.width, ia.height],
            };
            let d = compare(
                &ia,
                &ib,
                threshold,
                [region[0], region[1], region[2], region[3]],
            );
            println!(
                "{} of {} pixels differ by more than {threshold} ({:.2}%), max channel diff {}, mean abs {:.3}",
                d.differing,
                d.total,
                d.fraction() * 100.0,
                d.max_channel,
                d.mean_abs
            );
            Ok(())
        }
        "png" => {
            let input = positional.first().ok_or("missing in.ppm")?;
            let out = flag("o").ok_or("missing -o <out.png>")?;
            let img = Image::from_ppm(&std::fs::read(input).map_err(|e| format!("{input}: {e}"))?)?;
            let img = match flag("crop") {
                Some(c) => {
                    let c = pair(c)?;
                    let (x0, y0, x1, y1) =
                        (c[0] as usize, c[1] as usize, c[2] as usize, c[3] as usize);
                    let mut cut = Image::new(x1 - x0, y1 - y0, [0, 0, 0]);
                    for y in y0..y1 {
                        for x in x0..x1 {
                            cut.rgb[((y - y0) * (x1 - x0) + (x - x0)) * 3..][..3]
                                .copy_from_slice(&img.pixel(x, y));
                        }
                    }
                    cut
                }
                None => img,
            };
            let scale: usize =
                flag("scale").map_or(Ok(1), |s| s.parse().map_err(|_| "bad --scale"))?;
            let img = img.upscaled(scale.max(1));
            let bytes = if out.ends_with(".ppm") {
                img.to_ppm()
            } else {
                img.to_png()
            };
            std::fs::write(out, bytes).map_err(|e| format!("{out}: {e}"))
        }
        "meas" => {
            let input = positional.first().ok_or("missing ram.bin")?;
            let ram = std::fs::read(input).map_err(|e| format!("{input}: {e}"))?;
            print_measurements(&ram)
        }
        "profile" => {
            let (nm, pc) = (
                positional.first().ok_or("missing nm.txt")?,
                positional.get(1).ok_or("missing pc.csv")?,
            );
            let top: usize = positional
                .get(2)
                .map_or(Ok(30), |t| t.parse().map_err(|_| "bad top"))?;
            profile(nm, pc, top)
        }
        other => Err(format!("unknown command `{other}`")),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("psx-svg-cook: {e}");
            ExitCode::FAILURE
        }
    }
}
