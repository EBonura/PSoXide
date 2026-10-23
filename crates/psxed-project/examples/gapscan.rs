//! Looks for holes in the ground of the far view on a route.
//!
//! Usage:
//!   `gapscan scan <frames dir> <route.csv> <trace.log> <out dir> [offset [worst]]`
//!   `gapscan compare <before out dir> <after out dir>`
//!
//! `scan` takes a headless run's screenshots (`tick-NNNNNN.ppm`), its per-tick route log and
//! the debug lines of a `card-trace` build, which give the screen row of the horizon and the
//! eye's height for every drawn frame (`ct H`). In a frame, every pixel more than `offset`
//! rows (40) below the horizon that shows the plain backdrop colour is a pixel where nothing
//! is drawn: no geometry, no card. On a flat floor the ground twelve sectors away from an eye
//! one and a half sectors up is about 40 rows under the horizon, so those rows hold only
//! ground nearer than twelve sectors. Only frames whose eye stands within a sector of the
//! height it starts at (the plain the level opens on; off it the floor is not flat and the
//! rows say nothing) count. Writes `shares.csv` (poll, share, counted) and the worst frames
//! with the pixels painted red.
//!
//! `compare` pairs the two runs' frames by controller poll (the same tape puts the camera
//! in the same place) and reports the share before and after over the counted frames.

use std::path::{Path, PathBuf};

use image::{Rgb, RgbImage};

/// The backdrop's flat colour as the console shows it (5 bits a channel).
const BACKDROP: [u8; 3] = [33, 41, 41];
/// Eye heights within this of the starting height (runtime units) count as the plain.
const PLAIN_TOLERANCE: i32 = 80;

fn read_ppm(path: &Path) -> Option<(usize, usize, Vec<u8>)> {
    let data = std::fs::read(path).ok()?;
    let mut fields = Vec::new();
    let mut at = 0;
    while fields.len() < 4 && at < data.len() {
        while at < data.len() && data[at].is_ascii_whitespace() {
            at += 1;
        }
        let start = at;
        while at < data.len() && !data[at].is_ascii_whitespace() {
            at += 1;
        }
        fields.push(std::str::from_utf8(&data[start..at]).ok()?.to_string());
    }
    at += 1;
    if fields.first().map(String::as_str) != Some("P6") {
        return None;
    }
    let (w, h) = (fields[1].parse().ok()?, fields[2].parse().ok()?);
    Some((w, h, data[at..].to_vec()))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("scan") if args.len() >= 5 => scan(&args[1..]),
        Some("compare") if args.len() == 3 => compare(&args[1], &args[2]),
        _ => {
            eprintln!(
                "usage: gapscan scan <frames dir> <route.csv> <trace.log> <out dir> [offset [worst]]\n       gapscan compare <before out dir> <after out dir>"
            );
            std::process::exit(2);
        }
    }
}

fn scan(args: &[String]) {
    let frames = PathBuf::from(&args[0]);
    let offset: i32 = args.get(4).and_then(|a| a.parse().ok()).unwrap_or(40);
    let worst: usize = args.get(5).and_then(|a| a.parse().ok()).unwrap_or(8);
    let out = PathBuf::from(&args[3]);
    std::fs::create_dir_all(&out).expect("create the output directory");
    let route = std::fs::read_to_string(&args[1]).expect("read the route log");
    let mut lines = route.lines();
    let header: Vec<&str> = lines.next().expect("a header").split(',').collect();
    let col = |name: &str| {
        header
            .iter()
            .position(|h| *h == name)
            .expect("a route column")
    };
    let (tick_col, cycle_col, poll_col) =
        (col("route_tick"), col("bus_cycles"), col("port1_polls"));
    // Per tick: bus cycle and controller polls.
    let ticks: Vec<(u64, u64, u64)> = lines
        .filter_map(|l| {
            let c: Vec<&str> = l.split(',').collect();
            Some((
                c.get(tick_col)?.parse().ok()?,
                c.get(cycle_col)?.parse().ok()?,
                c.get(poll_col)?.parse().ok()?,
            ))
        })
        .collect();
    let trace = std::fs::read_to_string(&args[2]).expect("read the trace");
    // Per drawn frame: bus cycle, horizon row, eye height.
    let mut samples: Vec<(u64, i32, i32)> = Vec::new();
    for line in trace.lines() {
        let Some(rest) = line.split("] ct H ").nth(1) else {
            continue;
        };
        let mut numbers = rest.split_whitespace();
        let (Some(value), Some(eye_y)) = (numbers.next(), numbers.next()) else {
            continue;
        };
        let Some(cycle) = line.split(" c").nth(1).and_then(|s| s.split(']').next()) else {
            continue;
        };
        let (Ok(cycle), Ok(value), Ok(eye_y)) = (
            cycle.parse::<u64>(),
            value.parse::<i32>(),
            eye_y.parse::<i32>(),
        ) else {
            continue;
        };
        if value != 9999 {
            samples.push((cycle, value - 4096, eye_y - 32768));
        }
    }
    let start_height = {
        let mut first: Vec<i32> = samples.iter().skip(200).take(300).map(|s| s.2).collect();
        first.sort_unstable();
        first.get(first.len() / 2).copied().unwrap_or(0)
    };
    println!(
        "{} horizon samples, {} route ticks, the plain at eye height {}",
        samples.len(),
        ticks.len(),
        start_height
    );
    let mut files: Vec<(u64, PathBuf)> = std::fs::read_dir(&frames)
        .expect("read the frames")
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let tick = p
                .file_stem()?
                .to_str()?
                .strip_prefix("tick-")?
                .parse()
                .ok()?;
            Some((tick, p))
        })
        .collect();
    files.sort();
    // (poll, share, counted, tick)
    let mut rows: Vec<(u64, f32, bool, u64)> = Vec::new();
    for (tick, path) in &files {
        let Some(&(_, cycle, poll)) = ticks.iter().find(|t| t.0 == *tick) else {
            continue;
        };
        let Some(&(_, horizon, eye_y)) = samples.iter().rev().find(|s| s.0 <= cycle) else {
            continue;
        };
        let Some((w, h, px)) = read_ppm(path) else {
            continue;
        };
        let start = (horizon + offset).clamp(0, h as i32) as usize;
        let (mut count, mut total) = (0usize, 0usize);
        for y in start..h {
            for x in 0..w {
                let i = (y * w + x) * 3;
                total += 1;
                count += usize::from(px[i..i + 3] == BACKDROP);
            }
        }
        if total > 0 {
            let counted = (eye_y - start_height).abs() <= PLAIN_TOLERANCE;
            rows.push((poll, count as f32 / total as f32, counted, *tick));
        }
    }
    let mut csv = String::from("poll,share,counted\n");
    for r in &rows {
        csv.push_str(&format!("{},{:.5},{}\n", r.0, r.1, u8::from(r.2)));
    }
    std::fs::write(out.join("shares.csv"), csv).expect("write shares.csv");
    let mut shares: Vec<f32> = rows.iter().filter(|r| r.2).map(|r| r.1).collect();
    if shares.is_empty() {
        println!("no frame on the plain");
        return;
    }
    shares.sort_by(f32::total_cmp);
    let n = shares.len();
    let over = |t: f32| shares.iter().filter(|&&s| s > t).count();
    println!(
        "{n} of {} frames on the plain: backdrop share of the ground rows mean {:.4}, p50 {:.4}, p90 {:.4}, max {:.4}; over 1 percent {}, over 5 percent {}",
        rows.len(),
        shares.iter().sum::<f32>() / n as f32,
        shares[n / 2],
        shares[n * 9 / 10],
        shares[n - 1],
        over(0.01),
        over(0.05)
    );
    let mut order: Vec<_> = rows.iter().filter(|r| r.2).collect();
    order.sort_by(|a, b| b.1.total_cmp(&a.1));
    for &&(poll, share, _, tick) in order.iter().take(worst) {
        let Some((w, h, px)) = read_ppm(&frames.join(format!("tick-{tick:06}.ppm"))) else {
            continue;
        };
        let cycle = ticks.iter().find(|t| t.0 == tick).map_or(0, |t| t.1);
        let horizon = samples
            .iter()
            .rev()
            .find(|s| s.0 <= cycle)
            .map_or(0, |s| s.1);
        let start = (horizon + offset).clamp(0, h as i32) as usize;
        let mut image = RgbImage::new(w as u32, h as u32);
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 3;
                let mut p = [px[i], px[i + 1], px[i + 2]];
                if y >= start && px[i..i + 3] == BACKDROP {
                    p = [255, 0, 0];
                }
                image.put_pixel(x as u32, y as u32, Rgb(p));
            }
        }
        let file = out.join(format!("gap-poll{poll:05}.png"));
        image.save(&file).expect("write the frame");
        println!("worst: poll {poll} share {share:.4} -> {}", file.display());
    }
}

fn load(dir: &str) -> std::collections::BTreeMap<u64, (f32, bool)> {
    let text = std::fs::read_to_string(Path::new(dir).join("shares.csv")).expect("shares.csv");
    let mut map = std::collections::BTreeMap::new();
    for line in text.lines().skip(1) {
        let c: Vec<&str> = line.split(',').collect();
        if let (Some(Ok(poll)), Some(Ok(share)), Some(counted)) = (
            c.first().map(|s| s.parse::<u64>()),
            c.get(1).map(|s| s.parse::<f32>()),
            c.get(2),
        ) {
            map.entry(poll).or_insert((share, *counted == "1"));
        }
    }
    map
}

fn compare(before: &str, after: &str) {
    let (a, b) = (load(before), load(after));
    let (mut n, mut sum_a, mut sum_b) = (0usize, 0.0f32, 0.0f32);
    let (mut worse, mut better, mut over_a, mut over_b) = (0usize, 0usize, 0usize, 0usize);
    let mut changes: Vec<(f32, u64)> = Vec::new();
    for (poll, &(sa, counted_a)) in &a {
        let Some(&(sb, counted_b)) = b.get(poll) else {
            continue;
        };
        if !(counted_a && counted_b) {
            continue;
        }
        n += 1;
        sum_a += sa;
        sum_b += sb;
        over_a += usize::from(sa > 0.01);
        over_b += usize::from(sb > 0.01);
        worse += usize::from(sb > sa + 0.005);
        better += usize::from(sa > sb + 0.005);
        changes.push((sa - sb, *poll));
    }
    if n == 0 {
        println!("no paired frames");
        return;
    }
    println!(
        "{n} paired frames on the plain: mean uncovered share before {:.4}, after {:.4}; over 1 percent: before {over_a}, after {over_b}; after is better by more than half a percent in {better}, worse in {worse}",
        sum_a / n as f32,
        sum_b / n as f32
    );
    changes.sort_by(|x, y| y.0.total_cmp(&x.0));
    for (d, poll) in changes.iter().take(5) {
        println!("largest gain: poll {poll} by {d:.4}");
    }
}
