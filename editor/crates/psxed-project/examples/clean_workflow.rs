//! Demonstrate the clean separated pipeline:
//!   1. import the model ALONE (mesh + skeleton, no baked library)
//!   2. import the animation pack ONCE as a skeleton-shared library
//!   3. Character = Model + shared AnimationSet
//!
//!   cargo run -p psxed-project --example clean_workflow -- \
//!       <project.ron> <model.fbx> <pack.fbx> "<ModelName>" "<LibraryName>"

use std::path::PathBuf;

use psxed_project::model_import::{import_animation_library, import_model_only};
use psxed_project::{NodeKind, ProjectDocument, ResourceData};
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
    let cfg = RigidModelConfig { force_single_bind: true, ..Default::default() };

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

    // 1. MODEL ONLY (no auto-library)
    let model_id = import_model_only(&mut project, &model_fbx, &model_name, &root, cfg.clone())
        .expect("import_model_only");
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
                ch.animation_set = Some(lib.set_id);
                ch.idle_clip = None;  // resolved from the shared set
                ch.walk_clip = None;
                ch.run_clip = None;
                ch.turn_clip = None;
                ch.roll_clip = None;
                ch.backstep_clip = None;
                ch.action_clips.clear();
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

    project.save_to_path(&project_path).expect("save");
    println!("Saved {}", project_path.display());
}
