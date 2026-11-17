//! Worst pose difference between two `anim_dump` directories, per clip, in model
//! units: every joint's vertices (and the cook's 600 unit lever arms) displaced by
//! both poses, compared at the same time (every Q8 phase of the first dump's clip,
//! mapped onto the second's rate and held at its last interval). The looped sampler
//! blends a pose table's last interval towards frame 0 while a keyed clip blends
//! towards its last frame, so that interval is left out when either clip is a pose
//! table and the encodings differ.
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
    for list in &mut out {
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

fn apply(pose: &JointPose, p: [f64; 3]) -> [f64; 3] {
    let m = pose.matrix;
    let t = [pose.translation.x, pose.translation.y, pose.translation.z];
    [0, 1, 2].map(|r| {
        (f64::from(m[r][0]) * p[0] + f64::from(m[r][1]) * p[1] + f64::from(m[r][2]) * p[2]) / 4096.0
            + f64::from(t[r])
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (a_dir, b_dir) = (PathBuf::from(&args[0]), PathBuf::from(&args[1]));
    let mut overall: f64 = 0.0;
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
                        worst = worst.max(d);
                    }
                }
                phase += 16;
            }
            if ab != bb {
                let (da, db) = (
                    fa as f64 * 1000.0 / f64::from(a.sample_rate_hz()),
                    fb as f64 * 1000.0 / f64::from(b.sample_rate_hz()),
                );
                println!(
                    "{}/{}: {} B -> {} B, {} Hz -> {} Hz, {:.0} -> {:.0} ms, worst {:.2} units",
                    model_name.to_string_lossy(),
                    clip.file_name().unwrap().to_string_lossy(),
                    ab.len(),
                    bb.len(),
                    a.sample_rate_hz(),
                    b.sample_rate_hz(),
                    da,
                    db,
                    worst
                );
            }
            overall = overall.max(worst);
        }
    }
    println!("worst over every clip: {overall:.2} units");
}
