use super::*;

#[derive(Default)]
pub(crate) struct AnimationCatalogueReport {
    pub(crate) skeletons_added: usize,
    pub(crate) source_candidates: usize,
    pub(crate) sources_added: usize,
    pub(crate) sources_updated: usize,
    pub(crate) clips_added: usize,
    pub(crate) sets_added: usize,
    pub(crate) sets_updated: usize,
    pub(crate) models_updated: usize,
    pub(crate) characters_updated: usize,
}

impl AnimationCatalogueReport {
    pub(crate) const fn changed(&self) -> bool {
        self.skeletons_added > 0
            || self.sources_added > 0
            || self.sources_updated > 0
            || self.clips_added > 0
            || self.sets_added > 0
            || self.sets_updated > 0
            || self.models_updated > 0
            || self.characters_updated > 0
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AnimationSourceCandidate {
    pub(crate) resource_name: String,
    pub(crate) source_path: String,
    pub(crate) clip_name: String,
    pub(crate) provider: psxed_project::AnimationSourceProvider,
    pub(crate) role: psxed_project::AnimationRole,
    pub(crate) looping: bool,
    pub(crate) tags: Vec<String>,
}

pub(crate) fn catalogue_animation_sources_from_path(
    project: &mut ProjectDocument,
    project_root: &Path,
    source_root: &Path,
) -> Result<AnimationCatalogueReport, String> {
    let mut candidates = Vec::new();
    if source_root.is_dir() {
        collect_animation_source_candidates_from_dir(source_root, project_root, &mut candidates)?;
    } else if is_zip_path(source_root) {
        collect_animation_source_candidates_from_zip(source_root, project_root, &mut candidates)?;
    } else if is_animation_source_file_path(source_root) {
        candidates.push(animation_source_candidate_for_file(
            &EditorWorkspace::display_project_path(source_root, project_root),
            source_root
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("animation"),
        ));
    } else {
        return Err(format!(
            "{} is not an animation source folder, zip, FBX, GLB, or glTF file",
            source_root.display()
        ));
    }

    candidates.sort_by_key(|candidate| {
        (
            candidate.provider.label(),
            candidate.source_path.to_ascii_lowercase(),
        )
    });
    candidates.dedup_by(|a, b| {
        a.source_path.eq_ignore_ascii_case(&b.source_path)
            && a.clip_name.eq_ignore_ascii_case(&b.clip_name)
    });

    let mut report = AnimationCatalogueReport {
        source_candidates: candidates.len(),
        ..AnimationCatalogueReport::default()
    };
    for candidate in candidates {
        match upsert_animation_source_candidate(project, candidate) {
            AnimationSourceUpsert::Added => report.sources_added += 1,
            AnimationSourceUpsert::Updated => report.sources_updated += 1,
            AnimationSourceUpsert::Unchanged => {}
        }
    }
    Ok(report)
}

pub(crate) fn collect_animation_source_candidates_from_dir(
    root: &Path,
    project_root: &Path,
    out: &mut Vec<AnimationSourceCandidate>,
) -> Result<(), String> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let entries =
            std::fs::read_dir(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
        let mut entries: Vec<_> = entries
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("{}: {error}", dir.display()))?;
        entries.sort_by_key(|entry| entry.path());
        for entry in entries.into_iter().rev() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                if !should_skip_animation_source_dir(&path) {
                    pending.push(path);
                }
                continue;
            }
            if !is_animation_source_file_path(&path) {
                continue;
            }
            let stored = EditorWorkspace::display_project_path(&path, project_root);
            if should_skip_animation_source_path(&stored) {
                continue;
            }
            let clip_name = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("animation");
            out.push(animation_source_candidate_for_file(&stored, clip_name));
        }
    }
    Ok(())
}

pub(crate) fn collect_animation_source_candidates_from_zip(
    zip_path: &Path,
    project_root: &Path,
    out: &mut Vec<AnimationSourceCandidate>,
) -> Result<(), String> {
    let file = std::fs::File::open(zip_path)
        .map_err(|error| format!("{}: {error}", zip_path.display()))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|error| format!("{}: {error}", zip_path.display()))?;
    let stored_zip = EditorWorkspace::display_project_path(zip_path, project_root);
    for index in 0..archive.len() {
        let file = archive
            .by_index(index)
            .map_err(|error| format!("{} entry #{index}: {error}", zip_path.display()))?;
        if file.is_dir() {
            continue;
        }
        let entry_name = file.name().replace('\\', "/");
        if !is_animation_source_entry_name(&entry_name)
            || should_skip_animation_source_path(&entry_name)
        {
            continue;
        }
        let clip_name = Path::new(&entry_name)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("animation");
        let stored = format!("{stored_zip}::{entry_name}");
        out.push(animation_source_candidate_for_file(&stored, clip_name));
    }
    Ok(())
}

pub(crate) enum AnimationSourceUpsert {
    Added,
    Updated,
    Unchanged,
}

pub(crate) fn upsert_animation_source_candidate(
    project: &mut ProjectDocument,
    candidate: AnimationSourceCandidate,
) -> AnimationSourceUpsert {
    if let Some(resource) = project
        .resources
        .iter_mut()
        .find(|resource| match &resource.data {
            ResourceData::AnimationSource(source) => {
                source.target_model.is_none()
                    && source
                        .source_path
                        .eq_ignore_ascii_case(&candidate.source_path)
                    && source.clip_name.eq_ignore_ascii_case(&candidate.clip_name)
            }
            _ => false,
        })
    {
        let ResourceData::AnimationSource(source) = &mut resource.data else {
            return AnimationSourceUpsert::Unchanged;
        };
        let mut changed = false;
        if source.provider != candidate.provider {
            source.provider = candidate.provider;
            changed = true;
        }
        if source.role != candidate.role {
            source.role = candidate.role;
            changed = true;
        }
        if source.looping != candidate.looping {
            source.looping = candidate.looping;
            changed = true;
        }
        if source.tags != candidate.tags {
            source.tags = candidate.tags;
            changed = true;
        }
        if changed {
            AnimationSourceUpsert::Updated
        } else {
            AnimationSourceUpsert::Unchanged
        }
    } else {
        project.add_resource(
            candidate.resource_name,
            ResourceData::AnimationSource(psxed_project::AnimationSourceResource {
                source_path: candidate.source_path,
                clip_name: candidate.clip_name,
                provider: candidate.provider,
                skeleton: None,
                target_model: None,
                role: candidate.role,
                looping: candidate.looping,
                tags: candidate.tags,
            }),
        );
        AnimationSourceUpsert::Added
    }
}

pub(crate) fn animation_source_candidate_for_file(
    stored_path: &str,
    clip_name: &str,
) -> AnimationSourceCandidate {
    let provider = psxed_project::AnimationSourceProvider::guess_from_path(stored_path);
    let role = psxed_project::AnimationRole::guess_from_name(clip_name);
    AnimationSourceCandidate {
        resource_name: animation_source_resource_name(clip_name),
        source_path: stored_path.to_string(),
        clip_name: clip_name.to_string(),
        provider,
        role,
        looping: animation_source_default_looping(role, clip_name, stored_path),
        tags: animation_source_tags(provider, role, clip_name, stored_path),
    }
}

pub(crate) fn animation_source_resource_name(clip_name: &str) -> String {
    let mut name = clip_name
        .strip_prefix("A_MOD_SWD_")
        .or_else(|| clip_name.strip_prefix("A_"))
        .unwrap_or(clip_name)
        .trim_end_matches("_Sword")
        .trim_end_matches("_Neut")
        .replace('_', " ");
    name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        "Animation Source".to_string()
    } else {
        name
    }
}

pub(crate) fn animation_source_default_looping(
    role: psxed_project::AnimationRole,
    clip_name: &str,
    source_path: &str,
) -> bool {
    let lowered = format!("{clip_name} {source_path}").to_ascii_lowercase();
    if lowered.contains("loop") {
        return true;
    }
    if lowered.contains("begin")
        || lowered.contains("enter")
        || lowered.contains("end")
        || lowered.contains("exit")
        || lowered.contains("returntoidle")
        || lowered.contains("draw")
        || lowered.contains("sheathe")
        || lowered.contains("parry")
        || lowered.contains("backstep")
        || lowered.contains("back_step")
        || lowered.contains("back step")
        || lowered.contains("step_back")
        || lowered.contains("dodge")
        || lowered.contains("roll")
        || lowered.contains("knockdown")
        || lowered.contains("stagger")
        || lowered.contains("stun")
    {
        return false;
    }
    matches!(
        role,
        psxed_project::AnimationRole::Idle
            | psxed_project::AnimationRole::Walk
            | psxed_project::AnimationRole::Run
            | psxed_project::AnimationRole::Turn
            | psxed_project::AnimationRole::Generic
    )
}

pub(crate) fn animation_source_tags(
    provider: psxed_project::AnimationSourceProvider,
    role: psxed_project::AnimationRole,
    clip_name: &str,
    source_path: &str,
) -> Vec<String> {
    let lowered = format!("{clip_name} {source_path}").to_ascii_lowercase();
    let mut tags = Vec::new();
    if !matches!(provider, psxed_project::AnimationSourceProvider::Unknown) {
        tags.push(provider.label().to_ascii_lowercase());
    }
    if !matches!(role, psxed_project::AnimationRole::Generic) {
        tags.push(role.label().to_ascii_lowercase());
    }
    for (needle, tag) in [
        ("polygon", "polygon"),
        ("sidekick", "sidekick"),
        ("rootmotion", "root_motion"),
        ("_rm_", "root_motion"),
        ("rmh", "root_motion_horizontal"),
        ("rmv", "root_motion_vertical"),
        ("returntoidle", "return_to_idle"),
        ("dodge", "dodge"),
        ("roll", "roll"),
        ("block", "block"),
        ("parry", "parry"),
        ("stagger", "stagger"),
        ("knockdown", "knockdown"),
        ("stun", "stun"),
        ("draw", "draw"),
        ("sheathe", "sheathe"),
        ("sheathed", "sheathed"),
        ("femn", "feminine"),
        ("masc", "masculine"),
        ("sword", "sword"),
    ] {
        if lowered.contains(needle) {
            tags.push(tag.to_string());
        }
    }
    tags.sort();
    tags.dedup();
    tags
}

pub(crate) fn should_skip_animation_source_dir(path: &Path) -> bool {
    let lowered = path.to_string_lossy().to_ascii_lowercase();
    lowered.contains("/target/")
        || lowered.ends_with("/target")
        || lowered.contains("/.git/")
        || lowered.ends_with("/.git")
        || lowered.contains("/models/")
        || lowered.ends_with("/models")
        || lowered.contains("/textures/")
        || lowered.ends_with("/textures")
}

pub(crate) fn should_skip_animation_source_path(path: &str) -> bool {
    let lowered = path.replace('\\', "/").to_ascii_lowercase();
    lowered.contains("/models/")
        || lowered.starts_with("models/")
        || lowered.contains("/textures/")
        || lowered.starts_with("textures/")
        || lowered.contains("/__macosx/")
        || lowered.starts_with("__macosx/")
}

pub(crate) fn is_animation_source_entry_name(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    lowered.ends_with(".fbx") || lowered.ends_with(".glb") || lowered.ends_with(".gltf")
}

pub(crate) fn is_animation_source_file_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| matches!(ext.to_ascii_lowercase().as_str(), "fbx" | "glb" | "gltf"))
        .unwrap_or(false)
}

pub(crate) fn is_zip_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("zip"))
        .unwrap_or(false)
}

pub(crate) fn collect_attachment_socket_names(project: &ProjectDocument) -> Vec<String> {
    let mut names: Vec<String> = project
        .resources
        .iter()
        .filter_map(|resource| match &resource.data {
            ResourceData::Model(model) => Some(model),
            _ => None,
        })
        .flat_map(|model| model.attachments.iter().map(|socket| socket.name.trim()))
        .filter(|name| !name.is_empty())
        .map(|name| name.to_string())
        .collect();
    names.sort_by_key(|name| name.to_ascii_lowercase());
    names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    names
}

/// Collect every Character Profile resource as `(id, name)`. The
/// controller/spawn inspectors use this to populate their pickers.
pub(crate) fn collect_character_options(project: &ProjectDocument) -> Vec<(ResourceId, String)> {
    project
        .resources
        .iter()
        .filter_map(|r| match &r.data {
            ResourceData::Character(_) => Some((r.id, r.name.clone())),
            _ => None,
        })
        .collect()
}

/// Collect every Weapon resource as `(id, name)`.
pub(crate) fn collect_weapon_options(project: &ProjectDocument) -> Vec<(ResourceId, String)> {
    project
        .resources
        .iter()
        .filter_map(|r| match &r.data {
            ResourceData::Weapon(_) => Some((r.id, r.name.clone())),
            _ => None,
        })
        .collect()
}
