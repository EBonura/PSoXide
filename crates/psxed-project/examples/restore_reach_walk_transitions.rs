//! Restore the approved v4 walk transitions from a recovered project snapshot.
//! Usage: cargo run -p psxed-project --example restore_reach_walk_transitions -- SOURCE/project.ron TARGET/project.ron
//! Only the three clip/source resources and their action options are transferred.
use psxed_project::{ProjectDocument, ResourceData, ResourceId};
use std::{fs, path::PathBuf};
fn named(p: &ProjectDocument, name: &str) -> ResourceId {
    p.resources.iter().find(|r| r.name == name).expect(name).id
}
fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    assert_eq!(args.len(), 2);
    let src_path = PathBuf::from(&args[0]);
    let dst_path = PathBuf::from(&args[1]);
    let src = ProjectDocument::load_from_path(&src_path).unwrap();
    let mut dst = ProjectDocument::load_from_path(&dst_path).unwrap();
    let before = dst.to_ron_string().unwrap();
    let scene_before = ron::ser::to_string(&dst.scenes).unwrap();
    let set_name = "Aletha Delivered Animation Set";
    let ResourceData::AnimationSet(src_set) = &src.resource(named(&src, set_name)).unwrap().data
    else {
        panic!("animation set")
    };
    for name in [
        "gen_walk_fwd_windup",
        "gen_walk_fwd_winddown",
        "gen_walk_fwd_winddown_mirror",
    ] {
        let src_id = named(&src, name);
        let dst_id = named(&dst, name);
        let ResourceData::AnimationClip(mut clip) = src.resource(src_id).unwrap().data.clone()
        else {
            panic!("clip")
        };
        assert!(clip.preserve_samples && !clip.calibration.in_place);
        // Validate the skeleton and target-model identities instead of silently
        // applying a skin-space animation to an unrelated character.
        for id in [clip.skeleton, clip.target_model].into_iter().flatten() {
            let a = src.resource(id).unwrap();
            let b = dst.resource(id).unwrap();
            assert_eq!(a.name, b.name);
            if matches!(a.data, ResourceData::Skeleton(_)) {
                assert_eq!(a.data, b.data);
            }
        }
        let source = src.resource(clip.source.unwrap()).unwrap();
        let ResourceData::AnimationSource(source_data) = source.data.clone() else {
            panic!("source")
        };
        let new_source = if let Some(old) = dst.resources.iter().find(|r| r.name == source.name) {
            assert_eq!(old.data, source.data);
            old.id
        } else {
            dst.add_resource(
                source.name.clone(),
                ResourceData::AnimationSource(source_data.clone()),
            )
        };
        clip.source = Some(new_source);
        let path = PathBuf::from(&clip.psxanim_path);
        assert!(!path.is_absolute() && !clip.psxanim_path.contains(".."));
        let bytes = fs::read(src_path.parent().unwrap().join(&path)).unwrap();
        let anim = psx_asset::Animation::from_bytes(&bytes).unwrap();
        assert_eq!(anim.sample_rate_hz(), 30);
        let frames = if name.ends_with("windup") { 22 } else { 30 };
        assert_eq!(anim.frame_count(), frames);
        for joint in 0..anim.joint_count() {
            assert_eq!(anim.pose(frames - 2, joint), anim.pose(frames - 1, joint));
        }
        let target_file = dst_path.parent().unwrap().join(path);
        fs::create_dir_all(target_file.parent().unwrap()).unwrap();
        fs::write(target_file, bytes).unwrap();
        assert!(
            dst_path
                .parent()
                .unwrap()
                .join(&source_data.source_path)
                .is_file(),
            "approved source must be copied first"
        );
        dst.resource_mut(dst_id).unwrap().data = ResourceData::AnimationClip(clip);
        let binding = src_set
            .action_clips
            .iter()
            .find(|b| b.clip == src_id)
            .unwrap();
        let set_id = named(&dst, set_name);
        let ResourceData::AnimationSet(set) = &mut dst.resource_mut(set_id).unwrap().data else {
            unreachable!()
        };
        let target = set
            .action_clips
            .iter_mut()
            .find(|b| b.action == binding.action)
            .unwrap();
        let mut replacement = binding.clone();
        replacement.clip = dst_id;
        *target = replacement;
        println!(
            "Restored {name}: clip {}, source {}, {frames} frames at 30 Hz",
            dst_id.raw(),
            new_source.raw()
        );
    }
    assert_eq!(
        scene_before,
        ron::ser::to_string(&dst.scenes).unwrap(),
        "level and scene components must remain intact"
    );
    let backup = dst_path
        .parent()
        .unwrap()
        .join("logs/project.ron.before-walk-v4");
    assert!(
        !backup.exists(),
        "backup already exists; inspect before rerunning"
    );
    fs::create_dir_all(backup.parent().unwrap()).unwrap();
    fs::write(backup, before).unwrap();
    dst.save_to_path(&dst_path).unwrap();
    println!("Saved {}", dst_path.display());
}
