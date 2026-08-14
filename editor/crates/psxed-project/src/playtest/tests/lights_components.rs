use super::*;

#[test]
fn starter_project_emits_one_light_record() {
    // Starter Stone Room ships with a "Preview Light" node.
    // It should now appear in `package.lights` with a
    // sensible intensity_q8 derived from the editor's
    // authored intensity float.
    let project = project_with_one_room();
    let expected_color = starter_light_color(&project);
    let expected_intensity_q8 = starter_light_intensity_q8(&project);
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("starter cooks");
    assert_eq!(package.lights.len(), 1);
    let light = package.lights[0];
    assert_eq!(light.room, 0);
    assert!(light.radius > 0);
    assert_eq!(light.intensity_q8, expected_intensity_q8);
    assert_eq!(light.color, expected_color);
}

#[test]
fn room_light_is_emitted_once_without_generated_splits() {
    let mut project = project_with_one_room();
    let floor_material = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Material(_)))
        .expect("starter has a room material")
        .id;
    let room_id = {
        let scene = project.active_scene();
        scene
            .nodes()
            .iter()
            .find(|n| matches!(n.kind, NodeKind::Section { .. }))
            .expect("starter has a room")
            .id
    };
    if let Some(room) = project.active_scene_mut().node_mut(room_id) {
        let NodeKind::Section { grid } = &mut room.kind else {
            panic!("starter room is a room");
        };
        *grid = crate::WorldGrid::empty(1, 16, crate::DEFAULT_WORLD_SECTOR_SIZE);
        for z in 0..grid.depth {
            grid.set_floor(0, z, 0, Some(floor_material));
        }
    }
    for id in starter_light_ids(&project) {
        let Some(light) = project.active_scene_mut().node_mut(id) else {
            continue;
        };
        light.transform.translation = [0.0, 0.0, 0.0];
        let NodeKind::PointLight { radius, .. } = &mut light.kind else {
            continue;
        };
        *radius = 2.0;
    }
    let player_character = player_character_resource_id(&project);
    demote_player_spawns(&mut project);
    let spawn_id = project.active_scene_mut().add_node(
        room_id,
        "Chunk Test Spawn",
        NodeKind::SpawnPoint {
            player: true,
            character: Some(player_character),
        },
    );
    if let Some(spawn) = project.active_scene_mut().node_mut(spawn_id) {
        spawn.transform.translation = [0.0, 0.0, 0.0];
    }

    let (package, report) = build_package(&project, &starter_project_root());

    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("cooks");
    assert_eq!(package.rooms.len(), 1);
    assert_eq!(package.lights.len(), 1);
    assert_eq!(package.lights[0].room, 0);
}

#[test]
fn floor_transition_wall_stays_in_single_manual_room() {
    let mut project = project_with_one_room();
    let floor_material = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Material(_)))
        .expect("starter has a room material")
        .id;
    let room_id = project
        .active_scene()
        .nodes()
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Section { .. }))
        .expect("starter has a room")
        .id;
    if let Some(room) = project.active_scene_mut().node_mut(room_id) {
        let NodeKind::Section { grid } = &mut room.kind else {
            panic!("starter room is a room");
        };
        *grid = crate::WorldGrid::empty(17, 1, crate::DEFAULT_WORLD_SECTOR_SIZE);
        for x in 0..grid.width {
            let height = if x < 16 { 0 } else { 512 };
            grid.set_floor(x, 0, height, Some(floor_material));
        }
    }
    let player = player_spawn_node_id(&project);
    if let Some(node) = project.active_scene_mut().node_mut(player) {
        node.transform.translation = [0.0, 0.0, 0.0];
    }

    let (package, report) = build_package(&project, &starter_project_root());

    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("cooks");
    assert_eq!(package.rooms.len(), 1);
    let mut transition_walls = 0usize;
    for room in &package.rooms {
        let world = psx_asset::World::from_bytes(
            &package.assets[room.world_asset_index.expect("grid room world asset")].bytes,
        )
        .expect("chunk psxw parses");
        for x in 0..world.width() {
            for z in 0..world.depth() {
                let Some(sector) = world.sector(x, z) else {
                    continue;
                };
                for local_wall in 0..sector.wall_count() {
                    let wall = world.sector_wall(sector, local_wall).expect("wall exists");
                    if wall.direction() == psxw::direction::EAST
                        && wall.heights() == [0, 0, 512, 512]
                    {
                        transition_walls += 1;
                    }
                }
            }
        }
    }
    assert_eq!(transition_walls, 1);
}

#[test]
fn starter_project_bakes_static_surface_lights() {
    let project = ProjectDocument::legacy_grid_starter();
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("starter cooks");
    let room = &package.rooms[0];
    let asset = &package.assets[room.world_asset_index.expect("grid room world asset")];
    let world = psx_asset::World::from_bytes(&asset.bytes).expect("room psxw parses");
    assert!(world.static_vertex_lighting());
    assert!((0..world.surface_light_count())
        .filter_map(|index| world.surface_light(index))
        .any(|light| light.vertex_rgb().iter().any(|rgb| *rgb != [0, 0, 0])));
}

#[test]
fn diagonal_walls_bake_static_surface_lights() {
    use crate::world_cook::{
        CookedGridSector, CookedGridVerticalFace, CookedGridWalls, DEFAULT_BAKED_VERTEX_RGB,
    };
    use crate::{MaterialFaceSidedness, PsxBlendMode};

    fn diagonal_wall(heights: [i32; 4]) -> CookedGridVerticalFace {
        CookedGridVerticalFace {
            heights,
            material: 0,
            shape: psxw::wall_shape::QUAD,
            uvs: psxw::WALL_UVS,
            baked_vertex_rgb: DEFAULT_BAKED_VERTEX_RGB,
            solid: true,
        }
    }

    let source = ProjectDocument::legacy_grid_starter().resources[0].id;
    let mut room = CookedRoomBakeInput {
        room_index: 0,
        world_asset_index: 0,
        world_origin: [0, 0],
        origin_y: 0,
        cooked: CookedWorldGrid {
            width: 1,
            depth: 1,
            sector_size: 1024,
            sectors: vec![Some(CookedGridSector {
                floor: None,
                ceiling: None,
                walls: CookedGridWalls {
                    north_west_south_east: vec![diagonal_wall([0, 16, 1024, 1008])],
                    north_east_south_west: vec![diagonal_wall([32, 48, 960, 944])],
                    ..CookedGridWalls::default()
                },
            })],
            materials: vec![CookedWorldMaterial {
                slot: 0,
                source,
                psxt_path: None,
                blend_mode: PsxBlendMode::Opaque,
                tint: [128, 128, 128],
                animation: crate::MaterialAnimation::default(),
                face_sidedness: MaterialFaceSidedness::Both,
            }],
            ambient_color: [32, 24, 16],
            static_vertex_lighting: true,
            fog_enabled: false,
            fog_color: [0, 0, 0],
            fog_near: 0,
            fog_far: 0,
        },
    };

    bake_static_surface_lights(std::slice::from_mut(&mut room), &[]);

    let sector = room.cooked.sectors[0].as_ref().expect("sector");
    let cases = [
        (
            psxw::direction::NORTH_WEST_SOUTH_EAST,
            &sector.walls.north_west_south_east[0],
        ),
        (
            psxw::direction::NORTH_EAST_SOUTH_WEST,
            &sector.walls.north_east_south_west[0],
        ),
    ];
    for (direction, wall) in cases {
        let verts =
            wall_vertices(0, 0, 1024, direction, wall.heights).expect("diagonal wall vertices");
        let expected = bake_surface_vertex_rgb(
            &room.cooked.materials,
            room.cooked.ambient_color,
            verts,
            wall.material,
            &[],
        );
        assert_ne!(expected, DEFAULT_BAKED_VERTEX_RGB);
        assert_eq!(wall.baked_vertex_rgb, expected);
    }
}

#[test]
fn billboard_image_props_bake_vertical_static_lighting() {
    let mut props = vec![PlaytestImageProp {
        room: 0,
        texture_asset_index: 0,
        x: 0,
        y: 0,
        z: 0,
        pitch: 0,
        yaw: 0,
        roll: 0,
        width: 128,
        height: 512,
        tint_rgb: [128, 128, 128],
        baked_vertex_rgb: [(128, 128, 128); 4],
        collision_min: [0; 3],
        collision_max: [0; 3],
        flags: image_prop_flags::CYLINDRICAL_BILLBOARD,
    }];
    let room = CookedRoomBakeInput {
        room_index: 0,
        world_asset_index: 0,
        world_origin: [0, 0],
        origin_y: 0,
        cooked: CookedWorldGrid {
            width: 0,
            depth: 0,
            sector_size: 1024,
            sectors: Vec::new(),
            materials: Vec::new(),
            ambient_color: [32, 32, 32],
            static_vertex_lighting: true,
            fog_enabled: false,
            fog_color: [0, 0, 0],
            fog_near: 0,
            fog_far: 0,
        },
    };
    let lights = [PlaytestLight {
        room: 0,
        x: 0,
        y: 512,
        z: 0,
        radius: 1024,
        intensity_q8: 256,
        color: [128, 128, 128],
    }];

    bake_static_image_prop_lights(&mut props, &[room], &lights);

    assert_eq!(props[0].baked_vertex_rgb[0], (160, 160, 160));
    assert_eq!(props[0].baked_vertex_rgb[1], (160, 160, 160));
    assert_eq!(props[0].baked_vertex_rgb[2], (96, 96, 96));
    assert_eq!(props[0].baked_vertex_rgb[3], (96, 96, 96));
}

#[test]
fn light_with_zero_radius_fails() {
    let mut project = ProjectDocument::legacy_grid_starter();
    insert_preview_light(&mut project);
    let ids = starter_light_ids(&project);
    let scene = project.active_scene_mut();
    for id in ids {
        if let Some(node) = scene.node_mut(id) {
            if let NodeKind::PointLight { radius, .. } = &mut node.kind {
                *radius = 0.0
            }
        }
    }
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(package.is_none());
    assert!(
        report.errors.iter().any(|e| e.contains("radius")),
        "errors: {:?}",
        report.errors,
    );
}

#[test]
fn light_with_negative_intensity_fails() {
    let mut project = ProjectDocument::legacy_grid_starter();
    insert_preview_light(&mut project);
    let ids = starter_light_ids(&project);
    let scene = project.active_scene_mut();
    for id in ids {
        if let Some(node) = scene.node_mut(id) {
            if let NodeKind::PointLight { intensity, .. } = &mut node.kind {
                *intensity = -0.5
            }
        }
    }
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(package.is_none());
    assert!(
        report.errors.iter().any(|e| e.contains("intensity")),
        "errors: {:?}",
        report.errors,
    );
}

#[test]
fn light_radius_converts_sectors_to_world_units() {
    // Author a 4-sector radius; cook stores world units using
    // the room's current sector size.
    let mut project = ProjectDocument::legacy_grid_starter();
    insert_preview_light(&mut project);
    let sector_size = project
        .active_scene()
        .nodes()
        .iter()
        .find_map(|node| match &node.kind {
            NodeKind::Section { grid } => Some(grid.sector_size),
            _ => None,
        })
        .expect("starter has a room");
    let ids = starter_light_ids(&project);
    let scene = project.active_scene_mut();
    for id in ids {
        if let Some(node) = scene.node_mut(id) {
            if let NodeKind::PointLight { radius, .. } = &mut node.kind {
                *radius = 4.0
            }
        }
    }
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("cooks");
    assert_eq!(package.lights[0].radius, (sector_size * 4) as u16);
}

#[test]
fn rendered_manifest_emits_lights_block() {
    let mut project = ProjectDocument::legacy_grid_starter();
    insert_preview_light(&mut project);
    let (package, _) = build_package(&project, &starter_project_root());
    let package = package.expect("cooks");
    let color = package.lights[0].color;
    let src = render_manifest_source(&package);
    assert!(src.contains("PointLightRecord"));
    assert!(src.contains("pub static LIGHTS"));
    assert!(!src.contains("SurfaceLightRecord"));
    assert!(!src.contains("SURFACE_LIGHTS"));
    assert!(src.contains("intensity_q8"));
    assert!(src.contains(&format!(
        "color: [{}, {}, {}]",
        color[0], color[1], color[2]
    )));
}

#[test]
fn interactable_component_emits_prompt_and_message_records() {
    let mut project = ProjectDocument::legacy_grid_starter();
    let scene = project.active_scene_mut();
    let room = scene
        .nodes()
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Section { .. }))
        .map(|n| n.id)
        .expect("starter has room");
    let entity = scene.add_node(room, "Echo Body", NodeKind::Entity);
    scene.add_node(
        entity,
        "Interactable",
        NodeKind::Interactable {
            kind: crate::InteractableKind::Message {
                title: "ECHO REMNANT".to_string(),
                body: "The signal breaks here.".to_string(),
            },
            prompt: "READ ECHO".to_string(),
            radius: 128,
            enabled: true,
        },
    );

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("cooks");
    assert_eq!(package.interactables.len(), 1);
    assert_eq!(package.interactable_messages.len(), 1);
    assert_eq!(package.interactables[0].prompt, "READ ECHO");
    assert_eq!(package.interactables[0].radius, 128);
    assert_eq!(package.interactable_messages[0].title, "ECHO REMNANT");

    let src = render_manifest_source(&package);
    assert!(src.contains("pub static INTERACTABLE_MESSAGES"));
    assert!(src.contains("pub static INTERACTABLES"));
    assert!(src.contains("InteractableKind::Message"));
    assert!(src.contains("READ ECHO"));
    assert!(src.contains("The signal breaks here."));
}

/// Synthetic player-with-equipment project shared by the weapon cook
/// tests: returns the document plus the Weapon resource id so tests
/// can author combat fields before cooking.
fn equipment_test_project() -> (ProjectDocument, crate::ResourceId) {
    let starter = ProjectDocument::legacy_grid_starter();
    let mut starter_model = starter
        .resources
        .iter()
        .find_map(|resource| match &resource.data {
            ResourceData::Model(model) => Some(model.clone()),
            _ => None,
        })
        .expect("starter has a model");
    let mut project = ProjectDocument::new("equipment-test");
    // A grid fixture: it authors a Section, not brushes, so it has to say
    // so. New documents are BSP by default and the cook fails closed on a
    // BSP project with no brush world.
    project.set_world_format(crate::ProjectWorldFormat::LegacyGrid);
    // Skeleton-scoped animation fixture so the model is renderable and
    // the player has idle/walk via its Animation Set.
    let skeleton = project.add_resource(
        "Skeleton",
        ResourceData::Skeleton(crate::SkeletonResource {
            joint_count: 1,
            parents: vec![None],
            signature: "equip-test".to_string(),
            note: String::new(),
            joint_names: Vec::new(),
        }),
    );
    starter_model.skeleton = Some(skeleton);
    let idle = project.add_resource(
        "idle",
        ResourceData::AnimationClip(crate::AnimationClipResource {
            psxanim_path: "assets/models/obsidian_wraith/obsidian_wraith_unsteady_walk.psxanim"
                .to_string(),
            skeleton: Some(skeleton),
            target_model: None,
            source: None,
            bake: crate::AnimationClipBakeKind::LegacyShared,
            role: crate::AnimationRole::Idle,
            looping: true,
            tags: Vec::new(),
            calibration: Default::default(),
            pose_corrections: Vec::new(),
        }),
    );
    let set = project.add_resource(
        "Set",
        ResourceData::AnimationSet(crate::AnimationSetResource {
            skeleton: Some(skeleton),
            idle_clip: Some(idle),
            walk_clip: Some(idle),
            ..crate::AnimationSetResource::defaults()
        }),
    );
    let material = project.add_resource(
        "Floor",
        ResourceData::Material(crate::MaterialResource::opaque(Some(
            "assets/textures/delven_01_slateflr1a_q2.psxt".to_string(),
        ))),
    );
    let model = project.add_resource("Wraith Model", ResourceData::Model(starter_model));
    let character = project.add_resource(
        "Wraith Character",
        ResourceData::Character(crate::CharacterResource {
            model: Some(model),
            animation_set: Some(set),
            ..crate::CharacterResource::defaults()
        }),
    );
    let weapon = project.add_resource(
        "Practice Sword",
        ResourceData::Weapon(crate::WeaponResource {
            model: Some(model),
            default_character_socket: "right_hand_grip".to_string(),
            grip: crate::WeaponGrip {
                name: "grip".to_string(),
                translation: [8, 16, 0],
                rotation_q12: [0, 1024, 0],
            },
            hitboxes: vec![crate::WeaponHitbox {
                name: "Blade".to_string(),
                shape: crate::WeaponHitShape::Capsule {
                    start: [0, 0, 0],
                    end: [0, 640, 0],
                    radius: 32,
                },
                active_start_frame: 4,
                active_end_frame: 9,
            }],
            ..crate::WeaponResource::default()
        }),
    );

    let scene = project.active_scene_mut();
    let mut grid = crate::WorldGrid::empty(2, 2, 1024);
    grid.set_floor(0, 0, 0, Some(material));
    grid.set_floor(1, 1, 0, Some(material));
    let room = scene.add_node(scene.root, "Room", NodeKind::Section { grid });
    let entity = scene.add_node(room, "Player", NodeKind::Entity);
    if let Some(node) = scene.node_mut(entity) {
        node.transform.translation = [0.5, 0.0, 0.5];
    }
    scene.add_node(
        entity,
        "Model Renderer",
        NodeKind::ModelRenderer {
            model: Some(model),
            material: None,
            visual_offset: [0; 3],
            visual_scale_q8: crate::MODEL_SCALE_ONE_Q8,
        },
    );
    scene.add_node(
        entity,
        "Animator",
        NodeKind::Animator {
            clip: Some(0),
            action_clips: Vec::new(),
            autoplay: true,
            pose_frame: 0,
        },
    );
    scene.add_node(
        entity,
        "Character Controller",
        NodeKind::CharacterController {
            character: Some(character),
            settings: CharacterControllerSettings::default(),
            player: true,
        },
    );
    scene.add_node(
        entity,
        "Equipment",
        NodeKind::Equipment {
            weapon: Some(weapon),
            character_socket: "right_hand_grip".to_string(),
            weapon_grip: "grip".to_string(),
        },
    );

    (project, weapon)
}

#[test]
fn equipment_component_emits_weapon_and_hitbox_records() {
    let (project, _) = equipment_test_project();

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("cooks");
    assert_eq!(package.weapons.len(), 1);
    assert_eq!(package.equipment.len(), 1);
    assert_eq!(package.weapon_hitboxes.len(), 1);
    assert_eq!(package.model_sockets.len(), 1);
    assert_eq!(package.models[0].socket_first, 0);
    assert_eq!(package.models[0].socket_count, 1);
    assert_eq!(package.model_sockets[0].name, "right_hand_grip");
    assert_eq!(package.model_sockets[0].joint, 0);
    assert_eq!(package.weapons[0].model, Some(0));
    assert_eq!(package.weapons[0].grip_translation, [8, 16, 0]);
    // Serde-default melee arc numbers cook as authored: reach 640,
    // 60 degrees -> 682 PSX angle units, damage/poise 25.
    assert_eq!(package.weapons[0].arc_reach, 640);
    assert_eq!(package.weapons[0].arc_half_angle, 682);
    assert_eq!(package.weapons[0].damage, 25);
    assert_eq!(package.weapons[0].poise_damage, 25);
    assert_eq!(package.equipment[0].weapon, 0);
    assert_eq!(
        package.equipment[0].flags & psx_level::equipment_flags::PLAYER,
        psx_level::equipment_flags::PLAYER
    );

    let src = render_manifest_source(&package);
    assert!(src.contains("pub static MODEL_SOCKETS"));
    assert!(src.contains("LevelModelSocketRecord"));
    assert!(src.contains("pub static WEAPONS"));
    assert!(src.contains("pub static EQUIPMENT"));
    assert!(src.contains("WeaponHitShapeRecord::Capsule"));
    assert!(src.contains("arc_reach: 640"));
    assert!(src.contains("arc_half_angle: 682"));
    assert!(src.contains("damage: 25"));
    assert!(src.contains("poise_damage: 25"));
}

#[test]
fn weapon_melee_arc_cooks_authored_values() {
    let (mut project, weapon) = equipment_test_project();
    if let Some(resource) = project.resource_mut(weapon) {
        if let ResourceData::Weapon(weapon) = &mut resource.data {
            weapon.arc_reach = 704;
            weapon.arc_half_angle_degrees = 90;
            weapon.damage = 30;
            weapon.poise_damage = 40;
        }
    }
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("cooks");
    assert_eq!(package.weapons[0].arc_reach, 704);
    // 90 degrees = a quarter turn = 1024 PSX angle units, exactly.
    assert_eq!(package.weapons[0].arc_half_angle, 1024);
    assert_eq!(package.weapons[0].damage, 30);
    assert_eq!(package.weapons[0].poise_damage, 40);
}

#[test]
fn weapon_melee_arc_rejects_zero_reach_zero_damage_and_degenerate_angles() {
    type WeaponCase = (&'static str, fn(&mut crate::WeaponResource));
    let cases: [WeaponCase; 4] = [
        ("arc reach 0", |weapon| weapon.arc_reach = 0),
        ("damage 0", |weapon| weapon.damage = 0),
        ("half-angle 0", |weapon| weapon.arc_half_angle_degrees = 0),
        ("half-angle 171", |weapon| {
            weapon.arc_half_angle_degrees = 171
        }),
    ];
    for (label, mutate) in cases {
        let (mut project, weapon) = equipment_test_project();
        if let Some(resource) = project.resource_mut(weapon) {
            if let ResourceData::Weapon(weapon) = &mut resource.data {
                mutate(weapon);
            }
        }
        let (package, report) = build_package(&project, &starter_project_root());
        assert!(
            package.is_none(),
            "{label}: a combat-dead weapon must fail the cook loudly"
        );
        assert!(
            report
                .errors
                .iter()
                .any(|error| error.contains("Weapon 'Practice Sword'")),
            "{label}: expected a weapon arc error, got {:?}",
            report.errors
        );
    }
}
