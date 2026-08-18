use super::*;

#[derive(Debug, Deserialize, Serialize)]
struct UiFontScaleFixture {
    #[serde(
        default = "default_ui_font_scale",
        deserialize_with = "deserialize_ui_font_scale",
        serialize_with = "serialize_ui_font_scale"
    )]
    font_scale: u16,
}

#[test]
fn ui_font_scale_deserializes_legacy_and_fractional_values() {
    let legacy: UiFontScaleFixture = ron::from_str("(font_scale: 2)").unwrap();
    assert_eq!(legacy.font_scale, UI_FONT_SCALE_ONE_Q8 * 2);

    let fractional: UiFontScaleFixture = ron::from_str("(font_scale: 1.5)").unwrap();
    assert_eq!(
        fractional.font_scale,
        UI_FONT_SCALE_ONE_Q8 + UI_FONT_SCALE_ONE_Q8 / 2
    );

    let q8: UiFontScaleFixture = ron::from_str("(font_scale: 384)").unwrap();
    assert_eq!(
        q8.font_scale,
        UI_FONT_SCALE_ONE_Q8 + UI_FONT_SCALE_ONE_Q8 / 2
    );
}

#[test]
fn ui_font_scale_serializes_as_decimal_multiplier() {
    let value = UiFontScaleFixture {
        font_scale: UI_FONT_SCALE_ONE_Q8 + UI_FONT_SCALE_ONE_Q8 / 2,
    };
    let ron = ron::to_string(&value).unwrap();
    assert!(ron.contains("1.5"), "{ron}");
}

#[test]
fn horizontal_face_height_samples_editor_corner_convention() {
    let mut floor = GridHorizontalFace::flat(0, None);
    floor.heights = [100, 200, 300, 400];

    assert_eq!(floor.height_at_local(0, 1024, 1024), 100);
    assert_eq!(floor.height_at_local(1024, 1024, 1024), 200);
    assert_eq!(floor.height_at_local(1024, 0, 1024), 300);
    assert_eq!(floor.height_at_local(0, 0, 1024), 400);
}

#[test]
fn horizontal_face_lowest_height_includes_triangle_overrides() {
    let mut floor = GridHorizontalFace::flat(128, None);
    floor.heights = [64, 256, 384, 192];
    assert_eq!(floor.lowest_height(), 64);

    floor.triangle_heights_mut(1)[1] = -192;
    assert_eq!(floor.lowest_height(), -192);
}

#[test]
fn grid_floor_height_handles_negative_origin_cells() {
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.origin = [-1, -1];
    grid.set_floor(0, 0, 256, None);

    assert_eq!(grid.floor_height_at_room_local(-512, -512), Some(256));
    assert_eq!(grid.floor_height_at_room_local(0, 0), None);
}

#[test]
fn snap_height_rounds_to_nearest_quantum() {
    assert_eq!(HEIGHT_QUANTUM, 64);
    // Exact multiples are unchanged (positive + negative).
    assert_eq!(snap_height(0), 0);
    assert_eq!(snap_height(64), 64);
    assert_eq!(snap_height(1024), 1024);
    assert_eq!(snap_height(-64), -64);
    assert_eq!(snap_height(-1024), -1024);
    // Below half-quantum rounds down toward zero.
    assert_eq!(snap_height(31), 0);
    assert_eq!(snap_height(-31), 0);
    // At half-quantum (32), away-from-zero on both sides.
    assert_eq!(snap_height(32), 64);
    assert_eq!(snap_height(-32), -64);
    // Above half-quantum rounds up away from zero.
    assert_eq!(snap_height(33), 64);
    assert_eq!(snap_height(-33), -64);
    // Past one quantum the same rule applies -- round to the
    // nearest multiple.
    assert_eq!(snap_height(95), 64);
    assert_eq!(snap_height(96), 128);
    assert_eq!(snap_height(-95), -64);
    assert_eq!(snap_height(-96), -128);
}

#[test]
fn character_resource_deserializes_without_new_motor_tuning_fields() {
    let ron = r#"(
            model: None,
            animation_set: None,
            idle_clip: None,
            walk_clip: None,
            run_clip: None,
            turn_clip: None,
            radius: 192,
            height: 1024,
            walk_speed: 48,
            run_speed: 96,
            turn_speed_degrees_per_second: 180,
            camera_distance: 6144,
            camera_height: 1280,
            camera_target_height: 640,
        )"#;
    let character: CharacterResource =
        ron::from_str(ron).expect("legacy character resource deserializes");

    assert_eq!(
        character.stamina_max_q12,
        default_character_stamina_max_q12()
    );
    assert_eq!(character.roll_speed, default_character_roll_speed());
    assert_eq!(
        character.backstep_invulnerable_frames,
        default_character_backstep_invulnerable_frames()
    );
    assert!(character.combat_capsules.is_empty());
    assert!(character.material.is_none());
    assert_eq!(character.spawn_role, CharacterSpawnRole::Auto);
    assert!(character.enemy_behavior.is_none());
    assert_eq!(
        character.camera_lock_rise_percent,
        default_world_camera_lock_rise_percent()
    );
}

#[test]
fn character_combat_capsules_roundtrip_roles_and_joint_local_geometry() {
    let mut character = CharacterResource::defaults();
    character.combat_capsules = vec![
        CharacterCombatCapsule {
            name: "Torso".to_string(),
            joint: 3,
            capsule: JointCapsule {
                start: [0, -120, 0],
                end: [0, 180, 0],
                radius: 96,
            },
            role: CombatCapsuleRole::Hurtbox,
        },
        CharacterCombatCapsule {
            name: "Right Fist".to_string(),
            joint: 14,
            capsule: JointCapsule {
                start: [0, 0, 0],
                end: [90, 0, 0],
                radius: 42,
            },
            role: CombatCapsuleRole::Hitbox {
                action: CharacterAnimationAction::LightAttack,
                active_start_frame: 8,
                active_end_frame: 13,
                damage: 30,
                poise_damage: 20,
            },
        },
    ];

    let ron = ron::to_string(&character).expect("combat capsules serialize");
    let restored: CharacterResource = ron::from_str(&ron).expect("combat capsules deserialize");
    assert_eq!(restored, character);
}

#[test]
fn enemy_behavior_deserializes_without_combat_director_tuning_fields() {
    let ron = r#"(
        aggro_radius: 2048,
        patrol_offset: (0, 0, 0),
        patrol_wait_ticks: 60,
        windup_ticks: 20,
        recovery_ticks: 24,
        poise: 100,
        touch_damage: 10,
        max_health: 100,
    )"#;
    let enemy: EnemyBehaviorSettings =
        ron::from_str(ron).expect("legacy enemy behavior deserializes");
    let defaults = EnemyBehaviorSettings::defaults();

    assert_eq!(enemy.reaction_ticks, defaults.reaction_ticks);
    assert_eq!(enemy.preferred_distance, defaults.preferred_distance);
    assert_eq!(enemy.spacing_tolerance, defaults.spacing_tolerance);
    assert_eq!(
        enemy.decision_interval_ticks,
        defaults.decision_interval_ticks
    );
    assert_eq!(enemy.circle_chance, defaults.circle_chance);
    assert_eq!(enemy.attack_priority, defaults.attack_priority);
    assert_eq!(enemy.attack_cooldown_ticks, defaults.attack_cooldown_ticks);
    assert_eq!(
        enemy.group_attack_delay_ticks,
        defaults.group_attack_delay_ticks
    );
}

#[test]
fn cortex_project_deserializes_authored_enemy_combat_profile() {
    // Reads the TRACKED sample rather than a working project. The original
    // fixture, `projects/cortex_ignition_v1`, was renamed and its successor
    // lives under `projects/`, which is gitignored so working projects stay
    // untracked -- so pointing this at cortex_v1 would pass locally and fail on
    // any fresh checkout. `samples/cortex_v1` is the miniaturised copy that
    // ships with releases, and it carries the same authored enemy profile.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples/cortex_v1/project.ron");
    let text = std::fs::read_to_string(&path).expect("tracked Cortex sample is readable");
    let project = ProjectDocument::from_ron_str(&text).expect("tracked Cortex project parses");
    let enemy = project
        .active_scene()
        .nodes()
        .iter()
        .find_map(|node| match &node.kind {
            NodeKind::CharacterController {
                settings:
                    CharacterControllerSettings {
                        enemy: Some(enemy), ..
                    },
                player: false,
                ..
            } => Some(*enemy),
            _ => None,
        })
        .expect("Cortex scene contains an enemy controller");

    assert_eq!(enemy.reaction_ticks, 22);
    assert_eq!(enemy.preferred_distance, 768);
    assert_eq!(enemy.spacing_tolerance, 128);
    assert_eq!(enemy.decision_interval_ticks, 12);
    assert_eq!(enemy.circle_chance, 65);
    assert_eq!(enemy.attack_priority, 4);
    assert_eq!(enemy.attack_cooldown_ticks, 45);
    assert_eq!(enemy.group_attack_delay_ticks, 18);
}

#[test]
fn camera_node_kind_serializes_roundtrip() {
    let kind = NodeKind::Camera {
        settings: WorldCameraSettings {
            distance: 2048,
            height: 900,
            target_height: 700,
            lock_rise_percent: 24,
            min_floor_clearance: 96,
            orbit_speed_level: 6,
            position_lag_shift: 1,
            focus_lag_shift: 2,
            distance_lag_shift: 3,
        },
    };

    let ron = ron::to_string(&kind).expect("camera node serializes");
    let decoded: NodeKind = ron::from_str(&ron).expect("camera node deserializes");
    assert_eq!(decoded, kind);
}

#[test]
fn sky_settings_resolve_clamps_subdivision_defaults() {
    let default_sky = SkySettings::default().resolved_for_room(false, [0, 0, 0]);
    assert_eq!(default_sky.skybox_columns, SKYBOX_COLUMNS_DEFAULT);
    assert_eq!(default_sky.skybox_rows, SKYBOX_ROWS_DEFAULT);
    assert_eq!(
        default_sky.horizon_glow_percent,
        default_sky_horizon_glow_percent()
    );
    assert_eq!(
        default_sky.horizon_glow_yaw_degrees,
        default_sky_horizon_glow_yaw_degrees()
    );
    assert_eq!(default_sky.sun_enabled, default_sky_sun_enabled());
    assert_eq!(default_sky.sun_color, default_sky_sun_color());
    assert_eq!(default_sky.sun_border_color, default_sky_sun_border_color());
    assert_eq!(default_sky.sun_yaw_degrees, default_sky_sun_yaw_degrees());
    assert_eq!(
        default_sky.sun_pitch_degrees,
        default_sky_sun_pitch_degrees()
    );
    assert_eq!(default_sky.sun_size_percent, default_sky_sun_size_percent());
    assert_eq!(default_sky.sun_glow_percent, default_sky_sun_glow_percent());
    assert_eq!(
        default_sky.sun_glow_size_percent,
        default_sky_sun_glow_size_percent()
    );
    assert_eq!(
        default_sky.mountain_height_percent,
        default_sky_mountain_height_percent()
    );
    assert_eq!(
        default_sky.mountain_top_color,
        default_sky_mountain_top_color()
    );
    assert_eq!(
        default_sky.mountain_base_color,
        default_sky_mountain_base_color()
    );
    assert_eq!(
        default_sky.mountain_gap_percent,
        default_sky_mountain_gap_percent()
    );
    assert_eq!(
        default_sky.mountain_roughness_percent,
        default_sky_mountain_roughness_percent()
    );
    assert_eq!(
        default_sky.mountain_layer_count,
        default_sky_mountain_layer_count()
    );

    let sky = SkySettings {
        horizon_glow_percent: 240,
        horizon_glow_yaw_degrees: 720,
        sun_yaw_degrees: -720,
        sun_pitch_degrees: 120,
        sun_size_percent: 0,
        sun_glow_percent: 240,
        sun_glow_size_percent: 240,
        mountain_height_percent: 240,
        mountain_gap_percent: 240,
        mountain_roughness_percent: 240,
        mountain_layer_count: 9,
        skybox_columns: 1,
        skybox_rows: 99,
        ..Default::default()
    };
    let resolved = sky.resolved_for_room(false, [0, 0, 0]);
    assert_eq!(resolved.horizon_glow_percent, 100);
    assert_eq!(resolved.horizon_glow_yaw_degrees, 180);
    assert_eq!(resolved.sun_yaw_degrees, -180);
    assert_eq!(resolved.sun_pitch_degrees, 75);
    assert_eq!(resolved.sun_size_percent, 1);
    assert_eq!(resolved.sun_glow_percent, 100);
    assert_eq!(resolved.sun_glow_size_percent, 100);
    assert_eq!(
        resolved.mountain_height_percent,
        SKY_MOUNTAIN_HEIGHT_PERCENT_MAX
    );
    assert_eq!(resolved.mountain_gap_percent, 100);
    assert_eq!(resolved.mountain_roughness_percent, 100);
    assert_eq!(resolved.mountain_layer_count, 3);
    assert_eq!(resolved.skybox_columns, SKYBOX_COLUMNS_MIN);
    assert_eq!(resolved.skybox_rows, SKYBOX_ROWS_MAX);
}

#[test]
fn sky_cyclorama_generation_is_cook_time_geometry() {
    let mut sky = SkySettings::default();
    sky.cloud_layer.enabled = true;
    sky.cloud_layer.density = 192;
    let resolved = sky.resolved_for_room(false, [0, 0, 0]);
    let quads = generate_sky_cyclorama(resolved);
    assert!(!quads.is_empty());
    assert!(quads.len() <= SKY_CYCLORAMA_QUAD_MAX);
    assert!(quads
        .iter()
        .any(|quad| quad.direction_q12[0] != quad.direction_q12[1]));

    let mut disabled = sky;
    disabled.mode = SkyMode::Off;
    assert!(generate_sky_cyclorama(disabled.resolved_for_room(false, [0, 0, 0])).is_empty());
}

#[test]
fn dense_cyclorama_sky_stays_under_playtest_budget() {
    let mut sky = SkySettings {
        top_color: [36, 36, 36],
        horizon_color: [87, 34, 34],
        lower_color: [0, 0, 0],
        horizon_percent: 40,
        horizon_thickness_percent: 0,
        sun_enabled: true,
        mountain_layer_count: 3,
        skybox_columns: 12,
        skybox_rows: 5,
        ..Default::default()
    };
    sky.cloud_layer.enabled = true;
    sky.cloud_layer.color = [155, 142, 140];
    sky.cloud_layer.density = 255;
    sky.cloud_layer.altitude = 5800;
    sky.cloud_layer.extent = 49_800;
    sky.cloud_layer.tile_count = 9;
    sky.cloud_layer.noise_seed = 0x5a7b_c91d;

    let quads = generate_sky_cyclorama(sky.resolved_for_room(false, [0, 0, 0]));

    // The runtime consumes a baked panorama; this guard keeps
    // cook/editor-preview source geometry from growing without
    // bound as the procedural sky gains detail.
    assert!(
        quads.len() <= 1050,
        "dense sky generated {} quads",
        quads.len()
    );
}

#[test]
fn sky_cyclorama_sun_uses_faceted_polar_geometry() {
    let mut sky = SkySettings {
        sun_enabled: true,
        mountain_height_percent: 0,
        top_color: [178, 178, 198],
        horizon_color: [142, 108, 100],
        lower_color: [80, 58, 70],
        ..Default::default()
    };
    sky.cloud_layer.enabled = false;

    let resolved = sky.resolved_for_room(false, [0, 0, 0]);
    let base_quads = resolved.skybox_columns as usize * resolved.skybox_rows as usize;
    let quads = generate_sky_cyclorama(resolved);
    let sun_quads = &quads[base_quads..];

    assert_eq!(sun_quads.len(), SKY_CYCLORAMA_SUN_QUAD_MAX);
    assert!(sun_quads.iter().any(|quad| {
        quad.direction_q12[2] == quad.direction_q12[3]
            && quad.direction_q12[0] != quad.direction_q12[2]
    }));
    assert!(sun_quads.iter().any(|quad| quad.rgb[0] != quad.rgb[1]));
}

#[test]
fn normalize_loaded_snaps_room_heights_to_quantum() {
    let mut project = ProjectDocument::legacy_grid_starter();
    let room_id = project
        .active_scene()
        .nodes()
        .iter()
        .find(|node| matches!(node.kind, NodeKind::Section { .. }))
        .map(|node| node.id)
        .unwrap();
    {
        let room = project.active_scene_mut().node_mut(room_id).unwrap();
        let NodeKind::Section { grid } = &mut room.kind else {
            panic!("expected room");
        };
        let sector = grid.ensure_sector(0, 0).unwrap();
        sector.floor = Some(GridHorizontalFace::flat(33, None));
        let walls = sector.walls.get_mut(GridDirection::West);
        walls.clear();
        walls.push(GridVerticalFace::with_heights([0, 0, 965, 802], None));
    }

    project.normalize_loaded();

    let room = project.active_scene().node(room_id).unwrap();
    let NodeKind::Section { grid } = &room.kind else {
        panic!("expected room");
    };
    let sector = grid.sector(0, 0).unwrap();
    assert_eq!(sector.floor.as_ref().unwrap().heights, [64, 64, 64, 64]);
    assert_eq!(
        sector.walls.get(GridDirection::West)[0].heights,
        [0, 0, 960, 832]
    );
}

#[test]
fn snap_world_sector_size_quantizes_to_128_units() {
    assert_eq!(WORLD_SECTOR_SIZE_QUANTUM, 128);
    assert_eq!(snap_world_sector_size(1), 128);
    assert_eq!(snap_world_sector_size(127), 128);
    assert_eq!(snap_world_sector_size(191), 128);
    assert_eq!(snap_world_sector_size(192), 256);
    assert_eq!(snap_world_sector_size(1500), 1536);
    assert_eq!(
        snap_world_sector_size(MAX_WORLD_SECTOR_SIZE + 1),
        MAX_WORLD_SECTOR_SIZE
    );
}

#[test]
fn world_camera_settings_default_normalize_and_inherit() {
    let mut project = ProjectDocument::legacy_grid_starter();
    let scene = project.active_scene();
    let world_id = scene
        .nodes()
        .iter()
        .find(|node| matches!(node.kind, NodeKind::World { .. }))
        .map(|node| node.id)
        .expect("starter has world");
    let room_id = scene
        .nodes()
        .iter()
        .find(|node| matches!(node.kind, NodeKind::Section { .. }))
        .map(|node| node.id)
        .expect("starter has room");

    let inherited_camera = scene
        .nodes()
        .iter()
        .find_map(|node| match &node.kind {
            NodeKind::World { camera, .. } => Some(*camera),
            _ => None,
        })
        .expect("starter world has camera settings");
    assert_eq!(scene.world_camera_for_node(room_id), Some(inherited_camera));

    {
        let world = project.active_scene_mut().node_mut(world_id).unwrap();
        let NodeKind::World { camera, .. } = &mut world.kind else {
            panic!("expected world");
        };
        *camera = WorldCameraSettings {
            distance: 1,
            height: MAX_WORLD_CAMERA_HEIGHT + 1,
            target_height: -1,
            lock_rise_percent: MAX_WORLD_CAMERA_LOCK_RISE_PERCENT + 1,
            min_floor_clearance: MAX_WORLD_CAMERA_MIN_FLOOR_CLEARANCE + 1,
            orbit_speed_level: MAX_WORLD_CAMERA_ORBIT_SPEED_LEVEL + 1,
            position_lag_shift: MAX_WORLD_CAMERA_LAG_SHIFT + 1,
            focus_lag_shift: MAX_WORLD_CAMERA_LAG_SHIFT + 2,
            distance_lag_shift: MAX_WORLD_CAMERA_LAG_SHIFT + 3,
        };
    }

    project.normalize_loaded();

    assert_eq!(
        project.active_scene().world_camera_for_node(room_id),
        Some(WorldCameraSettings {
            distance: MIN_WORLD_CAMERA_DISTANCE,
            height: MAX_WORLD_CAMERA_HEIGHT,
            target_height: 0,
            lock_rise_percent: MAX_WORLD_CAMERA_LOCK_RISE_PERCENT,
            min_floor_clearance: MAX_WORLD_CAMERA_MIN_FLOOR_CLEARANCE,
            orbit_speed_level: MAX_WORLD_CAMERA_ORBIT_SPEED_LEVEL,
            position_lag_shift: MAX_WORLD_CAMERA_LAG_SHIFT,
            focus_lag_shift: MAX_WORLD_CAMERA_LAG_SHIFT,
            distance_lag_shift: MAX_WORLD_CAMERA_LAG_SHIFT,
        })
    );
}

#[test]
fn world_streaming_settings_separate_resident_and_visible_limits() {
    let settings = WorldStreamingSettings {
        resident_chunk_limit: 24,
        visible_chunk_limit: 8,
    }
    .normalized();

    assert_eq!(settings.resident_chunk_limit, 24);
    assert_eq!(settings.visible_chunk_limit, 8);
}

#[test]
fn world_streaming_legacy_visible_limit_inherits_resident_limit() {
    let settings = WorldStreamingSettings {
        resident_chunk_limit: 18,
        visible_chunk_limit: 0,
    }
    .normalized();

    assert_eq!(settings.resident_chunk_limit, 18);
    assert_eq!(settings.visible_chunk_limit, 18);
}

#[test]
fn changing_world_sector_size_rescales_descendant_room_and_colliders() {
    let mut project = ProjectDocument::new("test");
    let scene = project.active_scene_mut();
    let world = scene.add_node(
        scene.root,
        "World",
        NodeKind::World {
            sector_size: 1024,
            sky: SkySettings::default(),
            far_vista: FarVistaSettings::default(),
            camera: WorldCameraSettings::default(),
            culling: WorldCullingSettings::default(),
            streaming: WorldStreamingSettings::default(),
            physics: WorldPhysicsSettings::default(),
        },
    );
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.set_floor(0, 0, 160, None);
    grid.add_wall(0, 0, GridDirection::North, 0, 1024, None);
    let upper = grid.push_floor();
    let upper_grid = grid.floor_mut(upper).expect("upper floor");
    upper_grid.set_floor(0, 0, 320, None);
    upper_grid.add_wall(0, 0, GridDirection::North, 0, 2048, None);
    let room = scene.add_node(world, "Room", NodeKind::Section { grid });
    let entity = scene.add_node(room, "Entity", NodeKind::Entity);
    let collider = scene.add_node(
        entity,
        "Collider",
        NodeKind::Collider {
            shape: ColliderShape::Capsule {
                radius: 128,
                height: 1024,
            },
            solid: true,
        },
    );

    assert_eq!(project.set_world_sector_size(world, 1500), Some(1536));
    assert_eq!(project.world_sector_size_for_node(entity), 1536);

    let scene = project.active_scene();
    let NodeKind::Section { grid } = &scene.node(room).unwrap().kind else {
        panic!("expected Room");
    };
    assert_eq!(grid.sector_size, 1536);
    let sector = grid.sector(0, 0).unwrap();
    assert_eq!(sector.floor.as_ref().unwrap().heights, [256; 4]);
    assert_eq!(
        sector
            .walls
            .get(GridDirection::North)
            .first()
            .unwrap()
            .heights,
        [0, 0, 1536, 1536]
    );
    let upper_grid = grid.floor(upper).expect("upper floor remains present");
    assert_eq!(upper_grid.sector_size, 1536);
    assert_eq!(upper_grid.elevation, 3072);
    let upper_sector = upper_grid.sector(0, 0).expect("upper sector");
    assert_eq!(upper_sector.floor.as_ref().unwrap().heights, [512; 4]);
    assert_eq!(
        upper_sector
            .walls
            .get(GridDirection::North)
            .first()
            .unwrap()
            .heights,
        [0, 0, 3072, 3072]
    );

    let NodeKind::Collider {
        shape: ColliderShape::Capsule { radius, height },
        ..
    } = &scene.node(collider).unwrap().kind
    else {
        panic!("expected capsule collider");
    };
    assert_eq!((*radius, *height), (192, 1536));
}

#[test]
fn normalize_loaded_removes_only_legacy_character_capsule_colliders() {
    let mut project = ProjectDocument::new("legacy-character-collider");
    let scene = project.active_scene_mut();
    let character = scene.add_node(scene.root, "Character", NodeKind::Entity);
    scene.add_node(
        character,
        "Character Controller",
        NodeKind::CharacterController {
            character: None,
            settings: CharacterControllerSettings::default(),
            player: true,
        },
    );
    let redundant_capsule = scene.add_node(
        character,
        "Collider",
        NodeKind::Collider {
            shape: ColliderShape::Capsule {
                radius: 312,
                height: 1664,
            },
            solid: true,
        },
    );
    let retained_box = scene.add_node(
        character,
        "Hit Box",
        NodeKind::Collider {
            shape: ColliderShape::Box {
                half_extents: [128; 3],
            },
            solid: false,
        },
    );
    let retained_custom_capsule = scene.add_node(
        character,
        "Interaction Capsule",
        NodeKind::Collider {
            shape: ColliderShape::Capsule {
                radius: 640,
                height: 1024,
            },
            solid: false,
        },
    );
    let prop = scene.add_node(scene.root, "Prop", NodeKind::Entity);
    let retained_prop_capsule = scene.add_node(
        prop,
        "Collider",
        NodeKind::Collider {
            shape: ColliderShape::Capsule {
                radius: 128,
                height: 512,
            },
            solid: true,
        },
    );

    project.normalize_loaded();

    let scene = project.active_scene();
    assert!(scene.node(redundant_capsule).is_none());
    assert!(scene.node(retained_box).is_some());
    assert!(scene.node(retained_custom_capsule).is_some());
    assert!(scene.node(retained_prop_capsule).is_some());
    assert!(!scene
        .node(character)
        .unwrap()
        .children
        .contains(&redundant_capsule));
}

#[test]
fn saving_normalizes_room_sector_size_to_world() {
    let mut project = ProjectDocument::legacy_grid_starter();
    let scene = project.active_scene_mut();
    let room_id = scene
        .nodes()
        .iter()
        .find(|node| matches!(node.kind, NodeKind::Section { .. }))
        .map(|node| node.id)
        .unwrap();
    let NodeKind::Section { grid } = &mut scene.node_mut(room_id).unwrap().kind else {
        panic!("expected Room");
    };
    grid.sector_size = 2030;

    let dir = unique_temp_dir("normalize-room-sector-size");
    let path = dir.join("project.ron");
    project.save_to_path(&path).unwrap();

    let saved = std::fs::read_to_string(&path).unwrap();
    let expected_sector_size = project.world_sector_size_for_node(room_id);
    assert!(saved.contains(&format!("kind: World(sector_size: {expected_sector_size},")));
    assert!(saved.contains(&format!("sector_size: {expected_sector_size}")));
    assert!(!saved.contains("sector_size: 2030"));

    let loaded = ProjectDocument::load_from_path(&path).unwrap();
    let scene = loaded.active_scene();
    let NodeKind::Section { grid } = &scene.node(room_id).unwrap().kind else {
        panic!("expected Room");
    };
    assert_eq!(grid.sector_size, expected_sector_size);

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn dynamic_material_recipe_round_trips_through_project_ron() {
    let mut project = ProjectDocument::new("dynamic material persistence");
    let mut material = MaterialResource::opaque(None);
    let layer = ModelSecondaryLayer {
        motion: MaterialUvMotion {
            enabled: true,
            speed_u_q8: 640,
            speed_v_q8: -384,
            phase_u: 17,
            phase_v: 231,
        },
        ..Default::default()
    };
    material.secondary_layer = Some(layer.clone());
    material.set_secondary_layer_enabled(false);
    material.animation = MaterialAnimation {
        mode: MaterialAnimationMode::Flipbook,
        flipbook: MaterialFlipbook {
            columns: 4,
            rows: 2,
            frame_count: 7,
            ticks_per_frame: 3,
            phase: 2,
        },
        ..MaterialAnimation::default()
    };
    let expected_animation = material.animation;
    let material_id = project.add_resource("Flowing glass", ResourceData::Material(material));

    let dir = unique_temp_dir("dynamic-material-roundtrip");
    let path = dir.join("project.ron");
    project.save_to_path(&path).unwrap();
    let loaded = ProjectDocument::load_from_path(&path).unwrap();
    let ResourceData::Material(loaded_material) = &loaded.resource(material_id).unwrap().data
    else {
        panic!("saved resource changed kind");
    };
    assert!(loaded_material.enabled_secondary_layer().is_none());
    let loaded_layer = loaded_material
        .secondary_layer
        .as_ref()
        .expect("disabled layer recipe remains serialized");
    assert!(!loaded_layer.enabled);
    assert_eq!(loaded_layer.motion, layer.motion);
    assert_eq!(loaded_material.animation, expected_animation);

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn material_versions_preserve_complete_recipes_and_stable_identity() {
    let mut material = MaterialResource::opaque(Some("materials/original.psxt".to_string()));
    material.tint = [24, 48, 96];
    material.blend_mode = PsxBlendMode::Average;
    material.animation.mode = MaterialAnimationMode::UvScroll;
    material.secondary_layer = Some(ModelSecondaryLayer::moving_default());

    let original_recipe = MaterialVersionRecipe::from(&material);
    let llm_version = material.create_version("LLM Gothic");
    assert_eq!(llm_version.raw(), 2);
    assert_eq!(material.active_version_name, "LLM Gothic");
    assert_eq!(material.version_count(), 2);

    material.texture_mode = MaterialTextureMode::Generated;
    material.psxt_path = Some("materials/llm_gothic.psxt".to_string());
    material.tint = [131, 37, 171];
    material.generated.noise.seed = 0xfeed_beef;
    material.secondary_layer = None;
    let llm_recipe = MaterialVersionRecipe::from(&material);

    assert!(material.activate_version(MaterialVersionId::ORIGINAL));
    assert_eq!(material.active_version_name, "Original");
    assert_eq!(MaterialVersionRecipe::from(&material), original_recipe);
    assert!(material.activate_version(llm_version));
    assert_eq!(MaterialVersionRecipe::from(&material), llm_recipe);

    assert!(material.rename_version(llm_version, "LLM Cathedral"));
    assert!(!material.rename_version(llm_version, "Original"));
    assert!(material.delete_version(llm_version));
    assert_eq!(material.active_version_id, MaterialVersionId::ORIGINAL);
    assert_eq!(material.version_count(), 1);
    assert!(!material.delete_version(MaterialVersionId::ORIGINAL));
}

#[test]
fn material_versions_round_trip_and_legacy_materials_become_original() {
    let legacy = r#"(
        texture_mode: SimpleImage,
        psxt_path: Some("materials/legacy.psxt"),
        blend_mode: Opaque,
        tint: (128, 128, 128),
        face_sidedness: Both,
        double_sided: true,
    )"#;
    let legacy: MaterialResource = ron::from_str(legacy).expect("legacy material parses");
    assert_eq!(legacy.active_version_id, MaterialVersionId::ORIGINAL);
    assert_eq!(legacy.active_version_name, "Original");
    assert_eq!(legacy.version_count(), 1);

    let mut project = ProjectDocument::new("version persistence");
    let mut material = legacy;
    let generated = material.create_version("LLM Moss");
    material.texture_mode = MaterialTextureMode::Generated;
    material.generated.noise.seed = 73;
    material.tint = [52, 91, 47];
    let material_id = project.add_resource("Stone", ResourceData::Material(material));

    let loaded = ProjectDocument::from_ron_str(&project.to_ron_string().unwrap()).unwrap();
    let ResourceData::Material(loaded) = &loaded.resource(material_id).unwrap().data else {
        panic!("material changed resource kind");
    };
    assert_eq!(loaded.active_version_id, generated);
    assert_eq!(loaded.active_version_name, "LLM Moss");
    assert_eq!(loaded.version_count(), 2);
    assert_eq!(loaded.generated.noise.seed, 73);
    assert!(loaded
        .version_options()
        .iter()
        .any(|(id, name)| *id == MaterialVersionId::ORIGINAL && name == "Original"));
}

#[test]
fn new_materials_default_to_front_faces() {
    for material in [
        MaterialResource::opaque(None),
        MaterialResource::translucent(None, PsxBlendMode::Average),
    ] {
        assert_eq!(material.face_sidedness, MaterialFaceSidedness::Front);
        assert_eq!(material.sidedness(), MaterialFaceSidedness::Front);
        assert!(!material.double_sided);
    }
    assert_eq!(
        MaterialFaceSidedness::default(),
        MaterialFaceSidedness::Front
    );
}

#[test]
fn legacy_double_sided_materials_still_resolve_to_both_faces() {
    let legacy = r#"(
        texture_mode: SimpleImage,
        blend_mode: Opaque,
        tint: (128, 128, 128),
        double_sided: true,
    )"#;
    let material: MaterialResource = ron::from_str(legacy).expect("legacy material parses");

    assert_eq!(material.face_sidedness, MaterialFaceSidedness::Front);
    assert_eq!(material.sidedness(), MaterialFaceSidedness::Both);
}

#[test]
fn grid_direction_physical_edges_use_editor_z_convention() {
    assert_eq!(
        GridDirection::North.physical_edge(2, 3),
        Some(GridPhysicalEdge {
            x: 2,
            z: 4,
            axis: GridEdgeAxis::EastWest,
        })
    );
    assert_eq!(
        GridDirection::South.physical_edge(2, 3),
        Some(GridPhysicalEdge {
            x: 2,
            z: 3,
            axis: GridEdgeAxis::EastWest,
        })
    );
    assert_eq!(
        GridDirection::East.physical_edge(2, 3),
        Some(GridPhysicalEdge {
            x: 3,
            z: 3,
            axis: GridEdgeAxis::NorthSouth,
        })
    );
    assert_eq!(
        GridDirection::West.physical_edge(2, 3),
        Some(GridPhysicalEdge {
            x: 2,
            z: 3,
            axis: GridEdgeAxis::NorthSouth,
        })
    );
    assert_eq!(GridDirection::NorthWestSouthEast.physical_edge(2, 3), None);
}

#[test]
fn cell_bounds_match_editor_corner_and_wall_convention() {
    let grid = WorldGrid::empty(2, 2, 1024);
    let bounds = grid.cell_bounds_world(1, 1);

    assert_eq!(bounds.horizontal_corner_xz(Corner::NW), [1024, 2048]);
    assert_eq!(bounds.horizontal_corner_xz(Corner::NE), [2048, 2048]);
    assert_eq!(bounds.horizontal_corner_xz(Corner::SE), [2048, 1024]);
    assert_eq!(bounds.horizontal_corner_xz(Corner::SW), [1024, 1024]);

    assert_eq!(
        bounds.wall_endpoints_xz(GridDirection::North),
        Some(([1024, 2048], [2048, 2048]))
    );
    assert_eq!(
        bounds.wall_endpoints_xz(GridDirection::South),
        Some(([2048, 1024], [1024, 1024]))
    );
    assert_eq!(
        bounds.wall_endpoints_xz(GridDirection::NorthWestSouthEast),
        Some(([1024, 2048], [2048, 1024]))
    );
    assert_eq!(
        bounds.wall_endpoints_xz(GridDirection::NorthEastSouthWest),
        Some(([2048, 2048], [1024, 1024]))
    );
}

#[test]
fn wall_placement_aligns_bottom_edge_to_floor_vertices() {
    let mut grid = WorldGrid::empty(1, 1, 1024);
    let mut floor = GridHorizontalFace::flat(0, None);
    floor.heights = [128, 256, 384, 512];
    grid.ensure_sector(0, 0).unwrap().floor = Some(floor);

    grid.add_wall_aligned_to_surfaces(0, 0, GridDirection::North, None);

    let wall = grid
        .sector(0, 0)
        .unwrap()
        .walls
        .get(GridDirection::North)
        .first()
        .unwrap();
    assert_eq!(wall.heights, [128, 256, 2304, 2176]);
}

#[test]
fn wall_placement_aligns_top_edge_to_ceiling_vertices() {
    let mut grid = WorldGrid::empty(1, 1, 1024);
    let mut floor = GridHorizontalFace::flat(0, None);
    floor.heights = [128, 256, 384, 512];
    let mut ceiling = GridHorizontalFace::flat(1024, None);
    ceiling.heights = [900, 1000, 1100, 1200];
    let sector = grid.ensure_sector(0, 0).unwrap();
    sector.floor = Some(floor);
    sector.ceiling = Some(ceiling);

    grid.add_wall_aligned_to_surfaces(0, 0, GridDirection::East, None);

    let wall = grid
        .sector(0, 0)
        .unwrap()
        .walls
        .get(GridDirection::East)
        .first()
        .unwrap();
    assert_eq!(wall.heights, [256, 384, 1100, 1000]);
}

#[test]
fn diagonal_wall_placement_aligns_to_horizontal_diagonal_vertices() {
    let mut grid = WorldGrid::empty(1, 1, 1024);
    let mut floor = GridHorizontalFace::flat(0, None);
    floor.heights = [128, 256, 384, 512];
    let mut ceiling = GridHorizontalFace::flat(1024, None);
    ceiling.heights = [900, 1000, 1100, 1200];
    let sector = grid.ensure_sector(0, 0).unwrap();
    sector.floor = Some(floor);
    sector.ceiling = Some(ceiling);

    grid.add_wall_aligned_to_surfaces(0, 0, GridDirection::NorthWestSouthEast, None);
    grid.add_wall_aligned_to_surfaces(0, 0, GridDirection::NorthEastSouthWest, None);

    let sector = grid.sector(0, 0).unwrap();
    let nw_se = sector
        .walls
        .get(GridDirection::NorthWestSouthEast)
        .first()
        .unwrap();
    let ne_sw = sector
        .walls
        .get(GridDirection::NorthEastSouthWest)
        .first()
        .unwrap();
    assert_eq!(nw_se.heights, [128, 384, 1100, 900]);
    assert_eq!(ne_sw.heights, [256, 512, 1200, 1000]);
}

#[test]
fn ceiling_placement_aligns_edge_to_touching_wall_top() {
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.ensure_sector(0, 0)
        .unwrap()
        .walls
        .get_mut(GridDirection::North)
        .push(GridVerticalFace::with_heights([0, 0, 1472, 1344], None));

    grid.set_ceiling_aligned_to_neighbors(0, 0, None);

    let ceiling = grid.sector(0, 0).unwrap().ceiling.as_ref().unwrap();
    assert_eq!(ceiling.heights, [1344, 1472, 2048, 2048]);
}

#[test]
fn floor_placement_with_one_flat_neighbor_uses_that_height_for_whole_face() {
    let mut grid = WorldGrid::empty(2, 1, 1024);
    grid.set_floor(0, 0, 384, None);

    grid.set_floor_aligned_to_neighbors(1, 0, 0, None);

    let floor = grid.sector(1, 0).unwrap().floor.as_ref().unwrap();
    assert_eq!(floor.heights, [384; 4]);
}

#[test]
fn floor_preview_for_off_grid_cell_matches_flat_neighbor_height() {
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.set_floor(0, 0, 384, None);

    let heights = grid.floor_heights_aligned_to_neighbors_for_world_cell(1, 0, 0);

    assert_eq!(heights, [384; 4]);
}

#[test]
fn floor_placement_with_one_sloped_neighbor_keeps_only_the_shared_edge() {
    let mut grid = WorldGrid::empty(2, 1, 1024);
    let mut floor = GridHorizontalFace::flat(0, None);
    floor.heights = [128, 256, 384, 512];
    grid.ensure_sector(0, 0).unwrap().floor = Some(floor);

    grid.set_floor_aligned_to_neighbors(1, 0, 0, None);

    let floor = grid.sector(1, 0).unwrap().floor.as_ref().unwrap();
    assert_eq!(floor.heights, [256, 0, 0, 384]);
}

#[test]
fn ceiling_placement_with_one_flat_neighbor_uses_that_height_for_whole_face() {
    let mut grid = WorldGrid::empty(2, 1, 1024);
    grid.ensure_sector(0, 0).unwrap().ceiling = Some(GridHorizontalFace::flat(1536, None));

    grid.set_ceiling_aligned_to_neighbors(1, 0, None);

    let ceiling = grid.sector(1, 0).unwrap().ceiling.as_ref().unwrap();
    assert_eq!(ceiling.heights, [1536; 4]);
}

#[test]
fn ceiling_preview_for_off_grid_cell_matches_flat_neighbor_height() {
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.ensure_sector(0, 0).unwrap().ceiling = Some(GridHorizontalFace::flat(1536, None));

    let heights = grid.ceiling_heights_aligned_to_neighbors_for_world_cell(1, 0);

    assert_eq!(heights, [1536; 4]);
}

#[test]
fn ceiling_placement_aligns_edge_to_touching_neighbor_wall_top() {
    let mut grid = WorldGrid::empty(2, 1, 1024);
    grid.ensure_sector(0, 0)
        .unwrap()
        .walls
        .get_mut(GridDirection::East)
        .push(GridVerticalFace::with_heights([0, 0, 1600, 1536], None));

    grid.set_ceiling_aligned_to_neighbors(1, 0, None);

    let ceiling = grid.sector(1, 0).unwrap().ceiling.as_ref().unwrap();
    assert_eq!(ceiling.heights, [1536, 2048, 2048, 1600]);
}

#[test]
fn ceiling_placement_aligns_edge_to_touching_neighbor_ceiling() {
    let mut grid = WorldGrid::empty(2, 1, 1024);
    let mut ceiling = GridHorizontalFace::flat(2048, None);
    ceiling.heights = [1024, 1152, 1280, 1408];
    grid.ensure_sector(0, 0).unwrap().ceiling = Some(ceiling);

    grid.set_ceiling_aligned_to_neighbors(1, 0, None);

    let ceiling = grid.sector(1, 0).unwrap().ceiling.as_ref().unwrap();
    assert_eq!(ceiling.heights, [1152, 2048, 2048, 1280]);
}

#[test]
fn off_grid_wall_preview_samples_adjacent_floor_edge() {
    let mut grid = WorldGrid::empty(1, 1, 1024);
    let mut floor = GridHorizontalFace::flat(0, None);
    floor.heights = [128, 256, 384, 512];
    grid.ensure_sector(0, 0).unwrap().floor = Some(floor);

    let heights = grid.wall_heights_aligned_to_surfaces_for_world_cell(1, 0, GridDirection::West);

    assert_eq!(heights, [384, 256, 2304, 2432]);
}

#[test]
fn wall_stack_placement_starts_above_highest_wall_top() {
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.add_wall(0, 0, GridDirection::North, 0, 1024, None);

    let heights = grid.wall_heights_above_stack_or_surfaces(0, 0, GridDirection::North);

    assert_eq!(heights, [1024, 1024, 3072, 3072]);
}

#[test]
fn wall_stack_placement_preserves_sloped_top_edge() {
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.ensure_sector(0, 0)
        .unwrap()
        .walls
        .get_mut(GridDirection::North)
        .push(GridVerticalFace::with_heights([0, 0, 1408, 1152], None));

    let heights = grid.wall_heights_above_stack_or_surfaces(0, 0, GridDirection::North);

    assert_eq!(heights, [1152, 1408, 3456, 3200]);
}

#[test]
fn pushing_floor_below_preserves_existing_layers_and_inherits_room_look() {
    let mut grid = WorldGrid::empty(2, 1, 1024);
    grid.elevation = 4096;
    grid.ambient_color = [18, 24, 31];
    grid.set_floor(1, 0, 128, None);
    grid.push_floor();
    grid.floor_mut(1).unwrap().set_floor(0, 0, 64, None);

    grid.push_floor_below();

    assert_eq!(grid.floor_count(), 3);
    assert_eq!(grid.elevation, 2048);
    assert_eq!(grid.ambient_color, [18, 24, 31]);
    assert!(grid.floor(0).unwrap().sector(1, 0).is_none());
    assert!(grid.floor(1).unwrap().sector(1, 0).unwrap().floor.is_some());
    assert_eq!(grid.floor(1).unwrap().elevation, 4096);
    assert!(grid.floor(2).unwrap().sector(0, 0).unwrap().floor.is_some());
    assert_eq!(grid.floor(2).unwrap().elevation, 6144);

    let encoded = ron::ser::to_string_pretty(&grid, ron::ser::PrettyConfig::default()).unwrap();
    let decoded: WorldGrid = ron::from_str(&encoded).unwrap();
    assert_eq!(
        decoded, grid,
        "stacked layers must use the existing RON schema"
    );
}

#[test]
fn removing_empty_base_floor_promotes_upper_floor_and_reports_height_shift() {
    let mut grid = WorldGrid::empty(2, 2, 1024);
    grid.elevation = -2048;
    grid.push_floor();
    grid.floor_mut(1).unwrap().set_floor(1, 1, 0, None);

    let shift = grid.remove_empty_floor(0).expect("empty base is removable");

    assert_eq!(shift, 2048);
    assert_eq!(grid.floor_count(), 1);
    assert_eq!(grid.elevation, 0);
    assert!(grid.sector(1, 1).unwrap().floor.is_some());
}

#[test]
fn removing_empty_floor_refuses_the_only_or_a_populated_floor() {
    let mut grid = WorldGrid::empty(1, 1, 1024);
    assert_eq!(grid.remove_empty_floor(0), None);

    grid.push_floor();
    grid.set_floor(0, 0, 0, None);
    assert_eq!(grid.remove_empty_floor(0), None);
    assert_eq!(grid.floor_count(), 2);
}

#[test]
fn stone_room_perimeter_uses_editor_direction_convention() {
    let grid = WorldGrid::stone_room(2, 3, 1024, None, None);
    let default_wall_height = default_wall_height_for_sector_size(1024);

    for x in 0..grid.width {
        assert!(!grid
            .sector(x, 0)
            .unwrap()
            .walls
            .get(GridDirection::South)
            .is_empty());
        assert!(grid
            .sector(x, 0)
            .unwrap()
            .walls
            .get(GridDirection::North)
            .is_empty());
        assert!(!grid
            .sector(x, grid.depth - 1)
            .unwrap()
            .walls
            .get(GridDirection::North)
            .is_empty());
        assert!(grid
            .sector(x, grid.depth - 1)
            .unwrap()
            .walls
            .get(GridDirection::South)
            .is_empty());
    }
    let south_wall = grid
        .sector(0, 0)
        .unwrap()
        .walls
        .get(GridDirection::South)
        .first()
        .unwrap();
    assert_eq!(
        south_wall.heights,
        [0, 0, default_wall_height, default_wall_height]
    );
}

#[test]
fn editor_to_room_local_round_trip_origin_zero() {
    let grid = WorldGrid::stone_room(3, 3, 1024, None, None);
    for editor in [[0.0_f32, 0.0], [1.5, -0.25], [-1.4, 1.49]] {
        let world = grid.editor_to_room_local(editor);
        let back = grid.room_local_to_editor(world);
        assert!(
            (back[0] - editor[0]).abs() < 1e-3,
            "x: {editor:?} → {back:?}"
        );
        assert!(
            (back[1] - editor[1]).abs() < 1e-3,
            "z: {editor:?} → {back:?}"
        );
    }
}

#[test]
fn editor_to_room_local_round_trip_negative_origin() {
    let mut grid = WorldGrid::stone_room(3, 3, 1024, None, None);
    // Force a -2/-3 origin via the public grow path so the
    // test shape matches what auto-grow actually produces.
    grid.extend_to_include(-2, -3);
    assert_eq!(grid.origin, [-2, -3]);

    for editor in [[0.0_f32, 0.0], [2.0, -1.25], [-3.5, 1.0]] {
        let world = grid.editor_to_room_local(editor);
        let back = grid.room_local_to_editor(world);
        assert!(
            (back[0] - editor[0]).abs() < 1e-3,
            "x: {editor:?} → {back:?}"
        );
        assert!(
            (back[1] - editor[1]).abs() < 1e-3,
            "z: {editor:?} → {back:?}"
        );
    }
}

#[test]
fn editor_cells_to_array_resolves_to_correct_cell() {
    // Plain 3×3, origin [0, 0]: editor (0, 0) is room centre,
    // which falls inside cell (1, 1).
    let grid = WorldGrid::stone_room(3, 3, 1024, None, None);
    assert_eq!(grid.editor_cells_to_array([0.0, 0.0]), Some((1, 1)));
    assert_eq!(grid.editor_cells_to_array([-1.4, -1.4]), Some((0, 0)));
    assert_eq!(grid.editor_cells_to_array([1.4, 1.4]), Some((2, 2)));
    // Past the room edge: out of range.
    assert_eq!(grid.editor_cells_to_array([-2.0, 0.0]), None);
}

#[test]
fn editor_cells_to_array_after_negative_grow_is_origin_aware() {
    // Negative-side grow: origin shifts but the previously-
    // existing cells must remain reachable from the same
    // editor coordinates. After `extend_to_include(-1, 0)` on a
    // 3×3 starter the room becomes width=4, depth=3, origin=[-1,0].
    // Old cell at world-cell (0, 0) is now at array (1, 0).
    let mut grid = WorldGrid::stone_room(3, 3, 1024, None, None);
    grid.extend_to_include(-1, 0);
    assert_eq!(grid.origin, [-1, 0]);
    assert_eq!(grid.width, 4);
    // grid_center_cells = [-1 + 2, 0 + 1.5] = [1.0, 1.5]; cell
    // (1, 0) has world-cell centre [0.5, 0.5], so editor centre
    // is [0.5 - 1.0, 0.5 - 1.5] = [-0.5, -1.0].
    assert_eq!(grid.editor_cells_to_array([-0.5, -1.0]), Some((1, 0)));
    // Newly-included cell at array (0, 0) -- world-cell (-1, 0),
    // editor centre [-0.5 - 1.0, -1.0] = [-1.5, -1.0].
    assert_eq!(grid.editor_cells_to_array([-1.5, -1.0]), Some((0, 0)));
}

#[test]
fn cell_center_world_in_editor_units_matches_helper() {
    let mut grid = WorldGrid::stone_room(4, 5, 1024, None, None);
    grid.extend_to_include(-2, -1);
    let s = grid.sector_size as f32;
    for (sx, sz) in [(0u16, 0u16), (1, 2), (3, 4)] {
        let world_centre = grid.cell_center_world(sx, sz);
        let editor = grid.world_cells_to_editor([world_centre[0] / s, world_centre[1] / s]);
        // Same cell via editor_cells_to_array should round-trip.
        assert_eq!(grid.editor_cells_to_array(editor), Some((sx, sz)));
    }
}

#[test]
fn authored_footprint_ignores_empty_allocation() {
    let mut grid = WorldGrid::empty(8, 6, 1024);
    let _ = grid.ensure_sector(0, 0);
    grid.set_floor(2, 1, 0, None);
    grid.add_wall(5, 4, GridDirection::North, 0, 1024, None);

    let footprint = grid.authored_footprint().expect("authored geometry");
    assert_eq!(
        footprint,
        WorldGridFootprint {
            x: 2,
            z: 1,
            width: 4,
            depth: 4,
        }
    );
    assert_eq!(grid.populated_sector_count(), 2);

    let budget = grid.authored_budget();
    assert_eq!(budget.width, 4);
    assert_eq!(budget.depth, 4);
    assert_eq!(budget.total_cells, 16);
    assert_eq!(budget.populated_cells, 2);
}

#[test]
fn authored_footprint_is_empty_after_last_face_is_deleted() {
    let mut grid = WorldGrid::empty(8, 6, 1024);
    grid.set_floor(2, 1, 0, None);

    grid.sector_mut(2, 1).expect("authored sector").floor = None;

    assert_eq!(grid.populated_sector_count(), 0);
    assert_eq!(grid.authored_footprint(), None);
    assert_eq!(grid.authored_budget(), WorldGridBudget::default());
}

#[test]
fn budget_empty_grid_reports_no_geometry() {
    let grid = WorldGrid::empty(3, 3, 1024);
    let b = grid.budget();
    assert_eq!(b.width, 3);
    assert_eq!(b.depth, 3);
    assert_eq!(b.total_cells, 9);
    assert_eq!(b.populated_cells, 0);
    assert_eq!(b.floors, 0);
    assert_eq!(b.ceilings, 0);
    assert_eq!(b.walls, 0);
    assert_eq!(b.triangles, 0);
    // AssetHeader + active WorldHeader + 9 sector records.
    // `.psxw` stores a record per cell whether populated or not.
    assert_eq!(
        b.psxw_bytes,
        12 + psxed_format::world::WorldHeader::SIZE + 9 * psxed_format::world::SectorRecord::SIZE
    );
    assert_eq!(b.static_light_table_bytes, 0);
    assert_eq!(b.psxw_static_lit_bytes, b.psxw_bytes);
    assert_eq!(
        b.future_compact_estimated_bytes,
        12 + psxed_format::world::WorldHeader::SIZE + 9 * 28
    );
    assert!(!b.over_budget());
    assert!(!b.static_lit_over_budget());
}

#[test]
fn budget_starter_room_matches_authored_geometry() {
    let grid = WorldGrid::stone_room(3, 3, 1024, None, None);
    let b = grid.budget();
    assert_eq!(b.populated_cells, 9);
    assert_eq!(b.floors, 9);
    assert_eq!(b.ceilings, 0);
    // Perimeter only: 4 sides * 3 cells = 12 walls.
    assert_eq!(b.walls, 12);
    // 2 tris per face: 9 floors + 12 walls = 21 faces.
    assert_eq!(b.triangles, 42);
    // The future compact estimate should be strictly smaller
    // than the active format once any geometry exists.
    assert!(b.future_compact_estimated_bytes < b.psxw_bytes);
    assert_eq!(
        b.static_light_table_bytes,
        (9 * 2 + 12) * psxed_format::world::SurfaceLightRecord::SIZE
    );
    assert_eq!(
        b.psxw_static_lit_bytes,
        b.psxw_bytes + b.static_light_table_bytes
    );
    assert!(!b.over_budget());
    assert!(!b.static_lit_over_budget());
}

#[test]
fn budget_counts_generated_floor_transition_walls() {
    let mut grid = WorldGrid::empty(2, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    grid.set_floor(1, 0, 512, None);

    let b = grid.budget();

    assert_eq!(b.floors, 2);
    assert_eq!(b.walls, 1);
    assert_eq!(b.triangles, 6);
}

#[test]
fn budget_max_dimension_grid_within_caps() {
    // Floors-only at MAX_ROOM_WIDTH × MAX_ROOM_DEPTH = 32 × 32.
    // Stresses the byte-cap path without going over MAX_ROOM_TRIANGLES.
    let mut grid = WorldGrid::empty(MAX_ROOM_WIDTH, MAX_ROOM_DEPTH, 1024);
    for x in 0..MAX_ROOM_WIDTH {
        for z in 0..MAX_ROOM_DEPTH {
            grid.set_floor(x, z, 0, None);
        }
    }
    let b = grid.budget();
    assert_eq!(b.populated_cells, 1024);
    assert_eq!(b.floors, 1024);
    assert_eq!(b.triangles, 2048);
    assert!(b.triangles <= MAX_ROOM_TRIANGLES);
    // Active format remains under the byte cap for floors-only;
    // the wall-stack-heavy worst case is what pushes rooms over.
    assert!(b.psxw_bytes <= MAX_ROOM_BYTES);
    assert!(b.psxw_static_lit_bytes > MAX_ROOM_BYTES);
    assert!(b.future_compact_estimated_bytes <= MAX_ROOM_BYTES);
    assert!(!b.over_budget());
    assert!(b.static_lit_over_budget());
}

#[test]
fn budget_flags_oversized_room_dimensions() {
    // 64×16 fits the byte cap but blows past MAX_ROOM_WIDTH.
    // The old `over_budget` check only watched triangles +
    // bytes; this test pins the new width/depth check that
    // catches asymmetric over-sized rooms.
    let grid = WorldGrid::empty(MAX_ROOM_WIDTH * 2, MAX_ROOM_DEPTH / 2, 1024);
    let b = grid.budget();
    assert!(b.over_budget(), "{b:?}");
}

#[test]
fn extend_to_include_grows_positively_without_shift() {
    let mut grid = WorldGrid::stone_room(3, 3, 1024, None, None);
    let baseline_floor_world = grid.cell_world_x(0); // 0
    let cell = grid.extend_to_include(5, 1);
    assert_eq!(cell, (5, 1));
    assert_eq!(grid.width, 6);
    assert_eq!(grid.depth, 3);
    assert_eq!(grid.origin, [0, 0]);
    // Old (0, 0) data still at array (0, 0), still at world 0.
    assert_eq!(grid.cell_world_x(0), baseline_floor_world);
    assert!(grid.sector(0, 0).is_some());
}

#[test]
fn extend_to_include_grows_negatively_preserving_world_position() {
    let mut grid = WorldGrid::stone_room(3, 3, 1024, None, None);
    let cell = grid.extend_to_include(-2, 0);
    assert_eq!(cell, (0, 0));
    // Two new columns prepended in -X.
    assert_eq!(grid.width, 5);
    assert_eq!(grid.origin[0], -2);
    // Old (0, 0) data is now at array (2, 0), still at world 0.
    assert_eq!(grid.cell_world_x(2), 0);
    assert!(grid.sector(2, 0).is_some());
    // The newly-included cell at array (0, 0) is empty.
    assert!(grid.sector(0, 0).is_none());
}

#[test]
fn embedded_default_project_ron_deserializes() {
    let project = ProjectDocument::from_ron_str(DEFAULT_PROJECT_RON).unwrap();
    let material_path = |r: &Resource| match &r.data {
        ResourceData::Material(material) => material.psxt_path.clone(),
        _ => None,
    };
    assert!(project
        .resources
        .iter()
        .any(|r| material_path(r).is_some_and(|p| p.ends_with("courtyard_cobbles.psxt"))));
    assert!(project
        .resources
        .iter()
        .any(|r| material_path(r).is_some_and(|p| p.ends_with("sanctum_masonry.psxt"))));
    assert!(!project.resources.iter().any(|r| material_path(r)
        .is_some_and(|p| p.ends_with("floor.psxt") || p.ends_with("brick-wall.psxt"))));
    // The legacy Texture resource kind is fully folded at load.
    assert!(!project
        .resources
        .iter()
        .any(|r| matches!(&r.data, ResourceData::Texture { .. })));
    let starter_materials: Vec<&MaterialResource> = project
        .resources
        .iter()
        .filter_map(|resource| match &resource.data {
            ResourceData::Material(material) => Some(material),
            _ => None,
        })
        .collect();
    assert!(!starter_materials.is_empty());
    assert!(starter_materials.iter().all(|material| {
        material.face_sidedness == MaterialFaceSidedness::Front && !material.double_sided
    }));
    // Starter seeds the active player with the VERIFIED cortex_v1 combat
    // loadout: the ci_player model, the Aletha Complete Animation Set (the
    // clips the measured attack windows were authored against), and the
    // measured combat capsules. The Bonnie AI / Aletha-uthana resources stay
    // in the project for experiments, but the starter profile must stay on
    // catalogue-synced content or it dangles in freshly synced projects.
    // Resolve the character through the wired player controller rather than
    // assuming a particular resource id.
    // The starter player is an Entity with a player-controlled
    // CharacterController (the same form the tech demo uses), so the
    // renderer component can set the presentation scale.
    let character_id = project
        .active_scene()
        .nodes()
        .iter()
        .find_map(|node| match &node.kind {
            NodeKind::CharacterController {
                player: true,
                character,
                ..
            } => *character,
            _ => None,
        })
        .expect("starter scene wires a player controller to a character");
    let character = project
        .resource(character_id)
        .and_then(|resource| match &resource.data {
            ResourceData::Character(character) => Some(character),
            _ => None,
        })
        .expect("starter player character resource missing");
    let model_id = character.model.expect("starter character has a model");
    let model = project
        .resource(model_id)
        .and_then(|resource| match &resource.data {
            ResourceData::Model(model) => Some(model),
            _ => None,
        })
        .expect("starter player model resource missing");
    assert!(model
        .model_path
        .ends_with("aletha_delivered/aletha_delivered.psxmdl"));
    assert!(model
        .texture_path
        .as_deref()
        .is_some_and(|path| path.ends_with("aletha_delivered.psxt")));
    assert!(model.skeleton.is_some());
    assert_eq!(
        model.collision_radius,
        default_model_collision_radius_for_height(model.world_height)
    );
    assert_eq!(model.scale_q8, [MODEL_SCALE_ONE_Q8; 3]);
    assert!(
        model
            .attachments
            .iter()
            .any(|socket| socket.name == "right_hand_grip" && socket.joint == 13),
        "starter player model must expose the verified weapon socket"
    );
    assert_eq!(
        character.combat_capsules.len(),
        4,
        "starter Aletha carries the verified hurtbox plus three attack capsules"
    );

    let animation_set_id = character
        .animation_set
        .expect("starter character has an animation set");
    let animation_set_resource = project
        .resource(animation_set_id)
        .expect("starter animation set resource missing");
    assert_eq!(
        animation_set_resource.name,
        "Aletha Delivered Animation Set"
    );
    let ResourceData::AnimationSet(animation_set) = &animation_set_resource.data else {
        panic!("starter animation set has the wrong resource kind");
    };
    // Walk is the approved generated gait (MoMask candidate C, cooked by
    // import-locomotion); the rest are the artist moveset's native takes.
    let walk_clip = animation_set
        .action_clip(CharacterAnimationAction::Walk)
        .expect("starter animation set is missing Walk");
    let walk = project
        .resource(walk_clip)
        .and_then(|resource| match &resource.data {
            ResourceData::AnimationClip(clip) => Some(clip),
            _ => None,
        })
        .expect("starter Walk clip resource missing");
    assert_eq!(walk.psxanim_path, "assets/animations/gen/walk_fwd.psxanim");
    let run = animation_set
        .action_clip(CharacterAnimationAction::Run)
        .and_then(|id| project.resource(id))
        .and_then(|resource| match &resource.data {
            ResourceData::AnimationClip(clip) => Some(clip),
            _ => None,
        })
        .expect("starter Run clip resource missing");
    assert_eq!(run.psxanim_path, "assets/animations/gen/run_fwd.psxanim");
    // The whole locked set is baked into the gen pack (the strafes are the
    // artist's takes, un-turned by the study's face-forward pass).
    for (action, stem) in [
        (CharacterAnimationAction::WalkBackward, "walk_bwd"),
        (CharacterAnimationAction::StrafeLeft, "walk_lft"),
        (CharacterAnimationAction::StrafeRight, "walk_rgt"),
        // Combat is the recorded set too: the three horizontal levels, the
        // vertical axis's first level, and the hit reaction.
        (CharacterAnimationAction::LightAttack, "light_attack"),
        (CharacterAnimationAction::HeavyAttack, "heavy_attack"),
        (CharacterAnimationAction::ComboAttack, "combo_attack"),
        (CharacterAnimationAction::VertLightAttack, "vert_light_attack"),
        (CharacterAnimationAction::HitReact, "hit_react"),
    ] {
        let clip = animation_set
            .action_clip(action)
            .and_then(|id| project.resource(id))
            .and_then(|resource| match &resource.data {
                ResourceData::AnimationClip(clip) => Some(clip),
                _ => None,
            })
            .unwrap_or_else(|| panic!("starter {action:?} clip resource missing"));
        assert_eq!(clip.psxanim_path, format!("assets/animations/gen/{stem}.psxanim"));
    }
    for (action, stem) in [
        (CharacterAnimationAction::Idle, "aletha_idle"),
        (CharacterAnimationAction::Roll, "aletha_dash_fwd"),
        (CharacterAnimationAction::Death, "aletha_death"),
        // The delivered heavy-weapon set keeps the alternate slots; the
        // vertical axis has its own actions and does not touch them.
        (
            CharacterAnimationAction::AltLightAttack,
            "aletha_heavy_wpn_light_atk_a",
        ),
        (
            CharacterAnimationAction::AltHeavyAttack,
            "aletha_heavy_wpn_heavy_atk",
        ),
    ] {
        let clip_id = animation_set
            .action_clip(action)
            .unwrap_or_else(|| panic!("starter animation set is missing {action:?}"));
        let clip = project
            .resource(clip_id)
            .and_then(|resource| match &resource.data {
                ResourceData::AnimationClip(clip) => Some(clip),
                _ => None,
            })
            .unwrap_or_else(|| panic!("starter {action:?} clip resource missing"));
        assert_eq!(
            clip.psxanim_path,
            format!("assets/animations/aletha_delivered/{stem}.psxanim")
        );
        // Native clips may pin their model or stay skeleton-shared (None);
        // either resolves to the starter model on this skeleton.
        assert!(clip.target_model.is_none_or(|model| model == model_id));
        assert_eq!(clip.skeleton, model.skeleton);
    }
}

#[test]
fn legacy_world_and_actor_project_ron_migrates_to_world_sector_and_entity() {
    fn replace_first_world_payload(source: &str) -> String {
        let start = source
            .find("kind: World(")
            .expect("default fixture has a parameterised World kind");
        let payload_start = start + "kind: World".len();
        let mut depth = 0i32;
        for (offset, ch) in source[payload_start..].char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        let end = payload_start + offset + ch.len_utf8();
                        return format!("{}kind: World{}", &source[..start], &source[end..]);
                    }
                }
                _ => {}
            }
        }
        panic!("unterminated World payload");
    }

    let starter = ProjectDocument::from_ron_str(crate::LEGACY_GRID_STARTER_RON).unwrap();
    assert!(starter
        .active_scene()
        .nodes()
        .iter()
        .any(|node| matches!(node.kind, NodeKind::World { .. })));
    // Whichever Entity node comes first is the one the rewrite below
    // demotes to a legacy Actor, so capture its name rather than assuming
    // the starter actor is still called "Player".
    let demoted_name = starter
        .active_scene()
        .nodes()
        .iter()
        .find(|node| matches!(node.kind, NodeKind::Entity))
        .map(|node| node.name.clone())
        .expect("starter has an entity node");
    let legacy = replace_first_world_payload(crate::LEGACY_GRID_STARTER_RON).replacen(
        "kind: Entity,",
        "kind: Actor,",
        1,
    );

    let project = ProjectDocument::from_ron_str(&legacy).unwrap();
    let scene = project.active_scene();
    let root = scene.node(scene.root).expect("world root exists");
    assert!(matches!(root.kind, NodeKind::World { .. }));
    assert_eq!(root.name, "World");
    assert!(scene.nodes().iter().all(|node| node.name != "Root"));
    let world = scene
        .nodes()
        .iter()
        .find(|node| node.name == "World")
        .expect("starter world exists");
    assert!(matches!(
        &world.kind,
        NodeKind::World { sector_size, .. } if *sector_size == DEFAULT_WORLD_SECTOR_SIZE
    ));
    let migrated = scene
        .nodes()
        .iter()
        .find(|node| node.name == demoted_name)
        .expect("starter player entity exists");
    assert!(matches!(&migrated.kind, NodeKind::Entity));
}

#[test]
fn starter_model_files_present_on_disk() {
    let root = legacy_grid_starter_dir();
    assert!(root
        .join("assets/models/crimson_cross_knight/crimson_cross_knight.psxmdl")
        .is_file());
    assert!(root
        .join("assets/models/crimson_cross_knight/crimson_cross_knight.psxt")
        .is_file());
    assert!(root
            .join("assets/models/crimson_cross_knight/crimson_cross_knight_armature_idle_03_baselayer.psxanim")
            .is_file());
    assert!(root
        .join("assets/models/ci_player/ci_player.psxmdl")
        .is_file());
    assert!(root
        .join("assets/models/rust_mantis/rust_mantis.psxmdl")
        .is_file());
    assert!(root
        .join("assets/animations/ci_player_complete/roll.psxanim")
        .is_file());
    assert!(root
        .join("assets/animations/rust_mantis_starter/idle.psxanim")
        .is_file());
}

#[test]
fn projects_dir_resolves_to_real_directory() {
    assert!(projects_dir().is_dir(), "{}", projects_dir().display());
    assert!(default_project_dir().join("project.ron").is_file());
    assert!(default_project_dir()
        .join("assets/textures/courtyard_cobbles.psxt")
        .is_file());
    assert!(default_project_dir()
        .join("assets/models/rust_mantis/rust_mantis.psxmdl")
        .is_file());
    assert!(legacy_grid_starter_dir()
        .join("assets/models/crimson_cross_knight/crimson_cross_knight.psxmdl")
        .is_file());
}

#[test]
fn project_file_stem_is_filesystem_safe() {
    assert_eq!(
        project_file_stem("Stone Room: Vertical Slice!"),
        "stone_room_vertical_slice"
    );
    assert_eq!(project_file_stem("PSoXide 2"), "psoxide_2");
    assert_eq!(project_file_stem("..."), "project");
}

#[test]
fn starter_project_has_scene_tree_and_resources() {
    let project = ProjectDocument::legacy_grid_starter();

    assert_eq!(project.scenes.len(), 1);
    // Starter includes a room texture/material set plus gameplay resources for
    // the animated character and weapon path.
    assert!(project.resources.len() >= 10);
    assert!(project
        .active_scene()
        .hierarchy_rows()
        .iter()
        .any(|row| row.kind == "Section"));
    let grid = project
        .active_scene()
        .nodes()
        .iter()
        .find_map(|node| match &node.kind {
            NodeKind::Section { grid } => Some(grid),
            _ => None,
        })
        .expect("starter should contain a room node");
    assert!(grid.width > 0);
    assert!(grid.depth > 0);
    assert_eq!(
        grid.sectors.len(),
        grid.width as usize * grid.depth as usize
    );
    assert!(grid.populated_sector_count() > 0);

    let aletha = project
        .resources
        .iter()
        .find_map(|resource| match &resource.data {
            ResourceData::Character(character) if resource.name == "Aletha" => Some(character),
            _ => None,
        })
        .expect("starter includes Aletha");
    assert_eq!(aletha.spawn_role, CharacterSpawnRole::Player);
    assert_eq!(
        (aletha.radius, aletha.walk_speed, aletha.run_speed),
        (188, 44, 94)
    );
    assert_eq!(aletha.roll_speed, 165);
    let aletha_material = aletha
        .material
        .expect("Aletha carries her crystal material");
    let material_resource = project
        .resource(aletha_material)
        .expect("Aletha material exists");
    assert_eq!(material_resource.name, "Aletha Crystal");
    assert!(matches!(material_resource.data, ResourceData::Material(_)));
    assert_eq!(
        (
            aletha.camera_distance,
            aletha.camera_height,
            aletha.camera_target_height,
        ),
        (3300, 1500, 900)
    );

    let mantis = project
        .resources
        .iter()
        .find_map(|resource| match &resource.data {
            ResourceData::Character(character) if resource.name == "Rust Mantis Enemy" => {
                Some(character)
            }
            _ => None,
        })
        .expect("starter includes the Rust Mantis enemy");
    assert_eq!(mantis.spawn_role, CharacterSpawnRole::Enemy);
    assert_eq!(mantis.walk_speed, 28);
    let enemy = mantis.enemy_behavior.expect("Mantis enemy behavior preset");
    assert_eq!(enemy.aggro_radius, 2335);
    assert_eq!(enemy.patrol_offset, [0, 0, -6000]);
    assert_eq!(enemy.reaction_ticks, 22);

    for name in [
        "Obsidian Wraith Enemy",
        "Hooded Wretch Enemy",
        "Crowned Wraith Enemy",
    ] {
        let character = project
            .resources
            .iter()
            .find_map(|resource| match &resource.data {
                ResourceData::Character(character) if resource.name == name => Some(character),
                _ => None,
            })
            .unwrap_or_else(|| panic!("starter includes {name}"));
        assert_eq!(character.spawn_role, CharacterSpawnRole::Enemy, "{name}");
        assert_eq!(character.walk_speed, 28, "{name}");
        assert_eq!(
            character.enemy_behavior.map(|enemy| enemy.aggro_radius),
            Some(2335),
            "{name}"
        );
    }
}

#[test]
fn project_missing_point_light_color_and_room_ambient_uses_defaults() {
    // Serde defaults contract, tested at the field level: a PointLight
    // authored without a color, and a room grid authored without
    // ambient/fog fields, deserialize to the documented defaults. The
    // minimal starter ships neither, so the fields are exercised on
    // round-tripped values with the lines stripped rather than on the
    // starter document.
    let light: NodeKind =
        ron::from_str("PointLight(intensity: 1.25, radius: 3.0)").expect("light parses");
    let NodeKind::PointLight { color, .. } = light else {
        panic!("parsed kind is a light");
    };
    assert_eq!(color, default_light_color());

    let grid = WorldGrid::empty(1, 1, DEFAULT_WORLD_SECTOR_SIZE);
    let source = ron::ser::to_string_pretty(&grid, ron::ser::PrettyConfig::default())
        .expect("grid serializes");
    let stripped: String = source
        .lines()
        .filter(|line| {
            let t = line.trim_start();
            !(t.starts_with("ambient_color:")
                || t.starts_with("fog_color:")
                || t.starts_with("fog_near:")
                || t.starts_with("fog_far:"))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let grid: WorldGrid = ron::from_str(&stripped).expect("stripped grid parses");
    assert_eq!(grid.ambient_color, default_ambient_color());
    assert_eq!(
        (grid.fog_color, grid.fog_near, grid.fog_far),
        (default_fog_color(), default_fog_near(), default_fog_far())
    );
}

#[test]
fn legacy_door_motion_uses_brush_defaults() {
    let door: LogicNodeKind = ron::from_str("Door(box_prop: \"Legacy Box\", start_open: true)")
        .expect("legacy door parses");
    assert_eq!(
        door,
        LogicNodeKind::Door {
            box_prop: "Legacy Box".to_string(),
            start_open: true,
            open_offset: default_brush_door_open_offset(),
            travel_ticks: default_brush_door_travel_ticks(),
        }
    );
}

#[test]
fn adding_node_preserves_parent_child_relationship() {
    let mut scene = Scene::new("Test");

    let room = scene.add_node(
        scene.root,
        "Room",
        NodeKind::Section {
            grid: WorldGrid::empty(2, 2, 1024),
        },
    );
    let child = scene.add_node(
        room,
        "Spawn",
        NodeKind::SpawnPoint {
            player: true,
            character: None,
        },
    );

    assert_eq!(scene.node(child).and_then(|node| node.parent), Some(room));
    assert!(scene
        .node(room)
        .is_some_and(|node| node.children.contains(&child)));
}

#[test]
fn removing_node_removes_descendants() {
    let mut scene = Scene::new("Test");
    let parent = scene.add_node(scene.root, "A", NodeKind::Node3D);
    let child = scene.add_node(parent, "B", NodeKind::Node3D);

    assert!(scene.remove_node(parent));
    assert!(scene.node(parent).is_none());
    assert!(scene.node(child).is_none());
    assert!(scene
        .node(scene.root)
        .is_some_and(|root| root.children.is_empty()));
}

#[test]
fn move_node_reparents_and_reorders() {
    let mut scene = Scene::new("Test");
    let a = scene.add_node(scene.root, "A", NodeKind::Node3D);
    let b = scene.add_node(scene.root, "B", NodeKind::Node3D);
    let c = scene.add_node(a, "C", NodeKind::Node3D);

    // Reparent c from a to b at position 0.
    assert!(scene.move_node(c, b, 0));
    assert_eq!(scene.node(c).unwrap().parent, Some(b));
    assert!(scene.node(a).unwrap().children.is_empty());
    assert_eq!(scene.node(b).unwrap().children, vec![c]);

    // Reorder b before a at the root.
    assert!(scene.move_node(b, scene.root, 0));
    assert_eq!(scene.node(scene.root).unwrap().children, vec![b, a]);
}

#[test]
fn move_node_rejects_cycles_and_root() {
    let mut scene = Scene::new("Test");
    let a = scene.add_node(scene.root, "A", NodeKind::Node3D);
    let b = scene.add_node(a, "B", NodeKind::Node3D);

    // Cannot reparent a node under itself.
    assert!(!scene.move_node(a, a, 0));
    // Cannot reparent an ancestor under its descendant.
    assert!(!scene.move_node(a, b, 0));
    // Cannot move the root.
    assert!(!scene.move_node(scene.root, a, 0));
}

#[test]
fn project_roundtrips_through_ron_string() {
    let project = ProjectDocument::legacy_grid_starter();
    let ron = project.to_ron_string().unwrap();

    assert!(ron.contains("Crimson Cross Knight Player"));
    assert_eq!(ProjectDocument::from_ron_str(&ron).unwrap(), project);
}

#[test]
fn new_project_seeds_hud_ui_scene() {
    let project = ProjectDocument::new("ui");
    let ui_scene = project.active_ui_scene().expect("default UI scene");
    assert_eq!(ui_scene.name, "HUD");
    assert!(matches!(
        ui_scene.node(ui_scene.root).map(|node| &node.kind),
        Some(UiNodeKind::Canvas {
            width: 320,
            height: 240
        })
    ));
    assert!(ui_scene.nodes().iter().any(|node| {
        node.name == "Health Bar"
            && matches!(
                node.kind,
                UiNodeKind::Bar {
                    value: UiValueBinding::PlayerHealth,
                    max: UiValueBinding::PlayerHealthMax,
                    ..
                }
            )
    }));
    assert!(ui_scene.nodes().iter().any(|node| {
        node.name == "Stamina Bar"
            && matches!(
                node.kind,
                UiNodeKind::Bar {
                    value: UiValueBinding::PlayerStamina,
                    max: UiValueBinding::PlayerStaminaMax,
                    ..
                }
            )
    }));
}

#[test]
fn normalize_loaded_restores_missing_ui_scenes() {
    let mut project = ProjectDocument::new("legacy");
    project.ui_scenes.clear();
    project.scene_states.clear();
    project.normalize_loaded();
    assert_eq!(project.active_ui_scene().unwrap().name, "HUD");
    assert!(project
        .scene_states
        .iter()
        .any(|state| state.world == SceneWorldLayer::Gameplay));
}

#[test]
fn normalize_loaded_creates_screen_states_for_ui_scenes() {
    let mut project = ProjectDocument::new("states");
    project.add_ui_scene("Menu");
    for state in &mut project.scene_states {
        state.id = SceneStateId::UNASSIGNED;
    }
    project.normalize_loaded();

    for scene in &project.ui_scenes {
        assert!(
            project
                .scene_states
                .iter()
                .any(|state| state.ui_scene == Some(scene.id)),
            "missing state for UI scene {}",
            scene.name
        );
    }
    assert!(project
        .scene_states
        .iter()
        .any(|state| state.world == SceneWorldLayer::Gameplay));
    let ids: HashSet<SceneStateId> = project.scene_states.iter().map(|state| state.id).collect();
    assert_eq!(ids.len(), project.scene_states.len());
    assert!(ids.iter().all(|id| *id != SceneStateId::UNASSIGNED));
}

#[test]
fn normalize_loaded_assigns_stable_unique_ui_scene_ids() {
    let mut project = ProjectDocument::new("ids");
    // Simulate a legacy project: scenes with the unassigned sentinel.
    let mut second = UiScene::default_hud();
    second.name = "Pause".to_string();
    second.id = UiSceneId::UNASSIGNED;
    project.ui_scenes.push(second);
    for scene in &mut project.ui_scenes {
        scene.id = UiSceneId::UNASSIGNED;
    }

    project.normalize_loaded();

    let ids: Vec<UiSceneId> = project.ui_scenes.iter().map(|scene| scene.id).collect();
    assert!(ids.iter().all(|id| *id != UiSceneId::UNASSIGNED));
    let unique: HashSet<UiSceneId> = ids.iter().copied().collect();
    assert_eq!(unique.len(), ids.len(), "scene ids must be unique");

    // Ids are stable across a second normalize and addressable.
    let first_id = project.ui_scenes[0].id;
    project.normalize_loaded();
    assert_eq!(project.ui_scenes[0].id, first_id);
    assert_eq!(
        project.ui_scene(first_id).map(|scene| scene.name.as_str()),
        Some(project.ui_scenes[0].name.as_str())
    );
    assert_eq!(
        project.ui_scene_at(1).map(|scene| scene.id),
        Some(project.ui_scenes[1].id)
    );
}

#[test]
fn ui_scene_id_survives_ron_roundtrip() {
    let mut project = ProjectDocument::new("ids");
    project.normalize_loaded();
    let assigned = project.ui_scenes[0].id;
    let ron = project.to_ron_string().unwrap();
    let mut reloaded = ProjectDocument::from_ron_str(&ron).unwrap();
    reloaded.normalize_loaded();
    assert_eq!(reloaded.ui_scenes[0].id, assigned);
}

#[test]
fn add_ui_scene_seeds_empty_canvas_with_fresh_id() {
    let mut project = ProjectDocument::new("crud");
    project.normalize_loaded();
    let existing: HashSet<UiSceneId> = project.ui_scenes.iter().map(|scene| scene.id).collect();

    let id = project.add_ui_scene("Pause");
    assert!(id != UiSceneId::UNASSIGNED);
    assert!(!existing.contains(&id), "new scene id is unique");

    let scene = project.ui_scene(id).unwrap();
    assert_eq!(scene.name, "Pause");
    assert!(project
        .scene_states
        .iter()
        .any(|state| state.ui_scene == Some(id)));
    // Empty root canvas at PSX resolution: exactly one node, a Canvas.
    assert_eq!(scene.nodes().len(), 1);
    assert_eq!(scene.root, UiNodeId::ROOT);
    let root = scene.node(scene.root).unwrap();
    assert!(matches!(
        root.kind,
        UiNodeKind::Canvas {
            width: 320,
            height: 240
        }
    ));
}

#[test]
fn duplicate_ui_scene_deep_copies_after_source_with_fresh_id() {
    let mut project = ProjectDocument::new("crud");
    project.normalize_loaded();
    // Give the source an extra node so the deep copy is observable.
    let source_index = 0;
    let source_root = project.ui_scenes[source_index].root;
    let extra = project.ui_scenes[source_index].add_node(
        source_root,
        "Extra",
        UiNodeKind::Rect {
            rect: UiRect::new(1, 2, 3, 4),
            color: [9, 9, 9],
            gradient: None,
        },
    );
    let source_id = project.ui_scenes[source_index].id;
    let source_name = project.ui_scenes[source_index].name.clone();
    let source_node_count = project.ui_scenes[source_index].nodes().len();

    let copy_id = project.duplicate_ui_scene(source_index).unwrap();
    // Inserted directly after the source.
    assert_eq!(project.ui_scenes[source_index + 1].id, copy_id);
    assert_ne!(copy_id, source_id, "copy gets a fresh id");
    let copy = project.ui_scene(copy_id).unwrap();
    assert_eq!(copy.name, format!("{source_name} Copy"));
    assert_eq!(copy.nodes().len(), source_node_count);
    assert!(copy.node(extra).is_some(), "deep copy carries child nodes");

    // Editing the copy does not touch the source.
    let copy_index = source_index + 1;
    project.ui_scenes[copy_index].remove_node(extra);
    assert!(project.ui_scene(source_id).unwrap().node(extra).is_some());
}

#[test]
fn remove_ui_scene_never_leaves_list_empty() {
    let mut project = ProjectDocument::new("crud");
    project.normalize_loaded();
    project.add_ui_scene("Second");
    assert_eq!(project.ui_scenes.len(), 2);

    assert!(project.remove_ui_scene(0));
    assert_eq!(project.ui_scenes.len(), 1);

    // Removing the final scene re-seeds a default so the list is
    // never empty, and out-of-range indices are a no-op.
    assert!(project.remove_ui_scene(0));
    assert_eq!(project.ui_scenes.len(), 1, "list re-seeds a default HUD");
    assert!(project.ui_scenes[0].id != UiSceneId::UNASSIGNED);
    assert!(!project.remove_ui_scene(9));
    assert_eq!(project.ui_scenes.len(), 1);
}

#[test]
fn ui_scene_remove_node_removes_descendants_and_root_is_stable() {
    let mut scene = UiScene::default_hud();
    let group = scene.add_node(
        scene.root,
        "Prompt",
        UiNodeKind::Group {
            rect: UiRect::new(48, 180, 120, 24),
        },
    );
    let label = scene.add_node(
        group,
        "Prompt Text",
        UiNodeKind::Label {
            rect: UiRect::new(52, 184, 96, 12),
            text: "Open".to_string(),
            random_message: false,
            messages: Vec::new(),
            tag: String::new(),
            align: UiTextAlign::Left,
            wrap: false,
            font: UiFontChoice::Basic,
            font_scale: default_ui_font_scale(),
            letter_spacing: default_ui_letter_spacing(),
            color: [220, 226, 240],
            gradient: None,
            effect: UiImageEffect::None,
        },
    );

    assert!(!scene.remove_node(scene.root));
    assert!(scene.remove_node(group));
    assert!(scene.node(group).is_none());
    assert!(scene.node(label).is_none());
    assert!(!scene
        .node(scene.root)
        .expect("root")
        .children
        .contains(&group));
}

#[test]
fn ui_scene_parent_rect_offsets_children() {
    let mut scene = UiScene::default_hud();
    let group = scene.add_node(
        scene.root,
        "Panel",
        UiNodeKind::Group {
            rect: UiRect::new(40, 30, 100, 50),
        },
    );
    let label = scene.add_node(
        group,
        "Prompt",
        UiNodeKind::Label {
            rect: UiRect::new(8, 6, 48, 12),
            text: "Open".to_string(),
            random_message: false,
            messages: Vec::new(),
            tag: String::new(),
            align: UiTextAlign::Left,
            wrap: false,
            font: UiFontChoice::Basic,
            font_scale: default_ui_font_scale(),
            letter_spacing: default_ui_letter_spacing(),
            color: [220, 226, 240],
            gradient: None,
            effect: UiImageEffect::None,
        },
    );

    assert_eq!(
        scene.absolute_rect(label),
        Some(UiRect::new(48, 36, 48, 12))
    );
    assert_eq!(
        scene
            .hierarchy_node_ids()
            .into_iter()
            .filter(|id| *id == group || *id == label)
            .collect::<Vec<_>>(),
        vec![group, label]
    );
}

#[test]
fn ui_scene_absolute_rect_preserves_visual_transform() {
    let mut scene = UiScene::default_hud();
    let rect = UiRect::new(8, 6, 48, 12)
        .with_rotation(30)
        .with_flips(true, false);
    let label = scene.add_node(
        scene.root,
        "Prompt",
        UiNodeKind::Label {
            rect,
            text: "Open".to_string(),
            random_message: false,
            messages: Vec::new(),
            tag: String::new(),
            align: UiTextAlign::Left,
            wrap: false,
            font: UiFontChoice::Basic,
            font_scale: default_ui_font_scale(),
            letter_spacing: default_ui_letter_spacing(),
            color: [220, 226, 240],
            gradient: None,
            effect: UiImageEffect::None,
        },
    );

    let absolute = scene.absolute_rect(label).expect("absolute rect");
    assert_eq!(absolute.x, 8);
    assert_eq!(absolute.y, 6);
    assert_eq!(absolute.rotation_degrees, 30);
    assert!(absolute.flip_x);
    assert!(!absolute.flip_y);
}

#[test]
fn ui_scene_move_node_reparents_and_rejects_cycles() {
    let mut scene = UiScene::default_hud();
    let a = scene.add_node(
        scene.root,
        "A",
        UiNodeKind::Group {
            rect: UiRect::new(4, 5, 16, 16),
        },
    );
    let b = scene.add_node(
        scene.root,
        "B",
        UiNodeKind::Group {
            rect: UiRect::new(20, 30, 16, 16),
        },
    );
    let label = scene.add_node(
        a,
        "Label",
        UiNodeKind::Label {
            rect: UiRect::new(2, 3, 8, 8),
            text: "x".to_string(),
            random_message: false,
            messages: Vec::new(),
            tag: String::new(),
            align: UiTextAlign::Left,
            wrap: false,
            font: UiFontChoice::Basic,
            font_scale: default_ui_font_scale(),
            letter_spacing: default_ui_letter_spacing(),
            color: [255, 255, 255],
            gradient: None,
            effect: UiImageEffect::None,
        },
    );

    assert!(scene.move_node(label, b, 0));
    assert_eq!(scene.node(label).unwrap().parent, Some(b));
    assert_eq!(scene.absolute_rect(label), Some(UiRect::new(22, 33, 8, 8)));
    assert!(!scene.move_node(b, label, 0));
    assert!(!scene.move_node(scene.root, b, 0));
}

#[test]
fn ui_scene_paste_subtree_remaps_ids_and_preserves_children() {
    let mut source = UiScene::empty_canvas("Source", UiSceneId::FIRST);
    let group = source.add_node(
        source.root,
        "Panel",
        UiNodeKind::Group {
            rect: UiRect::new(10, 12, 80, 40),
        },
    );
    let child = source.add_node(
        group,
        "Child",
        UiNodeKind::Group {
            rect: UiRect::new(3, 4, 16, 8),
        },
    );
    let subtree = source.subtree_nodes(group).unwrap();

    let mut target = UiScene::empty_canvas("Target", UiSceneId(2));
    let parent = target.add_node(
        target.root,
        "Destination",
        UiNodeKind::Group {
            rect: UiRect::new(20, 30, 100, 50),
        },
    );
    let pasted = target.paste_subtree(parent, &subtree, group).unwrap();

    assert_ne!(pasted, group);
    let pasted_node = target.node(pasted).unwrap();
    assert_eq!(pasted_node.name, "Panel");
    assert_eq!(pasted_node.parent, Some(parent));
    assert_eq!(pasted_node.children.len(), 1);

    let pasted_child = pasted_node.children[0];
    assert_ne!(pasted_child, child);
    assert_eq!(target.node(pasted_child).unwrap().name, "Child");
    assert_eq!(target.node(pasted_child).unwrap().parent, Some(pasted));
    assert_eq!(
        target.absolute_rect(pasted_child),
        Some(UiRect::new(33, 46, 16, 8))
    );
}

#[test]
fn editor_camera_roundtrips_through_ron_string() {
    let mut project = ProjectDocument::new("camera");
    project.editor_camera = EditorCameraState {
        mode: EditorCameraMode::Free,
        orbit_yaw_q12: 384,
        orbit_pitch_q12: 4096 - 128,
        orbit_radius: 8192,
        orbit_target: [1024, 512, -2048],
        free_yaw_q12: 1536,
        free_pitch_q12: 128,
        free_position: [-300, 700, 900],
        free_initialized: true,
        zoom_speed: 1.6,
    };
    let ron = project.to_ron_string().unwrap();

    assert!(ron.contains("editor_camera"));
    assert_eq!(ProjectDocument::from_ron_str(&ron).unwrap(), project);
}

#[test]
fn editor_visibility_roundtrips_through_ron_string() {
    let mut project = ProjectDocument::new("visibility");
    project.editor_visibility = EditorVisibilityState {
        show_grid: false,
        show_portals: true,
        show_lights: false,
        preview_fog: false,
        preview_backface_wireframe: true,
        preview_bounds: false,
        show_play_debug_overlays: false,
        show_play_debug_map: true,
        show_brush_wireframes: true,
    };
    let ron = project.to_ron_string().unwrap();

    assert!(ron.contains("editor_visibility"));
    assert_eq!(ProjectDocument::from_ron_str(&ron).unwrap(), project);
}

#[test]
fn editor_workspace_roundtrips_through_ron_string() {
    let mut project = ProjectDocument::new("workspace");
    project.editor_workspace = EditorWorkspaceState {
        active: EditorWorkspaceView::Material,
    };
    let ron = project.to_ron_string().unwrap();

    assert!(ron.contains("editor_workspace"));
    assert_eq!(ProjectDocument::from_ron_str(&ron).unwrap(), project);
}

#[test]
fn editor_viewport_roundtrips_and_defaults_for_legacy_projects() {
    let mut project = ProjectDocument::new("viewport");
    project.editor_viewport = EditorViewportState {
        view_2d: true,
        orthographic_view: EditorOrthographicView::Front,
        orthographic_focus: [128.0, 256.0, -64.0],
        viewport_zoom: 48.0,
        snap_units: 32,
    };
    let ron = project.to_ron_string().unwrap();
    assert!(ron.contains("editor_viewport"));
    assert_eq!(ProjectDocument::from_ron_str(&ron).unwrap(), project);

    // A project saved before the field existed loads with defaults: the
    // struct fills every field from serde defaults (empty record), and
    // the ProjectDocument field itself is #[serde(default)].
    let legacy: EditorViewportState = ron::from_str("()").unwrap();
    assert_eq!(legacy, EditorViewportState::default());
}

#[test]
fn material_lab_recipe_roundtrips_through_ron_string() {
    let mut project = ProjectDocument::new("material-lab");
    let mut material = MaterialResource::translucent(None, PsxBlendMode::Average);
    material.texture_mode = MaterialTextureMode::Generated;
    material.generated = GeneratedMaterialTexture {
        size: 32,
        base_color: [16, 32, 48],
        noise_color: [200, 210, 220],
        noise_uv: GeneratedTextureUv {
            scale_u_q8: 384,
            scale_v_q8: 192,
            offset_u: -3,
            offset_v: 9,
            rotation_quarters: 3,
        },
        ..GeneratedMaterialTexture::default()
    };
    project.add_resource("Generated Glass", ResourceData::Material(material));

    let ron = project.to_ron_string().unwrap();
    assert!(ron.contains("texture_mode: Generated"));
    assert_eq!(ProjectDocument::from_ron_str(&ron).unwrap(), project);
}

#[test]
fn transition_material_recipe_roundtrips_through_ron_string() {
    let mut project = ProjectDocument::new("transition-material");
    let source_a = project.add_resource(
        "Sand",
        ResourceData::Material(MaterialResource::opaque(Some("sand.psxt".to_string()))),
    );
    let source_b = project.add_resource(
        "Stone",
        ResourceData::Material(MaterialResource::opaque(Some("stone.psxt".to_string()))),
    );
    let mut material = MaterialResource::opaque(None);
    material.texture_mode = MaterialTextureMode::Transition;
    material.transition = TransitionMaterialTexture {
        source_a: Some(source_a),
        source_b: Some(source_b),
        size: 128,
        coverage: 191,
        shape: TransitionMaskShape::Corner,
        rotation_quarters: 3,
        flip_x: true,
        flip_y: false,
        edge_breakup: 37,
        seed: 0x5eed_cafe,
        connected_edges: 0b1010,
    };
    project.add_resource("Sand over stone", ResourceData::Material(material));

    let ron = project.to_ron_string().unwrap();
    assert!(ron.contains("texture_mode: Transition"));
    assert!(ron.contains("shape: Corner"));
    assert!(ron.contains("connected_edges: 10"));
    assert_eq!(ProjectDocument::from_ron_str(&ron).unwrap(), project);
}

#[test]
fn runtime_depth_sort_mode_roundtrips_through_ron_string() {
    let mut project = ProjectDocument::new("depth-sort");
    project.runtime_depth_sort_mode = RuntimeDepthSortMode::HybridWalls;
    project.runtime_texture_split_mode = RuntimeTextureSplitMode::DepthSorted;
    project.runtime_room_draw_order_mode = RuntimeRoomDrawOrderMode::Portal;
    project.runtime_texture_split_max_edge = 96;
    let ron = project.to_ron_string().unwrap();

    assert!(ron.contains("runtime_depth_sort_mode"));
    assert!(ron.contains("runtime_texture_split_mode"));
    assert!(ron.contains("runtime_room_draw_order_mode"));
    assert!(ron.contains("runtime_texture_split_max_edge"));
    assert_eq!(ProjectDocument::from_ron_str(&ron).unwrap(), project);
}

#[test]
fn animation_library_resources_roundtrip_and_resolve_by_path() {
    let mut project = ProjectDocument::new("Animation Test");
    let skeleton = project.add_resource(
        "Humanoid Skeleton",
        ResourceData::Skeleton(SkeletonResource {
            joint_count: 2,
            parents: vec![None, Some(0)],
            signature: "psx-parent-v1:2:root,0".to_string(),
            note: "test skeleton".to_string(),
            joint_names: Vec::new(),
        }),
    );
    let idle_animation = project.add_resource(
        "Idle",
        ResourceData::AnimationClip(AnimationClipResource {
            psxanim_path: "assets/animations/idle.psxanim".to_string(),
            skeleton: Some(skeleton),
            target_model: None,
            source: None,
            bake: AnimationClipBakeKind::LegacyShared,
            role: AnimationRole::Idle,
            looping: true,
            tags: vec!["idle".to_string()],
            calibration: Default::default(),
            pose_corrections: Vec::new(),
        }),
    );
    let set = project.add_resource(
        "Humanoid Set",
        ResourceData::AnimationSet(AnimationSetResource {
            skeleton: Some(skeleton),
            idle_clip: Some(idle_animation),
            walk_clip: None,
            run_clip: None,
            turn_clip: None,
            roll_clip: None,
            backstep_clip: None,
            action_clips: Vec::new(),
            clips: Vec::new(),
        }),
    );
    let model = project.add_resource(
        "Humanoid Model",
        ResourceData::Model(ModelResource {
            model_path: "assets/models/humanoid.psxmdl".to_string(),
            source_path: None,
            texture_path: Some("assets/models/humanoid.psxt".to_string()),
            skeleton: Some(skeleton),
            world_height: 1024,
            collision_radius: default_model_collision_radius_for_height(1024),
            scale_q8: [MODEL_SCALE_ONE_Q8; 3],
            default_visual_yaw_q12: 0,
            attachments: Vec::new(),
        }),
    );
    project.add_resource(
        "Character",
        ResourceData::Character(CharacterResource {
            model: Some(model),
            animation_set: Some(set),
            ..CharacterResource::defaults()
        }),
    );

    let restored = ProjectDocument::from_ron_str(&project.to_ron_string().unwrap()).unwrap();
    assert_eq!(restored, project);
    assert_eq!(
        restored.resolved_model_animation_index(model, idle_animation),
        Some(0),
        "standalone clips matching legacy model-local paths resolve to the stable legacy index",
    );
}

#[test]
fn model_targeted_animation_clips_do_not_leak_across_shared_skeletons() {
    let mut project = ProjectDocument::new("Targeted Animation Test");
    let skeleton = project.add_resource(
        "Humanoid Skeleton",
        ResourceData::Skeleton(SkeletonResource {
            joint_count: 1,
            parents: vec![None],
            signature: "psx-parent-v1:1:root".to_string(),
            note: String::new(),
            joint_names: Vec::new(),
        }),
    );
    let make_model = |path: &str| ModelResource {
        model_path: path.to_string(),
        source_path: None,
        texture_path: None,
        skeleton: Some(skeleton),
        world_height: 1024,
        collision_radius: default_model_collision_radius_for_height(1024),
        scale_q8: [MODEL_SCALE_ONE_Q8; 3],
        default_visual_yaw_q12: 0,
        attachments: Vec::new(),
    };
    let model_a = project.add_resource(
        "Model A",
        ResourceData::Model(make_model("assets/models/a.psxmdl")),
    );
    let model_b = project.add_resource(
        "Model B",
        ResourceData::Model(make_model("assets/models/b.psxmdl")),
    );
    let make_clip = |path: &str, target_model| AnimationClipResource {
        psxanim_path: path.to_string(),
        skeleton: Some(skeleton),
        target_model,
        source: None,
        bake: if target_model.is_some() {
            AnimationClipBakeKind::Retargeted
        } else {
            AnimationClipBakeKind::LegacyShared
        },
        role: AnimationRole::Idle,
        looping: true,
        tags: Vec::new(),
        calibration: Default::default(),
        pose_corrections: Vec::new(),
    };
    let shared = project.add_resource(
        "Shared",
        ResourceData::AnimationClip(make_clip("assets/animations/shared.psxanim", None)),
    );
    let clip_a = project.add_resource(
        "A Idle",
        ResourceData::AnimationClip(make_clip("assets/animations/a_idle.psxanim", Some(model_a))),
    );
    let clip_b = project.add_resource(
        "B Idle",
        ResourceData::AnimationClip(make_clip("assets/animations/b_idle.psxanim", Some(model_b))),
    );

    let resolved_a = project.resolved_model_animation_clips(model_a);
    assert_eq!(
        resolved_a
            .iter()
            .filter_map(|clip| clip.animation_resource)
            .collect::<Vec<_>>(),
        vec![clip_a, shared],
    );
    assert_eq!(
        project.resolved_model_animation_index(model_a, clip_b),
        None
    );

    let resolved_b = project.resolved_model_animation_clips(model_b);
    assert_eq!(
        resolved_b
            .iter()
            .filter_map(|clip| clip.animation_resource)
            .collect::<Vec<_>>(),
        vec![clip_b, shared],
    );
    assert_eq!(
        project.resolved_model_animation_index(model_b, clip_a),
        None
    );

    let restored = ProjectDocument::from_ron_str(&project.to_ron_string().unwrap()).unwrap();
    assert_eq!(restored, project);

    let mut deleted = restored;
    deleted.delete_resource(model_a).unwrap();
    let ResourceData::AnimationClip(orphaned) = &deleted.resource(clip_a).unwrap().data else {
        unreachable!();
    };
    assert_eq!(orphaned.target_model, None);
    assert_eq!(orphaned.skeleton, None);
    assert_eq!(
        deleted
            .resolved_model_animation_clips(model_b)
            .iter()
            .filter_map(|clip| clip.animation_resource)
            .collect::<Vec<_>>(),
        vec![clip_b, shared],
    );
}

#[test]
fn mesh_instance_with_animation_clip_roundtrips() {
    let mut project = ProjectDocument::legacy_grid_starter();
    let scene = project.active_scene_mut();
    let room_id = scene
        .nodes()
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Section { .. }))
        .map(|n| n.id)
        .unwrap();
    let model_resource_id = ResourceId(99);
    scene.add_node(
        room_id,
        "TestWraith",
        NodeKind::MeshInstance {
            mesh: Some(model_resource_id),
            material: None,
            animation_clip: Some(2),
        },
    );
    let ron = project.to_ron_string().unwrap();
    let restored = ProjectDocument::from_ron_str(&ron).unwrap();
    assert_eq!(restored, project);
    // Confirm the new field survives.
    let surviving = restored
        .active_scene()
        .nodes()
        .iter()
        .find(|n| n.name == "TestWraith")
        .unwrap();
    assert!(matches!(
        surviving.kind,
        NodeKind::MeshInstance {
            mesh: Some(_),
            animation_clip: Some(2),
            ..
        }
    ));
}

#[test]
fn legacy_mesh_instance_without_animation_clip_loads() {
    // Synthesize the pre-extension MeshInstance shape -- `animation_clip`
    // missing -- and confirm `#[serde(default)]` lands `None`.
    let ron = r#"
            (
                name: "Legacy",
                next_resource_id: 1,
                resources: [],
                scenes: [
                    Scene(
                        name: "Demo",
                        next_node_id: 3,
                        root: NodeId(1),
                        nodes: [
                            (
                                id: NodeId(1),
                                name: "Root",
                                parent: None,
                                children: [NodeId(2)],
                                kind: Node3D,
                                transform: (translation: (0.0, 0.0, 0.0), rotation_degrees: (0.0, 0.0, 0.0), scale: (1.0, 1.0, 1.0)),
                            ),
                            (
                                id: NodeId(2),
                                name: "OldMesh",
                                parent: Some(NodeId(1)),
                                children: [],
                                kind: MeshInstance(mesh: None, material: None),
                                transform: (translation: (0.0, 0.0, 0.0), rotation_degrees: (0.0, 0.0, 0.0), scale: (1.0, 1.0, 1.0)),
                            ),
                        ],
                    ),
                ],
            )
        "#;
    let project = ProjectDocument::from_ron_str(ron).unwrap();
    let mesh = project
        .active_scene()
        .nodes()
        .iter()
        .find(|n| n.name == "OldMesh")
        .unwrap();
    assert!(matches!(
        mesh.kind,
        NodeKind::MeshInstance {
            mesh: None,
            material: None,
            animation_clip: None,
        }
    ));
}

#[test]
fn project_saves_and_loads_from_disk() {
    let mut project = ProjectDocument::legacy_grid_starter();
    project.name = "Disk Test".to_string();

    let dir = std::env::temp_dir().join(format!(
        "psxed-project-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let path = dir.join("project.ron");

    project.save_to_path(&path).unwrap();
    assert_eq!(ProjectDocument::load_from_path(&path).unwrap(), project);

    let _ = std::fs::remove_dir_all(dir);
}

pub(crate) fn unique_temp_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "psxed-project-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[test]
fn resource_rename_moves_project_owned_texture_file() {
    let root = unique_temp_dir("resource-rename-texture");
    let texture_dir = root.join("assets").join("textures");
    std::fs::create_dir_all(&texture_dir).unwrap();
    std::fs::write(texture_dir.join("floor.psxt"), b"texture").unwrap();

    let mut project = ProjectDocument::new("test");
    let id = project.add_resource(
        "Floor",
        ResourceData::Material(MaterialResource::opaque(Some(
            "assets/textures/floor.psxt".to_string(),
        ))),
    );

    let report = project
        .rename_resource_with_files(id, "Stone Floor", &root)
        .unwrap();

    assert_eq!(project.resource_name(id), Some("Stone Floor"));
    let ResourceData::Material(material) = &project.resource(id).unwrap().data else {
        panic!("expected material");
    };
    assert_eq!(
        material.psxt_path.as_deref(),
        Some("assets/textures/stone_floor.psxt")
    );
    assert!(!texture_dir.join("floor.psxt").exists());
    assert_eq!(
        std::fs::read(texture_dir.join("stone_floor.psxt")).unwrap(),
        b"texture"
    );
    assert_eq!(report.renamed_files.len(), 1);

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn deleting_versioned_material_removes_each_unshared_texture() {
    let root = unique_temp_dir("delete-versioned-material-files");
    let texture_dir = root.join("assets").join("textures");
    std::fs::create_dir_all(&texture_dir).unwrap();
    std::fs::write(texture_dir.join("original.psxt"), b"original").unwrap();
    std::fs::write(texture_dir.join("llm.psxt"), b"llm").unwrap();

    let mut project = ProjectDocument::new("version files");
    let mut material = MaterialResource::opaque(Some("assets/textures/original.psxt".to_string()));
    material.create_version("LLM");
    material.psxt_path = Some("assets/textures/llm.psxt".to_string());
    let id = project.add_resource("Stone", ResourceData::Material(material));

    let report = project
        .delete_resource_with_files(id, &root)
        .expect("versioned material deletes");
    let mut deleted = report
        .deleted_files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<Vec<_>>();
    deleted.sort_unstable();
    assert_eq!(
        deleted,
        vec!["assets/textures/llm.psxt", "assets/textures/original.psxt"]
    );
    assert!(!texture_dir.join("original.psxt").exists());
    assert!(!texture_dir.join("llm.psxt").exists());

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn renaming_material_keeps_texture_shared_with_saved_version() {
    let root = unique_temp_dir("rename-version-shared-texture");
    let texture_dir = root.join("assets").join("textures");
    std::fs::create_dir_all(&texture_dir).unwrap();
    std::fs::write(texture_dir.join("stone.psxt"), b"stone").unwrap();

    let mut project = ProjectDocument::new("version sharing");
    let mut material = MaterialResource::opaque(Some("assets/textures/stone.psxt".to_string()));
    material.create_version("LLM");
    let id = project.add_resource("Stone", ResourceData::Material(material));

    let report = project
        .rename_resource_with_files(id, "Cathedral Stone", &root)
        .unwrap();
    assert!(report.renamed_files.is_empty());
    assert!(texture_dir.join("stone.psxt").exists());
    assert!(!texture_dir.join("cathedral_stone.psxt").exists());
    let ResourceData::Material(material) = &project.resource(id).unwrap().data else {
        panic!("stone remains a material");
    };
    assert!(material
        .version_texture_paths()
        .iter()
        .all(|path| *path == "assets/textures/stone.psxt"));

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn resource_rename_moves_imported_model_bundle_files() {
    let root = unique_temp_dir("resource-rename-model");
    let bundle_dir = root.join("assets").join("models").join("obsidian_wraith");
    std::fs::create_dir_all(&bundle_dir).unwrap();
    std::fs::write(bundle_dir.join("obsidian_wraith.psxmdl"), b"model").unwrap();
    std::fs::write(bundle_dir.join("obsidian_wraith.psxt"), b"atlas").unwrap();

    let mut project = ProjectDocument::new("test");
    let id = project.add_resource(
        "Obsidian Wraith",
        ResourceData::Model(ModelResource {
            model_path: "assets/models/obsidian_wraith/obsidian_wraith.psxmdl".to_string(),
            source_path: None,
            texture_path: Some("assets/models/obsidian_wraith/obsidian_wraith.psxt".to_string()),
            skeleton: None,
            world_height: 1024,
            collision_radius: default_model_collision_radius_for_height(1024),
            scale_q8: [MODEL_SCALE_ONE_Q8; 3],
            default_visual_yaw_q12: 0,
            attachments: Vec::new(),
        }),
    );

    let report = project
        .rename_resource_with_files(id, "Hooded Wretch", &root)
        .unwrap();

    let ResourceData::Model(model) = &project.resource(id).unwrap().data else {
        panic!("expected model");
    };
    assert_eq!(
        model.model_path,
        "assets/models/hooded_wretch/hooded_wretch.psxmdl"
    );
    assert_eq!(
        model.texture_path.as_deref(),
        Some("assets/models/hooded_wretch/hooded_wretch.psxt")
    );
    assert_eq!(report.renamed_files.len(), 2);
    assert!(!bundle_dir.exists());
    assert!(root
        .join("assets/models/hooded_wretch/hooded_wretch.psxmdl")
        .exists());

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn resource_rename_refuses_existing_target_without_mutating_project() {
    let root = unique_temp_dir("resource-rename-target-exists");
    let texture_dir = root.join("assets").join("textures");
    std::fs::create_dir_all(&texture_dir).unwrap();
    std::fs::write(texture_dir.join("floor.psxt"), b"old").unwrap();
    std::fs::write(texture_dir.join("stone_floor.psxt"), b"target").unwrap();

    let mut project = ProjectDocument::new("test");
    let id = project.add_resource(
        "Floor",
        ResourceData::Material(MaterialResource::opaque(Some(
            "assets/textures/floor.psxt".to_string(),
        ))),
    );

    let error = project
        .rename_resource_with_files(id, "Stone Floor", &root)
        .unwrap_err();

    assert!(matches!(error, ResourceRenameError::TargetExists(_)));
    assert_eq!(project.resource_name(id), Some("Floor"));
    let ResourceData::Material(material) = &project.resource(id).unwrap().data else {
        panic!("expected material");
    };
    assert_eq!(
        material.psxt_path.as_deref(),
        Some("assets/textures/floor.psxt")
    );
    assert_eq!(
        std::fs::read(texture_dir.join("floor.psxt")).unwrap(),
        b"old"
    );
    assert_eq!(
        std::fs::read(texture_dir.join("stone_floor.psxt")).unwrap(),
        b"target"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn delete_resource_removes_entry_and_clears_references() {
    let root = unique_temp_dir("resource-delete");
    let texture_dir = root.join("assets").join("textures");
    std::fs::create_dir_all(&texture_dir).unwrap();
    std::fs::write(texture_dir.join("target.psxt"), b"texture").unwrap();

    let mut project = ProjectDocument::new("delete-resource");
    let target = project.add_resource(
        "Target",
        ResourceData::Material(MaterialResource::opaque(Some(
            "assets/textures/target.psxt".to_string(),
        ))),
    );
    let character = project.add_resource(
        "Character",
        ResourceData::Character(CharacterResource {
            model: Some(target),
            ..CharacterResource::defaults()
        }),
    );
    let weapon = project.add_resource(
        "Weapon",
        ResourceData::Weapon(WeaponResource {
            model: Some(target),
            ..WeaponResource::default()
        }),
    );

    let scene = project.active_scene_mut();
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.set_floor(0, 0, 0, Some(target));
    grid.add_wall(0, 0, GridDirection::North, 0, 1024, Some(target));
    let room = scene.add_node(scene.root, "Room", NodeKind::Section { grid });
    scene.add_node(
        room,
        "Mesh",
        NodeKind::MeshInstance {
            mesh: Some(target),
            material: Some(target),
            animation_clip: None,
        },
    );
    let entity = scene.add_node(room, "Entity", NodeKind::Entity);
    scene.add_node(
        entity,
        "Renderer",
        NodeKind::ModelRenderer {
            model: Some(target),
            material: Some(target),
            visual_offset: [0; 3],
            visual_scale_q8: MODEL_SCALE_ONE_Q8,
        },
    );
    scene.add_node(
        entity,
        "Controller",
        NodeKind::CharacterController {
            character: Some(target),
            settings: CharacterControllerSettings::default(),
            player: true,
        },
    );
    scene.add_node(
        entity,
        "Equipment",
        NodeKind::Equipment {
            weapon: Some(target),
            character_socket: "right_hand_grip".to_string(),
            weapon_grip: "grip".to_string(),
        },
    );
    scene.add_node(
        room,
        "Spawn",
        NodeKind::SpawnPoint {
            player: false,
            character: Some(target),
        },
    );
    assert_eq!(project.resource_reference_count(target), 11);
    let report = project
        .delete_resource_with_files(target, &root)
        .expect("resource exists");
    assert_eq!(report.removed.name, "Target");
    assert_eq!(report.cleared_references, 11);
    assert_eq!(
        report.deleted_files,
        vec![ResourceFileDelete {
            path: "assets/textures/target.psxt".to_string(),
        }]
    );
    assert!(report.skipped_files.is_empty());
    assert!(!texture_dir.join("target.psxt").exists());
    assert!(project.resource(target).is_none());
    let ResourceData::Character(character_data) = &project.resource(character).unwrap().data else {
        panic!("expected character");
    };
    assert_eq!(character_data.model, None);
    let ResourceData::Weapon(weapon_data) = &project.resource(weapon).unwrap().data else {
        panic!("expected weapon");
    };
    assert_eq!(weapon_data.model, None);

    for node in project.active_scene().nodes() {
        match &node.kind {
            NodeKind::Section { grid } => {
                let sector = grid.sector(0, 0).unwrap();
                assert_eq!(sector.floor.as_ref().unwrap().material, None);
                assert_eq!(
                    sector
                        .walls
                        .get(GridDirection::North)
                        .first()
                        .unwrap()
                        .material,
                    None
                );
            }
            NodeKind::MeshInstance { mesh, material, .. } => {
                assert_eq!((*mesh, *material), (None, None));
            }
            NodeKind::ModelRenderer {
                model, material, ..
            } => {
                assert_eq!((*model, *material), (None, None));
            }
            NodeKind::CharacterController { character, .. }
            | NodeKind::SpawnPoint { character, .. } => {
                assert_eq!(*character, None);
            }
            NodeKind::Equipment { weapon, .. } => {
                assert_eq!(*weapon, None);
            }
            _ => {}
        }
    }

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn delete_material_source_clears_transition_recipes() {
    let mut project = ProjectDocument::new("delete-transition-source");
    let source = project.add_resource(
        "Source",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let transition = project.add_resource(
        "Transition",
        ResourceData::Material(MaterialResource {
            texture_mode: MaterialTextureMode::Transition,
            transition: TransitionMaterialTexture {
                source_a: Some(source),
                source_b: Some(source),
                ..TransitionMaterialTexture::default()
            },
            secondary_layer: Some(ModelSecondaryLayer {
                texture_mode: MaterialTextureMode::Transition,
                transition: TransitionMaterialTexture {
                    source_a: Some(source),
                    source_b: Some(source),
                    ..TransitionMaterialTexture::default()
                },
                ..ModelSecondaryLayer::default()
            }),
            ..MaterialResource::opaque(None)
        }),
    );

    assert_eq!(project.resource_reference_count(source), 4);
    let report = project.delete_resource(source).expect("source exists");
    assert_eq!(report.cleared_references, 4);
    let ResourceData::Material(material) = &project.resource(transition).unwrap().data else {
        panic!("transition remains a material");
    };
    assert_eq!(material.transition.source_a, None);
    assert_eq!(material.transition.source_b, None);
    let layer = material
        .secondary_layer
        .as_ref()
        .expect("layer is preserved");
    assert_eq!(layer.transition.source_a, None);
    assert_eq!(layer.transition.source_b, None);
}

#[test]
fn delete_material_source_clears_inactive_version_recipes() {
    let mut project = ProjectDocument::new("delete-versioned-transition-source");
    let source = project.add_resource(
        "Source",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let mut transition = MaterialResource::opaque(None);
    transition.texture_mode = MaterialTextureMode::Transition;
    transition.transition.source_a = Some(source);
    transition.transition.source_b = Some(source);
    transition.create_version("LLM Alternative");
    transition.texture_mode = MaterialTextureMode::Generated;
    transition.transition.source_a = None;
    transition.transition.source_b = None;
    let transition_id = project.add_resource("Transition", ResourceData::Material(transition));

    assert_eq!(project.resource_reference_count(source), 2);
    let report = project.delete_resource(source).expect("source exists");
    assert_eq!(report.cleared_references, 2);
    let ResourceData::Material(material) = &mut project.resource_mut(transition_id).unwrap().data
    else {
        panic!("transition remains a material");
    };
    assert!(material.activate_version(MaterialVersionId::ORIGINAL));
    assert_eq!(material.transition.source_a, None);
    assert_eq!(material.transition.source_b, None);
}

#[test]
fn corner_surviving_split_picks_diagonal_that_keeps_a_triangle() {
    // Drop NE → only the NW-SE diagonal keeps a triangle.
    // Drop NW → only the NE-SW diagonal keeps a triangle.
    assert_eq!(Corner::NE.surviving_split(), GridSplit::NorthWestSouthEast);
    assert_eq!(Corner::SW.surviving_split(), GridSplit::NorthWestSouthEast);
    assert_eq!(Corner::NW.surviving_split(), GridSplit::NorthEastSouthWest);
    assert_eq!(Corner::SE.surviving_split(), GridSplit::NorthEastSouthWest);
}

#[test]
fn drop_corner_marks_face_as_triangle_and_flips_split() {
    let mut face = GridHorizontalFace::flat(0, None);
    face.split = GridSplit::NorthWestSouthEast; // would die if NW dropped
    face.drop_corner(Corner::NW);
    assert!(face.is_triangle());
    assert_eq!(face.dropped_corner, Some(Corner::NW));
    assert_eq!(face.split, GridSplit::NorthEastSouthWest);

    face.restore_corner();
    assert!(!face.is_triangle());
    assert_eq!(face.dropped_corner, None);
}

#[test]
fn horizontal_triangle_overrides_inherit_until_set() {
    let parent = ResourceId(11);
    let triangle = ResourceId(12);
    let mut face = GridHorizontalFace::flat(0, Some(parent));
    face.uv.offset = [3, 4];
    face.walkable = true;

    assert_eq!(face.triangle_material(0), Some(parent));
    assert_eq!(face.triangle_uv(0), face.uv);
    assert!(face.triangle_walkable(0));

    let override_a = face.triangle_override_mut(0);
    override_a.material = Some(GridTriangleMaterialOverride::Resource(triangle));
    override_a.uv = Some(GridUvTransform {
        offset: [9, 10],
        span: [64, 32],
        rotation: GridUvRotation::Deg90,
        flip_u: true,
        flip_v: false,
    });
    override_a.walkable = Some(false);

    assert_eq!(face.triangle_material(0), Some(triangle));
    assert_eq!(face.triangle_material(1), Some(parent));
    assert_eq!(face.triangle_uv(0).offset, [9, 10]);
    assert!(!face.triangle_walkable(0));
    assert!(face.triangle_walkable(1));
}

#[test]
fn drop_corner_on_wall_marks_triangle() {
    let mut wall = GridVerticalFace::flat(0, 64, None);
    wall.drop_corner(WallCorner::TL);
    assert!(wall.is_triangle());
    assert_eq!(wall.dropped_corner, Some(WallCorner::TL));
}

#[test]
fn grid_uv_transform_rotates_quad_without_rebaking_texture() {
    let transform = GridUvTransform {
        offset: [0, 0],
        span: [0, 0],
        rotation: GridUvRotation::Deg90,
        flip_u: false,
        flip_v: false,
    };

    assert_eq!(
        transform.apply_to_quad([(0, 0), (64, 0), (64, 64), (0, 64)]),
        [(64, 0), (64, 64), (0, 64), (0, 0)]
    );
}

#[test]
fn grid_uv_transform_rotates_quad_45_degrees_without_rebaking_texture() {
    let transform = GridUvTransform {
        offset: [0, 0],
        span: [0, 0],
        rotation: GridUvRotation::Deg45,
        flip_u: false,
        flip_v: false,
    };

    assert_eq!(
        transform.apply_to_quad([(0, 0), (64, 0), (64, 64), (0, 64)]),
        [(32, 0), (64, 32), (32, 64), (0, 32)]
    );
}

#[test]
fn grid_uv_transform_rotates_quad_315_degrees_without_rebaking_texture() {
    let transform = GridUvTransform {
        offset: [0, 0],
        span: [0, 0],
        rotation: GridUvRotation::Deg315,
        flip_u: false,
        flip_v: false,
    };

    assert_eq!(
        transform.apply_to_quad([(0, 0), (64, 0), (64, 64), (0, 64)]),
        [(0, 32), (32, 0), (64, 32), (32, 64)]
    );
}

#[test]
fn grid_uv_transform_flips_and_wraps_ps1_uv_offsets() {
    let transform = GridUvTransform {
        offset: [-8, 12],
        span: [0, 0],
        rotation: GridUvRotation::Deg0,
        flip_u: true,
        flip_v: false,
    };

    assert_eq!(
        transform.apply_to_quad([(0, 0), (64, 0), (64, 64), (0, 64)]),
        [(56, 12), (248, 12), (248, 76), (56, 76)]
    );
}

#[test]
fn grid_uv_transform_scales_quad_span_without_rebaking_texture() {
    let transform = GridUvTransform {
        offset: [0, 0],
        span: [0, 32],
        rotation: GridUvRotation::Deg0,
        flip_u: false,
        flip_v: false,
    };

    assert_eq!(
        transform.apply_to_quad([(0, 64), (64, 64), (64, 0), (0, 0)]),
        [(0, 32), (64, 32), (64, 0), (0, 0)]
    );
}

#[test]
fn wall_autotile_sets_double_height_v_span_without_changing_geometry() {
    let mut wall = GridVerticalFace::flat(0, 1536, None);
    let heights = wall.heights;

    let clamped = wall.autotile_uv(768);

    assert!(!clamped);
    assert_eq!(wall.heights, heights);
    assert_eq!(wall.uv.span, [0, 128]);
}

#[test]
fn wall_autotile_uses_partial_v_span_for_short_wall() {
    let mut wall = GridVerticalFace::flat(0, 384, None);

    let clamped = wall.autotile_uv(768);

    assert!(!clamped);
    assert_eq!(wall.heights, [0, 0, 384, 384]);
    assert_eq!(wall.uv.span, [0, 32]);
}

#[test]
fn wall_autotile_clamps_one_quad_to_ps1_uv_range() {
    let mut wall = GridVerticalFace::flat(0, 768 * 5, None);

    let clamped = wall.autotile_uv(768);

    assert!(clamped);
    assert_eq!(wall.heights, [0, 0, 3840, 3840]);
    assert_eq!(wall.uv.span, [0, 255]);
}

#[test]
fn default_tall_wall_keeps_single_authored_uv_primitive() {
    let wall = GridVerticalFace::flat(0, 768 * 5, None);

    let segments = wall.split_into_autotile_segments(768);

    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].heights, [0, 0, 3840, 3840]);
    assert_eq!(segments[0].uv.span, [0, 0]);
}

#[test]
fn wall_autotile_keeps_one_primitive_when_repeated_uvs_fit_packet() {
    let mut wall = GridVerticalFace::flat(0, 1536, None);
    wall.uv.offset[1] = -5;
    wall.autotile_uv(768);

    let segments = wall.split_into_autotile_segments(768);

    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].heights, [0, 0, 1536, 1536]);
    assert_eq!(segments[0].uv.span, [0, 128]);
    assert_eq!(segments[0].uv.offset[1], -5);
}

#[test]
fn wall_autotile_segments_restore_clamped_tall_wall_density() {
    let mut wall = GridVerticalFace::flat(0, 768 * 5, None);
    wall.autotile_uv(768);

    let segments = wall.split_into_autotile_segments(768);

    assert_eq!(segments.len(), 5);
    assert!(segments.iter().all(|segment| segment.uv.span == [0, 0]));
    assert_eq!(segments[4].heights, [3072, 3072, 3840, 3840]);
}

#[test]
fn wall_split_height_segments_keeps_uvs_and_sloped_edges_connected() {
    let mut wall = GridVerticalFace::flat(0, 1536, None);
    wall.heights = [0, 384, 1536, 1920];
    wall.uv.span = [12, 96];

    let segments = wall.split_into_height_segments(768);

    assert_eq!(segments.len(), 3);
    assert_eq!(
        [
            segments[0].heights[WallCorner::BL.idx()],
            segments[0].heights[WallCorner::BR.idx()],
        ],
        [0, 384]
    );
    assert_eq!(
        [
            segments[2].heights[WallCorner::TL.idx()],
            segments[2].heights[WallCorner::TR.idx()],
        ],
        [1920, 1536]
    );
    for pair in segments.windows(2) {
        assert_eq!(
            pair[0].heights[WallCorner::TL.idx()],
            pair[1].heights[WallCorner::BL.idx()]
        );
        assert_eq!(
            pair[0].heights[WallCorner::TR.idx()],
            pair[1].heights[WallCorner::BR.idx()]
        );
    }
    assert!(segments.iter().all(|segment| segment.uv.span == [12, 96]));
}

#[test]
fn legacy_texture_resources_migrate_into_materials_on_load() {
    // Pre-merge project shape: Texture resources + materials that
    // reference them by id, plus direct texture references from a UI
    // image, a far-vista panel, and a particle emitter. Loading must
    // fold wrapped textures into their materials, keep direct
    // references valid by converting those textures into materials in
    // place (same id), and drop the orphaned leftovers.
    let mut project = ProjectDocument::new("legacy-textures");
    let wrapped = project.add_resource(
        "WALL_1A",
        ResourceData::Texture {
            psxt_path: "assets/textures/wall_1a.psxt".to_string(),
        },
    );
    let material = project.add_resource(
        "WALL_1A Material",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let direct = project.add_resource(
        "VISTA_SLICE",
        ResourceData::Texture {
            psxt_path: "assets/textures/vista.psxt".to_string(),
        },
    );
    let orphan = project.add_resource(
        "UNUSED",
        ResourceData::Texture {
            psxt_path: "assets/textures/unused.psxt".to_string(),
        },
    );
    let world_id = project.active_scene().root;
    if let Some(node) = project.active_scene_mut().node_mut(world_id) {
        if let NodeKind::World { far_vista, .. } = &mut node.kind {
            far_vista.texture = Some(direct);
        }
    }

    // New saves never write the legacy `texture:` field, so inject it
    // textually the way a pre-merge project file carried it.
    let ron = project.to_ron_string().expect("serializes");
    assert_eq!(ron.matches("Material((").count(), 1);
    let ron = ron.replace(
        "Material((",
        &format!("Material((texture: Some(({})),", wrapped.raw()),
    );
    let loaded = ProjectDocument::from_ron_str(&ron).expect("loads");

    assert!(
        !loaded
            .resources
            .iter()
            .any(|r| matches!(&r.data, ResourceData::Texture { .. })),
        "no Texture resources survive migration"
    );
    let ResourceData::Material(folded) = &loaded.resource(material).expect("material kept").data
    else {
        panic!("material kept its kind");
    };
    assert_eq!(
        folded.psxt_path.as_deref(),
        Some("assets/textures/wall_1a.psxt")
    );
    assert_eq!(folded.legacy_texture, None);
    let ResourceData::Material(converted) = &loaded.resource(direct).expect("direct ref kept").data
    else {
        panic!("directly referenced texture converts in place");
    };
    assert_eq!(
        converted.psxt_path.as_deref(),
        Some("assets/textures/vista.psxt")
    );
    assert!(loaded.resource(orphan).is_none(), "orphan dropped");
    let vista_ref = loaded
        .active_scene()
        .nodes()
        .iter()
        .find_map(|node| match &node.kind {
            NodeKind::World { far_vista, .. } => far_vista.texture,
            _ => None,
        });
    assert_eq!(vista_ref, Some(direct), "direct reference id unchanged");

    // Round-trip stability: a migrated project reloads unchanged.
    let ron2 = loaded.to_ron_string().expect("reserializes");
    let reloaded = ProjectDocument::from_ron_str(&ron2).expect("reloads");
    assert_eq!(loaded.resources, reloaded.resources);
}

/// A prefab is only reusable if it survives a RON round trip and lands on the
/// destination project's materials. `ResourceId` is a per-project counter, so
/// a piece authored where "Stone" happens to be id 1 must not bind to whatever
/// sits at id 1 in the project it is stamped into -- that failure is silent,
/// the geometry appears with the wrong texture and nothing reports it.
#[test]
fn a_prefab_round_trips_and_rebinds_its_materials_by_name() {
    let mut source = ProjectDocument::new("prefab-source");
    let filler = source.add_resource(
        "Filler",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let stone = source.add_resource(
        "Stone",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    assert_ne!(filler, stone, "source needs two distinct ids");

    let mut sector = GridSector::with_floor(0, Some(stone));
    sector.floor.as_mut().unwrap().heights = [0, 32, 64, 96];
    sector
        .walls
        .north
        .push(GridVerticalFace::flat(0, 512, Some(stone)));
    let room = source.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Section {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    // An upper floor that links back down to the base, so the round trip has a
    // floor link and a non-default relative elevation to preserve.
    let mut upper = GridSector::with_floor(1024, Some(stone));
    upper.floor_below = Some(GridFloorLink::room(room));
    let prefab = Prefab::capture(
        "Stair Block",
        1024,
        1,
        1,
        false,
        vec![
            PrefabFloor {
                relative_elevation: 0,
                cells: vec![PrefabCell {
                    offset: [0, 0],
                    sector: Some(sector),
                }],
            },
            PrefabFloor {
                relative_elevation: 1024,
                cells: vec![PrefabCell {
                    offset: [0, 0],
                    sector: Some(upper),
                }],
            },
        ],
        room,
        0,
        &source,
    );
    assert_eq!(
        prefab.materials.get(&stone.raw()).map(String::as_str),
        Some("Stone")
    );

    let path = std::env::temp_dir().join("psoxide-prefab-round-trip.ron");
    prefab.save_to_path(&path).expect("prefab saves");
    let loaded = Prefab::load_from_path(&path).expect("prefab loads");
    let _ = std::fs::remove_file(&path);
    assert_eq!(loaded.name, "Stair Block");
    assert_eq!(loaded.sector_size, 1024);
    assert_eq!(loaded.floors.len(), 2, "both floors survive");
    assert_eq!(loaded.floors[1].relative_elevation, 1024);
    assert_eq!(
        loaded.floors[0].cells[0]
            .sector
            .as_ref()
            .unwrap()
            .floor
            .as_ref()
            .unwrap()
            .heights,
        [0, 32, 64, 96]
    );
    // The link pointed at floor 0 of the captured piece, so it is stored
    // self-relative with no room id at all.
    let stored_link = loaded.floors[1].cells[0]
        .sector
        .as_ref()
        .unwrap()
        .floor_below
        .expect("the downward link survives capture");
    assert_eq!(stored_link.target_room, None, "no source NodeId is carried");
    assert_eq!(stored_link.target_floor, 0);

    // The destination handed the source's "Stone" id to something else, which
    // is the collision this test exists for: binding by id gives "Dirt".
    let mut destination = ProjectDocument::new("prefab-destination");
    destination.add_resource(
        "Grass",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let dirt = destination.add_resource(
        "Dirt",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let stone_here = destination.add_resource(
        "Stone",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    assert_eq!(dirt, stone, "the destination reuses the source's Stone id");
    assert_ne!(stone_here, stone);

    let dest_room = destination.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Section {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    let (floors, unbound) = loaded.bound_floors(&destination, dest_room, 0);
    assert_eq!(unbound, 0);
    let sector = floors[0].cells[0].sector.as_ref().unwrap();
    assert_eq!(sector.floor.as_ref().unwrap().material, Some(stone_here));
    assert_eq!(sector.walls.north[0].material, Some(stone_here));

    // The self-relative link resolves onto the destination room, not the id it
    // was authored against.
    let bound_link = floors[1].cells[0]
        .sector
        .as_ref()
        .unwrap()
        .floor_below
        .expect("the link resolves");
    assert_eq!(bound_link.target_room, Some(dest_room));
    assert_eq!(bound_link.target_floor, 0);

    // Stamped onto floor 2 of the destination, the same link has to follow.
    let (floors, _) = loaded.bound_floors(&destination, dest_room, 2);
    assert_eq!(
        floors[1].cells[0]
            .sector
            .as_ref()
            .unwrap()
            .floor_below
            .unwrap()
            .target_floor,
        2,
        "a self-relative link rebases with the stamp"
    );

    // A project with no matching name clears the reference rather than
    // guessing: an unassigned face is visibly wrong, a wrong one is not.
    let bare = ProjectDocument::new("prefab-bare");
    let (floors, unbound) = loaded.bound_floors(&bare, dest_room, 0);
    assert_eq!(
        unbound, 3,
        "two floor faces and the wall all lose their material"
    );
    assert_eq!(
        floors[0].cells[0]
            .sector
            .as_ref()
            .unwrap()
            .floor
            .as_ref()
            .unwrap()
            .material,
        None
    );
}

/// The authored node was `Map`, then `Room`, and is now `Section`. Every
/// project on disk still says one of the older two, so the alias chain is the
/// only thing standing between this rename and every saved level failing to
/// load. New saves must write the new name.
#[test]
fn section_nodes_load_under_every_historical_name() {
    let mut doc = ProjectDocument::new("legacy");
    let room = doc.active_scene_mut().add_node(
        NodeId::ROOT,
        "Bay",
        NodeKind::Section {
            grid: WorldGrid::empty(2, 3, 1024),
        },
    );
    let current = doc.to_ron_string().expect("serialises");
    assert!(
        current.contains("Section("),
        "the current name is what gets written"
    );

    // Rewrite the variant name to forge a file from each earlier era. Going
    // through the real serialiser keeps the rest of the schema honest, which
    // hand-written RON does not.
    for legacy in ["Map", "Room"] {
        let aged = current.replace("Section(", &format!("{legacy}("));
        let loaded = ProjectDocument::from_ron_str(&aged)
            .unwrap_or_else(|e| panic!("a project saved as {legacy} must still load: {e}"));
        let node = loaded
            .active_scene()
            .node(room)
            .unwrap_or_else(|| panic!("{legacy}: the node survived"));
        let NodeKind::Section { grid } = &node.kind else {
            panic!("{legacy} deserialised into something other than a Section");
        };
        assert_eq!((grid.width, grid.depth), (2, 3), "{legacy} kept its grid");

        // The alias is a migration path, not a permanent second spelling.
        let resaved = loaded.to_ron_string().expect("re-serialises");
        assert!(
            resaved.contains("Section("),
            "{legacy} re-saves under the new name"
        );
        assert!(
            !resaved.contains(&format!("{legacy}(grid")),
            "{legacy} does not survive the round trip"
        );
    }
}

/// Two Sections placed edge to edge with a paired portal must cook into two
/// runtime rooms that are actually connected.
///
/// Before cross-section wiring, `plan_portal_rooms` only cut seams inside one
/// grid, so a level built from several Sections was a set of islands at runtime
/// however it looked in the editor. This is the gate for that.
#[test]
fn two_sections_with_a_paired_portal_cook_into_connected_runtime_rooms() {
    let mut project = ProjectDocument::legacy_grid_starter();
    let material = project
        .resources
        .iter()
        .find(|r| matches!(r.data, ResourceData::Material(_)))
        .map(|r| r.id)
        .expect("starter has a material");

    // Clear the starter geometry so only the two sections under test cook.
    let existing: Vec<NodeId> = project
        .active_scene()
        .nodes()
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Section { .. }))
        .map(|n| n.id)
        .collect();
    for id in &existing {
        if let Some(NodeKind::Section { grid }) = project
            .active_scene_mut()
            .node_mut(*id)
            .map(|n| &mut n.kind)
        {
            for sector in grid.sectors.iter_mut() {
                *sector = None;
            }
        }
    }

    // West section at cells x=0..1, east section at x=2..3: they share the
    // edge between cell 1 and cell 2.
    let mut west = WorldGrid::stone_room(2, 2, 1024, Some(material), Some(material));
    west.origin = [0, 0];
    let mut east = WorldGrid::stone_room(2, 2, 1024, Some(material), Some(material));
    east.origin = [2, 0];
    let west_id =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "West", NodeKind::Section { grid: west });
    let east_id =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "East", NodeKind::Section { grid: east });

    // A reciprocal portal pair on the shared edge.
    let mut portal = |room: NodeId, target: NodeId, at: [f32; 3]| {
        let id = project.active_scene_mut().add_node(
            room,
            "Seam",
            NodeKind::Portal {
                target_room: Some(target),
                target_entry: String::new(),
                entry_name: String::new(),
                geometry: None,
            },
        );
        if let Some(node) = project.active_scene_mut().node_mut(id) {
            node.transform.translation = at;
        }
        id
    };
    portal(west_id, east_id, [1.0, 0.0, 0.0]);
    portal(east_id, west_id, [-1.0, 0.0, 0.0]);

    // The starter's player lived in the geometry cleared above. Reuse one of
    // its Character profiles rather than inventing a bare spawn, which the cook
    // rejects when several Characters are defined.
    let character = project
        .resources
        .iter()
        .find(|r| matches!(r.data, ResourceData::Character(_)))
        .map(|r| r.id)
        .expect("starter defines a character");
    project.active_scene_mut().add_node(
        west_id,
        "Player Spawn",
        NodeKind::SpawnPoint {
            player: true,
            character: Some(character),
        },
    );

    let (package, report) = playtest::build_package(&project, &legacy_grid_starter_dir());
    assert!(
        report.is_ok(),
        "a paired cross-section portal must cook: {:?}",
        report.errors
    );
    let package = package.expect("package built");

    // The assertion has to be that a portal spans the two SECTIONS. Counting
    // portals is not enough: each section's own marker also cuts its own grid,
    // so intra-section pairs exist either way and would pass a naive check.
    let owner = |index: u16| -> &str {
        package
            .rooms
            .get(index as usize)
            .map(|r| r.name.as_str())
            .unwrap_or("?")
    };
    let spans: Vec<(&str, &str)> = package
        .room_portals
        .iter()
        .map(|p| (owner(p.source_room), owner(p.destination_room)))
        .filter(|(a, b)| a.contains("West") != b.contains("West"))
        .collect();
    assert!(
        !spans.is_empty(),
        "a portal must join a West room to an East room; got {:?}",
        package
            .room_portals
            .iter()
            .map(|p| (owner(p.source_room), owner(p.destination_room)))
            .collect::<Vec<_>>()
    );
    // And it must be reciprocal, or visibility only works one way.
    let (first_a, first_b) = spans[0];
    assert!(
        spans.iter().any(|(a, b)| *a == first_b && *b == first_a),
        "the cross-section portal is reciprocal: {spans:?}"
    );
}

/// Two Sections placed edge to edge with facing openings must connect without
/// anyone authoring a Portal marker.
///
/// This is the prefab socket contract applied between Sections: a socket is the
/// absence of a perimeter wall, so touching openings already describe a doorway.
/// The negative half matters just as much -- a wall on either side must stop
/// it, or the cook would punch holes through sealed geometry.
#[test]
fn adjacent_sections_with_facing_openings_connect_without_an_authored_portal() {
    fn cook(seal_east: bool) -> usize {
        let mut project = ProjectDocument::legacy_grid_starter();
        let material = project
            .resources
            .iter()
            .find(|r| matches!(r.data, ResourceData::Material(_)))
            .map(|r| r.id)
            .expect("starter has a material");
        let character = project
            .resources
            .iter()
            .find(|r| matches!(r.data, ResourceData::Character(_)))
            .map(|r| r.id)
            .expect("starter defines a character");
        let existing: Vec<NodeId> = project
            .active_scene()
            .nodes()
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Section { .. }))
            .map(|n| n.id)
            .collect();
        for id in &existing {
            if let Some(NodeKind::Section { grid }) = project
                .active_scene_mut()
                .node_mut(*id)
                .map(|n| &mut n.kind)
            {
                for sector in grid.sectors.iter_mut() {
                    *sector = None;
                }
            }
        }

        // Plain floors, no perimeter walls: every edge is a socket.
        let mut west = WorldGrid::empty(2, 1, 1024);
        west.origin = [0, 0];
        let mut east = WorldGrid::empty(2, 1, 1024);
        east.origin = [2, 0];
        for x in 0..2 {
            west.set_floor(x, 0, 0, Some(material));
            east.set_floor(x, 0, 0, Some(material));
        }
        if seal_east {
            // One wall on the shared edge must veto the join.
            west.add_wall(1, 0, GridDirection::East, 0, 2048, Some(material));
        }

        let west_id = project.active_scene_mut().add_node(
            NodeId::ROOT,
            "West",
            NodeKind::Section { grid: west },
        );
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "East", NodeKind::Section { grid: east });
        project.active_scene_mut().add_node(
            west_id,
            "Player Spawn",
            NodeKind::SpawnPoint {
                player: true,
                character: Some(character),
            },
        );

        let (package, report) = playtest::build_package(&project, &legacy_grid_starter_dir());
        assert!(report.is_ok(), "cooks: {:?}", report.errors);
        let package = package.expect("package built");
        let owner = |index: u16| -> String {
            package
                .rooms
                .get(index as usize)
                .map(|r| r.name.clone())
                .unwrap_or_default()
        };
        package
            .room_portals
            .iter()
            .filter(|p| {
                owner(p.source_room).contains("West") != owner(p.destination_room).contains("West")
            })
            .count()
    }

    assert!(
        cook(false) >= 2,
        "facing openings connect on their own, with a reciprocal pair"
    );
    assert_eq!(
        cook(true),
        0,
        "a wall on the shared edge vetoes the automatic join"
    );
}

/// The authored and automatic cross-section passes must never emit the same
/// directed link twice.
///
/// They cannot generally collide, and it took a wrong test to see why: an
/// authored Portal marker can never sit on a section boundary, because
/// `portal_edge_key_for_node` only considers edges whose neighbour cell is
/// populated and in the same grid. An authored cross-section portal therefore
/// always snaps to an internal seam of its own section and says "this way lies
/// section B", while the adjacency pass wires the physical boundary. Those are
/// different links between different runtime rooms and both are wanted. What
/// must not happen is the same room pair being linked twice.
#[test]
fn an_authored_cross_section_portal_suppresses_the_automatic_one() {
    let mut project = ProjectDocument::legacy_grid_starter();
    let material = project
        .resources
        .iter()
        .find(|r| matches!(r.data, ResourceData::Material(_)))
        .map(|r| r.id)
        .expect("starter has a material");
    let character = project
        .resources
        .iter()
        .find(|r| matches!(r.data, ResourceData::Character(_)))
        .map(|r| r.id)
        .expect("starter defines a character");
    let existing: Vec<NodeId> = project
        .active_scene()
        .nodes()
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Section { .. }))
        .map(|n| n.id)
        .collect();
    for id in &existing {
        if let Some(NodeKind::Section { grid }) = project
            .active_scene_mut()
            .node_mut(*id)
            .map(|n| &mut n.kind)
        {
            for sector in grid.sectors.iter_mut() {
                *sector = None;
            }
        }
    }

    // Open floors on both sides, so the automatic pass would fire on its own.
    let mut west = WorldGrid::empty(2, 1, 1024);
    west.origin = [0, 0];
    let mut east = WorldGrid::empty(2, 1, 1024);
    east.origin = [2, 0];
    for x in 0..2 {
        west.set_floor(x, 0, 0, Some(material));
        east.set_floor(x, 0, 0, Some(material));
    }
    let west_id =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "West", NodeKind::Section { grid: west });
    let east_id =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "East", NodeKind::Section { grid: east });

    // And an author wired the same seam by hand.
    for (room, target, at) in [
        (west_id, east_id, [1.0f32, 0.0, 0.0]),
        (east_id, west_id, [-1.0f32, 0.0, 0.0]),
    ] {
        let id = project.active_scene_mut().add_node(
            room,
            "Seam",
            NodeKind::Portal {
                target_room: Some(target),
                target_entry: String::new(),
                entry_name: String::new(),
                geometry: None,
            },
        );
        if let Some(node) = project.active_scene_mut().node_mut(id) {
            node.transform.translation = at;
        }
    }
    project.active_scene_mut().add_node(
        west_id,
        "Player Spawn",
        NodeKind::SpawnPoint {
            player: true,
            character: Some(character),
        },
    );

    let (package, report) = playtest::build_package(&project, &legacy_grid_starter_dir());
    assert!(report.is_ok(), "cooks: {:?}", report.errors);
    let package = package.expect("package built");
    let owner = |index: u16| -> String {
        package
            .rooms
            .get(index as usize)
            .map(|r| r.name.clone())
            .unwrap_or_default()
    };
    let crossing: Vec<(u16, u16)> = package
        .room_portals
        .iter()
        .filter(|p| {
            owner(p.source_room).contains("West") != owner(p.destination_room).contains("West")
        })
        .map(|p| (p.source_room, p.destination_room))
        .collect();
    assert!(
        crossing.len() >= 2,
        "the sections are linked at all: {crossing:?}"
    );
    let mut seen = std::collections::HashSet::new();
    for pair in &crossing {
        assert!(
            seen.insert(*pair),
            "room pair {pair:?} is linked twice: {crossing:?}"
        );
    }
    // And every link is reciprocal, or visibility only flows one way.
    for (a, b) in &crossing {
        assert!(
            crossing.contains(&(*b, *a)),
            "link {a}->{b} has no return: {crossing:?}"
        );
    }
}

/// The embedded kit is the one on disk: a piece added to `editor/prefabs/`
/// without being embedded would silently not ship, and an embedded body that
/// no longer parses would seed a broken prefab into every new project.
#[test]
fn embedded_prefab_kit_matches_the_source_tree_and_parses() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("prefabs");
    let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
        .expect("source-tree prefabs directory")
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            (path.extension()? == "ron").then(|| {
                path.file_stem()
                    .expect("stem")
                    .to_string_lossy()
                    .into_owned()
            })
        })
        .collect();
    on_disk.sort();

    let mut embedded: Vec<String> = crate::prefab_kit_names().map(str::to_string).collect();
    embedded.sort();
    assert_eq!(
        embedded, on_disk,
        "embedded kit and editor/prefabs/ disagree"
    );

    for name in crate::prefab_kit_names() {
        let body = crate::prefab_kit_body(name).expect("embedded body");
        let prefab = ron::from_str::<crate::Prefab>(body)
            .unwrap_or_else(|e| panic!("embedded prefab {name} does not parse: {e}"));
        assert!(
            prefab
                .cells()
                .filter_map(|cell| cell.sector.as_ref())
                .any(|sector| sector.ceiling.is_some()),
            "embedded prefab {name} has no ceiling geometry"
        );
    }
}

#[test]
fn legacy_prefab_resources_load_but_do_not_reserialize() {
    let mut project = ProjectDocument::new("legacy-prefab-resource");
    project.add_resource(
        "Old Prefab Row",
        ResourceData::Prefab {
            source_path: "/shared/prefabs/old.ron".to_string(),
        },
    );
    let legacy_ron = project.to_ron_string().expect("legacy project serializes");
    assert!(legacy_ron.contains("Prefab"));

    let loaded = ProjectDocument::from_ron_str(&legacy_ron).expect("legacy project loads");
    assert!(!loaded
        .resources
        .iter()
        .any(|resource| matches!(resource.data, ResourceData::Prefab { .. })));
    assert!(!loaded
        .to_ron_string()
        .expect("normalized project serializes")
        .contains("Prefab"));
}

// --- Project world-format discriminator -------------------------------
//
// The persisted `world_format` field replaced the presence-based selection
// chain documented in docs/legacy-grid-boundary.md section 1. These tests
// pin the compatibility contract: documents written before the field
// existed must resolve to exactly the format they used to cook as, and a
// resolved value must survive a round trip untouched.

#[test]
fn legacy_grid_projects_without_the_field_load_as_grid() {
    for relative in [
        "../../archive/fixtures/legacy-grid-starter/project.ron",
        "../../samples/cortex_v1/project.ron",
    ] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
        let text = std::fs::read_to_string(&path).expect("tracked grid project is readable");
        assert!(
            !text.contains("world_format"),
            "{relative} is the pre-discriminator fixture; it must stay field-free \
             so the compatibility default keeps being exercised"
        );
        let project = ProjectDocument::from_ron_str(&text).expect("tracked grid project parses");
        assert_eq!(
            project.world_format(),
            ProjectWorldFormat::LegacyGrid,
            "{relative} must keep loading as a legacy grid project"
        );
    }
}

#[test]
fn bsp_projects_without_the_field_load_as_bsp() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../archive/fixtures/brush-first-playable/project.ron");
    let mut text = std::fs::read_to_string(&path).expect("tracked BSP project is readable");
    // Strip the field if the tracked copy already carries it: the point is
    // the pre-discriminator shape, which must still resolve to BSP.
    text = text
        .lines()
        .filter(|line| !line.trim_start().starts_with("world_format:"))
        .collect::<Vec<_>>()
        .join("\n");
    let project = ProjectDocument::from_ron_str(&text).expect("tracked BSP project parses");
    assert_eq!(project.world_format(), ProjectWorldFormat::Bsp);
}

#[test]
fn world_format_round_trips_and_is_never_re_derived() {
    // A BSP project whose brushes were all deleted stays BSP: the stored
    // value wins over geometry presence, so the cook reports the empty world
    // instead of silently reopening as a grid project.
    let mut project = ProjectDocument::new("format-round-trip");
    project.set_world_format(ProjectWorldFormat::Bsp);
    assert!(project.active_scene().brushes.is_empty());

    let ron = project.to_ron_string().expect("project serializes");
    assert!(ron.contains("world_format"), "the field must be persisted");
    let loaded = ProjectDocument::from_ron_str(&ron).expect("project parses");
    assert_eq!(loaded.world_format(), ProjectWorldFormat::Bsp);

    // And the reverse: a grid project that gains brushes keeps its stored
    // format (the cook then refuses, see the fail-closed tests).
    let mut grid = ProjectDocument::new("legacy-format-round-trip");
    grid.set_world_format(ProjectWorldFormat::LegacyGrid);
    let ron = grid.to_ron_string().expect("project serializes");
    let loaded = ProjectDocument::from_ron_str(&ron).expect("project parses");
    assert_eq!(loaded.world_format(), ProjectWorldFormat::LegacyGrid);
}

#[test]
fn a_fresh_document_is_bsp_before_it_has_any_geometry() {
    // New projects are BSP-only (docs/legacy-grid-boundary.md A13). An empty
    // scene must not read as "legacy grid" just because it has no brushes yet.
    let project = ProjectDocument::new("fresh-format");
    assert!(project.active_scene().brushes.is_empty());
    assert_eq!(project.world_format(), ProjectWorldFormat::Bsp);

    let ron = project.to_ron_string().expect("project serializes");
    assert!(ron.contains("world_format: Bsp"));
    let loaded = ProjectDocument::from_ron_str(&ron).expect("project parses");
    assert_eq!(loaded.world_format(), ProjectWorldFormat::Bsp);
}

#[test]
fn a_legacy_grid_project_holding_brushes_fails_the_cook_closed() {
    let mut project = ProjectDocument::new("grid-with-brushes");
    project.set_world_format(ProjectWorldFormat::LegacyGrid);
    project
        .active_scene_mut()
        .brushes
        .push(crate::brush::Brush::cuboid([0, 0, 0], [256, 256, 256]));
    let (package, report) =
        crate::playtest::build_package(&project, std::path::Path::new(env!("CARGO_MANIFEST_DIR")));
    assert!(package.is_none(), "a mixed project must not cook");
    assert!(
        report
            .errors
            .iter()
            .any(|error| error.contains("Legacy grid") && error.contains("brush")),
        "expected a named format mismatch, got {:?}",
        report.errors
    );
}

#[test]
fn a_bsp_project_without_brushes_fails_the_cook_closed() {
    let mut project = ProjectDocument::new("bsp-without-brushes");
    project.set_world_format(ProjectWorldFormat::Bsp);
    let (package, report) =
        crate::playtest::build_package(&project, std::path::Path::new(env!("CARGO_MANIFEST_DIR")));
    assert!(package.is_none(), "an empty BSP project must not cook");
    assert!(
        report
            .errors
            .iter()
            .any(|error| error.contains("BSP") && error.contains("no brushes")),
        "expected a named empty-world error, got {:?}",
        report.errors
    );
    assert!(
        !report
            .errors
            .iter()
            .any(|error| error.contains("at least one Room node")),
        "a BSP project must never be told to add a grid Room: {:?}",
        report.errors
    );
}

// --- Fail-closed grid boundary ----------------------------------------
//
// A BSP project must never instantiate grid spatial state. The cook's
// guards are checked from both ends: the authored input that would start a
// grid build is refused, and the cooked outputs are re-checked so a future
// leak surfaces as a named error instead of a level that silently streams
// rooms nobody authored.

/// The tracked BSP first-playable, loaded as a synthetic-mutation base.
fn bsp_fixture() -> (ProjectDocument, std::path::PathBuf) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../archive/fixtures/brush-first-playable");
    let project = ProjectDocument::load_from_path(dir.join("project.ron"))
        .expect("tracked BSP first-playable loads");
    assert_eq!(project.world_format(), ProjectWorldFormat::Bsp);
    (project, dir)
}

#[test]
fn a_bsp_project_holding_a_grid_section_fails_the_cook_closed() {
    let (mut project, dir) = bsp_fixture();
    let (baseline, report) = crate::playtest::build_package(&project, &dir);
    assert!(
        baseline.is_some(),
        "unmutated fixture cooks: {:?}",
        report.errors
    );

    let scene = project.active_scene_mut();
    let root = scene.root;
    scene.add_node(
        root,
        "Smuggled Section",
        NodeKind::Section {
            grid: WorldGrid::stone_room(1, 1, 1024, None, None),
        },
    );

    let (package, report) = crate::playtest::build_package(&project, &dir);
    assert!(
        package.is_none(),
        "a BSP project holding a Section must not cook"
    );
    let joined = report.error_messages().join(" | ");
    assert!(
        joined.contains("grid Section") && joined.contains("Smuggled Section"),
        "expected a named Section diagnostic, got {joined}"
    );
    assert!(
        matches!(
            report.focus_target(),
            Some(crate::playtest::PlaytestValidationTarget::Node(_))
        ),
        "the diagnostic must focus the offending node, got {:?}",
        report.focus_target()
    );
}

#[test]
fn a_cooked_bsp_package_carries_no_grid_spatial_state() {
    let (project, dir) = bsp_fixture();
    let (package, report) = crate::playtest::build_package(&project, &dir);
    assert!(report.is_ok(), "BSP fixture cooks: {:?}", report.errors);
    let package = package.expect("BSP package");

    assert!(package.chunks.is_empty(), "no room chunks");
    assert!(
        package.room_visibility.is_empty(),
        "no room visibility rows"
    );
    assert!(package.visibility_cells.is_empty(), "no visibility cells");
    assert!(package.visibility_pvs.is_empty(), "no visibility PVS rows");
    assert!(package.room_surface_caches.is_empty(), "no surface caches");
    assert!(package.room_portals.is_empty(), "no room portals");
    assert!(package.room_floor_links.is_empty(), "no floor links");
    assert!(package.water_cells.is_empty(), "no grid water cells");
    assert!(
        !package
            .assets
            .iter()
            .any(|asset| asset.kind == crate::playtest::PlaytestAssetKind::RoomWorld),
        "no PSXW world assets"
    );
    // The deliberate exception: one non-spatial metadata room carrying
    // gravity/camera/sky/fog, with no world geometry behind it. See
    // docs/quake-psoxide-convergence-handoff.md section 0.10.
    assert_eq!(package.rooms.len(), 1);
    assert_eq!(package.rooms[0].world_asset_index, None);
    assert!(matches!(
        package.world_geometry,
        crate::playtest::PlaytestWorldGeometry::Pxbsp(_)
    ));
}

#[test]
fn the_editor_room_topology_overlay_is_empty_for_a_bsp_project() {
    let (project, _dir) = bsp_fixture();
    let topology = crate::playtest::build_debug_topology(&project);
    assert!(topology.cells.is_empty());
    assert!(topology.portals.is_empty());
}
