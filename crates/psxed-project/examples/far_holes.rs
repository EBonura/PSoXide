//! Where the game's far view is missing, found against the level itself.
//!
//! Usage: `far_holes <project.ron> <trace.log> <replay dir> <out dir> [every=N] [from=FRAME] [real=SECTORS]`
//!
//! `trace.log` is a `card-trace` run's stderr (its `pt Q` lines give the camera and the bake
//! eye in use of every frame); `<replay dir>` holds `route.csv` and `frames/tick-NNNNNN.ppm` of
//! the same run. For each sampled frame the whole level is rendered as the truth, and every
//! pixel the far view owns (beyond `real` sectors from the eye, or sky in the truth) is
//! compared with the game's frame: a *hole* is a pixel where the truth shows the level and the
//! game shows the sky backdrop, an *extra* one the other way round. One line a frame, and a
//! `truth | game | holes` image for the worst frames.

use std::path::{Path, PathBuf};

use image::{Rgb, RgbImage};
use psxed_project::grid::cook::panels::{Camera, World};
use psxed_project::playtest::{cook_for_cards, panel_textures};
use psxed_project::ProjectDocument;

const SKY: [f32; 3] = [50.0, 58.0, 56.0];
const FOCAL: f32 = 305.0;

struct Pose {
    frame: u32,
    cycles: u64,
    eye: [f32; 3],
    rot: [[i16; 3]; 3],
    bake_eye: u32,
}

fn parse_poses(path: &Path) -> Vec<Pose> {
    let text = std::fs::read_to_string(path).expect("read the trace");
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(at) = line.find("pt Q ") else {
            continue;
        };
        let frame: u32 = line
            .split("[guest f")
            .nth(1)
            .and_then(|s| s.split(' ').next())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let raw: Vec<i64> = line[at + 5..]
            .split_whitespace()
            .filter_map(|n| n.parse::<i64>().ok())
            .collect();
        if raw.len() < 13 {
            continue;
        }
        let v: Vec<i64> = raw[..12].iter().map(|n| n - 32768).collect();
        let cycles: u64 = line
            .split(" c")
            .nth(1)
            .and_then(|s| s.split(']').next())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        out.push(Pose {
            frame,
            cycles,
            eye: [v[0] as f32, v[1] as f32, v[2] as f32],
            rot: [
                [v[3] as i16, v[4] as i16, v[5] as i16],
                [v[6] as i16, v[7] as i16, v[8] as i16],
                [v[9] as i16, v[10] as i16, v[11] as i16],
            ],
            bake_eye: raw[12] as u32,
        });
    }
    out
}

/// The camera of every presented frame of a route log that watched `PANEL_POSE` (eight words from
/// column `col`: the eye, then the rotation packed in 16 bits), taken `lag` presented frames
/// before the one on screen: the frame shown at a flip was drawn a little earlier.
fn poses_from_csv(path: &Path, col: usize, lag: usize) -> Vec<Pose> {
    let text = std::fs::read_to_string(path).expect("read the route log");
    let mut rows: Vec<(u32, u64, Pose)> = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        let word = |k: usize| f.get(col + k).and_then(|v| v.parse::<u32>().ok());
        let (Some(tick), Some(cycles)) = (
            f.first().and_then(|v| v.parse::<u32>().ok()),
            f.get(3).and_then(|v| v.parse::<u64>().ok()),
        ) else {
            continue;
        };
        let flipped = f.get(10).is_some_and(|v| *v != "0");
        let (Some(x), Some(y), Some(z)) = (word(0), word(1), word(2)) else {
            continue;
        };
        let mut r = [0i16; 9];
        for k in 0..4 {
            let Some(w) = word(3 + k) else { break };
            r[2 * k] = w as u16 as i16;
            r[2 * k + 1] = (w >> 16) as u16 as i16;
        }
        r[8] = word(7).unwrap_or(0) as u16 as i16;
        if !flipped || (x | y | z) == 0 {
            continue;
        }
        rows.push((
            tick,
            cycles,
            Pose {
                frame: tick,
                cycles,
                eye: [x as i32 as f32, y as i32 as f32, z as i32 as f32],
                rot: [[r[0], r[1], r[2]], [r[3], r[4], r[5]], [r[6], r[7], r[8]]],
                bake_eye: 0,
            },
        ));
    }
    (lag..rows.len())
        .map(|k| {
            let p = &rows[k - lag].2;
            Pose {
                frame: rows[k].0,
                cycles: rows[k].1,
                eye: p.eye,
                rot: p.rot,
                bake_eye: 0,
            }
        })
        .collect()
}

struct GameFrames {
    dir: PathBuf,
    /// `(bus cycles, route tick)` of every route log row.
    ticks: Vec<(u64, u32)>,
    /// The ticks that have a frame saved, ascending.
    saved: Vec<u32>,
}

impl GameFrames {
    fn open(dir: &Path) -> Self {
        let text = std::fs::read_to_string(dir.join("route.csv")).expect("route.csv");
        let ticks = text
            .lines()
            .skip(1)
            .filter_map(|l| {
                let f: Vec<&str> = l.split(',').collect();
                Some((f.get(3)?.parse().ok()?, f.first()?.parse().ok()?))
            })
            .collect();
        let mut saved: Vec<u32> = std::fs::read_dir(dir.join("frames"))
            .expect("frames")
            .filter_map(|e| {
                let name = e.ok()?.path().file_stem()?.to_str()?.to_string();
                name.strip_prefix("tick-")?.parse().ok()
            })
            .collect();
        saved.sort_unstable();
        Self {
            dir: dir.to_path_buf(),
            ticks,
            saved,
        }
    }

    /// The saved frame nearest the tick the game was at `cycles`, as 320x240 RGB.
    fn frame_at(&self, cycles: u64) -> Option<(u32, Vec<[u8; 3]>)> {
        let at = self
            .ticks
            .partition_point(|t| t.0 < cycles)
            .min(self.ticks.len() - 1);
        let tick = self.ticks[at].1;
        let k = self.saved.partition_point(|&n| n < tick).min(self.saved.len() - 1);
        let n = self.saved[k];
        let bytes = std::fs::read(self.dir.join(format!("frames/tick-{n:06}.ppm"))).ok()?;
        let mut at = 0;
        for _ in 0..4 {
            while bytes.get(at)?.is_ascii_whitespace() {
                at += 1;
            }
            while !bytes.get(at)?.is_ascii_whitespace() {
                at += 1;
            }
        }
        at += 1;
        let data = bytes.get(at..)?;
        (data.len() >= 320 * 240 * 3).then(|| {
            (
                n,
                data.chunks_exact(3)
                    .take(320 * 240)
                    .map(|c| [c[0], c[1], c[2]])
                    .collect(),
            )
        })
    }
}

/// Whether a game pixel is the sky backdrop (olive) and not geometry (blue or grey).
fn is_backdrop(p: [u8; 3]) -> bool {
    p[1] >= p[2] && p[0] >= 30 && p[1] >= 38 && p[0] <= 64 && p[1] <= 70 && p[2] <= 64 && p[1] >= p[0]
}

fn arg(args: &[String], key: &str) -> Option<f32> {
    args.iter()
        .find_map(|a| a.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
        .and_then(|v| v.parse().ok())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 4 {
        eprintln!("usage: far_holes <project.ron> <trace.log> <replay dir> <out dir> [every=N] [from=FRAME] [real=S]");
        std::process::exit(2);
    }
    let path = Path::new(&args[0]);
    let text = std::fs::read_to_string(path).expect("read the project");
    let project = ProjectDocument::from_ron_str(&text).expect("parse the project");
    let root = path.parent().map_or_else(|| PathBuf::from("."), PathBuf::from);
    let out = PathBuf::from(&args[3]);
    std::fs::create_dir_all(&out).expect("create the output directory");
    let every = arg(&args, "every").unwrap_or(10.0) as usize;
    let from = arg(&args, "from").unwrap_or(0.0) as u32;
    let real_sectors = arg(&args, "real").unwrap_or(12.0);
    let worst = arg(&args, "worst").unwrap_or(12.0) as usize;
    let scene = project.active_scene();
    let grid = scene.world.grid().expect("a grid world");
    let inputs = cook_for_cards(&project, grid, &root).expect("cook the level");
    let (cooked, albedo) = (inputs.cooked, inputs.albedo);
    let sector = cooked.level.sector_units as f32;
    let real_r = real_sectors * sector;
    let world = World::new(&cooked, &albedo, Some(panel_textures(&project, &root, &cooked)));
    println!("level: {} rooms, {} triangles, sector {sector}", world.rooms(), world.triangles());
    // The bake eyes, to say which hole pixels lie in rooms the eye in use leaves out of its
    // panels (the rooms the game draws whole from anywhere within reach of it).
    let mut scaled = project.clone();
    psxed_project::units::scale_project_to_engine_units(&mut scaled);
    let sgrid = scaled.active_scene().world.grid().expect("a grid world");
    let eyes: Vec<[f32; 3]> = psxed_project::playtest::panel_eyes(&scaled, sgrid, &cooked, 10)
        .iter()
        .map(|e| e.eye)
        .collect();
    let box_distance = |room: usize, p: [f32; 3]| -> f32 {
        world.room_bounds(room).map_or(f32::MAX, |(lo, hi)| {
            let dx = (lo[0] - p[0]).max(0.0).max(p[0] - hi[0]);
            let dz = (lo[2] - p[2]).max(0.0).max(p[2] - hi[2]);
            (dx * dx + dz * dz).sqrt() / sector
        })
    };
    let mut classes = [0usize; 4];
    let game = GameFrames::open(Path::new(&args[2]));
    let mut poses = match arg(&args, "pose_col") {
        Some(col) => poses_from_csv(
            Path::new(&args[1]),
            col as usize,
            arg(&args, "lag").unwrap_or(1.0) as usize,
        ),
        None => parse_poses(Path::new(&args[1])),
    };
    poses.retain(|p| p.frame >= from);
    println!("poses: {}", poses.len());
    let mut results: Vec<(f64, u32, u32, RgbImage)> = Vec::new();
    let (mut tot_seen, mut tot_holes, mut tot_extra) = (0usize, 0usize, 0usize);
    for pose in poses.iter().step_by(every.max(1)) {
        let Some((tick, frame)) = game.frame_at(pose.cycles) else {
            continue;
        };
        let cam = Camera::game(pose.eye, pose.rot, FOCAL);
        let truth = world.render(&cam, 2, &|_| true).resolve(2);
        let near = |room: usize| {
            world.centres[room].is_some_and(|c| {
                ((c[0] - pose.eye[0]).powi(2) + (c[1] - pose.eye[2]).powi(2)).sqrt() < real_r
            })
        };
        let real = world.render(&cam, 2, &near).resolve(2);
        let mut hole_rooms: std::collections::HashMap<u16, usize> = std::collections::HashMap::new();
        let mut image = RgbImage::new(320 * 3, 240);
        let (mut seen, mut holes, mut extra) = (0usize, 0usize, 0usize);
        for y in 0..240usize {
            for x in 0..320usize {
                let at = y * 320 + x;
                let t_cov = truth.coverage[at] >= 0.5;
                let t_rgb = if t_cov { truth.rgb[at] } else { SKY };
                image.put_pixel(x as u32, y as u32, Rgb(t_rgb.map(|c| c.clamp(0.0, 255.0) as u8)));
                image.put_pixel(320 + x as u32, y as u32, Rgb(frame[at]));
                // The HUD's corner and the player's column are the game's own.
                if (y < 64 && x < 150) || (y > 90 && (110..210).contains(&x)) {
                    continue;
                }
                let far = !t_cov || (truth.room[at] != u16::MAX && !near(usize::from(truth.room[at])));
                if !far || real.coverage[at] >= 0.5 {
                    continue;
                }
                seen += 1;
                let sky = is_backdrop(frame[at]);
                let hole = t_cov && sky;
                let spare = !t_cov && !sky;
                holes += usize::from(hole);
                if hole {
                    *hole_rooms.entry(truth.room[at]).or_default() += 1;
                }
                if hole && truth.room[at] != u16::MAX {
                    let room = usize::from(truth.room[at]);
                    let at_eye = eyes
                        .iter()
                        .copied()
                        .min_by(|a, b| {
                            let d = |e: &[f32; 3]| (0..3).map(|k| (e[k] - pose.eye[k]).powi(2)).sum::<f32>();
                            d(a).total_cmp(&d(b))
                        })
                        .unwrap_or(pose.eye);
                    let class = match (box_distance(room, at_eye) <= 3.44, box_distance(room, pose.eye) <= 12.0) {
                        (true, true) => 0,
                        (true, false) => 1,
                        (false, true) => 2,
                        (false, false) => 3,
                    };
                    classes[class] += 1;
                }
                extra += usize::from(spare);
                let mark = if hole {
                    [255, 0, 0]
                } else if spare {
                    [0, 160, 255]
                } else {
                    [frame[at][0] / 4, frame[at][1] / 4, frame[at][2] / 4]
                };
                image.put_pixel(640 + x as u32, y as u32, Rgb(mark));
            }
        }
        tot_seen += seen;
        tot_holes += holes;
        tot_extra += extra;
        let pct = 100.0 * holes as f64 / seen.max(1) as f64;
        if pct > 20.0 {
            let mut v: Vec<_> = hole_rooms.iter().collect();
            v.sort_by_key(|e| std::cmp::Reverse(*e.1));
            let top: Vec<String> = v.iter().take(4).map(|(r, n)| format!("room {r}: {n}")).collect();
            println!("  HOLEROOMS f{} {}", pose.frame, top.join(", "));
        }
        println!(
            "F f{} tick {tick} eye {} far {seen} holes {pct:.1}% extra {:.1}%",
            pose.frame,
            pose.bake_eye,
            100.0 * extra as f64 / seen.max(1) as f64
        );
        results.push((pct, pose.frame, tick, image));
    }
    println!(
        "HOLE rooms: left out of the eye's panels and within 12 sectors of the camera {}, left out and farther {}, not left out but within 12 {}, neither {}",
        classes[0], classes[1], classes[2], classes[3]
    );
    println!(
        "TOTAL far pixels {tot_seen} holes {:.2}% extra {:.2}%",
        100.0 * tot_holes as f64 / tot_seen.max(1) as f64,
        100.0 * tot_extra as f64 / tot_seen.max(1) as f64
    );
    results.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (pct, frame, tick, image) in results.into_iter().take(worst) {
        let name = out.join(format!("worst-f{frame}-t{tick}-{pct:.0}.png"));
        image.save(&name).expect("save");
    }
}
