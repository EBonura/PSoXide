use super::*;

#[test]
fn collect_entity_bounds_covers_starter_scene_entities() {
    let workspace = EditorWorkspace::open_directory(psxed_project::legacy_grid_starter_dir()).unwrap();
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
        EditorWorkspace::open_directory(psxed_project::legacy_grid_starter_dir()).unwrap();
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
        EditorWorkspace::open_directory(psxed_project::legacy_grid_starter_dir()).unwrap();
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
        EditorWorkspace::open_directory(psxed_project::legacy_grid_starter_dir()).unwrap();
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
        NodeKind::Section {
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
        ..psxed_project::WeaponResource::default()
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
            settings: Some(CharacterControllerSettings::default()),
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
fn character_controller_role_is_derived_from_existing_controller_state() {
    let mut settings = CharacterControllerSettings::default();
    assert_eq!(
        CharacterControllerRole::from_controller(false, &settings),
        CharacterControllerRole::Passive
    );

    settings.enemy = Some(psxed_project::EnemyBehaviorSettings::defaults());
    assert_eq!(
        CharacterControllerRole::from_controller(false, &settings),
        CharacterControllerRole::Enemy
    );
    assert_eq!(
        CharacterControllerRole::from_controller(true, &settings),
        CharacterControllerRole::Player
    );
}

#[test]
fn character_controller_role_preserves_enemy_tuning_while_player_controlled() {
    let mut settings = CharacterControllerSettings {
        enemy: Some(psxed_project::EnemyBehaviorSettings::defaults()),
        ..Default::default()
    };
    let original_enemy = settings.enemy;
    let mut player = false;

    assert!(CharacterControllerRole::Player.apply_to(&mut player, &mut settings));
    assert!(player);
    assert_eq!(settings.enemy, original_enemy);

    assert!(CharacterControllerRole::Enemy.apply_to(&mut player, &mut settings));
    assert!(!player);
    assert_eq!(settings.enemy, original_enemy);

    assert!(CharacterControllerRole::Passive.apply_to(&mut player, &mut settings));
    assert!(!player);
    assert!(settings.enemy.is_none());
}

#[test]
fn debug_scene_focus_selects_an_entity_by_name_or_id() {
    let mut project = ProjectDocument::new("debug-scene-focus");
    let entity =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Enemy Captain", NodeKind::Entity);
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);

    assert!(workspace.focus_scene_node_for_debug("enemy captain"));
    assert_eq!(workspace.selection.selected_node, entity);

    workspace.replace_node_selection(NodeId::ROOT);
    assert!(workspace.focus_scene_node_for_debug(&entity.raw().to_string()));
    assert_eq!(workspace.selection.selected_node, entity);
}

#[test]
fn inline_character_controller_edit_undoes_with_entity_selected() {
    let mut project = ProjectDocument::new("inline-controller-undo");
    let entity = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Enemy", NodeKind::Entity);
    let settings = CharacterControllerSettings {
        enemy: Some(psxed_project::EnemyBehaviorSettings::defaults()),
        ..Default::default()
    };
    let controller = project.active_scene_mut().add_node(
        entity,
        "Character Controller",
        NodeKind::CharacterController {
            character: None,
            settings: Some(settings),
            player: false,
        },
    );
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.replace_node_selection(entity);
    let before = workspace.project.clone();
    let history_epoch = workspace.history.epoch();

    let NodeKind::CharacterController { settings: Some(settings), .. } = &mut workspace
        .project
        .active_scene_mut()
        .node_mut(controller)
        .unwrap()
        .kind
    else {
        panic!("expected Character Controller");
    };
    settings.enemy.as_mut().unwrap().aggro_radius = 4096;
    workspace.finish_inspector_undo(before, history_epoch, InspectorUndoInput::default());
    workspace.do_undo();

    assert_eq!(workspace.selection.selected_node, entity);
    let NodeKind::CharacterController { settings: Some(settings), .. } = &workspace
        .project
        .active_scene()
        .node(controller)
        .unwrap()
        .kind
    else {
        panic!("expected Character Controller");
    };
    assert_eq!(settings.enemy.as_ref().unwrap().aggro_radius, 2048);
}

#[test]
fn character_action_preview_uses_animator_binding_without_mutating_animator() {
    let mut project = ProjectDocument::new("character-action-preview");
    let entity = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Enemy", NodeKind::Entity);
    let controller = project.active_scene_mut().add_node(
        entity,
        "Character Controller",
        NodeKind::CharacterController {
            character: None,
            settings: Some(CharacterControllerSettings::default()),
            player: false,
        },
    );
    let animator = project.active_scene_mut().add_node(
        entity,
        "Animator",
        NodeKind::Animator {
            clip: Some(1),
            action_clips: vec![psxed_project::CharacterActionClip {
                action: psxed_project::CharacterAnimationAction::Roll,
                clip: 7,
                options: None,
            }],
            autoplay: false,
            pose_frame: 18,
        },
    );
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.replace_node_selection(controller);

    assert!(!workspace
        .preview_character_action(controller, psxed_project::CharacterAnimationAction::Roll));

    let NodeKind::Animator {
        clip,
        autoplay,
        pose_frame,
        ..
    } = &workspace
        .project
        .active_scene()
        .node(animator)
        .unwrap()
        .kind
    else {
        panic!("expected Animator");
    };
    assert_eq!(*clip, Some(1));
    assert!(!*autoplay);
    assert_eq!(*pose_frame, 18);
    assert_eq!(workspace.character_motion_preview().unwrap().clip, 7);
    assert!(workspace.status.contains("Previewing Roll"));
}

#[test]
fn editing_animator_clears_transient_action_preview_that_would_mask_editor_clip() {
    let mut project = ProjectDocument::new("animator-live-preview-refresh");
    let entity = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Player", NodeKind::Entity);
    let controller = project.active_scene_mut().add_node(
        entity,
        "Character Controller",
        NodeKind::CharacterController {
            character: None,
            settings: Some(CharacterControllerSettings::default()),
            player: true,
        },
    );
    let animator = project.active_scene_mut().add_node(
        entity,
        "Animator",
        NodeKind::Animator {
            clip: Some(1),
            action_clips: vec![psxed_project::CharacterActionClip {
                action: psxed_project::CharacterAnimationAction::Roll,
                clip: 7,
                options: None,
            }],
            autoplay: true,
            pose_frame: 0,
        },
    );
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.replace_node_selection(controller);
    workspace.preview_character_action(controller, psxed_project::CharacterAnimationAction::Roll);
    assert_eq!(workspace.character_motion_preview().unwrap().clip, 7);

    let before = workspace
        .project
        .active_scene()
        .node(animator)
        .unwrap()
        .kind
        .clone();
    let NodeKind::Animator { clip, .. } = &mut workspace
        .project
        .active_scene_mut()
        .node_mut(animator)
        .unwrap()
        .kind
    else {
        panic!("expected Animator");
    };
    *clip = Some(2);
    workspace.reconcile_character_preview_after_node_kind_edit(animator, &before);

    assert!(workspace.character_motion_preview().is_none());
    let NodeKind::Animator { clip, .. } = &workspace
        .project
        .active_scene()
        .node(animator)
        .unwrap()
        .kind
    else {
        panic!("expected Animator");
    };
    assert_eq!(*clip, Some(2));
}

#[test]
fn character_motion_preview_moves_without_mutating_authored_transform_and_tracks_camera() {
    let mut project = ProjectDocument::new("character-motion-preview");
    let entity = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Player", NodeKind::Entity);
    let settings = CharacterControllerSettings {
        walk_speed: 10,
        turn_speed_degrees_per_second: 180,
        ..Default::default()
    };
    project.active_scene_mut().add_node(
        entity,
        "Character Controller",
        NodeKind::CharacterController {
            character: None,
            settings: Some(settings),
            player: true,
        },
    );
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.replace_node_selection(entity);
    let authored = workspace
        .project
        .active_scene()
        .node(entity)
        .unwrap()
        .transform;
    workspace.character_motion_preview = Some(CharacterMotionPreviewState {
        entity,
        action: psxed_project::CharacterAnimationAction::Walk,
        clip: 0,
        started_at: Instant::now() - std::time::Duration::from_millis(500),
    });

    let preview = workspace.character_motion_preview().expect("walk preview");
    assert!(preview.origin[2] >= 290 && preview.origin[2] <= 320);
    assert_eq!(
        workspace
            .project
            .active_scene()
            .node(entity)
            .unwrap()
            .transform,
        authored
    );
    let camera = workspace.viewport_3d_camera();
    assert_eq!(camera.mode, ViewportCameraMode::Orbit);
    assert_eq!(camera.target[0], preview.origin[0]);
    assert_eq!(camera.target[2], preview.origin[2]);

    workspace.character_motion_preview = Some(CharacterMotionPreviewState {
        entity,
        action: psxed_project::CharacterAnimationAction::Turn,
        clip: 0,
        started_at: Instant::now() - std::time::Duration::from_millis(500),
    });
    let turn = workspace.character_motion_preview().expect("turn preview");
    assert!(turn.yaw_q12 >= 1010 && turn.yaw_q12 <= 1040);
    assert_eq!(
        workspace
            .project
            .active_scene()
            .node(entity)
            .unwrap()
            .transform,
        authored
    );
}

#[test]
fn scene_graph_add_menu_is_structure_only() {
    let addable = scene_graph_addable_kinds();
    assert_eq!(addable.len(), 3);
    assert!(addable
        .iter()
        .any(|(label, kind)| *label == "Section" && matches!(kind, NodeKind::Section { .. })));
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
fn entity_add_child_menu_includes_camera_component() {
    let addable = scene_graph_addable_kinds_for_host_label(NodeKind::Entity.label());
    assert!(addable
        .iter()
        .any(|(label, kind)| *label == "Camera" && matches!(kind, NodeKind::Camera { .. })));
    assert!(addable
        .iter()
        .any(|(label, kind)| *label == "Entity" && matches!(kind, NodeKind::Entity)));

    let folder_addable = scene_graph_addable_kinds_for_host_label(NodeKind::Node.label());
    assert!(!folder_addable
        .iter()
        .any(|(_, kind)| matches!(kind, NodeKind::Camera { .. })));
}

#[test]
fn add_component_to_host_creates_child_and_selects_it() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::legacy_grid_starter_dir()).unwrap();
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
                settings: Some(CharacterControllerSettings::default()),
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
fn add_camera_component_to_entity_selects_new_child() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::legacy_grid_starter_dir()).unwrap();
    let room = workspace.active_room_id().expect("starter has room");
    let entity = workspace
        .project
        .active_scene_mut()
        .add_node(room, "Player", NodeKind::Entity);

    let camera = workspace
        .add_component_to_host(
            entity,
            "Camera",
            NodeKind::Camera {
                settings: WorldCameraSettings::default(),
            },
        )
        .expect("component is added");

    let scene = workspace.project.active_scene();
    assert_eq!(workspace.selection.selected_node, camera);
    assert!(scene.node(entity).unwrap().children.contains(&camera));
    assert!(matches!(
        scene.node(camera).unwrap().kind,
        NodeKind::Camera { .. }
    ));
    assert!(workspace.is_dirty());
}

#[test]
fn camera_preview_request_targets_floor_anchored_player_origin() {
    let mut project = ProjectDocument::new("camera-preview");
    let scene = project.active_scene_mut();
    let mut grid = WorldGrid::empty(3, 3, 1024);
    grid.origin = [-1, -2];
    grid.set_floor(1, 2, 256, None);
    let room = scene.add_node(scene.root, "Room", NodeKind::Section { grid });
    let player = scene.add_node(room, "Player", NodeKind::Entity);
    scene
        .node_mut(player)
        .expect("player exists")
        .transform
        .translation = [0.0, 7.0, 0.0];
    let camera = scene.add_node(
        player,
        "Camera",
        NodeKind::Camera {
            settings: WorldCameraSettings {
                distance: 2048,
                height: 768,
                target_height: 512,
                lock_rise_percent: 15,
                min_floor_clearance: 64,
                orbit_speed_level: 5,
                position_lag_shift: 2,
                focus_lag_shift: 2,
                distance_lag_shift: 3,
            },
        },
    );

    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("camera-preview-request"), project);
    workspace.selection.selected_node = camera;
    let request = workspace
        .selected_camera_preview_request()
        .expect("camera preview request");
    let scene = workspace.project.active_scene();
    let NodeKind::Section { grid } = &scene.node(room).expect("room exists").kind else {
        panic!("expected room");
    };
    let expected_player = psxed_project::spatial::floor_anchored_node_preview_origin(
        grid,
        &scene.node(player).unwrap().transform,
    );

    assert_eq!(request.active_room, Some(room));
    assert_eq!(request.active_floor, 0);
    assert_eq!(
        request.camera.target,
        [
            expected_player[0],
            expected_player[1] + 512,
            expected_player[2]
        ]
    );
    assert_ne!(request.camera.target, [0, 7 + 512, 0]);
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
        NodeKind::Section {
            grid: WorldGrid::empty(9, 9, 1024),
        },
        "Room",
    );

    let room = workspace.selection.selected_node;
    let scene = workspace.project.active_scene();
    let node = scene.node(room).expect("new room exists");
    let NodeKind::Section { grid } = &node.kind else {
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
        NodeKind::Section {
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
        NodeKind::Section {
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
fn dropping_enemy_profile_first_preserves_authored_enemy_defaults() {
    let mut project = ProjectDocument::new("drop-enemy-profile");
    let enemy_behavior = psxed_project::EnemyBehaviorSettings {
        aggro_radius: 2335,
        patrol_offset: [0, 0, -6000],
        reaction_ticks: 22,
        ..psxed_project::EnemyBehaviorSettings::defaults()
    };
    let character = project.add_resource(
        "Rust Mantis Enemy",
        ResourceData::Character(psxed_project::CharacterResource {
            spawn_role: psxed_project::CharacterSpawnRole::Enemy,
            enemy_behavior: Some(enemy_behavior),
            walk_speed: 28,
            ..psxed_project::CharacterResource::defaults()
        }),
    );
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Section {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);

    workspace.drop_resource_at_room_hit(character, room, [0.0, 0.0, 0.0], None);

    let entity = workspace.selection.selected_node;
    let scene = workspace.project.active_scene();
    let (player, settings) = scene
        .node(entity)
        .expect("enemy entity exists")
        .children
        .iter()
        .filter_map(|id| scene.node(*id))
        .find_map(|child| match child.kind {
            NodeKind::CharacterController {
                player, settings: Some(settings), ..
            } => Some((player, settings)),
            _ => None,
        })
        .expect("enemy has a controller");
    assert!(!player, "an enemy profile never claims the player slot");
    assert_eq!(settings.walk_speed, 28);
    assert_eq!(settings.enemy, Some(enemy_behavior));
    assert!(scene
        .node(entity)
        .unwrap()
        .children
        .iter()
        .filter_map(|id| scene.node(*id))
        .all(|child| !matches!(child.kind, NodeKind::Camera { .. })));
}

#[test]
fn dropping_player_profile_applies_camera_preset_and_replaces_player_source() {
    let mut project = ProjectDocument::new("drop-player-profile");
    let material = project.add_resource(
        "Aletha Crystal",
        ResourceData::Material(psxed_project::MaterialResource::opaque(None)),
    );
    let model = project.add_resource(
        "Aletha Model",
        ResourceData::Model(psxed_project::ModelResource {
            model_path: "assets/aletha.psxmdl".to_string(),
            source_path: None,
            texture_path: None,
            skeleton: None,
            world_height: 1024,
            collision_radius: 192,
            scale_q8: [psxed_project::MODEL_SCALE_ONE_Q8; 3],
            default_visual_yaw_q12: 0,
            attachments: Vec::new(),
        }),
    );
    let character = project.add_resource(
        "Aletha",
        ResourceData::Character(psxed_project::CharacterResource {
            model: Some(model),
            material: Some(material),
            spawn_role: psxed_project::CharacterSpawnRole::Player,
            radius: 188,
            walk_speed: 44,
            run_speed: 94,
            roll_speed: 165,
            camera_distance: 3300,
            camera_height: 1500,
            camera_target_height: 900,
            camera_lock_rise_percent: 25,
            camera_min_floor_clearance: 110,
            camera_orbit_speed_level: 3,
            camera_position_lag_shift: 6,
            ..psxed_project::CharacterResource::defaults()
        }),
    );
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Section {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    let old_spawn = project.active_scene_mut().add_node(
        room,
        "Old Player",
        NodeKind::SpawnPoint {
            player: true,
            character: None,
        },
    );
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);

    workspace.drop_resource_at_room_hit(character, room, [0.0, 0.0, 0.0], None);

    let entity = workspace.selection.selected_node;
    let scene = workspace.project.active_scene();
    assert!(matches!(
        scene.node(old_spawn).unwrap().kind,
        NodeKind::SpawnPoint { player: false, .. }
    ));
    let children: Vec<_> = scene
        .node(entity)
        .expect("Aletha entity exists")
        .children
        .iter()
        .filter_map(|id| scene.node(*id))
        .collect();
    let settings = children
        .iter()
        .find_map(|child| match child.kind {
            NodeKind::CharacterController {
                player: true,
                settings: Some(settings),
                ..
            } => Some(settings),
            _ => None,
        })
        .expect("Aletha is the player");
    assert_eq!(
        (settings.radius, settings.walk_speed, settings.run_speed),
        (188, 44, 94)
    );
    assert_eq!(settings.roll_speed, 165);
    let camera = children
        .iter()
        .find_map(|child| match child.kind {
            NodeKind::Camera { settings } => Some(settings),
            _ => None,
        })
        .expect("Aletha gets her camera preset");
    assert_eq!(
        (camera.distance, camera.height, camera.target_height),
        (3300, 1500, 900)
    );
    assert_eq!(camera.lock_rise_percent, 25);
    assert_eq!(camera.min_floor_clearance, 110);
    assert_eq!(camera.position_lag_shift, 6);
    assert!(children.iter().any(|child| matches!(
        child.kind,
        NodeKind::ModelRenderer {
            material: Some(id),
            ..
        } if id == material
    )));
}

#[test]
fn player_source_demote_handles_spawn_points_and_character_controllers() {
    let mut project = ProjectDocument::new("player-source-demote");
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Section {
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
            settings: Some(CharacterControllerSettings::default()),
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
        NodeKind::Section {
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
            settings: Some(CharacterControllerSettings::default()),
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
    let workspace = EditorWorkspace::open_directory(psxed_project::legacy_grid_starter_dir()).unwrap();
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
        NodeKind::Section {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    let prop = project.active_scene_mut().add_node(
        room,
        "Crate",
        NodeKind::BoxProp {
            materials: [None; psxed_project::BOX_PROP_FACE_COUNT],
            uvs: [GridUvTransform::IDENTITY; psxed_project::BOX_PROP_FACE_COUNT],
            vertices: psxed_project::box_prop_vertices_for_size(1024),
            collision_enabled: true,
            break_flags: 0,
            erosion: psxed_project::BoxPropErosion::default(),
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
    let project = ProjectDocument::legacy_grid_starter();
    let prefab_library = load_prefab_library().expect("prefab library loads");
    let rows = project_filesystem_rows(&project, &prefab_library);
    let material_name = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Material(_)))
        .map(resource_file_name)
        .expect("starter project has a material resource");

    assert!(rows.iter().any(|row| row.name == "res://"));
    assert!(rows.iter().any(|row| row.name == "main.map"));
    assert!(rows.iter().any(|row| row.name == "characters"));
    assert!(rows
        .iter()
        .any(|row| row.name == "crimson_cross_knight_player.profile" && row.resource.is_some()));
    assert!(rows
        .iter()
        .any(|row| row.name == material_name && row.resource.is_some()));
    assert!(rows.iter().any(|row| {
        row.key.starts_with("res://prefabs/") && row.resource.is_none() && row.prefab.is_some()
    }));
}

#[test]
fn collapsed_project_filesystem_folder_hides_children() {
    let project = ProjectDocument::legacy_grid_starter();
    let prefab_library = load_prefab_library().expect("prefab library loads");
    let rows = project_filesystem_rows(&project, &prefab_library);
    let material_name = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Material(_)))
        .map(resource_file_name)
        .expect("starter project has a material resource");
    let mut collapsed = HashSet::new();
    collapsed.insert("res://materials".to_string());

    let display_rows = project_filesystem_display_rows(&rows, "", &collapsed);

    assert!(display_rows.iter().any(|row| row.name == "materials"));
    assert!(!display_rows.iter().any(|row| row.name == material_name));
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
    let project = ProjectDocument::legacy_grid_starter();
    // Legacy Texture resources fold into materials at load.
    assert!(!project
        .resources
        .iter()
        .any(|resource| matches!(resource.data, ResourceData::Texture { .. })));
    let material = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Material(_)))
        .unwrap();
    let material_search = resource_search_token(material);

    assert!(resource_matches_filter(
        material,
        ResourceFilter::Material,
        &material_search
    ));
    assert!(resource_matches_filter(
        material,
        ResourceFilter::ImagePropSource,
        &material_search
    ));
    assert!(!resource_matches_filter(
        material,
        ResourceFilter::Model,
        &material_search
    ));
}
