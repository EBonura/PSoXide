//! Rendered lowest/highest vertex per frame of a clip, in engine units above the
//! placement floor, replicating the runtime's anchor: origin lift = world_height x
//! visual_scale / 2, per-clip anchor y = idle.floor_y(frame 0) - clip.floor_y(frame 0)
//! where floor_y is the cook's max(raw y). usage: clip_floor <model.psxmdl> <idle.psxanim> <clip.psxanim> <world_height> <visual_scale_q8> [cook_div]
use psxed_project::playtest::{
    bake_model_frame_pair_bounds, model_bounds_joint_transform, transform_model_bounds_vertex,
};
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mb = std::fs::read(&a[0]).unwrap();
    let ib = std::fs::read(&a[1]).unwrap();
    let cb = std::fs::read(&a[2]).unwrap();
    let model = psx_asset::Model::from_bytes(&mb).unwrap();
    let idle = psx_asset::Animation::from_bytes(&ib).unwrap();
    let clip = psx_asset::Animation::from_bytes(&cb).unwrap();
    let world_height: f64 = a[3].parse().unwrap();
    let scale: f64 = a[4].parse::<f64>().unwrap() / 256.0;
    let div: f64 = a.get(5).map(|s| s.parse().unwrap()).unwrap_or(16.0);
    let l2w = model.local_to_world_q12() as f64 / 4096.0;
    let to_engine = |raw: f64| raw / div * l2w * scale;
    let extremes = |anim: &psx_asset::Animation<'_>, frame: u16| -> (f64, f64) {
        let mut lo = f64::MAX;
        let mut hi = f64::MIN;
        for part_index in 0..model.part_count() {
            let part = model.part(part_index).unwrap();
            let pose = anim.pose(frame, part.joint_index() as u16).unwrap();
            let t = model_bounds_joint_transform(pose, 0x1000);
            for v in part.first_vertex()..part.first_vertex() + part.vertex_count() {
                let p = transform_model_bounds_vertex(t, model.vertex(v).unwrap());
                lo = lo.min(p[1] as f64);
                hi = hi.max(p[1] as f64);
            }
        }
        (lo, hi)
    };
    let cook_floor = |anim: &psx_asset::Animation<'_>, frame: u16| {
        bake_model_frame_pair_bounds(&model, anim, frame, frame, 0).floor_y as f64
    };
    let _ = world_height;
    let lift = model.bind_pose_floor_lift() as f64 / div * scale;
    let anchor_raw = cook_floor(&idle, 0) - cook_floor(&clip, 0); // runtime: reference - clip, raw model units
    println!(
        "lift={lift:.2}  cook floor_y idle0={:.0} clip0={:.0} (raw)  anchor(engine)={:.2}",
        cook_floor(&idle, 0),
        cook_floor(&clip, 0),
        anchor_raw / div * l2w * scale
    );
    let (ilo, ihi) = extremes(&idle, 0);
    println!(
        "idle frame0: lowest={:.2} highest={:.2} (engine units above floor)",
        lift + to_engine(ilo),
        lift + to_engine(ihi)
    );
    for f in 0..clip.frame_count() {
        let (lo, hi) = extremes(&clip, f);
        let off = anchor_raw / div * l2w * scale;
        println!(
            "clip frame {f:2}: lowest={:6.2} highest={:6.2}",
            lift + to_engine(lo) + off,
            lift + to_engine(hi) + off
        );
    }
}
