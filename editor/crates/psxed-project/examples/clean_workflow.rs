//! Demonstrate the clean separated pipeline:
//!   1. import the model ALONE (mesh + skeleton, no baked library)
//!   2. import the animation pack ONCE as a skeleton-shared library
//!   3. Character = Model + shared AnimationSet
//!
//!   cargo run -p psxed-project --example clean_workflow -- \
//!       <project.ron> <model.fbx> <pack.fbx> "<ModelName>" "<LibraryName>"

use std::path::PathBuf;

use psxed_project::model_import::{import_animation_library, import_model_with_animation_sources};
use psxed_project::{
    AnimationClipBakeKind, AnimationClipResource, AnimationRole, NodeKind, ProjectDocument,
    ResourceData,
};
use psxed_gltf::RigidModelConfig;

const KEEP_MODEL_MARKER: &str = "ps1_clean_power_barricade";

fn curated() -> Vec<String> {
    [
        "armature_idle_loop",
        "armature_walk_loop",
        "armature_jog_fwd_loop",
        "armature_sword_attack",
        "armature_death01",
        "armature_hit_chest",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let project_path = PathBuf::from(args.next().expect("usage: <project.ron> <model.fbx> <pack.fbx> <ModelName> <LibName>"));
    let model_fbx = PathBuf::from(args.next().expect("model fbx"));
    let pack_fbx = PathBuf::from(args.next().expect("pack fbx"));
    let model_name = args.next().unwrap_or_else(|| "Rust Mantis".into());
    let lib_name = args.next().unwrap_or_else(|| "Biped Library".into());
    let root = project_path.parent().unwrap().to_path_buf();
    // prune_detached_face_islands=0: keep hand-authored separate pieces
    // (armor plates, detail bits). The default (4) is tuned for Meshy
    // auto-gen scraps and wrongly deletes small intentional parts.
    let cfg = RigidModelConfig {
        force_single_bind: true,
        prune_detached_face_islands: 0,
        ..Default::default()
    };

    let mut project = ProjectDocument::load_from_path(&project_path).expect("load");

    // Wipe any pre-existing animation library cruft so the project ends
    // up reflecting only the clean separated structure we build below.
    let legacy: Vec<_> = project.resources.iter().filter(|r| matches!(
        r.data,
        ResourceData::AnimationSource(_) | ResourceData::AnimationClip(_) | ResourceData::AnimationSet(_)
    )).map(|r| r.id).collect();
    let legacy_n = legacy.len();
    for id in legacy { project.delete_resource(id); }
    println!("pruned {legacy_n} pre-existing animation-library resources");

    // capture players + renderers + old models to clean up
    let mut player_chars = Vec::new();
    for scene in &project.scenes {
        for node in scene.nodes() {
            if let NodeKind::CharacterController { character: Some(id), .. }
            | NodeKind::SpawnPoint { character: Some(id), .. } = &node.kind { player_chars.push(*id); }
        }
    }
    let old_models: Vec<_> = project.resources.iter().filter_map(|r| match &r.data {
        ResourceData::Model(m) if !m.model_path.contains(KEEP_MODEL_MARKER) => Some(r.id),
        _ => None,
    }).collect();
    let mut repoint = Vec::new();
    for (si, scene) in project.scenes.iter().enumerate() {
        for node in scene.nodes() {
            if let NodeKind::ModelRenderer { model: Some(mid), .. } = &node.kind {
                if old_models.contains(mid) { repoint.push((si, node.id)); }
            }
        }
    }

    // 1. MODEL ONLY (registration never auto-builds a library)
    let model_id =
        import_model_with_animation_sources(&mut project, &model_fbx, &[], &model_name, &root, cfg.clone())
            .expect("import model");
    println!("imported model only: {model_id:?} '{model_name}'");

    // 2. SHARED ANIMATION LIBRARY (one bake, target_model=None)
    let lib = import_animation_library(
        &mut project, &model_fbx, &[pack_fbx], &lib_name, &root, cfg, Some(&curated()),
    ).expect("import_animation_library");
    println!("imported shared library '{lib_name}': set {:?}, {} clips, skeleton {:?}",
        lib.set_id, lib.clip_ids.len(), lib.skeleton_id);

    // 3. CHARACTER = model + shared set (roles resolved from the set)
    for id in player_chars.iter().copied() {
        let name = format!("{model_name} Player");
        if let Some(res) = project.resource_mut(id) {
            res.name = name;
            if let ResourceData::Character(ch) = &mut res.data {
                ch.model = Some(model_id);
                // Animations resolve entirely from the shared Animation Set.
                ch.animation_set = Some(lib.set_id);
            }
        }
    }

    // cleanup: delete other characters + old models; repoint; clear animators
    let other_chars: Vec<_> = project.resources.iter().filter_map(|r| match &r.data {
        ResourceData::Character(_) if !player_chars.contains(&r.id) => Some(r.id), _ => None,
    }).collect();
    for id in other_chars { project.delete_resource(id); }
    for id in old_models { project.delete_resource(id); }
    for (si, node_id) in repoint {
        if let Some(node) = project.scenes[si].node_mut(node_id) {
            if let NodeKind::ModelRenderer { model, .. } = &mut node.kind { *model = Some(model_id); }
        }
    }
    for scene in &mut project.scenes {
        let ids: Vec<_> = scene.nodes().iter().map(|n| n.id).collect();
        for id in ids {
            if let Some(node) = scene.node_mut(id) {
                if let NodeKind::Animator { action_clips, clip, autoplay, .. } = &mut node.kind {
                    if !action_clips.is_empty() || clip.is_some() {
                        action_clips.clear(); *clip = None; *autoplay = true;
                    }
                }
            }
        }
    }

    // Ensure every Model is renderable: a geometry-only model with no
    // skeleton clips (e.g. a static prop) gets its bundle's bind-pose
    // .psxanim registered as a skeleton-scoped AnimationClip.
    let models: Vec<_> = project.resources.iter().filter_map(|r| match &r.data {
        ResourceData::Model(m) => Some((r.id, m.skeleton, m.model_path.clone())),
        _ => None,
    }).collect();
    for (model_id, skeleton, model_path) in models {
        if !project.resolved_model_animation_clips(model_id).is_empty() {
            continue;
        }
        let Some(skeleton) = skeleton else { continue };
        let bundle = root.join(&model_path);
        let Some(dir) = bundle.parent() else { continue };
        let psxanim = std::fs::read_dir(dir).ok().and_then(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .find(|p| p.extension().and_then(|x| x.to_str()) == Some("psxanim"))
        });
        let Some(psxanim) = psxanim else { continue };
        let rel = psxanim.strip_prefix(&root).unwrap_or(&psxanim).to_string_lossy().replace('\\', "/");
        let name = psxanim.file_stem().and_then(|s| s.to_str()).unwrap_or("bind_pose").to_string();
        project.add_resource(name, ResourceData::AnimationClip(AnimationClipResource {
            psxanim_path: rel,
            skeleton: Some(skeleton),
            source: None,
            bake: AnimationClipBakeKind::LegacyShared,
            role: AnimationRole::Generic,
            looping: false,
            tags: Vec::new(),
            calibration: Default::default(),
        }));
        println!("registered bind-pose clip for geometry-only model {model_id:?}");
    }

    project.save_to_path(&project_path).expect("save");
    println!("Saved {}", project_path.display());
}
