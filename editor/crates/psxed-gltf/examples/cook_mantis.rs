//! Throwaway: cook the axis-corrected Rust Mantis GLB and report what came out.
fn main() {
    let src = std::env::args().nth(1).expect("source glb");
    let out = std::env::args().nth(2).expect("out dir");
    std::fs::create_dir_all(&out).unwrap();
    let cfg = psxed_gltf::RigidModelConfig {
        // PRE-DIVIDE units, matching the project's Model record: the cook
        // divides by 16, so 64 here renders the model 16x too small
        world_height: 1024,
        // the .psxanim carries no bounds of its own and the runtime decodes
        // joint translations using the MODEL's, so a clip that inflates its own
        // bounds desyncs the two and distorts the pose
        extra_animations_affect_bounds: false,
        // the source FBX carries 25 near-static AnimTest takes; only the
        // retargeted gaits passed in as extras should ship
        ignore_embedded_animations: true,
        ..Default::default()
    };
    let anims: Vec<std::path::PathBuf> = std::env::args().skip(3).map(Into::into).collect();
    let pkg = if anims.is_empty() {
        psxed_gltf::convert_rigid_model_path(&src, &cfg).expect("cook")
    } else {
        psxed_gltf::convert_rigid_model_path_with_animation_paths(&src, &anims, &cfg).expect("cook")
    };
    let model = psx_asset::Model::from_bytes(&pkg.model).expect("model");
    let (mut lo, mut hi) = ([i32::MAX; 3], [i32::MIN; 3]);
    for v in 0..model.vertex_count() {
        let p = model.vertex(v).unwrap().position;
        for (a, value) in [p.x, p.y, p.z].iter().enumerate() {
            lo[a] = lo[a].min(i32::from(*value));
            hi[a] = hi[a].max(i32::from(*value));
        }
    }
    println!(
        "model: verts={} parts={} joints={} bounds x[{},{}] y[{},{}] z[{},{}]",
        model.vertex_count(),
        model.part_count(),
        model.joint_count(),
        lo[0], hi[0], lo[1], hi[1], lo[2], hi[2]
    );
    println!("  spans: x={} y={} z={}", hi[0]-lo[0], hi[1]-lo[1], hi[2]-lo[2]);
    std::fs::write(format!("{out}/rust_mantis.psxmdl"), &pkg.model).unwrap();
    if let Some(texture) = &pkg.texture {
        std::fs::write(format!("{out}/rust_mantis.psxt"), texture).unwrap();
    }
    for clip in &pkg.clips {
        let anim = psx_asset::Animation::from_bytes(&clip.bytes).expect("clip");
        println!("  clip '{}' frames={}", clip.source_name.as_deref().unwrap_or("?"), anim.frame_count());
        std::fs::write(
            format!("{out}/{}.psxanim", clip.sanitized_name),
            &clip.bytes,
        )
        .unwrap();
    }
}
