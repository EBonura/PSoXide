//! Validate the Reach transfer, including the bytes consumed by the runtime.
use psxed_project::{playtest::build_package, ProjectDocument, ResourceData};
use std::{fs, path::Path};
fn main() {
    let path = std::env::args().nth(1).expect("project.ron path");
    let path = Path::new(&path);
    let project = ProjectDocument::load_from_path(path).unwrap();
    let root = path.parent().unwrap();
    let before =
        ProjectDocument::load_from_path(root.join("logs/project.ron.before-walk-v4")).unwrap();
    assert_eq!(
        ron::ser::to_string(&before.scenes).unwrap(),
        ron::ser::to_string(&project.scenes).unwrap()
    );
    for old in &before.resources {
        if ![
            "gen_walk_fwd_windup",
            "gen_walk_fwd_winddown",
            "gen_walk_fwd_winddown_mirror",
            "Aletha Delivered Animation Set",
        ]
        .contains(&old.name.as_str())
        {
            assert_eq!(
                &project.resource(old.id).unwrap().data,
                &old.data,
                "unexpected resource change: {}",
                old.name
            );
        }
    }
    let (package, report) = build_package(&project, root);
    assert!(report.is_ok(), "{:?}", report.errors);
    let package = package.unwrap();
    for (name, frames) in [
        ("gen_walk_fwd_windup", 22),
        ("gen_walk_fwd_winddown", 30),
        ("gen_walk_fwd_winddown_mirror", 30),
    ] {
        let resource = project.resources.iter().find(|r| r.name == name).unwrap();
        let ResourceData::AnimationClip(clip) = &resource.data else {
            panic!("clip");
        };
        assert!(clip.preserve_samples && !clip.calibration.in_place);
        let source_bytes = fs::read(root.join(&clip.psxanim_path)).unwrap();
        let source = psx_asset::Animation::from_bytes(&source_bytes).unwrap();
        let cooked = package
            .model_clips
            .iter()
            .find(|c| c.animation_resource == Some(resource.id))
            .unwrap();
        let anim =
            psx_asset::Animation::from_bytes(&package.assets[cooked.animation_asset_index].bytes)
                .unwrap();
        assert_eq!(anim.frame_count(), frames);
        assert_eq!(anim.sample_rate_hz(), 30);
        for joint in 0..anim.joint_count() {
            assert_eq!(anim.pose(frames - 2, joint), anim.pose(frames - 1, joint));
            for frame in 0..frames {
                let a = source.pose(frame, joint).unwrap();
                let b = anim.pose(frame, joint).unwrap();
                assert_eq!(
                    a.matrix, b.matrix,
                    "rotations changed: {name} frame {frame} joint {joint}"
                );
            }
        }
        println!("PASS {name}: {frames} cooked frames at 30 Hz; rotations and endpoint preserved");
    }
    let cooked_animation = |name: &str| {
        let clip = package.model_clips.iter().find(|c| c.name == name).unwrap();
        psx_asset::Animation::from_bytes(&package.assets[clip.animation_asset_index].bytes).unwrap()
    };
    let start = cooked_animation("gen_walk_fwd_windup");
    let walk = cooked_animation("gen_walk_fwd");
    let mut max_rotation_error = 0;
    for joint in 0..start.joint_count() {
        let a = start.pose(start.frame_count() - 2, joint).unwrap();
        let b = walk.pose(0, joint).unwrap();
        assert_eq!(
            a.translation, b.translation,
            "handoff translation at joint {joint}"
        );
        for column in 0..3 {
            for row in 0..3 {
                max_rotation_error = max_rotation_error.max(
                    (i32::from(a.matrix[column][row]) - i32::from(b.matrix[column][row])).abs(),
                );
            }
        }
    }
    assert!(
        max_rotation_error <= 1,
        "handoff rotation error {max_rotation_error}"
    );
    println!("PASS start-to-walk seam: identical joint translations, rotation error <= 1/4096");
    println!("PASS scene geometry/components and all unrelated resources unchanged");
}
