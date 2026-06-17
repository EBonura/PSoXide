//! Diagnostic: import the model's OWN native takes (no retargeting) as the
//! library and play one as idle, to isolate retarget bugs from import/skin bugs.
//!   cargo run -p psxed-project --example skel_diag -- <project.ron> <model.fbx> "<idle clip substr>"

use std::path::PathBuf;
use psxed_project::model_import::{import_animation_library, import_model_with_animation_sources};
use psxed_project::{NodeKind, ProjectDocument, ResourceData};
use psxed_gltf::RigidModelConfig;

fn main() {
    let mut a = std::env::args().skip(1);
    let project_path = PathBuf::from(a.next().unwrap());
    let model_fbx = PathBuf::from(a.next().unwrap());
    let idle_substr = a.next().unwrap_or_else(|| "walk_inplace".into());
    // Trailing args: external animation pack fbx files. When present, clips are
    // baked through the RETARGET path (source rig != target bind) instead of the
    // native direct bake. This is what exposes / validates the retarget math.
    let pack: Vec<PathBuf> = a.map(PathBuf::from).collect();
    let root = project_path.parent().unwrap().to_path_buf();
    let cfg = RigidModelConfig {
        force_single_bind: true,
        double_sided: true,
        ignore_embedded_animations: false,
        animation_fps: 30,
        ..Default::default()
    };
    let mut project = ProjectDocument::load_from_path(&project_path).unwrap();

    let mut players = Vec::new();
    for s in &project.scenes { for n in s.nodes() {
        if let NodeKind::CharacterController{character:Some(id),..}|NodeKind::SpawnPoint{character:Some(id),..}=&n.kind { players.push(*id); }
    }}
    let old: Vec<_> = project.resources.iter().filter_map(|r| match &r.data {
        ResourceData::Model(m) if !m.model_path.contains("barricade") => Some(r.id), _=>None }).collect();
    let mut repoint=Vec::new();
    for (si,s) in project.scenes.iter().enumerate() { for n in s.nodes() {
        if let NodeKind::ModelRenderer{model:Some(m),..}=&n.kind { if old.contains(m){repoint.push((si,n.id));} } }}
    // prune any existing anim library
    let lib: Vec<_> = project.resources.iter().filter(|r| matches!(r.data,
        ResourceData::AnimationSource(_)|ResourceData::AnimationClip(_)|ResourceData::AnimationSet(_))).map(|r|r.id).collect();
    for id in lib { project.delete_resource(id); }

    let model_id = import_model_with_animation_sources(&mut project, &model_fbx, &[], "Rust Mantis", &root, cfg.clone()).unwrap();
    // Empty pack -> the model's own baked animations (NO retarget).
    // Non-empty pack -> external clips baked through the RETARGET path.
    let lib_name = if pack.is_empty() { "Native Takes" } else { "Retargeted Pack" };
    let limport = import_animation_library(&mut project, &model_fbx, &pack, lib_name, &root, cfg, None).unwrap();
    println!("clips ({lib_name}):");
    for id in &limport.clip_ids {
        if let Some(r)=project.resource(*id){ println!("  {:?} {}", id, r.name); }
    }
    // pick the clip to drive idle/walk by substring match; fall back to last.
    let idle = limport.clip_ids.iter().copied().find(|id|
        project.resource(*id).map(|r| r.name.to_lowercase().contains(&idle_substr)).unwrap_or(false))
        .or_else(|| limport.clip_ids.last().copied());
    println!("idle clip -> {idle:?} (substr '{idle_substr}', packs={})", pack.len());

    // force the set's idle/walk to that clip so the standing player shows it
    if let Some(idle)=idle { if let Some(res)=project.resource_mut(limport.set_id) {
        if let ResourceData::AnimationSet(set)=&mut res.data { set.idle_clip=Some(idle); set.walk_clip=Some(idle); }
    }}
    for id in players.iter().copied() { if let Some(res)=project.resource_mut(id) {
        if let ResourceData::Character(ch)=&mut res.data { ch.model=Some(model_id); ch.animation_set=Some(limport.set_id); } } }
    let others: Vec<_> = project.resources.iter().filter_map(|r| match &r.data {
        ResourceData::Character(_) if !players.contains(&r.id)=>Some(r.id), _=>None}).collect();
    for id in others { project.delete_resource(id); }
    for id in old { project.delete_resource(id); }
    for (si,nid) in repoint { if let Some(n)=project.scenes[si].node_mut(nid) {
        if let NodeKind::ModelRenderer{model,..}=&mut n.kind { *model=Some(model_id); } } }
    for s in &mut project.scenes { let ids:Vec<_>=s.nodes().iter().map(|n|n.id).collect();
        for id in ids { if let Some(n)=s.node_mut(id) {
            if let NodeKind::Animator{action_clips,clip,autoplay,..}=&mut n.kind { action_clips.clear(); *clip=None; *autoplay=true; } } } }
    // barricade bind pose
    let models:Vec<_>=project.resources.iter().filter_map(|r| match &r.data { ResourceData::Model(m)=>Some((r.id,m.skeleton,m.model_path.clone())),_=>None}).collect();
    for (mid,skel,mp) in models { if !project.resolved_model_animation_clips(mid).is_empty(){continue;}
        let Some(skel)=skel else {continue}; let b=root.join(&mp); let Some(d)=b.parent() else {continue};
        if let Some(px)=std::fs::read_dir(d).ok().and_then(|rd| rd.flatten().map(|e|e.path()).find(|p|p.extension().and_then(|x|x.to_str())==Some("psxanim"))) {
            let rel=px.strip_prefix(&root).unwrap_or(&px).to_string_lossy().replace('\\',"/");
            let nm=px.file_stem().and_then(|s|s.to_str()).unwrap_or("bind").to_string();
            project.add_resource(nm, ResourceData::AnimationClip(psxed_project::AnimationClipResource{
                psxanim_path:rel, skeleton:Some(skel), source:None, bake:psxed_project::AnimationClipBakeKind::LegacyShared,
                role:psxed_project::AnimationRole::Generic, looping:false, tags:Vec::new(), calibration:Default::default() })); } }
    project.save_to_path(&project_path).unwrap();
    println!("saved");
}
