use super::*;

#[test]
fn ui_font_runtime_indices_follow_the_editor_font_table() {
    for (index, font) in UiFontChoice::ALL.iter().copied().enumerate() {
        assert_eq!(font.runtime_index(), index as u8, "{}", font.label());
    }
}

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
        CharacterCombatCapsule {
            name: "Right Palm Bolt".to_string(),
            joint: 14,
            capsule: JointCapsule {
                start: [30, 0, 0],
                end: [30, 0, 0],
                radius: 36,
            },
            role: CombatCapsuleRole::ProjectileEmitter {
                action: CharacterAnimationAction::LightAttack,
                charge_start_frame: 6,
                active_start_frame: 10,
                active_end_frame: 13,
                projectile: None,
                speed: 192,
                lifetime_ticks: 150,
                min_range: 600,
                max_range: 5000,
                damage: 26,
                poise_damage: 18,
                tint_rgb: [120, 210, 255],
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
fn normalize_loaded_removes_only_legacy_character_capsule_colliders() {
    let mut project = ProjectDocument::new("legacy-character-collider");
    let scene = project.active_scene_mut();
    let character = scene.add_node(scene.root, "Character", NodeKind::Entity);
    scene.add_node(
        character,
        "Character Controller",
        NodeKind::CharacterController {
            character: None,
            settings: Some(CharacterControllerSettings::default()),
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
fn dynamic_material_recipe_round_trips_through_project_ron() {
    let mut project = ProjectDocument::new("dynamic material persistence");
    let source_a = project.add_resource(
        "Frame A",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let source_b = project.add_resource(
        "Frame B",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
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
            source_a: Some(source_a),
            source_b: Some(source_b),
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
    material.sky_aperture = false;

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
    material.sky_aperture = true;
    let llm_recipe = MaterialVersionRecipe::from(&material);

    assert!(material.activate_version(MaterialVersionId::ORIGINAL));
    assert_eq!(material.active_version_name, "Original");
    assert_eq!(MaterialVersionRecipe::from(&material), original_recipe);
    assert!(!material.sky_aperture);
    assert!(material.activate_version(llm_version));
    assert_eq!(MaterialVersionRecipe::from(&material), llm_recipe);
    assert!(material.sky_aperture);

    assert!(material.rename_version(llm_version, "LLM Cathedral"));
    assert!(!material.rename_version(llm_version, "Original"));
    assert!(material.delete_version(llm_version));
    assert_eq!(material.active_version_id, MaterialVersionId::ORIGINAL);
    assert_eq!(material.version_count(), 1);
    assert!(!material.delete_version(MaterialVersionId::ORIGINAL));
}

#[test]
fn legacy_material_sky_migrates_to_one_world_sky_and_one_aperture_flag() {
    let mut project = ProjectDocument::new("legacy directional sky");
    let mut material = MaterialResource::opaque(Some("assets/sky.psxt".to_string()));
    material.directional_sky = true;
    let sky_material = project.add_resource("Legacy Sky", ResourceData::Material(material));
    let mut brush = crate::brush::Brush::cuboid([0, 0, 0], [128, 128, 128]);
    for face in &mut brush.faces {
        face.material = Some(sky_material);
    }
    project.active_scene_mut().brushes.push(brush);
    let root = project.active_scene().root;
    let NodeKind::World { sky, .. } = &mut project
        .active_scene_mut()
        .node_mut(root)
        .expect("world")
        .kind
    else {
        panic!("root is not a World");
    };
    sky.mode = SkyMode::Off;

    project.normalize_loaded();

    let ResourceData::Material(material) = &project.resource(sky_material).unwrap().data else {
        panic!("resource changed kind");
    };
    assert!(material.sky_aperture);
    assert!(!material.layered_sky);
    assert!(!material.directional_sky);
    let NodeKind::World { sky, .. } = &project.active_scene().node(root).unwrap().kind else {
        panic!("root is not a World");
    };
    assert_eq!(sky.mode, SkyMode::Cube);
    assert_eq!(sky.visibility, SkyVisibility::ThroughSkySurfaces);
    assert_eq!(sky.texture, Some(sky_material));
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
        (
            CharacterAnimationAction::VertLightAttack,
            "vert_light_attack",
        ),
        (
            CharacterAnimationAction::VertHeavyAttack,
            "vert_heavy_attack",
        ),
        (
            CharacterAnimationAction::VertComboAttack,
            "vert_combo_attack",
        ),
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
        assert_eq!(
            clip.psxanim_path,
            format!("assets/animations/gen/{stem}.psxanim")
        );
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
fn default_project_instantiates_the_single_animated_heavy_enemy() {
    let project = ProjectDocument::from_ron_str(DEFAULT_PROJECT_RON).unwrap();
    let tank_resource = project
        .resources
        .iter()
        .find(|resource| resource.name == "Heavy Enemy")
        .expect("default project catalogues the boss enemy");
    let ResourceData::Character(tank) = &tank_resource.data else {
        panic!("Heavy Enemy is a Character resource");
    };
    assert_eq!(tank.spawn_role, CharacterSpawnRole::Enemy);

    let model_id = tank.model.expect("Heavy Enemy character owns a model");
    let model_resource = project
        .resource(model_id)
        .expect("Heavy Enemy model exists");
    let ResourceData::Model(model) = &model_resource.data else {
        panic!("Heavy Enemy model binding points at a Model resource");
    };
    assert_eq!(model_resource.name, "Heavy Enemy Model");
    assert!(
        model.source_path.is_none(),
        "generated Heavy Enemy animation sources remain local-only"
    );
    assert!(default_project_dir().join(&model.model_path).is_file());
    assert!(model
        .texture_path
        .as_ref()
        .is_some_and(|path| default_project_dir().join(path).is_file()));

    let animation_set_id = tank
        .animation_set
        .expect("Heavy Enemy has an animation-set binding");
    let animation_set = project
        .resource(animation_set_id)
        .expect("Heavy Enemy animation set exists");
    let ResourceData::AnimationSet(animation_set) = &animation_set.data else {
        panic!("Heavy Enemy animation binding points at an Animation Set");
    };
    assert_eq!(
        animation_set.clips.len(),
        9,
        "idle, four-direction locomotion, attack, hit, stun and death ship"
    );
    for action in [
        CharacterAnimationAction::Idle,
        CharacterAnimationAction::Walk,
        CharacterAnimationAction::WalkBackward,
        CharacterAnimationAction::StrafeLeft,
        CharacterAnimationAction::StrafeRight,
        CharacterAnimationAction::LightAttack,
        CharacterAnimationAction::HitReact,
        CharacterAnimationAction::Stun,
        CharacterAnimationAction::Death,
    ] {
        let binding = animation_set
            .action_clips
            .iter()
            .find(|binding| binding.action == action)
            .unwrap_or_else(|| panic!("Heavy Enemy has a {action:?} binding"));
        let clip = project.resource(binding.clip).expect("bound clip exists");
        assert!(matches!(clip.data, ResourceData::AnimationClip(_)));
    }

    let scene = project.active_scene();
    let tank_entity = scene
        .nodes()
        .iter()
        .find(|node| node.name == "Heavy Enemy" && matches!(node.kind, NodeKind::Entity))
        .expect("animated Heavy Enemy is placed in the default level");
    assert!(tank_entity.children.iter().any(|child| {
        scene.node(*child).is_some_and(|node| {
            matches!(
                &node.kind,
                NodeKind::ModelRenderer {
                    model: Some(id), ..
                } if *id == model_id
            )
        })
    }));
    assert!(tank_entity.children.iter().any(|child| {
        scene.node(*child).is_some_and(|node| {
            matches!(
                &node.kind,
                NodeKind::CharacterController {
                    character: Some(id),
                    player: false,
                    ..
                } if *id == tank_resource.id
            )
        })
    }));

    for removed in [
        "Tank Boss Model",
        "Tank Boss Animation Set",
        "Tank Boss / Rest Pose",
        "Tank Boss Skeleton",
    ] {
        assert!(
            project
                .resources
                .iter()
                .all(|resource| resource.name != removed),
            "obsolete duplicate resource '{removed}' should be gone"
        );
    }

    let (package, report) = playtest::build_package(&project, &default_project_dir());
    assert!(
        report.is_ok(),
        "default project with the animated Heavy Enemy must cook: {:?}",
        report.errors
    );
    let package = package.expect("default project produces a playtest package");
    assert!(
        package.models.iter().any(|cooked| cooked.clip_count == 9),
        "the cooked Heavy Enemy model carries all nine selected clips"
    );
    assert!(
        package.game_entities.iter().any(|entity| {
            entity.flags & psx_level::game_entity_flags::RANGED_ATTACK != 0
                && entity.attack_min_range == 768 / crate::units::WORLD_UNIT_DIVISOR as u16
                && entity.attack_max_range == 4608 / crate::units::WORLD_UNIT_DIVISOR as u16
        }),
        "the placed Heavy Enemy cooks its projectile-driven AI attack band: {:?}",
        package
            .game_entities
            .iter()
            .map(|entity| (
                entity.flags,
                entity.attack_min_range,
                entity.attack_max_range
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn current_player_attack_slots_are_exactly_the_four_direct_shoulders() {
    assert_eq!(
        CharacterAnimationAction::PLAYER_ATTACKS,
        [
            CharacterAnimationAction::LightAttack,
            CharacterAnimationAction::HeavyAttack,
            CharacterAnimationAction::VertLightAttack,
            CharacterAnimationAction::VertHeavyAttack,
        ]
    );
    assert_eq!(
        CharacterAnimationAction::LightAttack.label(),
        "Horizon Light"
    );
    assert_eq!(
        CharacterAnimationAction::HeavyAttack.label(),
        "Horizon Heavy"
    );
    assert_eq!(
        CharacterAnimationAction::VertLightAttack.label(),
        "Zenith Light"
    );
    assert_eq!(
        CharacterAnimationAction::VertHeavyAttack.label(),
        "Zenith Heavy"
    );
    assert!(
        !CharacterAnimationAction::PLAYER_ATTACKS.contains(&CharacterAnimationAction::ComboAttack)
    );
    assert!(!CharacterAnimationAction::PLAYER_ATTACKS
        .contains(&CharacterAnimationAction::VertComboAttack));
}

#[test]
fn default_project_uses_one_complete_stun_clip_per_character() {
    assert_eq!(
        CharacterAnimationAction::guess_from_name("stun recovery"),
        Some(CharacterAnimationAction::Stun),
        "new imports should author the complete motion as one action"
    );
    assert!(
        !CharacterAnimationAction::AUTHORABLE.contains(&CharacterAnimationAction::StunRecovery),
        "the legacy split recovery slot must not appear in current authoring"
    );
    let project = ProjectDocument::from_ron_str(DEFAULT_PROJECT_RON).unwrap();
    for character_name in ["Aletha", "Light Enemy", "Heavy Enemy"] {
        let character = project
            .resources
            .iter()
            .find_map(|resource| match &resource.data {
                ResourceData::Character(character) if resource.name == character_name => {
                    Some(character)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {character_name} Character"));
        let set = character
            .animation_set
            .and_then(|id| project.resource(id))
            .and_then(|resource| match &resource.data {
                ResourceData::AnimationSet(set) => Some(set),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {character_name} Animation Set"));
        let stun = set
            .action_clip(CharacterAnimationAction::Stun)
            .unwrap_or_else(|| panic!("missing {character_name} Stun"));
        assert!(
            set.action_clip(CharacterAnimationAction::StunRecovery)
                .is_none(),
            "{character_name} must author its recovery inside Stun"
        );
        assert_ne!(
            Some(stun),
            set.action_clip(CharacterAnimationAction::HitReact),
            "{character_name} uses a dedicated complete stun/recovery one-shot"
        );
    }

    let bytes = std::fs::read(
        default_project_dir().join("assets/animations/aletha_delivered/aletha_stun.psxanim"),
    )
    .expect("read unified Aletha stun");
    let stun = psx_asset::Animation::from_bytes(&bytes).expect("parse unified Aletha stun");
    assert_eq!(
        (
            stun.joint_count(),
            stun.frame_count(),
            stun.sample_rate_hz()
        ),
        (26, 25, 12)
    );

    for (path, expected) in [
        (
            "assets/animations/rust_mantis_starter/stun_recovery.psxanim",
            (22, 12, 12),
        ),
        (
            "assets/animations/tank_boss_ai/stun_recovery.psxanim",
            (27, 24, 12),
        ),
    ] {
        let bytes = std::fs::read(default_project_dir().join(path))
            .unwrap_or_else(|error| panic!("read {path}: {error}"));
        let stun = psx_asset::Animation::from_bytes(&bytes)
            .unwrap_or_else(|error| panic!("parse {path}: {error:?}"));
        assert_eq!(
            (
                stun.joint_count(),
                stun.frame_count(),
                stun.sample_rate_hz(),
            ),
            expected,
            "{path} is the selected no-pause stun/recovery cook"
        );
    }
}

#[test]
fn starter_model_files_present_on_disk() {
    let root = default_project_dir();
    assert!(root
        .join("assets/models/aletha_delivered/aletha_delivered.psxmdl")
        .is_file());
    assert!(root
        .join("assets/models/aletha_delivered/aletha_delivered.psxt")
        .is_file());
    assert!(root
        .join("assets/animations/aletha_delivered/aletha_idle.psxanim")
        .is_file());
    assert!(root
        .join("assets/models/tank_boss_animated_model/tank_boss_animated_model.psxmdl")
        .is_file());
    assert!(root
        .join("assets/models/rust_mantis/rust_mantis.psxmdl")
        .is_file());
    assert!(root
        .join("assets/animations/tank_boss_ai/idle.psxanim")
        .is_file());
    let mantis_idle_path = root.join("assets/animations/rust_mantis_starter/idle.psxanim");
    assert!(mantis_idle_path.is_file());
    let mantis_idle_bytes = std::fs::read(&mantis_idle_path).expect("read Light Enemy idle");
    let mantis_idle =
        psx_asset::Animation::from_bytes(&mantis_idle_bytes).expect("parse Light Enemy idle");
    assert_eq!(mantis_idle.joint_count(), 22);
    assert_eq!(mantis_idle.frame_count(), 97);
    assert_eq!(mantis_idle.sample_rate_hz(), 12);
    for joint in 0..mantis_idle.joint_count() {
        assert_eq!(
            mantis_idle.pose(0, joint),
            mantis_idle.pose(mantis_idle.frame_count() - 1, joint),
            "Light Enemy idle must close exactly at joint {joint}"
        );
    }
}

#[test]
fn light_enemy_look_idle_is_installed_in_every_enemy_project() {
    let expected = std::fs::read(
        default_project_dir().join("assets/animations/rust_mantis_starter/idle.psxanim"),
    )
    .expect("read selected Light Enemy idle");

    for project_name in ["default", "mantis", "quake-e1m1-geometry", "tech-demo"] {
        let root = projects_dir().join(project_name);
        let idle_path = root.join("assets/animations/rust_mantis_starter/idle.psxanim");
        assert_eq!(
            std::fs::read(&idle_path).expect("read project Light Enemy idle"),
            expected,
            "{project_name} must carry the selected look-around idle"
        );

        let project = ProjectDocument::load_from_path(root.join("project.ron"))
            .unwrap_or_else(|error| panic!("load {project_name}: {error}"));
        let clip = project
            .resources
            .iter()
            .find_map(|resource| match &resource.data {
                ResourceData::AnimationClip(clip) if resource.name == "Light Enemy / Idle" => {
                    Some(clip)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("{project_name} has no Light Enemy idle resource"));
        assert!(clip.looping, "{project_name} idle must loop");
        assert_eq!(
            clip.tags,
            ["rust_mantis_idle_look_v2", "selected_idle_look_01"],
            "{project_name} must identify the selected look-around take"
        );
    }
}

#[test]
fn light_enemy_turn_and_alert_are_installed_in_every_enemy_project() {
    let default_root = default_project_dir();
    let expected_turn =
        std::fs::read(default_root.join("assets/animations/rust_mantis_starter/turn.psxanim"))
            .expect("read selected Light Enemy turn");
    let expected_alert =
        std::fs::read(default_root.join("assets/animations/rust_mantis_starter/alert.psxanim"))
            .expect("read selected Light Enemy alert");
    let turn = psx_asset::Animation::from_bytes(&expected_turn).expect("parse Light Enemy turn");
    let alert = psx_asset::Animation::from_bytes(&expected_alert).expect("parse Light Enemy alert");
    assert_eq!(
        (
            turn.joint_count(),
            turn.frame_count(),
            turn.sample_rate_hz()
        ),
        (22, 11, 12)
    );
    assert_eq!(
        (
            alert.joint_count(),
            alert.frame_count(),
            alert.sample_rate_hz()
        ),
        (22, 9, 12)
    );

    for project_name in ["default", "mantis", "quake-e1m1-geometry", "tech-demo"] {
        let root = projects_dir().join(project_name);
        assert_eq!(
            std::fs::read(root.join("assets/animations/rust_mantis_starter/turn.psxanim"))
                .expect("read project Light Enemy turn"),
            expected_turn,
            "{project_name} must carry the selected turn take"
        );
        assert_eq!(
            std::fs::read(root.join("assets/animations/rust_mantis_starter/alert.psxanim"))
                .expect("read project Light Enemy alert"),
            expected_alert,
            "{project_name} must carry the selected alert take"
        );

        let project = ProjectDocument::load_from_path(root.join("project.ron"))
            .unwrap_or_else(|error| panic!("load {project_name}: {error}"));
        let set = project
            .resources
            .iter()
            .find_map(|resource| match &resource.data {
                ResourceData::AnimationSet(set) if resource.name == "Light Enemy Animation Set" => {
                    Some(set)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("{project_name} has no Light Enemy animation set"));
        assert!(
            set.action_clip(CharacterAnimationAction::Turn).is_some(),
            "{project_name} must bind the tracking turn"
        );
        assert!(
            set.action_clip(CharacterAnimationAction::Intro).is_some(),
            "{project_name} must bind the first-acquisition alert"
        );
    }
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
    assert!(default_project_dir()
        .join("assets/models/tank_boss_animated_model/tank_boss_animated_model.psxmdl")
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
    let project = ProjectDocument::starter();

    assert_eq!(project.scenes.len(), 1);
    // Starter includes BSP geometry plus gameplay resources for the animated
    // character and weapon path.
    assert!(project.resources.len() >= 10);
    assert!(!project.active_scene().brushes.is_empty());
    assert!(project
        .active_scene()
        .nodes()
        .iter()
        .all(|node| !matches!(node.kind, NodeKind::Section { .. })));

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
        (188, 10, 75)
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
        (2000, 1000, 850)
    );

    let mantis = project
        .resources
        .iter()
        .find_map(|resource| match &resource.data {
            ResourceData::Character(character) if resource.name == "Light Enemy" => Some(character),
            _ => None,
        })
        .expect("starter includes the Light Enemy");
    assert_eq!(mantis.spawn_role, CharacterSpawnRole::Enemy);
    assert_eq!(mantis.walk_speed, 28);
    let enemy = mantis.enemy_behavior.expect("Mantis enemy behavior preset");
    assert_eq!(enemy.aggro_radius, 2335);
    assert_eq!(enemy.patrol_offset, [0, 0, -6000]);
    assert_eq!(enemy.reaction_ticks, 42);

    let mantis_idle = project
        .resources
        .iter()
        .find_map(|resource| match &resource.data {
            ResourceData::AnimationClip(clip) if resource.name == "Light Enemy / Idle" => {
                Some(clip)
            }
            _ => None,
        })
        .expect("starter includes the selected Light Enemy idle");
    assert_eq!(
        mantis_idle.psxanim_path,
        "assets/animations/rust_mantis_starter/idle.psxanim"
    );
    assert!(mantis_idle.looping);
    assert_eq!(
        mantis_idle.tags,
        ["rust_mantis_idle_look_v2", "selected_idle_look_01"]
    );
}

#[test]
fn project_missing_point_light_color_uses_default() {
    let light: NodeKind =
        ron::from_str("PointLight(intensity: 1.25, radius: 3.0)").expect("light parses");
    let NodeKind::PointLight { color, .. } = light else {
        panic!("parsed kind is a light");
    };
    assert_eq!(color, default_light_color());
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
fn brush_groups_normalize_and_remove_with_their_subtree() {
    let mut scene = Scene::new("Test");
    let group = scene.add_node(scene.root, "Architecture", NodeKind::Group);
    let nested = scene.add_node(group, "Stairs", NodeKind::Group);
    let not_a_group = scene.add_node(scene.root, "Entity", NodeKind::Entity);
    for owner in [
        Some(group),
        Some(nested),
        Some(not_a_group),
        Some(NodeId(99_999)),
    ] {
        let mut brush = brush::Brush::cuboid([0, 0, 0], [128, 128, 128]);
        brush.group = owner;
        scene.brushes.push(brush);
    }

    scene.normalize_brush_groups();
    assert_eq!(scene.brushes[0].group, Some(group));
    assert_eq!(scene.brushes[1].group, Some(nested));
    assert_eq!(scene.brushes[2].group, None);
    assert_eq!(scene.brushes[3].group, None);
    assert_eq!(scene.brush_indices_in_group(group, true), vec![0, 1]);

    assert!(scene.remove_node(group));
    assert!(scene.brushes.iter().all(|brush| brush.group.is_none()));
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
    let project = ProjectDocument::starter();
    let ron = project.to_ron_string().unwrap();

    assert!(ron.contains("Aletha"));
    assert_eq!(ProjectDocument::from_ron_str(&ron).unwrap(), project);
}

#[test]
fn point_of_interest_world_message_and_boost_module_roundtrip() {
    let mut project = ProjectDocument::new("messages");
    let scene = project.active_scene_mut();
    let root = scene.root;
    let NodeKind::World { world_message, .. } = &mut scene.node_mut(root).expect("world root").kind
    else {
        panic!("root must be a world");
    };
    *world_message = Some(WorldMessage {
        pages: vec![
            "The cortex stirs.".to_string(),
            "Ignition follows.".to_string(),
        ],
    });
    let host = scene.add_node(root, "Archive Beacon", NodeKind::Entity);
    scene.add_node(
        host,
        "Archive Beacon",
        NodeKind::PointOfInterest {
            pages: vec!["Recovered protocol.".to_string()],
            prompt: "READ".to_string(),
            radius: 576,
            marker_height: 192,
            repeatable: false,
            persistence_id: "archive-beacon-01".to_string(),
            reward: Some(PointOfInterestReward {
                module: None,
                quantity: 1,
                item_name: "Kinetic Relay".to_string(),
                description: "Amplifies Horizon attack output.".to_string(),
                modifiers: vec![BoostStatModifier {
                    stat: BoostStatKind::HorizonAttack,
                    percent: 15,
                }],
            }),
            enabled: true,
        },
    );

    let ron = project.to_ron_string().expect("serializes");
    let restored = ProjectDocument::from_ron_str(&ron).expect("deserializes");
    assert_eq!(restored, project);
}

#[test]
fn legacy_world_without_message_keeps_world_message_disabled() {
    let world: NodeKind = ron::from_str("World()")
        .expect("all World fields, including world_message, have compatible defaults");
    assert!(matches!(
        world,
        NodeKind::World {
            world_message: None,
            ..
        }
    ));
}

#[test]
fn deleting_boost_module_clears_point_of_interest_reward_reference() {
    let mut project = ProjectDocument::new("poi-reward-delete");
    let module = project.add_resource(
        "Surge Drive",
        ResourceData::BoostModule(BoostModuleResource {
            kind: BoostModuleKind::Surge,
            ..BoostModuleResource::default()
        }),
    );
    let scene = project.active_scene_mut();
    let host = scene.add_node(scene.root, "Beacon", NodeKind::Entity);
    let poi = scene.add_node(
        host,
        "Beacon",
        NodeKind::PointOfInterest {
            pages: default_message_pages(),
            prompt: default_point_of_interest_prompt(),
            radius: default_point_of_interest_radius(),
            marker_height: default_point_of_interest_marker_height(),
            repeatable: true,
            persistence_id: String::new(),
            reward: Some(PointOfInterestReward {
                module: Some(module),
                quantity: 1,
                ..PointOfInterestReward::default()
            }),
            enabled: true,
        },
    );

    project.delete_resource(module).expect("module exists");
    let NodeKind::PointOfInterest { reward, .. } =
        &project.active_scene().node(poi).expect("poi remains").kind
    else {
        panic!("poi remains authored");
    };
    assert_eq!(*reward, None);
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
    assert_eq!(ui_scene.nodes().len(), 1);
    assert!(project.resources.is_empty());
}

#[test]
fn ui_bar_frame_mapping_keeps_zero_empty_and_max_completely_full() {
    assert_eq!(ui_bar_frame_index(0, 4096, 7), 0);
    assert_eq!(ui_bar_frame_index(1, 4096, 7), 1);
    assert_eq!(ui_bar_frame_index(2048, 4096, 7), 3);
    assert_eq!(ui_bar_frame_index(4095, 4096, 7), 6);
    assert_eq!(ui_bar_frame_index(4096, 4096, 7), 6);
}

#[test]
fn deleting_a_sprite_bar_material_clears_the_ui_reference() {
    let mut project = ProjectDocument::new("ui resource deletion");
    let texture = project.add_resource(
        "Gauge",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let scene = project.active_ui_scene_mut().expect("HUD");
    let gauge = scene.add_node(
        scene.root,
        "Gauge",
        UiNodeKind::Bar {
            rect: UiRect::new(0, 0, 106, 29),
            value: UiValueBinding::PlayerHealth,
            max: UiValueBinding::PlayerHealthMax,
            texture: Some(texture),
            frame_count: 7,
            fill: [128, 128, 128],
            fill_gradient: None,
            background: [0, 0, 0],
            background_gradient: None,
        },
    );
    assert_eq!(project.resource_reference_count(texture), 1);
    let report = project.delete_resource(texture).expect("resource deleted");
    assert_eq!(report.cleared_references, 1);
    assert!(matches!(
        project
            .active_ui_scene()
            .and_then(|scene| scene.node(gauge))
            .map(|node| &node.kind),
        Some(UiNodeKind::Bar { texture: None, .. })
    ));
}

#[test]
fn default_project_uses_the_texture_free_twin_ladder_hud() {
    let project_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../projects/default/project.ron");
    let project = ProjectDocument::load_from_path(&project_path)
        .unwrap_or_else(|error| panic!("{}: {error}", project_path.display()));
    let hud = project
        .ui_scenes
        .iter()
        .find(|scene| scene.name == "HUD")
        .expect("default HUD scene");

    let bar = |name: &str| {
        hud.nodes()
            .iter()
            .find(|node| node.name == name)
            .and_then(|node| match node.kind {
                UiNodeKind::Bar {
                    rect,
                    value,
                    max,
                    texture,
                    frame_count,
                    ..
                } => Some((rect, value, max, texture, frame_count)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing geometric {name}"))
    };
    let horizon = bar("Horizon Fill");
    assert_eq!((horizon.0.width, horizon.0.height), (82, 2));
    assert_eq!(horizon.1, UiValueBinding::PlayerHealth);
    assert_eq!(horizon.2, UiValueBinding::PlayerHealthMax);
    assert_eq!((horizon.3, horizon.4), (None, 0));
    let zenith = bar("Zenith Fill");
    assert_eq!((zenith.0.width, zenith.0.height), (82, 2));
    assert_eq!(zenith.1, UiValueBinding::PlayerHealthSecondary);
    assert_eq!(zenith.2, UiValueBinding::PlayerHealthSecondaryMax);
    assert_eq!((zenith.3, zenith.4), (None, 0));

    for (name, text) in [("Horizon Name", "HRZ"), ("Zenith Name", "ZNT")] {
        let authored = hud
            .nodes()
            .iter()
            .find(|node| node.name == name)
            .and_then(|node| match &node.kind {
                UiNodeKind::Label { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {name}"));
        assert_eq!(authored, text);
    }

    for (name, y) in [("Horizon Shell", 12), ("Zenith Shell", 21)] {
        let (rect, shape) = hud
            .nodes()
            .iter()
            .find(|node| node.name == name)
            .and_then(|node| match node.kind {
                UiNodeKind::Rect {
                    rect,
                    shape: Some(shape),
                    ..
                } => Some((rect, shape)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {name}"));
        assert_eq!((rect.x, rect.y, rect.width, rect.height), (52, y, 88, 8));
        assert_eq!(shape.corner_cut, 2);
        assert!(shape.cut_top_left && shape.cut_bottom_right);
    }
    assert_eq!(
        hud.nodes()
            .iter()
            .filter(|node| node.name.contains(" Divider "))
            .count(),
        14
    );
    assert!(!project.resources.iter().any(|resource| {
        matches!(
            &resource.data,
            ResourceData::Material(material)
                if material
                    .psxt_path
                    .as_deref()
                    .is_some_and(|path| path.ends_with("health_bar_clean_slim.psxt"))
        )
    }));
}

#[test]
fn default_project_inventory_routes_four_continuous_vitality_poles() {
    let project_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../projects/default/project.ron");
    let project = ProjectDocument::load_from_path(&project_path)
        .unwrap_or_else(|error| panic!("{}: {error}", project_path.display()));
    let inventory = project
        .ui_scenes
        .iter()
        .find(|scene| scene.name == "Dual Vitality Inventory")
        .expect("Dual Vitality Inventory UI scene");

    for (name, action, tag) in [
        ("Horizon Empty Boost", 200, "boost.horizon.empty"),
        ("Horizon Full Boost", 201, "boost.horizon.full"),
        ("Zenith Empty Boost", 202, "boost.zenith.empty"),
        ("Zenith Full Boost", 203, "boost.zenith.full"),
        ("Inventory Item Slot 1", 210, "inventory.item.0"),
        ("Inventory Item Slot 2", 211, "inventory.item.1"),
        ("Inventory Item Slot 3", 212, "inventory.item.2"),
        ("Remove Socketed Module", 220, "boost.remove"),
    ] {
        let (authored_action, authored_tag) = inventory
            .nodes()
            .iter()
            .find(|node| node.name == name)
            .and_then(|node| match &node.kind {
                UiNodeKind::Button { action, tag, .. } => Some((action, tag.as_str())),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {name}"));
        assert_eq!(authored_action, &UiAction::Game(action));
        assert_eq!(authored_tag, tag);
    }

    for (name, expected_label) in [
        ("Horizon Empty Boost", "E // NONE"),
        ("Horizon Full Boost", "F // NONE"),
        ("Zenith Empty Boost", "E // NONE"),
        ("Zenith Full Boost", "F // NONE"),
        ("Inventory Item Slot 1", "MODULE 01"),
        ("Inventory Item Slot 2", "MODULE 02"),
        ("Inventory Item Slot 3", "MODULE 03"),
        ("Remove Socketed Module", "REMOVE"),
    ] {
        let label = inventory
            .nodes()
            .iter()
            .find(|node| node.name == name)
            .and_then(|node| match &node.kind {
                UiNodeKind::Button { label, .. } => Some(label.as_str()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {name}"));
        assert_eq!(label, expected_label, "{name} must match starter state");
    }

    for (name, value, flip_x) in [
        ("Horizon Health", UiValueBinding::PlayerHealth, true),
        (
            "Zenith Health",
            UiValueBinding::PlayerHealthSecondary,
            false,
        ),
    ] {
        let (rect, authored_value) = inventory
            .nodes()
            .iter()
            .find(|node| node.name == name)
            .and_then(|node| match node.kind {
                UiNodeKind::Bar { rect, value, .. } => Some((rect, value)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {name}"));
        assert_eq!(authored_value, value);
        assert_eq!(rect.flip_x, flip_x);
        assert_eq!((rect.width, rect.height), (133, 4));
    }

    for (name, value) in [
        (
            "Horizon Empty Influence",
            UiValueBinding::PlayerHealthEmptyInfluence,
        ),
        (
            "Horizon Full Influence",
            UiValueBinding::PlayerHealthFullInfluence,
        ),
        (
            "Zenith Empty Influence",
            UiValueBinding::PlayerHealthSecondaryEmptyInfluence,
        ),
        (
            "Zenith Full Influence",
            UiValueBinding::PlayerHealthSecondaryFullInfluence,
        ),
    ] {
        let (authored_value, max) = inventory
            .nodes()
            .iter()
            .find(|node| node.name == name)
            .and_then(|node| match node.kind {
                UiNodeKind::Bar { value, max, .. } => Some((value, max)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {name}"));
        assert_eq!(authored_value, value);
        assert_eq!(max, UiValueBinding::ConstantQ12(4096));
    }

    let (assignment_text, assignment_tag) = inventory
        .nodes()
        .iter()
        .find(|node| node.name == "Assignment Prompt")
        .and_then(|node| match &node.kind {
            UiNodeKind::Label { text, tag, .. } => Some((text.as_str(), tag.as_str())),
            _ => None,
        })
        .expect("Assignment Prompt");
    assert_eq!(assignment_text, "ASSIGN MODULE: CHOOSE SLOT");
    assert_eq!(assignment_tag, "boost.assignment.prompt");
    for (name, tag) in [
        ("Selected Module Base Effect", "boost.selected.base"),
        ("Horizon Attack Stat", "boost.stat.horizon"),
        ("Zenith Attack Stat", "boost.stat.zenith"),
        ("Defence Stat", "boost.stat.defence"),
        ("Movement Speed Stat", "boost.stat.movement"),
        ("Attack Speed Stat", "boost.stat.attack_speed"),
    ] {
        let authored_tag = inventory
            .nodes()
            .iter()
            .find(|node| node.name == name)
            .and_then(|node| match &node.kind {
                UiNodeKind::Label { tag, .. } => Some(tag.as_str()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {name}"));
        assert_eq!(authored_tag, tag);
    }
    let zenith_caption = inventory
        .nodes()
        .iter()
        .find(|node| node.name == "Zenith Pole Caption")
        .and_then(|node| match &node.kind {
            UiNodeKind::Label { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .expect("Zenith Pole Caption");
    assert!(zenith_caption.is_empty());

    for (name, endpoint_x) in [
        ("Horizon Full Trace", 19),
        ("Horizon Empty Trace", 151),
        ("Zenith Empty Trace", 168),
        ("Zenith Full Trace", 300),
    ] {
        let rect = inventory
            .nodes()
            .iter()
            .find(|node| node.name == name)
            .and_then(|node| match node.kind {
                UiNodeKind::Rect { rect, .. } => Some(rect),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {name}"));
        assert_eq!(rect.x, endpoint_x, "{name} must meet its health endpoint");
        assert_eq!((rect.y, rect.width, rect.height), (76, 2, 16));
    }

    assert!(inventory.nodes().iter().all(|node| !matches!(
        node.name.as_str(),
        "Inventory Title" | "Inventory Rule" | "Footer Line" | "Footer"
    )));
    for (name, tag) in [
        ("Inventory Player Tab", "tab.player.selected"),
        ("Inventory System Tab", "tab.system"),
    ] {
        let authored_tag = inventory
            .nodes()
            .iter()
            .find(|node| node.name == name)
            .and_then(|node| match &node.kind {
                UiNodeKind::Button { tag, .. } => Some(tag.as_str()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {name}"));
        assert_eq!(authored_tag, tag);
    }
    assert_eq!(
        inventory
            .nodes()
            .iter()
            .filter(|node| matches!(&node.kind, UiNodeKind::Button { tag, .. } if tag.starts_with("tab.")))
            .count(),
        2,
        "Player and System are the only real pause categories"
    );
    assert!(inventory.nodes().iter().all(|node| {
        !node.name.contains("Armament")
            && !matches!(&node.kind, UiNodeKind::Button { tag, action: UiAction::Game(301), .. } if tag == "tab.armament")
    }));
    for name in [
        "Inventory L1 Glyph Housing",
        "Inventory L1 Glyph Text",
        "Inventory R1 Glyph Housing",
        "Inventory R1 Glyph Text",
    ] {
        assert!(
            inventory.nodes().iter().any(|node| node.name == name),
            "missing {name}"
        );
    }
    let inventory_rail = inventory
        .nodes()
        .iter()
        .find(|node| node.name == "Inventory Tab Rail")
        .and_then(|node| match node.kind {
            UiNodeKind::Rect { rect, .. } => Some(rect),
            _ => None,
        })
        .expect("Inventory Tab Rail");
    assert_eq!(
        (
            inventory_rail.x,
            inventory_rail.y,
            inventory_rail.width,
            inventory_rail.height,
        ),
        (184, 8, 127, 29)
    );
    assert!(inventory
        .nodes()
        .iter()
        .all(|node| node.name != "Player Tab Glow"));

    for (name, expected_rect, expected_color) in [
        (
            "Collected Module Inset Rail",
            (15, 115, 112, 22),
            [126, 34, 26],
        ),
        (
            "Module Analysis Inset Rail",
            (133, 115, 172, 22),
            [118, 30, 25],
        ),
    ] {
        let (rect, color, shape) = inventory
            .nodes()
            .iter()
            .find(|node| node.name == name)
            .and_then(|node| match &node.kind {
                UiNodeKind::Rect {
                    rect,
                    color,
                    shape: Some(shape),
                    ..
                } => Some((rect, color, shape)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {name}"));
        assert_eq!(
            (rect.x, rect.y, rect.width, rect.height),
            expected_rect,
            "{name} must share its panel perimeter"
        );
        assert_eq!(*color, expected_color);
        assert_eq!(shape.corner_cut, 5);
        assert!(shape.cut_top_left);
        assert!(!shape.cut_top_right && !shape.cut_bottom_right && !shape.cut_bottom_left);
        assert_eq!(shape.border_width, 1);
        assert!(!shape.semi_transparent_fill);
    }

    for (name, expected_rect) in [
        ("Collected Module Inset Rail Rule", (15, 136, 112, 1)),
        ("Module Analysis Inset Rail Rule", (133, 136, 172, 1)),
    ] {
        let rect = inventory
            .nodes()
            .iter()
            .find(|node| node.name == name)
            .and_then(|node| match node.kind {
                UiNodeKind::Rect { rect, .. } => Some(rect),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {name}"));
        assert_eq!((rect.x, rect.y, rect.width, rect.height), expected_rect);
    }

    for name in ["Live World Dimmer", "Live World Quarter Scrim"] {
        let (rect, color, shape) = inventory
            .nodes()
            .iter()
            .find(|node| node.name == name)
            .and_then(|node| match &node.kind {
                UiNodeKind::Rect {
                    rect,
                    color,
                    shape: Some(shape),
                    ..
                } => Some((rect, color, shape)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {name}"));
        assert_eq!((rect.x, rect.y, rect.width, rect.height), (0, 0, 320, 240));
        assert_eq!(*color, [0, 0, 0]);
        assert!(shape.semi_transparent_fill);
        assert_eq!((shape.corner_cut, shape.border_width), (0, 0));
    }

    for name in [
        "Inventory Tab Rail",
        "Horizon Ladder Shell",
        "Zenith Ladder Shell",
        "Collected Module Panel",
        "Module Analysis Panel",
    ] {
        let shape = inventory
            .nodes()
            .iter()
            .find(|node| node.name == name)
            .and_then(|node| match &node.kind {
                UiNodeKind::Rect {
                    shape: Some(shape), ..
                } => Some(shape),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {name}"));
        assert!(
            !shape.semi_transparent_fill,
            "{name} must mask the world like the approved mockup"
        );
    }

    for name in [
        "Horizon Empty Boost",
        "Horizon Full Boost",
        "Zenith Empty Boost",
        "Zenith Full Boost",
    ] {
        let (focus_chrome, shape) = inventory
            .nodes()
            .iter()
            .find(|node| node.name == name)
            .and_then(|node| match &node.kind {
                UiNodeKind::Button {
                    focus_chrome,
                    shape: Some(shape),
                    ..
                } => Some((focus_chrome, shape)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {name}"));
        assert!(!focus_chrome, "{name} must remain visible when unfocused");
        assert!(
            !shape.semi_transparent_fill,
            "{name} must use the mockup's solid socket treatment"
        );
    }

    for (name, expected_y) in [
        ("Inventory Item Slot 1", 143),
        ("Inventory Item Slot 2", 166),
        ("Inventory Item Slot 3", 189),
    ] {
        let (rect, focus_chrome, font, font_scale) = inventory
            .nodes()
            .iter()
            .find(|node| node.name == name)
            .and_then(|node| match node.kind {
                UiNodeKind::Button {
                    rect,
                    focus_chrome,
                    font,
                    font_scale,
                    ..
                } => Some((rect, focus_chrome, font, font_scale)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {name}"));
        assert_eq!(
            (rect.x, rect.y, rect.width, rect.height),
            (23, expected_y, 96, 17)
        );
        assert!(
            focus_chrome,
            "{name} must only draw selection chrome while focused"
        );
        assert_eq!(font, UiFontChoice::Spleen5x8);
        assert_eq!(font_scale, 256, "{name} must use native-size 5x8 copy");
    }

    let empty_state = inventory
        .nodes()
        .iter()
        .find(|node| node.name == "Empty Inventory Message")
        .and_then(|node| match &node.kind {
            UiNodeKind::Label {
                text,
                tag,
                font,
                font_scale,
                ..
            } => Some((text.as_str(), tag.as_str(), *font, *font_scale)),
            _ => None,
        })
        .expect("Empty Inventory Message");
    assert_eq!(empty_state.0, "NO MODULES");
    assert_eq!(empty_state.1, "inventory.empty");
    assert_eq!(empty_state.2, UiFontChoice::Spleen5x8Italic);
    assert_eq!(empty_state.3, 256);

    assert!(
        inventory
            .nodes()
            .iter()
            .all(|node| node.name != "Assign Hint"),
        "the redundant X PICK // X SOCKET prompt must stay removed"
    );
}

#[test]
fn default_project_ui_controls_ship_with_cortex_sound_palette() {
    let project_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../projects/default/project.ron");
    let project = ProjectDocument::load_from_path(&project_path)
        .unwrap_or_else(|error| panic!("{}: {error}", project_path.display()));
    let project_root = project_path.parent().expect("default project root");
    let mut buttons = 0usize;
    let mut sliders = 0usize;

    for scene in &project.ui_scenes {
        for node in scene.nodes() {
            let pools: [&[UiSfxCue]; 4] = match &node.kind {
                UiNodeKind::Button { sfx, .. } => {
                    buttons += 1;
                    assert!(!sfx.focus.is_empty(), "{} needs focus SFX", node.name);
                    assert!(!sfx.activate.is_empty(), "{} needs press SFX", node.name);
                    [sfx.focus.as_slice(), sfx.activate.as_slice(), &[], &[]]
                }
                UiNodeKind::Slider { sfx, .. } => {
                    sliders += 1;
                    assert!(!sfx.focus.is_empty(), "{} needs focus SFX", node.name);
                    assert!(!sfx.nudge.is_empty(), "{} needs nudge SFX", node.name);
                    assert!(!sfx.limit.is_empty(), "{} needs limit SFX", node.name);
                    [
                        sfx.focus.as_slice(),
                        &[],
                        sfx.nudge.as_slice(),
                        sfx.limit.as_slice(),
                    ]
                }
                _ => continue,
            };
            for cue in pools.into_iter().flatten() {
                assert!(
                    cue.wav_path.starts_with("assets/audio/ui/"),
                    "{} uses a UI cue outside the shared palette: {}",
                    node.name,
                    cue.wav_path
                );
                assert!(
                    project_root.join(&cue.wav_path).is_file(),
                    "{} references missing UI cue {}",
                    node.name,
                    cue.wav_path
                );
            }
        }
    }

    assert_eq!(buttons, 16);
    assert_eq!(sliders, 12);
}

#[test]
fn default_project_system_overlay_matches_inventory_language_and_exposes_options() {
    let project_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../projects/default/project.ron");
    let project = ProjectDocument::load_from_path(&project_path)
        .unwrap_or_else(|error| panic!("{}: {error}", project_path.display()));
    let system_scene = project
        .ui_scenes
        .iter()
        .find(|scene| scene.name == "System Overlay")
        .expect("System Overlay UI scene");
    let gameplay = project
        .scene_states
        .iter()
        .find(|state| state.name == "Gameplay")
        .expect("Gameplay state");
    let inventory_state = project
        .scene_states
        .iter()
        .find(|state| state.name == "Inventory Overlay")
        .expect("Inventory Overlay state");
    let system = project
        .scene_states
        .iter()
        .find(|state| state.name == "System Overlay")
        .expect("System Overlay state");

    assert_eq!(gameplay.start_state, Some(inventory_state.id));
    assert_eq!(inventory_state.start_state, Some(gameplay.id));
    assert_eq!(system.start_state, Some(gameplay.id));
    assert_eq!(system.world, SceneWorldLayer::Gameplay);
    assert_eq!(system.ui_scene, Some(system_scene.id));
    assert!(system.ui_input);
    assert!(system.pause_world);
    assert!(project
        .scene_states
        .iter()
        .all(|state| state.name != "Paused Settings"));

    let node_names: HashSet<&str> = system_scene
        .nodes()
        .iter()
        .map(|node| node.name.as_str())
        .collect();
    assert!(node_names.contains("System Player Tab"));
    assert!(node_names.contains("System Tab"));
    assert!(node_names.iter().all(|name| !name.contains("Armament")));
    assert!(!node_names.contains("Settings"));
    assert!(node_names.contains("Return To Title"));
    assert_eq!(
        system_scene
            .default_focus
            .and_then(|id| system_scene.node(id))
            .map(|node| node.name.as_str()),
        Some("System Tab")
    );
    assert_eq!(
        system_scene
            .nodes()
            .iter()
            .filter(|node| matches!(&node.kind, UiNodeKind::Button { tag, .. } if tag.starts_with("tab.")))
            .count(),
        2
    );
    let system_rail = system_scene
        .nodes()
        .iter()
        .find(|node| node.name == "System Tab Rail")
        .and_then(|node| match node.kind {
            UiNodeKind::Rect { rect, .. } => Some(rect),
            _ => None,
        })
        .expect("System Tab Rail");
    assert_eq!(
        (
            system_rail.x,
            system_rail.y,
            system_rail.width,
            system_rail.height,
        ),
        (184, 8, 127, 29)
    );
    assert!(system_scene
        .nodes()
        .iter()
        .all(|node| node.name != "System Tab Glow"));

    let housing = |name: &str| {
        system_scene
            .nodes()
            .iter()
            .find(|node| node.name == name)
            .and_then(|node| match node.kind {
                UiNodeKind::Rect {
                    shape: Some(shape), ..
                } => Some(shape),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing geometric {name}"))
    };
    let left = housing("System L1 Glyph Housing");
    assert_eq!(left.corner_cut, 2);
    assert!(left.cut_top_left && left.cut_bottom_right);
    assert!(!left.cut_top_right && !left.cut_bottom_left);
    let right = housing("System R1 Glyph Housing");
    assert_eq!(right.corner_cut, 2);
    assert!(right.cut_top_right && right.cut_bottom_left);
    assert!(!right.cut_top_left && !right.cut_bottom_right);

    for name in ["System World Dimmer", "System World Quarter Scrim"] {
        let (rect, color, shape) = system_scene
            .nodes()
            .iter()
            .find(|node| node.name == name)
            .and_then(|node| match &node.kind {
                UiNodeKind::Rect {
                    rect,
                    color,
                    shape: Some(shape),
                    ..
                } => Some((rect, color, shape)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {name}"));
        assert_eq!((rect.x, rect.y, rect.width, rect.height), (0, 0, 320, 240));
        assert_eq!(*color, [0, 0, 0]);
        assert!(shape.semi_transparent_fill);
        assert_eq!((shape.corner_cut, shape.border_width), (0, 0));
    }

    for name in ["System Tab Rail", "System Panel"] {
        let frame = housing(name);
        assert!(!frame.semi_transparent_fill);
        assert_eq!(frame.corner_cut, 5);
        assert!(frame.cut_top_left && frame.cut_bottom_right);
    }
    assert!(system_scene
        .nodes()
        .iter()
        .all(|node| !node.name.contains("Session")));

    for (name, expected_rect) in [("System Inset Rail", (15, 49, 290, 22))] {
        let (rect, shape) = system_scene
            .nodes()
            .iter()
            .find(|node| node.name == name)
            .and_then(|node| match &node.kind {
                UiNodeKind::Rect {
                    rect,
                    shape: Some(shape),
                    ..
                } => Some((rect, shape)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {name}"));
        assert_eq!((rect.x, rect.y, rect.width, rect.height), expected_rect);
        assert_eq!((shape.corner_cut, shape.border_width), (5, 1));
        assert!(shape.cut_top_left);
        assert!(!shape.cut_top_right && !shape.cut_bottom_right && !shape.cut_bottom_left);
        assert!(!shape.semi_transparent_fill);
    }

    for (name, option) in [
        ("Screen X", OptionId(1)),
        ("Screen Y", OptionId(2)),
        ("Brightness", OptionId(6)),
        ("Stick Deadzone", OptionId(5)),
        ("Music Volume", OptionId(3)),
        ("SFX Volume", OptionId(4)),
    ] {
        let (authored_option, rect) = system_scene
            .nodes()
            .iter()
            .find(|node| node.name == name)
            .and_then(|node| match &node.kind {
                UiNodeKind::Slider { option, rect, .. } => Some((*option, *rect)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {name}"));
        assert_eq!(authored_option, option);
        assert_eq!((rect.x, rect.width, rect.height), (94, 201, 6));
    }
    let return_to_title = system_scene
        .nodes()
        .iter()
        .find(|node| node.name == "Return To Title")
        .and_then(|node| match node.kind {
            UiNodeKind::Button { rect, .. } => Some(rect),
            _ => None,
        })
        .expect("Return To Title");
    assert_eq!(
        (
            return_to_title.x,
            return_to_title.y,
            return_to_title.width,
            return_to_title.height,
        ),
        (94, 201, 201, 18),
        "Return to Title must be the final aligned row after SFX"
    );
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
fn screen_state_start_targets_survive_normalization_and_clear_when_invalid() {
    let mut project = ProjectDocument::new("pause flow");
    let gameplay = project
        .scene_states
        .iter()
        .find(|state| state.world == SceneWorldLayer::Gameplay)
        .expect("default gameplay state")
        .id;
    let pause = project.add_scene_state("Pause Menu");
    project.scene_state_mut(gameplay).unwrap().start_state = Some(pause);
    project.scene_state_mut(pause).unwrap().start_state = Some(gameplay);

    project.normalize_loaded();
    assert_eq!(
        project.scene_state(gameplay).unwrap().start_state,
        Some(pause)
    );
    assert_eq!(
        project.scene_state(pause).unwrap().start_state,
        Some(gameplay)
    );

    project.scene_state_mut(pause).unwrap().start_state = Some(pause);
    project.normalize_loaded();
    assert_eq!(project.scene_state(pause).unwrap().start_state, None);

    project.scene_state_mut(gameplay).unwrap().start_state = Some(pause);
    let pause_index = project
        .scene_states
        .iter()
        .position(|state| state.id == pause)
        .unwrap();
    assert!(project.remove_scene_state(pause_index));
    assert_eq!(project.scene_state(gameplay).unwrap().start_state, None);
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
            transparent: false,
            shape: None,
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
        show_brush_surface_grid: false,
        show_lights: false,
        preview_bounds: false,
        show_play_debug_overlays: false,
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
            weapon_appearance_tracks: Vec::new(),
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
    let mut project = ProjectDocument::new("mesh animation roundtrip");
    let scene = project.active_scene_mut();
    let model_resource_id = ResourceId(99);
    scene.add_node(
        NodeId::ROOT,
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
    let mut project = ProjectDocument::starter();
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
            settings: Some(CharacterControllerSettings::default()),
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
fn delete_material_source_clears_cycle_recipe() {
    let mut project = ProjectDocument::new("delete-cycle-source");
    let source = project.add_resource(
        "Source",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let mut cycle = MaterialResource::opaque(None);
    cycle.animation.mode = MaterialAnimationMode::Flipbook;
    cycle.animation.flipbook.source_a = Some(source);
    cycle.animation.flipbook.source_b = Some(source);
    let cycle = project.add_resource("Cycle", ResourceData::Material(cycle));

    assert_eq!(project.resource_reference_count(source), 2);
    let report = project.delete_resource(source).expect("source exists");
    assert_eq!(report.cleared_references, 2);
    let ResourceData::Material(material) = &project.resource(cycle).unwrap().data else {
        panic!("cycle remains a material");
    };
    assert_eq!(material.animation.flipbook.source_a, None);
    assert_eq!(material.animation.flipbook.source_b, None);
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
