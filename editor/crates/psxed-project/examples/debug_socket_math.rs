//! TEMP diagnostic: run the runtime's socket math on host with the
//! real cooked character data to find where the weapon origin
//! saturates. Prints joint pose translation magnitudes and the
//! composed world transform per frame.
//!
//! Usage: cargo run -p psxed-project --example debug_socket_math -- \
//!     <model.psxmdl> <clip.psxanim> <joint> <visual_scale_q8>

use psx_engine::{compute_joint_world_transform, LocalToWorldScale, WorldVertex};

fn main() {
    let mut args = std::env::args().skip(1);
    let model_path = args.next().expect("model.psxmdl");
    let clip_path = args.next().expect("clip.psxanim");
    let joint: u16 = args.next().expect("joint").parse().expect("joint number");
    let visual_scale_q8: u32 = args
        .next()
        .unwrap_or_else(|| "256".to_string())
        .parse()
        .expect("scale");

    let model_bytes = std::fs::read(&model_path).expect("read model");
    let clip_bytes = std::fs::read(&clip_path).expect("read clip");
    let model = psx_asset::Model::from_bytes(&model_bytes).expect("parse model");
    let anim = psx_asset::Animation::from_bytes(&clip_bytes).expect("parse anim");

    let base_q12 = model.local_to_world_q12() as u32;
    let scaled_q12 = ((base_q12 * visual_scale_q8) + 128) / 256;
    let local_to_world = LocalToWorldScale::from_q12(scaled_q12 as u16);
    println!(
        "model local_to_world_q12={base_q12} visual_scale_q8={visual_scale_q8} -> scaled_q12={scaled_q12}"
    );

    let instance_rotation = psx_engine::Mat3I16 {
        m: [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]],
    };
    let origin = WorldVertex::new(8575, 1152, 7872);

    for frame in 0..anim.frame_count().min(8) {
        let phase = (frame as u32) << 12;
        let Some(pose) = anim.pose_looped_q12(phase, joint) else {
            println!("frame {frame}: no pose");
            continue;
        };
        let t = pose.translation;
        let joint_world = compute_joint_world_transform(pose, instance_rotation, local_to_world, origin);
        println!(
            "frame {frame}: pose.t=({}, {}, {}) applied=({}, {}, {}) world=({}, {}, {}) rot00={}",
            t.x,
            t.y,
            t.z,
            local_to_world.apply(t.x),
            local_to_world.apply(t.y),
            local_to_world.apply(t.z),
            joint_world.translation.x,
            joint_world.translation.y,
            joint_world.translation.z,
            joint_world.rotation.m[0][0],
        );
    }
}
