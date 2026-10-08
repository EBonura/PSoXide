//! Compare two `anim_dump` directories clip by clip: worst vertex displacement
//! in model units, bytes before/after, and optional frame strips (before,
//! after, difference) rendered with a software rasteriser.
//!
//! Usage: anim_compare <dump-before> <dump-after> [strip-out-dir] [clip-substring ...]

use std::path::{Path, PathBuf};

use image::{Rgb, RgbImage};
use psx_asset::{Animation, JointPose, Model};

type V3 = [f64; 3];

fn xform(p: &JointPose, v: V3) -> V3 {
    let m = p.matrix;
    let f =
        |r: usize| (m[r][0] as f64 * v[0] + m[r][1] as f64 * v[1] + m[r][2] as f64 * v[2]) / 4096.0;
    [
        f(0) + p.translation.x as f64,
        f(1) + p.translation.y as f64,
        f(2) + p.translation.z as f64,
    ]
}

fn posed(model: &Model<'_>, anim: &Animation<'_>, frame: u16) -> Vec<V3> {
    let poses: Vec<JointPose> = (0..anim.joint_count())
        .map(|j| anim.pose(frame, j).unwrap())
        .collect();
    (0..model.vertex_count())
        .map(|i| {
            let v = model.vertex(i).unwrap();
            let pos = [
                v.position.x as f64,
                v.position.y as f64,
                v.position.z as f64,
            ];
            // Find the part joint for this vertex.
            let mut joint = 0usize;
            for p in 0..model.part_count() {
                let part = model.part(p).unwrap();
                if i >= part.first_vertex() && i < part.first_vertex() + part.vertex_count() {
                    joint = part.joint_index() as usize;
                    break;
                }
            }
            let a = xform(&poses[joint.min(poses.len() - 1)], pos);
            if v.is_blend() && (v.joint1 as usize) < poses.len() {
                let b = xform(&poses[v.joint1 as usize], pos);
                let w = v.blend as f64 / 255.0;
                [
                    a[0] + (b[0] - a[0]) * w,
                    a[1] + (b[1] - a[1]) * w,
                    a[2] + (b[2] - a[2]) * w,
                ]
            } else {
                a
            }
        })
        .collect()
}

fn dist(a: V3, b: V3) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// Flat-shaded silhouette of a posed model, front view (x right, y up).
fn render(model: &Model<'_>, pts: &[V3], w: u32, h: u32, scale: f64, cx: f64, cy: f64) -> RgbImage {
    let mut img = RgbImage::from_pixel(w, h, Rgb([24, 24, 28]));
    let mut tris: Vec<(f64, [V3; 3])> = vec![];
    for f in 0..model.face_count() {
        let face = model.face(f).unwrap();
        let t = [
            pts[face.corners[0].vertex_index as usize],
            pts[face.corners[1].vertex_index as usize],
            pts[face.corners[2].vertex_index as usize],
        ];
        tris.push(((t[0][2] + t[1][2] + t[2][2]) / 3.0, t));
    }
    tris.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    for (_, t) in tris {
        let n = {
            let u = [t[1][0] - t[0][0], t[1][1] - t[0][1], t[1][2] - t[0][2]];
            let v = [t[2][0] - t[0][0], t[2][1] - t[0][1], t[2][2] - t[0][2]];
            let c = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            let l = (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt().max(1e-9);
            [c[0] / l, c[1] / l, c[2] / l]
        };
        let shade = (0.35 + 0.65 * n[2].abs()).clamp(0.0, 1.0);
        let col = Rgb([
            (200.0 * shade) as u8,
            (190.0 * shade) as u8,
            (170.0 * shade) as u8,
        ]);
        let sp: Vec<(f64, f64)> = t
            .iter()
            .map(|p| {
                (
                    (p[0] - cx) * scale + w as f64 / 2.0,
                    h as f64 / 2.0 - (p[1] - cy) * scale,
                )
            })
            .collect();
        let (minx, maxx) = (
            sp.iter()
                .map(|p| p.0)
                .fold(f64::MAX, f64::min)
                .floor()
                .max(0.0) as i32,
            sp.iter()
                .map(|p| p.0)
                .fold(f64::MIN, f64::max)
                .ceil()
                .min(w as f64 - 1.0) as i32,
        );
        let (miny, maxy) = (
            sp.iter()
                .map(|p| p.1)
                .fold(f64::MAX, f64::min)
                .floor()
                .max(0.0) as i32,
            sp.iter()
                .map(|p| p.1)
                .fold(f64::MIN, f64::max)
                .ceil()
                .min(h as f64 - 1.0) as i32,
        );
        let edge = |a: (f64, f64), b: (f64, f64), p: (f64, f64)| {
            (p.0 - a.0) * (b.1 - a.1) - (p.1 - a.1) * (b.0 - a.0)
        };
        let area = edge(sp[0], sp[1], sp[2]);
        if area.abs() < 1e-9 {
            continue;
        }
        for y in miny..=maxy {
            for x in minx..=maxx {
                let p = (x as f64 + 0.5, y as f64 + 0.5);
                let (w0, w1, w2) = (
                    edge(sp[1], sp[2], p),
                    edge(sp[2], sp[0], p),
                    edge(sp[0], sp[1], p),
                );
                let inside = if area > 0.0 {
                    w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0
                } else {
                    w0 <= 0.0 && w1 <= 0.0 && w2 <= 0.0
                };
                if inside {
                    img.put_pixel(x as u32, y as u32, col);
                }
            }
        }
    }
    img
}

fn clips(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = vec![];
    for m in std::fs::read_dir(dir).unwrap().flatten() {
        if !m.path().is_dir() {
            continue;
        }
        for c in std::fs::read_dir(m.path()).unwrap().flatten() {
            if c.path().extension().is_some_and(|e| e == "psxanim") {
                out.push((
                    format!(
                        "{}/{}",
                        m.file_name().to_string_lossy(),
                        c.file_name().to_string_lossy()
                    ),
                    c.path(),
                ));
            }
        }
    }
    out.sort();
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (da, db) = (PathBuf::from(&args[1]), PathBuf::from(&args[2]));
    let strip_dir = args.get(3).map(PathBuf::from);
    let filters: Vec<&String> = args.iter().skip(4).collect();
    if let Some(d) = &strip_dir {
        std::fs::create_dir_all(d).unwrap();
    }
    let (mut tot_a, mut tot_b) = (0usize, 0usize);
    let mut worst_overall: f64 = 0.0;
    println!(
        "{:<62} {:>7} {:>7} {:>6} {:>8} {:>8}",
        "clip", "before", "after", "ver", "maxerr", "changed"
    );
    for (name, pa) in clips(&da) {
        let pb = db.join(&name);
        let (ba, bb) = (std::fs::read(&pa).unwrap(), std::fs::read(&pb).unwrap());
        let (a, b) = (
            Animation::from_bytes(&ba).unwrap(),
            Animation::from_bytes(&bb).unwrap(),
        );
        let mdir = pa.parent().unwrap();
        let mbytes = std::fs::read(mdir.join("model.psxmdl")).unwrap();
        let model = Model::from_bytes(&mbytes).unwrap();
        tot_a += ba.len().next_multiple_of(4);
        tot_b += bb.len().next_multiple_of(4);
        assert_eq!(
            (a.frame_count(), a.joint_count()),
            (b.frame_count(), b.joint_count()),
            "{name}"
        );
        // Per joint vertex sets for the displacement metric.
        let mut per_joint: Vec<Vec<V3>> = vec![vec![]; model.joint_count() as usize];
        for p in 0..model.part_count() {
            let part = model.part(p).unwrap();
            for v in part.first_vertex()..part.first_vertex() + part.vertex_count() {
                let vert = model.vertex(v).unwrap();
                let pos = [
                    vert.position.x as f64,
                    vert.position.y as f64,
                    vert.position.z as f64,
                ];
                per_joint[part.joint_index() as usize].push(pos);
                if vert.is_blend() && (vert.joint1 as usize) < per_joint.len() {
                    per_joint[vert.joint1 as usize].push(pos);
                }
            }
        }
        let (mut worst, mut changed) = (0.0f64, 0usize);
        let mut worst_frame = 0u16;
        for f in 0..a.frame_count() {
            for j in 0..a.joint_count() {
                let (pa_, pb_) = (a.pose(f, j).unwrap(), b.pose(f, j).unwrap());
                if pa_ != pb_ {
                    changed += 1;
                }
                for &v in &per_joint[j as usize] {
                    let d = dist(xform(&pa_, v), xform(&pb_, v));
                    if d > worst {
                        worst = d;
                        worst_frame = f;
                    }
                }
            }
        }
        worst_overall = worst_overall.max(worst);
        println!(
            "{:<62} {:>7} {:>7} v{}>v{} {:>8.2} {:>8}",
            name,
            ba.len(),
            bb.len(),
            u16::from_le_bytes([ba[4], ba[5]]),
            u16::from_le_bytes([bb[4], bb[5]]),
            worst,
            changed
        );
        if let Some(out) = &strip_dir {
            if !filters.is_empty() && !filters.iter().any(|f| name.contains(f.as_str())) {
                continue;
            }
            let n = a.frame_count();
            let picks: Vec<u16> = {
                let mut v = vec![
                    0,
                    n / 4,
                    n / 2,
                    (3 * n) / 4,
                    n.saturating_sub(1),
                    worst_frame,
                ];
                v.sort();
                v.dedup();
                v
            };
            let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
            for v in 0..model.vertex_count() {
                let p = model.vertex(v).unwrap().position;
                for (k, c) in [p.x, p.y, p.z].into_iter().enumerate() {
                    lo[k] = lo[k].min(c as f64);
                    hi[k] = hi[k].max(c as f64);
                }
            }
            let (cx, cy) = ((lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0);
            let (w, h) = (260u32, 420u32);
            let scale =
                (h as f64 * 0.92 / (hi[1] - lo[1])).min(w as f64 * 0.92 / ((hi[0] - lo[0]) * 1.5));
            let mut sheet = RgbImage::new(w * picks.len() as u32, h * 3);
            for (i, &fr) in picks.iter().enumerate() {
                let pa_pts = posed(&model, &a, fr);
                let pb_pts = posed(&model, &b, fr);
                let ia = render(&model, &pa_pts, w, h, scale, cx, cy);
                let ib = render(&model, &pb_pts, w, h, scale, cx, cy);
                let mut id = RgbImage::from_pixel(w, h, Rgb([0, 0, 0]));
                for y in 0..h {
                    for x in 0..w {
                        let (pa1, pb1) = (ia.get_pixel(x, y), ib.get_pixel(x, y));
                        let d: i32 = (0..3).map(|k| (pa1[k] as i32 - pb1[k] as i32).abs()).sum();
                        id.put_pixel(
                            x,
                            y,
                            if d > 36 {
                                Rgb([255, 40, 40])
                            } else {
                                Rgb([pa1[0] / 4, pa1[1] / 4, pa1[2] / 4])
                            },
                        );
                    }
                }
                for y in 0..h {
                    for x in 0..w {
                        sheet.put_pixel(w * i as u32 + x, y, *ia.get_pixel(x, y));
                        sheet.put_pixel(w * i as u32 + x, h + y, *ib.get_pixel(x, y));
                        sheet.put_pixel(w * i as u32 + x, 2 * h + y, *id.get_pixel(x, y));
                    }
                }
            }
            let file = out.join(format!(
                "{}.png",
                name.replace('/', "__").replace(".psxanim", "")
            ));
            sheet.save(&file).unwrap();
        }
    }
    println!("TOTAL before={tot_a} after={tot_b} saved={} worst_vertex_error={worst_overall:.2} model units", tot_a as i64 - tot_b as i64);
}
