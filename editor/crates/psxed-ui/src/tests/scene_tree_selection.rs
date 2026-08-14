use super::*;

#[test]
fn dragging_selected_node_moves_it_in_xz_space() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    let spawn = starter_player_entity(workspace.project.active_scene()).id;
    let sector_size = node_translation_sector_size(&workspace.project, spawn);
    let start = workspace
        .project
        .active_scene()
        .node(spawn)
        .unwrap()
        .transform
        .translation;

    workspace.selection.selected_node = spawn;
    workspace.drag_selected_node(Vec2::new(96.0, -48.0));

    let node = workspace.project.active_scene().node(spawn).unwrap();
    assert!(
        (node.transform.translation[0]
            - snap_node_transform_component_to_world_step(start[0] + 1.0, sector_size))
        .abs()
            < 0.001
    );
    assert!(
        (node.transform.translation[2]
            - snap_node_transform_component_to_world_step(start[2] + 0.5, sector_size))
        .abs()
            < 0.001
    );
    assert!(workspace.is_dirty());
}

#[test]
fn light_transform_normalises_hidden_rotation_scale_and_y_quantum() {
    let mut transform = psxed_project::Transform3 {
        translation: [10.0, 0.05, 20.0],
        rotation_degrees: [10.0, 90.0, 5.0],
        scale: [2.0, 3.0, 4.0],
    };

    assert!(normalise_light_transform(
        &mut transform,
        DEFAULT_WORLD_SECTOR_SIZE
    ));

    assert_eq!(
        transform.translation,
        [
            10.0,
            HEIGHT_QUANTUM as f32 / DEFAULT_WORLD_SECTOR_SIZE as f32,
            20.0
        ]
    );
    assert_eq!(transform.rotation_degrees, [0.0, 0.0, 0.0]);
    assert_eq!(transform.scale, [1.0, 1.0, 1.0]);
    assert!(!normalise_light_transform(
        &mut transform,
        DEFAULT_WORLD_SECTOR_SIZE
    ));
}

#[test]
fn rotate_selected_yaw_ignores_light_nodes() {
    let mut project = ProjectDocument::new("light-rotate");
    let light = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Point Light",
        NodeKind::PointLight {
            color: [255, 240, 200],
            intensity: 1.0,
            radius: 4.0,
        },
    );
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("light-rotate"), project);
    workspace.selection.selected_node = light;

    workspace.rotate_selected_yaw_90();

    let node = workspace.project.active_scene().node(light).unwrap();
    assert_eq!(node.transform.rotation_degrees, [0.0, 0.0, 0.0]);
    assert!(!workspace.is_dirty());
}

#[test]
fn rotate_selected_yaw_rotates_entity_hosts() {
    let mut project = ProjectDocument::new("entity-rotate");
    let entity = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Prop", NodeKind::Entity);
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("entity-rotate"), project);
    workspace.selection.selected_node = entity;

    workspace.rotate_selected_yaw_90();

    let node = workspace.project.active_scene().node(entity).unwrap();
    assert_eq!(node.transform.rotation_degrees, [0.0, 90.0, 0.0]);
    assert!(workspace.is_dirty());
}

#[test]
fn node_transform_inspector_hides_unused_transform_fields() {
    assert_eq!(
        node_transform_inspector(&NodeKind::Node),
        NodeTransformInspector::Hidden
    );
    assert_eq!(
        node_transform_inspector(&NodeKind::Section {
            grid: WorldGrid::empty(1, 1, DEFAULT_WORLD_SECTOR_SIZE),
        }),
        NodeTransformInspector::RoomGrid
    );
    assert_eq!(
        node_transform_inspector(&NodeKind::PointLight {
            color: [255, 240, 200],
            intensity: 1.0,
            radius: 4.0,
        }),
        NodeTransformInspector::PositionOnly
    );
    assert_eq!(
        node_transform_inspector(&NodeKind::SpawnPoint {
            player: false,
            character: None,
        }),
        NodeTransformInspector::PositionYaw
    );
    assert_eq!(
        node_transform_inspector(&NodeKind::ImageProp {
            material: None,
            width: 256,
            height: 256,
            cylindrical_billboard: false,
            collision_enabled: false,
            collision_size: [256; 3],
        }),
        NodeTransformInspector::PositionFullRotation
    );
    assert_eq!(
        node_transform_inspector(&NodeKind::Node3D),
        NodeTransformInspector::FullTransform
    );
}

#[test]
fn tree_row_drop_zone_uses_top_band_without_extra_layout() {
    let rect = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(200.0, 24.0));

    assert_eq!(
        tree_row_drop_zone(rect, Some(Pos2::new(40.0, 22.0)), true),
        TreeRowDropZone::Before
    );
    assert_eq!(
        tree_row_drop_zone(rect, Some(Pos2::new(40.0, 32.0)), true),
        TreeRowDropZone::Inside
    );
    assert_eq!(
        tree_row_drop_zone(rect, Some(Pos2::new(40.0, 22.0)), false),
        TreeRowDropZone::Inside
    );
}

#[test]
fn tree_drag_autoscroll_delta_tracks_edge_bands() {
    let viewport = Rect::from_min_size(Pos2::new(0.0, 100.0), Vec2::new(240.0, 200.0));

    assert!(tree_drag_autoscroll_delta(viewport, Pos2::new(120.0, 106.0)) > 0.0);
    assert_eq!(
        tree_drag_autoscroll_delta(viewport, Pos2::new(120.0, 200.0)),
        0.0
    );
    assert!(tree_drag_autoscroll_delta(viewport, Pos2::new(120.0, 294.0)) < 0.0);
    assert_eq!(
        tree_drag_autoscroll_delta(viewport, Pos2::new(-4.0, 106.0)),
        0.0
    );
}

#[test]
fn scene_tree_select_clears_inspector_shadow_selection() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    let scene = workspace.project.active_scene();
    let room = scene
        .nodes()
        .iter()
        .find(|node| matches!(node.kind, NodeKind::Section { .. }))
        .expect("starter scene has a Room")
        .id;
    let spawn = starter_player_entity(scene).id;
    let resource = workspace
        .project
        .resources
        .first()
        .expect("starter project has resources")
        .id;

    workspace.selection.selected_node = NodeId::ROOT;
    workspace.selection.selected_resource = Some(resource);
    workspace.selection.selected_primitive = Some(Selection::Face(FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Floor,
    }));

    workspace.apply_tree_action(
        TreeAction::Select {
            id: spawn,
            modifiers: egui::Modifiers::NONE,
        },
        &[NodeId::ROOT, room, spawn],
    );

    assert_eq!(workspace.selection.selected_node, spawn);
    assert_eq!(workspace.selection.selected_primitive, None);
    assert_eq!(workspace.selection.selected_resource, None);
}

#[test]
fn scene_tree_ctrl_toggles_node_multi_selection() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    let order = workspace.scene_node_order();
    let ids: Vec<NodeId> = order
        .iter()
        .copied()
        .filter(|id| *id != NodeId::ROOT)
        .take(2)
        .collect();
    assert!(ids.len() >= 2, "starter scene has at least two nodes");

    let mut ctrl = egui::Modifiers::NONE;
    ctrl.ctrl = true;
    workspace.apply_tree_action(
        TreeAction::Select {
            id: ids[0],
            modifiers: egui::Modifiers::NONE,
        },
        &order,
    );
    workspace.apply_tree_action(
        TreeAction::Select {
            id: ids[1],
            modifiers: ctrl,
        },
        &order,
    );

    assert!(workspace.selection.selected_nodes.contains(&ids[0]));
    assert!(workspace.selection.selected_nodes.contains(&ids[1]));
    assert_eq!(workspace.selection.selected_nodes.len(), 2);

    workspace.apply_tree_action(
        TreeAction::Select {
            id: ids[0],
            modifiers: ctrl,
        },
        &order,
    );
    assert!(!workspace.selection.selected_nodes.contains(&ids[0]));
    assert!(workspace.selection.selected_nodes.contains(&ids[1]));
    assert_eq!(workspace.selection.selected_node, ids[1]);
}

#[test]
fn scene_tree_shift_selects_visible_node_range() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    let order = workspace.scene_node_order();
    let ids: Vec<NodeId> = order
        .iter()
        .copied()
        .filter(|id| *id != NodeId::ROOT)
        .take(3)
        .collect();
    assert!(ids.len() >= 3, "starter scene has at least three nodes");

    let mut shift = egui::Modifiers::NONE;
    shift.shift = true;
    workspace.apply_tree_action(
        TreeAction::Select {
            id: ids[0],
            modifiers: egui::Modifiers::NONE,
        },
        &order,
    );
    workspace.apply_tree_action(
        TreeAction::Select {
            id: ids[2],
            modifiers: shift,
        },
        &order,
    );

    for id in &ids {
        assert!(workspace.selection.selected_nodes.contains(id));
    }
    assert_eq!(workspace.selection.selected_nodes.len(), 3);
}

#[test]
fn scene_tree_dragging_selected_group_moves_all_into_folder() {
    let mut project = ProjectDocument::new("multi-drag-folder");
    let scene = project.active_scene_mut();
    let folder = scene.add_node(NodeId::ROOT, "Folder", NodeKind::Node);
    let a = scene.add_node(NodeId::ROOT, "A", NodeKind::Entity);
    let b = scene.add_node(NodeId::ROOT, "B", NodeKind::Entity);
    let c = scene.add_node(NodeId::ROOT, "C", NodeKind::Entity);
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("multi-drag-folder"), project);
    workspace.selection.selected_node = a;
    workspace.selection.selected_nodes = [a, b].into_iter().collect();

    let order = workspace.scene_node_order();
    workspace.apply_tree_action(
        TreeAction::Reparent {
            source: a,
            target_parent: folder,
            position: 0,
        },
        &order,
    );

    let scene = workspace.project.active_scene();
    assert_eq!(scene.node(folder).unwrap().children, vec![a, b]);
    assert_eq!(scene.node(NodeId::ROOT).unwrap().children, vec![folder, c]);
    assert_eq!(scene.node(a).unwrap().parent, Some(folder));
    assert_eq!(scene.node(b).unwrap().parent, Some(folder));
    assert_eq!(workspace.selection.selected_node, a);
    assert_eq!(workspace.selection.selected_nodes.len(), 2);
    assert!(workspace.selection.selected_nodes.contains(&a));
    assert!(workspace.selection.selected_nodes.contains(&b));
}

#[test]
fn scene_tree_dragging_selected_siblings_reorders_as_group() {
    let mut project = ProjectDocument::new("multi-drag-reorder");
    let scene = project.active_scene_mut();
    let a = scene.add_node(NodeId::ROOT, "A", NodeKind::Entity);
    let b = scene.add_node(NodeId::ROOT, "B", NodeKind::Entity);
    let c = scene.add_node(NodeId::ROOT, "C", NodeKind::Entity);
    let d = scene.add_node(NodeId::ROOT, "D", NodeKind::Entity);
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("multi-drag-reorder"), project);
    workspace.selection.selected_node = b;
    workspace.selection.selected_nodes = [b, c].into_iter().collect();

    let order = workspace.scene_node_order();
    workspace.apply_tree_action(
        TreeAction::Reparent {
            source: b,
            target_parent: NodeId::ROOT,
            position: 4,
        },
        &order,
    );

    let scene = workspace.project.active_scene();
    assert_eq!(scene.node(NodeId::ROOT).unwrap().children, vec![a, d, b, c]);
    assert_eq!(workspace.selection.selected_node, b);
    assert_eq!(workspace.selection.selected_nodes.len(), 2);
    assert!(workspace.selection.selected_nodes.contains(&b));
    assert!(workspace.selection.selected_nodes.contains(&c));
}

#[test]
fn scene_tree_toggle_expanded_hides_descendants_from_display_rows() {
    let mut project = ProjectDocument::new("tree-collapse");
    let parent = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Parent", NodeKind::Node);
    let child = project
        .active_scene_mut()
        .add_node(parent, "Child", NodeKind::Node3D);
    let grandchild = project
        .active_scene_mut()
        .add_node(child, "Grandchild", NodeKind::Entity);
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("tree-collapse"), project);

    workspace.apply_tree_action(TreeAction::ToggleExpanded(parent), &[]);

    let rows = workspace.project.active_scene().hierarchy_rows();
    let visible = scene_tree_display_rows(&rows, "", &workspace.collapsed_scene_nodes)
        .into_iter()
        .map(|row| row.id)
        .collect::<Vec<_>>();
    assert!(visible.contains(&parent));
    assert!(!visible.contains(&child));
    assert!(!visible.contains(&grandchild));

    workspace.apply_tree_action(TreeAction::ToggleExpanded(parent), &[]);
    let rows = workspace.project.active_scene().hierarchy_rows();
    let visible = scene_tree_display_rows(&rows, "", &workspace.collapsed_scene_nodes)
        .into_iter()
        .map(|row| row.id)
        .collect::<Vec<_>>();
    assert!(visible.contains(&child));
    assert!(visible.contains(&grandchild));
}

#[test]
fn scene_tree_toggle_visibility_hides_entity_bounds() {
    let mut project = ProjectDocument::new("tree-visibility");
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Section {
            grid: WorldGrid::stone_room(1, 1, 1024, None, None),
        },
    );
    let actor = project
        .active_scene_mut()
        .add_node(room, "Actor", NodeKind::Entity);
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("tree-visibility"), project);

    assert!(workspace
        .collect_entity_bounds(Some(room))
        .iter()
        .any(|bound| bound.node == actor));

    workspace.apply_tree_action(TreeAction::ToggleVisibility(actor), &[]);

    assert!(workspace.hidden_scene_nodes.contains(&actor));
    assert!(!workspace
        .collect_entity_bounds(Some(room))
        .iter()
        .any(|bound| bound.node == actor));

    workspace.apply_tree_action(TreeAction::ToggleVisibility(actor), &[]);
    assert!(!workspace.hidden_scene_nodes.contains(&actor));
    assert!(workspace
        .collect_entity_bounds(Some(room))
        .iter()
        .any(|bound| bound.node == actor));
}

#[test]
fn ui_tree_visibility_is_scene_local_and_hides_canvas_hits() {
    let mut project = ProjectDocument::new("ui-tree-visibility");
    project.ui_scenes = vec![UiScene::empty_canvas("Main", UiSceneId::FIRST)];
    let first_scene_id = project.ui_scenes[0].id;
    let group = project.ui_scenes[0].add_node(
        UiNodeId::ROOT,
        "Panel",
        UiNodeKind::Group {
            rect: UiRect::new(0, 0, 96, 64),
        },
    );
    let label = project.ui_scenes[0].add_node(
        group,
        "Label",
        UiNodeKind::Group {
            rect: UiRect::new(4, 4, 48, 16),
        },
    );
    let second_scene_id = project.add_ui_scene("Settings");
    let second_group = project.ui_scene_mut(second_scene_id).unwrap().add_node(
        UiNodeId::ROOT,
        "Panel",
        UiNodeKind::Group {
            rect: UiRect::new(0, 0, 96, 64),
        },
    );
    assert_eq!(group.raw(), second_group.raw());

    let mut workspace = EditorWorkspace::with_project(test_temp_dir("ui-tree-visibility"), project);
    workspace.apply_ui_tree_action(UiTreeAction::ToggleVisibility(group));

    let first_scene = workspace.current_ui_scene().unwrap();
    assert!(workspace.hidden_ui_nodes.contains(&(first_scene_id, group)));
    assert!(ui_node_hidden(
        first_scene,
        &workspace.hidden_ui_nodes,
        group
    ));
    assert!(ui_node_hidden(
        first_scene,
        &workspace.hidden_ui_nodes,
        label
    ));
    let canvas = Rect::from_min_size(Pos2::ZERO, Vec2::new(320.0, 240.0));
    assert_eq!(
        ui_scene_hit_test(
            first_scene,
            &workspace.hidden_ui_nodes,
            canvas,
            [320, 240],
            Pos2::new(8.0, 8.0)
        ),
        None
    );

    let second_scene = workspace.project.ui_scene(second_scene_id).unwrap();
    assert!(!ui_node_hidden(
        second_scene,
        &workspace.hidden_ui_nodes,
        second_group
    ));
    assert_eq!(
        ui_scene_hit_test(
            second_scene,
            &workspace.hidden_ui_nodes,
            canvas,
            [320, 240],
            Pos2::new(8.0, 8.0)
        ),
        Some(second_group)
    );
}

#[test]
fn ui_node_clipboard_pastes_subtree_into_another_ui_scene() {
    let mut project = ProjectDocument::new("ui-node-clipboard");
    project.ui_scenes = vec![UiScene::empty_canvas("Main", UiSceneId::FIRST)];
    let source_root = project.ui_scenes[0].root;
    let panel = project.ui_scenes[0].add_node(
        source_root,
        "Panel",
        UiNodeKind::Group {
            rect: UiRect::new(10, 12, 80, 40),
        },
    );
    project.ui_scenes[0].add_node(
        panel,
        "Child",
        UiNodeKind::Group {
            rect: UiRect::new(3, 4, 16, 8),
        },
    );
    let target_scene_id = project.add_ui_scene("Settings");
    let target_root = project.ui_scene(target_scene_id).unwrap().root;
    let target_parent = project.ui_scene_mut(target_scene_id).unwrap().add_node(
        target_root,
        "Destination",
        UiNodeKind::Group {
            rect: UiRect::new(20, 30, 100, 50),
        },
    );

    let mut workspace = EditorWorkspace::with_project(test_temp_dir("ui-node-clipboard"), project);
    assert!(workspace.copy_ui_node(panel));
    workspace.active_ui_scene_index = 1;
    workspace.selection.selected_ui_node = target_parent;
    assert!(workspace.paste_ui_node());

    let pasted = workspace.selection.selected_ui_node;
    let scene = workspace.current_ui_scene().unwrap();
    let pasted_node = scene.node(pasted).unwrap();
    assert_eq!(pasted_node.name, "Panel");
    assert_eq!(pasted_node.parent, Some(target_parent));
    assert_eq!(pasted_node.children.len(), 1);
    let pasted_child = pasted_node.children[0];
    assert_eq!(scene.node(pasted_child).unwrap().name, "Child");
    assert_eq!(scene.node(pasted_child).unwrap().parent, Some(pasted));
    assert_eq!(
        scene.absolute_rect(pasted_child),
        Some(UiRect::new(33, 46, 16, 8))
    );
}

#[test]
fn resource_browser_supports_ctrl_and_shift_multi_selection() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    let order: Vec<ResourceId> = workspace
        .project
        .resources
        .iter()
        .map(|resource| resource.id)
        .take(3)
        .collect();
    assert!(
        order.len() >= 3,
        "starter project has at least three resources"
    );

    let mut ctrl = egui::Modifiers::NONE;
    ctrl.ctrl = true;
    workspace.apply_resource_selection_modifiers(order[0], egui::Modifiers::NONE, &order);
    workspace.apply_resource_selection_modifiers(order[1], ctrl, &order);

    assert!(workspace.selection.selected_resources.contains(&order[0]));
    assert!(workspace.selection.selected_resources.contains(&order[1]));

    let mut shift = egui::Modifiers::NONE;
    shift.shift = true;
    workspace.apply_resource_selection_modifiers(order[2], shift, &order);

    assert!(workspace.selection.selected_resources.contains(&order[1]));
    assert!(workspace.selection.selected_resources.contains(&order[2]));
    assert!(!workspace.selection.selected_resources.contains(&order[0]));
    assert_eq!(workspace.selection.selected_resources.len(), 2);
}

#[test]
fn select_all_current_scope_selects_all_resources_from_resource_context() {
    let mut project = ProjectDocument::new("select-all-resources");
    let first = project.add_resource("A", ResourceData::Material(MaterialResource::opaque(None)));
    project.add_resource("B", ResourceData::Material(MaterialResource::opaque(None)));
    project.add_resource("C", ResourceData::Material(MaterialResource::opaque(None)));
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.replace_resource_selection(first);

    workspace.select_all_current_scope();

    assert_eq!(workspace.selection.selected_resources.len(), 3);
    assert_eq!(workspace.selection.selected_resource, Some(first));
    assert_eq!(workspace.selection.selected_node, NodeId::ROOT);
}

#[test]
fn select_all_current_scope_selects_scene_nodes_outside_select_tool() {
    let mut project = ProjectDocument::new("select-all-nodes");
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Section {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    let entity = project
        .active_scene_mut()
        .add_node(room, "Entity", NodeKind::Entity);
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.active_tool = ViewTool::PaintFloor;

    workspace.select_all_current_scope();

    assert!(workspace.selection.selected_nodes.contains(&room));
    assert!(workspace.selection.selected_nodes.contains(&entity));
    assert!(!workspace.selection.selected_nodes.contains(&NodeId::ROOT));
    assert_eq!(workspace.selection.selected_nodes.len(), 2);
    assert!(workspace.selection.selected_primitives.is_empty());
}

#[test]
fn select_all_current_scope_selects_all_faces_in_active_room() {
    let mut project = ProjectDocument::new("select-all-faces");
    let mut grid = WorldGrid::empty(2, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    grid.set_floor(1, 0, 0, None);
    grid.ensure_sector(0, 0).unwrap().ceiling = Some(GridHorizontalFace::flat(1024, None));
    grid.add_wall(0, 0, GridDirection::North, 0, 1024, None);
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.active_tool = ViewTool::Select;
    workspace.selection_mode = SelectionMode::Face;
    workspace.replace_node_selection(room);

    workspace.select_all_current_scope();

    let floor0 = Selection::Face(FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Floor,
    });
    let floor1 = Selection::Face(FaceRef {
        room,
        sx: 1,
        sz: 0,
        kind: FaceKind::Floor,
    });
    let ceiling = Selection::Face(FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Ceiling,
    });
    let wall = Selection::Face(FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Wall {
            dir: GridDirection::North,
            stack: 0,
        },
    });
    for selection in [floor0, floor1, ceiling, wall] {
        assert!(workspace.selection.selected_primitives.contains(&selection));
    }
    assert_eq!(workspace.selection.selected_primitives.len(), 4);
    assert_eq!(workspace.selection.selected_node, NodeId::ROOT);
}

#[test]
fn select_all_current_scope_respects_edge_and_vertex_modes() {
    let mut project = ProjectDocument::new("select-all-modes");
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.active_tool = ViewTool::Select;
    workspace.replace_node_selection(room);

    workspace.selection_mode = SelectionMode::Edge;
    workspace.select_all_current_scope();
    assert_eq!(workspace.selection.selected_primitives.len(), 4);
    assert!(workspace
        .selection
        .selected_primitives
        .iter()
        .all(|selection| matches!(selection, Selection::Edge(_))));

    workspace.replace_node_selection(room);
    workspace.selection_mode = SelectionMode::Vertex;
    workspace.select_all_current_scope();
    assert_eq!(workspace.selection.selected_primitives.len(), 4);
    assert!(workspace
        .selection
        .selected_primitives
        .iter()
        .all(|selection| matches!(selection, Selection::Vertex(_))));
}

#[test]
fn ctrl_selected_sector_delete_removes_all_selected_tiles() {
    let mut project = ProjectDocument::new("ctrl-delete");
    let mut grid = WorldGrid::empty(2, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    grid.set_floor(1, 0, 0, None);
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    let coords = [(0u16, 0u16), (1u16, 0u16)];

    let mut ctrl = egui::Modifiers::NONE;
    ctrl.ctrl = true;
    workspace.select_sector((room, coords[0].0, coords[0].1), egui::Modifiers::NONE);
    workspace.select_sector((room, coords[1].0, coords[1].1), ctrl);

    assert_eq!(workspace.selection.selected_sectors.len(), 2);

    workspace.delete_selected_sectors();

    let scene = workspace.project.active_scene();
    let node = scene.node(room).expect("room node exists");
    let NodeKind::Section { grid } = &node.kind else {
        panic!("active room is a room node");
    };
    assert!(grid.sector(coords[0].0, coords[0].1).is_none());
    assert!(grid.sector(coords[1].0, coords[1].1).is_none());
    assert!(workspace.selection.selected_sectors.is_empty());
}

#[test]
fn deleting_every_tile_removes_the_now_empty_layer_in_the_same_undo_step() {
    let mut project = ProjectDocument::new("delete-emptied-layer");
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    grid.push_floor();
    grid.floor_mut(1).unwrap().set_floor(0, 0, 0, None);
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    project
        .active_scene_mut()
        .node_mut(room)
        .unwrap()
        .transform
        .translation[1] = -2.0;
    let upper_entity = project
        .active_scene_mut()
        .add_node(room, "Upper entity", NodeKind::Entity);
    project
        .active_scene_mut()
        .node_mut(upper_entity)
        .unwrap()
        .floor = 1;

    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.select_sector((room, 0, 0), egui::Modifiers::NONE);
    workspace.delete_selected_sectors();

    let scene = workspace.project.active_scene();
    let room_node = scene.node(room).unwrap();
    let NodeKind::Section { grid } = &room_node.kind else {
        panic!("room node");
    };
    assert_eq!(grid.floor_count(), 1);
    assert!(grid.sector(0, 0).unwrap().floor.is_some());
    assert_eq!(room_node.transform.translation[1], 0.0);
    assert_eq!(scene.node(upper_entity).unwrap().floor, 0);
    assert_eq!(workspace.active_floor, 0);

    workspace.do_undo();
    let scene = workspace.project.active_scene();
    let room_node = scene.node(room).unwrap();
    let NodeKind::Section { grid } = &room_node.kind else {
        panic!("room node");
    };
    assert_eq!(grid.floor_count(), 2);
    assert!(grid.sector(0, 0).unwrap().floor.is_some());
    assert!(grid.floor(1).unwrap().sector(0, 0).unwrap().floor.is_some());
    assert_eq!(room_node.transform.translation[1], -2.0);
    assert_eq!(scene.node(upper_entity).unwrap().floor, 1);
}

#[test]
fn autotile_selected_sector_walls_updates_all_selected_tiles() {
    let mut project = ProjectDocument::new("autotile-selected-tiles");
    let mut grid = WorldGrid::empty(2, 1, 1024);
    for sx in 0..=1 {
        grid.add_wall(sx, 0, GridDirection::North, 0, 2048, None);
    }
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    let mut ctrl = egui::Modifiers::NONE;
    ctrl.ctrl = true;

    workspace.select_sector((room, 0, 0), egui::Modifiers::NONE);
    workspace.select_sector((room, 1, 0), ctrl);

    assert_eq!(workspace.autotile_selected_sector_walls(), 2);

    let grid = workspace.room_grid_view(room).unwrap();
    for sx in 0..=1 {
        let wall = &grid.sector(sx, 0).unwrap().walls.get(GridDirection::North)[0];
        assert_eq!(wall.uv.span, [0, 128]);
    }
    assert!(workspace.is_dirty());
}

#[test]
fn shift_selects_sector_rectangle_from_anchor() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    let room = workspace.active_room_id().expect("starter has room");

    let mut shift = egui::Modifiers::NONE;
    shift.shift = true;
    workspace.select_sector((room, 0, 0), egui::Modifiers::NONE);
    workspace.select_sector((room, 1, 1), shift);

    assert_eq!(workspace.selection.selected_sectors.len(), 4);
    for sx in 0..=1 {
        for sz in 0..=1 {
            assert!(workspace
                .selection
                .selected_sectors
                .contains(&(room, sx, sz)));
        }
    }
    assert_eq!(workspace.selection.selected_sector, Some((1, 1)));
}

#[test]
fn shift_selects_horizontal_faces_as_primitives() {
    let mut project = ProjectDocument::new("horizontal-primitive-selection");
    let mut grid = WorldGrid::empty(2, 1, 1024);
    for sx in 0..=1 {
        grid.set_floor(sx, 0, 0, None);
        grid.ensure_sector(sx, 0).unwrap().ceiling = Some(GridHorizontalFace::flat(1024, None));
        grid.add_wall(sx, 0, GridDirection::North, 0, 1024, None);
    }
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("horizontal-primitive-selection"), project);
    let floor_0 = Selection::Face(FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Floor,
    });
    let ceiling_1 = Selection::Face(FaceRef {
        room,
        sx: 1,
        sz: 0,
        kind: FaceKind::Ceiling,
    });
    let mut shift = egui::Modifiers::NONE;
    shift.shift = true;

    workspace.apply_primitive_selection_modifiers(floor_0, egui::Modifiers::NONE);
    workspace.apply_primitive_selection_modifiers(ceiling_1, shift);

    assert!(workspace.selection.selected_primitives.contains(&floor_0));
    assert!(workspace.selection.selected_primitives.contains(&ceiling_1));
    assert_eq!(workspace.selection.selected_primitives.len(), 2);
    assert!(workspace.selection.selected_sectors.is_empty());
    assert!(workspace.selected_sector_faces().is_empty());
    assert_eq!(workspace.selection.selected_primitive, Some(ceiling_1));
}

#[test]
fn shift_selects_horizontal_face_rectangle_from_anchor() {
    let mut project = ProjectDocument::new("horizontal-face-rect-selection");
    let mut grid = WorldGrid::empty(3, 3, 1024);
    for sx in 0..3 {
        for sz in 0..3 {
            grid.set_floor(sx, sz, 0, None);
            grid.ensure_sector(sx, sz).unwrap().ceiling =
                Some(GridHorizontalFace::flat(1024, None));
        }
    }
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("horizontal-face-rect-selection"), project);
    let floor_at = |sx, sz| {
        Selection::Face(FaceRef {
            room,
            sx,
            sz,
            kind: FaceKind::Floor,
        })
    };
    let ceiling_at = |sx, sz| {
        Selection::Face(FaceRef {
            room,
            sx,
            sz,
            kind: FaceKind::Ceiling,
        })
    };
    let mut shift = egui::Modifiers::NONE;
    shift.shift = true;

    workspace.apply_primitive_selection_modifiers(floor_at(0, 0), egui::Modifiers::NONE);
    workspace.apply_primitive_selection_modifiers(floor_at(2, 1), shift);

    assert_eq!(workspace.selection.selected_primitives.len(), 6);
    for sx in 0..=2 {
        for sz in 0..=1 {
            assert!(workspace
                .selection
                .selected_primitives
                .contains(&floor_at(sx, sz)));
        }
    }
    assert!(!workspace
        .selection
        .selected_primitives
        .contains(&floor_at(2, 2)));
    assert!(!workspace
        .selection
        .selected_primitives
        .contains(&ceiling_at(0, 0)));
    assert_eq!(workspace.selection.selected_primitive, Some(floor_at(2, 1)));

    workspace.apply_primitive_selection_modifiers(ceiling_at(2, 2), egui::Modifiers::NONE);
    workspace.apply_primitive_selection_modifiers(ceiling_at(1, 0), shift);

    assert_eq!(workspace.selection.selected_primitives.len(), 6);
    for sx in 1..=2 {
        for sz in 0..=2 {
            assert!(workspace
                .selection
                .selected_primitives
                .contains(&ceiling_at(sx, sz)));
        }
    }
    assert!(!workspace
        .selection
        .selected_primitives
        .contains(&ceiling_at(0, 0)));
    assert!(!workspace
        .selection
        .selected_primitives
        .contains(&floor_at(2, 2)));
    assert_eq!(
        workspace.selection.selected_primitive,
        Some(ceiling_at(1, 0))
    );
    assert!(workspace.selection.selected_sectors.is_empty());
}

#[test]
fn viewport_box_select_selects_cells_inside_screen_rectangle() {
    let mut project = ProjectDocument::new("box-select");
    let mut grid = WorldGrid::empty(3, 3, 1024);
    for sx in 0..3 {
        for sz in 0..3 {
            grid.set_floor(sx, sz, 0, None);
        }
    }
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("box-select"), project);
    let transform = ViewportTransform::new(
        Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::splat(400.0)),
        Vec2::ZERO,
        100.0,
    );

    workspace.begin_viewport_box_select(Pos2::new(90.0, 190.0), Some(room), egui::Modifiers::NONE);
    assert!(workspace.update_viewport_box_select(Pos2::new(210.0, 310.0), transform));

    for sx in 0..=1 {
        for sz in 0..=1 {
            assert!(
                workspace
                    .selection
                    .selected_sectors
                    .contains(&(room, sx, sz)),
                "missing selected sector {sx},{sz}"
            );
        }
    }
    assert_eq!(workspace.selection.selected_sectors.len(), 4);
    assert!(!workspace.selection.selected_sectors.contains(&(room, 2, 0)));
    assert_eq!(workspace.selection.selected_node, room);
}

#[test]
fn additive_viewport_box_select_keeps_initial_sector_selection() {
    let mut project = ProjectDocument::new("box-select-additive");
    let mut grid = WorldGrid::empty(3, 3, 1024);
    for sx in 0..3 {
        for sz in 0..3 {
            grid.set_floor(sx, sz, 0, None);
        }
    }
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("box-select-additive"), project);
    let transform = ViewportTransform::new(
        Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::splat(400.0)),
        Vec2::ZERO,
        100.0,
    );
    let mut shift = egui::Modifiers::NONE;
    shift.shift = true;

    workspace.select_sector((room, 2, 2), egui::Modifiers::NONE);
    workspace.begin_viewport_box_select(Pos2::new(90.0, 290.0), Some(room), shift);
    assert!(workspace.update_viewport_box_select(Pos2::new(110.0, 310.0), transform));

    assert!(workspace.selection.selected_sectors.contains(&(room, 0, 0)));
    assert!(workspace.selection.selected_sectors.contains(&(room, 2, 2)));
    assert_eq!(workspace.selection.selected_sectors.len(), 2);
}

#[test]
fn viewport_3d_box_select_selects_projected_floor_faces() {
    let mut project = ProjectDocument::new("box-select-3d");
    let mut grid = WorldGrid::empty(2, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    grid.set_floor(1, 0, 0, None);
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("box-select-3d"), project);
    workspace.camera_rig.mode = ViewportCameraMode::Free;
    workspace.camera_rig.free_position = [512, 512, -2048];
    workspace.camera_rig.free_yaw = 2048;
    workspace.camera_rig.free_pitch = signed_to_q12(-128);
    workspace.camera_rig.free_initialized = true;

    let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
    let center = project_world_to_viewport_screen(
        workspace.viewport_3d_camera(),
        viewport,
        [512.0, 0.0, 512.0],
    )
    .expect("floor center projects");
    workspace.begin_viewport_3d_box_select(
        center - Vec2::splat(10.0),
        Some(room),
        egui::Modifiers::NONE,
    );
    assert!(workspace.update_viewport_3d_box_select(center + Vec2::splat(10.0), viewport));

    let floor_0 = Selection::Face(FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Floor,
    });
    let floor_1 = Selection::Face(FaceRef {
        room,
        sx: 1,
        sz: 0,
        kind: FaceKind::Floor,
    });
    assert!(workspace.selection.selected_primitives.contains(&floor_0));
    assert!(!workspace.selection.selected_primitives.contains(&floor_1));
    assert_eq!(workspace.selection.selected_primitives.len(), 1);
    assert_eq!(workspace.selection.selected_node, room);
}

#[test]
fn additive_viewport_3d_box_select_keeps_initial_primitive_selection() {
    let mut project = ProjectDocument::new("box-select-3d-additive");
    let mut grid = WorldGrid::empty(2, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    grid.set_floor(1, 0, 0, None);
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("box-select-3d-additive"), project);
    workspace.camera_rig.mode = ViewportCameraMode::Free;
    workspace.camera_rig.free_position = [512, 512, -2048];
    workspace.camera_rig.free_yaw = 2048;
    workspace.camera_rig.free_pitch = signed_to_q12(-128);
    workspace.camera_rig.free_initialized = true;

    let floor_0 = Selection::Face(FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Floor,
    });
    let floor_1 = Selection::Face(FaceRef {
        room,
        sx: 1,
        sz: 0,
        kind: FaceKind::Floor,
    });
    workspace.replace_primitive_selection(floor_1);

    let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
    let center = project_world_to_viewport_screen(
        workspace.viewport_3d_camera(),
        viewport,
        [512.0, 0.0, 512.0],
    )
    .expect("floor center projects");
    let mut shift = egui::Modifiers::NONE;
    shift.shift = true;
    workspace.begin_viewport_3d_box_select(center - Vec2::splat(10.0), Some(room), shift);
    assert!(workspace.update_viewport_3d_box_select(center + Vec2::splat(10.0), viewport));

    assert!(workspace.selection.selected_primitives.contains(&floor_0));
    assert!(workspace.selection.selected_primitives.contains(&floor_1));
    assert_eq!(workspace.selection.selected_primitives.len(), 2);
}

#[test]
fn floating_duplicate_previews_moves_and_commits_world_geometry() {
    let mut project = ProjectDocument::new("geometry-duplicate");
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    grid.sector_mut(0, 0)
        .unwrap()
        .floor
        .as_mut()
        .unwrap()
        .heights = [0, 32, 64, 96];
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("geometry-duplicate"), project);

    workspace.select_sector((room, 0, 0), egui::Modifiers::NONE);
    workspace.begin_floating_geometry_duplicate();

    let grid = workspace.room_grid_view(room).unwrap();
    assert_eq!(grid.width, 2);
    assert_eq!(
        grid.sector(1, 0).unwrap().floor.as_ref().unwrap().heights,
        [0, 32, 64, 96]
    );
    assert!(workspace.selection.selected_sectors.contains(&(room, 1, 0)));
    assert_eq!(workspace.selection.selected_sector, Some((1, 0)));
    assert!(!workspace.is_dirty());

    assert!(workspace.update_floating_geometry_origin([0, 1]));
    let grid = workspace.room_grid_view(room).unwrap();
    assert_eq!((grid.width, grid.depth), (1, 2));
    assert_eq!(
        grid.sector(0, 1).unwrap().floor.as_ref().unwrap().heights,
        [0, 32, 64, 96]
    );
    assert!(workspace.selection.selected_sectors.contains(&(room, 0, 1)));
    assert_eq!(workspace.selection.selected_sector, Some((0, 1)));
    assert!(!workspace.is_dirty());

    // Placement must restore the duplicate selection even if another input
    // path disturbed the transient preview selection during the click frame.
    workspace.clear_sector_selection();
    assert!(workspace.commit_floating_geometry());
    assert!(workspace.floating_geometry.is_none());
    assert!(workspace.is_dirty());
    assert!(workspace.selection.selected_sectors.contains(&(room, 0, 1)));
    assert_eq!(workspace.selection.selected_sector, Some((0, 1)));

    workspace.do_undo();
    let grid = workspace.room_grid_view(room).unwrap();
    assert_eq!((grid.width, grid.depth), (1, 1));
    assert!(grid.sector(0, 1).is_none());
}

#[test]
fn floating_duplicate_stays_adjacent_until_the_pointer_deliberately_moves() {
    let mut project = ProjectDocument::new("geometry-duplicate-pointer-anchor");
    let mut grid = WorldGrid::empty(2, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    grid.set_floor(1, 0, 0, None);
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("geometry-duplicate-pointer-anchor"), project);

    workspace.select_sector((room, 0, 0), egui::Modifiers::NONE);
    let mut additive = egui::Modifiers::NONE;
    additive.ctrl = true;
    workspace.select_sector((room, 1, 0), additive);
    workspace.begin_floating_geometry_duplicate();

    // The two-cell duplicate starts directly beside the source at world cells
    // (2, 0) and (3, 0).
    // The first viewport frame may report a completely unrelated stale mouse
    // cell; observing it must not teleport the preview there.
    assert!(workspace.track_floating_geometry_pointer_origin([20, 20]));
    let grid = workspace.room_grid_view(room).unwrap();
    assert!(grid.sector(2, 0).is_some());
    assert!(grid.sector(3, 0).is_some());
    assert_eq!((grid.width, grid.depth), (4, 1));

    // Dwelling in the same cell still means no deliberate placement motion.
    assert!(workspace.track_floating_geometry_pointer_origin([20, 20]));
    assert_eq!(workspace.room_grid_view(room).unwrap().origin, [0, 0]);

    // Crossing one cell must move the nearby preview by one cell. The pointer
    // itself is twenty cells away, so absolute pointer snapping would create a
    // huge room and reproduce the editor's rough teleport.
    assert!(workspace.track_floating_geometry_pointer_origin([21, 20]));
    let grid = workspace.room_grid_view(room).unwrap();
    let (sx, sz) = grid.world_cell_to_array(3, 0).unwrap();
    assert!(grid.sector(sx, sz).is_some());
    let (sx, sz) = grid.world_cell_to_array(4, 0).unwrap();
    assert!(grid.sector(sx, sz).is_some());
    assert_eq!((grid.width, grid.depth), (5, 1));
    assert!(grid
        .world_cell_to_array(2, 0)
        .is_some_and(|(sx, sz)| grid.sector(sx, sz).is_none()));
}

#[test]
fn floating_duplicate_of_face_selection_copies_only_selected_primitives() {
    let mut project = ProjectDocument::new("primitive-geometry-duplicate");
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    grid.sector_mut(0, 0)
        .unwrap()
        .floor
        .as_mut()
        .unwrap()
        .heights = [0, 32, 64, 96];
    grid.ensure_sector(0, 0).unwrap().ceiling = Some(GridHorizontalFace::flat(1024, None));
    grid.add_wall(0, 0, GridDirection::North, 0, 1024, None);
    grid.add_wall(0, 0, GridDirection::South, 0, 1024, None);
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("primitive-geometry-duplicate"), project);
    let floor = Selection::Face(FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Floor,
    });
    let north_wall = Selection::Face(FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Wall {
            dir: GridDirection::North,
            stack: 0,
        },
    });
    let mut shift = egui::Modifiers::NONE;
    shift.shift = true;
    workspace.apply_primitive_selection_modifiers(floor, egui::Modifiers::NONE);
    workspace.apply_primitive_selection_modifiers(north_wall, shift);

    workspace.begin_floating_geometry_duplicate();

    let grid = workspace.room_grid_view(room).unwrap();
    let duplicate = grid.sector(1, 0).expect("preview creates target sector");
    assert_eq!(duplicate.floor.as_ref().unwrap().heights, [0, 32, 64, 96]);
    assert!(duplicate.ceiling.is_none());
    assert_eq!(duplicate.walls.get(GridDirection::North).len(), 1);
    assert!(duplicate.walls.get(GridDirection::South).is_empty());
    assert!(workspace.selection.selected_sectors.is_empty());
    assert!(workspace
        .selection
        .selected_primitives
        .contains(&Selection::Face(FaceRef {
            room,
            sx: 1,
            sz: 0,
            kind: FaceKind::Floor,
        })));
    assert!(workspace
        .selection
        .selected_primitives
        .contains(&Selection::Face(FaceRef {
            room,
            sx: 1,
            sz: 0,
            kind: FaceKind::Wall {
                dir: GridDirection::North,
                stack: 0,
            },
        })));
    assert_eq!(workspace.selection.selected_primitives.len(), 2);
    assert!(!workspace.is_dirty());

    workspace.clear_primitive_selection_state();
    assert!(workspace.commit_floating_geometry());
    assert!(workspace
        .selection
        .selected_primitives
        .contains(&Selection::Face(FaceRef {
            room,
            sx: 1,
            sz: 0,
            kind: FaceKind::Floor,
        })));
    assert!(workspace
        .selection
        .selected_primitives
        .contains(&Selection::Face(FaceRef {
            room,
            sx: 1,
            sz: 0,
            kind: FaceKind::Wall {
                dir: GridDirection::North,
                stack: 0,
            },
        })));
    assert_eq!(workspace.selection.selected_primitives.len(), 2);
}

#[test]
fn floating_duplicate_flip_x_mirrors_preview_geometry() {
    let mut project = ProjectDocument::new("geometry-duplicate-flip-x");
    let mut grid = WorldGrid::empty(2, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    grid.sector_mut(0, 0)
        .unwrap()
        .floor
        .as_mut()
        .unwrap()
        .heights = [0, 32, 64, 96];
    grid.ensure_sector(0, 0)
        .unwrap()
        .walls
        .get_mut(GridDirection::North)
        .push(GridVerticalFace::with_heights([0, 10, 110, 100], None));
    grid.set_floor(1, 0, 0, None);
    grid.sector_mut(1, 0)
        .unwrap()
        .floor
        .as_mut()
        .unwrap()
        .heights = [100, 132, 164, 196];
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("geometry-duplicate-flip-x"), project);
    let mut ctrl = egui::Modifiers::NONE;
    ctrl.ctrl = true;
    workspace.select_sector((room, 0, 0), egui::Modifiers::NONE);
    workspace.select_sector((room, 1, 0), ctrl);

    workspace.begin_floating_geometry_duplicate();
    workspace.flip_floating_geometry_x();

    let grid = workspace.room_grid_view(room).unwrap();
    assert_eq!(
        grid.sector(2, 0).unwrap().floor.as_ref().unwrap().heights,
        [132, 100, 196, 164]
    );
    let mirrored_first = grid.sector(3, 0).unwrap();
    assert_eq!(
        mirrored_first.floor.as_ref().unwrap().heights,
        [32, 0, 96, 64]
    );
    assert_eq!(
        mirrored_first.walls.get(GridDirection::North)[0].heights,
        [10, 0, 100, 110]
    );
    assert!(workspace.selection.selected_sectors.contains(&(room, 2, 0)));
    assert!(workspace.selection.selected_sectors.contains(&(room, 3, 0)));
    assert!(!workspace.is_dirty());
}

#[test]
fn flip_sector_z_reverses_floor_and_wall_orientation() {
    let mut sector = GridSector::empty();
    sector.floor = Some(GridHorizontalFace::flat(0, None));
    sector.floor.as_mut().unwrap().heights = [0, 32, 64, 96];
    sector
        .walls
        .get_mut(GridDirection::North)
        .push(GridVerticalFace::with_heights([1, 2, 3, 4], None));
    sector
        .walls
        .get_mut(GridDirection::NorthWestSouthEast)
        .push(GridVerticalFace::with_heights([5, 6, 7, 8], None));

    let flipped = flip_sector_z(&sector);

    assert_eq!(flipped.floor.as_ref().unwrap().heights, [96, 64, 32, 0]);
    assert!(flipped.walls.get(GridDirection::North).is_empty());
    assert_eq!(
        flipped.walls.get(GridDirection::South)[0].heights,
        [2, 1, 4, 3]
    );
    assert!(flipped
        .walls
        .get(GridDirection::NorthWestSouthEast)
        .is_empty());
    assert_eq!(
        flipped.walls.get(GridDirection::NorthEastSouthWest)[0].heights,
        [6, 5, 8, 7]
    );
}

#[test]
fn flip_sector_x_remaps_split_triangles_and_diagonal_walls() {
    let mut sector = GridSector::empty();
    let mut face = GridHorizontalFace::flat(0, None);
    face.heights = [0, 10, 20, 30];
    face.split = GridSplit::NorthWestSouthEast;
    face.dropped_corner = Some(Corner::NW);
    face.uv.flip_u = false;
    face.triangle_override_mut(0).heights = Some([100, 110, 120]);
    face.triangle_override_mut(0).uv = Some(GridUvTransform::IDENTITY);
    face.triangle_override_mut(0).walkable = Some(false);
    sector.floor = Some(face);
    sector
        .walls
        .get_mut(GridDirection::NorthWestSouthEast)
        .push(GridVerticalFace::with_heights([1, 2, 3, 4], None));

    let flipped = flip_sector_x(&sector);
    let floor = flipped.floor.as_ref().unwrap();

    assert_eq!(floor.heights, [10, 0, 30, 20]);
    assert_eq!(floor.split, GridSplit::NorthEastSouthWest);
    assert_eq!(floor.dropped_corner, Some(Corner::NE));
    assert!(floor.uv.flip_u);
    assert!(!floor.uv.flip_v);
    assert_eq!(floor.triangle_override(0).heights, Some([110, 100, 120]));
    assert!(floor.triangle_override(0).uv.unwrap().flip_u);
    assert_eq!(floor.triangle_override(0).walkable, Some(false));
    assert!(flipped
        .walls
        .get(GridDirection::NorthWestSouthEast)
        .is_empty());
    assert_eq!(
        flipped.walls.get(GridDirection::NorthEastSouthWest)[0].heights,
        [1, 2, 3, 4]
    );
}

#[test]
fn flip_sector_z_remaps_split_triangles_and_dropped_wall_corners() {
    let mut sector = GridSector::empty();
    let mut face = GridHorizontalFace::flat(0, None);
    face.heights = [0, 10, 20, 30];
    face.split = GridSplit::NorthEastSouthWest;
    face.dropped_corner = Some(Corner::SE);
    face.uv.flip_v = false;
    face.triangle_override_mut(1).heights = Some([200, 210, 220]);
    face.triangle_override_mut(1).uv = Some(GridUvTransform::IDENTITY);
    sector.ceiling = Some(face);
    let mut diagonal = GridVerticalFace::with_heights([5, 6, 7, 8], None);
    diagonal.dropped_corner = Some(WallCorner::TR);
    sector
        .walls
        .get_mut(GridDirection::NorthEastSouthWest)
        .push(diagonal);

    let flipped = flip_sector_z(&sector);
    let ceiling = flipped.ceiling.as_ref().unwrap();

    assert_eq!(ceiling.heights, [30, 20, 10, 0]);
    assert_eq!(ceiling.split, GridSplit::NorthWestSouthEast);
    assert_eq!(ceiling.dropped_corner, Some(Corner::NE));
    assert!(!ceiling.uv.flip_u);
    assert!(ceiling.uv.flip_v);
    assert_eq!(ceiling.triangle_override(0).heights, Some([220, 210, 200]));
    assert!(ceiling.triangle_override(0).uv.unwrap().flip_v);
    assert!(flipped
        .walls
        .get(GridDirection::NorthEastSouthWest)
        .is_empty());
    let wall = &flipped.walls.get(GridDirection::NorthWestSouthEast)[0];
    assert_eq!(wall.heights, [6, 5, 8, 7]);
    assert_eq!(wall.dropped_corner, Some(WallCorner::TL));
}

#[test]
fn floating_duplicate_cancel_restores_base_world_geometry() {
    let mut project = ProjectDocument::new("geometry-duplicate-cancel");
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    grid.sector_mut(0, 0)
        .unwrap()
        .floor
        .as_mut()
        .unwrap()
        .heights = [0, 32, 64, 96];
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("geometry-duplicate-cancel"), project);

    workspace.select_sector((room, 0, 0), egui::Modifiers::NONE);
    workspace.begin_floating_geometry_duplicate();
    assert!(workspace.update_floating_geometry_origin([0, 1]));

    assert!(workspace.cancel_floating_geometry());

    let grid = workspace.room_grid_view(room).unwrap();
    assert_eq!((grid.width, grid.depth), (1, 1));
    assert_eq!(
        grid.sector(0, 0).unwrap().floor.as_ref().unwrap().heights,
        [0, 32, 64, 96]
    );
    assert!(grid.sector(0, 1).is_none());
    assert!(workspace.floating_geometry.is_none());
    assert!(workspace.selection.selected_sectors.is_empty());
    assert!(!workspace.is_dirty());
}

#[test]
fn rotate_selected_world_geometry_rotates_cells_and_wall_orientation() {
    let mut project = ProjectDocument::new("geometry-rotate");
    let mut grid = WorldGrid::empty(2, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    grid.sector_mut(0, 0)
        .unwrap()
        .floor
        .as_mut()
        .unwrap()
        .heights = [0, 32, 64, 96];
    grid.ensure_sector(0, 0)
        .unwrap()
        .walls
        .get_mut(GridDirection::North)
        .push(GridVerticalFace::with_heights([0, 10, 110, 100], None));
    grid.set_floor(1, 0, 0, None);
    grid.sector_mut(1, 0)
        .unwrap()
        .floor
        .as_mut()
        .unwrap()
        .heights = [100, 132, 164, 196];
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("geometry-rotate"), project);
    let mut ctrl = egui::Modifiers::NONE;
    ctrl.ctrl = true;
    workspace.select_sector((room, 0, 0), egui::Modifiers::NONE);
    workspace.select_sector((room, 1, 0), ctrl);

    workspace.rotate_selected_geometry_cw();

    let grid = workspace.room_grid_view(room).unwrap();
    assert_eq!((grid.width, grid.depth), (2, 2));
    assert!(grid.sector(1, 0).is_none());
    assert_eq!(
        grid.sector(0, 1).unwrap().floor.as_ref().unwrap().heights,
        [96, 0, 32, 64]
    );
    let rotated_wall_sector = grid.sector(0, 1).unwrap();
    assert!(rotated_wall_sector
        .walls
        .get(GridDirection::North)
        .is_empty());
    assert_eq!(
        rotated_wall_sector.walls.get(GridDirection::East)[0].heights,
        [0, 10, 110, 100]
    );
    assert_eq!(
        grid.sector(0, 0).unwrap().floor.as_ref().unwrap().heights,
        [196, 100, 132, 164]
    );
    assert!(workspace.selection.selected_sectors.contains(&(room, 0, 0)));
    assert!(workspace.selection.selected_sectors.contains(&(room, 0, 1)));
    assert_eq!(workspace.selection.selected_sectors.len(), 2);
    assert!(workspace.is_dirty());
}
