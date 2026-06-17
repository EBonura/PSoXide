//! Wire the five with-skin Mixamo clips into cortex_ignition_v1 as one clean
//! skeleton-scoped library. Each with-skin file carries the model's mantis bind
//! plus a bare `mixamo.com` take (the new clip) and two leftover Armature takes;
//! the `mixamo_com` clip filter keeps only the new clip, so each bake adds
//! exactly one correctly-roled clip with no duplicates.
//!   cargo run -p psxed-project --example wire_mixamo -- <project.ron>

use std::collections::HashMap;
use std::path::PathBuf;

use psxed_project::model_import::{import_animation_library, import_model_with_animation_sources};
use psxed_project::{
    AnimationClipBakeKind, AnimationClipResource, AnimationRole, AnimationSetResource, NodeKind,
    ProjectDocument, ResourceData, ResourceId,
};
use psxed_gltf::RigidModelConfig;

fn main() {
    let project_path = PathBuf::from(std::env::args().nth(1).expect("usage: <project.ron>"));
    let root = project_path.parent().unwrap().to_path_buf();
    let dl = PathBuf::from(std::env::var("HOME").unwrap()).join("Downloads");
    let model_fbx = dl.join("Enemy_01_AnimTest01 (1).fbx");
    let cfg = RigidModelConfig {
        force_single_bind: true,
        double_sided: true,
        ignore_embedded_animations: true,
        // Clips must quantize against the SAME bounds as the cooked model, since
        // the runtime decodes .psxanim joint translations using the model's
        // center/extent. Letting each clip inflate its own bounds desyncs them
        // and distorts poses (worst on the run, which moves joints farthest).
        extra_animations_affect_bounds: false,
        ..Default::default()
    };

    // (file stem, clip name, role, looping)
    let specs: [(&str, &str, AnimationRole, bool); 3] = [
        ("Idle (2)", "idle", AnimationRole::Idle, true),
        ("Walking (1)", "walk", AnimationRole::Walk, true),
        ("Drunk Run Forward", "run", AnimationRole::Run, true),
    ];

    let mut project = ProjectDocument::load_from_path(&project_path).unwrap();

    // capture players + old (non-barricade) models + renderers to repoint
    let mut players = Vec::new();
    for s in &project.scenes {
        for n in s.nodes() {
            if let NodeKind::CharacterController { character: Some(id), .. }
            | NodeKind::SpawnPoint { character: Some(id), .. } = &n.kind
            {
                players.push(*id);
            }
        }
    }
    let old: Vec<_> = project
        .resources
        .iter()
        .filter_map(|r| match &r.data {
            ResourceData::Model(m) if !m.model_path.contains("barricade") => Some(r.id),
            _ => None,
        })
        .collect();
    let mut repoint = Vec::new();
    for (si, s) in project.scenes.iter().enumerate() {
        for n in s.nodes() {
            if let NodeKind::ModelRenderer { model: Some(m), .. } = &n.kind {
                if old.contains(m) {
                    repoint.push((si, n.id));
                }
            }
        }
    }
    // prune any pre-existing animation library
    let lib: Vec<_> = project
        .resources
        .iter()
        .filter(|r| {
            matches!(
                r.data,
                ResourceData::AnimationSource(_)
                    | ResourceData::AnimationClip(_)
                    | ResourceData::AnimationSet(_)
            )
        })
        .map(|r| r.id)
        .collect();
    for id in lib {
        project.delete_resource(id);
    }

    // model geometry
    let model_id =
        import_model_with_animation_sources(&mut project, &model_fbx, &[], "Rust Mantis", &root, cfg.clone())
            .unwrap();

    // bake each clip individually (one clip per file, role known by file)
    let mut skeleton_id = None;
    let mut role_clip: Vec<(&str, ResourceId)> = Vec::new();
    for (stem, name, role, looping) in specs {
        let pack = vec![dl.join(format!("{stem}.fbx"))];
        let imp =
            import_animation_library(&mut project, &model_fbx, &pack, name, &root, cfg.clone(), None)
                .unwrap_or_else(|e| panic!("import {stem}: {e:?}"));
        skeleton_id = Some(imp.skeleton_id);
        // drop the auto-generated set; we build one explicitly below
        project.delete_resource(imp.set_id);
        // The new clip is named after the file (e.g. "idle_1"); the leftover
        // takes carried in every file are named "armature_*". Keep the former,
        // discard the latter so the library holds exactly the five new clips.
        let clip = imp
            .clip_ids
            .iter()
            .copied()
            .find(|id| {
                project
                    .resource(*id)
                    .map(|r| !r.name.starts_with("armature_"))
                    .unwrap_or(false)
            })
            .unwrap_or_else(|| panic!("no new clip baked from {stem}"));
        for junk in imp.clip_ids.iter().copied().filter(|id| *id != clip) {
            project.delete_resource(junk);
        }
        if let Some(res) = project.resource_mut(clip) {
            res.name = name.to_string();
            if let ResourceData::AnimationClip(c) = &mut res.data {
                c.role = role;
                c.looping = looping;
            }
        }
        println!("clip {name:18} <- {stem:36} {clip:?} role {role:?}");
        role_clip.push((name, clip));
    }

    let by: HashMap<&str, ResourceId> = role_clip.iter().copied().collect();
    let set = AnimationSetResource {
        skeleton: skeleton_id,
        idle_clip: by.get("idle").copied(),
        walk_clip: by.get("walk").copied(),
        run_clip: by.get("run").copied(),
        clips: role_clip.iter().map(|(_, id)| *id).collect(),
        ..Default::default()
    };
    let set_id = project.add_resource("Rust Mantis Animation Set", ResourceData::AnimationSet(set));

    // wire players to the model + set
    for id in players.iter().copied() {
        if let Some(res) = project.resource_mut(id) {
            if let ResourceData::Character(ch) = &mut res.data {
                ch.model = Some(model_id);
                ch.animation_set = Some(set_id);
            }
        }
    }
    // cleanup: delete other characters + old models; repoint renderers; clear animators
    let others: Vec<_> = project
        .resources
        .iter()
        .filter_map(|r| match &r.data {
            ResourceData::Character(_) if !players.contains(&r.id) => Some(r.id),
            _ => None,
        })
        .collect();
    for id in others {
        project.delete_resource(id);
    }
    for id in old {
        project.delete_resource(id);
    }
    for (si, nid) in repoint {
        if let Some(n) = project.scenes[si].node_mut(nid) {
            if let NodeKind::ModelRenderer { model, .. } = &mut n.kind {
                *model = Some(model_id);
            }
        }
    }
    for s in &mut project.scenes {
        let ids: Vec<_> = s.nodes().iter().map(|n| n.id).collect();
        for id in ids {
            if let Some(n) = s.node_mut(id) {
                if let NodeKind::Animator { action_clips, clip, autoplay, .. } = &mut n.kind {
                    action_clips.clear();
                    *clip = None;
                    *autoplay = true;
                }
            }
        }
    }
    // barricade (geometry-only) bind pose so every model stays renderable
    let models: Vec<_> = project
        .resources
        .iter()
        .filter_map(|r| match &r.data {
            ResourceData::Model(m) => Some((r.id, m.skeleton, m.model_path.clone())),
            _ => None,
        })
        .collect();
    for (mid, skel, mp) in models {
        if !project.resolved_model_animation_clips(mid).is_empty() {
            continue;
        }
        let Some(skel) = skel else { continue };
        let b = root.join(&mp);
        let Some(d) = b.parent() else { continue };
        if let Some(px) = std::fs::read_dir(d).ok().and_then(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .find(|p| p.extension().and_then(|x| x.to_str()) == Some("psxanim"))
        }) {
            let rel = px.strip_prefix(&root).unwrap_or(&px).to_string_lossy().replace('\\', "/");
            let nm = px.file_stem().and_then(|s| s.to_str()).unwrap_or("bind").to_string();
            project.add_resource(
                nm,
                ResourceData::AnimationClip(AnimationClipResource {
                    psxanim_path: rel,
                    skeleton: Some(skel),
                    source: None,
                    bake: AnimationClipBakeKind::LegacyShared,
                    role: AnimationRole::Generic,
                    looping: false,
                    tags: Vec::new(),
                    calibration: Default::default(),
                }),
            );
        }
    }
    project.save_to_path(&project_path).unwrap();
    println!("saved {}", project_path.display());
}
