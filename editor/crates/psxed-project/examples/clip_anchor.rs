//! Cooked clip anchor vs the grounding the mesh actually needs.
//!
//! Prints, for one cooked model + clip: the cook's `floor_y` (what the runtime
//! turns into the origin lift), the lift that follows from it at a given visual
//! scale, and the lift the posed mesh really wants (origin to lowest posed
//! vertex). A gap between the two is exactly how far the actor floats.
//!
//! usage: clip_anchor <model.psxmdl> <clip.psxanim> <visual_scale_q8>
use psxed_project::playtest::{
    bake_model_frame_pair_bounds, model_bounds_joint_transform, transform_model_bounds_vertex,
};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let model_bytes = std::fs::read(&a[0]).unwrap();
    let clip_bytes = std::fs::read(&a[1]).unwrap();
    let model = psx_asset::Model::from_bytes(&model_bytes).unwrap();
    let clip = psx_asset::Animation::from_bytes(&clip_bytes).unwrap();
    let scale_q8: u32 = a[2].parse().unwrap();
    let composed = ((model.local_to_world_q12() as u32 * scale_q8 + 128) / 256).clamp(1, 65535);
    let apply = |v: i32| ((v as i64 * composed as i64) >> 12) as i32;

    let bounds = bake_model_frame_pair_bounds(&model, &clip, 0, 0, 0);
    // What the posed mesh wants: origin to lowest posed vertex of frame 0.
    let mut lowest = i32::MAX;
    for part_index in 0..model.part_count() {
        let part = model.part(part_index).unwrap();
        let Some(pose) = clip.pose(0, part.joint_index() as u16) else {
            continue;
        };
        let t = model_bounds_joint_transform(pose, 0x1000);
        for v in part.first_vertex()..part.first_vertex() + part.vertex_count() {
            let p = transform_model_bounds_vertex(t, model.vertex(v).unwrap());
            lowest = lowest.min(p[1]);
        }
    }
    println!(
        "cook floor_y={} -> runtime lift={}  |  lowest posed vertex={} -> lift wanted={}  |  float={} world units",
        bounds.floor_y,
        apply(-bounds.floor_y),
        lowest,
        apply(-lowest),
        apply(-bounds.floor_y) - apply(-lowest)
    );
}
