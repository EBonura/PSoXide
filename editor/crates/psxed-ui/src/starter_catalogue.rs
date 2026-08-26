use super::*;

/// Recursive directory copy. `std` doesn't ship one and we only
/// need it for the New-Project flow, so a 15-line helper is
/// preferable to taking a dep on `fs_extra`.
pub(crate) fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[derive(Debug, Default)]
pub struct StarterCharacterSyncReport {
    pub resources_added: usize,
    pub resources_updated: usize,
    pub resources_removed: usize,
    pub files_copied: usize,
    pub files_removed: usize,
}

#[derive(Debug, Default)]
pub struct BuiltinSkySyncReport {
    pub resources_added: usize,
    pub resources_updated: usize,
    pub files_written: usize,
}

impl BuiltinSkySyncReport {
    pub const fn changed(&self) -> bool {
        self.resources_added > 0 || self.resources_updated > 0 || self.files_written > 0
    }
}

impl StarterCharacterSyncReport {
    pub const fn changed(&self) -> bool {
        self.resources_added > 0
            || self.resources_updated > 0
            || self.resources_removed > 0
            || self.files_copied > 0
            || self.files_removed > 0
    }
}

#[derive(Clone, Copy)]
pub(crate) enum StarterCataloguePhase {
    Skeleton,
    Material,
    Model,
    /// After models: weapons reference their weapon model by id.
    Weapon,
    AnimationClip,
    AnimationSet,
    Character,
}

pub(crate) fn load_project_with_starter_catalogue(
    dir: &Path,
) -> Result<(ProjectDocument, Option<String>, bool), String> {
    let project_file = dir.join("project.ron");
    let mut project = ProjectDocument::load_from_path(&project_file)
        .map_err(|error| format!("{}: {error}", project_file.display()))?;
    let mut status_parts = Vec::new();
    let sky_report = if should_auto_sync_builtin_sky_catalogue(dir) {
        match sync_builtin_sky_catalogue(&mut project, dir) {
            Ok(report) => report,
            Err(error) => {
                status_parts.push(format!("sky library sync failed: {error}"));
                BuiltinSkySyncReport::default()
            }
        }
    } else {
        BuiltinSkySyncReport::default()
    };
    let character_report = if should_auto_sync_starter_character_catalogue(&project) {
        match sync_starter_character_catalogue(&mut project, dir) {
            Ok(report) => report,
            Err(error) => {
                status_parts.push(format!("starter content sync failed: {error}"));
                StarterCharacterSyncReport::default()
            }
        }
    } else {
        StarterCharacterSyncReport::default()
    };
    if !sky_report.changed() && !character_report.changed() {
        let status = (!status_parts.is_empty())
            .then(|| format!("Loaded {}; {}", short_path(dir), status_parts.join("; ")));
        return Ok((project, status, false));
    }

    if sky_report.changed() {
        status_parts.push(format!(
            "sky library: {} added, {} updated, {} file(s) written",
            sky_report.resources_added, sky_report.resources_updated, sky_report.files_written
        ));
    }
    if character_report.changed() {
        status_parts.push(format!(
            "starter content: {} added, {} updated, {} removed, {} file(s) copied, {} file(s) removed",
            character_report.resources_added,
            character_report.resources_updated,
            character_report.resources_removed,
            character_report.files_copied,
            character_report.files_removed
        ));
    }
    if status_parts.is_empty() {
        return Ok((project, None, false));
    }

    project.normalize_loaded();
    match project.save_to_path(&project_file) {
        Ok(()) => {
            let status = format!("Synced {}", status_parts.join("; "));
            Ok((project, Some(status), false))
        }
        Err(error) => {
            let status = format!(
                "Synced {} but save failed: {error}; save manually",
                status_parts.join("; ")
            );
            Ok((project, Some(status), true))
        }
    }
}

/// Install the two authored PS1 sky atlases and their Material resources.
///
/// The bytes are embedded in the editor so this works in packaged builds as
/// well as a source checkout. Project-local copies keep cooking deterministic
/// and let projects move between machines without depending on a global cache.
pub fn sync_builtin_sky_catalogue(
    project: &mut ProjectDocument,
    project_root: &Path,
) -> Result<BuiltinSkySyncReport, String> {
    let mut report = BuiltinSkySyncReport::default();
    for (name, relative_path, bytes) in [
        (
            BUILTIN_QUAKE_SKY_NAME,
            BUILTIN_QUAKE_SKY_PATH,
            BUILTIN_QUAKE_SKY_BYTES,
        ),
        (
            BUILTIN_CUBE_SKY_NAME,
            BUILTIN_CUBE_SKY_PATH,
            BUILTIN_CUBE_SKY_BYTES,
        ),
    ] {
        let destination = project_root.join(relative_path);
        report.files_written += write_if_changed(&destination, bytes)
            .map_err(|error| format!("write {}: {error}", destination.display()))?;

        let canonical = {
            let mut material = MaterialResource::opaque(Some(relative_path.to_string()));
            material.sky_aperture = true;
            ResourceData::Material(material)
        };
        let existing = project.resources.iter().position(|resource| {
            resource.name == name
                || matches!(
                    &resource.data,
                    ResourceData::Material(material)
                        if material.psxt_path.as_deref() == Some(relative_path)
                )
        });
        if let Some(index) = existing {
            let resource = &mut project.resources[index];
            if resource.name != name || resource.data != canonical {
                resource.name = name.to_string();
                resource.data = canonical;
                report.resources_updated += 1;
            }
        } else {
            project.add_resource(name, canonical);
            report.resources_added += 1;
        }
    }
    Ok(report)
}

fn should_auto_sync_builtin_sky_catalogue(project_root: &Path) -> bool {
    let projects_root = psxed_project::projects_dir();
    project_root
        .parent()
        .is_some_and(|parent| paths_resolve_equal(parent, &projects_root))
        || paths_resolve_equal(project_root, &psxed_project::new_project_template_dir())
}

fn paths_resolve_equal(left: &Path, right: &Path) -> bool {
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> std::io::Result<usize> {
    if std::fs::read(path).is_ok_and(|existing| existing == bytes) {
        return Ok(0);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)?;
    Ok(1)
}

pub(crate) fn should_auto_sync_starter_character_catalogue(project: &ProjectDocument) -> bool {
    let has_legacy_starter_character = project.resources.iter().any(|resource| {
        matches!(resource.data, ResourceData::Character(_))
            && resource.name == LEGACY_WRAITH_HERO_PROFILE_NAME
    });
    let has_legacy_obsidian_warden = project
        .resources
        .iter()
        .any(legacy_obsidian_warden_resource);

    has_legacy_obsidian_warden || has_legacy_starter_character
}

/// The resources-panel "Starter Content" action: bring a project's copy of
/// the verified character catalogue and saved material library (skeletons,
/// materials, models, weapons, clips, sets and profiles) up to date with the
/// embedded default project, matching by name and remapping ids. Also callable headlessly
/// (`cargo run -p psxed-ui --example sync_starter -- <project_dir>`).
pub fn sync_starter_character_catalogue(
    project: &mut ProjectDocument,
    project_root: &Path,
) -> Result<StarterCharacterSyncReport, String> {
    let starter = ProjectDocument::starter();
    let mut report = StarterCharacterSyncReport::default();
    purge_legacy_obsidian_warden_catalogue(project, project_root, &mut report)?;
    let mut id_map: HashMap<ResourceId, ResourceId> = HashMap::new();
    // Target resources already consumed by this run. The starter catalogue
    // legitimately carries several resources sharing one (name, variant)
    // pair (the legacy and Meshy Gold "Crimson Cross Knight" models); with
    // first-match-only lookup they would fight over one target and rewrite
    // it on every sync, so each starter resource claims its own target.
    let mut claimed: HashSet<ResourceId> = HashSet::new();

    for phase in [
        StarterCataloguePhase::Skeleton,
        StarterCataloguePhase::Material,
        StarterCataloguePhase::Model,
        StarterCataloguePhase::Weapon,
        StarterCataloguePhase::AnimationClip,
        StarterCataloguePhase::AnimationSet,
        StarterCataloguePhase::Character,
    ] {
        for starter_resource in starter
            .resources
            .iter()
            .filter(|resource| starter_catalogue_resource_matches_phase(resource, phase))
        {
            let mut data = starter_resource.data.clone();
            remap_resource_data(&mut data, &id_map);
            if let Some(existing_id) =
                find_starter_catalogue_target(project, starter_resource, &claimed)
            {
                claimed.insert(existing_id);
                if let Some(existing) = project.resource_mut(existing_id) {
                    if existing.name != starter_resource.name || existing.data != data {
                        existing.name = starter_resource.name.clone();
                        existing.data = data;
                        report.resources_updated += 1;
                    }
                    id_map.insert(starter_resource.id, existing_id);
                }
            } else {
                let id = project.add_resource(starter_resource.name.clone(), data);
                claimed.insert(id);
                id_map.insert(starter_resource.id, id);
                report.resources_added += 1;
            }
        }
    }

    report.files_copied = copy_starter_character_asset_dirs(project_root)
        .map_err(|error| format!("copy starter content assets: {error}"))?;
    Ok(report)
}

pub(crate) fn purge_legacy_obsidian_warden_catalogue(
    project: &mut ProjectDocument,
    project_root: &Path,
    report: &mut StarterCharacterSyncReport,
) -> Result<(), String> {
    let ids: Vec<ResourceId> = project
        .resources
        .iter()
        .filter(|resource| legacy_obsidian_warden_resource(resource))
        .map(|resource| resource.id)
        .collect();
    for id in ids {
        if project.delete_resource(id).is_some() {
            report.resources_removed += 1;
        }
    }

    let legacy_dir = project_root.join(LEGACY_OBSIDIAN_WARDEN_ASSET_DIR);
    if legacy_dir.exists() {
        let removed_files = count_files_recursive(&legacy_dir).unwrap_or(0);
        std::fs::remove_dir_all(&legacy_dir)
            .map_err(|error| format!("remove legacy Obsidian Warden assets: {error}"))?;
        report.files_removed += removed_files;
    }

    Ok(())
}

pub(crate) fn legacy_obsidian_warden_resource(resource: &Resource) -> bool {
    if LEGACY_OBSIDIAN_WARDEN_RESOURCE_NAMES.contains(&resource.name.as_str()) {
        return true;
    }

    match &resource.data {
        ResourceData::Model(model) => {
            legacy_obsidian_warden_asset_path(&model.model_path)
                || model
                    .texture_path
                    .as_deref()
                    .is_some_and(legacy_obsidian_warden_asset_path)
        }
        ResourceData::AnimationClip(clip) => legacy_obsidian_warden_asset_path(&clip.psxanim_path),
        _ => false,
    }
}

pub(crate) fn legacy_obsidian_warden_asset_path(path: &str) -> bool {
    path.strip_prefix(LEGACY_OBSIDIAN_WARDEN_ASSET_DIR)
        .is_some_and(|remaining| remaining.starts_with('/'))
}

pub(crate) fn starter_catalogue_resource_matches_phase(
    resource: &Resource,
    phase: StarterCataloguePhase,
) -> bool {
    match (&resource.data, phase) {
        (ResourceData::Skeleton(_), StarterCataloguePhase::Skeleton) => {
            STARTER_CHARACTER_SKELETON_NAMES.contains(&resource.name.as_str())
        }
        (ResourceData::Material(material), StarterCataloguePhase::Material) => {
            STARTER_CHARACTER_MATERIAL_NAMES.contains(&resource.name.as_str())
                || material
                    .psxt_path
                    .as_deref()
                    .is_some_and(starter_texture_asset_path)
        }
        (ResourceData::Model(_), StarterCataloguePhase::Model) => {
            STARTER_CHARACTER_MODEL_NAMES.contains(&resource.name.as_str())
        }
        (ResourceData::Weapon(_), StarterCataloguePhase::Weapon) => {
            STARTER_WEAPON_NAMES.contains(&resource.name.as_str())
        }
        (ResourceData::AnimationClip(clip), StarterCataloguePhase::AnimationClip) => {
            starter_character_asset_path(&clip.psxanim_path)
        }
        (ResourceData::AnimationSet(_), StarterCataloguePhase::AnimationSet) => {
            STARTER_ANIMATION_SET_NAMES.contains(&resource.name.as_str())
        }
        (ResourceData::Character(_), StarterCataloguePhase::Character) => {
            STARTER_CHARACTER_PROFILE_NAMES.contains(&resource.name.as_str())
        }
        _ => false,
    }
}

pub(crate) fn find_starter_catalogue_target(
    project: &ProjectDocument,
    starter_resource: &Resource,
    claimed: &HashSet<ResourceId>,
) -> Option<ResourceId> {
    if let ResourceData::Skeleton(starter_skeleton) = &starter_resource.data {
        if let Some(id) = project
            .resources
            .iter()
            .filter(|resource| !claimed.contains(&resource.id))
            .find_map(|resource| match &resource.data {
                ResourceData::Skeleton(existing)
                    if existing.signature == starter_skeleton.signature =>
                {
                    Some(resource.id)
                }
                _ => None,
            })
        {
            return Some(id);
        }
    }

    if let Some(id) = project
        .resources
        .iter()
        .filter(|resource| !claimed.contains(&resource.id))
        .find_map(|resource| {
            (resource.name == starter_resource.name
                && same_resource_variant(&resource.data, &starter_resource.data))
            .then_some(resource.id)
        })
    {
        return Some(id);
    }

    if starter_resource.name == "Rust Mantis Enemy"
        && matches!(starter_resource.data, ResourceData::Character(_))
    {
        return project
            .resources
            .iter()
            .filter(|resource| !claimed.contains(&resource.id))
            .find_map(|resource| {
                (resource.name == LEGACY_WRAITH_HERO_PROFILE_NAME
                    && matches!(resource.data, ResourceData::Character(_)))
                .then_some(resource.id)
            });
    }

    None
}

pub(crate) fn same_resource_variant(a: &ResourceData, b: &ResourceData) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}

pub(crate) fn remap_resource_data(
    data: &mut ResourceData,
    id_map: &HashMap<ResourceId, ResourceId>,
) {
    match data {
        // Materials own their image path; nothing to remap.
        ResourceData::Material(_) => {}
        ResourceData::Model(model) => remap_resource_id_option(&mut model.skeleton, id_map),
        ResourceData::AnimationSource(source) => {
            remap_resource_id_option(&mut source.skeleton, id_map);
            remap_resource_id_option(&mut source.target_model, id_map);
        }
        ResourceData::AnimationClip(clip) => {
            remap_resource_id_option(&mut clip.skeleton, id_map);
            remap_resource_id_option(&mut clip.target_model, id_map);
            remap_resource_id_option(&mut clip.source, id_map);
        }
        ResourceData::AnimationSet(set) => {
            remap_resource_id_option(&mut set.skeleton, id_map);
            remap_resource_id_option(&mut set.idle_clip, id_map);
            remap_resource_id_option(&mut set.walk_clip, id_map);
            remap_resource_id_option(&mut set.run_clip, id_map);
            remap_resource_id_option(&mut set.turn_clip, id_map);
            remap_resource_id_option(&mut set.roll_clip, id_map);
            remap_resource_id_option(&mut set.backstep_clip, id_map);
            for binding in &mut set.action_clips {
                if let Some(mapped) = id_map.get(&binding.clip).copied() {
                    binding.clip = mapped;
                }
            }
            for clip in &mut set.clips {
                if let Some(mapped) = id_map.get(clip).copied() {
                    *clip = mapped;
                }
            }
        }
        ResourceData::Character(character) => {
            remap_resource_id_option(&mut character.model, id_map);
            remap_resource_id_option(&mut character.material, id_map);
            remap_resource_id_option(&mut character.animation_set, id_map);
        }
        ResourceData::Weapon(weapon) => remap_resource_id_option(&mut weapon.model, id_map),
        ResourceData::Texture { .. }
        | ResourceData::Mesh { .. }
        | ResourceData::Scene { .. }
        | ResourceData::Script { .. }
        | ResourceData::Audio { .. }
        | ResourceData::Skeleton(_)
        | ResourceData::BoostModule(_) => {}
    }
}

pub(crate) fn remap_resource_id_option(
    id: &mut Option<ResourceId>,
    id_map: &HashMap<ResourceId, ResourceId>,
) {
    if let Some(mapped) = id.and_then(|id| id_map.get(&id).copied()) {
        *id = Some(mapped);
    }
}

pub(crate) fn starter_character_asset_path(path: &str) -> bool {
    STARTER_CHARACTER_ASSET_DIRS.iter().any(|dir| {
        path.strip_prefix(dir)
            .is_some_and(|remaining| remaining.starts_with('/'))
    })
}

pub(crate) fn starter_texture_asset_path(path: &str) -> bool {
    path.strip_prefix("assets/textures")
        .is_some_and(|remaining| remaining.starts_with('/'))
}

pub(crate) fn copy_starter_character_asset_dirs(project_root: &Path) -> std::io::Result<usize> {
    let default_root = psxed_project::default_project_dir();
    if paths_equivalent(project_root, &default_root) {
        return Ok(0);
    }

    let mut copied = 0;
    for rel in STARTER_CHARACTER_ASSET_DIRS {
        let src = default_root.join(rel);
        let dst = project_root.join(rel);
        copied += copy_dir_recursive_missing(&src, &dst)?;
    }
    for rel in STARTER_CHARACTER_SOURCE_ASSET_PATHS {
        let src = default_root.join(rel);
        let dst = project_root.join(rel);
        copied += copy_path_missing(&src, &dst)?;
    }
    Ok(copied)
}

pub(crate) fn copy_path_missing(src: &Path, dst: &Path) -> std::io::Result<usize> {
    if src.is_dir() {
        return copy_dir_recursive_missing(src, dst);
    }
    if dst.exists() {
        return Ok(0);
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(src, dst)?;
    Ok(1)
}

pub(crate) fn copy_dir_recursive_missing(src: &Path, dst: &Path) -> std::io::Result<usize> {
    std::fs::create_dir_all(dst)?;
    let mut copied = 0;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copied += copy_dir_recursive_missing(&from, &to)?;
        } else if !to.exists() {
            std::fs::copy(&from, &to)?;
            copied += 1;
        }
    }
    Ok(copied)
}

pub(crate) fn count_files_recursive(path: &Path) -> std::io::Result<usize> {
    let mut count = 0;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            count += count_files_recursive(&entry.path())?;
        } else {
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod builtin_sky_tests {
    use super::*;

    #[test]
    fn sky_catalogue_is_project_local_and_idempotent() {
        let root = std::env::temp_dir().join(format!(
            "psoxide-sky-catalogue-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&root);
        let mut project = ProjectDocument::new("sky catalogue");

        let first = sync_builtin_sky_catalogue(&mut project, &root).expect("first sky sync");
        assert_eq!(first.resources_added, 2);
        assert_eq!(first.resources_updated, 0);
        assert_eq!(first.files_written, 2);
        for (name, path, bytes) in [
            (
                BUILTIN_QUAKE_SKY_NAME,
                BUILTIN_QUAKE_SKY_PATH,
                BUILTIN_QUAKE_SKY_BYTES,
            ),
            (
                BUILTIN_CUBE_SKY_NAME,
                BUILTIN_CUBE_SKY_PATH,
                BUILTIN_CUBE_SKY_BYTES,
            ),
        ] {
            let resource = project
                .resources
                .iter()
                .find(|resource| resource.name == name)
                .expect("built-in sky resource");
            let ResourceData::Material(material) = &resource.data else {
                panic!("built-in sky must be a Material");
            };
            assert_eq!(material.psxt_path.as_deref(), Some(path));
            assert!(material.sky_aperture);
            assert_eq!(std::fs::read(root.join(path)).unwrap(), bytes);
        }

        let second = sync_builtin_sky_catalogue(&mut project, &root).expect("repeat sky sync");
        assert!(!second.changed(), "repeat sync must be a no-op: {second:?}");
        let _ = std::fs::remove_dir_all(root);
    }
}
