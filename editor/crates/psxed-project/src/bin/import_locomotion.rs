//! Bake a directional locomotion pack onto a project's player character.
//!
//! AI motion generators (Uthana, SayMotion, Cascadeur exports, Mixamo) emit
//! one single-take file per action on whatever humanoid rig they like. This
//! binary is the repeatable path from a folder of those takes to bound
//! gameplay actions, so regenerating a clip is one command rather than a
//! sequence of import dialogs.
//!
//! ```text
//! import-locomotion archive/legacy-grid/archive/cortex/cortex_anim/project.ron "$HOME/Desktop/Bonnie Studios"
//! ```
//!
//! The pack is a fixed filename convention, matching the eight locomotion
//! slots the runtime drives (four while free-running, four more while
//! locked on and preserving facing):
//!
//! | file        | action         |
//! |-------------|----------------|
//! | `walk_fwd`  | Walk           |
//! | `walk_bwd`  | WalkBackward   |
//! | `walk_lft`  | StrafeLeft     |
//! | `walk_rgt`  | StrafeRight    |
//! | `run_fwd`   | Run            |
//! | `walk_fwd_windup`   | WalkWindup (one shot) |
//! | `walk_fwd_winddown` | WalkWinddown (one shot) |
//! | `walk_fwd_winddown_mirror` | WalkWinddownAlt (one shot, other foot) |
//! | `run_fwd_windup`    | RunWindup (one shot)   |
//! | `run_fwd_winddown`  | RunWinddown (one shot) |
//! | `run_fwd_winddown_mirror` | RunWinddownAlt (one shot, other foot) |
//!
//! Missing files are skipped, so a partial pack (walk only) is a valid run
//! and leaves the other slots on whatever they were bound to.
//!
//! Generated takes usually hold several repeats of one gait cycle, so cooked
//! clips are trimmed to a single cycle by default (`--no-trim` keeps them
//! whole). That is a RAM saving, but the loop quality matters more: the
//! runtime loops the whole clip, so a take holding two and a half cycles
//! loops back mid-stride every time it wraps.
//!
//! Everything else is derived from the project: the player character supplies
//! the target model, the model's `source_path` supplies the retarget
//! reference, and existing per-action options (in-place, speed) are preserved
//! across the rebind. Clips bake with `extra_animations_affect_bounds` off:
//! the `.psxanim` carries no bounds of its own and the runtime decodes joint
//! translations using the MODEL's, so a clip that inflates its own bounds
//! desyncs the two and distorts the pose.

use std::path::{Path, PathBuf};

use psxed_project::model_import::register_cooked_model_bundle;
use psxed_project::model_import::{
    animation_stats_from_bytes, model_stats_from_bytes, preview_model_with_animation_sources,
    resolve_path,
};
use psxed_project::{
    AnimationActionBinding, AnimationClipBakeKind, AnimationClipResource, AnimationRole,
    AnimationSetResource, CharacterAnimationAction, CharacterSpawnRole, ProjectDocument,
    ResourceData, ResourceId,
};

/// Pack filename stem, the gameplay action it binds, its clip role, and
/// whether gameplay loops it. Only looping clips are cycle-trimmed; a
/// one-shot has no cycle to find and must keep its full length.
const PACK: [(&str, CharacterAnimationAction, AnimationRole, bool); 21] = [
    (
        "walk_fwd_winddown_mirror",
        CharacterAnimationAction::WalkWinddownAlt,
        AnimationRole::Walk,
        false,
    ),
    (
        "run_fwd_winddown_mirror",
        CharacterAnimationAction::RunWinddownAlt,
        AnimationRole::Run,
        false,
    ),
    (
        "run_fwd_windup",
        CharacterAnimationAction::RunWindup,
        AnimationRole::Run,
        false,
    ),
    (
        "run_fwd_winddown",
        CharacterAnimationAction::RunWinddown,
        AnimationRole::Run,
        false,
    ),
    (
        "walk_fwd_windup",
        CharacterAnimationAction::WalkWindup,
        AnimationRole::Walk,
        false,
    ),
    (
        "walk_fwd_winddown",
        CharacterAnimationAction::WalkWinddown,
        AnimationRole::Walk,
        false,
    ),
    (
        "idle",
        CharacterAnimationAction::Idle,
        AnimationRole::Idle,
        true,
    ),
    (
        "wake_up",
        CharacterAnimationAction::Intro,
        AnimationRole::Generic,
        false,
    ),
    (
        "death",
        CharacterAnimationAction::Death,
        AnimationRole::Death,
        false,
    ),
    (
        "hit_react",
        CharacterAnimationAction::HitReact,
        AnimationRole::Hit,
        false,
    ),
    (
        "light_attack",
        CharacterAnimationAction::LightAttack,
        AnimationRole::Attack,
        false,
    ),
    (
        "heavy_attack",
        CharacterAnimationAction::HeavyAttack,
        AnimationRole::Attack,
        false,
    ),
    (
        "combo_attack",
        CharacterAnimationAction::ComboAttack,
        AnimationRole::Attack,
        false,
    ),
    // The vertical axis: overhead strikes, its own three actions. The Alt*
    // slots are already the delivered heavy-weapon set (they bind by name).
    (
        "vert_light_attack",
        CharacterAnimationAction::VertLightAttack,
        AnimationRole::Attack,
        false,
    ),
    (
        "vert_heavy_attack",
        CharacterAnimationAction::VertHeavyAttack,
        AnimationRole::Attack,
        false,
    ),
    (
        "vert_combo_attack",
        CharacterAnimationAction::VertComboAttack,
        AnimationRole::Attack,
        false,
    ),
    (
        "walk_fwd",
        CharacterAnimationAction::Walk,
        AnimationRole::Walk,
        true,
    ),
    (
        "walk_bwd",
        CharacterAnimationAction::WalkBackward,
        AnimationRole::Walk,
        true,
    ),
    (
        "walk_lft",
        CharacterAnimationAction::StrafeLeft,
        AnimationRole::Walk,
        true,
    ),
    (
        "walk_rgt",
        CharacterAnimationAction::StrafeRight,
        AnimationRole::Walk,
        true,
    ),
    (
        "run_fwd",
        CharacterAnimationAction::Run,
        AnimationRole::Run,
        true,
    ),
];

/// How far the feet may rise, in the units of the freshly baked clip (16x the
/// cooked ones, the scale to engine units happens later). Measured across the
/// shipped set, every grounded clip stays under 600: walk 80, light attack
/// 270, the vertical strikes 310 and 577. The same vertical clip baked from a
/// foreign rig reads 9206, so this sits an order of magnitude clear of both.
const FLOOR_LIFT_WARN_UNITS: i32 = 2000;

/// How far the lowest posed VERTEX rises above its frame-0 height, in model
/// units. Deliberately not the cooked frame bounds: those are a sphere, and a
/// crouch shrinks the sphere, so its underside climbs even with planted feet.
fn clip_floor_lift(model_bytes: &[u8], clip_bytes: &[u8]) -> Option<i32> {
    let model = psx_asset::Model::from_bytes(model_bytes).ok()?;
    let clip = psx_asset::Animation::from_bytes(clip_bytes).ok()?;
    let lowest = |frame: u16| -> i64 {
        let mut low = i64::MAX;
        for part_index in 0..model.part_count() {
            let Some(part) = model.part(part_index) else {
                continue;
            };
            let Some(pose) = clip.pose(frame, part.joint_index() as u16) else {
                continue;
            };
            for v in part.first_vertex()..part.first_vertex() + part.vertex_count() {
                let Some(vertex) = model.vertex(v) else {
                    continue;
                };
                let m = pose.matrix;
                let p = [
                    vertex.position.x as i64,
                    vertex.position.y as i64,
                    vertex.position.z as i64,
                ];
                let y = (m[0][1] as i64 * p[0] + m[1][1] as i64 * p[1] + m[2][1] as i64 * p[2])
                    / 4096
                    + pose.translation.y as i64;
                low = low.min(y);
            }
        }
        low
    };
    let first = lowest(0);
    let peak = (1..clip.frame_count()).map(lowest).max().unwrap_or(first);
    Some((peak - first) as i32)
}

/// Source extensions tried for each pack stem, in order.
const EXTENSIONS: [&str; 3] = ["glb", "gltf", "fbx"];

/// `AssetHeader` size: magic, version, flags, payload length.
const HEADER_BYTES: usize = 12;
/// `AnimationHeader` size: joints, frames, rate, translation shift.
const ANIMATION_HEADER_BYTES: usize = 8;

fn fail(message: impl std::fmt::Display) -> ! {
    eprintln!("[import-locomotion] {message}");
    std::process::exit(2);
}

fn main() {
    let mut positional = Vec::new();
    let mut fps = 12u16;
    let mut pack_name = "locomotion".to_string();
    let mut trim = true;
    let mut extras: Vec<String> = Vec::new();
    let mut new_model: Option<String> = None;
    let mut character_name: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--no-trim" => trim = false,
            "--fps" => {
                fps = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| fail("--fps needs an integer"));
            }
            "--pack" => {
                pack_name = args.next().unwrap_or_else(|| fail("--pack needs a name"));
            }
            "--new-model" => {
                new_model = Some(
                    args.next()
                        .unwrap_or_else(|| fail("--new-model needs a name")),
                );
            }
            "--character" => {
                character_name = Some(
                    args.next()
                        .unwrap_or_else(|| fail("--character needs a resource name")),
                );
            }
            "--extra" => {
                extras.push(args.next().unwrap_or_else(|| fail("--extra needs a stem")));
            }
            other => positional.push(other.to_string()),
        }
    }
    let [project_arg, clips_arg] = positional.as_slice() else {
        eprintln!(
            "usage: import-locomotion <project.ron> <clips-dir> [--fps N] [--pack NAME] [--no-trim] [--extra STEM] [--new-model NAME]\n\
             \n\
             Bakes walk_/run_ fwd|bwd|lft|rgt takes onto the project's player\n\
             character and binds them to the eight locomotion actions."
        );
        std::process::exit(2);
    };

    let project_path = PathBuf::from(project_arg);
    let clips_dir = PathBuf::from(clips_arg);
    let project_root = match project_path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => PathBuf::from("."),
    };

    let text = std::fs::read_to_string(&project_path)
        .unwrap_or_else(|e| fail(format!("{}: {e}", project_path.display())));
    let mut project = ProjectDocument::from_ron_str(&text)
        .unwrap_or_else(|e| fail(format!("{}: parse failed: {e}", project_path.display())));

    let (character_name, model_id, set_id) = find_target(&project, character_name.as_deref());
    let (model_name, model_source, model_skeleton, world_height, cooked_model_path) =
        model_details(&project, model_id, &project_root);

    let model_joints = std::fs::read(&cooked_model_path)
        .ok()
        .and_then(|bytes| model_stats_from_bytes(&bytes).ok())
        .map(|stats| stats.joint_count)
        .unwrap_or_else(|| {
            fail(format!(
                "unreadable cooked model {}",
                cooked_model_path.display()
            ))
        });

    println!(
        "player {character_name} -> model {model_name} ({model_joints} joints)\n\
         reference {}\n\
         baking at {fps} Hz into assets/animations/{pack_name}/",
        model_source.display()
    );

    // --new-model cooks the takes' OWN model and binds its native clips,
    // instead of retargeting them onto whatever the player already uses.
    if let Some(model_name) = new_model {
        build_native_model(
            &mut project,
            &clips_dir,
            &project_root,
            &model_name,
            &pack_name,
            fps,
            trim,
            world_height,
        );
        project
            .save_to_path(&project_path)
            .unwrap_or_else(|e| fail(format!("{}: save failed: {e}", project_path.display())));
        return;
    }

    let config = psxed_gltf::RigidModelConfig {
        animation_fps: fps,
        world_height,
        // The clip must quantize against the cooked model's bounds, never its
        // own. See the module docs.
        extra_animations_affect_bounds: false,
        ..Default::default()
    };

    let out_dir = project_root
        .join("assets")
        .join("animations")
        .join(&pack_name);
    std::fs::create_dir_all(&out_dir)
        .unwrap_or_else(|e| fail(format!("{}: {e}", out_dir.display())));

    // Pack entries bind a gameplay action; extras are cooked and registered
    // but left unbound, for clips the runtime has no action slot for yet.
    let jobs: Vec<(
        String,
        Option<CharacterAnimationAction>,
        AnimationRole,
        bool,
    )> = PACK
        .iter()
        .map(|(stem, action, role, looping)| ((*stem).to_string(), Some(*action), *role, *looping))
        .chain(
            extras
                .iter()
                .map(|stem| (stem.clone(), None, AnimationRole::Generic, false)),
        )
        .collect();

    let mut bound = 0usize;
    let mut skipped = Vec::new();
    for (stem, action, role, looping) in &jobs {
        let (stem, action, role, looping) = (stem.as_str(), *action, *role, *looping);
        let Some(source) = find_source(&clips_dir, stem) else {
            skipped.push(stem.to_string());
            continue;
        };

        // Cook one take at a time: these files carry a single unnamed take, so
        // the cooked clip is identified by position, not by name.
        let package = preview_model_with_animation_sources(
            &model_source,
            std::slice::from_ref(&source),
            config.clone(),
        )
        .unwrap_or_else(|e| fail(format!("{}: bake failed: {e:?}", source.display())));
        let clip = package
            .clips
            .last()
            .unwrap_or_else(|| fail(format!("{}: produced no clips", source.display())));

        let mut bytes = clip.bytes.clone();
        let mut trim_note = String::new();
        if trim && looping {
            if let Some((trimmed, frames, seam)) = trim_to_single_cycle(&bytes) {
                trim_note = format!(
                    "  trimmed {} -> {frames} frames, seam {seam:.2} frames",
                    clip.frames
                );
                bytes = trimmed;
            }
        }

        let stats = animation_stats_from_bytes(stem, &bytes, model_joints)
            .unwrap_or_else(|e| fail(format!("{stem}: {e:?}")));
        if !stats.valid_for_model {
            fail(format!(
                "{stem}: cooked {} joints, model has {model_joints}; the cook would reject this bundle",
                stats.joint_count
            ));
        }
        // See `clip_floor_lift`: feet climbing off the floor is how a clip
        // baked from a foreign rig shows up.
        if let Some(lift) = clip_floor_lift(&package.model, &bytes) {
            if lift > FLOOR_LIFT_WARN_UNITS {
                println!(
                    "  WARNING {stem}: feet rise {lift} model units above the clip's own floor. \
                     If this is not a jump, the source is rigged on another skeleton -- retarget it first."
                );
            }
        }

        let psxanim_path = out_dir.join(format!("{stem}.psxanim"));
        std::fs::write(&psxanim_path, &bytes)
            .unwrap_or_else(|e| fail(format!("{}: {e}", psxanim_path.display())));

        let resource = AnimationClipResource {
            psxanim_path: relative_to(&psxanim_path, &project_root),
            skeleton: model_skeleton,
            target_model: Some(model_id),
            source: None,
            bake: AnimationClipBakeKind::Retargeted,
            role,
            looping,
            tags: vec![pack_name.clone()],
            calibration: Default::default(),
            pose_corrections: Vec::new(),
        };
        let clip_id = upsert_clip(&mut project, format!("{pack_name}_{stem}"), resource);
        let target = match action {
            Some(action) => {
                bind_action(&mut project, set_id, action, clip_id);
                bound += 1;
                format!("{action:?}")
            }
            None => "(unbound)".to_string(),
        };

        println!(
            "  {stem:9} -> {target:14} {} frames @ {} Hz  {:.1} KB{trim_note}",
            stats.frame_count,
            stats.sample_rate_hz,
            stats.bytes as f32 / 1024.0
        );
    }

    if bound == 0 {
        fail(format!("no pack files found in {}", clips_dir.display()));
    }
    project
        .save_to_path(&project_path)
        .unwrap_or_else(|e| fail(format!("{}: save failed: {e}", project_path.display())));

    println!("bound {bound}/{} actions", PACK.len());
    if !skipped.is_empty() {
        println!(
            "missing (left on their existing clips): {}",
            skipped.join(", ")
        );
    }
}

/// The character this run binds onto: its name, model, and animation set.
///
/// Named explicitly for an enemy, since an enemy's own combat clips resolve
/// off ITS animation set, not the player's. Without a name this keeps the old
/// behaviour and finds the player.
fn find_target(project: &ProjectDocument, name: Option<&str>) -> (String, ResourceId, ResourceId) {
    let Some(name) = name else {
        return find_player(project);
    };
    for resource in &project.resources {
        let ResourceData::Character(character) = &resource.data else {
            continue;
        };
        if !resource.name.eq_ignore_ascii_case(name) {
            continue;
        }
        let Some(model) = character.model else {
            fail(format!("Character '{name}' has no model"));
        };
        let Some(set) = character.animation_set else {
            fail(format!("Character '{name}' has no animation set"));
        };
        return (resource.name.clone(), model, set);
    }
    fail(format!("no Character resource named '{name}'"))
}

/// Player character in the project: its name, model, and animation set.
fn find_player(project: &ProjectDocument) -> (String, ResourceId, ResourceId) {
    let mut fallback = None;
    for resource in &project.resources {
        let ResourceData::Character(character) = &resource.data else {
            continue;
        };
        let Some(model) = character.model else {
            continue;
        };
        let Some(set) = character.animation_set else {
            continue;
        };
        match character.spawn_role {
            CharacterSpawnRole::Player => return (resource.name.clone(), model, set),
            CharacterSpawnRole::Auto if fallback.is_none() => {
                fallback = Some((resource.name.clone(), model, set));
            }
            _ => {}
        }
    }
    fallback.unwrap_or_else(|| fail("no player character with both a model and an animation set"))
}

/// Model name, original source, skeleton, world height, cooked `.psxmdl`.
fn model_details(
    project: &ProjectDocument,
    model_id: ResourceId,
    project_root: &Path,
) -> (String, PathBuf, Option<ResourceId>, u16, PathBuf) {
    let Some(resource) = project.resource(model_id) else {
        fail(format!("model {model_id:?} is missing"));
    };
    let ResourceData::Model(model) = &resource.data else {
        fail(format!("resource {} is not a model", resource.name));
    };
    let Some(source) = model.source_path.as_ref() else {
        fail(format!(
            "model {} has no source_path; retargeting needs the original rigged file",
            resource.name
        ));
    };
    let source = resolve_path(source, Some(project_root));
    if !source.is_file() {
        fail(format!("model source {} not found", source.display()));
    }
    (
        resource.name.clone(),
        source,
        model.skeleton,
        model.world_height,
        resolve_path(&model.model_path, Some(project_root)),
    )
}

/// Cook a model and its clips from the generated takes in ONE pass, then point
/// the player at them.
///
/// The generator hands back a complete rigged model alongside every clip, so
/// the retarget the other path performs is avoidable work: cooking that model
/// natively means the clips already live on the skeleton they were authored
/// for. Model and clips come out of a single `convert` call, so they share one
/// set of quantisation bounds by construction rather than by agreement.
fn build_native_model(
    project: &mut ProjectDocument,
    clips_dir: &Path,
    project_root: &Path,
    model_name: &str,
    pack_name: &str,
    fps: u16,
    trim: bool,
    world_height: u16,
) {
    // The model source is one of the takes; the rest ride along as extras. The
    // cooked clip order is [model source's own take, extras in order], which is
    // how their identities are recovered -- the takes are unnamed.
    let mut jobs: Vec<(&str, Option<CharacterAnimationAction>, bool, PathBuf)> = Vec::new();
    for (stem, action, _role, looping) in PACK {
        if let Some(path) = find_source(clips_dir, stem) {
            jobs.push((stem, Some(action), looping, path));
        }
    }
    if jobs.is_empty() {
        fail(format!("no pack files found in {}", clips_dir.display()));
    }
    let model_source = jobs[0].3.clone();
    let extras: Vec<PathBuf> = jobs[1..].iter().map(|job| job.3.clone()).collect();

    let config = psxed_gltf::RigidModelConfig {
        animation_fps: fps,
        world_height,
        // Full bundle import: every clip must be inside the model's bounds, so
        // the clips DO get a vote on them here (the opposite of an add-on bake).
        extra_animations_affect_bounds: true,
        ..Default::default()
    };
    let package = preview_model_with_animation_sources(&model_source, &extras, config.clone())
        .unwrap_or_else(|e| fail(format!("{}: cook failed: {e:?}", model_source.display())));
    if package.clips.len() != jobs.len() {
        fail(format!(
            "cooked {} clips for {} takes; clip identity is positional, refusing to guess",
            package.clips.len(),
            jobs.len()
        ));
    }

    // Bundle directories are lowercase-with-underscores across this project,
    // so fold anything else (spaces, hyphens) rather than carrying it into a
    // path: "Aletha-uthana" -> "aletha_uthana".
    let safe: String = model_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let safe = safe.trim_matches('_').to_string();
    let bundle_dir = project_root.join("assets").join("models").join(&safe);
    std::fs::create_dir_all(&bundle_dir)
        .unwrap_or_else(|e| fail(format!("{}: {e}", bundle_dir.display())));
    for entry in std::fs::read_dir(&bundle_dir)
        .into_iter()
        .flatten()
        .flatten()
    {
        let path = entry.path();
        if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("psxmdl") | Some("psxt") | Some("psxanim")
        ) {
            let _ = std::fs::remove_file(path);
        }
    }
    let model_path = bundle_dir.join(format!("{safe}.psxmdl"));
    std::fs::write(&model_path, &package.model)
        .unwrap_or_else(|e| fail(format!("{}: {e}", model_path.display())));
    if let Some(texture) = &package.texture {
        let texture_path = bundle_dir.join(format!("{safe}.psxt"));
        std::fs::write(&texture_path, texture)
            .unwrap_or_else(|e| fail(format!("{}: {e}", texture_path.display())));
    }

    // Re-importing the same pack must land on the SAME model resource. A fresh
    // registration every run would orphan the previous model and, because clip
    // identity is keyed on `target_model`, orphan every clip with it.
    let existing_model = project
        .resources
        .iter()
        .find(|r| r.name == model_name && matches!(r.data, ResourceData::Model(_)))
        .map(|r| r.id);
    let fresh_id =
        register_cooked_model_bundle(project, &bundle_dir, model_name, Some(project_root))
            .unwrap_or_else(|e| fail(format!("register {model_name}: {e:?}")));
    let model_id = match existing_model {
        Some(existing) if existing != fresh_id => {
            let fresh = project
                .resource(fresh_id)
                .map(|resource| resource.data.clone())
                .unwrap_or_else(|| fail("freshly registered model vanished"));
            if let Some(slot) = project.resource_mut(existing) {
                slot.data = fresh;
            }
            project.resources.retain(|resource| resource.id != fresh_id);
            existing
        }
        _ => fresh_id,
    };
    let mut skeleton_id = None;
    if let Some(resource) = project.resource_mut(model_id) {
        if let ResourceData::Model(model) = &mut resource.data {
            model.source_path = Some(relative_to(&model_source, project_root));
            model.world_height = world_height;
            skeleton_id = model.skeleton;
        }
    }
    let model_joints = model_stats_from_bytes(&package.model)
        .map(|stats| stats.joint_count)
        .unwrap_or(0);
    println!(
        "model {model_name} <- {} ({model_joints} joints, {} tris) + {} takes",
        model_source
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        package.report.faces,
        jobs.len()
    );

    let out_dir = project_root
        .join("assets")
        .join("animations")
        .join(pack_name);
    std::fs::create_dir_all(&out_dir)
        .unwrap_or_else(|e| fail(format!("{}: {e}", out_dir.display())));

    // Cook and trim everything first: the foot-joint vote needs every gait clip
    // before any of them can be aligned.
    let mut staged: Vec<(Vec<u8>, String)> = Vec::new();
    for (index, (_stem, _action, looping, _path)) in jobs.iter().enumerate() {
        let clip = &package.clips[index];
        let mut bytes = clip.bytes.clone();
        let mut note = String::new();
        if trim && *looping {
            if let Some((trimmed, frames, seam)) = trim_to_single_cycle(&bytes) {
                note = format!("  trimmed {} -> {frames}, seam {seam:.2}", clip.frames);
                bytes = trimmed;
            }
        }
        staged.push((bytes, note));
    }
    // Ground-contact one-shots (the spawn intro) are levelled so the cook's
    // frame-0 anchor holds for every frame, not just the first.
    let mut ground_offsets: std::collections::BTreeMap<usize, i32> =
        std::collections::BTreeMap::new();
    // The clip an intro hands control to; its floor is the height to match.
    let idle_floor = jobs
        .iter()
        .position(|(_, action, _, _)| *action == Some(CharacterAnimationAction::Idle))
        .and_then(|index| {
            let model = psx_asset::Model::from_bytes(&package.model).ok()?;
            let anim = psx_asset::Animation::from_bytes(&staged[index].0).ok()?;
            psxed_project::playtest::bake_model_clip_frame_bounds(
                &model,
                &anim,
                psxed_project::playtest::MODEL_FRAME_BOUNDS_PAD_UNITS,
            )
            .first()
            .map(|b| b.floor_y)
        });
    for (index, (stem, action, _looping, _path)) in jobs.iter().enumerate() {
        if *action != Some(CharacterAnimationAction::Intro) {
            continue;
        }
        let Some(target) = idle_floor else { continue };
        // Cancel the clip anchor for this clip.
        //
        // The runtime shifts a clip vertically by `reference.floor - clip.floor`
        // to reconcile clips authored on different ground planes. That compares
        // frame-0 contact points, which is only meaningful when both clips have
        // comparable contact geometry. A prone intro's frame-0 contact is the
        // TORSO; idle's is the FEET. Differencing them produced a bogus 38198
        // unit lift and left the character hanging in the air -- measured at
        // 90px, and confirmed by disabling the term (float drops to +/-10px).
        //
        // Every other clip is authored standing, so their difference is under
        // ~1500 units and the term is harmless; this is the first clip where it
        // is not. `pose_offset` is added AFTER the anchor, so writing the
        // negated difference here nets the term to zero without an engine
        // change and without touching any other clip.
        let anchor = psx_asset::Model::from_bytes(&package.model)
            .ok()
            .and_then(|model| {
                let anim = psx_asset::Animation::from_bytes(&staged[index].0).ok()?;
                psxed_project::playtest::bake_model_clip_frame_bounds(
                    &model,
                    &anim,
                    psxed_project::playtest::MODEL_FRAME_BOUNDS_PAD_UNITS,
                )
                .first()
                .map(|f| f.floor_y)
            });
        if let Some(anchor) = anchor {
            let cancel = anchor - target;
            ground_offsets.insert(index, cancel);
            staged[index]
                .1
                .push_str(&format!("  anchor cancelled ({cancel})"));
            println!("  {stem}: cancelling spurious clip anchor, pose_offset y={cancel}");
        }
    }

    let gait: Vec<&[u8]> = jobs
        .iter()
        .enumerate()
        .filter(|(_, (_, action, looping, _))| *looping && action.is_some_and(is_gait))
        .map(|(index, _)| staged[index].0.as_slice())
        .collect();
    if let Some(foot) = foot_joint_vote(&gait) {
        println!("  gait alignment: foot joint {foot}");
        for (index, (_stem, action, looping, _path)) in jobs.iter().enumerate() {
            if !looping || !action.is_some_and(is_gait) {
                continue;
            }
            if let Some((aligned, shift)) = align_cycle_to_foot_down(&staged[index].0, foot) {
                staged[index].0 = aligned;
                staged[index]
                    .1
                    .push_str(&format!("  rotated {shift} to foot-down"));
            }
        }
    }

    let mut bindings: Vec<AnimationActionBinding> = Vec::new();
    let mut clip_ids: Vec<ResourceId> = Vec::new();
    for (index, (stem, action, looping, _path)) in jobs.iter().enumerate() {
        let (bytes, note) = (staged[index].0.clone(), staged[index].1.clone());
        let stats = animation_stats_from_bytes(*stem, &bytes, model_joints)
            .unwrap_or_else(|e| fail(format!("{stem}: {e:?}")));
        if !stats.valid_for_model {
            fail(format!(
                "{stem}: {} joints against a {model_joints}-joint model",
                stats.joint_count
            ));
        }
        // A clip baked from a source rigged on a DIFFERENT skeleton keeps its
        // rotations but loses the root's motion, and the tell is the feet: the
        // hips stay at rest height, so a crouch lifts the feet off the floor
        // instead of lowering the body. Retarget the source onto this model
        // first (tools/retarget_clip.py). A real jump lifts off too, hence a
        // warning and not an error.
        if let Some(lift) = clip_floor_lift(&package.model, &bytes) {
            if lift > FLOOR_LIFT_WARN_UNITS {
                println!(
                    "  WARNING {stem}: feet rise {lift} model units above the clip's own floor.                      If this is not a jump, the source is rigged on another skeleton -- retarget it first."
                );
            }
        }
        let psxanim_path = out_dir.join(format!("{stem}.psxanim"));
        std::fs::write(&psxanim_path, &bytes)
            .unwrap_or_else(|e| fail(format!("{}: {e}", psxanim_path.display())));

        let clip_id = upsert_clip(
            project,
            format!("{pack_name}_{stem}"),
            AnimationClipResource {
                psxanim_path: relative_to(&psxanim_path, project_root),
                skeleton: skeleton_id,
                target_model: Some(model_id),
                source: None,
                // Native, not retargeted: these clips were authored on this rig.
                bake: AnimationClipBakeKind::ModelNative,
                role: AnimationRole::Generic,
                looping: *looping,
                tags: vec![pack_name.to_string()],
                calibration: psxed_project::AnimationClipCalibration {
                    in_place: true,
                    offset: [0, ground_offsets.get(&index).copied().unwrap_or(0), 0],
                },
                pose_corrections: Vec::new(),
            },
        );
        clip_ids.push(clip_id);
        if let Some(action) = action {
            bindings.push(AnimationActionBinding {
                action: *action,
                clip: clip_id,
                options: None,
            });
        }
        println!(
            "  {stem:12} -> {:14} {} frames @ {} Hz  {:.1} KB{note}",
            action.map(|a| format!("{a:?}")).unwrap_or_default(),
            stats.frame_count,
            stats.sample_rate_hz,
            stats.bytes as f32 / 1024.0
        );
    }

    let set_name = format!("{pack_name}_set");
    let set = AnimationSetResource {
        skeleton: skeleton_id,
        action_clips: bindings,
        clips: clip_ids,
        ..Default::default()
    };
    let set_id = match project
        .resources
        .iter()
        .find(|r| r.name == set_name && matches!(r.data, ResourceData::AnimationSet(_)))
    {
        Some(existing) => {
            let id = existing.id;
            if let Some(slot) = project.resource_mut(id) {
                slot.data = ResourceData::AnimationSet(set);
            }
            id
        }
        None => project.add_resource(set_name.clone(), ResourceData::AnimationSet(set)),
    };

    // Point the player at the new model and set. Swapping two fields keeps the
    // character's authored tuning and makes the change reversible; the old
    // model and set stay in the project, just unreferenced.
    let (player_name, ..) = find_player(project);
    let player_id = project
        .resources
        .iter()
        .find(|r| r.name == player_name && matches!(r.data, ResourceData::Character(_)))
        .map(|r| r.id)
        .unwrap_or_else(|| fail("player character vanished"));
    let previous_model = project
        .resource(player_id)
        .and_then(|resource| match &resource.data {
            ResourceData::Character(character) => character.model,
            _ => None,
        });
    if let Some(resource) = project.resource_mut(player_id) {
        if let ResourceData::Character(character) = &mut resource.data {
            character.model = Some(model_id);
            character.animation_set = Some(set_id);
        }
    }

    // The Character resource is not the only thing pointing at the old model.
    // Scene nodes carry their own references, and the cook validates through
    // THEM: a ModelRenderer still on the old model fails the character's
    // skeleton check, and an Animator holds model-LOCAL clip indices that
    // index the old clip table. Leaving either behind makes the project
    // refuse to cook, so the swap has to reach them too.
    // Collect first, then apply: `Scene` exposes its nodes immutably and only
    // hands out one mutable node at a time.
    let mut renderer_targets: Vec<(usize, psxed_project::NodeId)> = Vec::new();
    let mut animator_targets: Vec<(usize, psxed_project::NodeId)> = Vec::new();
    for (scene_index, scene) in project.scenes.iter().enumerate() {
        for node in scene.nodes() {
            match &node.kind {
                psxed_project::NodeKind::ModelRenderer { model, .. }
                    if *model == previous_model =>
                {
                    renderer_targets.push((scene_index, node.id));
                }
                psxed_project::NodeKind::Animator {
                    clip, action_clips, ..
                } if clip.is_some() || !action_clips.is_empty() => {
                    animator_targets.push((scene_index, node.id));
                }
                _ => {}
            }
        }
    }
    let (renderers, animators) = (renderer_targets.len(), animator_targets.len());
    for (scene_index, node_id) in renderer_targets {
        if let Some(node) = project.scenes[scene_index].node_mut(node_id) {
            if let psxed_project::NodeKind::ModelRenderer { model, .. } = &mut node.kind {
                *model = Some(model_id);
            }
        }
    }
    for (scene_index, node_id) in animator_targets {
        if let Some(node) = project.scenes[scene_index].node_mut(node_id) {
            if let psxed_project::NodeKind::Animator {
                clip, action_clips, ..
            } = &mut node.kind
            {
                *clip = None;
                action_clips.clear();
            }
        }
    }
    println!(
        "player {player_name} -> model {model_name}, set {set_name} \
         ({renderers} scene renderer(s) repointed, {animators} stale animator binding(s) cleared)"
    );
}
/// Whether an action is a stepping gait, as opposed to merely looping.
///
/// Idle loops but has no stride, so foot-down alignment is meaningless for it
/// and its near-static feet only add noise to the foot-joint vote.
const fn is_gait(action: CharacterAnimationAction) -> bool {
    matches!(
        action,
        CharacterAnimationAction::Walk
            | CharacterAnimationAction::WalkBackward
            | CharacterAnimationAction::StrafeLeft
            | CharacterAnimationAction::StrafeRight
            | CharacterAnimationAction::Run
    )
}

/// Joint whose lowest point sits furthest to -X across a clip: the left foot.
///
/// Cooked clips carry no joint names, so the foot is found by shape. Every
/// gait clip votes and the majority wins, because a single clip can pick a
/// hand on a pose where an arm swings below the ankle.
fn foot_joint_vote(clips: &[&[u8]]) -> Option<u16> {
    let mut votes: Vec<(u16, usize)> = Vec::new();
    for bytes in clips {
        let Ok(anim) = psx_asset::Animation::from_bytes(bytes) else {
            continue;
        };
        let frames = anim.frame_count();
        let mut lowest: Vec<(i32, i32, u16)> = Vec::new();
        for joint in 0..anim.joint_count() {
            let mut min_y = i32::MAX;
            let mut x_at_min = 0;
            for frame in 0..frames {
                if let Some(pose) = anim.pose(frame, joint) {
                    if pose.translation.y < min_y {
                        min_y = pose.translation.y;
                        x_at_min = pose.translation.x;
                    }
                }
            }
            lowest.push((min_y, x_at_min, joint));
        }
        lowest.sort_by_key(|entry| entry.0);
        // The two lowest joints are the feet; the one on -X is the left.
        if let Some(pick) = lowest.iter().take(2).min_by_key(|entry| entry.1) {
            match votes.iter_mut().find(|(joint, _)| *joint == pick.2) {
                Some((_, count)) => *count += 1,
                None => votes.push((pick.2, 1)),
            }
        }
    }
    votes
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(joint, _)| joint)
}

/// Rotate a single-cycle looping clip so frame 0 is `foot`'s lowest point.
///
/// Gait clips come out of the generator starting at arbitrary points in the
/// stride (measured: anywhere from phase 0.00 to 0.94). Carrying phase across
/// a transition is only meaningful once phase 0 means the same thing in every
/// clip, and rotating a loop is lossless, so the alignment belongs here rather
/// than as a per-clip offset the runtime has to carry.
fn align_cycle_to_foot_down(bytes: &[u8], foot: u16) -> Option<(Vec<u8>, u16)> {
    let anim = psx_asset::Animation::from_bytes(bytes).ok()?;
    let frames = anim.frame_count();
    let joints = anim.joint_count();
    let record = anim.pose_record_size();
    // The last stored frame duplicates frame 0, so the cycle is frames - 1.
    let cycle = frames.checked_sub(1).filter(|c| *c >= 2)?;
    let mut best = (i32::MAX, 0u16);
    for frame in 0..cycle {
        if let Some(pose) = anim.pose(frame, foot) {
            if pose.translation.y < best.0 {
                best = (pose.translation.y, frame);
            }
        }
    }
    let shift = best.1;
    if shift == 0 {
        return None;
    }
    let stride = usize::from(joints) * record;
    let table = HEADER_BYTES + ANIMATION_HEADER_BYTES;
    let mut out = bytes.to_vec();
    for frame in 0..cycle {
        let src = usize::from((frame + shift) % cycle);
        let dst = usize::from(frame);
        let from = table + src * stride;
        let to = table + dst * stride;
        out[to..to + stride].copy_from_slice(&bytes[from..from + stride]);
    }
    // Restore the endpoint duplicate the looping sampler expects.
    let first = table;
    let last = table + usize::from(cycle) * stride;
    let head = out[first..first + stride].to_vec();
    out[last..last + stride].copy_from_slice(&head);
    Some((out, shift))
}

/// Total absolute difference between two frames of a clip, summed over every
/// joint's pose matrix and translation. Decoded through `psx_asset` so it is
/// independent of the on-disk record encoding.
fn frame_distance(anim: &psx_asset::Animation<'_>, a: u16, b: u16) -> i64 {
    let mut total = 0i64;
    for joint in 0..anim.joint_count() {
        let (Some(pa), Some(pb)) = (anim.pose(a, joint), anim.pose(b, joint)) else {
            continue;
        };
        for col in 0..3 {
            for row in 0..3 {
                total += i64::from(pa.matrix[col][row] - pb.matrix[col][row]).abs();
            }
        }
        total += i64::from(pa.translation.x - pb.translation.x).abs();
        total += i64::from(pa.translation.y - pb.translation.y).abs();
        total += i64::from(pa.translation.z - pb.translation.z).abs();
    }
    total
}

/// Mean frame distance at a given lag.
fn mean_distance_at_lag(anim: &psx_asset::Animation<'_>, lag: u16) -> i64 {
    let pairs = anim.frame_count().saturating_sub(lag);
    if pairs == 0 {
        return i64::MAX;
    }
    let total: i64 = (0..pairs).map(|i| frame_distance(anim, i, i + lag)).sum();
    total / i64::from(pairs)
}

/// Shortest repeating period in frames, when the clip clearly holds more than
/// one cycle.
///
/// A clip repeats with period `p` when frames `p` apart are about as similar
/// as neighbouring frames are. Two traps: every MULTIPLE of the true period
/// also scores well, so this takes the first candidate rather than the global
/// minimum; and on a smooth cycle the lags either side of the true period
/// score nearly as well, so it descends to the local minimum instead of
/// stopping at the first lag under the threshold.
fn detect_cycle(anim: &psx_asset::Animation<'_>) -> Option<u16> {
    let frames = anim.frame_count();
    if frames < 8 {
        return None;
    }
    let step = mean_distance_at_lag(anim, 1);
    if step == 0 {
        return None;
    }
    let threshold = step * 3 / 2;
    // A quarter of a cycle is the shortest plausible period for a gait; below
    // that the detector starts matching pose symmetry (left step against right
    // step) rather than a genuine repeat.
    let max_lag = frames / 2;
    let mut lag = (4..=max_lag).find(|&lag| mean_distance_at_lag(anim, lag) <= threshold)?;
    while lag < max_lag && mean_distance_at_lag(anim, lag + 1) < mean_distance_at_lag(anim, lag) {
        lag += 1;
    }
    Some(lag)
}

/// Cut a cooked clip down to one cycle, keeping the endpoint frame the runtime
/// expects (it loops `frame_count - 2` back to frame 0, treating the last
/// stored frame as the duplicate of the first).
///
/// Returns the trimmed bytes plus the seam cost: the frame distance across the
/// new loop point, in units of one frame of motion. Below roughly 1.0 the wrap
/// is no larger a step than any other frame boundary.
fn trim_to_single_cycle(bytes: &[u8]) -> Option<(Vec<u8>, u16, f32)> {
    let anim = psx_asset::Animation::from_bytes(bytes).ok()?;
    let period = detect_cycle(&anim)?;
    let keep = period + 1;
    if keep >= anim.frame_count() {
        return None;
    }
    let step = mean_distance_at_lag(&anim, 1).max(1);
    let seam = frame_distance(&anim, 0, period) as f32 / step as f32;

    let record = anim.pose_record_size();
    let pose_table = usize::from(keep) * usize::from(anim.joint_count()) * record;
    let mut out = bytes[..HEADER_BYTES + ANIMATION_HEADER_BYTES + pose_table].to_vec();
    let payload = (ANIMATION_HEADER_BYTES + pose_table) as u32;
    out[8..12].copy_from_slice(&payload.to_le_bytes());
    out[14..16].copy_from_slice(&keep.to_le_bytes());
    Some((out, keep, seam))
}

/// Replace the clip resource of this name if the pack already imported one,
/// otherwise add it. Rebaking a pack is the common case (a regenerated take,
/// a different sample rate), and it must not leave the previous resource
/// behind orphaned.
fn upsert_clip(
    project: &mut ProjectDocument,
    name: String,
    resource: AnimationClipResource,
) -> ResourceId {
    let existing = project.resources.iter().find_map(|candidate| {
        let ResourceData::AnimationClip(clip) = &candidate.data else {
            return None;
        };
        (candidate.name == name && clip.target_model == resource.target_model)
            .then_some(candidate.id)
    });
    match existing {
        Some(id) => {
            if let Some(slot) = project.resource_mut(id) {
                slot.data = ResourceData::AnimationClip(resource);
            }
            id
        }
        None => project.add_resource(name, ResourceData::AnimationClip(resource)),
    }
}

/// Point `action` at `clip`, keeping any options already authored for it.
fn bind_action(
    project: &mut ProjectDocument,
    set_id: ResourceId,
    action: CharacterAnimationAction,
    clip: ResourceId,
) {
    let Some(resource) = project.resource_mut(set_id) else {
        fail(format!("animation set {set_id:?} is missing"));
    };
    let ResourceData::AnimationSet(set) = &mut resource.data else {
        fail(format!(
            "resource {} is not an animation set",
            resource.name
        ));
    };
    match set.action_clips.iter_mut().find(|b| b.action == action) {
        Some(binding) => binding.clip = clip,
        None => set
            .action_clips
            .push(psxed_project::AnimationActionBinding {
                action,
                clip,
                options: None,
            }),
    }
    // The set's clip LIST is what carries membership, and the starter
    // catalogue copies a set by walking it. A clip bound to an action but
    // absent from the list synced into a fresh project as a dangling
    // reference, because its id was never in the remap.
    if !set.clips.contains(&clip) {
        set.clips.push(clip);
    }
}

fn find_source(dir: &Path, stem: &str) -> Option<PathBuf> {
    EXTENSIONS
        .iter()
        .map(|ext| dir.join(format!("{stem}.{ext}")))
        .find(|path| path.is_file())
}

fn relative_to(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use psxed_format::animation::{
        encode_rotation_q11, MAGIC, POSE_RECORD_SIZE_V3, POSE_ROTATION_BLOCK_SIZE_V3, VERSION_V3,
    };

    /// One-joint v3 clip whose translation repeats every `period` frames.
    fn synthetic_clip(frames: u16, period: u16) -> Vec<u8> {
        let identity: [i16; 9] = [4096, 0, 0, 0, 4096, 0, 0, 0, 4096];
        let mut block = [0u8; POSE_ROTATION_BLOCK_SIZE_V3];
        encode_rotation_q11(&identity, &mut block);

        let pose_table = usize::from(frames) * POSE_RECORD_SIZE_V3;
        let mut out = Vec::with_capacity(HEADER_BYTES + pose_table);
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&VERSION_V3.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&((ANIMATION_HEADER_BYTES + pose_table) as u32).to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // joints
        out.extend_from_slice(&frames.to_le_bytes());
        out.extend_from_slice(&15u16.to_le_bytes()); // rate
        out.extend_from_slice(&0u16.to_le_bytes()); // translation shift
        for frame in 0..frames {
            let phase = f64::from(frame % period) / f64::from(period);
            let x = (2000.0 * (phase * std::f64::consts::TAU).sin()) as i16;
            out.extend_from_slice(&block);
            out.extend_from_slice(&x.to_le_bytes());
            out.extend_from_slice(&0i16.to_le_bytes());
            out.extend_from_slice(&0i16.to_le_bytes());
        }
        out
    }

    #[test]
    fn detects_the_shortest_repeating_period_not_a_multiple() {
        let bytes = synthetic_clip(40, 8);
        let anim = psx_asset::Animation::from_bytes(&bytes).unwrap();
        assert_eq!(detect_cycle(&anim), Some(8));
    }

    /// A clip that never repeats must be left alone rather than cut at an
    /// arbitrary lag.
    #[test]
    fn leaves_a_single_cycle_untrimmed() {
        let bytes = synthetic_clip(40, 40);
        let anim = psx_asset::Animation::from_bytes(&bytes).unwrap();
        assert_eq!(detect_cycle(&anim), None);
        assert!(trim_to_single_cycle(&bytes).is_none());
    }

    /// One-joint v3 clip whose Y dips to its minimum at `low_frame`.
    fn synthetic_gait(frames: u16, low_frame: u16) -> Vec<u8> {
        let identity: [i16; 9] = [4096, 0, 0, 0, 4096, 0, 0, 0, 4096];
        let mut block = [0u8; POSE_ROTATION_BLOCK_SIZE_V3];
        encode_rotation_q11(&identity, &mut block);
        let cycle = frames - 1;
        let pose_table = usize::from(frames) * POSE_RECORD_SIZE_V3;
        let mut out = Vec::new();
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&VERSION_V3.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&((ANIMATION_HEADER_BYTES + pose_table) as u32).to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&frames.to_le_bytes());
        out.extend_from_slice(&15u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        for frame in 0..frames {
            // Cosine trough at low_frame, and the last frame duplicates the first.
            let phase =
                f64::from((frame % cycle + cycle - low_frame % cycle) % cycle) / f64::from(cycle);
            let y = (-1000.0 * (phase * std::f64::consts::TAU).cos()) as i16;
            out.extend_from_slice(&block);
            out.extend_from_slice(&0i16.to_le_bytes());
            out.extend_from_slice(&y.to_le_bytes());
            out.extend_from_slice(&0i16.to_le_bytes());
        }
        out
    }

    /// A gait clip is rotated so its lowest foot frame becomes frame 0, and
    /// the endpoint duplicate the looping sampler relies on is rebuilt.
    #[test]
    fn alignment_puts_foot_down_at_frame_zero() {
        let bytes = synthetic_gait(17, 5);
        let anim = psx_asset::Animation::from_bytes(&bytes).unwrap();
        let low = (0..16)
            .min_by_key(|f| anim.pose(*f, 0).unwrap().translation.y)
            .unwrap();
        assert_eq!(low, 5, "fixture should dip at frame 5");

        let (aligned, shift) = align_cycle_to_foot_down(&bytes, 0).unwrap();
        assert_eq!(shift, 5);
        let anim = psx_asset::Animation::from_bytes(&aligned).unwrap();
        assert_eq!(anim.frame_count(), 17, "rotation must not change length");
        let low = (0..16)
            .min_by_key(|f| anim.pose(*f, 0).unwrap().translation.y)
            .unwrap();
        assert_eq!(low, 0, "foot-down must land on frame 0");
        assert_eq!(
            anim.pose(0, 0).unwrap().translation.y,
            anim.pose(16, 0).unwrap().translation.y,
            "last stored frame must still duplicate frame 0"
        );
    }

    /// An already-aligned clip is left alone rather than rewritten.
    #[test]
    fn alignment_is_a_no_op_when_already_at_foot_down() {
        assert!(align_cycle_to_foot_down(&synthetic_gait(17, 0), 0).is_none());
    }
    /// Trimming keeps the endpoint frame the looping runtime expects and
    /// leaves a header the asset reader still accepts.
    #[test]
    fn trim_keeps_one_cycle_plus_the_loop_endpoint() {
        let bytes = synthetic_clip(40, 8);
        let (trimmed, frames, seam) = trim_to_single_cycle(&bytes).unwrap();
        assert_eq!(frames, 9);
        let anim = psx_asset::Animation::from_bytes(&trimmed).unwrap();
        assert_eq!(anim.frame_count(), 9);
        assert_eq!(anim.joint_count(), 1);
        assert_eq!(
            anim.pose(0, 0).unwrap().translation.x,
            anim.pose(8, 0).unwrap().translation.x,
            "last stored frame must duplicate frame 0"
        );
        assert!(seam < 0.01, "exact repeat should have no seam, got {seam}");
    }
}
