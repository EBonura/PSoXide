//! Print a joint's per-frame translation from a cooked .psxanim.
//! usage: dump_anim_joint <clip.psxanim> <joint> [<joint>...]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bytes = std::fs::read(&args[0]).expect("read clip");
    let anim = psx_asset::Animation::from_bytes(&bytes).expect("parse clip");
    let joints: Vec<u16> = args[1..].iter().map(|a| a.parse().unwrap()).collect();
    println!(
        "frames={} joints={} rate_hz={:?}",
        anim.frame_count(),
        anim.joint_count(),
        anim.sample_rate_hz()
    );
    for frame in 0..anim.frame_count() {
        let mut line = format!("{frame:3}");
        for &j in &joints {
            let p = anim.pose(frame, j).expect("pose");
            line += &format!(
                "  j{j}: x={:6} y={:6} z={:6}",
                p.translation.x, p.translation.y, p.translation.z
            );
        }
        println!("{line}");
    }
}
