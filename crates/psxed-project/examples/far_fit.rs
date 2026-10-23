//! How well each way of showing the far level matches the level itself, along a played route.
//!
//! Usage: `far_fit <project.ron> <trace.log> <out dir> [every=N] [samples=N] [lattice=S] [real=R]`
//!
//! `trace.log` is a `card-trace` run's stderr: its `ct Q` lines give the camera of every frame.
//! For each sampled frame the whole level is rendered as the truth (every room), then each
//! design draws what lies beyond `real` sectors from the eye under the real rooms and the
//! composite is compared with the truth on the pixels the far level owns.
//!
//! Designs: `cards` (the game's room cards and layers as they are, ideal streaming), and the
//! lattice panels with a depth mesh, with one flat depth, and baked exactly at the eye.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use image::{Rgb, RgbImage};
use psx_grid::render::{outer_bands, view_toward, CARD_UNLIMITED};
use psxed_project::grid::cook::cards::{bake, bake_layers, LayerSpec};
use psxed_project::grid::cook::panels::{
    bake_panel, draw_panel, draw_textured_quad, Camera, DepthRule, Layer, Panel, PanelShape, World,
};
use psxed_project::playtest::{cook_for_cards, room_card_eye};
use psxed_project::ProjectDocument;

const SKY: [f32; 3] = [50.0, 58.0, 56.0];
const FOCAL: f32 = 305.0;

struct Pose {
    frame: u32,
    /// Bus cycles at the frame, from the trace line's header.
    cycles: u64,
    eye: [f32; 3],
    rot: [[i16; 3]; 3],
    /// Cards the game drew this frame: `(card index, view shown)`.
    drawn: Vec<(usize, usize)>,
}

fn parse_poses(path: &Path) -> Vec<Pose> {
    let text = std::fs::read_to_string(path).expect("read the trace");
    let mut out = Vec::new();
    for line in text.lines() {
        if let Some(at) = line.find("ct W ") {
            let v: Vec<usize> = line[at + 5..]
                .split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect();
            if let (Some(last), true) = (out.last_mut(), v.len() >= 2) {
                let last: &mut Pose = last;
                last.drawn.push((v[0], v[1]));
            }
            continue;
        }
        let Some(at) = line.find("ct Q ").or_else(|| line.find("pt Q ")) else {
            continue;
        };
        let frame: u32 = line
            .split("[guest f")
            .nth(1)
            .and_then(|s| s.split(' ').next())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let v: Vec<i32> = line[at + 5..]
            .split_whitespace()
            .filter_map(|n| n.parse::<i32>().ok())
            .map(|n| n - 32768)
            .collect();
        if v.len() < 12 {
            continue;
        }
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
            drawn: Vec::new(),
        });
    }
    out
}

/// The frames of a replay (`<dir>/frames/tick-NNNNNN.ppm`) and its route log, to find the
/// frame the game showed at a trace line's cycle count.
struct GameFrames {
    dir: PathBuf,
    /// `(bus cycles, route tick)` of every route log row.
    ticks: Vec<(u64, u32)>,
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
        Self {
            dir: dir.to_path_buf(),
            ticks,
        }
    }

    /// The frame nearest `cycles` (frames are saved every few ticks), as 320x240 RGB.
    fn frame_at(&self, cycles: u64) -> Option<Vec<[u8; 3]>> {
        let at = self
            .ticks
            .partition_point(|t| t.0 < cycles)
            .min(self.ticks.len() - 1);
        let tick = self.ticks[at].1;
        let mut best: Option<(u32, PathBuf)> = None;
        for entry in std::fs::read_dir(self.dir.join("frames")).ok()? {
            let path = entry.ok()?.path();
            let name = path.file_stem()?.to_str()?.to_string();
            let n: u32 = name.strip_prefix("tick-")?.parse().ok()?;
            if best
                .as_ref()
                .is_none_or(|b| n.abs_diff(tick) < b.0.abs_diff(tick))
            {
                best = Some((n, path));
            }
        }
        let bytes = std::fs::read(best?.1).ok()?;
        // P6 header: "P6\n320 240\n255\n".
        let mut at = 0;
        let mut fields = 0;
        while fields < 4 {
            while bytes.get(at)?.is_ascii_whitespace() {
                at += 1;
            }
            while !bytes.get(at)?.is_ascii_whitespace() {
                at += 1;
            }
            fields += 1;
        }
        at += 1;
        let data = bytes.get(at..)?;
        (data.len() >= 320 * 240 * 3).then(|| {
            data.chunks_exact(3)
                .take(320 * 240)
                .map(|c| [c[0], c[1], c[2]])
                .collect()
        })
    }
}

/// Whether a game pixel is the sky backdrop (olive) and not geometry (blue or grey).
fn is_backdrop(p: [u8; 3]) -> bool {
    p[1] >= p[2]
        && p[0] >= 30
        && p[1] >= 38
        && p[0] <= 64
        && p[1] <= 70
        && p[2] <= 64
        && p[1] >= p[0]
}

fn arg(args: &[String], key: &str) -> Option<f32> {
    args.iter()
        .find_map(|a| a.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
        .and_then(|v| v.parse().ok())
}

#[derive(Default, Clone, Copy)]
struct Score {
    pixels: usize,
    err: f64,
    bad: usize,
    holes: usize,
    extra: usize,
    /// Far pixels and holes by the distance of the room they show: 12-18, 18-24, 24-36, 36-48,
    /// 48 and more sectors.
    bucket_pixels: [usize; 5],
    bucket_holes: [usize; 5],
}

const BUCKETS: [&str; 5] = ["12-18", "18-24", "24-36", "36-48", "48+"];

fn bucket(sectors: f32) -> usize {
    match sectors {
        s if s < 18.0 => 0,
        s if s < 24.0 => 1,
        s if s < 36.0 => 2,
        s if s < 48.0 => 3,
        _ => 4,
    }
}

fn compare(
    truth: &psxed_project::grid::cook::panels::Picture,
    real: &psxed_project::grid::cook::panels::Picture,
    layer: &Layer,
    far_room: &dyn Fn(u16) -> bool,
    room_sectors: &dyn Fn(u16) -> f32,
    image: &mut RgbImage,
    ox: u32,
) -> Score {
    let mut score = Score::default();
    for y in 0..truth.height {
        for x in 0..truth.width {
            let at = y * truth.width + x;
            let t_cov = truth.coverage[at] >= 0.5;
            let t_rgb = if t_cov { truth.rgb[at] } else { SKY };
            let r_cov = real.coverage[at] >= 0.5;
            let sys = if r_cov {
                real.rgb[at]
            } else if layer.covered[at] {
                layer.rgb[at]
            } else {
                SKY
            };
            image.put_pixel(
                ox + x as u32,
                y as u32,
                Rgb(sys.map(|c| c.clamp(0.0, 255.0) as u8)),
            );
            let far_pixel = !t_cov || far_room(truth.room[at]);
            if !far_pixel || r_cov {
                continue;
            }
            score.pixels += 1;
            let e = (0..3).map(|i| (sys[i] - t_rgb[i]).abs()).sum::<f32>() / 3.0;
            score.err += f64::from(e);
            if e > 40.0 {
                score.bad += 1;
            }
            let b = if t_cov {
                bucket(room_sectors(truth.room[at]))
            } else {
                4
            };
            if t_cov {
                score.bucket_pixels[b] += 1;
            }
            if t_cov && !layer.covered[at] {
                score.holes += 1;
                score.bucket_holes[b] += 1;
            }
            if !t_cov && layer.covered[at] {
                score.extra += 1;
            }
        }
    }
    score
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!(
            "usage: far_fit <project.ron> <trace.log> <out dir> [samples=N] [lattice=S] [real=R]"
        );
        std::process::exit(2);
    }
    let path = Path::new(&args[0]);
    let text = std::fs::read_to_string(path).expect("read the project");
    let project = ProjectDocument::from_ron_str(&text).expect("parse the project");
    let root = path
        .parent()
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    let out = PathBuf::from(&args[2]);
    std::fs::create_dir_all(&out).expect("create the output directory");
    let samples = arg(&args, "samples").unwrap_or(40.0) as usize;
    let lattice = arg(&args, "lattice").unwrap_or(8.0);
    let real_sectors = arg(&args, "real").unwrap_or(12.0);
    let first_frame = arg(&args, "from").unwrap_or(0.0) as u32;
    let scene = project.active_scene();
    let grid = scene.world.grid().expect("a grid world");
    let inputs = cook_for_cards(&project, grid, &root).expect("cook the level");
    let (cooked, albedo) = (inputs.cooked, inputs.albedo);
    let sector = cooked.level.sector_units as f32;
    let real_r = real_sectors * sector;
    let textured = std::env::var_os("FAR_FIT_FLAT").is_none();
    let world = World::new(
        &cooked,
        &albedo,
        textured.then(|| psxed_project::playtest::panel_textures(&project, &root, &cooked)),
    );
    println!(
        "level: {} rooms, {} triangles, sector {sector}, real radius {real_r}",
        world.rooms(),
        world.triangles()
    );

    // The cards as the game bakes them.
    let flood = &grid.stream.flood;
    let sector_i = sector as i32;
    let eye_cfg = room_card_eye(flood, sector_i, inputs.spawn_floor_y);
    let (rooms_baked, _) = bake(&cooked, &albedo, Some(eye_cfg));
    let room_far = flood.card_far_sectors() * sector_i;
    let room_fade = 3 * sector_i;
    let bands = outer_bands(room_far, room_fade);
    let mut specs = Vec::new();
    let mut prev_far = room_far;
    for (band, layer) in bands.iter().zip(flood.card_layer_bands()) {
        specs.push(LayerSpec {
            near: prev_far,
            far: (band.far < CARD_UNLIMITED).then_some(band.far),
            cell: layer.cell * sector_i,
            depth: layer.depth.map(|d| d * sector_i),
        });
        prev_far = band.far;
    }
    let layer_cards = bake_layers(&cooked, &albedo, &rooms_baked, &specs);
    // Cards in the game's own index order: the rooms', then each layer's.
    let mut cards: Vec<Option<(u8, &psxed_project::grid::cook::cards::BakedCard)>> = rooms_baked
        .iter()
        .map(|c| c.as_ref().map(|c| (0u8, c)))
        .collect();
    for (k, layer) in layer_cards.iter().enumerate() {
        for card in layer {
            cards.push(Some((k as u8 + 1, card)));
        }
    }
    let layer_bands = [
        (0.0f32, room_far as f32 + room_fade as f32),
        (bands[0].near as f32, bands[0].far as f32),
        (bands[1].near as f32, 1.0e9),
    ];
    println!("cards: {} (bands {layer_bands:?})", cards.len());

    let mut poses = parse_poses(Path::new(&args[1]));
    poses.retain(|p| p.frame >= first_frame);
    println!("poses: {}", poses.len());
    let step = (poses.len() / samples.max(1)).max(1);
    let wanted_frames: Vec<u32> = std::env::var("FAR_FIT_FRAMES")
        .ok()
        .map(|v| v.split(',').filter_map(|n| n.parse().ok()).collect())
        .unwrap_or_default();
    let picked: Vec<&Pose> = if wanted_frames.is_empty() {
        poses.iter().step_by(step).take(samples).collect()
    } else {
        wanted_frames
            .iter()
            .filter_map(|&f| poses.iter().min_by_key(|p| p.frame.abs_diff(f)))
            .collect()
    };

    // Lattice vertices come into being where the route passes: on the lattice node nearest to
    // an eye, at that eye's height.
    let node = |eye: [f32; 3]| -> (i32, i32) {
        (
            (eye[0] / (lattice * sector)).round() as i32,
            (eye[2] / (lattice * sector)).round() as i32,
        )
    };
    let mut vertex_y: HashMap<(i32, i32), (f32, f32)> = HashMap::new();
    for pose in &poses {
        let n = node(pose.eye);
        let d = ((pose.eye[0] - n.0 as f32 * lattice * sector).powi(2)
            + (pose.eye[2] - n.1 as f32 * lattice * sector).powi(2))
        .sqrt();
        let e = vertex_y.entry(n).or_insert((pose.eye[1], f32::MAX));
        if d < e.1 {
            *e = (pose.eye[1], d);
        }
    }

    let shape = PanelShape {
        focal: FOCAL,
        tile_cols: 5,
        tile_rows: 7,
        horizon: 3.0 * 63.0,
        cells: [
            arg(&args, "cx").unwrap_or(2.0) as usize,
            arg(&args, "cy").unwrap_or(3.0) as usize,
        ],
    };
    let rule = match arg(&args, "rule").unwrap_or(1.0) as i32 {
        0 => DepthRule::Min,
        2 => DepthRule::Max,
        _ => DepthRule::Median,
    };
    const NAMES: [&str; 5] = ["game", "ideal", "mesh", "flat", "exact"];
    let mut panel_cache: HashMap<((i32, i32), usize, u8), Panel> = HashMap::new();
    let mut csv = String::from("frame,eye_x,eye_z,to_vertex_sectors,heading_err_deg");
    for n in NAMES {
        csv.push_str(&format!(",{n}_err,{n}_bad,{n}_holes"));
    }
    csv.push_str(",pixels\n");
    let mut totals = [Score::default(); 5];
    let mut density: [Vec<(f32, f32)>; 2] = [Vec::new(), Vec::new()];
    let game_frames = std::env::var_os("FAR_FIT_GAME").map(|d| GameFrames::open(Path::new(&d)));
    let mut game_totals = (0usize, 0usize, 0usize);
    let mut fill_total = (0usize, 0usize);
    for (index, pose) in picked.iter().enumerate() {
        let cam = Camera::game(pose.eye, pose.rot, FOCAL);
        let truth = world.render(&cam, 2, &|_| true).resolve(2);
        let near = |room: usize| {
            world.centres[room].is_some_and(|c| {
                ((c[0] - pose.eye[0]).powi(2) + (c[1] - pose.eye[2]).powi(2)).sqrt() < real_r
            })
        };
        let real = world.render(&cam, 2, &near).resolve(2);
        let far_room_of = |room: u16| room != u16::MAX && !near(usize::from(room));
        let far_room = &far_room_of;
        let room_sectors = |room: u16| {
            world.centres[usize::from(room)].map_or(0.0, |c| {
                ((c[0] - pose.eye[0]).powi(2) + (c[1] - pose.eye[2]).powi(2)).sqrt() / sector
            })
        };
        let mut image = RgbImage::new(320 * 6, 240);
        if let Some(game) = game_frames.as_ref() {
            if let Some(frame) = game.frame_at(pose.cycles) {
                let (mut seen, mut holes, mut extra) = (0usize, 0usize, 0usize);
                for y in 0..240usize {
                    for x in 0..320usize {
                        // The HUD's corner and the player's column are the game's own.
                        if (y < 64 && x < 150) || (y > 90 && (110..210).contains(&x)) {
                            continue;
                        }
                        let at = y * 320 + x;
                        let t_cov = truth.coverage[at] >= 0.5;
                        let far = !t_cov || far_room_of(truth.room[at]);
                        if !far || real.coverage[at] >= 0.5 {
                            continue;
                        }
                        seen += 1;
                        let sky = is_backdrop(frame[at]);
                        holes += usize::from(t_cov && sky);
                        extra += usize::from(!t_cov && !sky);
                    }
                }
                game_totals.0 += seen;
                game_totals.1 += holes;
                game_totals.2 += extra;
                println!(
                    "  GAMEFRAME f{} far pixels {seen} holes {:.1}% extra {:.1}%",
                    pose.frame,
                    100.0 * holes as f64 / seen.max(1) as f64,
                    100.0 * extra as f64 / seen.max(1) as f64
                );
            }
        }
        for y in 0..240 {
            for x in 0..320 {
                let at = y * 320 + x;
                let c = if truth.coverage[at] >= 0.5 {
                    truth.rgb[at]
                } else {
                    SKY
                };
                image.put_pixel(
                    x as u32,
                    y as u32,
                    Rgb(c.map(|v| v.clamp(0.0, 255.0) as u8)),
                );
            }
        }
        // Cards: the game's own (the cards and views it drew this frame) and the ideal ones
        // (every card in its band, the view nearest the eye).
        let draw_card =
            |layer: &mut Layer, card: &psxed_project::grid::cook::cards::BakedCard, view: usize| {
                let (dx, dz) = (
                    pose.eye[0] - card.centre[0] as f32,
                    pose.eye[2] - card.centre[1] as f32,
                );
                let len = (dx * dx + dz * dz).sqrt().max(1.0);
                let (rx, rz) = (dz / len, -dx / len);
                let (wx, wz) = (rx * card.half_width as f32, rz * card.half_width as f32);
                let (cx, cz) = (card.centre[0] as f32, card.centre[1] as f32);
                let (bottom, top) = (card.base as f32, (card.base + card.height) as f32);
                let corners = [
                    [cx - wx, top, cz - wz],
                    [cx + wx, top, cz + wz],
                    [cx + wx, bottom, cz + wz],
                    [cx - wx, bottom, cz - wz],
                ];
                let cams = corners.map(|p| cam.to_camera(p));
                if cams.iter().any(|c| c[2] < sector * 0.04) {
                    return;
                }
                let screen = cams.map(|c| cam.pixel(c));
                let tile = &card.preview[view * 64 * 63..(view + 1) * 64 * 63];
                let uv = [[0.0, 0.0], [64.0, 0.0], [64.0, 63.0], [0.0, 63.0]];
                draw_textured_quad(layer, screen, uv, tile, cams[0][2]);
            };
        let mut scores = [Score::default(); 5];
        // Rooms the truth shows beyond the real radius: which of them the game drew a card for.
        if std::env::var_os("FAR_FIT_ROOMS").is_some() {
            let mut seen: HashMap<u16, usize> = HashMap::new();
            for (at, &room) in truth.room.iter().enumerate() {
                if truth.coverage[at] >= 0.5 && far_room(room) {
                    *seen.entry(room).or_default() += 1;
                }
            }
            let mut seen: Vec<(u16, usize)> = seen.into_iter().filter(|e| e.1 >= 150).collect();
            seen.sort_by_key(|e| std::cmp::Reverse(e.1));
            for (room, pixels) in seen {
                let d = room_sectors(room);
                let card = cards[usize::from(room)].is_some();
                let l0 = pose.drawn.iter().any(|&(i, _)| i == usize::from(room));
                println!(
                    "  ROOM f{} room {room} dist {d:.1} pixels {pixels} card {card} L0drawn {l0}",
                    pose.frame
                );
            }
        }
        // The game's.
        let mut layer = Layer::new(320, 240);
        let mut order: Vec<(u8, f32, usize, usize)> = Vec::new();
        let mut heading_err = (0.0f32, 0usize);
        for &(i, view) in &pose.drawn {
            let Some((lk, card)) = cards.get(i).copied().flatten() else {
                continue;
            };
            let (dx, dz) = (
                pose.eye[0] - card.centre[0] as f32,
                pose.eye[2] - card.centre[1] as f32,
            );
            let d = (dx * dx + dz * dz).sqrt();
            order.push((lk, d, i, view));
            if lk == 0 {
                // The angle between the direction to the eye and the heading of the view.
                let a = dz.atan2(dx);
                let h = view as f32 * std::f32::consts::FRAC_PI_4;
                let mut e = (a - h).rem_euclid(std::f32::consts::TAU);
                if e > std::f32::consts::PI {
                    e -= std::f32::consts::TAU;
                }
                heading_err.0 += e.abs().to_degrees();
                heading_err.1 += 1;
            }
        }
        order.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.total_cmp(&a.1)));
        for (_, _, i, view) in order {
            if let Some((_, card)) = cards[i] {
                draw_card(&mut layer, card, view);
            }
        }
        density[0].extend(layer.density.iter().copied());
        scores[0] = compare(
            &truth,
            &real,
            &layer,
            far_room,
            &room_sectors,
            &mut image,
            320,
        );
        // Ideal.
        let mut layer = Layer::new(320, 240);
        let mut order: Vec<(u8, f32, usize)> = Vec::new();
        for (i, entry) in cards.iter().enumerate() {
            let Some((lk, card)) = entry else { continue };
            let (dx, dz) = (
                pose.eye[0] - card.centre[0] as f32,
                pose.eye[2] - card.centre[1] as f32,
            );
            let d = (dx * dx + dz * dz).sqrt();
            let band = layer_bands[usize::from(*lk)];
            let lo = if *lk == 0 { real_r } else { band.0 };
            if d < lo || d > band.1 {
                continue;
            }
            order.push((*lk, d, i));
        }
        order.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.total_cmp(&a.1)));
        if std::env::var_os("FAR_FIT_CARDS").is_some() {
            let count = |lk: u8| order.iter().filter(|o| o.0 == lk).count();
            let game = |lk: u8| {
                pose.drawn
                    .iter()
                    .filter(|&&(i, _)| cards.get(i).copied().flatten().is_some_and(|c| c.0 == lk))
                    .count()
            };
            if std::env::var_os("FAR_FIT_CARDS").is_some_and(|v| v == "2") {
                let fwd = [f32::from(pose.rot[2][0]), f32::from(pose.rot[2][2])];
                let fl = (fwd[0] * fwd[0] + fwd[1] * fwd[1]).sqrt().max(1.0);
                for &(lk, d, i) in order.iter().filter(|o| o.0 >= 1) {
                    let card = cards[i].expect("card").1;
                    let (ex, ez) = (
                        card.centre[0] as f32 - pose.eye[0],
                        card.centre[1] as f32 - pose.eye[2],
                    );
                    let ahead = (fwd[0] * ex + fwd[1] * ez) / fl;
                    let across = (-fwd[1] * ex + fwd[0] * ez) / fl;
                    let drawn = pose.drawn.iter().any(|&(j, _)| j == i);
                    println!(
                        "    L{lk} card {i} dist {:.1} sect ahead {:.1} across {:.1} halfw {:.1} drawn {drawn}",
                        d / sector, ahead / sector, across / sector, card.half_width as f32 / sector
                    );
                }
            }
            println!(
                "  f{} cards in band / drawn: L0 {}/{} L1 {}/{} L2 {}/{}",
                pose.frame,
                count(0),
                game(0),
                count(1),
                game(1),
                count(2),
                game(2)
            );
        }
        for (_, _, i) in order {
            if let Some((_, card)) = cards[i] {
                let (dx, dz) = (
                    pose.eye[0] - card.centre[0] as f32,
                    pose.eye[2] - card.centre[1] as f32,
                );
                draw_card(
                    &mut layer,
                    card,
                    usize::from(view_toward(dx as i32, dz as i32)),
                );
            }
        }
        scores[1] = compare(
            &truth,
            &real,
            &layer,
            far_room,
            &room_sectors,
            &mut image,
            640,
        );

        // Panels from the lattice vertex nearest the eye.
        let n = node(pose.eye);
        let (vy, _) = vertex_y.get(&n).copied().unwrap_or((pose.eye[1], 0.0));
        let vertex = [
            n.0 as f32 * lattice * sector,
            vy,
            n.1 as f32 * lattice * sector,
        ];
        let to_vertex =
            ((pose.eye[0] - vertex[0]).powi(2) + (pose.eye[2] - vertex[2]).powi(2)).sqrt();
        let exclude_r = (real_r - (0.75 * lattice * sector)).max(0.0);
        let centres = &world.centres;
        let far_of = |b: [f32; 3]| {
            let b = [b[0], b[2]];
            move |room: usize| {
                centres[room].is_some_and(|c| {
                    ((c[0] - b[0]).powi(2) + (c[1] - b[1]).powi(2)).sqrt() >= exclude_r
                })
            }
        };
        let forward = [f32::from(pose.rot[2][0]), f32::from(pose.rot[2][2])];
        let yaw = forward[1].atan2(forward[0]);
        let headings: Vec<usize> = (0..8)
            .filter(|&k| {
                let a = k as f32 * std::f32::consts::FRAC_PI_4;
                let mut d = (a - yaw).rem_euclid(std::f32::consts::TAU);
                if d > std::f32::consts::PI {
                    d -= std::f32::consts::TAU;
                }
                d.abs() < std::f32::consts::FRAC_PI_8 + 0.5
            })
            .collect();
        let band = (
            -std::f32::consts::FRAC_PI_8 - 0.03,
            std::f32::consts::FRAC_PI_8 + 0.03,
        );
        for (variant, flat, exact) in [(2usize, false, false), (3, true, false), (4, false, true)] {
            let mut layer = Layer::new(320, 240);
            let at = if exact { pose.eye } else { vertex };
            let key_node = if exact { (index as i32, i32::MAX) } else { n };
            for &k in &headings {
                let key = (key_node, k, 0u8);
                let panel = panel_cache.entry(key).or_insert_with(|| {
                    let keep = far_of(at);
                    bake_panel(&world, at, k, &shape, &keep, 2, rule)
                });
                let flat_depth = flat.then(|| {
                    let mut d: Vec<f32> = panel
                        .depth
                        .iter()
                        .copied()
                        .filter(|&v| v < 400.0 * sector)
                        .collect();
                    d.sort_by(f32::total_cmp);
                    d.get(d.len() / 2).copied().unwrap_or(30.0 * sector)
                });
                draw_panel(
                    &mut layer,
                    &cam,
                    panel,
                    Some(band),
                    flat_depth,
                    sector * 0.04,
                );
            }
            if variant == 2 {
                density[1].extend(layer.density.iter().copied());
            }
            if variant == 2 {
                let covered = layer.covered.iter().filter(|&&c| c).count();
                let hidden = (0..320 * 240)
                    .filter(|&at| layer.covered[at] && real.coverage[at] >= 0.5)
                    .count();
                fill_total.0 += covered;
                fill_total.1 += hidden;
            }
            scores[variant] = compare(
                &truth,
                &real,
                &layer,
                far_room,
                &room_sectors,
                &mut image,
                320 * (variant as u32 + 1),
            );
        }
        let fmt = |s: &Score| {
            if s.pixels == 0 {
                (0.0, 0.0, 0.0)
            } else {
                (
                    s.err / s.pixels as f64,
                    s.bad as f64 / s.pixels as f64,
                    s.holes as f64 / s.pixels as f64,
                )
            }
        };
        let herr = if heading_err.1 > 0 {
            heading_err.0 / heading_err.1 as f32
        } else {
            0.0
        };
        csv.push_str(&format!(
            "{},{:.0},{:.0},{:.2},{:.1}",
            pose.frame,
            pose.eye[0],
            pose.eye[2],
            to_vertex / sector,
            herr
        ));
        let mut line = format!(
            "f{:5} eye {:5.0} {:5.0} vtx {:4.1} sect hdg {:4.1}deg |",
            pose.frame,
            pose.eye[0],
            pose.eye[2],
            to_vertex / sector,
            herr
        );
        for (name, s) in NAMES.iter().zip(&scores) {
            let (e, b, h) = fmt(s);
            csv.push_str(&format!(",{e:.1},{b:.3},{h:.3}"));
            line.push_str(&format!(" {name} {e:5.1}/{b:4.2}/{h:4.2}"));
        }
        csv.push_str(&format!(",{}\n", scores[0].pixels));
        println!("{line}  px {}", scores[0].pixels);
        for i in 0..5 {
            totals[i].pixels += scores[i].pixels;
            totals[i].err += scores[i].err;
            totals[i].bad += scores[i].bad;
            totals[i].holes += scores[i].holes;
            totals[i].extra += scores[i].extra;
            for b in 0..5 {
                totals[i].bucket_pixels[b] += scores[i].bucket_pixels[b];
                totals[i].bucket_holes[b] += scores[i].bucket_holes[b];
            }
        }
        image
            .save(out.join(format!("sample-{:04}-f{}.png", index, pose.frame)))
            .expect("save");
    }
    std::fs::write(out.join("fit.csv"), csv).expect("write csv");
    if game_frames.is_some() {
        println!(
            "GAME FRAMES: {} far pixels, holes (truth geometry, game backdrop) {:.2}%, extra {:.2}%",
            game_totals.0,
            100.0 * game_totals.1 as f64 / game_totals.0.max(1) as f64,
            100.0 * game_totals.2 as f64 / game_totals.0.max(1) as f64
        );
    }
    println!(
        "PANEL FILL: {} pixels covered by panel texels, {:.1}% of them hidden behind the rooms drawn whole",
        fill_total.0,
        100.0 * fill_total.1 as f64 / fill_total.0.max(1) as f64
    );
    // Screen pixels per texel (median) by depth, for the game's cards and the panels.
    for (name, list) in ["cards", "panels"].iter().zip(&density) {
        let mut line = format!("PX/TEXEL {name:7}");
        for (lo, hi) in [(12.0, 16.0), (16.0, 24.0), (24.0, 36.0), (36.0, 100.0)] {
            let mut v: Vec<f32> = list
                .iter()
                .filter(|(d, _)| *d / sector >= lo && *d / sector < hi)
                .map(|e| e.1)
                .collect();
            v.sort_by(f32::total_cmp);
            line.push_str(&format!(
                " | {lo:.0}-{hi:.0}: {} (n {})",
                v.get(v.len() / 2).map_or("-".into(), |m| format!("{m:.2}")),
                v.len()
            ));
        }
        println!("{line}");
    }
    for (name, s) in NAMES.iter().zip(totals) {
        let holes: Vec<String> = (0..5)
            .map(|b| {
                format!(
                    "{} {:.1}% of {}",
                    BUCKETS[b],
                    100.0 * s.bucket_holes[b] as f64 / s.bucket_pixels[b].max(1) as f64,
                    s.bucket_pixels[b]
                )
            })
            .collect();
        println!("HOLES {name:6} {}", holes.join(" | "));
        println!(
            "TOTAL {name:6} mean err {:.2}  bad {:.3}  holes {:.3}  extra {:.3}  ({} px)",
            s.err / s.pixels.max(1) as f64,
            s.bad as f64 / s.pixels.max(1) as f64,
            s.holes as f64 / s.pixels.max(1) as f64,
            s.extra as f64 / s.pixels.max(1) as f64,
            s.pixels
        );
    }
}
