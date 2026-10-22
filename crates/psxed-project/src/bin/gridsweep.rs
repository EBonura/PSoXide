//! Look for holes in the faces of a grid world by casting rays.
//!
//! Usage: `gridsweep <project dir> [out tsv]`, or
//! `gridsweep <project dir> pose ex ey ez tx ty tz` to classify what one
//! camera sees (see `print_pose`).
//!
//! From a point above every floor, rays go out in a fan of directions. A ray
//! that meets the BACK of a one-sided face before any front face went through
//! a gap: it entered a solid the surface should have closed. The census counts
//! those rays and groups them by the sector of the face they land on.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::process::ExitCode;

use psxed_project::grid::faces::{all_faces_with, GridFace};
use psxed_project::ProjectDocument;

type V3 = [f64; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Faces bucketed by the sector columns their bounding box covers.
struct Index<'a> {
    faces: &'a [GridFace],
    cells: HashMap<[i32; 2], Vec<u32>>,
    sector: f64,
}

impl<'a> Index<'a> {
    fn new(faces: &'a [GridFace], sector: f64) -> Self {
        let mut cells: HashMap<[i32; 2], Vec<u32>> = HashMap::new();
        for (i, f) in faces.iter().enumerate() {
            let xs = f.points.iter().map(|p| p[0]);
            let zs = f.points.iter().map(|p| p[2]);
            let (x0, x1) = (xs.clone().min().unwrap_or(0), xs.max().unwrap_or(0));
            let (z0, z1) = (zs.clone().min().unwrap_or(0), zs.max().unwrap_or(0));
            let cell = |v: i32| (f64::from(v) / sector).floor() as i32;
            for cz in cell(z0)..=cell(z1) {
                for cx in cell(x0)..=cell(x1) {
                    cells.entry([cx, cz]).or_default().push(i as u32);
                }
            }
        }
        Self {
            faces,
            cells,
            sector,
        }
    }

    /// The nearest face a ray meets: its index, distance and whether it was
    /// hit from the front. Double-sided faces count as front.
    fn cast(&self, eye: V3, dir: V3, limit: f64) -> Option<(usize, f64, bool)> {
        let s = self.sector;
        let mut cell = [(eye[0] / s).floor() as i32, (eye[2] / s).floor() as i32];
        let step = [
            if dir[0] > 0.0 { 1 } else { -1 },
            if dir[2] > 0.0 { 1 } else { -1 },
        ];
        let next = |c: i32, d: f64, e: f64, st: i32| -> f64 {
            if d.abs() < 1e-12 {
                return f64::MAX;
            }
            let edge = if st > 0 {
                (c + 1) as f64 * s
            } else {
                c as f64 * s
            };
            (edge - e) / d
        };
        let mut t_max = [
            next(cell[0], dir[0], eye[0], step[0]),
            next(cell[1], dir[2], eye[2], step[1]),
        ];
        let t_delta = [
            if dir[0].abs() < 1e-12 {
                f64::MAX
            } else {
                s / dir[0].abs()
            },
            if dir[2].abs() < 1e-12 {
                f64::MAX
            } else {
                s / dir[2].abs()
            },
        ];
        let mut best: Option<(usize, f64, bool)> = None;
        loop {
            if let Some(list) = self.cells.get(&cell) {
                for &i in list {
                    let f = &self.faces[i as usize];
                    if let Some((t, front)) = hit(f, eye, dir) {
                        if t < limit && best.is_none_or(|b| t < b.1) {
                            best = Some((i as usize, t, front));
                        }
                    }
                }
            }
            let t_exit = t_max[0].min(t_max[1]);
            if best.is_some_and(|b| b.1 <= t_exit) || t_exit > limit {
                return best;
            }
            if t_max[0] < t_max[1] {
                cell[0] += step[0];
                t_max[0] += t_delta[0];
            } else {
                cell[1] += step[1];
                t_max[1] += t_delta[1];
            }
        }
    }
}

/// Distance along the ray to the face, and whether the front was hit.
fn hit(f: &GridFace, eye: V3, dir: V3) -> Option<(f64, bool)> {
    let front = dot(f.normal, dir) < 0.0;
    let p: Vec<V3> = f
        .points
        .iter()
        .map(|p| [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])])
        .collect();
    for t in 1..p.len().saturating_sub(1) {
        let (e1, e2) = (sub(p[t], p[0]), sub(p[t + 1], p[0]));
        let h = cross(dir, e2);
        let det = dot(e1, h);
        if det.abs() < 1e-9 {
            continue;
        }
        let s = sub(eye, p[0]);
        let u = dot(s, h) / det;
        if !(0.0..=1.0).contains(&u) {
            continue;
        }
        let q = cross(s, e1);
        let v = dot(dir, q) / det;
        if v < 0.0 || u + v > 1.0 {
            continue;
        }
        let along = dot(e2, q) / det;
        if along > 1.0 {
            return Some((along, front || f.double_sided));
        }
    }
    None
}

impl Index<'_> {
    /// Height of the highest upward face over the plan point, if any.
    fn ground(&self, x: f64, z: f64) -> Option<f64> {
        self.cast([x, 1.0e6, z], [0.0, -1.0, 0.0], 3.0e6)
            .filter(|(i, _, front)| *front && self.faces[*i].normal[1] > 0.02)
            .map(|(_, t, _)| 1.0e6 - t)
    }

    /// Where a ray that ends on the back of a face first went below the
    /// ground: the last point in the air and the first point inside, as
    /// plan sector columns, with the height of the crossing.
    fn entry(&self, eye: V3, dir: V3, until: f64) -> Option<([i32; 2], [i32; 2], f64)> {
        let col = |p: V3| {
            [
                (p[0] / self.sector).floor() as i32,
                (p[2] / self.sector).floor() as i32,
            ]
        };
        let at = |t: f64| {
            [
                eye[0] + dir[0] * t,
                eye[1] + dir[1] * t,
                eye[2] + dir[2] * t,
            ]
        };
        let mut t = 0.0;
        let mut last = None;
        while t < until {
            let p = at(t);
            let below = self.ground(p[0], p[2]).is_some_and(|h| p[1] < h - 4.0);
            if below {
                return last.map(|a: V3| (col(a), col(p), p[1]));
            }
            last = Some(p);
            t += 48.0;
        }
        None
    }
}

/// What the camera at `eye` looking at `target` sees through every 8th pixel
/// of a 640x480 frame (focal length 640 pixels, as the editor preview has
/// it), one line per row: `S` nothing, `.` the front of a face, `B` the back
/// of a one-sided face.
fn print_pose(index: &Index<'_>, eye: V3, target: V3) {
    let norm = |a: V3| {
        let l = dot(a, a).sqrt();
        [a[0] / l, a[1] / l, a[2] / l]
    };
    let fwd = norm(sub(target, eye));
    let right = norm(cross(fwd, [0.0, 1.0, 0.0]));
    let up = cross(right, fwd);
    for row in 0..60 {
        let mut line = String::new();
        for col in 0..80 {
            let (x, y) = (f64::from(col) * 8.0 + 4.0, f64::from(row) * 8.0 + 4.0);
            let d = norm([
                640.0 * fwd[0] + (x - 320.0) * right[0] - (y - 240.0) * up[0],
                640.0 * fwd[1] + (x - 320.0) * right[1] - (y - 240.0) * up[1],
                640.0 * fwd[2] + (x - 320.0) * right[2] - (y - 240.0) * up[2],
            ]);
            line.push(match index.cast(eye, d, 200_000.0) {
                None => 'S',
                Some((_, _, true)) => '.',
                Some((_, _, false)) => 'B',
            });
        }
        println!("{line}");
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(dir) = args.first() else {
        eprintln!("usage: gridsweep <project dir> [out tsv]");
        return ExitCode::from(2);
    };
    let doc = match ProjectDocument::load_from_path(std::path::Path::new(dir).join("project.ron")) {
        Ok(doc) => doc,
        Err(err) => {
            eprintln!("cannot load {dir}: {err:?}");
            return ExitCode::FAILURE;
        }
    };
    let Some(world) = doc.active_scene().world.grid() else {
        eprintln!("{dir} has no grid world");
        return ExitCode::FAILURE;
    };
    let sector = f64::from(world.scale.to_editor(1024));
    let faces = all_faces_with(world, &());
    let index = Index::new(&faces, sector);
    // Eye points: above the highest upward face over each sector column.
    let mut tops: BTreeMap<[i32; 2], f64> = BTreeMap::new();
    for f in &faces {
        if f.normal[1] > 0.02 {
            let cx = f.points.iter().map(|p| f64::from(p[0])).sum::<f64>() / f.points.len() as f64;
            let cz = f.points.iter().map(|p| f64::from(p[2])).sum::<f64>() / f.points.len() as f64;
            let top = f
                .points
                .iter()
                .map(|p| f64::from(p[1]))
                .fold(f64::MIN, f64::max);
            let cell = [(cx / sector).floor() as i32, (cz / sector).floor() as i32];
            let e = tops.entry(cell).or_insert(f64::MIN);
            *e = e.max(top);
        }
    }
    if args.get(1).map(String::as_str) == Some("pose") {
        let n: Vec<f64> = args[2..8].iter().filter_map(|v| v.parse().ok()).collect();
        if n.len() == 6 {
            print_pose(&index, [n[0], n[1], n[2]], [n[3], n[4], n[5]]);
            return ExitCode::SUCCESS;
        }
    }
    let mut rays = 0u64;
    let mut holes = 0u64;
    let mut sites: BTreeMap<([i32; 2], [i32; 2]), u32> = BTreeMap::new();
    let no_ground = &mut 0u64;
    let mut ngd = String::new();
    let mut tsv = String::new();
    let (azimuths, elevations) = (24, [-50.0f64, -30.0, -15.0, -5.0, 5.0, 15.0, 30.0]);
    // The base the converter gives the void, in editor units: nothing
    // stands there, and from below the surface is open (a ray meeting the
    // underside of the island from there is no gap).
    let base = f64::from(world.scale.to_editor(-1024));
    for (cell, _) in tops.iter().filter(|(_, top)| **top > base + 100.0) {
        let (ex, ez) = (
            (f64::from(cell[0]) + 0.5) * sector + 37.0,
            (f64::from(cell[1]) + 0.5) * sector + 91.0,
        );
        // Stand on the ground at the eye's own spot, not at the cell's top.
        let Some(here) = index.ground(ex, ez) else {
            continue;
        };
        let eye = [ex, here + 1100.0, ez];
        for a in 0..azimuths {
            let az = (f64::from(a) + 0.37) * std::f64::consts::TAU / f64::from(azimuths);
            for el in elevations {
                let e = el.to_radians();
                let dir = [az.sin() * e.cos(), e.sin(), -az.cos() * e.cos()];
                rays += 1;
                if let Some((i, t, false)) = index.cast(eye, dir, 80_000.0) {
                    holes += 1;
                    let f = &faces[i];
                    let Some((air, solid, y)) = index.entry(eye, dir, t) else {
                        // Went under ground without a ground to enter: the
                        // surface is missing here altogether.
                        *no_ground += 1;
                        let _ = writeln!(
                            ngd,
                            "{}\t{}\t{:.0}\t{:.0}\t{:.0}\t{:.0}\t{:.3}\t{:.3}\t{:.3}\t{}\t{:?}\t{:?}",
                            cell[0], cell[1], eye[0], eye[1], eye[2], t, dir[0], dir[1], dir[2], f.room, f.cell, f.kind
                        );
                        continue;
                    };
                    *sites.entry((air, solid)).or_default() += 1;
                    let _ = writeln!(
                        tsv,
                        "{}\t{}\t{}\t{}\t{:.0}\t{:.0}\t{:.0}\t{:.0}\t{:.0}\t{:.3}\t{:.3}\t{:.3}\t{}\t{:?}",
                        air[0], air[1], solid[0], solid[1], y, eye[0], eye[1], eye[2], t, dir[0], dir[1], dir[2],
                        f.room, f.kind
                    );
                }
            }
        }
    }
    println!(
        "{} faces, {} eye points, {rays} rays, {holes} rays ended on the back of a face: {} gaps (pairs of neighbouring sector columns the rays crossed), {} rays with no ground at the crossing",
        faces.len(),
        tops.len(),
        sites.len(),
        no_ground
    );
    if let Some(out) = args.get(1) {
        let _ = std::fs::write(format!("{out}.noground"), ngd);
        if let Err(err) = std::fs::write(out, tsv) {
            eprintln!("cannot write {out}: {err}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}
