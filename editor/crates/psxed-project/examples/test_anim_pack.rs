//! Compatibility check: retarget an animation-pack FBX/GLB onto a model
//! FBX through the importer and report the resulting clips.
//!   cargo run -p psxed-project --example test_anim_pack -- <model.fbx> <pack.fbx>

use std::path::PathBuf;
use psxed_project::model_import::preview_model_with_animation_sources;
use psxed_gltf::RigidModelConfig;

fn main() {
    let mut a = std::env::args().skip(1);
    let model = PathBuf::from(a.next().expect("usage: <model.fbx> <pack.fbx>"));
    let pack = PathBuf::from(a.next().expect("missing pack path"));
    let cfg = RigidModelConfig { force_single_bind: true, ..Default::default() };

    match preview_model_with_animation_sources(&model, &[pack.clone()], cfg) {
        Ok(pkg) => {
            let r = &pkg.report;
            println!("COMPATIBLE: cooked {} joints, {} clips total", r.joints, r.clip_frames.len());
            for (name, frames) in &r.clip_frames {
                println!("    {name:44} {frames} frames");
            }
        }
        Err(e) => println!("INCOMPATIBLE / error: {e}"),
    }
}
