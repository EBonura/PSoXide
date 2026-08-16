//! Lowest posed vertex per frame of a COOKED clip, in engine units relative to
//! the placement floor, replicating the runtime's placement exactly:
//!   origin = floor + apply(lift), vertex = origin + apply(pose * v)
//! usage: cooked_floor <model.psxmdl> <clip.psxanim> <visual_scale_q8> [frames]
use psxed_project::playtest::{model_bounds_joint_transform, transform_model_bounds_vertex};
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mb = std::fs::read(&a[0]).unwrap();
    let cb = std::fs::read(&a[1]).unwrap();
    let scale_q8: u32 = a[2].parse().unwrap();
    let model = psx_asset::Model::from_bytes(&mb).unwrap();
    let clip = psx_asset::Animation::from_bytes(&cb).unwrap();
    let composed = ((model.local_to_world_q12() as u32 * scale_q8 + 128) / 256).clamp(1, 65535);
    let apply = |v: i32| ((v as i64 * composed as i64) >> 12) as i32;
    let lift = apply(model.bind_pose_floor_lift());
    println!(
        "l2w={} composed_q12={} bind_lift_model={} lift_world={}",
        model.local_to_world_q12(),
        composed,
        model.bind_pose_floor_lift(),
        lift
    );
    let frames: u16 = a
        .get(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(clip.frame_count());
    for frame in 0..frames.min(clip.frame_count()) {
        let mut lowest = i32::MAX;
        for part_index in 0..model.part_count() {
            let part = model.part(part_index).unwrap();
            let Some(pose) = clip.pose(frame, part.joint_index() as u16) else {
                continue;
            };
            let t = model_bounds_joint_transform(pose, 0x1000);
            for v in part.first_vertex()..part.first_vertex() + part.vertex_count() {
                let p = transform_model_bounds_vertex(t, model.vertex(v).unwrap());
                lowest = lowest.min(p[1]);
            }
        }
        println!(
            "frame {frame:3}: lowest_model={lowest:7} -> world {:+.2} above floor",
            (lift + apply(lowest)) as f64
        );
    }
}
