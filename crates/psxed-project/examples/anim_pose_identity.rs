//! Prove two `anim_dump` directories decode to the same poses: every clip, every
//! joint, at every playback phase the runtime sampler can produce (every Q8
//! position of a keyed clip, every Q12 phase of a pose table, plus every whole
//! frame). Exits non-zero on the first difference.
//!
//! Usage: anim_pose_identity <dump-before> <dump-after>

use std::path::{Path, PathBuf};

use psx_asset::{Animation, JointPose};

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    out.sort();
    out
}

fn same(a: &JointPose, b: &JointPose) -> bool {
    a.matrix == b.matrix && a.translation == b.translation
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (before, after) = (PathBuf::from(&args[0]), PathBuf::from(&args[1]));
    let (mut clips, mut samples, mut bytes_a, mut bytes_b) = (0usize, 0u64, 0usize, 0usize);
    for mdir in files(&before).into_iter().filter(|p| p.is_dir()) {
        let model = mdir.file_name().unwrap();
        for clip in files(&mdir)
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "psxanim"))
        {
            let other = after.join(model).join(clip.file_name().unwrap());
            let (a_bytes, b_bytes) = (
                std::fs::read(&clip).unwrap(),
                std::fs::read(&other).unwrap(),
            );
            bytes_a += a_bytes.len();
            bytes_b += b_bytes.len();
            let (a, b) = (
                Animation::from_bytes(&a_bytes).unwrap(),
                Animation::from_bytes(&b_bytes).unwrap(),
            );
            let label = format!(
                "{}/{}",
                model.to_string_lossy(),
                clip.file_name().unwrap().to_string_lossy()
            );
            assert_eq!(
                (a.joint_count(), a.frame_count(), a.sample_rate_hz()),
                (b.joint_count(), b.frame_count(), b.sample_rate_hz()),
                "{label}: header"
            );
            for f in 0..a.frame_count() {
                for j in 0..a.joint_count() {
                    let (pa, pb) = (a.pose(f, j).unwrap(), b.pose(f, j).unwrap());
                    assert!(
                        same(&pa, &pb),
                        "{label}: frame {f} joint {j}: {pa:?} vs {pb:?}"
                    );
                    samples += 1;
                }
            }
            // Keyed clips read the phase at Q8, pose tables at Q12; walk every Q12
            // phase of a pose table and every Q8 phase (step 16 in Q12) of a keyed one.
            let cycle = u32::from(a.frame_count().saturating_sub(1).max(1)) << 12;
            let step = if a.is_keyed_tracks() || b.is_keyed_tracks() {
                16
            } else {
                1
            };
            let mut phase = 0u32;
            while phase < cycle {
                let (sa, sb) = (
                    a.looped_pose_sample_q12(phase).unwrap(),
                    b.looped_pose_sample_q12(phase).unwrap(),
                );
                for j in 0..a.joint_count() {
                    let (pa, pb) = (sa.pose(j).unwrap(), sb.pose(j).unwrap());
                    assert!(
                        same(&pa, &pb),
                        "{label}: phase {phase:#x} joint {j}: {pa:?} vs {pb:?}"
                    );
                    samples += 1;
                }
                phase += step;
            }
            clips += 1;
        }
    }
    println!(
        "identical: {clips} clips, {samples} joint samples; {bytes_a} B -> {bytes_b} B ({} B saved)",
        bytes_a as i64 - bytes_b as i64
    );
}
