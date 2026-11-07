use psxed_project::model_import::{preview_model_with_animation_sources, RigidModelConfig};
fn main() {
    let root = std::path::PathBuf::from(std::env::args().nth(1).expect("project root"));
    let config = RigidModelConfig {
        animation_fps: 15,
        world_height: 1024,
        extra_animations_affect_bounds: false,
        ..Default::default()
    };
    let package = preview_model_with_animation_sources(
        &root.join("source_assets/characters/rust_mantis.glb"),
        &[root.join("source_assets/animations/light_enemy/charge_volley.glb")],
        config,
    )
    .unwrap();
    let existing =
        std::fs::read(root.join("assets/models/rust_mantis/rust_mantis.psxmdl")).unwrap();
    let old = psx_asset::Model::from_bytes(&existing).unwrap();
    let new = psx_asset::Model::from_bytes(&package.model).unwrap();
    println!(
        "MODEL existing joints={} scale={} vertices={} generated joints={} scale={} vertices={}",
        old.joint_count(),
        old.local_to_world_q12(),
        old.vertex_count(),
        new.joint_count(),
        new.local_to_world_q12(),
        new.vertex_count()
    );
    assert_eq!(old.joint_count(), new.joint_count());
    assert_eq!(old.local_to_world_q12(), new.local_to_world_q12());
    assert_eq!(old.vertex_count(), new.vertex_count());
    for index in 0..old.joint_count() {
        assert_eq!(
            old.joint(index).unwrap().parent(),
            new.joint(index).unwrap().parent()
        );
    }
    for index in 0..old.vertex_count() {
        let a = old.vertex(index).unwrap().position;
        let b = new.vertex(index).unwrap().position;
        assert!(
            (i32::from(a.x) - i32::from(b.x)).abs() <= 1
                && (i32::from(a.y) - i32::from(b.y)).abs() <= 1
                && (i32::from(a.z) - i32::from(b.z)).abs() <= 1,
            "different quantization frame"
        );
    }
    for c in &package.clips {
        println!("CLIP {} {} bytes", c.sanitized_name, c.bytes.len());
    }
    let clip = package.clips.last().unwrap();
    std::fs::write(
        root.join("assets/animations/mantis_combat/charge_volley.psxanim"),
        &clip.bytes,
    )
    .unwrap();
}
