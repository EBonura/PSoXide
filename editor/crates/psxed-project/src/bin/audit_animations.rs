//! Read-only animation catalogue audit for an editor project.
//!
//! Usage:
//!   cargo run -p psxed-project --bin audit-animations -- path/to/project.ron

use std::{
    collections::{BTreeSet, HashSet},
    env, fs,
    path::Path,
};

use psx_asset::Animation;
use psxed_project::{
    model_import::resolve_path,
    playtest::{build_package, PlaytestPackage, CHARACTER_CLIP_NONE},
    CharacterAnimationAction, ProjectDocument, ResourceData, ResourceId,
};

fn main() {
    let args = env::args_os().skip(1).collect::<Vec<_>>();
    let Some(project_path) = args.first() else {
        eprintln!("usage: audit-animations <project.ron>");
        std::process::exit(2);
    };
    let runtime_audit = args.iter().any(|arg| arg == "--runtime");
    let project_path = Path::new(project_path);
    let project_root = project_path.parent().unwrap_or_else(|| Path::new("."));
    let project = match ProjectDocument::load_from_path(project_path) {
        Ok(project) => project,
        Err(error) => {
            eprintln!(
                "animation audit could not load {}: {error}",
                project_path.display()
            );
            std::process::exit(2);
        }
    };

    let model_count = project
        .resources
        .iter()
        .filter(|resource| matches!(resource.data, ResourceData::Model(_)))
        .count();
    let clip_count = project
        .resources
        .iter()
        .filter(|resource| matches!(resource.data, ResourceData::AnimationClip(_)))
        .count();
    let source_count = project
        .resources
        .iter()
        .filter(|resource| matches!(resource.data, ResourceData::AnimationSource(_)))
        .count();
    let set_count = project
        .resources
        .iter()
        .filter(|resource| matches!(resource.data, ResourceData::AnimationSet(_)))
        .count();
    println!("ANIMATION CATALOGUE AUDIT");
    println!("project: {}", project_path.display());
    println!(
        "resources: {model_count} models · {clip_count} cooked clips · {source_count} raw sources · {set_count} sets"
    );

    let mut errors = Vec::new();
    let mut registered_paths = BTreeSet::new();
    let mut set_clips = HashSet::new();

    println!("\nMODEL CATALOGUES");
    for resource in &project.resources {
        let ResourceData::Model(model) = &resource.data else {
            continue;
        };
        let skeleton_name = display_resource(&project, model.skeleton);
        let clips = project.resolved_model_animation_clips(resource.id);
        println!(
            "- {} [#{}] · {} · {} clips",
            resource.name,
            resource.id.raw(),
            skeleton_name,
            clips.len()
        );
        for clip in clips {
            println!("    {} -> {}", clip.name, clip.psxanim_path);
        }
    }

    println!("\nANIMATION SETS");
    for resource in &project.resources {
        let ResourceData::AnimationSet(set) = &resource.data else {
            continue;
        };
        println!(
            "- {} [#{}] · {}",
            resource.name,
            resource.id.raw(),
            display_resource(&project, set.skeleton)
        );
        for binding in &set.action_clips {
            set_clips.insert(binding.clip);
            let clip_name = project
                .resource_name(binding.clip)
                .unwrap_or("<missing clip>");
            println!(
                "    {:<24} {} [#{}]",
                binding.action.label(),
                clip_name,
                binding.clip.raw()
            );
            validate_set_clip(
                &project,
                &resource.name,
                set.skeleton,
                binding.clip,
                &mut errors,
            );
        }
        for clip in &set.clips {
            set_clips.insert(*clip);
            validate_set_clip(&project, &resource.name, set.skeleton, *clip, &mut errors);
        }
    }

    println!("\nCOOKED CLIPS");
    for resource in &project.resources {
        let ResourceData::AnimationClip(clip) = &resource.data else {
            continue;
        };
        registered_paths.insert(normalize_relative_path(&clip.psxanim_path));
        let path = resolve_path(&clip.psxanim_path, Some(project_root));
        let status = match fs::read(&path) {
            Ok(bytes) => match Animation::from_bytes(&bytes) {
                Ok(animation) => {
                    let expected_joints = clip.skeleton.and_then(|skeleton| {
                        project.resource(skeleton).and_then(|resource| {
                            let ResourceData::Skeleton(skeleton) = &resource.data else {
                                return None;
                            };
                            Some(skeleton.joint_count)
                        })
                    });
                    if expected_joints.is_some_and(|count| count != animation.joint_count()) {
                        errors.push(format!(
                            "clip '{}' has {} joints but {} expects {}",
                            resource.name,
                            animation.joint_count(),
                            display_resource(&project, clip.skeleton),
                            expected_joints.unwrap_or_default()
                        ));
                    }
                    format!(
                        "{} frames @ {} Hz · {} joints",
                        animation.frame_count(),
                        animation.sample_rate_hz(),
                        animation.joint_count()
                    )
                }
                Err(_) => {
                    errors.push(format!(
                        "clip '{}' is not a valid PSX animation",
                        resource.name
                    ));
                    "INVALID".to_string()
                }
            },
            Err(error) => {
                errors.push(format!(
                    "clip '{}' is missing {}: {error}",
                    resource.name,
                    path.display()
                ));
                "MISSING".to_string()
            }
        };
        if let Some(target_model) = clip.target_model {
            let compatible = project.resource(target_model).is_some_and(|resource| {
                let ResourceData::Model(model) = &resource.data else {
                    return false;
                };
                model.skeleton == clip.skeleton
            });
            if !compatible {
                errors.push(format!(
                    "clip '{}' targets incompatible model {}",
                    resource.name,
                    display_resource(&project, Some(target_model))
                ));
            }
        }
        println!(
            "- {} [#{}] · {} · {} · {}{}",
            resource.name,
            resource.id.raw(),
            display_resource(&project, clip.skeleton),
            clip.role.label(),
            status,
            if set_clips.contains(&resource.id) {
                ""
            } else {
                " · library only"
            }
        );
    }

    let mut cooked_files = Vec::new();
    collect_animation_files(
        &project_root.join("assets/animations"),
        project_root,
        &mut cooked_files,
    );
    cooked_files.sort();
    let unregistered = cooked_files
        .into_iter()
        .filter(|path| !registered_paths.contains(path))
        .collect::<Vec<_>>();
    println!("\nFILES");
    println!("- registered cooked paths: {}", registered_paths.len());
    println!("- unregistered cooked files: {}", unregistered.len());
    for path in &unregistered {
        println!("    {path}");
    }
    if !unregistered.is_empty() {
        errors.push(format!(
            "{} cooked animation file(s) are not registered as Animation Clip resources",
            unregistered.len()
        ));
    }

    if errors.is_empty() {
        println!("\nRESULT: PASS");
    } else {
        println!("\nRESULT: FAIL ({} issue(s))", errors.len());
        for error in &errors {
            println!("- {error}");
        }
        std::process::exit(1);
    }

    if runtime_audit {
        audit_runtime_package(&project, project_root);
    }
}

fn audit_runtime_package(project: &ProjectDocument, project_root: &Path) {
    println!("\nRUNTIME PACKAGE");
    let (package, report) = build_package(project, project_root);
    for warning in &report.warnings {
        println!("- warning: {warning}");
    }
    if !report.errors.is_empty() {
        for error in &report.errors {
            println!("- error: {error}");
        }
        std::process::exit(1);
    }
    let Some(package) = package else {
        println!("- package missing despite a successful report");
        std::process::exit(1);
    };
    let Some(controller) = package.player_controller else {
        println!("- no cooked player controller");
        return;
    };
    let Some(character) = package.characters.get(controller.character as usize) else {
        println!("- cooked player character index is out of range");
        return;
    };
    let Some(model) = package.models.get(character.model as usize) else {
        println!("- cooked player model index is out of range");
        return;
    };
    println!(
        "- player character #{} -> {} [model {}, clips {}..{}]",
        character.source_resource.raw(),
        model.name,
        character.model,
        model.clip_first,
        model.clip_first.saturating_add(model.clip_count),
    );
    let authored_model = project
        .resource(character.source_resource)
        .and_then(|resource| match &resource.data {
            ResourceData::Character(character) => character.model,
            _ => None,
        })
        .and_then(|model_id| project.resource(model_id))
        .and_then(|resource| match &resource.data {
            ResourceData::Model(model) => Some(model),
            _ => None,
        });
    println!("- player sockets:");
    for socket in package
        .model_sockets
        .iter()
        .skip(model.socket_first as usize)
        .take(model.socket_count as usize)
    {
        let authored = authored_model
            .and_then(|model| {
                model
                    .attachments
                    .iter()
                    .find(|item| item.name == socket.name)
            })
            .map(|item| format!("{:?}", item.translation))
            .unwrap_or_else(|| "<missing>".to_string());
        println!(
            "    {} joint {}: authored {authored} -> cooked {:?}",
            socket.name, socket.joint, socket.translation,
        );
    }
    println!("- player equipment:");
    for (index, equipment) in package
        .equipment
        .iter()
        .enumerate()
        .filter(|(_, equipment)| equipment.flags & psx_level::equipment_flags::PLAYER != 0)
    {
        let weapon = package
            .weapons
            .get(equipment.weapon as usize)
            .map(|weapon| weapon.name.as_str())
            .unwrap_or("<invalid weapon>");
        let appearances = package
            .weapon_appearances
            .iter()
            .filter(|appearance| {
                appearance.character == controller.character
                    && appearance.weapon == equipment.weapon
                    && appearance.character_socket == equipment.character_socket
            })
            .count();
        println!(
            "    equipment {index}: {weapon} on {} · {appearances} action track(s)",
            equipment.character_socket,
        );
    }
    for action in [
        CharacterAnimationAction::LightAttack,
        CharacterAnimationAction::HeavyAttack,
        CharacterAnimationAction::VertLightAttack,
        CharacterAnimationAction::VertHeavyAttack,
    ] {
        let index = action.to_index();
        let local = character.action_clips[index];
        if local == CHARACTER_CLIP_NONE {
            println!("- {:<16} UNBOUND", action.label());
            continue;
        }
        let global = usize::from(model.clip_first.saturating_add(local));
        let Some(clip) = package.model_clips.get(global) else {
            println!("- {:<16} invalid local clip {local}", action.label());
            continue;
        };
        let max_matrix_error = source_to_cooked_matrix_error(project, project_root, &package, clip);
        println!(
            "- {:<16} local {:>2} -> {:<24} resource #{:<3} source {}..{}/{} cooked {} speed {:>4} range {}..{} matrix_error {}",
            action.label(),
            local,
            clip.name,
            clip.animation_resource.map(ResourceId::raw).unwrap_or_default(),
            clip.source_frame_first,
            clip.source_frame_last,
            clip.source_frame_count,
            clip.cooked_frame_count,
            character.action_speeds[index],
            character.action_frame_ranges[index].start,
            character.action_frame_ranges[index].end,
            max_matrix_error
                .map(|value| value.to_string())
                .unwrap_or_else(|| "n/a".to_string()),
        );
    }
}

fn source_to_cooked_matrix_error(
    project: &ProjectDocument,
    project_root: &Path,
    package: &PlaytestPackage,
    cooked_clip: &psxed_project::playtest::PlaytestModelClip,
) -> Option<i32> {
    let resource = project.resource(cooked_clip.animation_resource?)?;
    let ResourceData::AnimationClip(source_clip) = &resource.data else {
        return None;
    };
    let source_bytes =
        fs::read(resolve_path(&source_clip.psxanim_path, Some(project_root))).ok()?;
    let source = Animation::from_bytes(&source_bytes).ok()?;
    let cooked_bytes = package
        .assets
        .get(cooked_clip.animation_asset_index)?
        .bytes
        .as_slice();
    let cooked = Animation::from_bytes(cooked_bytes).ok()?;
    if source.frame_count() != cooked.frame_count() || source.joint_count() != cooked.joint_count()
    {
        return None;
    }
    let mut max_error = 0i32;
    for frame in 0..source.frame_count() {
        for joint in 0..source.joint_count() {
            let a = source.pose(frame, joint)?;
            let b = cooked.pose(frame, joint)?;
            for column in 0..3 {
                for row in 0..3 {
                    max_error = max_error.max(
                        (i32::from(a.matrix[column][row]) - i32::from(b.matrix[column][row])).abs(),
                    );
                }
            }
        }
    }
    Some(max_error)
}

fn display_resource(project: &ProjectDocument, id: Option<ResourceId>) -> String {
    id.and_then(|id| {
        project
            .resource_name(id)
            .map(|name| format!("{name} [#{}]", id.raw()))
    })
    .unwrap_or_else(|| "<unassigned>".to_string())
}

fn validate_set_clip(
    project: &ProjectDocument,
    set_name: &str,
    set_skeleton: Option<ResourceId>,
    clip_id: ResourceId,
    errors: &mut Vec<String>,
) {
    let Some(resource) = project.resource(clip_id) else {
        errors.push(format!(
            "set '{set_name}' references missing clip #{}",
            clip_id.raw()
        ));
        return;
    };
    let ResourceData::AnimationClip(clip) = &resource.data else {
        errors.push(format!(
            "set '{set_name}' references non-animation '{}'",
            resource.name
        ));
        return;
    };
    if clip.skeleton != set_skeleton {
        errors.push(format!(
            "set '{set_name}' and clip '{}' use different skeletons",
            resource.name
        ));
    }
}

fn collect_animation_files(root: &Path, project_root: &Path, out: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_animation_files(&path, project_root, out);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "psxanim")
        {
            let relative = path.strip_prefix(project_root).unwrap_or(&path);
            out.push(normalize_relative_path(&relative.to_string_lossy()));
        }
    }
}

fn normalize_relative_path(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_string()
}
