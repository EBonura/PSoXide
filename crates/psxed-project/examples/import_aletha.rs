//! One-shot headless import of an artist-delivered character GLB
//! (model + on-rig clips) into a project, the same way the editor's
//! import UI would do it: model bundle first, then the animation
//! library against the same source, which auto-binds action slots by
//! clip name and registers the Animation Set.
//!
//! Usage:
//!   cargo run -p psxed-project --example import_aletha -- <project_root> <source.glb> <name>

use std::path::PathBuf;

use psxed_project::model_import::{import_animation_library, import_model_with_animation_sources};
use psxed_project::{NodeKind, ProjectDocument, ResourceData};

fn main() {
    let mut args = std::env::args().skip(1);
    let project_root = PathBuf::from(args.next().expect("project root"));
    let source = PathBuf::from(args.next().expect("source glb"));
    let name = args.next().expect("output name");

    let project_path = project_root.join("project.ron");
    let mut project = ProjectDocument::load_from_path(&project_path).expect("load project.ron");

    let config = psxed_gltf::RigidModelConfig {
        // Match the incumbent player model's world height so the new
        // character lands at a comparable on-screen scale before tuning.
        world_height: 1024,
        // 12 Hz instead of the 15 Hz default: the 25-clip delivery at
        // 15 Hz overflows the persistent-asset arena (and PS1 RAM) by
        // ~35 KB. Period-typical rate; revisit per-clip once the
        // importer can vary the rate by clip length.
        animation_fps: 12,
        ..Default::default()
    };

    let model_id = import_model_with_animation_sources(
        &mut project,
        &source,
        &[],
        &name,
        &project_root,
        config.clone(),
    )
    .expect("model import");
    println!("model resource: {model_id:?}");

    let library = import_animation_library(
        &mut project,
        &source,
        &[],
        &name,
        &project_root,
        config,
        None,
    )
    .expect("animation library import");
    println!(
        "skeleton {:?}, set {:?}, {} clips",
        library.skeleton_id,
        library.set_id,
        library.clip_ids.len()
    );

    // Report the action bindings the name guesser produced, so the
    // console log doubles as the mapping review.
    if let Some(resource) = project.resource(library.set_id) {
        if let ResourceData::AnimationSet(set) = &resource.data {
            for binding in &set.action_clips {
                let clip = project
                    .resource(binding.clip)
                    .map(|r| r.name.as_str())
                    .unwrap_or("?");
                println!("  {:?} <- {}", binding.action, clip);
            }
        }
    }

    // Rebind the incumbent player: the Character named "Aletha" moves to
    // the delivered model + set (material untouched), and any scene
    // ModelRenderer that showed her old model follows. Renderers showing
    // other models (e.g. cortex_v4's Crimson Cross Knight) are left alone.
    let mut old_model = None;
    for resource in &mut project.resources {
        if resource.name != "Aletha" {
            continue;
        }
        if let ResourceData::Character(character) = &mut resource.data {
            old_model = character.model;
            character.model = Some(model_id);
            character.animation_set = Some(library.set_id);
            println!(
                "rebound Character 'Aletha': model {old_model:?} -> {model_id:?}, set -> {:?}",
                library.set_id
            );
        }
    }
    if let Some(old_model) = old_model {
        for scene in &mut project.scenes {
            let targets: Vec<_> = scene
                .nodes()
                .iter()
                .filter(|node| {
                    matches!(node.kind, NodeKind::ModelRenderer { model, .. } if model == Some(old_model))
                })
                .map(|node| node.id)
                .collect();
            for id in targets {
                if let Some(node) = scene.node_mut(id) {
                    if let NodeKind::ModelRenderer { model, .. } = &mut node.kind {
                        *model = Some(model_id);
                        println!("repointed ModelRenderer node in scene '{}'", scene.name);
                    }
                }
            }
        }
    }

    project
        .save_to_path(&project_path)
        .expect("save project.ron");
    println!("saved {}", project_path.display());
}
