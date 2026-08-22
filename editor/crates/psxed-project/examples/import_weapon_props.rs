//! One-shot headless import of artist-delivered rigid weapon props:
//! cook each GLB as a static 1-joint model, register the generated
//! bind-pose clip (the cook demands >=1 clip per model and the
//! equipment draw skips clipless weapons), create Weapon resources,
//! give every named humanoid hand a `right_hand_grip`
//! socket, and hang Equipment components on the scene's character
//! entities so the swords render in Play.
//!
//! Rerun-safe: aborts if the sword resources already exist. The first
//! run stashes `logs/project.ron.pre-weapons.bak`.
//!
//! Usage:
//!   cargo run -p psxed-project --example import_weapon_props -- \
//!       <project.ron> <props_export_dir>

use std::path::PathBuf;

use psxed_project::model_import::{
    import_model_with_animation_sources, preview_model_with_animation_sources,
};
use psxed_project::{
    AnimationClipBakeKind, AnimationClipResource, AnimationRole, AttachmentSocket, NodeKind,
    ProjectDocument, ResourceData, ResourceId, WeaponResource,
};

// Character sources (idle.glb / Aletha.glb) span 1.878 units at
// world_height 1024, so 1 source unit = 545.3 engine units. Sword
// world heights come from their measured GLB Y spans (1.037 / 1.408).
const SWORDS: [(&str, &str, u16); 2] = [
    ("sword1_light.glb", "Sword1 Light", 566),
    ("sword1_heavy.glb", "Sword1 Heavy", 768),
];

fn right_hand_joint(project: &ProjectDocument, model_id: ResourceId) -> Option<u16> {
    let model = sword_model(project, model_id);
    let skeleton = model.skeleton.and_then(|id| project.resource(id))?;
    let ResourceData::Skeleton(skeleton) = &skeleton.data else {
        return None;
    };
    skeleton
        .joint_names
        .iter()
        .position(|name| {
            let leaf = name.rsplit([':', '|', '/']).next().unwrap_or(name);
            leaf.eq_ignore_ascii_case("RightHand") || leaf.eq_ignore_ascii_case("Right_Hand")
        })
        .and_then(|joint| u16::try_from(joint).ok())
}

fn sword_model(project: &ProjectDocument, id: ResourceId) -> &psxed_project::ModelResource {
    match &project.resource(id).expect("sword model resource").data {
        ResourceData::Model(model) => model,
        other => panic!("expected Model resource, got {other:?}"),
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let usage = "usage: import_weapon_props <project.ron> <props_export_dir>";
    let project_path = PathBuf::from(args.next().expect(usage));
    let props_dir = PathBuf::from(args.next().expect(usage));
    let project_root = project_path
        .parent()
        .expect("project.ron parent")
        .to_path_buf();

    let mut project = ProjectDocument::load_from_path(&project_path).expect("load project.ron");
    for (_, name, _) in SWORDS {
        assert!(
            !project.resources.iter().any(|r| r.name == name),
            "{name} already exists; remove the sword resources before rerunning"
        );
    }

    let backup = project_root
        .join("logs")
        .join("project.ron.pre-weapons.bak");
    if !backup.exists() {
        std::fs::create_dir_all(backup.parent().unwrap()).expect("logs dir");
        std::fs::copy(&project_path, &backup).expect("backup project.ron");
        println!("backup: {}", backup.display());
    }

    // Keep the artist sources inside the project tree.
    let src_dir = project_root.join("source_assets").join("props");
    std::fs::create_dir_all(&src_dir).expect("source_assets/props");

    let mut model_ids: Vec<ResourceId> = Vec::new();
    let mut clip_bytes: Vec<Vec<u8>> = Vec::new();
    for (file, name, world_height) in SWORDS {
        let dst = src_dir.join(file);
        std::fs::copy(props_dir.join(file), &dst)
            .unwrap_or_else(|e| panic!("copy {} into project: {e}", file));

        let config = psxed_gltf::RigidModelConfig {
            // The shared enemy atlas is authored at 256 px.
            texture_width: 256,
            texture_height: 256,
            world_height,
            ..Default::default()
        };
        // Preview first to capture the generated clips;
        // import_model_with_animation_sources writes only model+atlas.
        let package =
            preview_model_with_animation_sources(&dst, &[], config.clone()).expect("cook preview");
        let report = &package.report;
        println!(
            "{name}: {} src verts -> {} cooked, {} faces, {} joint(s), clips {:?}",
            report.source_vertices,
            report.cooked_vertices,
            report.faces,
            report.joints,
            report.clip_frames,
        );
        let bind = package
            .clips
            .iter()
            .find(|c| c.sanitized_name == "bind_pose")
            .expect("static cook emits a bind_pose clip");
        clip_bytes.push(bind.bytes.clone());

        let model_id = import_model_with_animation_sources(
            &mut project,
            &dst,
            &[],
            name,
            &project_root,
            config,
        )
        .expect("model import");
        model_ids.push(model_id);
    }

    // Both swords dedupe onto the same 1-joint skeleton, and a 1-joint
    // bind pose has no model-bounds-dependent translation to quantize,
    // so the clips should cook byte-identical. Verify instead of assume;
    // fall back to per-model clips if they ever diverge.
    let skeletons: Vec<ResourceId> = model_ids
        .iter()
        .map(|id| sword_model(&project, *id).skeleton.expect("sword skeleton"))
        .collect();
    let bundle_dirs: Vec<PathBuf> = model_ids
        .iter()
        .map(|id| {
            project_root
                .join(&sword_model(&project, *id).model_path)
                .parent()
                .expect("bundle dir")
                .to_path_buf()
        })
        .collect();
    let clip_resource = |path: String, skeleton, target_model| {
        ResourceData::AnimationClip(AnimationClipResource {
            psxanim_path: path,
            skeleton: Some(skeleton),
            target_model,
            source: None,
            bake: AnimationClipBakeKind::LegacyShared,
            role: AnimationRole::Idle,
            looping: true,
            tags: vec!["prop".to_string()],
            calibration: Default::default(),
            pose_corrections: Vec::new(),
        })
    };
    if clip_bytes[0] == clip_bytes[1] && skeletons[0] == skeletons[1] {
        let path = bundle_dirs[0].join("prop_bind_pose.psxanim");
        std::fs::write(&path, &clip_bytes[0]).expect("write bind_pose clip");
        let rel = path
            .strip_prefix(&project_root)
            .expect("clip under project root")
            .to_string_lossy()
            .into_owned();
        project.add_resource("prop_bind_pose", clip_resource(rel, skeletons[0], None));
        println!("registered shared prop_bind_pose clip");
    } else {
        for (i, (_, name, _)) in SWORDS.iter().enumerate() {
            let path = bundle_dirs[i].join("bind_pose.psxanim");
            std::fs::write(&path, &clip_bytes[i]).expect("write bind_pose clip");
            let rel = path
                .strip_prefix(&project_root)
                .expect("clip under project root")
                .to_string_lossy()
                .into_owned();
            project.add_resource(
                format!("{name} bind_pose"),
                clip_resource(rel, skeletons[i], Some(model_ids[i])),
            );
        }
        println!("bind_pose clips diverged; registered per-model clips");
    }

    // The swords sample the enemy's atlas and the texture cook is
    // deterministic, so identical inputs give identical .psxt bytes.
    // Share one file when a byte compare agrees, preferring the
    // already-resident enemy atlas.
    let mantis_atlas = project.resources.iter().find_map(|r| match &r.data {
        ResourceData::Model(m) if r.name == "Rust Mantis" => m.texture_path.clone(),
        _ => None,
    });
    let sword_atlases: Vec<Option<String>> = model_ids
        .iter()
        .map(|id| sword_model(&project, *id).texture_path.clone())
        .collect();
    let bytes_of = |rel: &Option<String>| {
        rel.as_ref()
            .and_then(|p| std::fs::read(project_root.join(p)).ok())
    };
    let mantis_bytes = bytes_of(&mantis_atlas);
    let sword_bytes: Vec<_> = sword_atlases.iter().map(bytes_of).collect();
    let repoint = |project: &mut ProjectDocument, id: ResourceId, target: &str| {
        let old = {
            let ResourceData::Model(m) = &mut project.resource_mut(id).unwrap().data else {
                unreachable!()
            };
            m.texture_path.replace(target.to_string())
        };
        if let Some(old) = old {
            if old != target {
                let _ = std::fs::remove_file(project_root.join(old));
            }
        }
    };
    let mut shared_with_mantis = false;
    if let (Some(target), Some(mb)) = (&mantis_atlas, &mantis_bytes) {
        if sword_bytes.iter().all(|b| b.as_ref() == Some(mb)) {
            for id in &model_ids {
                repoint(&mut project, *id, target);
            }
            println!("swords share the Rust Mantis atlas ({target})");
            shared_with_mantis = true;
        }
    }
    if !shared_with_mantis && sword_bytes[0].is_some() && sword_bytes[0] == sword_bytes[1] {
        let target = sword_atlases[0].clone().unwrap();
        repoint(&mut project, model_ids[1], &target);
        println!("Sword1 Heavy shares Sword1 Light's atlas ({target})");
    }

    // Socket only a named right hand. Guessing from joint count attached the
    // first campaign pass to a left-hand chain tip on the shared Mantis rig;
    // missing names are now a hard authoring seam, never runtime truth.
    let wielder_models: Vec<ResourceId> = project
        .resources
        .iter()
        .filter_map(|r| match &r.data {
            ResourceData::Character(c) => c.model,
            _ => None,
        })
        .collect();
    for id in wielder_models {
        let Some(joint) = right_hand_joint(&project, id) else {
            println!(
                "{}: no named RightHand joint; socket intentionally not guessed",
                project
                    .resource(id)
                    .map(|r| r.name.as_str())
                    .unwrap_or("model")
            );
            continue;
        };
        let Some(resource) = project.resource_mut(id) else {
            continue;
        };
        let name = resource.name.clone();
        let ResourceData::Model(model) = &mut resource.data else {
            continue;
        };
        if model
            .attachments
            .iter()
            .any(|s| s.name == "right_hand_grip")
        {
            continue;
        }
        model.attachments.push(AttachmentSocket {
            joint,
            ..AttachmentSocket::right_hand_grip()
        });
        println!("{name}: added right_hand_grip socket on named joint {joint}");
    }

    let mut weapon_ids: Vec<ResourceId> = Vec::new();
    for (i, (_, name, _)) in SWORDS.iter().enumerate() {
        let id = project.add_resource(
            *name,
            ResourceData::Weapon(WeaponResource {
                model: Some(model_ids[i]),
                ..WeaponResource::defaults()
            }),
        );
        weapon_ids.push(id);
    }

    // Equip: light sword to the player, heavy to everyone else.
    for scene in &mut project.scenes {
        let mut targets = Vec::new();
        for node in scene.nodes() {
            if let NodeKind::CharacterController { player, .. } = &node.kind {
                let Some(entity) = node.parent else { continue };
                let already = scene.nodes().iter().any(|n| {
                    n.parent == Some(entity) && matches!(n.kind, NodeKind::Equipment { .. })
                });
                if !already {
                    targets.push((entity, *player));
                }
            }
        }
        for (entity, player) in targets {
            let weapon = if player { weapon_ids[0] } else { weapon_ids[1] };
            scene.add_node(
                entity,
                "Equipment",
                NodeKind::Equipment {
                    weapon: Some(weapon),
                    character_socket: "right_hand_grip".to_string(),
                    weapon_grip: "grip".to_string(),
                },
            );
            println!(
                "scene '{}': equipped {} on entity {entity:?}",
                scene.name,
                if player {
                    "Sword1 Light (player)"
                } else {
                    "Sword1 Heavy"
                },
            );
        }
    }

    project
        .save_to_path(&project_path)
        .expect("save project.ron");
    println!("saved {}", project_path.display());
}
