//! Compare cooked animation revisions, including fractional-frame sampling.
use std::path::Path;
fn compare(old: &Path, new: &Path, totals: &mut [usize; 3]) {
    for entry in std::fs::read_dir(old).unwrap() {
        let old = entry.unwrap().path();
        let new = new.join(old.file_name().unwrap());
        if old.is_dir() {
            compare(&old, &new, totals);
            continue;
        }
        if old.extension().and_then(|s| s.to_str()) != Some("psxanim") {
            continue;
        }
        let a = std::fs::read(&old).unwrap();
        let b = std::fs::read(&new).unwrap();
        let a_clip = psx_asset::Animation::from_bytes(&a).unwrap();
        let b_clip = psx_asset::Animation::from_bytes(&b).unwrap();
        assert_eq!(
            (
                a_clip.frame_count(),
                a_clip.joint_count(),
                a_clip.sample_rate_hz()
            ),
            (
                b_clip.frame_count(),
                b_clip.joint_count(),
                b_clip.sample_rate_hz()
            )
        );
        for frame in 0..a_clip.frame_count() {
            for joint in 0..a_clip.joint_count() {
                assert_eq!(
                    a_clip.pose(frame, joint),
                    b_clip.pose(frame, joint),
                    "{} frame {frame} joint {joint}",
                    old.display()
                );
                for fraction in [0, 1024, 2048, 3072] {
                    let phase = u32::from(frame) * 4096 + fraction;
                    assert_eq!(
                        a_clip.pose_looped_q12(phase, joint),
                        b_clip.pose_looped_q12(phase, joint)
                    );
                }
            }
        }
        totals[0] += 1;
        totals[1] += a.len();
        totals[2] += b.len();
    }
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let mut totals = [0; 3];
    compare(Path::new(&args[1]), Path::new(&args[2]), &mut totals);
    println!(
        "{} clips match at every frame and quarter-frame; {} -> {} bytes ({} saved)",
        totals[0],
        totals[1],
        totals[2],
        totals[1] - totals[2]
    );
}
