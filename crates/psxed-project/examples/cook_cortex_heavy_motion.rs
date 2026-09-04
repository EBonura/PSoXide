use psxed_project::model_import::{preview_model_with_animation_sources, RigidModelConfig};
fn main() {
    let root = std::path::PathBuf::from(std::env::args().nth(1).expect("project root"));
    let base = root.join("source_assets/animations/heavy_enemy/model_reference_idle_02.glb");
    let existing = std::fs::read(
        root.join("assets/models/tank_boss_animated_model/tank_boss_animated_model.psxmdl"),
    )
    .unwrap();
    let old = psx_asset::Model::from_bytes(&existing).unwrap();
    for (name, hz) in [("alert", 12), ("turn", 12)] {
        let config = RigidModelConfig {
            animation_fps: hz,
            world_height: 1536,
            extra_animations_affect_bounds: false,
            ignore_embedded_animations: false,
            collapse_bone_patterns: vec![],
            ..Default::default()
        };
        let package = preview_model_with_animation_sources(
            &base,
            &[root.join(format!("source_assets/animations/heavy_enemy/{name}.glb"))],
            config,
        )
        .unwrap();
        let new = psx_asset::Model::from_bytes(&package.model).unwrap();
        println!("MODEL {name}: existing joints={} scale={} vertices={} generated joints={} scale={} vertices={}",old.joint_count(),old.local_to_world_q12(),old.vertex_count(),new.joint_count(),new.local_to_world_q12(),new.vertex_count());
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
        let clip = package.clips.last().unwrap();
        println!("CLIP {name} {} bytes", clip.bytes.len());
        std::fs::write(
            root.join(format!("assets/animations/tank_boss_ai/{name}.psxanim")),
            &clip.bytes,
        )
        .unwrap();
    }
}
