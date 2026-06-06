use super::*;

#[test]
fn collect_entity_bounds_covers_starter_scene_entities() {
    let workspace = EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    let bounds = workspace.collect_entity_bounds(workspace.active_room_id());
    assert!(
        !bounds.is_empty(),
        "starter scene should expose at least one selectable entity bound"
    );
    let scene = workspace.project.active_scene();
    // The starter fixture should expose at least one Entity
    // bound in the active Room with a positive half-extent
    // on every axis.
    let spawn = starter_player_entity(scene);
    let spawn_bound = bounds
        .iter()
        .find(|b| b.node == spawn.id)
        .expect("player entity bound was emitted");
    assert!(matches!(
        spawn_bound.kind,
        EntityBoundKind::Model | EntityBoundKind::MeshFallback
    ));
    assert!(spawn_bound.half_extents[0] > 0.0);
    assert!(spawn_bound.half_extents[1] > 0.0);
    assert!(spawn_bound.half_extents[2] > 0.0);
}

#[test]
fn selecting_character_component_uses_parent_entity_bounds() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    let scene = workspace.project.active_scene();
    let entity = starter_player_entity(scene);
    let controller = entity
        .children
        .iter()
        .copied()
        .find(|id| {
            scene
                .node(*id)
                .is_some_and(|node| matches!(node.kind, NodeKind::CharacterController { .. }))
        })
        .expect("starter player has a character controller");
    let entity_bounds = workspace
        .node_frame_bounds_3d(entity.id)
        .expect("entity has selectable bounds");

    workspace.replace_node_selection(controller);

    assert_eq!(workspace.selected_bounds_3d(), Some(entity_bounds));
}

#[test]
fn dropping_model_resource_creates_component_entity() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    let room = workspace.active_room_id().expect("starter has a room");
    let model_id = workspace
        .project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Model(_)))
        .expect("starter has a model")
        .id;

    workspace.drop_resource_at_room_hit(model_id, room, [512.0, 0.0, 512.0], None);

    let scene = workspace.project.active_scene();
    let entity = scene
        .node(workspace.selection.selected_node)
        .expect("new entity is selected");
    assert!(matches!(entity.kind, NodeKind::Entity));
    assert!(entity.children.iter().any(|id| {
        scene.node(*id).is_some_and(|child| {
            matches!(
                child.kind,
                NodeKind::ModelRenderer {
                    model: Some(id),
                    ..
                } if id == model_id
            )
        })
    }));
    assert!(entity.children.iter().any(|id| {
        scene
            .node(*id)
            .is_some_and(|child| matches!(child.kind, NodeKind::Animator { .. }))
    }));
    assert!(workspace.is_dirty());
}

#[test]
fn dropping_character_resource_creates_entity_components() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    let room = workspace.active_room_id().expect("starter has a room");
    let character_id = workspace
        .project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Character(_)))
        .expect("starter has a character")
        .id;

    workspace.drop_resource_at_room_hit(character_id, room, [512.0, 0.0, 512.0], None);

    let scene = workspace.project.active_scene();
    let entity = scene
        .node(workspace.selection.selected_node)
        .expect("new entity is selected");
    assert!(matches!(entity.kind, NodeKind::Entity));
    assert!(entity.children.iter().any(|id| {
        scene.node(*id).is_some_and(|child| {
            matches!(
                child.kind,
                NodeKind::CharacterController {
                    character: Some(id),
                    player: false,
                    ..
                } if id == character_id
            )
        })
    }));
    assert!(!entity.children.iter().any(|id| {
        scene
            .node(*id)
            .is_some_and(|child| matches!(child.kind, NodeKind::Collider { .. }))
    }));
    assert!(workspace.is_dirty());
}

#[test]
fn dropping_weapon_resource_creates_equipment_entity() {
    let mut project = ProjectDocument::new("weapon-drop");
    let weapon = project.add_resource(
        "Practice Sword",
        ResourceData::Weapon(psxed_project::WeaponResource {
            default_character_socket: "right_hand_grip".to_string(),
            grip: psxed_project::WeaponGrip {
                name: "grip".to_string(),
                ..psxed_project::WeaponGrip::default()
            },
            ..psxed_project::WeaponResource::default()
        }),
    );
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Room {
            grid: WorldGrid::empty(2, 2, 1024),
        },
    );
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);

    workspace.drop_resource_at_room_hit(weapon, room, [512.0, 0.0, 512.0], None);

    let scene = workspace.project.active_scene();
    let entity = scene
        .node(workspace.selection.selected_node)
        .expect("new entity is selected");
    assert!(matches!(entity.kind, NodeKind::Entity));
    assert!(entity.children.iter().any(|id| {
        scene.node(*id).is_some_and(|child| {
            matches!(
                &child.kind,
                NodeKind::Equipment {
                    weapon: Some(id),
                    character_socket,
                    weapon_grip,
                } if *id == weapon
                    && character_socket == "right_hand_grip"
                    && weapon_grip == "grip"
            )
        })
    }));
    assert!(workspace.is_dirty());
}

#[test]
fn attachment_socket_issue_counts_catches_authoring_errors() {
    let sockets = vec![
        psxed_project::AttachmentSocket {
            name: "right_hand_grip".to_string(),
            joint: 2,
            translation: [0, 0, 0],
            rotation_q12: [0, 0, 0],
        },
        psxed_project::AttachmentSocket {
            name: "Right_Hand_Grip".to_string(),
            joint: 8,
            translation: [0, 0, 0],
            rotation_q12: [0, 0, 0],
        },
        psxed_project::AttachmentSocket {
            name: " ".to_string(),
            joint: 0,
            translation: [0, 0, 0],
            rotation_q12: [0, 0, 0],
        },
    ];

    assert_eq!(
        attachment_socket_issue_counts(&sockets, Some(4)),
        AttachmentSocketIssueCounts {
            empty_names: 1,
            duplicate_names: 1,
            invalid_joints: 1,
        }
    );
}

#[test]
fn weapon_attachment_summary_reports_socket_and_reach() {
    let weapon = psxed_project::WeaponResource {
        model: None,
        default_character_socket: "missing_socket".to_string(),
        grip: psxed_project::WeaponGrip {
            name: "grip".to_string(),
            translation: [0, 0, 0],
            rotation_q12: [0, 0, 0],
        },
        hitboxes: vec![psxed_project::WeaponHitbox {
            name: "blade".to_string(),
            shape: psxed_project::WeaponHitShape::Capsule {
                start: [0, 0, 0],
                end: [0, 640, 0],
                radius: 32,
            },
            active_start_frame: 4,
            active_end_frame: 12,
        }],
    };

    let summary = weapon_attachment_summary(&weapon, &["right_hand_grip".to_string()]);
    assert_eq!(summary.hitbox_count, 1);
    assert_eq!(summary.active_window_label, "4..12");
    assert_eq!(summary.max_reach, 672);
    assert!(summary
        .warnings
        .iter()
        .any(|warning| warning.contains("missing_socket")));
    assert!(summary
        .warnings
        .iter()
        .any(|warning| warning.contains("visual model")));
}

#[test]
fn component_templates_filter_by_host_kind_and_singletons() {
    let entity_options = component_templates_for_host(&NodeKind::Entity);
    assert!(entity_options
        .iter()
        .any(|(_, kind)| matches!(kind, NodeKind::ModelRenderer { .. })));
    assert!(entity_options
        .iter()
        .any(|(_, kind)| matches!(kind, NodeKind::CharacterController { .. })));
    assert!(entity_options
        .iter()
        .any(|(_, kind)| matches!(kind, NodeKind::PhysicsBody { .. })));

    let entity_existing = [
        NodeKind::CharacterController {
            character: None,
            settings: CharacterControllerSettings::default(),
            player: false,
        },
        NodeKind::PhysicsBody {
            settings: PhysicsBodySettings::default(),
        },
    ];
    let existing_refs: Vec<&NodeKind> = entity_existing.iter().collect();
    let entity_options = addable_component_templates(&NodeKind::Entity, &existing_refs);
    assert!(!entity_options
        .iter()
        .any(|(_, kind)| matches!(kind, NodeKind::CharacterController { .. })));
    assert!(!entity_options
        .iter()
        .any(|(_, kind)| matches!(kind, NodeKind::PhysicsBody { .. })));
    assert!(entity_options
        .iter()
        .any(|(_, kind)| matches!(kind, NodeKind::Equipment { .. })));
    assert!(entity_options
        .iter()
        .any(|(_, kind)| matches!(kind, NodeKind::Interactable { .. })));
    assert!(entity_options
        .iter()
        .all(|(label, _)| !matches!(*label, "AI Controller" | "Combat")));
    assert!(!entity_options
        .iter()
        .any(|(_, kind)| matches!(kind, NodeKind::Collider { .. })));
}

#[test]
fn scene_graph_add_menu_is_structure_only() {
    let addable = scene_graph_addable_kinds();
    assert_eq!(addable.len(), 3);
    assert!(addable
        .iter()
        .any(|(label, kind)| *label == "Room" && matches!(kind, NodeKind::Room { .. })));
    assert!(addable
        .iter()
        .any(|(label, kind)| *label == "Entity" && matches!(kind, NodeKind::Entity)));
    assert!(addable
        .iter()
        .any(|(label, kind)| *label == "Folder" && matches!(kind, NodeKind::Node)));
    assert!(addable.iter().all(|(_, kind)| !kind.is_component()));
    assert!(addable
        .iter()
        .all(|(label, _)| !matches!(*label, "Trigger" | "Audio Source")));
    assert!(!addable
        .iter()
        .any(|(_, kind)| matches!(kind, NodeKind::MeshInstance { .. })));
}

#[test]
fn add_component_to_host_creates_child_and_selects_it() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    let room = workspace.active_room_id().expect("starter has room");
    let entity = workspace
        .project
        .active_scene_mut()
        .add_node(room, "Enemy", NodeKind::Entity);

    let controller = workspace
        .add_component_to_host(
            entity,
            "Character Controller",
            NodeKind::CharacterController {
                character: None,
                settings: CharacterControllerSettings::default(),
                player: false,
            },
        )
        .expect("component is added");

    let scene = workspace.project.active_scene();
    assert_eq!(workspace.selection.selected_node, controller);
    assert!(scene.node(entity).unwrap().children.contains(&controller));
    assert!(matches!(
        scene.node(controller).unwrap().kind,
        NodeKind::CharacterController { .. }
    ));
    assert!(workspace.is_dirty());
}

#[test]
fn add_room_child_creates_three_by_three_floor_with_first_material() {
    let mut project = ProjectDocument::new("new-room");
    let material = project.add_resource(
        "First Material",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    project.add_resource(
        "Second Material",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let world = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "World",
        NodeKind::World {
            sector_size: 1536,
            sky: SkySettings::default(),
            far_vista: FarVistaSettings::default(),
            camera: WorldCameraSettings::default(),
            culling: WorldCullingSettings::default(),
            streaming: WorldStreamingSettings::default(),
            physics: WorldPhysicsSettings::default(),
        },
    );
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.replace_node_selection(world);

    workspace.add_child(
        NodeKind::Room {
            grid: WorldGrid::empty(9, 9, 1024),
        },
        "Room",
    );

    let room = workspace.selection.selected_node;
    let scene = workspace.project.active_scene();
    let node = scene.node(room).expect("new room exists");
    let NodeKind::Room { grid } = &node.kind else {
        panic!("added node should be a room");
    };
    assert_eq!(node.parent, Some(world));
    assert_eq!((grid.width, grid.depth), (3, 3));
    assert_eq!(grid.sector_size, 1536);
    assert_eq!(grid.sectors.iter().flatten().count(), 9);
    for sector in grid.sectors.iter().flatten() {
        let floor = sector.floor.as_ref().expect("starter sector has floor");
        assert_eq!(floor.material, Some(material));
        assert!(sector.ceiling.is_none());
    }
    assert!(workspace.is_dirty());
}

#[test]
fn dropping_first_character_profile_creates_player_controller() {
    let mut project = ProjectDocument::new("drop-character");
    let character = project.add_resource(
        "Hero",
        ResourceData::Character(psxed_project::CharacterResource::defaults()),
    );
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Room {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);

    workspace.drop_resource_at_room_hit(character, room, [0.0, 0.0, 0.0], None);

    let entity = workspace.selection.selected_node;
    let scene = workspace.project.active_scene();
    let node = scene.node(entity).expect("character entity exists");
    assert_eq!(node.parent, Some(room));
    let controller = node
        .children
        .iter()
        .filter_map(|id| scene.node(*id))
        .find_map(|child| match child.kind {
            NodeKind::CharacterController {
                character, player, ..
            } => Some((character, player)),
            _ => None,
        })
        .expect("character entity has controller component");
    assert_eq!(controller, (Some(character), true));
    assert!(workspace.status.contains("Player Character Entity"));
}

#[test]
fn dropping_character_profile_stays_non_player_when_player_exists() {
    let mut project = ProjectDocument::new("drop-npc");
    let character = project.add_resource(
        "NPC",
        ResourceData::Character(psxed_project::CharacterResource::defaults()),
    );
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Room {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    project.active_scene_mut().add_node(
        room,
        "Player Spawn",
        NodeKind::SpawnPoint {
            player: true,
            character: None,
        },
    );
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);

    workspace.drop_resource_at_room_hit(character, room, [0.0, 0.0, 0.0], None);

    let entity = workspace.selection.selected_node;
    let scene = workspace.project.active_scene();
    let controller = scene
        .node(entity)
        .expect("character entity exists")
        .children
        .iter()
        .filter_map(|id| scene.node(*id))
        .find_map(|child| match child.kind {
            NodeKind::CharacterController { player, .. } => Some(player),
            _ => None,
        })
        .expect("character entity has controller component");
    assert!(!controller);
}

#[test]
fn player_source_demote_handles_spawn_points_and_character_controllers() {
    let mut project = ProjectDocument::new("player-source-demote");
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Room {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    let spawn = project.active_scene_mut().add_node(
        room,
        "Legacy Player",
        NodeKind::SpawnPoint {
            player: true,
            character: None,
        },
    );
    let entity = project
        .active_scene_mut()
        .add_node(room, "Entity Player", NodeKind::Entity);
    let controller = project.active_scene_mut().add_node(
        entity,
        "Character Controller",
        NodeKind::CharacterController {
            character: None,
            settings: CharacterControllerSettings::default(),
            player: true,
        },
    );
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);

    workspace.demote_player_sources_except(Some(controller));

    let scene = workspace.project.active_scene();
    assert!(matches!(
        scene.node(spawn).unwrap().kind,
        NodeKind::SpawnPoint { player: false, .. }
    ));
    assert!(matches!(
        scene.node(controller).unwrap().kind,
        NodeKind::CharacterController { player: true, .. }
    ));
}

#[test]
fn character_controller_player_toggle_demotes_existing_player_source() {
    let mut project = ProjectDocument::new("player-source-toggle");
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Room {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    let spawn = project.active_scene_mut().add_node(
        room,
        "Legacy Player",
        NodeKind::SpawnPoint {
            player: true,
            character: None,
        },
    );
    let entity = project
        .active_scene_mut()
        .add_node(room, "Wraith", NodeKind::Entity);
    let controller = project.active_scene_mut().add_node(
        entity,
        "Character Controller",
        NodeKind::CharacterController {
            character: None,
            settings: CharacterControllerSettings::default(),
            player: false,
        },
    );
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);

    workspace.set_character_controller_player_controlled(controller, true);

    let scene = workspace.project.active_scene();
    assert!(matches!(
        scene.node(spawn).unwrap().kind,
        NodeKind::SpawnPoint { player: false, .. }
    ));
    assert!(matches!(
        scene.node(controller).unwrap().kind,
        NodeKind::CharacterController { player: true, .. }
    ));
    assert!(workspace.is_dirty());
}

#[test]
fn pick_entity_bound_returns_node_when_ray_hits_centre() {
    let workspace = EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    let bounds = workspace.collect_entity_bounds(workspace.active_room_id());
    let target = bounds
        .iter()
        .find(|b| {
            matches!(
                b.kind,
                EntityBoundKind::Model | EntityBoundKind::MeshFallback
            )
        })
        .copied()
        .expect("starter player Entity produces a bound");
    // Cast a ray straight at the bound's centre from far
    // outside it; ray_intersects_aabb is the primitive
    // pick_entity_bound calls into.
    let origin = [
        target.center[0] - 4096.0,
        target.center[1],
        target.center[2],
    ];
    let dir = [1.0, 0.0, 0.0];
    let t = ray_intersects_aabb(origin, dir, target.center, target.half_extents);
    assert!(t.is_some(), "ray straight at bound centre must hit");
}

#[test]
fn pick_entity_bound_includes_box_prop_bounds() {
    let mut project = ProjectDocument::new("box-prop-pick");
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Room {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    let prop = project.active_scene_mut().add_node(
        room,
        "Crate",
        NodeKind::BoxProp {
            materials: [None; psxed_project::BOX_PROP_FACE_COUNT],
            vertices: psxed_project::box_prop_vertices_for_size(1024),
            collision_enabled: true,
            break_flags: 0,
        },
    );
    let workspace = EditorWorkspace::with_project(test_temp_dir("box-prop-pick"), project);
    let bounds = workspace.collect_entity_bounds(Some(room));
    let target = bounds
        .iter()
        .find(|bound| bound.node == prop && bound.kind == EntityBoundKind::BoxProp)
        .copied()
        .expect("box prop produces a pickable entity bound");
    let origin = [
        target.center[0] - 4096.0,
        target.center[1],
        target.center[2],
    ];
    let dir = [1.0, 0.0, 0.0];
    let t = ray_intersects_aabb(origin, dir, target.center, target.half_extents);
    assert!(t.is_some(), "ray straight at box prop centre must hit");
}

#[test]
fn project_filesystem_rows_are_generated_from_resources() {
    let project = ProjectDocument::starter();
    let rows = project_filesystem_rows(&project);
    let texture_name = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Texture { .. }))
        .map(resource_file_name)
        .expect("starter project has a texture resource");
    let material_name = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Material(_)))
        .map(resource_file_name)
        .expect("starter project has a material resource");

    assert!(rows.iter().any(|row| row.name == "res://"));
    assert!(rows.iter().any(|row| row.name == "main.map"));
    assert!(rows.iter().any(|row| row.name == texture_name));
    assert!(rows.iter().any(|row| row.name == "characters"));
    assert!(rows
        .iter()
        .any(|row| row.name == "crimson_cross_knight_player.profile" && row.resource.is_some()));
    assert!(rows
        .iter()
        .any(|row| row.name == material_name && row.resource.is_some()));
}

#[test]
fn collapsed_project_filesystem_folder_hides_children() {
    let project = ProjectDocument::starter();
    let rows = project_filesystem_rows(&project);
    let material_name = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Material(_)))
        .map(resource_file_name)
        .expect("starter project has a material resource");
    let mut collapsed = HashSet::new();
    collapsed.insert("res://textures".to_string());

    let display_rows = project_filesystem_display_rows(&rows, "", &collapsed);

    assert!(display_rows.iter().any(|row| row.name == "textures"));
    assert!(!display_rows.iter().any(|row| row.name.ends_with(".psxt")));
    assert!(display_rows.iter().any(|row| row.name == material_name));
}

#[test]
fn compact_middle_keeps_long_asset_names_dock_sized() {
    let name = "meshy_ai_obsidian_wraith_biped_meshy_ai_meshy_merged_animations.psxmdl";
    let compact = compact_middle(name, 32);

    assert!(compact.chars().count() <= 32);
    assert!(compact.starts_with("meshy_ai"));
    assert!(compact.ends_with(".psxmdl"));
    assert!(compact.contains("..."));
}

#[test]
fn resource_filter_and_search_match_expected_resources() {
    let project = ProjectDocument::starter();
    let texture = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Texture { .. }))
        .unwrap();
    let material = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Material(_)))
        .unwrap();
    let texture_search = resource_search_token(texture);
    let material_search = resource_search_token(material);

    assert!(resource_matches_filter(
        texture,
        ResourceFilter::Texture,
        &texture_search
    ));
    assert!(!resource_matches_filter(
        texture,
        ResourceFilter::Material,
        &texture_search
    ));
    assert!(resource_matches_filter(
        material,
        ResourceFilter::Material,
        &material_search
    ));
}
