use super::*;

pub(crate) fn cook_sky_panorama_texture_asset(
    sky: crate::ResolvedSkySettings,
    sky_texture_assets: &mut Vec<(crate::ResolvedSkySettings, usize)>,
    assets: &mut Vec<PlaytestAsset>,
) -> Option<usize> {
    if !sky.enabled {
        return None;
    }
    if let Some((_, existing)) = sky_texture_assets
        .iter()
        .find(|(existing_sky, _)| *existing_sky == sky)
    {
        return Some(*existing);
    }
    let bytes = crate::generate_sky_panorama_psxt(sky)?;
    let sky_index = sky_texture_assets.len();
    let asset_index = assets.len();
    assets.push(PlaytestAsset {
        kind: PlaytestAssetKind::Texture,
        bytes,
        filename: format!("sky/sky_{sky_index:03}.psxt"),
        source_label: format!("Cooked Sky Panorama {sky_index}"),
        // The sky panorama is gameplay-scoped: CD-streamed off UI.PAK and
        // staged through the larger gameplay buffer, loaded on gameplay
        // entry and freed on gameplay exit so it stays out of `.data`.
        streamed_class: StreamedClass::Gameplay,
    });
    sky_texture_assets.push((sky, asset_index));
    Some(asset_index)
}

pub(crate) fn cook_far_vista_texture_asset(
    project: &ProjectDocument,
    project_root: &Path,
    texture_id: ResourceId,
    context: &str,
    texture_asset_for_path: &mut HashMap<String, usize>,
    assets: &mut Vec<PlaytestAsset>,
    report: &mut PlaytestValidationReport,
) -> Option<usize> {
    let Some(texture_resource) = find_resource(project, texture_id) else {
        report.warn(format!(
            "{context}: texture resource #{} is missing; using placeholder",
            texture_id.raw()
        ));
        return None;
    };
    let Some(psxt_path) = resource_psxt_path(texture_resource) else {
        report.warn(format!(
            "{context}: material '{}' has no texture; using placeholder",
            texture_resource.name
        ));
        return None;
    };
    if let Some(existing) = texture_asset_for_path.get(psxt_path).copied() {
        return Some(existing);
    }
    let psxt_path = psxt_path.to_string();
    let bytes = match load_psxt_bytes(&texture_resource.name, &psxt_path, project_root) {
        Ok(bytes) => bytes,
        Err(msg) => {
            report.warn(format!("{context}: {msg}; using placeholder"));
            return None;
        }
    };
    if let Err(msg) = expect_room_material_depth(&texture_resource.name, &bytes) {
        report.warn(format!("{context}: {msg}; using placeholder"));
        return None;
    }

    let texture_index = texture_asset_for_path.len();
    let new_index = assets.len();
    assets.push(PlaytestAsset {
        kind: PlaytestAssetKind::Texture,
        bytes,
        filename: format!("texture_{texture_index:03}.psxt"),
        source_label: texture_resource.name.clone(),
        streamed_class: StreamedClass::None,
    });
    texture_asset_for_path.insert(psxt_path, new_index);
    Some(new_index)
}

pub(crate) fn collect_runtime_model_clip_requirements(
    project: &ProjectDocument,
    scene: &crate::Scene,
) -> HashMap<ResourceId, BTreeSet<u16>> {
    let mut out = HashMap::new();

    for resource in &project.resources {
        match &resource.data {
            ResourceData::Model(_) => {
                add_model_clip_requirement(project, &mut out, resource.id, None);
            }
            ResourceData::Character(character) => {
                add_character_clip_requirements(project, &mut out, character);
            }
            ResourceData::Weapon(weapon) => {
                if let Some(model) = weapon.model {
                    add_model_clip_requirement(project, &mut out, model, None);
                }
            }
            _ => {}
        }
    }

    for node in scene.nodes() {
        match &node.kind {
            NodeKind::Entity => {
                if let Some(model) = component_model_renderer(scene, node).and_then(|r| r.model) {
                    if let Some(animator) = component_animator(scene, node) {
                        add_model_clip_requirement(project, &mut out, model, animator.clip);
                        for binding in animator.action_clips {
                            add_model_clip_requirement(
                                project,
                                &mut out,
                                model,
                                Some(binding.clip),
                            );
                        }
                    } else {
                        add_model_clip_requirement(project, &mut out, model, None);
                    }
                }
                if let Some(controller) = component_character_controller(scene, node) {
                    if let Some(character_id) = controller.character {
                        if let Some(ResourceData::Character(character)) =
                            project.resource(character_id).map(|r| &r.data)
                        {
                            add_character_clip_requirements(project, &mut out, character);
                        }
                    }
                }
                if let Some(equipment) = component_equipment(scene, node) {
                    if let Some(weapon_id) = equipment.weapon {
                        if let Some(ResourceData::Weapon(weapon)) =
                            project.resource(weapon_id).map(|r| &r.data)
                        {
                            if let Some(model) = weapon.model {
                                add_model_clip_requirement(project, &mut out, model, None);
                            }
                        }
                    }
                }
            }
            NodeKind::MeshInstance {
                mesh: Some(model),
                animation_clip,
                ..
            } if project
                .resource(*model)
                .is_some_and(|r| matches!(r.data, ResourceData::Model(_))) =>
            {
                add_model_clip_requirement(project, &mut out, *model, *animation_clip);
            }
            NodeKind::SpawnPoint {
                character: Some(character_id),
                ..
            }
            | NodeKind::CharacterController {
                character: Some(character_id),
                ..
            } => {
                if let Some(ResourceData::Character(character)) =
                    project.resource(*character_id).map(|r| &r.data)
                {
                    add_character_clip_requirements(project, &mut out, character);
                }
            }
            _ => {}
        }
    }

    out
}

pub(crate) fn add_character_clip_requirements(
    project: &ProjectDocument,
    out: &mut HashMap<ResourceId, BTreeSet<u16>>,
    character: &crate::CharacterResource,
) {
    let Some(model) = character.model else {
        return;
    };
    add_model_clip_requirement(project, out, model, None);

    let Some(set) = character.animation_set.and_then(|id| {
        project
            .resource(id)
            .and_then(|resource| match &resource.data {
                ResourceData::AnimationSet(set) => Some(set),
                _ => None,
            })
    }) else {
        return;
    };

    for action in CharacterAnimationAction::ALL {
        if let Some(animation_id) = animation_set_action_clip(project, set, action) {
            if let Some(index) = project.resolved_model_animation_index(model, animation_id) {
                add_model_clip_requirement(project, out, model, Some(index));
            }
        }
    }
}

pub(crate) fn animation_set_action_clip(
    project: &ProjectDocument,
    set: &crate::AnimationSetResource,
    action: CharacterAnimationAction,
) -> Option<ResourceId> {
    if let Some(id) = set.action_clip(action) {
        return Some(id);
    }
    set.clips.iter().copied().find(|id| {
        project
            .resource(*id)
            .and_then(|resource| match &resource.data {
                ResourceData::AnimationClip(clip) => {
                    let role_matches = match action {
                        CharacterAnimationAction::HeavyAttack
                        | CharacterAnimationAction::ComboAttack
                        | CharacterAnimationAction::Block => false,
                        _ => action.role_hint().is_some_and(|role| {
                            clip.role == role
                                || AnimationRole::guess_from_name(&resource.name) == role
                        }),
                    };
                    let action_matches =
                        CharacterAnimationAction::guess_from_name(&resource.name) == Some(action);
                    Some(role_matches || action_matches)
                }
                _ => None,
            })
            .unwrap_or(false)
    })
}

pub(crate) fn character_action_flags_for(
    action: CharacterAnimationAction,
    options: Option<crate::CharacterActionOptions>,
) -> u8 {
    let mut flags = 0;
    if options
        .map(|options| options.looping)
        .unwrap_or_else(|| action.loops_by_default())
    {
        flags |= character_action_flags::LOOPING;
    }
    if let Some(options) = options {
        flags |= character_action_flags::IN_PLACE_OVERRIDE;
        if options.in_place {
            flags |= character_action_flags::IN_PLACE;
        }
    }
    flags
}

/// Q8 playback speed (`256 = 1.0x`) cooked for one action binding,
/// defaulting to unscaled when the binding leaves options unset.
pub(crate) fn character_action_speed_for(options: Option<crate::CharacterActionOptions>) -> u16 {
    options
        .map(|options| options.speed_q8)
        .unwrap_or(crate::ACTION_SPEED_UNSCALED_Q8)
}

pub(crate) fn character_action_frame_range_for(
    options: Option<crate::CharacterActionOptions>,
) -> psx_level::CharacterActionFrameRange {
    let Some(options) = options else {
        return psx_level::CharacterActionFrameRange::FULL;
    };
    let end = if options.frame_end != crate::ACTION_FRAME_END_FULL
        && options.frame_end < options.frame_start
    {
        options.frame_start
    } else {
        options.frame_end
    };
    psx_level::CharacterActionFrameRange {
        start: options.frame_start,
        end,
    }
}

pub(crate) fn character_action_push_for(
    options: Option<crate::CharacterActionOptions>,
) -> psx_level::CharacterActionPush {
    let Some(options) = options else {
        return psx_level::CharacterActionPush::NONE;
    };
    let end = if options.push_frame_end != crate::ACTION_FRAME_END_FULL
        && options.push_frame_end < options.push_frame_start
    {
        options.push_frame_start
    } else {
        options.push_frame_end
    };
    psx_level::CharacterActionPush {
        distance: options.push_distance.max(0),
        frame_range: psx_level::CharacterActionFrameRange {
            start: options.push_frame_start,
            end,
        },
    }
}

pub(crate) fn add_model_clip_requirement(
    project: &ProjectDocument,
    out: &mut HashMap<ResourceId, BTreeSet<u16>>,
    model: ResourceId,
    clip: Option<u16>,
) {
    let resolved_len = project.resolved_model_animation_clips(model).len();
    if resolved_len == 0 {
        return;
    }
    let index = clip.unwrap_or(0);
    if (index as usize) < resolved_len {
        out.entry(model).or_default().insert(index);
    }
}

pub(crate) fn runtime_model_clip_indices(
    resolved_len: usize,
    required: Option<&BTreeSet<u16>>,
    default_clip: u16,
) -> Vec<u16> {
    let mut selected = BTreeSet::new();
    if (default_clip as usize) < resolved_len {
        selected.insert(default_clip);
    }
    if let Some(required) = required {
        for index in required {
            if (*index as usize) < resolved_len {
                selected.insert(*index);
            }
        }
    }
    if selected.is_empty() && resolved_len > 0 {
        selected.insert(0);
    }
    selected.into_iter().collect()
}

pub(crate) fn remap_runtime_model_clip(
    remaps: &HashMap<ResourceId, Vec<Option<u16>>>,
    model: ResourceId,
    authored_index: u16,
) -> Option<u16> {
    remaps
        .get(&model)
        .and_then(|model_remap| model_remap.get(authored_index as usize))
        .copied()
        .flatten()
}

/// Cook one Character resource into a [`PlaytestCharacter`],
/// registering its backing model on first sight (deduped against
/// MeshInstance placements). Validates clip indices land inside
/// the resolved model's clip slice; the runtime trusts the
/// contract.
#[allow(clippy::too_many_arguments)]
pub(crate) fn cook_player_character(
    project: &ProjectDocument,
    project_root: &Path,
    spawn_node: &SceneNode,
    character_id: Option<ResourceId>,
    model_override: Option<ResourceId>,
    visual_offset: [i16; 3],
    visual_yaw: i16,
    visual_scale_q8: u16,
    action_overrides: &[crate::CharacterActionClip],
    controller_settings: Option<CharacterControllerSettings>,
    camera_settings: Option<WorldCameraSettings>,
    weight_q8: u16,
    assets: &mut Vec<PlaytestAsset>,
    models: &mut Vec<PlaytestModel>,
    model_clips: &mut Vec<PlaytestModelClip>,
    model_clip_bounds: &mut Vec<PlaytestModelClipBounds>,
    model_frame_bounds: &mut Vec<PlaytestModelFrameBounds>,
    model_sockets: &mut Vec<PlaytestModelSocket>,
    model_for_resource: &mut std::collections::HashMap<ResourceId, u16>,
    runtime_model_clips: &HashMap<ResourceId, BTreeSet<u16>>,
    model_clip_remaps: &mut HashMap<ResourceId, Vec<Option<u16>>>,
    characters: &mut Vec<PlaytestCharacter>,
    report: &mut PlaytestValidationReport,
) -> Option<u16> {
    let default_character = crate::CharacterResource::defaults();
    let (character, character_name) = match character_id {
        Some(character_id) => {
            let resource = match project.resource(character_id) {
                Some(r) => r,
                None => {
                    report.error(format!(
                        "Player Spawn '{}' references Character #{} which doesn't exist",
                        spawn_node.name,
                        character_id.raw()
                    ));
                    return None;
                }
            };
            match &resource.data {
                ResourceData::Character(c) => (c, resource.name.as_str()),
                _ => {
                    report.error(format!(
                        "Player Spawn '{}' references resource '{}' which is not a Character",
                        spawn_node.name, resource.name
                    ));
                    return None;
                }
            }
        }
        None => (&default_character, spawn_node.name.as_str()),
    };
    let settings = controller_settings
        .unwrap_or_else(|| CharacterControllerSettings::from_character(character));
    let camera = camera_settings.unwrap_or(WorldCameraSettings {
        distance: character.camera_distance,
        height: character.camera_height,
        target_height: character.camera_target_height,
        min_floor_clearance: crate::default_world_camera_min_floor_clearance(),
        orbit_speed_level: crate::default_world_camera_orbit_speed_level(),
        position_lag_shift: crate::default_world_camera_position_lag_shift(),
        focus_lag_shift: crate::default_world_camera_focus_lag_shift(),
        distance_lag_shift: crate::default_world_camera_distance_lag_shift(),
    });
    let camera = camera.normalized();

    let model_resource_id = match model_override.or(character.model) {
        Some(id) => id,
        None => {
            report.error(format!(
                "Character '{}' has no Model assigned -- add a Model Renderer or set a profile Model",
                character_name
            ));
            return None;
        }
    };
    let model_index = register_model_for_instance(
        project,
        project_root,
        model_resource_id,
        assets,
        models,
        model_clips,
        model_clip_bounds,
        model_frame_bounds,
        model_sockets,
        model_for_resource,
        runtime_model_clips,
        model_clip_remaps,
        report,
    )?;
    let model = &models[model_index as usize];

    let model_skeleton =
        project
            .resource(model_resource_id)
            .and_then(|resource| match &resource.data {
                ResourceData::Model(model) => model.skeleton,
                _ => None,
            });
    let animation_set = character.animation_set.and_then(|id| {
        let resource = project.resource(id)?;
        match &resource.data {
            ResourceData::AnimationSet(set) => Some((id, resource.name.as_str(), set)),
            _ => None,
        }
    });
    if let Some((_, set_name, set)) = animation_set {
        if set.skeleton.is_some() && model_skeleton.is_some() && set.skeleton != model_skeleton {
            report.error(format!(
                "Character '{}' clip role map '{}' targets a different skeleton than its model",
                character_name, set_name
            ));
            return None;
        }
    }

    let resolve_action = |action: CharacterAnimationAction,
                          required: bool,
                          project: &ProjectDocument,
                          report: &mut PlaytestValidationReport|
     -> Option<(
        u16,
        u8,
        u16,
        psx_level::CharacterActionFrameRange,
        psx_level::CharacterActionPush,
    )> {
        let action_label = action.label().to_ascii_lowercase();
        if let Some(binding) = action_overrides
            .iter()
            .find(|binding| binding.action == action)
        {
            let idx = binding.clip;
            return match remap_runtime_model_clip(model_clip_remaps, model_resource_id, idx) {
                Some(local) => Some((
                    local,
                    character_action_flags_for(action, binding.options),
                    character_action_speed_for(binding.options),
                    character_action_frame_range_for(binding.options),
                    character_action_push_for(binding.options),
                )),
                None => {
                    report.error(format!(
                        "Animator on '{}' maps {action_label} to clip {idx}, but that clip was not packaged for runtime",
                        spawn_node.name
                    ));
                    None
                }
            };
        }

        if let Some((_, set_name, set)) = animation_set {
            if let Some(animation_id) = animation_set_action_clip(project, set, action) {
                let options = set
                    .action_binding(action)
                    .filter(|binding| binding.clip == animation_id)
                    .and_then(|binding| binding.options);
                match project.resolved_model_animation_index(model_resource_id, animation_id) {
                    Some(index) => {
                        if let Some(local) =
                            remap_runtime_model_clip(model_clip_remaps, model_resource_id, index)
                        {
                            return Some((
                                local,
                                character_action_flags_for(action, options),
                                character_action_speed_for(options),
                                character_action_frame_range_for(options),
                                character_action_push_for(options),
                            ));
                        }
                        report.error(format!(
                            "Character '{}' {action_label} clip resolves to {index}, but that clip was not packaged for runtime",
                            character_name
                        ));
                        return None;
                    }
                    None => {
                        report.error(format!(
                            "Character '{}' action map '{}' {action_label} clip is not compatible with model '{}'",
                            character_name, set_name, model.name
                        ));
                        return None;
                    }
                }
            }
        }

        // No per-character inline binding: animations resolve only
        // through the AnimationSet (above). A required action with no
        // set entry is an error; an optional one falls back to "none".
        if required {
            report.error(format!(
                "Character '{}' has no {action_label} clip -- assign one in its Animation Set",
                character_name
            ));
            None
        } else {
            Some((
                CHARACTER_CLIP_NONE,
                character_action_flags_for(action, None),
                character_action_speed_for(None),
                character_action_frame_range_for(None),
                character_action_push_for(None),
            ))
        }
    };

    let mut action_clips = [CHARACTER_CLIP_NONE; PLAYTEST_CHARACTER_ACTION_COUNT];
    let mut action_flags = [0u8; PLAYTEST_CHARACTER_ACTION_COUNT];
    let mut action_speeds =
        [psx_level::CHARACTER_ACTION_SPEED_UNSCALED_Q8; PLAYTEST_CHARACTER_ACTION_COUNT];
    let mut action_frame_ranges =
        [psx_level::CharacterActionFrameRange::FULL; PLAYTEST_CHARACTER_ACTION_COUNT];
    let mut action_pushes = [psx_level::CharacterActionPush::NONE; PLAYTEST_CHARACTER_ACTION_COUNT];
    for action in CharacterAnimationAction::ALL {
        let (clip, flags, speed, frame_range, push) =
            resolve_action(action, action.required_for_player(), project, report)?;
        action_clips[action.to_index()] = clip;
        action_flags[action.to_index()] = flags;
        action_speeds[action.to_index()] = speed;
        action_frame_ranges[action.to_index()] = frame_range;
        action_pushes[action.to_index()] = push;
    }

    if settings.radius == 0 {
        report.error(format!("Character '{character_name}' radius must be > 0"));
        return None;
    }
    if settings.height == 0 {
        report.error(format!("Character '{character_name}' height must be > 0"));
        return None;
    }
    if settings.walk_speed <= 0 || settings.run_speed <= 0 {
        report.error(format!(
            "Character Controller for '{}' walk/run speeds must be > 0",
            character_name
        ));
        return None;
    }
    if settings.turn_speed_degrees_per_second == 0 {
        report.error(format!(
            "Character Controller for '{}' turn_speed must be > 0",
            character_name
        ));
        return None;
    }
    if settings.stamina_max_q12 <= 0 {
        report.error(format!(
            "Character Controller for '{}' stamina_max must be > 0",
            character_name
        ));
        return None;
    }
    if settings.sprint_min_q12 < 0
        || settings.sprint_drain_q12 < 0
        || settings.stamina_recover_q12 < 0
        || settings.roll_cost_q12 < 0
        || settings.backstep_cost_q12 < 0
    {
        report.error(format!(
            "Character Controller for '{}' stamina costs and recovery must be >= 0",
            character_name
        ));
        return None;
    }
    if settings.roll_speed <= 0 || settings.backstep_speed <= 0 {
        report.error(format!(
            "Character Controller for '{}' evade speeds must be > 0",
            character_name
        ));
        return None;
    }
    if settings.roll_active_frames == 0 || settings.backstep_active_frames == 0 {
        report.error(format!(
            "Character Controller for '{}' evade active frames must be > 0",
            character_name
        ));
        return None;
    }
    if camera.distance <= 0 {
        report.error(format!(
            "Camera for '{character_name}' distance must be > 0"
        ));
        return None;
    }
    if camera.height < 0 || camera.target_height < 0 {
        report.error(format!(
            "Camera for '{character_name}' offsets must be >= 0"
        ));
        return None;
    }

    if action_clips[CharacterAnimationAction::Run.to_index()] == CHARACTER_CLIP_NONE {
        report.warn(format!(
            "Character '{character_name}' has no run clip -- runtime will fall back to walk for run input",
        ));
    }
    if action_clips[CharacterAnimationAction::Turn.to_index()] == CHARACTER_CLIP_NONE {
        report.warn(format!("Character '{character_name}' has no turn clip"));
    }
    if action_clips[CharacterAnimationAction::Roll.to_index()] == CHARACTER_CLIP_NONE {
        report.warn(format!(
            "Character '{character_name}' has no roll clip -- runtime will fall back to run/walk",
        ));
    }
    if action_clips[CharacterAnimationAction::Backstep.to_index()] == CHARACTER_CLIP_NONE {
        report.warn(format!(
            "Character '{character_name}' has no backstep clip -- runtime will fall back to walk",
        ));
    }

    let character_index = u16::try_from(characters.len()).unwrap_or(u16::MAX);
    characters.push(PlaytestCharacter {
        source_resource: character_id.unwrap_or(model_resource_id),
        model: model_index,
        action_clips,
        action_flags,
        action_speeds,
        action_frame_ranges,
        action_pushes,
        visual_offset,
        visual_yaw,
        visual_scale_q8,
        weight_q8,
        radius: settings.radius,
        height: settings.height,
        walk_speed: settings.walk_speed,
        run_speed: settings.run_speed,
        turn_speed_degrees_per_second: settings.turn_speed_degrees_per_second,
        stamina_max_q12: settings.stamina_max_q12,
        sprint_min_q12: settings.sprint_min_q12,
        sprint_drain_q12: settings.sprint_drain_q12,
        stamina_recover_q12: settings.stamina_recover_q12,
        roll_cost_q12: settings.roll_cost_q12,
        roll_speed: settings.roll_speed,
        roll_active_frames: settings.roll_active_frames,
        roll_recovery_frames: settings.roll_recovery_frames,
        roll_invulnerable_frames: settings.roll_invulnerable_frames,
        backstep_cost_q12: settings.backstep_cost_q12,
        backstep_speed: settings.backstep_speed,
        backstep_active_frames: settings.backstep_active_frames,
        backstep_recovery_frames: settings.backstep_recovery_frames,
        backstep_invulnerable_frames: settings.backstep_invulnerable_frames,
        camera_distance: camera.distance,
        camera_height: camera.height,
        camera_target_height: camera.target_height,
    });
    Some(character_index)
}

/// Register a `ResourceData::Model` into the playtest package
/// on first sight; reuse the cached index otherwise. On
/// success, returns the model's index in `models`.
///
/// Failures (missing files, invalid blobs, joint-count
/// mismatches) push to `report.errors` and return `None`; the
/// caller turns that into a hard cook failure.
#[allow(clippy::too_many_arguments)]
pub(crate) fn register_model_for_instance(
    project: &ProjectDocument,
    project_root: &Path,
    model_resource_id: ResourceId,
    assets: &mut Vec<PlaytestAsset>,
    models: &mut Vec<PlaytestModel>,
    model_clips: &mut Vec<PlaytestModelClip>,
    model_clip_bounds: &mut Vec<PlaytestModelClipBounds>,
    model_frame_bounds: &mut Vec<PlaytestModelFrameBounds>,
    model_sockets: &mut Vec<PlaytestModelSocket>,
    model_for_resource: &mut std::collections::HashMap<ResourceId, u16>,
    runtime_model_clips: &HashMap<ResourceId, BTreeSet<u16>>,
    model_clip_remaps: &mut HashMap<ResourceId, Vec<Option<u16>>>,
    report: &mut PlaytestValidationReport,
) -> Option<u16> {
    if let Some(&existing) = model_for_resource.get(&model_resource_id) {
        return Some(existing);
    }
    let resource = project.resource(model_resource_id)?;
    let ResourceData::Model(model) = &resource.data else {
        report.error(format!(
            "MeshInstance references resource #{} which is not a Model",
            model_resource_id.raw()
        ));
        return None;
    };

    // Runtime contract: a placed model must carry an atlas
    // (the runtime renders textured) and at least one clip
    // (the runtime renders animated). Bind-pose / untextured
    // rendering would need engine-side work the current pass
    // doesn't ship -- fail loud at cook so the editor surfaces
    // it rather than silently dropping the instance at runtime.
    if model.texture_path.is_none() {
        report.error(format!(
            "Model '{}' has no atlas; the runtime can't render untextured models in this pass",
            resource.name
        ));
        return None;
    }
    let resolved_clips = project.resolved_model_animation_clips(model_resource_id);
    if resolved_clips.is_empty() {
        report.error(format!(
            "Model '{}' has no animation clips; the runtime requires at least one clip",
            resource.name
        ));
        return None;
    }
    if model.collision_radius == 0 {
        report.error(format!(
            "Model '{}' has zero collision radius; actor blockers must be at least 1 engine unit",
            resource.name
        ));
        return None;
    }

    let model_index = u16::try_from(models.len()).unwrap_or(u16::MAX);
    let safe = sanitise_model_dirname(&resource.name);
    let folder = format!("{MODELS_DIRNAME}/model_{:03}_{safe}", model_index);

    // Mesh asset.
    let mesh_path = resolve_path(&model.model_path, project_root);
    let mesh_bytes = match std::fs::read(&mesh_path) {
        Ok(b) => b,
        Err(e) => {
            report.error(format!(
                "Model '{}' mesh {}: {e}",
                resource.name,
                mesh_path.display()
            ));
            return None;
        }
    };
    let parsed_model = match psx_asset::Model::from_bytes(&mesh_bytes) {
        Ok(m) => m,
        Err(e) => {
            report.error(format!(
                "Model '{}' mesh parse failed: {e:?}",
                resource.name
            ));
            return None;
        }
    };
    let model_joint_count = parsed_model.joint_count();
    let mesh_asset_index = assets.len();
    assets.push(PlaytestAsset {
        kind: PlaytestAssetKind::ModelMesh,
        bytes: mesh_bytes.clone(),
        filename: format!("{folder}/mesh.psxmdl"),
        source_label: resource.name.clone(),
        streamed_class: StreamedClass::None,
    });

    // Atlas asset (optional).
    let texture_asset_index = if let Some(tex_path) = &model.texture_path {
        let abs = resolve_path(tex_path, project_root);
        let bytes = match std::fs::read(&abs) {
            Ok(b) => b,
            Err(e) => {
                report.error(format!(
                    "Model '{}' atlas {}: {e}",
                    resource.name,
                    abs.display()
                ));
                return None;
            }
        };
        let parsed_atlas = match psx_asset::Texture::from_bytes(&bytes) {
            Ok(t) => t,
            Err(e) => {
                report.error(format!(
                    "Model '{}' atlas parse failed: {e:?}",
                    resource.name
                ));
                return None;
            }
        };
        // Model atlases must be 8bpp (256-entry CLUT) -- the
        // runtime model atlas region uses an 8bpp tpage and a
        // 256-entry CLUT row per atlas. Other depths render with
        // wrong colours, so reject loud at cook time.
        if parsed_atlas.clut_entries() != 256 {
            report.error(format!(
                "Model '{}' atlas must be 8bpp (256-entry CLUT); found {} entries",
                resource.name,
                parsed_atlas.clut_entries(),
            ));
            return None;
        }
        let idx = assets.len();
        assets.push(PlaytestAsset {
            kind: PlaytestAssetKind::Texture,
            bytes,
            filename: format!("{folder}/atlas.psxt"),
            source_label: format!("{} atlas", resource.name),
            streamed_class: StreamedClass::None,
        });
        Some(idx)
    } else {
        None
    };

    // A Model carries no clips of its own, so the runtime default is
    // simply the first resolved (skeleton-scoped) clip.
    let authored_default_clip = 0u16;
    let selected_clip_indices = runtime_model_clip_indices(
        resolved_clips.len(),
        runtime_model_clips.get(&model_resource_id),
        authored_default_clip,
    );

    // Clip assets -- one .psxanim per runtime-needed clip. The
    // editor may keep a much larger animation library on the model;
    // the PS1 package only needs defaults, placed overrides, and
    // character role clips.
    let clip_first = u16::try_from(model_clips.len()).unwrap_or(u16::MAX);
    let mut clip_remap = vec![None; resolved_clips.len()];
    for (local_i, resolved_i) in selected_clip_indices.iter().copied().enumerate() {
        let Some(clip) = resolved_clips.get(resolved_i as usize) else {
            continue;
        };
        let abs = resolve_path(&clip.psxanim_path, project_root);
        let bytes = match std::fs::read(&abs) {
            Ok(b) => b,
            Err(e) => {
                report.error(format!(
                    "Model '{}' clip '{}' {}: {e}",
                    resource.name,
                    clip.name,
                    abs.display()
                ));
                return None;
            }
        };
        let parsed_anim = match psx_asset::Animation::from_bytes(&bytes) {
            Ok(a) => a,
            Err(e) => {
                report.error(format!(
                    "Model '{}' clip '{}' parse failed: {e:?}",
                    resource.name, clip.name
                ));
                return None;
            }
        };
        if parsed_anim.joint_count() != model_joint_count {
            report.error(format!(
                "Model '{}' clip '{}': animation has {} joints, model has {}",
                resource.name,
                clip.name,
                parsed_anim.joint_count(),
                model_joint_count
            ));
            return None;
        }
        let frame_first = match u16::try_from(model_frame_bounds.len()) {
            Ok(index) => index,
            Err(_) => {
                report.error(format!(
                    "Model '{}' clip '{}': too many baked model-bound frames",
                    resource.name, clip.name
                ));
                return None;
            }
        };
        let clip_index = match u16::try_from(model_clips.len()) {
            Ok(index) => index,
            Err(_) => {
                report.error(format!(
                    "Model '{}' has too many animation clips for the playtest manifest",
                    resource.name
                ));
                return None;
            }
        };
        let baked_bounds = bake_model_clip_frame_bounds(&parsed_model, &parsed_anim);
        let animation_bytes = compact_animation_bytes(&parsed_anim);
        let frame_count = match u16::try_from(baked_bounds.len()) {
            Ok(count) => count,
            Err(_) => {
                report.error(format!(
                    "Model '{}' clip '{}': too many baked model-bound frames",
                    resource.name, clip.name
                ));
                return None;
            }
        };
        let floor_y = baked_bounds
            .first()
            .map(|bounds| bounds.floor_y)
            .unwrap_or(0);
        model_frame_bounds.extend(baked_bounds);
        model_clip_bounds.push(PlaytestModelClipBounds {
            model: model_index,
            clip: clip_index,
            first_frame: frame_first,
            frame_count,
            floor_y,
            pose_offset: clip.calibration.offset,
            flags: if clip.calibration.in_place {
                model_clip_flags::IN_PLACE
            } else {
                0
            },
        });
        let asset_index = assets.len();
        let safe_clip = sanitise_model_dirname(&clip.name);
        assets.push(PlaytestAsset {
            kind: PlaytestAssetKind::ModelAnimation,
            bytes: animation_bytes,
            filename: format!("{folder}/clip_{:02}_{safe_clip}.psxanim", local_i),
            source_label: format!("{} / {}", resource.name, clip.name),
            streamed_class: StreamedClass::None,
        });
        clip_remap[resolved_i as usize] = u16::try_from(local_i).ok();
        model_clips.push(PlaytestModelClip {
            model: model_index,
            name: clip.name.clone(),
            animation_asset_index: asset_index,
        });
    }
    let clip_count = u16::try_from(model_clips.len() - clip_first as usize).unwrap_or(u16::MAX);

    // Resolve the model's default clip. Validation rules:
    //   - explicit `model.default_clip = Some(idx)` MUST be in
    //     range; out-of-range is a hard cook error so the user
    //     fixes the resource rather than a runtime instance
    //     silently pointing at clip 0.
    //   - `None` falls back to clip 0. Cooker has already
    //     refused empty-clip placed models, so `clip_count >= 1`.
    let Some(default_clip) = clip_remap
        .get(authored_default_clip as usize)
        .copied()
        .flatten()
    else {
        report.error(format!(
            "Model '{}' default_clip {authored_default_clip} was not packaged for runtime",
            resource.name
        ));
        return None;
    };

    let socket_first = u16::try_from(model_sockets.len()).unwrap_or(u16::MAX);
    let mut seen_sockets: Vec<&str> = Vec::new();
    for socket in &model.attachments {
        if socket.name.trim().is_empty() {
            report.error(format!(
                "Model '{}' has an attachment socket with no name",
                resource.name
            ));
            return None;
        }
        if socket.joint >= model_joint_count {
            report.error(format!(
                "Model '{}' socket '{}' references joint {}, but the model has {} joints",
                resource.name, socket.name, socket.joint, model_joint_count
            ));
            return None;
        }
        if seen_sockets.contains(&socket.name.as_str()) {
            report.error(format!(
                "Model '{}' has duplicate attachment socket '{}'",
                resource.name, socket.name
            ));
            return None;
        }
        seen_sockets.push(socket.name.as_str());
        model_sockets.push(PlaytestModelSocket {
            model: model_index,
            name: socket.name.clone(),
            joint: socket.joint,
            translation: socket.translation,
            rotation_q12: socket.rotation_q12,
        });
    }
    let socket_count =
        u16::try_from(model_sockets.len() - socket_first as usize).unwrap_or(u16::MAX);

    models.push(PlaytestModel {
        name: resource.name.clone(),
        source_resource: model_resource_id,
        mesh_asset_index,
        texture_asset_index,
        clip_first,
        clip_count,
        default_clip,
        socket_first,
        socket_count,
        world_height: model.world_height,
        collision_radius: model.collision_radius,
    });
    model_for_resource.insert(model_resource_id, model_index);
    model_clip_remaps.insert(model_resource_id, clip_remap);
    Some(model_index)
}

pub(crate) const MODEL_FRAME_BOUNDS_PAD_UNITS: i32 = 64;

pub(crate) fn compact_animation_bytes(animation: &psx_asset::Animation<'_>) -> Vec<u8> {
    let joint_count = animation.joint_count();
    let frame_count = animation.frame_count();
    let sample_rate_hz = animation.sample_rate_hz();
    let mut translations_max_abs = 0i32;
    let mut frame = 0u16;
    while frame < frame_count {
        let mut joint = 0u16;
        while joint < joint_count {
            if let Some(pose) = animation.pose(frame, joint) {
                translations_max_abs =
                    translations_max_abs.max(abs_i32_saturating(pose.translation.x));
                translations_max_abs =
                    translations_max_abs.max(abs_i32_saturating(pose.translation.y));
                translations_max_abs =
                    translations_max_abs.max(abs_i32_saturating(pose.translation.z));
            }
            joint += 1;
        }
        frame += 1;
    }

    let mut translation_shift = 0u16;
    while translations_max_abs > i16::MAX as i32 && translation_shift < 15 {
        translations_max_abs = (translations_max_abs + 1) >> 1;
        translation_shift += 1;
    }

    let pose_count = frame_count as usize * joint_count as usize;
    let payload_len = psxed_format::animation::AnimationHeader::SIZE
        + pose_count * psxed_format::animation::POSE_RECORD_SIZE;
    let mut out = Vec::with_capacity(psxed_format::AssetHeader::SIZE + payload_len);
    out.extend_from_slice(&psxed_format::animation::MAGIC);
    out.extend_from_slice(&psxed_format::animation::VERSION.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(payload_len as u32).to_le_bytes());
    out.extend_from_slice(&joint_count.to_le_bytes());
    out.extend_from_slice(&frame_count.to_le_bytes());
    out.extend_from_slice(&sample_rate_hz.to_le_bytes());
    out.extend_from_slice(&translation_shift.to_le_bytes());

    let shift = translation_shift as u8;
    let mut frame = 0u16;
    while frame < frame_count {
        let mut joint = 0u16;
        while joint < joint_count {
            let pose = animation
                .pose(frame, joint)
                .expect("validated animation frame/joint indices");
            for column in pose.matrix {
                for value in column {
                    out.extend_from_slice(&value.to_le_bytes());
                }
            }
            for value in [pose.translation.x, pose.translation.y, pose.translation.z] {
                out.extend_from_slice(&quantize_animation_translation(value, shift).to_le_bytes());
            }
            joint += 1;
        }
        frame += 1;
    }

    out
}

pub(crate) fn quantize_animation_translation(value: i32, shift: u8) -> i16 {
    round_shift_i32(value, shift).clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

pub(crate) fn round_shift_i32(value: i32, shift: u8) -> i32 {
    if shift == 0 {
        return value;
    }
    let value = value as i64;
    let half = 1i64 << (shift - 1);
    if value >= 0 {
        ((value + half) >> shift) as i32
    } else {
        -(((-value + half) >> shift) as i32)
    }
}

pub(crate) fn abs_i32_saturating(value: i32) -> i32 {
    if value == i32::MIN {
        i32::MAX
    } else if value < 0 {
        -value
    } else {
        value
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ModelBoundsJointTransform {
    pub(crate) matrix: [[i16; 3]; 3],
    pub(crate) translation: [i32; 3],
}

pub(crate) fn bake_model_clip_frame_bounds(
    model: &psx_asset::Model<'_>,
    animation: &psx_asset::Animation<'_>,
) -> Vec<PlaytestModelFrameBounds> {
    let frame_count = animation.frame_count();
    let cycle_frames = frame_count.saturating_sub(1).max(1);
    let mut out = Vec::with_capacity(cycle_frames as usize);
    let mut frame = 0u16;
    while frame < cycle_frames {
        let next = if cycle_frames <= 1 || frame + 1 >= cycle_frames {
            0
        } else {
            frame + 1
        };
        out.push(bake_model_frame_pair_bounds(model, animation, frame, next));
        frame += 1;
    }
    out
}

pub(crate) fn bake_model_frame_pair_bounds(
    model: &psx_asset::Model<'_>,
    animation: &psx_asset::Animation<'_>,
    a: u16,
    b: u16,
) -> PlaytestModelFrameBounds {
    let mut min = [i32::MAX; 3];
    let mut max = [i32::MIN; 3];
    let mut floor_y = i32::MIN;
    accumulate_model_frame_bounds(model, animation, a, &mut min, &mut max, &mut floor_y);
    if b != a {
        accumulate_model_frame_bounds(model, animation, b, &mut min, &mut max, &mut floor_y);
    }

    if min[0] == i32::MAX {
        return PlaytestModelFrameBounds {
            center: [0, 0, 0],
            radius: MODEL_FRAME_BOUNDS_PAD_UNITS,
            floor_y: 0,
        };
    }

    let center = [
        average_i32(min[0], max[0]),
        average_i32(min[1], max[1]),
        average_i32(min[2], max[2]),
    ];
    let radius = aabb_radius(min, max).saturating_add(MODEL_FRAME_BOUNDS_PAD_UNITS);
    PlaytestModelFrameBounds {
        center,
        radius,
        floor_y,
    }
}

pub(crate) fn accumulate_model_frame_bounds(
    model: &psx_asset::Model<'_>,
    animation: &psx_asset::Animation<'_>,
    frame: u16,
    min: &mut [i32; 3],
    max: &mut [i32; 3],
    floor_y: &mut i32,
) {
    let joint_count = model.joint_count().min(animation.joint_count());
    let mut joints = Vec::with_capacity(joint_count as usize);
    let mut raw_joints = Vec::with_capacity(joint_count as usize);
    let mut joint = 0u16;
    while joint < joint_count {
        if let Some(pose) = animation.pose(frame, joint) {
            joints.push(model_bounds_joint_transform(
                pose,
                model.local_to_world_q12(),
            ));
            raw_joints.push(model_bounds_joint_transform(pose, 0x1000));
        }
        joint += 1;
    }

    let mut part_index = 0u16;
    while part_index < model.part_count() {
        let Some(part) = model.part(part_index) else {
            part_index += 1;
            continue;
        };
        let primary_joint = part.joint_index() as usize;
        let Some(primary) = joints.get(primary_joint).copied() else {
            part_index += 1;
            continue;
        };
        let Some(raw_primary) = raw_joints.get(primary_joint).copied() else {
            part_index += 1;
            continue;
        };
        let first = part.first_vertex();
        let end = first
            .saturating_add(part.vertex_count())
            .min(model.vertex_count());
        let mut vertex_index = first;
        while vertex_index < end {
            if let Some(vertex) = model.vertex(vertex_index) {
                let mut point = transform_model_bounds_vertex(primary, vertex);
                let mut raw_point = transform_model_bounds_vertex(raw_primary, vertex);
                if vertex.is_blend() {
                    if let Some(secondary) = joints.get(vertex.joint1 as usize).copied() {
                        let secondary_point = transform_model_bounds_vertex(secondary, vertex);
                        point = lerp_bounds_point(point, secondary_point, vertex.blend);
                    }
                    if let Some(raw_secondary) = raw_joints.get(vertex.joint1 as usize).copied() {
                        let raw_secondary_point =
                            transform_model_bounds_vertex(raw_secondary, vertex);
                        raw_point = lerp_bounds_point(raw_point, raw_secondary_point, vertex.blend);
                    }
                }
                include_bounds_point(point, min, max);
                *floor_y = (*floor_y).max(raw_point[1]);
            }
            vertex_index += 1;
        }
        part_index += 1;
    }
}

pub(crate) fn model_bounds_joint_transform(
    pose: psx_asset::JointPose,
    local_to_world_q12: u16,
) -> ModelBoundsJointTransform {
    let mut matrix = [[0i16; 3]; 3];
    let mut row = 0usize;
    while row < 3 {
        let mut col = 0usize;
        while col < 3 {
            matrix[row][col] =
                clamp_i16_i64(((pose.matrix[col][row] as i64) * (local_to_world_q12 as i64)) >> 12);
            col += 1;
        }
        row += 1;
    }
    ModelBoundsJointTransform {
        matrix,
        translation: [
            apply_q12_i32(pose.translation.x, local_to_world_q12),
            apply_q12_i32(pose.translation.y, local_to_world_q12),
            apply_q12_i32(pose.translation.z, local_to_world_q12),
        ],
    }
}

pub(crate) fn transform_model_bounds_vertex(
    transform: ModelBoundsJointTransform,
    vertex: psx_asset::ModelVertex,
) -> [i32; 3] {
    let vx = vertex.position.x as i64;
    let vy = vertex.position.y as i64;
    let vz = vertex.position.z as i64;
    let row = |row: [i16; 3], translation: i32| -> i32 {
        let value = ((row[0] as i64) * vx + (row[1] as i64) * vy + (row[2] as i64) * vz) >> 12;
        clamp_i32_i64(value.saturating_add(translation as i64))
    };
    [
        row(transform.matrix[0], transform.translation[0]),
        row(transform.matrix[1], transform.translation[1]),
        row(transform.matrix[2], transform.translation[2]),
    ]
}

pub(crate) fn lerp_bounds_point(a: [i32; 3], b: [i32; 3], t: u8) -> [i32; 3] {
    let t = t as i64;
    let inv = 256 - t;
    [
        clamp_i32_i64(((a[0] as i64) * inv + (b[0] as i64) * t) >> 8),
        clamp_i32_i64(((a[1] as i64) * inv + (b[1] as i64) * t) >> 8),
        clamp_i32_i64(((a[2] as i64) * inv + (b[2] as i64) * t) >> 8),
    ]
}

pub(crate) fn include_bounds_point(point: [i32; 3], min: &mut [i32; 3], max: &mut [i32; 3]) {
    let mut axis = 0usize;
    while axis < 3 {
        min[axis] = min[axis].min(point[axis]);
        max[axis] = max[axis].max(point[axis]);
        axis += 1;
    }
}

pub(crate) fn average_i32(a: i32, b: i32) -> i32 {
    (((a as i64) + (b as i64)) / 2).clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

pub(crate) fn aabb_radius(min: [i32; 3], max: [i32; 3]) -> i32 {
    let half_x = half_extent_u128(min[0], max[0]);
    let half_y = half_extent_u128(min[1], max[1]);
    let half_z = half_extent_u128(min[2], max[2]);
    let square = half_x
        .saturating_mul(half_x)
        .saturating_add(half_y.saturating_mul(half_y))
        .saturating_add(half_z.saturating_mul(half_z));
    ceil_sqrt_u128(square).min(i32::MAX as u128) as i32
}

pub(crate) fn half_extent_u128(min: i32, max: i32) -> u128 {
    let extent = (max as i64).saturating_sub(min as i64).unsigned_abs() as u128;
    extent.div_ceil(2)
}

pub(crate) fn ceil_sqrt_u128(value: u128) -> u128 {
    if value <= 1 {
        return value;
    }
    let mut hi = 1u128;
    while hi.saturating_mul(hi) < value {
        hi = hi.saturating_mul(2);
    }
    let mut lo = hi / 2;
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if mid.saturating_mul(mid) >= value {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    hi
}

pub(crate) fn apply_q12_i32(value: i32, q12: u16) -> i32 {
    clamp_i32_i64(((value as i64) * (q12 as i64)) >> 12)
}

pub(crate) fn clamp_i16_i64(value: i64) -> i16 {
    value.clamp(i16::MIN as i64, i16::MAX as i64) as i16
}

pub(crate) fn clamp_i32_i64(value: i64) -> i32 {
    value.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

pub(crate) fn push_model_instance_for_resource(
    project: &ProjectDocument,
    project_root: &Path,
    node_name: &str,
    model_resource_id: ResourceId,
    clip_override: Option<u16>,
    pose_frame: u16,
    room_index: u16,
    pos: [i32; 3],
    yaw: i16,
    visual_yaw: i16,
    pitch: i16,
    roll: i16,
    visual_offset: [i16; 3],
    visual_scale_q8: u16,
    assets: &mut Vec<PlaytestAsset>,
    models: &mut Vec<PlaytestModel>,
    model_clips: &mut Vec<PlaytestModelClip>,
    model_clip_bounds: &mut Vec<PlaytestModelClipBounds>,
    model_frame_bounds: &mut Vec<PlaytestModelFrameBounds>,
    model_sockets: &mut Vec<PlaytestModelSocket>,
    model_instances: &mut Vec<PlaytestModelInstance>,
    model_for_resource: &mut HashMap<ResourceId, u16>,
    runtime_model_clips: &HashMap<ResourceId, BTreeSet<u16>>,
    model_clip_remaps: &mut HashMap<ResourceId, Vec<Option<u16>>>,
    report: &mut PlaytestValidationReport,
) -> bool {
    let Some(model_index) = register_model_for_instance(
        project,
        project_root,
        model_resource_id,
        assets,
        models,
        model_clips,
        model_clip_bounds,
        model_frame_bounds,
        model_sockets,
        model_for_resource,
        runtime_model_clips,
        model_clip_remaps,
        report,
    ) else {
        return false;
    };
    let clip = match clip_override {
        Some(idx) => {
            let authored_clip_count = project
                .resolved_model_animation_clips(model_resource_id)
                .len();
            if idx as usize >= authored_clip_count {
                report.error(format!(
                    "Model instance '{node_name}' clip override {idx} out of range (model has {authored_clip_count})"
                ));
                return false;
            }
            let Some(local) = remap_runtime_model_clip(model_clip_remaps, model_resource_id, idx)
            else {
                report.error(format!(
                    "Model instance '{node_name}' clip override {idx} was not packaged for runtime"
                ));
                return false;
            };
            local
        }
        None => MODEL_CLIP_INHERIT,
    };
    model_instances.push(PlaytestModelInstance {
        room: room_index,
        model: model_index,
        clip,
        pose_frame,
        x: pos[0],
        y: pos[1],
        z: pos[2],
        yaw,
        visual_yaw,
        pitch,
        roll,
        visual_offset,
        visual_scale_q8,
        flags: 0,
    });
    true
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn push_character_controller_idle_instance(
    project: &ProjectDocument,
    project_root: &Path,
    node_name: &str,
    character_id: ResourceId,
    room_index: u16,
    pos: [i32; 3],
    yaw: i16,
    assets: &mut Vec<PlaytestAsset>,
    models: &mut Vec<PlaytestModel>,
    model_clips: &mut Vec<PlaytestModelClip>,
    model_clip_bounds: &mut Vec<PlaytestModelClipBounds>,
    model_frame_bounds: &mut Vec<PlaytestModelFrameBounds>,
    model_sockets: &mut Vec<PlaytestModelSocket>,
    model_instances: &mut Vec<PlaytestModelInstance>,
    model_for_resource: &mut HashMap<ResourceId, u16>,
    runtime_model_clips: &HashMap<ResourceId, BTreeSet<u16>>,
    model_clip_remaps: &mut HashMap<ResourceId, Vec<Option<u16>>>,
    report: &mut PlaytestValidationReport,
) -> bool {
    let Some(resource) = project.resource(character_id) else {
        report.error(format!(
            "Non-player Character Controller '{node_name}' references Character #{} which doesn't exist",
            character_id.raw()
        ));
        return false;
    };
    let ResourceData::Character(character) = &resource.data else {
        report.error(format!(
            "Non-player Character Controller '{node_name}' references resource '{}' which is not a Character",
            resource.name
        ));
        return false;
    };
    let Some(model_resource_id) = character.model else {
        report.error(format!(
            "Character '{}' has no Model assigned - required for non-player Entity '{}'",
            resource.name, node_name
        ));
        return false;
    };
    let Some(model_index) = register_model_for_instance(
        project,
        project_root,
        model_resource_id,
        assets,
        models,
        model_clips,
        model_clip_bounds,
        model_frame_bounds,
        model_sockets,
        model_for_resource,
        runtime_model_clips,
        model_clip_remaps,
        report,
    ) else {
        return false;
    };
    let Some(clip) = character_idle_clip_for_model_instance(
        project,
        resource.name.as_str(),
        character,
        model_resource_id,
        &models[model_index as usize],
        model_clip_remaps,
        report,
    ) else {
        return false;
    };
    model_instances.push(PlaytestModelInstance {
        room: room_index,
        model: model_index,
        clip,
        pose_frame: MODEL_INSTANCE_POSE_ANIMATE,
        x: pos[0],
        y: pos[1],
        z: pos[2],
        yaw,
        visual_yaw: 0,
        pitch: 0,
        roll: 0,
        visual_offset: [0; 3],
        visual_scale_q8: crate::MODEL_SCALE_ONE_Q8,
        flags: 0,
    });
    true
}

pub(crate) fn character_idle_clip_for_model_instance(
    project: &ProjectDocument,
    character_name: &str,
    character: &crate::CharacterResource,
    model_resource_id: ResourceId,
    model: &PlaytestModel,
    model_clip_remaps: &HashMap<ResourceId, Vec<Option<u16>>>,
    report: &mut PlaytestValidationReport,
) -> Option<u16> {
    let model_skeleton =
        project
            .resource(model_resource_id)
            .and_then(|resource| match &resource.data {
                ResourceData::Model(model) => model.skeleton,
                _ => None,
            });
    let animation_set = character.animation_set.and_then(|id| {
        let resource = project.resource(id)?;
        match &resource.data {
            ResourceData::AnimationSet(set) => Some((resource.name.as_str(), set)),
            _ => None,
        }
    });
    if let Some((set_name, set)) = animation_set {
        if set.skeleton.is_some() && model_skeleton.is_some() && set.skeleton != model_skeleton {
            report.error(format!(
                "Character '{character_name}' clip role map '{set_name}' targets a different skeleton than its model"
            ));
            return None;
        }
        if let Some(animation_id) = set.role_clip(AnimationRole::Idle) {
            return match project.resolved_model_animation_index(model_resource_id, animation_id) {
                Some(index) => {
                    if let Some(local) =
                        remap_runtime_model_clip(model_clip_remaps, model_resource_id, index)
                    {
                        return Some(local);
                    }
                    report.error(format!(
                        "Character '{character_name}' idle clip resolves to {index}, but that clip was not packaged for runtime"
                    ));
                    None
                }
                None => {
                    report.error(format!(
                        "Character '{character_name}' clip role map '{set_name}' idle clip is not compatible with model '{}'",
                        model.name
                    ));
                    None
                }
            };
        }
    }

    // No AnimationSet idle: fall back to the model's runtime default clip.
    Some(model.default_clip)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn register_weapon_for_equipment(
    project: &ProjectDocument,
    project_root: &Path,
    weapon_resource_id: ResourceId,
    assets: &mut Vec<PlaytestAsset>,
    models: &mut Vec<PlaytestModel>,
    model_clips: &mut Vec<PlaytestModelClip>,
    model_clip_bounds: &mut Vec<PlaytestModelClipBounds>,
    model_frame_bounds: &mut Vec<PlaytestModelFrameBounds>,
    model_sockets: &mut Vec<PlaytestModelSocket>,
    model_for_resource: &mut HashMap<ResourceId, u16>,
    runtime_model_clips: &HashMap<ResourceId, BTreeSet<u16>>,
    model_clip_remaps: &mut HashMap<ResourceId, Vec<Option<u16>>>,
    weapon_hitboxes: &mut Vec<PlaytestWeaponHitbox>,
    weapons: &mut Vec<PlaytestWeapon>,
    weapon_for_resource: &mut HashMap<ResourceId, u16>,
    report: &mut PlaytestValidationReport,
) -> Option<u16> {
    if let Some(&existing) = weapon_for_resource.get(&weapon_resource_id) {
        return Some(existing);
    }
    let Some(resource) = project.resource(weapon_resource_id) else {
        report.error(format!(
            "Equipment references missing Weapon resource #{}",
            weapon_resource_id.raw()
        ));
        return None;
    };
    let ResourceData::Weapon(weapon) = &resource.data else {
        report.error(format!(
            "Equipment references resource '{}' which is not a Weapon",
            resource.name
        ));
        return None;
    };

    // Melee-arc contract (phase-3 combat): a cooked weapon must be
    // able to connect. Serde defaults are non-zero, so only an
    // explicitly authored zero trips these.
    let mut arc_ok = true;
    if weapon.arc_reach == 0 {
        report.error(format!(
            "Weapon '{}' has melee arc reach 0 - an equipped weapon \
             must be able to connect (set Arc Reach > 0)",
            resource.name
        ));
        arc_ok = false;
    }
    if weapon.arc_half_angle_degrees == 0 || weapon.arc_half_angle_degrees > 170 {
        report.error(format!(
            "Weapon '{}' has melee arc half-angle {} degrees - it must \
             be 1..=170 (a zero-width arc never connects; past 170 the \
             front-arc test degenerates)",
            resource.name, weapon.arc_half_angle_degrees
        ));
        arc_ok = false;
    }
    if weapon.damage == 0 {
        report.error(format!(
            "Weapon '{}' has damage 0 - an equipped weapon must \
             threaten something (set Damage > 0)",
            resource.name
        ));
        arc_ok = false;
    }
    if !arc_ok {
        return None;
    }

    let model = match weapon.model {
        Some(model_resource_id) => Some(register_model_for_instance(
            project,
            project_root,
            model_resource_id,
            assets,
            models,
            model_clips,
            model_clip_bounds,
            model_frame_bounds,
            model_sockets,
            model_for_resource,
            runtime_model_clips,
            model_clip_remaps,
            report,
        )?),
        None => None,
    };

    let hitbox_first = u16::try_from(weapon_hitboxes.len()).unwrap_or(u16::MAX);
    for hitbox in &weapon.hitboxes {
        weapon_hitboxes.push(PlaytestWeaponHitbox {
            name: hitbox.name.clone(),
            shape: playtest_weapon_shape(&hitbox.shape),
            active_start_frame: hitbox.active_start_frame,
            active_end_frame: hitbox.active_end_frame.max(hitbox.active_start_frame),
        });
    }
    let hitbox_count =
        u16::try_from(weapon_hitboxes.len() - hitbox_first as usize).unwrap_or(u16::MAX);
    let weapon_index = u16::try_from(weapons.len()).unwrap_or(u16::MAX);
    weapons.push(PlaytestWeapon {
        name: resource.name.clone(),
        source_resource: weapon_resource_id,
        model,
        default_character_socket: weapon.default_character_socket.clone(),
        grip_name: weapon.grip.name.clone(),
        grip_translation: weapon.grip.translation,
        grip_rotation_q12: weapon.grip.rotation_q12,
        hitbox_first,
        hitbox_count,
        arc_reach: weapon.arc_reach,
        arc_half_angle: weapon_arc_half_angle_psx(weapon.arc_half_angle_degrees),
        damage: weapon.damage,
        poise_damage: weapon.poise_damage,
    });
    weapon_for_resource.insert(weapon_resource_id, weapon_index);
    Some(weapon_index)
}

/// Authored degrees -> PSX angle units (4096 per full turn). Exact
/// for multiples of 45; the validation bound (1..=170) keeps the
/// result well inside u16.
pub(crate) fn weapon_arc_half_angle_psx(degrees: u16) -> u16 {
    ((u32::from(degrees) * 4096) / 360) as u16
}

pub(crate) fn playtest_weapon_shape(shape: &crate::WeaponHitShape) -> PlaytestWeaponHitShape {
    match shape {
        crate::WeaponHitShape::Box {
            center,
            half_extents,
        } => PlaytestWeaponHitShape::Box {
            center: *center,
            half_extents: *half_extents,
        },
        crate::WeaponHitShape::Capsule { start, end, radius } => PlaytestWeaponHitShape::Capsule {
            start: *start,
            end: *end,
            radius: *radius,
        },
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ModelRendererComponent {
    pub(crate) model: Option<ResourceId>,
    pub(crate) visual_offset: [i16; 3],
    pub(crate) visual_yaw: i16,
    pub(crate) visual_scale_q8: u16,
}

#[derive(Clone, Copy)]
pub(crate) struct AnimatorComponent<'a> {
    pub(crate) clip: Option<u16>,
    pub(crate) action_clips: &'a [crate::CharacterActionClip],
    pub(crate) autoplay: bool,
    pub(crate) pose_frame: u16,
}

#[derive(Clone, Copy)]
pub(crate) struct CharacterControllerComponent {
    pub(crate) character: Option<ResourceId>,
    pub(crate) settings: CharacterControllerSettings,
    pub(crate) player: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct CameraComponent {
    pub(crate) settings: WorldCameraSettings,
}

#[derive(Clone, Copy)]
pub(crate) struct PhysicsBodyComponent {
    pub(crate) settings: PhysicsBodySettings,
}

pub(crate) struct EquipmentComponent<'a> {
    pub(crate) weapon: Option<ResourceId>,
    pub(crate) character_socket: &'a str,
    pub(crate) weapon_grip: &'a str,
}

#[derive(Clone, Copy)]
pub(crate) struct InteractableComponent<'a> {
    pub(crate) kind: &'a crate::InteractableKind,
    pub(crate) prompt: &'a str,
    pub(crate) radius: u16,
    pub(crate) enabled: bool,
}

pub(crate) fn component_model_renderer(
    scene: &crate::Scene,
    host: &SceneNode,
) -> Option<ModelRendererComponent> {
    component_children(scene, host).find_map(|node| match &node.kind {
        NodeKind::ModelRenderer {
            model,
            material: _,
            visual_offset,
            visual_scale_q8,
        } => Some(ModelRendererComponent {
            model: *model,
            visual_offset: *visual_offset,
            visual_yaw: yaw_from_degrees(node.transform.rotation_degrees[1]),
            visual_scale_q8: *visual_scale_q8,
        }),
        _ => None,
    })
}

pub(crate) fn component_animator<'a>(
    scene: &'a crate::Scene,
    host: &'a SceneNode,
) -> Option<AnimatorComponent<'a>> {
    component_children(scene, host).find_map(|node| match &node.kind {
        NodeKind::Animator {
            clip,
            action_clips,
            autoplay,
            pose_frame,
        } => Some(AnimatorComponent {
            clip: *clip,
            action_clips,
            autoplay: *autoplay,
            pose_frame: *pose_frame,
        }),
        _ => None,
    })
}

pub(crate) fn component_character_controller(
    scene: &crate::Scene,
    host: &SceneNode,
) -> Option<CharacterControllerComponent> {
    component_children(scene, host).find_map(|node| match &node.kind {
        NodeKind::CharacterController {
            character,
            settings,
            player,
        } => Some(CharacterControllerComponent {
            character: *character,
            settings: *settings,
            player: *player,
        }),
        _ => None,
    })
}

pub(crate) fn component_camera(scene: &crate::Scene, host: &SceneNode) -> Option<CameraComponent> {
    component_children(scene, host).find_map(|node| match &node.kind {
        NodeKind::Camera { settings } => Some(CameraComponent {
            settings: settings.normalized(),
        }),
        _ => None,
    })
}

pub(crate) fn component_physics_body(
    scene: &crate::Scene,
    host: &SceneNode,
) -> Option<PhysicsBodyComponent> {
    component_children(scene, host).find_map(|node| match &node.kind {
        NodeKind::PhysicsBody { settings } => Some(PhysicsBodyComponent {
            settings: settings.normalized(),
        }),
        _ => None,
    })
}

pub(crate) fn physics_body_weight_q8(scene: &crate::Scene, host: &SceneNode) -> u16 {
    component_physics_body(scene, host)
        .map(|component| component.settings.weight_q8)
        .unwrap_or(PHYSICS_WEIGHT_ONE_Q8)
}

pub(crate) fn component_equipment<'a>(
    scene: &'a crate::Scene,
    host: &'a SceneNode,
) -> Option<EquipmentComponent<'a>> {
    component_children(scene, host).find_map(|node| match &node.kind {
        NodeKind::Equipment {
            weapon,
            character_socket,
            weapon_grip,
        } => Some(EquipmentComponent {
            weapon: *weapon,
            character_socket,
            weapon_grip,
        }),
        _ => None,
    })
}

pub(crate) fn component_interactable<'a>(
    scene: &'a crate::Scene,
    host: &'a SceneNode,
) -> Option<InteractableComponent<'a>> {
    component_children(scene, host).find_map(|node| match &node.kind {
        NodeKind::Interactable {
            kind,
            prompt,
            radius,
            enabled,
        } => Some(InteractableComponent {
            kind,
            prompt,
            radius: *radius,
            enabled: *enabled,
        }),
        _ => None,
    })
}

pub(crate) fn component_children<'a>(
    scene: &'a crate::Scene,
    host: &'a SceneNode,
) -> impl Iterator<Item = &'a SceneNode> + 'a {
    host.children
        .iter()
        .filter_map(|id| scene.node(*id))
        .filter(|node| node.kind.is_component())
}
