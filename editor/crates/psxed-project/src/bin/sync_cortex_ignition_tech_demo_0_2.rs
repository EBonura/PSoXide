//! Non-destructively synchronize Cortex Ignition 0.2 with the current 0.1
//! UI layer and canonical character catalog.
//!
//! The destination's authored 3D scenes are deliberately immutable in this
//! migration. Only UI scenes/flow/options and the dependency closures of the
//! player, light enemy, heavy enemy, and their weapons are synchronized.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use psxed_project::{
    BootTarget, CharacterAnimationAction, CharacterResource, CombatCapsuleRole, MaterialResource,
    ProjectDocument, Resource, ResourceData, ResourceId, UiAction, UiNodeKind, UiScene,
};

const SOURCE_PROJECT: &str = "default";
const DESTINATION_PROJECT: &str = "cortex-ignition-tech-demo-0.2";
const CHARACTER_ROOTS: [(&str, ResourceKind); 5] = [
    ("Aletha", ResourceKind::Character),
    ("Rust Mantis Enemy", ResourceKind::Character),
    ("Tank Boss", ResourceKind::Character),
    ("Sword1 Light", ResourceKind::Weapon),
    ("Sword1 Heavy", ResourceKind::Weapon),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResourceKind {
    Material,
    Model,
    Skeleton,
    AnimationSource,
    AnimationClip,
    AnimationSet,
    Mesh,
    Scene,
    Script,
    Audio,
    Character,
    Weapon,
    BoostModule,
}

impl ResourceKind {
    fn of(data: &ResourceData) -> Self {
        match data {
            ResourceData::Texture { .. } | ResourceData::Material(_) => Self::Material,
            ResourceData::Model(_) => Self::Model,
            ResourceData::Skeleton(_) => Self::Skeleton,
            ResourceData::AnimationSource(_) => Self::AnimationSource,
            ResourceData::AnimationClip(_) => Self::AnimationClip,
            ResourceData::AnimationSet(_) => Self::AnimationSet,
            ResourceData::Mesh { .. } => Self::Mesh,
            ResourceData::Scene { .. } => Self::Scene,
            ResourceData::Script { .. } => Self::Script,
            ResourceData::Audio { .. } => Self::Audio,
            ResourceData::Character(_) => Self::Character,
            ResourceData::Weapon(_) => Self::Weapon,
            ResourceData::BoostModule(_) => Self::BoostModule,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Material => "Material",
            Self::Model => "Model",
            Self::Skeleton => "Skeleton",
            Self::AnimationSource => "AnimationSource",
            Self::AnimationClip => "AnimationClip",
            Self::AnimationSet => "AnimationSet",
            Self::Mesh => "Mesh",
            Self::Scene => "Scene",
            Self::Script => "Script",
            Self::Audio => "Audio",
            Self::Character => "Character",
            Self::Weapon => "Weapon",
            Self::BoostModule => "BoostModule",
        }
    }
}

#[derive(Default)]
struct CopyReport {
    copied: usize,
    unchanged: usize,
    optional_missing: Vec<String>,
}

fn main() {
    let projects_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("projects");
    let source_dir = projects_root.join(SOURCE_PROJECT);
    let destination_dir = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| projects_root.join(DESTINATION_PROJECT));

    let source_path = source_dir.join("project.ron");
    let destination_path = destination_dir.join("project.ron");
    let source = ProjectDocument::load_from_path(&source_path)
        .unwrap_or_else(|error| panic!("load {}: {error}", source_path.display()));
    let mut destination = ProjectDocument::load_from_path(&destination_path)
        .unwrap_or_else(|error| panic!("load {}: {error}", destination_path.display()));
    let original_scenes = destination.scenes.clone();

    let mut copy_report = CopyReport::default();
    let ui_resource_roots = ui_resource_roots(&source);
    let ui_remap = import_resource_closure(
        &source,
        &mut destination,
        &ui_resource_roots,
        &source_dir,
        &destination_dir,
        &mut copy_report,
    );
    synchronize_ui_layer(&source, &mut destination, &ui_remap);
    copy_ui_source_files(&source, &source_dir, &destination_dir, &mut copy_report);

    let character_roots: Vec<ResourceId> = CHARACTER_ROOTS
        .iter()
        .map(|(name, kind)| named_resource(&source, name, *kind))
        .collect();
    let character_remap = import_resource_closure(
        &source,
        &mut destination,
        &character_roots,
        &source_dir,
        &destination_dir,
        &mut copy_report,
    );

    assert_eq!(
        destination.scenes, original_scenes,
        "migration modified the authored v0.2 3D scenes"
    );
    validate_ui_layer(&destination);
    validate_characters(&destination);

    destination
        .save_to_path(&destination_path)
        .unwrap_or_else(|error| panic!("save {}: {error}", destination_path.display()));

    let reloaded = ProjectDocument::load_from_path(&destination_path)
        .unwrap_or_else(|error| panic!("reload {}: {error}", destination_path.display()));
    assert_eq!(
        reloaded.scenes, original_scenes,
        "saved project changed the authored v0.2 3D scenes"
    );
    validate_ui_layer(&reloaded);
    validate_characters(&reloaded);

    println!("synchronized {}", destination_path.display());
    println!(
        "UI: {} scenes, {} screen states, {} options, boot {:?}",
        reloaded.ui_scenes.len(),
        reloaded.scene_states.len(),
        reloaded.options.len(),
        reloaded.boot
    );
    println!(
        "Resources: {} total ({} UI remaps, {} character remaps)",
        reloaded.resources.len(),
        ui_remap.len(),
        character_remap.len()
    );
    println!(
        "Files: {} copied, {} already identical",
        copy_report.copied, copy_report.unchanged
    );
    if !copy_report.optional_missing.is_empty() {
        println!(
            "Optional authoring sources not present in v0.1: {}",
            copy_report.optional_missing.join(", ")
        );
    }
}

fn synchronize_ui_layer(
    source: &ProjectDocument,
    destination: &mut ProjectDocument,
    resource_remap: &HashMap<ResourceId, ResourceId>,
) {
    let mut scenes = source.ui_scenes.clone();
    for scene in &mut scenes {
        remap_ui_scene_resources(scene, resource_remap);
    }
    destination.ui_scenes = scenes;
    destination.scene_states = source.scene_states.clone();
    destination.options = source.options.clone();
    destination.boot = source.boot;
}

fn remap_ui_scene_resources(scene: &mut UiScene, resource_remap: &HashMap<ResourceId, ResourceId>) {
    for node in scene.nodes_mut() {
        match &mut node.kind {
            UiNodeKind::Image { texture, .. } | UiNodeKind::Bar { texture, .. } => {
                remap_option(texture, resource_remap)
            }
            _ => {}
        }
    }
}

fn ui_resource_roots(project: &ProjectDocument) -> Vec<ResourceId> {
    let mut ids = Vec::new();
    for scene in &project.ui_scenes {
        for node in scene.nodes() {
            match &node.kind {
                UiNodeKind::Image {
                    texture: Some(id), ..
                }
                | UiNodeKind::Bar {
                    texture: Some(id), ..
                } => ids.push(*id),
                _ => {}
            }
        }
    }
    ids.sort_by_key(|id| id.raw());
    ids.dedup();
    ids
}

fn import_resource_closure(
    source: &ProjectDocument,
    destination: &mut ProjectDocument,
    roots: &[ResourceId],
    source_dir: &Path,
    destination_dir: &Path,
    copy_report: &mut CopyReport,
) -> HashMap<ResourceId, ResourceId> {
    let selected = resource_dependency_closure(source, roots);
    let mut remap = HashMap::new();

    for resource in &selected {
        let identity = resource_identity(resource);
        let destination_id = destination
            .resources
            .iter()
            .find(|candidate| resource_identity(candidate) == identity)
            .map(|candidate| candidate.id)
            .unwrap_or_else(|| {
                destination.add_resource(resource.name.clone(), resource.data.clone())
            });
        remap.insert(resource.id, destination_id);
    }

    for resource in &selected {
        let destination_id = mapped_resource(&remap, resource.id);
        let mut data = resource.data.clone();
        remap_resource_data(&mut data, &remap);
        if let (ResourceData::Character(existing), ResourceData::Character(source_character)) = (
            &destination
                .resource(destination_id)
                .expect("allocated destination resource")
                .data,
            &data,
        ) {
            data = ResourceData::Character(merge_character_profile(existing, source_character));
        }
        let destination_resource = destination
            .resource_mut(destination_id)
            .expect("newly allocated destination resource");
        destination_resource.name = resource.name.clone();
        destination_resource.data = data;
        copy_resource_files(resource, source_dir, destination_dir, copy_report);
    }

    remap
}

/// Keep project-specific movement, camera, hurtbox and encounter tuning while
/// accepting the canonical visual/animation references and action-driven
/// dealing volumes. This prevents an Animation Studio hurtbox experiment in
/// the source template from silently changing a playable project's combat.
fn merge_character_profile(
    existing: &CharacterResource,
    source: &CharacterResource,
) -> CharacterResource {
    let mut merged = existing.clone();
    merged.model = source.model;
    merged.material = source.material;
    merged.animation_set = source.animation_set;
    merged
        .combat_capsules
        .retain(|capsule| matches!(capsule.role, CombatCapsuleRole::Hurtbox));
    merged.combat_capsules.extend(
        source
            .combat_capsules
            .iter()
            .filter(|capsule| !matches!(capsule.role, CombatCapsuleRole::Hurtbox))
            .cloned(),
    );
    merged
}

fn resource_dependency_closure(project: &ProjectDocument, roots: &[ResourceId]) -> Vec<Resource> {
    let mut pending = roots.to_vec();
    let mut selected = HashSet::new();
    while let Some(id) = pending.pop() {
        if !selected.insert(id) {
            continue;
        }
        let resource = project
            .resource(id)
            .unwrap_or_else(|| panic!("missing dependency resource #{}", id.raw()));
        pending.extend(resource_dependencies(&resource.data));
    }
    project
        .resources
        .iter()
        .filter(|resource| selected.contains(&resource.id))
        .cloned()
        .collect()
}

fn resource_dependencies(data: &ResourceData) -> Vec<ResourceId> {
    let mut ids = Vec::new();
    let mut option = |id: Option<ResourceId>| ids.extend(id);
    match data {
        ResourceData::Material(material) => material_dependencies(material, &mut ids),
        ResourceData::Model(model) => option(model.skeleton),
        ResourceData::AnimationSource(source) => {
            option(source.skeleton);
            option(source.target_model);
        }
        ResourceData::AnimationClip(clip) => {
            option(clip.skeleton);
            option(clip.target_model);
            option(clip.source);
        }
        ResourceData::AnimationSet(set) => {
            for id in [
                set.skeleton,
                set.idle_clip,
                set.walk_clip,
                set.run_clip,
                set.turn_clip,
                set.roll_clip,
                set.backstep_clip,
            ] {
                option(id);
            }
            ids.extend(set.action_clips.iter().map(|binding| binding.clip));
            ids.extend(
                set.weapon_appearance_tracks
                    .iter()
                    .map(|track| track.weapon),
            );
            ids.extend(set.clips.iter().copied());
        }
        ResourceData::Character(character) => {
            option(character.model);
            option(character.material);
            option(character.animation_set);
        }
        ResourceData::Weapon(weapon) => option(weapon.model),
        ResourceData::Texture { .. }
        | ResourceData::Skeleton(_)
        | ResourceData::Mesh { .. }
        | ResourceData::Scene { .. }
        | ResourceData::Script { .. }
        | ResourceData::Audio { .. }
        | ResourceData::BoostModule(_) => {}
    }
    ids
}

fn material_dependencies(material: &MaterialResource, ids: &mut Vec<ResourceId>) {
    ids.extend(material.transition.source_a);
    ids.extend(material.transition.source_b);
    if let Some(layer) = &material.secondary_layer {
        ids.extend(layer.transition.source_a);
        ids.extend(layer.transition.source_b);
    }
    for version in &material.versions {
        ids.extend(version.recipe.transition.source_a);
        ids.extend(version.recipe.transition.source_b);
        if let Some(layer) = &version.recipe.secondary_layer {
            ids.extend(layer.transition.source_a);
            ids.extend(layer.transition.source_b);
        }
    }
}

fn remap_resource_data(data: &mut ResourceData, remap: &HashMap<ResourceId, ResourceId>) {
    match data {
        ResourceData::Material(material) => remap_material(material, remap),
        ResourceData::Model(model) => remap_option(&mut model.skeleton, remap),
        ResourceData::AnimationSource(source) => {
            remap_option(&mut source.skeleton, remap);
            remap_option(&mut source.target_model, remap);
        }
        ResourceData::AnimationClip(clip) => {
            remap_option(&mut clip.skeleton, remap);
            remap_option(&mut clip.target_model, remap);
            remap_option(&mut clip.source, remap);
        }
        ResourceData::AnimationSet(set) => {
            for id in [
                &mut set.skeleton,
                &mut set.idle_clip,
                &mut set.walk_clip,
                &mut set.run_clip,
                &mut set.turn_clip,
                &mut set.roll_clip,
                &mut set.backstep_clip,
            ] {
                remap_option(id, remap);
            }
            for binding in &mut set.action_clips {
                binding.clip = mapped_resource(remap, binding.clip);
            }
            for track in &mut set.weapon_appearance_tracks {
                track.weapon = mapped_resource(remap, track.weapon);
            }
            for clip in &mut set.clips {
                *clip = mapped_resource(remap, *clip);
            }
        }
        ResourceData::Character(character) => {
            remap_option(&mut character.model, remap);
            remap_option(&mut character.material, remap);
            remap_option(&mut character.animation_set, remap);
        }
        ResourceData::Weapon(weapon) => remap_option(&mut weapon.model, remap),
        ResourceData::Texture { .. }
        | ResourceData::Skeleton(_)
        | ResourceData::Mesh { .. }
        | ResourceData::Scene { .. }
        | ResourceData::Script { .. }
        | ResourceData::Audio { .. }
        | ResourceData::BoostModule(_) => {}
    }
}

fn remap_material(material: &mut MaterialResource, remap: &HashMap<ResourceId, ResourceId>) {
    remap_option(&mut material.transition.source_a, remap);
    remap_option(&mut material.transition.source_b, remap);
    if let Some(layer) = &mut material.secondary_layer {
        remap_option(&mut layer.transition.source_a, remap);
        remap_option(&mut layer.transition.source_b, remap);
    }
    for version in &mut material.versions {
        remap_option(&mut version.recipe.transition.source_a, remap);
        remap_option(&mut version.recipe.transition.source_b, remap);
        if let Some(layer) = &mut version.recipe.secondary_layer {
            remap_option(&mut layer.transition.source_a, remap);
            remap_option(&mut layer.transition.source_b, remap);
        }
    }
}

fn remap_option(id: &mut Option<ResourceId>, remap: &HashMap<ResourceId, ResourceId>) {
    if let Some(old) = *id {
        *id = Some(mapped_resource(remap, old));
    }
}

fn mapped_resource(remap: &HashMap<ResourceId, ResourceId>, old: ResourceId) -> ResourceId {
    *remap
        .get(&old)
        .unwrap_or_else(|| panic!("resource #{} escaped dependency closure", old.raw()))
}

fn named_resource(
    project: &ProjectDocument,
    name: &str,
    expected_kind: ResourceKind,
) -> ResourceId {
    project
        .resources
        .iter()
        .find(|resource| resource.name == name && ResourceKind::of(&resource.data) == expected_kind)
        .unwrap_or_else(|| {
            panic!(
                "canonical {} resource {name:?} not found",
                expected_kind.label()
            )
        })
        .id
}

fn resource_identity(resource: &Resource) -> String {
    let kind = ResourceKind::of(&resource.data).label();
    match &resource.data {
        ResourceData::Texture { psxt_path } => format!("{kind}:{psxt_path}"),
        ResourceData::Material(material) => format!(
            "{kind}:{}:{}",
            resource.name,
            material.psxt_path.as_deref().unwrap_or("")
        ),
        ResourceData::Model(model) => format!("{kind}:{}", model.model_path),
        ResourceData::Skeleton(skeleton) => format!("{kind}:{}", skeleton.signature),
        ResourceData::AnimationSource(source) => {
            format!("{kind}:{}:{}", source.source_path, source.clip_name)
        }
        ResourceData::AnimationClip(clip) => format!("{kind}:{}", clip.psxanim_path),
        ResourceData::AnimationSet(_) => format!("{kind}:{}", resource.name),
        ResourceData::Mesh { source_path }
        | ResourceData::Scene { source_path }
        | ResourceData::Script { source_path }
        | ResourceData::Audio { source_path } => format!("{kind}:{source_path}"),
        ResourceData::Character(_) | ResourceData::Weapon(_) | ResourceData::BoostModule(_) => {
            format!("{kind}:{}", resource.name)
        }
    }
}

fn copy_resource_files(
    resource: &Resource,
    source_dir: &Path,
    destination_dir: &Path,
    report: &mut CopyReport,
) {
    match &resource.data {
        ResourceData::Texture { psxt_path } => {
            copy_required(psxt_path, source_dir, destination_dir, report)
        }
        ResourceData::Material(material) => {
            for path in material_texture_paths(material) {
                copy_required(path, source_dir, destination_dir, report);
            }
        }
        ResourceData::Model(model) => {
            copy_required(&model.model_path, source_dir, destination_dir, report);
            if let Some(path) = &model.texture_path {
                copy_required(path, source_dir, destination_dir, report);
            }
            if let Some(path) = &model.source_path {
                copy_optional(path, source_dir, destination_dir, report);
            }
        }
        ResourceData::AnimationSource(source) => {
            copy_optional(&source.source_path, source_dir, destination_dir, report)
        }
        ResourceData::AnimationClip(clip) => {
            copy_required(&clip.psxanim_path, source_dir, destination_dir, report)
        }
        ResourceData::Mesh { source_path }
        | ResourceData::Scene { source_path }
        | ResourceData::Script { source_path }
        | ResourceData::Audio { source_path } => {
            copy_required(source_path, source_dir, destination_dir, report)
        }
        ResourceData::Skeleton(_)
        | ResourceData::AnimationSet(_)
        | ResourceData::Character(_)
        | ResourceData::Weapon(_)
        | ResourceData::BoostModule(_) => {}
    }
}

fn material_texture_paths(material: &MaterialResource) -> Vec<&str> {
    let mut paths = Vec::new();
    if let Some(path) = material.psxt_path.as_deref() {
        paths.push(path);
    }
    if let Some(path) = material
        .secondary_layer
        .as_ref()
        .and_then(|layer| layer.psxt_path.as_deref())
    {
        paths.push(path);
    }
    for version in &material.versions {
        if let Some(path) = version.recipe.psxt_path.as_deref() {
            paths.push(path);
        }
        if let Some(path) = version
            .recipe
            .secondary_layer
            .as_ref()
            .and_then(|layer| layer.psxt_path.as_deref())
        {
            paths.push(path);
        }
    }
    paths.sort_unstable();
    paths.dedup();
    paths
}

fn copy_ui_source_files(
    source: &ProjectDocument,
    source_dir: &Path,
    destination_dir: &Path,
    report: &mut CopyReport,
) {
    let mut paths = Vec::new();
    for scene in &source.ui_scenes {
        for node in scene.nodes() {
            match &node.kind {
                UiNodeKind::Music { wav_path, .. } => paths.push(wav_path.as_str()),
                UiNodeKind::Button { sfx, .. } | UiNodeKind::Slider { sfx, .. } => {
                    paths.extend(sfx.focus.iter().map(|cue| cue.wav_path.as_str()));
                    paths.extend(sfx.activate.iter().map(|cue| cue.wav_path.as_str()));
                    paths.extend(sfx.nudge.iter().map(|cue| cue.wav_path.as_str()));
                    paths.extend(sfx.limit.iter().map(|cue| cue.wav_path.as_str()));
                }
                _ => {}
            }
        }
    }
    paths.retain(|path| !path.trim().is_empty());
    paths.sort_unstable();
    paths.dedup();
    for path in paths {
        copy_required(path, source_dir, destination_dir, report);
    }
}

fn copy_required(stored: &str, source_dir: &Path, destination_dir: &Path, report: &mut CopyReport) {
    let (source, destination) = checked_project_paths(stored, source_dir, destination_dir);
    assert!(
        source.is_file(),
        "required source asset is missing: {}",
        source.display()
    );
    copy_if_changed(&source, &destination, report);
}

fn copy_optional(stored: &str, source_dir: &Path, destination_dir: &Path, report: &mut CopyReport) {
    let (source, destination) = checked_project_paths(stored, source_dir, destination_dir);
    if !source.is_file() {
        report.optional_missing.push(stored.to_string());
        return;
    }
    copy_if_changed(&source, &destination, report);
}

fn checked_project_paths(
    stored: &str,
    source_dir: &Path,
    destination_dir: &Path,
) -> (PathBuf, PathBuf) {
    let relative = Path::new(stored);
    assert!(
        !relative.is_absolute(),
        "project asset path must be relative: {stored}"
    );
    assert!(
        relative
            .components()
            .all(|component| !matches!(component, Component::ParentDir)),
        "project asset path escapes its project: {stored}"
    );
    (source_dir.join(relative), destination_dir.join(relative))
}

fn copy_if_changed(source: &Path, destination: &Path, report: &mut CopyReport) {
    if destination.is_file()
        && fs::read(source).expect("read source asset")
            == fs::read(destination).expect("read destination asset")
    {
        report.unchanged += 1;
        return;
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).expect("create destination asset directory");
    }
    fs::copy(source, destination).unwrap_or_else(|error| {
        panic!(
            "copy {} to {}: {error}",
            source.display(),
            destination.display()
        )
    });
    report.copied += 1;
}

fn validate_ui_layer(project: &ProjectDocument) {
    let scene_ids: HashSet<_> = project.ui_scenes.iter().map(|scene| scene.id).collect();
    let state_ids: HashSet<_> = project.scene_states.iter().map(|state| state.id).collect();
    let option_ids: HashSet<_> = project.options.iter().map(|option| option.id).collect();
    assert!(!project.ui_scenes.is_empty(), "project has no UI scenes");
    assert!(
        project.scene_states.iter().any(|state| {
            state.world == psxed_project::SceneWorldLayer::Gameplay
                && state.ui_scene == project.ui_scenes.first().map(|scene| scene.id)
        }),
        "gameplay screen state does not include the HUD"
    );
    for state in &project.scene_states {
        if let Some(scene) = state.ui_scene {
            assert!(
                scene_ids.contains(&scene),
                "state references missing UI scene"
            );
        }
        if let Some(target) = state.start_state {
            assert!(
                state_ids.contains(&target),
                "state references missing START target"
            );
        }
    }
    match project.boot {
        BootTarget::Gameplay => {}
        BootTarget::UiScene(scene) => assert!(scene_ids.contains(&scene)),
        BootTarget::SceneState(state) => assert!(state_ids.contains(&state)),
    }
    for scene in &project.ui_scenes {
        for node in scene.nodes() {
            match &node.kind {
                UiNodeKind::Image {
                    texture: Some(id), ..
                }
                | UiNodeKind::Bar {
                    texture: Some(id), ..
                } => {
                    let resource = project.resource(*id).unwrap_or_else(|| {
                        panic!("UI node references missing resource #{}", id.raw())
                    });
                    assert!(matches!(resource.data, ResourceData::Material(_)));
                }
                UiNodeKind::Slider { option, .. } => {
                    assert!(
                        option_ids.contains(option),
                        "slider references missing option"
                    )
                }
                UiNodeKind::Music {
                    volume_option: Some(option),
                    ..
                } => assert!(
                    option_ids.contains(option),
                    "music node references missing option"
                ),
                UiNodeKind::Button { action, .. } | UiNodeKind::Timer { action, .. } => {
                    validate_ui_action(*action, &scene_ids, &state_ids, &option_ids)
                }
                _ => {}
            }
        }
    }
}

fn validate_ui_action(
    action: UiAction,
    scene_ids: &HashSet<psxed_project::UiSceneId>,
    state_ids: &HashSet<psxed_project::SceneStateId>,
    option_ids: &HashSet<psxed_project::OptionId>,
) {
    match action {
        UiAction::GotoScene(scene) | UiAction::TransitionToScene { scene, .. } => {
            assert!(
                scene_ids.contains(&scene),
                "UI action targets missing scene"
            )
        }
        UiAction::GotoState(state) | UiAction::TransitionToState { state, .. } => {
            assert!(
                state_ids.contains(&state),
                "UI action targets missing state"
            )
        }
        UiAction::SetOption { option, .. } => {
            assert!(
                option_ids.contains(&option),
                "UI action targets missing option"
            )
        }
        UiAction::StartGameplay
        | UiAction::StartGameplayTransition { .. }
        | UiAction::Back
        | UiAction::Game(_) => {}
    }
}

fn validate_characters(project: &ProjectDocument) {
    validate_character_actions(
        project,
        "Aletha",
        &[
            CharacterAnimationAction::Idle,
            CharacterAnimationAction::Walk,
            CharacterAnimationAction::Run,
            CharacterAnimationAction::WalkBackward,
            CharacterAnimationAction::StrafeLeft,
            CharacterAnimationAction::StrafeRight,
            CharacterAnimationAction::LightAttack,
            CharacterAnimationAction::HeavyAttack,
            CharacterAnimationAction::ComboAttack,
            CharacterAnimationAction::VertLightAttack,
            CharacterAnimationAction::VertHeavyAttack,
            CharacterAnimationAction::VertComboAttack,
            CharacterAnimationAction::HitReact,
            CharacterAnimationAction::Death,
        ],
    );
    validate_character_actions(
        project,
        "Rust Mantis Enemy",
        &[
            CharacterAnimationAction::Idle,
            CharacterAnimationAction::Walk,
            CharacterAnimationAction::Run,
            CharacterAnimationAction::Turn,
            CharacterAnimationAction::WalkBackward,
            CharacterAnimationAction::StrafeLeft,
            CharacterAnimationAction::StrafeRight,
            CharacterAnimationAction::Intro,
            CharacterAnimationAction::LightAttack,
            CharacterAnimationAction::HitReact,
            CharacterAnimationAction::Death,
        ],
    );
    validate_character_actions(
        project,
        "Tank Boss",
        &[
            CharacterAnimationAction::Idle,
            CharacterAnimationAction::Walk,
            CharacterAnimationAction::WalkBackward,
            CharacterAnimationAction::StrafeLeft,
            CharacterAnimationAction::StrafeRight,
            CharacterAnimationAction::LightAttack,
            CharacterAnimationAction::HitReact,
            CharacterAnimationAction::Stun,
            CharacterAnimationAction::Death,
        ],
    );
}

fn validate_character_actions(
    project: &ProjectDocument,
    name: &str,
    required_actions: &[CharacterAnimationAction],
) {
    let character_id = named_resource(project, name, ResourceKind::Character);
    let character = match &project.resource(character_id).expect("character").data {
        ResourceData::Character(character) => character,
        _ => unreachable!(),
    };
    let model_id = character.model.expect("character must have a model");
    let model = match &project.resource(model_id).expect("character model").data {
        ResourceData::Model(model) => model,
        _ => panic!("{name} model reference is not a Model resource"),
    };
    let set_id = character
        .animation_set
        .unwrap_or_else(|| panic!("{name} has no animation set"));
    let set = match &project
        .resource(set_id)
        .expect("character animation set")
        .data
    {
        ResourceData::AnimationSet(set) => set,
        _ => panic!("{name} animation-set reference is not an AnimationSet resource"),
    };
    assert_eq!(
        set.skeleton, model.skeleton,
        "{name} model and animation set use different skeletons"
    );
    for action in required_actions {
        let clip_id = set
            .action_clip(*action)
            .unwrap_or_else(|| panic!("{name} is missing {}", action.label()));
        let clip = match &project
            .resource(clip_id)
            .unwrap_or_else(|| panic!("{name} action references missing clip"))
            .data
        {
            ResourceData::AnimationClip(clip) => clip,
            _ => panic!(
                "{name} {} does not reference an AnimationClip",
                action.label()
            ),
        };
        assert_eq!(
            clip.skeleton, model.skeleton,
            "{name} clip skeleton mismatch"
        );
        assert!(
            clip.target_model.is_none_or(|target| target == model_id),
            "{name} clip targets a different model"
        );
    }
    let actions: Vec<&str> = CharacterAnimationAction::AUTHORABLE
        .iter()
        .filter(|action| set.action_clip(**action).is_some())
        .map(|action| action.label())
        .collect();
    println!(
        "{name}: {} mapped actions [{}]",
        actions.len(),
        actions.join(", ")
    );
}
