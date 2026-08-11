use super::*;

#[test]
fn starter_project_emits_player_controller_and_character() {
    let project = project_with_one_room();
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("package returned on ok report");
    assert_eq!(
        package.characters.len(),
        1,
        "starter ships exactly one player Character"
    );
    let pc = package
        .player_controller
        .expect("player controller emitted");
    assert_eq!(pc.character, 0);
    assert_eq!(pc.spawn, package.spawn.unwrap());
    let character = &package.characters[0];
    for action in [
        CharacterAnimationAction::Idle,
        CharacterAnimationAction::Walk,
        CharacterAnimationAction::Run,
        CharacterAnimationAction::Roll,
        CharacterAnimationAction::LightAttack,
        CharacterAnimationAction::HeavyAttack,
        CharacterAnimationAction::ComboAttack,
        CharacterAnimationAction::HitReact,
        CharacterAnimationAction::Death,
    ] {
        assert_ne!(
            character.action_clips[action.to_index()],
            CHARACTER_CLIP_NONE,
            "{action:?} should be mapped for the starter player"
        );
    }
    // The verified Aletha Complete set authors no Intro or Turn clip; the
    // runtime falls straight into Idle on boot.
    for action in [
        CharacterAnimationAction::Intro,
        CharacterAnimationAction::Turn,
    ] {
        assert_eq!(
            character.action_clips[action.to_index()],
            CHARACTER_CLIP_NONE,
            "{action:?} is unauthored in the verified starter set"
        );
    }
}

#[test]
fn character_combat_capsules_cook_to_bounded_contiguous_runtime_slice() {
    let mut project = project_with_one_room();
    // The assertions below read `package.characters[0]`, which cooks from
    // the wired player. The starter carries several Character resources, so
    // taking the first one lands on a preset the cook never reaches.
    let character_id = player_character_resource_id(&project);
    let character = match &mut project.resource_mut(character_id).expect("player").data {
        ResourceData::Character(character) => character,
        _ => unreachable!("player controller resolves to a Character"),
    };
    character.combat_capsules = vec![
        crate::CharacterCombatCapsule {
            name: "Torso".to_string(),
            joint: 0,
            capsule: crate::JointCapsule {
                start: [0, 0, 0],
                end: [0, 256, 0],
                radius: 96,
            },
            role: crate::CombatCapsuleRole::Hurtbox,
        },
        crate::CharacterCombatCapsule {
            name: "Attack".to_string(),
            joint: 0,
            capsule: crate::JointCapsule {
                start: [0, 0, 0],
                end: [128, 0, 0],
                radius: 48,
            },
            role: crate::CombatCapsuleRole::Hitbox {
                action: CharacterAnimationAction::LightAttack,
                active_start_frame: 4,
                active_end_frame: 8,
                damage: 30,
                poise_damage: 20,
            },
        },
    ];

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("package returned on ok report");
    assert_eq!(package.combat_capsules.len(), 2);
    assert_eq!(package.characters[0].combat_capsule_first, 0);
    assert_eq!(package.characters[0].combat_capsule_count, 2);
    assert_eq!(
        package.combat_capsules[0].flags,
        psx_level::combat_capsule_flags::HURTBOX
    );
    assert_eq!(
        package.combat_capsules[1].flags,
        psx_level::combat_capsule_flags::HITBOX
    );
    assert_eq!(package.combat_capsules[1].active_start_frame, 4);
    assert_eq!(package.combat_capsules[1].active_end_frame, 8);
}

#[test]
fn animation_set_infers_evade_roles_from_extra_clip_names() {
    let mut project = ProjectDocument::new("role inference");
    let skeleton = project.add_resource(
        "Skeleton",
        ResourceData::Skeleton(crate::SkeletonResource {
            joint_count: 1,
            parents: vec![None],
            signature: "test".to_string(),
            note: String::new(),
            joint_names: Vec::new(),
        }),
    );
    let roll = project.add_resource(
        "Meshy Gold / roll dodge",
        ResourceData::AnimationClip(crate::AnimationClipResource {
            psxanim_path: "roll.psxanim".to_string(),
            skeleton: Some(skeleton),
            target_model: None,
            source: None,
            bake: crate::AnimationClipBakeKind::ModelNative,
            role: AnimationRole::Generic,
            looping: false,
            tags: Vec::new(),
            calibration: Default::default(),
            pose_corrections: Vec::new(),
        }),
    );
    let backstep = project.add_resource(
        "Meshy Gold / step back",
        ResourceData::AnimationClip(crate::AnimationClipResource {
            psxanim_path: "backstep.psxanim".to_string(),
            skeleton: Some(skeleton),
            target_model: None,
            source: None,
            bake: crate::AnimationClipBakeKind::ModelNative,
            role: AnimationRole::Generic,
            looping: false,
            tags: Vec::new(),
            calibration: Default::default(),
            pose_corrections: Vec::new(),
        }),
    );
    let light_attack = project.add_resource(
        "Standalone FBX / sword attack",
        ResourceData::AnimationClip(crate::AnimationClipResource {
            psxanim_path: "attack.psxanim".to_string(),
            skeleton: Some(skeleton),
            target_model: None,
            source: None,
            bake: crate::AnimationClipBakeKind::ModelNative,
            role: AnimationRole::Attack,
            looping: false,
            tags: Vec::new(),
            calibration: Default::default(),
            pose_corrections: Vec::new(),
        }),
    );
    let heavy_attack = project.add_resource(
        "Custom flourish",
        ResourceData::AnimationClip(crate::AnimationClipResource {
            psxanim_path: "heavy.psxanim".to_string(),
            skeleton: Some(skeleton),
            target_model: None,
            source: None,
            bake: crate::AnimationClipBakeKind::ModelNative,
            role: AnimationRole::Generic,
            looping: false,
            tags: Vec::new(),
            calibration: Default::default(),
            pose_corrections: Vec::new(),
        }),
    );
    let mut set = crate::AnimationSetResource {
        skeleton: Some(skeleton),
        clips: vec![roll, backstep, light_attack],
        ..crate::AnimationSetResource::default()
    };
    set.set_action_clip(CharacterAnimationAction::HeavyAttack, Some(heavy_attack));

    assert_eq!(
        animation_set_action_clip(&project, &set, CharacterAnimationAction::Roll),
        Some(roll)
    );
    assert_eq!(
        animation_set_action_clip(&project, &set, CharacterAnimationAction::Backstep),
        Some(backstep)
    );
    assert_eq!(
        animation_set_action_clip(&project, &set, CharacterAnimationAction::LightAttack),
        Some(light_attack)
    );
    assert_eq!(
        animation_set_action_clip(&project, &set, CharacterAnimationAction::HeavyAttack),
        Some(heavy_attack)
    );
    assert_eq!(
        animation_set_action_clip(&project, &set, CharacterAnimationAction::ComboAttack),
        None,
        "generic attack clips must not fill every combat action"
    );
}

#[test]
fn player_character_controller_settings_drive_cooked_character() {
    let mut project = project_with_one_room();
    let controller_id = player_controller_component_id(&project);
    let scene = project.active_scene_mut();
    let controller = scene.node_mut(controller_id).unwrap();
    let NodeKind::CharacterController { settings, .. } = &mut controller.kind else {
        panic!("starter player controller must be a Character Controller");
    };
    settings.walk_speed = 61;
    settings.run_speed = 133;
    settings.turn_speed_degrees_per_second = 270;
    settings.stamina_max_q12 = 2048;
    settings.roll_speed = 144;

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let character = &package.expect("package returned on ok report").characters[0];
    assert_eq!(character.walk_speed, 61);
    assert_eq!(character.run_speed, 133);
    assert_eq!(character.turn_speed_degrees_per_second, 270);
    assert_eq!(character.stamina_max_q12, 2048);
    assert_eq!(character.roll_speed, 144);
}

#[test]
fn player_camera_component_drives_cooked_camera() {
    let mut project = project_with_one_room();
    let player = player_spawn_node_id(&project);
    let authored = WorldCameraSettings {
        distance: 2048,
        height: 900,
        target_height: 700,
        lock_rise_percent: 24,
        min_floor_clearance: 96,
        orbit_speed_level: 6,
        position_lag_shift: 1,
        focus_lag_shift: 3,
        distance_lag_shift: 5,
    };
    // The starter already parents a Camera to the player. Adding a second
    // one leaves the cook reading the original, so retarget the existing
    // node when there is one and only add when there is not.
    let existing = project
        .active_scene()
        .nodes()
        .iter()
        .find(|node| node.parent == Some(player) && matches!(node.kind, NodeKind::Camera { .. }))
        .map(|node| node.id);
    match existing {
        Some(id) => {
            let scene = project.active_scene_mut();
            let node = scene.node_mut(id).expect("existing player camera");
            node.kind = NodeKind::Camera { settings: authored };
        }
        None => {
            project.active_scene_mut().add_node(
                player,
                "Camera",
                NodeKind::Camera { settings: authored },
            );
        }
    }

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("package returned on ok report");
    assert!(package.rooms.iter().all(|room| {
        room.camera.distance == 2048
            && room.camera.height == 900
            && room.camera.target_height == 700
            && room.camera.lock_rise_percent == 24
            && room.camera.min_floor_clearance == 96
            && room.camera.orbit_speed_level == 6
            && room.camera.position_lag_shift == 1
            && room.camera.focus_lag_shift == 3
            && room.camera.distance_lag_shift == 5
    }));
    let character = &package.characters[0];
    assert_eq!(character.camera_distance, 2048);
    assert_eq!(character.camera_height, 900);
    assert_eq!(character.camera_target_height, 700);
}

#[test]
fn world_physics_and_physics_body_drive_cooked_gravity() {
    let mut project = project_with_one_room();
    let world_id = project
        .active_scene()
        .nodes()
        .iter()
        .find(|node| matches!(node.kind, NodeKind::World { .. }))
        .expect("starter has world")
        .id;
    if let Some(world) = project.active_scene_mut().node_mut(world_id) {
        let NodeKind::World { physics, .. } = &mut world.kind else {
            panic!("expected world");
        };
        physics.gravity_per_tick = 123;
    }

    let player = player_spawn_node_id(&project);
    project.active_scene_mut().add_node(
        player,
        "Physics Body",
        NodeKind::PhysicsBody {
            settings: crate::PhysicsBodySettings { weight_q8: 384 },
        },
    );

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("package returned on ok report");
    assert!(package
        .rooms
        .iter()
        .all(|room| room.gravity_per_tick == 123));
    assert_eq!(package.characters[0].weight_q8, 384);
}

#[test]
fn player_model_renderer_visual_transform_drives_cooked_character() {
    let mut project = project_with_one_room();
    let spawn_id = player_spawn_node_id(&project);
    let scene = project.active_scene_mut();
    let renderer_id = scene
        .node(spawn_id)
        .and_then(|node| {
            node.children.iter().find_map(|child| {
                scene.node(*child).and_then(|node| {
                    matches!(node.kind, NodeKind::ModelRenderer { .. }).then_some(node.id)
                })
            })
        })
        .expect("starter player has a model renderer");
    let renderer = scene.node_mut(renderer_id).unwrap();
    let NodeKind::ModelRenderer {
        visual_offset,
        visual_scale_q8,
        ..
    } = &mut renderer.kind
    else {
        panic!("expected model renderer");
    };
    *visual_offset = [32, -16, 48];
    *visual_scale_q8 = crate::MODEL_SCALE_ONE_Q8 + 64;
    renderer.transform.rotation_degrees[1] = 45.0;

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let character = &package.expect("package returned on ok report").characters[0];
    assert_eq!(character.visual_offset, [32, -16, 48]);
    assert_eq!(character.visual_yaw, 512);
    assert_eq!(character.visual_scale_q8, crate::MODEL_SCALE_ONE_Q8 + 64);
}

#[test]
fn player_character_model_is_deduplicated_with_renderer_component() {
    // Starter includes both a ModelRenderer component and a
    // Character resource on the player
    // entity. The cooker must register the model once, but
    // must not also emit a static model instance for the
    // player-controlled renderer.
    let project = project_with_one_room();
    let (package, _report) = build_package(&project, &starter_project_root());
    let package = package.expect("starter cooks");
    assert_eq!(
        package.models.len(),
        1,
        "shared model should be registered once across ModelRenderer + Character"
    );
    // The player character references the model; the authored
    // renderer component is consumed by the player path, not
    // emitted as a second static draw.
    assert_eq!(package.characters[0].model, 0);
    assert!(package.model_instances.is_empty());
}

#[test]
fn player_character_model_lands_in_room_residency_without_placed_meshinstance() {
    // Simulate a project where the player Character points
    // at a Model that *isn't* also placed as a MeshInstance.
    // The starter has both, so we delete the placed renderer
    // before cooking and assert residency still picks up the
    // Wraith mesh + atlas + clips via the player path.
    let mut project = project_with_one_room();
    remove_model_renderer_components(&mut project);
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("package returned on ok report");
    // Only the player path should have registered the model
    // -- there's no MeshInstance left to pull it in.
    assert!(package.model_instances.is_empty());
    assert_eq!(package.models.len(), 1);
    assert_eq!(package.characters.len(), 1);

    let manifest = render_manifest_source(&package);
    // Asset indexes for the player mesh, atlas, and clips
    // come straight from `package.assets` -- every one of
    // them must show up in ROOM_0_REQUIRED_RAM/VRAM.
    let wraith = &package.models[0];
    let mesh_token = format!("AssetId({})", wraith.mesh_asset_index);
    assert!(
        manifest_contains_required(&manifest, "RAM", 0, &mesh_token),
        "RAM missing player mesh: {mesh_token}"
    );
    let atlas_token = format!(
        "AssetId({})",
        wraith
            .texture_asset_index
            .expect("starter wraith has atlas")
    );
    assert!(
        manifest_contains_required(&manifest, "VRAM", 0, &atlas_token),
        "VRAM missing player atlas: {atlas_token}"
    );
    let cf = wraith.clip_first as usize;
    let cc = wraith.clip_count as usize;
    for clip in &package.model_clips[cf..cf + cc] {
        let tok = format!("AssetId({})", clip.animation_asset_index);
        assert!(
            manifest_contains_required(&manifest, "RAM", 0, &tok),
            "RAM missing clip {}: {tok}",
            clip.name
        );
    }
}

#[test]
fn player_character_model_assets_dedupe_with_placed_meshinstance() {
    // Starter's player model is referenced twice: by the
    // placed renderer and by the Character. Each asset still
    // shows up exactly once in the manifest's residency
    // slice -- the player path mustn't double-add.
    let project = project_with_one_room();
    let (package, _) = build_package(&project, &starter_project_root());
    let package = package.expect("starter cooks");
    let manifest = render_manifest_source(&package);
    let wraith = &package.models[0];

    let mesh_token = format!("AssetId({})", wraith.mesh_asset_index);
    assert_eq!(
        count_required_occurrences(&manifest, "RAM", 0, &mesh_token),
        1,
        "player mesh appears more than once in RAM residency"
    );
    let atlas = wraith.texture_asset_index.unwrap();
    let atlas_token = format!("AssetId({atlas})");
    assert_eq!(
        count_required_occurrences(&manifest, "VRAM", 0, &atlas_token),
        1,
        "wraith atlas appears more than once in VRAM residency"
    );
}

/// `true` when `ROOM_<idx>_REQUIRED_<kind>` contains `token`.
fn manifest_contains_required(manifest: &str, kind: &str, idx: u16, token: &str) -> bool {
    count_required_occurrences(manifest, kind, idx, token) > 0
}

/// Count occurrences of `token` inside the
/// `ROOM_<idx>_REQUIRED_<kind>` slice declaration. Robust
/// enough for residency assertions; not a full Rust parser.
fn count_required_occurrences(manifest: &str, kind: &str, idx: u16, token: &str) -> usize {
    let header = format!("ROOM_{idx}_REQUIRED_{kind}: &[AssetId] = &[");
    let Some(start) = manifest.find(&header) else {
        return 0;
    };
    let body = &manifest[start + header.len()..];
    let Some(end) = body.find("];") else {
        return 0;
    };
    body[..end].matches(token).count()
}

#[test]
fn rendered_manifest_includes_characters_and_player_controller() {
    let project = project_with_one_room();
    let (package, _) = build_package(&project, &starter_project_root());
    let manifest = render_manifest_source(&package.unwrap());
    assert!(manifest.contains("pub static CHARACTERS:"));
    assert!(manifest.contains("LevelCharacterRecord"));
    assert!(manifest.contains("pub static PLAYER_CONTROLLER:"));
    assert!(manifest.contains("Some(PlayerControllerRecord"));
    assert!(manifest.contains("CHARACTER_CLIP_NONE"));
}

#[test]
fn legacy_spawn_without_character_assignment_auto_picks_when_one_exists() {
    // Keep exactly one Character. Component-authored players use
    // their Model Renderer directly, so this legacy auto-pick path
    // is only for SpawnPoint-authored projects.
    let mut project = project_with_one_room();
    let player_character = player_character_resource_id(&project);
    project.resources.retain(|resource| {
        !matches!(resource.data, ResourceData::Character(_)) || resource.id == player_character
    });
    demote_player_spawns(&mut project);
    let room_id = project
        .active_scene()
        .nodes()
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Section { .. }))
        .map(|n| n.id)
        .unwrap();
    let spawn_id = project.active_scene_mut().add_node(
        room_id,
        "Legacy Player Spawn",
        NodeKind::SpawnPoint {
            player: true,
            character: None,
        },
    );
    if let Some(node) = project.active_scene_mut().node_mut(spawn_id) {
        node.transform.translation = [0.0, 0.0, 0.0];
    }
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(
        report.is_ok(),
        "errors: {:?}; warnings: {:?}",
        report.errors,
        report.warnings
    );
    let package = package.expect("auto-pick should succeed");
    assert!(package.player_controller.is_some());
    assert!(report.warnings.iter().any(|w| w.contains("auto-picked")));
}

#[test]
fn component_player_without_profile_uses_model_renderer_and_animator() {
    let mut project = project_with_one_room();
    let model = player_model_resource_id(&project);
    let controller_id = player_controller_component_id(&project);
    let scene = project.active_scene_mut();
    if let Some(controller) = scene.node_mut(controller_id) {
        let NodeKind::CharacterController {
            character,
            settings,
            ..
        } = &mut controller.kind
        else {
            panic!("starter player controller must be a Character Controller");
        };
        *character = None;
        settings.walk_speed = 77;
    }
    let player = player_spawn_node_id(&project);
    let animator_id = project
        .active_scene()
        .node(player)
        .and_then(|node| {
            node.children.iter().find_map(|id| {
                project.active_scene().node(*id).and_then(|child| {
                    matches!(child.kind, NodeKind::Animator { .. }).then_some(child.id)
                })
            })
        })
        .expect("starter player has Animator component");
    if let Some(animator) = project.active_scene_mut().node_mut(animator_id) {
        let NodeKind::Animator { action_clips, .. } = &mut animator.kind else {
            panic!("expected Animator component");
        };
        action_clips.push(crate::CharacterActionClip {
            action: CharacterAnimationAction::Idle,
            clip: 0,
            options: None,
        });
        action_clips.push(crate::CharacterActionClip {
            action: CharacterAnimationAction::Walk,
            clip: 0,
            options: None,
        });
        action_clips.push(crate::CharacterActionClip {
            action: CharacterAnimationAction::Backstep,
            clip: 0,
            options: Some(crate::CharacterActionOptions {
                looping: true,
                in_place: false,
                speed_q8: crate::ACTION_SPEED_UNSCALED_Q8,
                frame_start: 3,
                frame_end: 9,
                push_distance: 256,
                push_frame_start: 4,
                push_frame_end: 8,
            }),
        });
    }

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    assert!(
        !report.warnings.iter().any(|w| w.contains("auto-picked")),
        "component player should not secretly auto-pick a profile: {:?}",
        report.warnings
    );
    let package = package.expect("component player cooks");
    let character = &package.characters[0];
    assert_eq!(character.walk_speed, 77);
    assert_eq!(
        package.models[character.model as usize].source_resource,
        model
    );
    assert_eq!(
        character.action_clips[CharacterAnimationAction::Backstep.to_index()],
        0
    );
    assert_eq!(
        character.action_flags[CharacterAnimationAction::Backstep.to_index()],
        character_action_flags::LOOPING | character_action_flags::IN_PLACE_OVERRIDE
    );
    assert_eq!(
        character.action_frame_ranges[CharacterAnimationAction::Backstep.to_index()],
        psx_level::CharacterActionFrameRange { start: 3, end: 9 }
    );
    assert_eq!(
        character.action_pushes[CharacterAnimationAction::Backstep.to_index()],
        psx_level::CharacterActionPush {
            distance: 256,
            frame_range: psx_level::CharacterActionFrameRange { start: 4, end: 8 },
        }
    );
}

#[test]
fn character_controller_with_zero_radius_fails_validation() {
    let mut project = project_with_one_room();
    let controller_id = player_controller_component_id(&project);
    if let Some(node) = project.active_scene_mut().node_mut(controller_id) {
        if let NodeKind::CharacterController { settings, .. } = &mut node.kind {
            settings.radius = 0;
        }
    }
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(package.is_none());
    assert!(report
        .errors
        .iter()
        .any(|e| e.contains("radius must be > 0")));
}

#[test]
fn legacy_spawn_without_character_field_still_loads() {
    // Older project.ron files lacked `character` on
    // SpawnPoint. `#[serde(default)]` should fill it with
    // `None` so they keep deserializing.
    let ron = r#"(
            name: "Legacy",
            scenes: [(
                name: "Main",
                root: (1),
                next_node_id: 3,
                nodes: [
                    (id: (1), name: "Root", kind: Node3D, transform: (translation: (0.0, 0.0, 0.0), rotation_degrees: (0.0, 0.0, 0.0), scale: (1.0, 1.0, 1.0)), parent: None, children: [(2)]),
                    (id: (2), name: "Spawn", kind: SpawnPoint(player: true), transform: (translation: (0.0, 0.0, 0.0), rotation_degrees: (0.0, 0.0, 0.0), scale: (1.0, 1.0, 1.0)), parent: Some((1)), children: []),
                ],
            )],
            resources: [],
            next_resource_id: 1,
        )"#;
    let project = ProjectDocument::from_ron_str(ron).expect("legacy spawn deserializes");
    let scene = project.active_scene();
    let spawn = scene
        .nodes()
        .iter()
        .find(|n| matches!(n.kind, NodeKind::SpawnPoint { player: true, .. }))
        .expect("spawn round-tripped");
    if let NodeKind::SpawnPoint { character, .. } = &spawn.kind {
        assert!(character.is_none(), "missing field should default to None");
    }
}

#[test]
fn character_resource_roundtrips_through_ron() {
    use crate::CharacterResource;
    let mut project = ProjectDocument::starter();
    let id = project.add_resource(
        "Test Character",
        crate::ResourceData::Character(CharacterResource {
            model: None,
            animation_set: None,
            radius: 200,
            height: 1024,
            walk_speed: 50,
            run_speed: 100,
            turn_speed_degrees_per_second: 240,
            camera_distance: 1500,
            camera_height: 800,
            camera_target_height: 600,
            ..CharacterResource::defaults()
        }),
    );
    let serialized = project.to_ron_string().expect("serializes");
    let reloaded = ProjectDocument::from_ron_str(&serialized).expect("deserializes");
    let resource = reloaded.resource(id).expect("character preserved");
    match &resource.data {
        crate::ResourceData::Character(c) => {
            assert_eq!(c.radius, 200);
            assert_eq!(c.walk_speed, 50);
            assert_eq!(
                c.roll_active_frames,
                CharacterResource::defaults().roll_active_frames
            );
            assert_eq!(c.camera_target_height, 600);
        }
        _ => panic!("character resource lost its variant after round-trip"),
    }
}
