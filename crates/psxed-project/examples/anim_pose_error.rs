//! Worst pose difference between two `anim_dump` directories, per clip, in model
//! units: every joint's vertices (and the cook's 600 unit lever arms) displaced by
//! both poses, compared at the same time (every Q8 phase of the first dump's clip,
//! mapped onto the second's rate and held at its last interval). The looped sampler
//! blends a pose table's last interval towards frame 0 while a keyed clip blends
//! towards its last frame, so that interval is left out when either clip is a pose
//! table and the encodings differ.
//!
//! `ANIM_ERR_RENDER=<dir>` also writes, per clip, the phase of its worst error
//! as a flat-shaded front view, dump A | dump B (PPM, 320x400 a panel).
//!
//! `ANIM_ERR_STEP=<q12>` sets the phase step (default 16; 4096 compares whole
//! source frames only, the step that measures a clip against its source poses).
//!
//! Usage: anim_pose_error <dump-a> <dump-b>

use std::path::{Path, PathBuf};

use psx_asset::{Animation, JointPose, Model};

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    out.sort();
    out
}

fn probes(model: &Model<'_>) -> Vec<Vec<[f64; 3]>> {
    let mut out = vec![Vec::new(); model.joint_count() as usize];
    for p in 0..model.part_count() {
        let part = model.part(p).unwrap();
        for v in part.first_vertex()..part.first_vertex() + part.vertex_count() {
            let vertex = model.vertex(v).unwrap();
            let pos = [
                f64::from(vertex.position.x),
                f64::from(vertex.position.y),
                f64::from(vertex.position.z),
            ];
            out[part.joint_index() as usize].push(pos);
            if vertex.is_blend() {
                out[vertex.joint1 as usize].push(pos);
            }
        }
    }
    // `ANIM_ERR_ARMS=0` measures drawn vertices only.
    let arms = std::env::var("ANIM_ERR_ARMS").map_or(true, |v| v != "0");
    for list in out.iter_mut().filter(|_| arms) {
        for axis in 0..3 {
            for r in [-600.0, 600.0] {
                let mut p = [0.0; 3];
                p[axis] = r;
                list.push(p);
            }
        }
    }
    out
}

/// `pose` applied to `p` the way the renderer applies it: pose matrices are
/// column-major (`scaled_pose_matrix` in psx-engine render3d.rs reads
/// `matrix[col][row]`).
fn apply(pose: &JointPose, p: [f64; 3]) -> [f64; 3] {
    let m = pose.matrix;
    let t = [pose.translation.x, pose.translation.y, pose.translation.z];
    [0, 1, 2].map(|r| {
        (f64::from(m[0][r]) * p[0] + f64::from(m[1][r]) * p[1] + f64::from(m[2][r]) * p[2]) / 4096.0
            + f64::from(t[r])
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (a_dir, b_dir) = (PathBuf::from(&args[0]), PathBuf::from(&args[1]));
    let mut overall: f64 = 0.0;
    let step: u64 = std::env::var("ANIM_ERR_STEP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(16);
    for mdir in files(&a_dir).into_iter().filter(|p| p.is_dir()) {
        let model_name = mdir.file_name().unwrap();
        let mesh = std::fs::read(mdir.join("model.psxmdl")).unwrap();
        let model = Model::from_bytes(&mesh).unwrap();
        let probes = probes(&model);
        for clip in files(&mdir)
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "psxanim"))
        {
            let other = b_dir.join(model_name).join(clip.file_name().unwrap());
            let (ab, bb) = (
                std::fs::read(&clip).unwrap(),
                std::fs::read(&other).unwrap(),
            );
            let (a, b) = (
                Animation::from_bytes(&ab).unwrap(),
                Animation::from_bytes(&bb).unwrap(),
            );
            if a.frame_count() < 2 || b.frame_count() < 2 {
                continue;
            }
            let (fa, fb) = (
                u64::from(a.frame_count() - 1),
                u64::from(b.frame_count() - 1),
            );
            let (ha, hb) = (u64::from(a.sample_rate_hz()), u64::from(b.sample_rate_hz()));
            let tail = a.is_keyed_tracks() != b.is_keyed_tracks();
            let (end_a, end_b) = if tail {
                ((fa - 1) << 12, (fb - 1) << 12)
            } else {
                (fa << 12, fb << 12)
            };
            let mut worst: f64 = 0.0;
            let mut worst_at = (0u32, 0u32);
            let (mut sum, mut count) = (0.0f64, 0u64);
            let mut phase = 0u64;
            while phase < end_a {
                let pb = (phase * hb / ha).min(end_b - 16) as u32;
                let (sa, sb) = (
                    a.looped_pose_sample_q12(phase as u32).unwrap(),
                    b.looped_pose_sample_q12(pb).unwrap(),
                );
                for j in 0..a.joint_count() {
                    let (pa, pbp) = (sa.pose(j).unwrap(), sb.pose(j).unwrap());
                    for &p in &probes[j as usize] {
                        let (x, y) = (apply(&pa, p), apply(&pbp, p));
                        let d =
                            ((x[0] - y[0]).powi(2) + (x[1] - y[1]).powi(2) + (x[2] - y[2]).powi(2))
                                .sqrt();
                        if d > worst {
                            worst = d;
                            worst_at = (phase as u32, pb);
                        }
                        sum += d;
                        count += 1;
                    }
                }
                phase += step;
            }
            if let (Some(dir), true) = (std::env::var_os("ANIM_ERR_RENDER"), ab != bb) {
                let (sa, sb) = (
                    a.looped_pose_sample_q12(worst_at.0).unwrap(),
                    b.looped_pose_sample_q12(worst_at.1).unwrap(),
                );
                let pa: Vec<JointPose> =
                    (0..a.joint_count()).map(|j| sa.pose(j).unwrap()).collect();
                let pb: Vec<JointPose> =
                    (0..b.joint_count()).map(|j| sb.pose(j).unwrap()).collect();
                let name = format!(
                    "{}__{}__{:.1}.ppm",
                    model_name.to_string_lossy(),
                    clip.file_stem().unwrap().to_string_lossy(),
                    worst
                );
                render_pair(&model, &pa, &pb, &PathBuf::from(dir).join(name));
            }
            if ab != bb {
                let (da, db) = (
                    fa as f64 * 1000.0 / f64::from(a.sample_rate_hz()),
                    fb as f64 * 1000.0 / f64::from(b.sample_rate_hz()),
                );
                println!(
                    "{}/{}: {} B -> {} B, {} Hz -> {} Hz, {:.0} -> {:.0} ms, worst {:.2} units, \
                     mean {:.2}",
                    model_name.to_string_lossy(),
                    clip.file_name().unwrap().to_string_lossy(),
                    ab.len(),
                    bb.len(),
                    a.sample_rate_hz(),
                    b.sample_rate_hz(),
                    da,
                    db,
                    worst,
                    sum / count.max(1) as f64
                );
            }
            overall = overall.max(worst);
        }
    }
    println!("worst over every clip: {overall:.2} units");
}

/// Flat-shaded orthographic front views (x right, y up), `a` | `b`.
fn render_pair(mesh: &Model<'_>, a: &[JointPose], b: &[JointPose], path: &Path) {
    let (w, h) = (320usize, 400usize);
    let pose_verts = |poses: &[JointPose]| -> Vec<[f64; 3]> {
        let mut out = vec![[0.0; 3]; mesh.vertex_count() as usize];
        for pi in 0..mesh.part_count() {
            let part = mesh.part(pi).unwrap();
            for v in part.first_vertex()..part.first_vertex() + part.vertex_count() {
                let vx = mesh.vertex(v).unwrap();
                let p = [
                    f64::from(vx.position.x),
                    f64::from(vx.position.y),
                    f64::from(vx.position.z),
                ];
                let mut q = apply(&poses[part.joint_index() as usize], p);
                if vx.is_blend() {
                    let r = apply(&poses[vx.joint1 as usize], p);
                    let t = f64::from(vx.blend) / 256.0;
                    q = [0, 1, 2].map(|k| q[k] * (1.0 - t) + r[k] * t);
                }
                out[v as usize] = q;
            }
        }
        out
    };
    let va = pose_verts(a);
    let vb = pose_verts(b);
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for v in va.iter().chain(vb.iter()) {
        for k in 0..2 {
            lo[k] = lo[k].min(v[k]);
            hi[k] = hi[k].max(v[k]);
        }
    }
    let scale = ((w as f64 - 20.0) / (hi[0] - lo[0])).min((h as f64 - 20.0) / (hi[1] - lo[1]));
    let mut img = vec![40u8; 2 * w * h * 3];
    for (panel, verts) in [&va, &vb].into_iter().enumerate() {
        let mut zb = vec![f64::MAX; w * h];
        let sx = |v: &[f64; 3]| -> [f64; 3] {
            [
                10.0 + (v[0] - lo[0]) * scale,
                h as f64 - 10.0 - (v[1] - lo[1]) * scale,
                -v[2],
            ]
        };
        for fi in 0..mesh.face_count() {
            let f = mesh.face(fi).unwrap();
            let p: Vec<[f64; 3]> = f
                .corners
                .iter()
                .map(|c| sx(&verts[c.vertex_index as usize]))
                .collect();
            let e1 = [p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]];
            let e2 = [p[2][0] - p[0][0], p[2][1] - p[0][1], p[2][2] - p[0][2]];
            let nz = e1[0] * e2[1] - e1[1] * e2[0];
            let nlen = ((e1[1] * e2[2] - e1[2] * e2[1]).powi(2)
                + (e1[2] * e2[0] - e1[0] * e2[2]).powi(2)
                + nz.powi(2))
            .sqrt()
            .max(1e-9);
            let shade = (60.0 + 190.0 * (nz / nlen).abs()) as u8;
            let x0 = p.iter().map(|q| q[0]).fold(f64::MAX, f64::min).max(0.0) as usize;
            let x1 = (p.iter().map(|q| q[0]).fold(f64::MIN, f64::max) as usize).min(w - 1);
            let y0 = p.iter().map(|q| q[1]).fold(f64::MAX, f64::min).max(0.0) as usize;
            let y1 = (p.iter().map(|q| q[1]).fold(f64::MIN, f64::max) as usize).min(h - 1);
            let area = (p[1][0] - p[0][0]) * (p[2][1] - p[0][1])
                - (p[2][0] - p[0][0]) * (p[1][1] - p[0][1]);
            if area.abs() < 1e-9 {
                continue;
            }
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                    let w0 =
                        ((p[1][0] - px) * (p[2][1] - py) - (p[2][0] - px) * (p[1][1] - py)) / area;
                    let w1 =
                        ((p[2][0] - px) * (p[0][1] - py) - (p[0][0] - px) * (p[2][1] - py)) / area;
                    let w2 = 1.0 - w0 - w1;
                    if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                        continue;
                    }
                    let z = w0 * p[0][2] + w1 * p[1][2] + w2 * p[2][2];
                    if z < zb[y * w + x] {
                        zb[y * w + x] = z;
                        let o = (y * 2 * w + panel * w + x) * 3;
                        img[o] = shade;
                        img[o + 1] = shade;
                        img[o + 2] = if panel == 0 { shade / 2 + 100 } else { shade };
                    }
                }
            }
        }
    }
    let mut out = format!("P6\n{} {}\n255\n", 2 * w, h).into_bytes();
    out.extend_from_slice(&img);
    std::fs::write(path, out).unwrap();
}
