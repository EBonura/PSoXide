use super::*;

#[test]
fn rotate_sector_preserves_authored_uv_rotation() {
    let mut sector = GridSector::empty();
    let mut floor = GridHorizontalFace::flat(0, None);
    floor.uv.rotation = GridUvRotation::Deg45;
    let mut floor_tri_a = GridUvTransform::IDENTITY;
    floor_tri_a.rotation = GridUvRotation::Deg135;
    floor.triangle_override_mut(0).uv = Some(floor_tri_a);
    let mut floor_tri_b = GridUvTransform::IDENTITY;
    floor_tri_b.rotation = GridUvRotation::Deg225;
    floor.triangle_override_mut(1).uv = Some(floor_tri_b);
    sector.floor = Some(floor);

    let mut ceiling = GridHorizontalFace::flat(1024, None);
    ceiling.uv.rotation = GridUvRotation::Deg315;
    sector.ceiling = Some(ceiling);

    let mut wall = GridVerticalFace::with_heights([0, 10, 110, 100], None);
    wall.uv.rotation = GridUvRotation::Deg90;
    sector.walls.get_mut(GridDirection::North).push(wall);

    let rotated = rotate_sector_cw(&sector);
    let floor = rotated.floor.as_ref().unwrap();
    let floor_override_rotations = [
        floor.triangle_override(0).uv.unwrap().rotation,
        floor.triangle_override(1).uv.unwrap().rotation,
    ];

    assert_eq!(floor.uv.rotation, GridUvRotation::Deg45);
    assert!(floor_override_rotations.contains(&GridUvRotation::Deg135));
    assert!(floor_override_rotations.contains(&GridUvRotation::Deg225));
    assert_eq!(
        rotated.ceiling.as_ref().unwrap().uv.rotation,
        GridUvRotation::Deg315
    );
    assert_eq!(
        rotated.walls.get(GridDirection::East)[0].uv.rotation,
        GridUvRotation::Deg90
    );
}

#[test]
fn rotate_sector_reverses_diagonal_wall_endpoint_order_when_needed() {
    let mut sector = GridSector::empty();
    sector
        .walls
        .get_mut(GridDirection::NorthEastSouthWest)
        .push(GridVerticalFace::with_heights([1, 2, 3, 4], None));

    let rotated = rotate_sector_cw(&sector);

    assert!(rotated
        .walls
        .get(GridDirection::NorthEastSouthWest)
        .is_empty());
    assert_eq!(
        rotated.walls.get(GridDirection::NorthWestSouthEast)[0].heights,
        [2, 1, 4, 3]
    );
}

#[test]
fn shift_selects_wall_span_from_anchor() {
    let mut project = ProjectDocument::new("wall-span");
    let mut grid = WorldGrid::empty(4, 1, 1024);
    for sx in 0..4 {
        grid.add_wall(sx, 0, GridDirection::North, 0, 1024, None);
    }
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    let wall_at = |sx| {
        Selection::Face(FaceRef {
            room,
            sx,
            sz: 0,
            kind: FaceKind::Wall {
                dir: GridDirection::North,
                stack: 0,
            },
        })
    };

    let mut shift = egui::Modifiers::NONE;
    shift.shift = true;
    workspace.apply_primitive_selection_modifiers(wall_at(0), egui::Modifiers::NONE);
    workspace.apply_primitive_selection_modifiers(wall_at(3), shift);

    assert_eq!(workspace.selection.selected_primitives.len(), 4);
    for sx in 0..4 {
        assert!(workspace
            .selection
            .selected_primitives
            .contains(&wall_at(sx)));
    }
    assert_eq!(workspace.selection.selected_primitive, Some(wall_at(3)));
}

#[test]
fn shift_selects_wall_top_edge_path_from_anchor() {
    let mut project = ProjectDocument::new("wall-edge-path");
    let mut grid = WorldGrid::empty(4, 1, 1024);
    for sx in 0..4 {
        grid.add_wall(sx, 0, GridDirection::North, 0, 1024, None);
    }
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    let edge_at = |sx| {
        Selection::Edge(EdgeRef {
            room,
            anchor: EdgeAnchor::Wall {
                sx,
                sz: 0,
                dir: GridDirection::North,
                stack: 0,
                edge: WallEdge::Top,
            },
        })
    };

    let mut shift = egui::Modifiers::NONE;
    shift.shift = true;
    workspace.apply_primitive_selection_modifiers(edge_at(0), egui::Modifiers::NONE);
    workspace.apply_primitive_selection_modifiers(edge_at(3), shift);

    assert_eq!(workspace.selection.selected_primitives.len(), 4);
    for sx in 0..4 {
        assert!(workspace
            .selection
            .selected_primitives
            .contains(&edge_at(sx)));
    }
    assert_eq!(workspace.selection.selected_primitive, Some(edge_at(3)));
}

#[test]
fn shift_selects_floor_edge_path_from_anchor() {
    let mut project = ProjectDocument::new("floor-edge-path");
    let mut grid = WorldGrid::empty(4, 1, 1024);
    for sx in 0..4 {
        grid.set_floor(sx, 0, 0, None);
    }
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    let edge_at = |sx| {
        Selection::Edge(EdgeRef {
            room,
            anchor: EdgeAnchor::Floor {
                sx,
                sz: 0,
                dir: GridDirection::North,
            },
        })
    };

    let mut shift = egui::Modifiers::NONE;
    shift.shift = true;
    workspace.apply_primitive_selection_modifiers(edge_at(0), egui::Modifiers::NONE);
    workspace.apply_primitive_selection_modifiers(edge_at(3), shift);

    assert_eq!(workspace.selection.selected_primitives.len(), 4);
    for sx in 0..4 {
        assert!(workspace
            .selection
            .selected_primitives
            .contains(&edge_at(sx)));
    }
    assert_eq!(workspace.selection.selected_primitive, Some(edge_at(3)));
}

#[test]
fn modified_primitive_selection_can_mix_floor_ceiling_and_wall_faces() {
    let mut project = ProjectDocument::new("mixed-face-selection");
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    grid.ensure_sector(0, 0).unwrap().ceiling = Some(GridHorizontalFace::flat(1024, None));
    grid.add_wall(0, 0, GridDirection::North, 0, 1024, None);
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("mixed-face-selection"), project);
    let floor = Selection::Face(FaceRef {
        room,
        sx: 0,
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
    let mut ctrl = egui::Modifiers::NONE;
    ctrl.ctrl = true;
    let mut shift = egui::Modifiers::NONE;
    shift.shift = true;

    workspace.apply_primitive_selection_modifiers(floor, egui::Modifiers::NONE);
    workspace.apply_primitive_selection_modifiers(ceiling, ctrl);
    workspace.apply_primitive_selection_modifiers(wall, shift);

    assert!(workspace.selection.selected_primitives.contains(&floor));
    assert!(workspace.selection.selected_primitives.contains(&ceiling));
    assert!(workspace.selection.selected_primitives.contains(&wall));
    assert_eq!(workspace.selection.selected_primitives.len(), 3);
    assert!(workspace.selection.selected_sectors.is_empty());
    assert_eq!(workspace.selection.selected_primitive, Some(wall));
}

#[test]
fn primitive_grid_drag_moves_selected_faces_without_whole_sector() {
    let mut project = ProjectDocument::new("primitive-grid-drag");
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
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("primitive-grid-drag"), project);
    let floor = Selection::Face(FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Floor,
    });

    workspace.selection.hovered_primitive = Some(floor);
    workspace.replace_primitive_selection(floor);
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(320.0, 240.0));
    assert!(workspace.begin_primitive_grid_drag(rect, rect.center(), egui::Modifiers::NONE));
    workspace
        .interaction
        .primitive_grid_drag_mut()
        .unwrap()
        .current_delta = [1, 0];
    workspace.apply_primitive_grid_drag_preview();

    let grid = workspace.room_grid_view(room).unwrap();
    let source = grid.sector(0, 0).unwrap();
    assert!(source.floor.is_none());
    assert!(source.ceiling.is_some());
    assert_eq!(source.walls.get(GridDirection::North).len(), 1);
    let moved = grid.sector(1, 0).unwrap();
    assert_eq!(moved.floor.as_ref().unwrap().heights, [0, 32, 64, 96]);
    assert!(moved.ceiling.is_none());
    assert!(moved.walls.get(GridDirection::North).is_empty());
    assert!(workspace
        .selection
        .selected_primitives
        .contains(&Selection::Face(FaceRef {
            room,
            sx: 1,
            sz: 0,
            kind: FaceKind::Floor,
        })));
    assert!(workspace.selection.selected_sectors.is_empty());

    workspace.end_primitive_grid_drag();
    assert!(workspace.is_dirty());
    workspace.do_undo();
    let grid = workspace.room_grid_view(room).unwrap();
    assert!(grid.sector(1, 0).is_none());
    assert!(grid.sector(0, 0).unwrap().floor.is_some());
}

#[test]
fn primitive_gizmo_y_moves_selected_face_by_height_quantum() {
    let mut project = ProjectDocument::new("primitive-gizmo-y");
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("primitive-gizmo-y"), project);
    set_gizmo_test_camera(&mut workspace);
    let floor = Selection::Face(FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Floor,
    });
    workspace.replace_primitive_selection(floor);

    let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
    let y_axis = projected_gizmo_axis(&workspace, viewport, PrimitiveGizmoAxis::Y);
    let unit = (y_axis.end - y_axis.start).normalized();
    assert!(workspace.begin_primitive_gizmo_drag(PrimitiveGizmoAxis::Y, viewport, y_axis.start));
    workspace.update_primitive_gizmo_drag(y_axis.start + unit * 4.0);
    workspace.end_primitive_gizmo_drag();

    let grid = workspace.room_grid_view(room).unwrap();
    assert_eq!(
        grid.sector(0, 0).unwrap().floor.as_ref().unwrap().heights,
        [HEIGHT_QUANTUM; 4]
    );
    assert!(workspace.is_dirty());

    workspace.do_undo();
    let grid = workspace.room_grid_view(room).unwrap();
    assert_eq!(
        grid.sector(0, 0).unwrap().floor.as_ref().unwrap().heights,
        [0; 4]
    );
}

#[test]
fn viewport_3d_pointer_target_prefers_primitive_gizmo_over_surface() {
    let mut project = ProjectDocument::new("primitive-gizmo-target-priority");
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("primitive-gizmo-target"), project);
    set_gizmo_test_camera(&mut workspace);
    workspace.replace_primitive_selection(Selection::Face(FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Floor,
    }));

    let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
    let z_axis = projected_gizmo_axis(&workspace, viewport, PrimitiveGizmoAxis::Z);
    let target =
        workspace.resolve_viewport_3d_pointer_target(viewport, z_axis.end, Some(room), true);

    assert!(
        matches!(target, Some(Viewport3dPointerTarget::PrimitiveGizmo(_))),
        "target was {target:?}"
    );
    assert!(target
        .and_then(Viewport3dPointerTarget::primitive_selection)
        .is_none());
}

#[test]
fn primitive_gizmo_y_moves_selected_triangle_by_height_quantum() {
    let mut project = ProjectDocument::new("primitive-gizmo-triangle-y");
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("primitive-gizmo-triangle-y"), project);
    set_gizmo_test_camera(&mut workspace);
    let triangle = Selection::Triangle(HorizontalTriangleRef {
        room,
        sx: 0,
        sz: 0,
        surface: HorizontalSurfaceKind::Floor,
        index: HorizontalTriangleIndex::A,
        corners: [Corner::NW, Corner::NE, Corner::SE],
    });
    workspace.replace_primitive_selection(triangle);

    let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
    let y_axis = projected_gizmo_axis(&workspace, viewport, PrimitiveGizmoAxis::Y);
    let unit = (y_axis.end - y_axis.start).normalized();
    assert!(workspace.begin_primitive_gizmo_drag(PrimitiveGizmoAxis::Y, viewport, y_axis.start));
    workspace.update_primitive_gizmo_drag(y_axis.start + unit * 4.0);
    workspace.end_primitive_gizmo_drag();

    let grid = workspace.room_grid_view(room).unwrap();
    let floor = grid.sector(0, 0).unwrap().floor.as_ref().unwrap();
    assert_eq!(floor.heights, [0; 4]);
    assert_eq!(
        floor.triangle_heights(HorizontalTriangleIndex::A.idx()),
        [HEIGHT_QUANTUM; 3]
    );
    assert_eq!(
        floor.triangle_heights(HorizontalTriangleIndex::B.idx()),
        [0; 3]
    );
    assert!(workspace.is_dirty());
}

#[test]
fn primitive_gizmo_x_moves_selected_face_one_cell() {
    let mut project = ProjectDocument::new("primitive-gizmo-x");
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
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("primitive-gizmo-x"), project);
    set_gizmo_test_camera(&mut workspace);
    let floor = Selection::Face(FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Floor,
    });
    workspace.replace_primitive_selection(floor);

    let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
    let x_axis = projected_gizmo_axis(&workspace, viewport, PrimitiveGizmoAxis::X);
    assert!(workspace.begin_primitive_gizmo_drag(PrimitiveGizmoAxis::X, viewport, x_axis.start));
    workspace.update_primitive_gizmo_drag(x_axis.start + (x_axis.end - x_axis.start) * 0.5);

    let grid = workspace.room_grid_view(room).unwrap();
    assert!(grid.sector(0, 0).unwrap().floor.is_none());
    assert_eq!(
        grid.sector(1, 0).unwrap().floor.as_ref().unwrap().heights,
        [0, 32, 64, 96]
    );
    assert!(workspace
        .selection
        .selected_primitives
        .contains(&Selection::Face(FaceRef {
            room,
            sx: 1,
            sz: 0,
            kind: FaceKind::Floor,
        })));

    workspace.end_primitive_gizmo_drag();
    assert!(workspace.is_dirty());
    workspace.do_undo();
    let grid = workspace.room_grid_view(room).unwrap();
    assert!(grid.sector(1, 0).is_none());
    assert!(grid.sector(0, 0).unwrap().floor.is_some());
}

#[test]
fn node_gizmo_axes_appear_for_selected_entity_and_light() {
    let mut project = ProjectDocument::new("node-gizmo-axes");
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
    let light = project.active_scene_mut().add_node(
        room,
        "Light",
        NodeKind::PointLight {
            color: [255, 240, 200],
            intensity: 1.0,
            radius: 4.0,
        },
    );
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("node-gizmo-axes"), project);
    set_gizmo_test_camera(&mut workspace);
    let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));

    workspace.replace_node_selection(entity);
    let entity_axes: HashSet<_> = workspace
        .node_gizmo_screen_axes(viewport)
        .into_iter()
        .map(|axis| axis.axis)
        .collect();
    assert!(entity_axes.contains(&PrimitiveGizmoAxis::X));
    assert!(entity_axes.contains(&PrimitiveGizmoAxis::Y));
    assert!(entity_axes.contains(&PrimitiveGizmoAxis::Z));

    workspace.replace_node_selection(light);
    let light_axes: HashSet<_> = workspace
        .node_gizmo_screen_axes(viewport)
        .into_iter()
        .map(|axis| axis.axis)
        .collect();
    assert!(light_axes.contains(&PrimitiveGizmoAxis::X));
    assert!(light_axes.contains(&PrimitiveGizmoAxis::Y));
    assert!(light_axes.contains(&PrimitiveGizmoAxis::Z));
}

#[test]
fn node_gizmo_move_planes_appear_for_selected_entity() {
    let mut project = ProjectDocument::new("node-gizmo-planes");
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
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("node-gizmo-planes"), project);
    set_gizmo_test_camera(&mut workspace);
    workspace.camera_rig.free_position = [2048, 1024, -2048];
    let (yaw, pitch) = camera_angles_to_look_at(
        workspace.camera_rig.free_position,
        [
            DEFAULT_WORLD_SECTOR_SIZE / 2,
            DEFAULT_WORLD_SECTOR_SIZE / 4,
            DEFAULT_WORLD_SECTOR_SIZE / 2,
        ],
    )
    .expect("oblique gizmo test camera can face the entity");
    workspace.camera_rig.free_yaw = yaw;
    workspace.camera_rig.free_pitch = pitch;
    workspace.replace_node_selection(entity);

    let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
    let planes: HashSet<_> = workspace
        .node_gizmo_screen_planes(viewport)
        .into_iter()
        .map(|plane| plane.plane)
        .collect();

    assert!(planes.contains(&NodeGizmoPlane::XY));
    assert!(planes.contains(&NodeGizmoPlane::XZ));
    assert!(planes.contains(&NodeGizmoPlane::YZ));
}

#[test]
fn node_gizmo_xy_plane_moves_entity_on_two_axes() {
    let mut project = ProjectDocument::new("entity-gizmo-xy");
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
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("entity-gizmo-xy"), project);
    set_gizmo_test_camera(&mut workspace);
    workspace.replace_node_selection(entity);

    let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
    let screen_plane = projected_node_gizmo_plane(&workspace, viewport, NodeGizmoPlane::XY);
    let start = screen_plane_center(screen_plane);
    assert_eq!(
        workspace.pick_node_gizmo_handle(viewport, start),
        Some(NodeGizmoHandle::Plane(NodeGizmoPlane::XY))
    );
    assert!(workspace.begin_node_gizmo_handle_drag(
        NodeGizmoHandle::Plane(NodeGizmoPlane::XY),
        viewport,
        start
    ));
    let start_hit = workspace
        .interaction
        .node_gizmo_drag()
        .and_then(|drag| drag.start_plane_hit)
        .expect("plane drag stores start hit");
    let target_hit = [
        start_hit[0] + HEIGHT_QUANTUM as f32,
        start_hit[1] + HEIGHT_QUANTUM as f32,
        start_hit[2],
    ];
    let target_pointer =
        project_world_to_viewport_screen(workspace.viewport_3d_camera(), viewport, target_hit)
            .expect("target hit projects");

    workspace.update_node_gizmo_drag(viewport, target_pointer, false);
    workspace.end_node_gizmo_drag();

    let node = workspace.project.active_scene().node(entity).unwrap();
    assert_vec3_approx(
        node.transform.translation,
        [
            HEIGHT_QUANTUM as f32 / 1024.0,
            HEIGHT_QUANTUM as f32 / 1024.0,
            0.0,
        ],
    );
    assert_eq!(workspace.status, "Moved 1 node on XY");
    assert!(workspace.is_dirty());

    workspace.do_undo();
    let node = workspace.project.active_scene().node(entity).unwrap();
    assert_eq!(node.transform.translation, [0.0, 0.0, 0.0]);
}

#[test]
fn node_gizmo_moves_bsp_entity_in_world_units() {
    // BSP scenes hang entities off the root in raw world units; a gizmo
    // step must move snap_units world units (the 1/1024-speed regression),
    // and Shift (free) must drop to single-unit precision.
    let mut project = ProjectDocument::new("entity-gizmo-bsp");
    project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [0, 0, 0],
            [256, 256, 256],
        ));
    let entity = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Entity", NodeKind::Entity);
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("entity-gizmo-bsp"), project);
    set_gizmo_test_camera(&mut workspace);
    workspace.replace_node_selection(entity);
    assert_eq!(
        node_translation_sector_size(&workspace.project, entity),
        1,
        "roomless BSP node authors in world units"
    );

    let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
    let x_axis = projected_node_gizmo_axis(&workspace, viewport, PrimitiveGizmoAxis::X);
    let unit = (x_axis.end - x_axis.start).normalized();
    assert!(workspace.begin_node_gizmo_drag(PrimitiveGizmoAxis::X, viewport, x_axis.start));
    workspace.update_node_gizmo_drag(viewport, x_axis.start + unit * 4.0, false);
    workspace.end_node_gizmo_drag();
    let node = workspace.project.active_scene().node(entity).unwrap();
    let step = f32::from(workspace.snap_units.max(1));
    assert!(
        (node.transform.translation[0] - step).abs() < 0.001,
        "one gizmo step = one grid step in world units, got {}",
        node.transform.translation[0]
    );
    workspace.do_undo();

    // Shift: single-unit steps.
    let x_axis = projected_node_gizmo_axis(&workspace, viewport, PrimitiveGizmoAxis::X);
    let unit = (x_axis.end - x_axis.start).normalized();
    assert!(workspace.begin_node_gizmo_drag(PrimitiveGizmoAxis::X, viewport, x_axis.start));
    workspace.update_node_gizmo_drag(viewport, x_axis.start + unit * 4.0, true);
    workspace.end_node_gizmo_drag();
    let node = workspace.project.active_scene().node(entity).unwrap();
    assert!(
        (node.transform.translation[0] - 1.0).abs() < 0.001,
        "free drag steps single world units, got {}",
        node.transform.translation[0]
    );
}

#[test]
fn node_gizmo_moves_entity_on_selected_axis() {
    let mut project = ProjectDocument::new("entity-gizmo-x");
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
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("entity-gizmo-x"), project);
    set_gizmo_test_camera(&mut workspace);
    workspace.replace_node_selection(entity);

    let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
    let x_axis = projected_node_gizmo_axis(&workspace, viewport, PrimitiveGizmoAxis::X);
    let unit = (x_axis.end - x_axis.start).normalized();
    assert!(workspace.begin_node_gizmo_drag(PrimitiveGizmoAxis::X, viewport, x_axis.start));
    workspace.update_node_gizmo_drag(viewport, x_axis.start + unit * 4.0, false);
    workspace.end_node_gizmo_drag();

    let node = workspace.project.active_scene().node(entity).unwrap();
    assert!((node.transform.translation[0] - HEIGHT_QUANTUM as f32 / 1024.0).abs() < 0.001);
    assert_eq!(node.transform.translation[1], 0.0);
    assert_eq!(node.transform.translation[2], 0.0);
    let scene = workspace.project.active_scene();
    let room_node = scene.node(room).unwrap();
    let NodeKind::Section { grid } = &room_node.kind else {
        unreachable!("test room is a room");
    };
    let world = psxed_project::spatial::node_preview_origin(grid, &node.transform);
    assert_eq!(world[0], DEFAULT_WORLD_SECTOR_SIZE / 2 + HEIGHT_QUANTUM);
    assert!(workspace.is_dirty());

    workspace.do_undo();
    let node = workspace.project.active_scene().node(entity).unwrap();
    assert_eq!(node.transform.translation, [0.0, 0.0, 0.0]);
}

#[test]
fn node_gizmo_moves_point_light_on_y_axis() {
    let mut project = ProjectDocument::new("light-gizmo-y");
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Section {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    let light = project.active_scene_mut().add_node(
        room,
        "Light",
        NodeKind::PointLight {
            color: [255, 240, 200],
            intensity: 1.0,
            radius: 4.0,
        },
    );
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("light-gizmo-y"), project);
    set_gizmo_test_camera(&mut workspace);
    workspace.replace_node_selection(light);

    let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
    let y_axis = projected_node_gizmo_axis(&workspace, viewport, PrimitiveGizmoAxis::Y);
    let unit = (y_axis.end - y_axis.start).normalized();
    assert!(workspace.begin_node_gizmo_drag(PrimitiveGizmoAxis::Y, viewport, y_axis.start));
    workspace.update_node_gizmo_drag(viewport, y_axis.start + unit * 4.0, false);
    workspace.end_node_gizmo_drag();

    let node = workspace.project.active_scene().node(light).unwrap();
    assert_vec3_approx(
        node.transform.translation,
        [0.0, HEIGHT_QUANTUM as f32 / 1024.0, 0.0],
    );
    let scene = workspace.project.active_scene();
    let room_node = scene.node(room).unwrap();
    let NodeKind::Section { grid } = &room_node.kind else {
        unreachable!("test room is a room");
    };
    let world = psxed_project::spatial::node_preview_origin(grid, &node.transform);
    assert_eq!(world[1], HEIGHT_QUANTUM);
    assert!(workspace.is_dirty());

    workspace.do_undo();
    let node = workspace.project.active_scene().node(light).unwrap();
    assert_eq!(node.transform.translation, [0.0, 0.0, 0.0]);
}

#[test]
fn node_gizmo_rotates_image_prop_around_y() {
    let mut project = ProjectDocument::new("image-prop-gizmo-rotate");
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Section {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    let prop = project.active_scene_mut().add_node(
        room,
        "Banner",
        NodeKind::ImageProp {
            material: None,
            width: 1024,
            height: 1024,
            cylindrical_billboard: false,
            collision_enabled: false,
            collision_size: [1024, 1024, 1024],
        },
    );
    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("image-prop-gizmo-rotate"), project);
    set_gizmo_test_camera(&mut workspace);
    workspace.replace_node_selection(prop);
    workspace.transform_gizmo_mode = TransformGizmoMode::Rotate;

    let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
    let ring = workspace
        .node_rotation_gizmo_screen_ring_for_axis(viewport, PrimitiveGizmoAxis::Y)
        .expect("rotation ring projects");
    // Sweep the pointer along the ring in its own point order (ring
    // points advance with a positive world rotation), so the drag must
    // rotate by the swept screen angle, positively, no matter where
    // the camera sits. Radial pointer motion sweeps no angle and must
    // change nothing.
    let start = ring.points[0];
    let target = ring.points[8];
    let start_angle = (start - ring.center).angle();
    let target_angle = (target - ring.center).angle();
    let mut swept = (target_angle - start_angle).to_degrees();
    while swept > 180.0 {
        swept -= 360.0;
    }
    while swept <= -180.0 {
        swept += 360.0;
    }
    let expected_yaw = swept.abs().round();
    assert!(expected_yaw >= 10.0, "test sweep too small: {swept}");

    assert!(workspace.begin_node_gizmo_drag(PrimitiveGizmoAxis::Y, viewport, start));
    let radial = (start - ring.center).normalized();
    workspace.update_node_gizmo_drag(viewport, start + radial * 24.0, false);
    let node = workspace.project.active_scene().node(prop).unwrap();
    assert_eq!(
        node.transform.rotation_degrees[1], 0.0,
        "radial motion must not rotate"
    );
    workspace.update_node_gizmo_drag(viewport, target, false);
    workspace.end_node_gizmo_drag();

    let node = workspace.project.active_scene().node(prop).unwrap();
    assert_eq!(node.transform.rotation_degrees, [0.0, expected_yaw, 0.0]);
    assert!(workspace.is_dirty());

    workspace.do_undo();
    let node = workspace.project.active_scene().node(prop).unwrap();
    assert_eq!(node.transform.rotation_degrees, [0.0, 0.0, 0.0]);
}

#[test]
fn node_gizmo_local_space_rotates_about_node_axis() {
    let mut project = ProjectDocument::new("image-prop-gizmo-local");
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Section {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    let prop = project.active_scene_mut().add_node(
        room,
        "Banner",
        NodeKind::ImageProp {
            material: None,
            width: 1024,
            height: 1024,
            cylindrical_billboard: false,
            collision_enabled: false,
            collision_size: [1024, 1024, 1024],
        },
    );
    let start_rotation = [90.0f32, 0.0, 0.0];
    project
        .active_scene_mut()
        .node_mut(prop)
        .unwrap()
        .transform
        .rotation_degrees = start_rotation;
    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("image-prop-gizmo-local"), project);
    set_gizmo_test_camera(&mut workspace);
    workspace.replace_node_selection(prop);
    workspace.transform_gizmo_mode = TransformGizmoMode::Rotate;
    workspace.gizmo_space = GizmoSpace::Local;

    let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
    let ring = workspace
        .node_rotation_gizmo_screen_ring_for_axis(viewport, PrimitiveGizmoAxis::Y)
        .expect("local rotation ring projects");
    let start = ring.points[0];
    let target = ring.points[8];
    let start_angle = (start - ring.center).angle();
    let target_angle = (target - ring.center).angle();
    let mut swept = (target_angle - start_angle).to_degrees();
    while swept > 180.0 {
        swept -= 360.0;
    }
    while swept <= -180.0 {
        swept += 360.0;
    }
    let steps = swept.abs().round();
    assert!(steps >= 10.0, "test sweep too small: {swept}");

    assert!(workspace.begin_node_gizmo_drag(PrimitiveGizmoAxis::Y, viewport, start));
    workspace.update_node_gizmo_drag(viewport, target, false);
    workspace.end_node_gizmo_drag();

    // A local-space drag must equal composing the delta in the node's
    // own frame; compare rotation matrices since Euler triples alias.
    let expected = psxed_project::spatial::rotate_euler_degrees(
        start_rotation,
        1,
        steps,
        psxed_project::spatial::RotationSpace::Local,
    );
    let node = workspace.project.active_scene().node(prop).unwrap();
    let actual_m = psxed_project::spatial::euler_degrees_to_matrix(node.transform.rotation_degrees);
    let expected_m = psxed_project::spatial::euler_degrees_to_matrix(expected);
    for row in 0..3 {
        for col in 0..3 {
            assert!(
                (actual_m[row][col] - expected_m[row][col]).abs() < 1e-3,
                "actual {:?} expected {expected:?}",
                node.transform.rotation_degrees
            );
        }
    }
    // And it must differ from the global-space composition, proving
    // the toggle reached the apply path.
    let global = psxed_project::spatial::rotate_euler_degrees(
        start_rotation,
        1,
        steps,
        psxed_project::spatial::RotationSpace::Global,
    );
    let global_m = psxed_project::spatial::euler_degrees_to_matrix(global);
    let mut differs = false;
    for row in 0..3 {
        for col in 0..3 {
            differs |= (actual_m[row][col] - global_m[row][col]).abs() > 1e-3;
        }
    }
    assert!(differs, "local and global must diverge for a pitched prop");
}

#[test]
fn arch_prop_exposes_move_rotate_and_quantized_scale_gizmos() {
    let mut project = ProjectDocument::new("arch-prop-gizmos");
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Section {
            grid: WorldGrid::empty(4, 4, 1024),
        },
    );
    let arch = project.active_scene_mut().add_node(
        room,
        "Arch",
        NodeKind::ArchProp {
            materials: [None; psxed_project::ARCH_PROP_MATERIAL_COUNT],
            uvs: [GridUvTransform::IDENTITY; psxed_project::ARCH_PROP_MATERIAL_COUNT],
            geometry: psxed_project::ArchPropGeometry::default(),
            collision_enabled: false,
        },
    );
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("arch-prop-gizmos"), project);
    set_gizmo_test_camera(&mut workspace);
    workspace.replace_node_selection(arch);
    let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));

    workspace.transform_gizmo_mode = TransformGizmoMode::Move;
    assert_eq!(workspace.selected_node_gizmo_targets(), vec![arch]);
    assert_eq!(workspace.node_gizmo_screen_axes(viewport).len(), 3);

    workspace.transform_gizmo_mode = TransformGizmoMode::Rotate;
    assert_eq!(
        workspace.selected_node_rotation_axes(),
        vec![PrimitiveGizmoAxis::Y]
    );
    assert!(!workspace
        .node_rotation_gizmo_screen_rings(viewport)
        .is_empty());

    workspace.transform_gizmo_mode = TransformGizmoMode::Scale;
    let x_axis = projected_node_gizmo_axis(&workspace, viewport, PrimitiveGizmoAxis::X);
    let unit = (x_axis.end - x_axis.start).normalized();
    assert!(workspace.begin_node_gizmo_drag(PrimitiveGizmoAxis::X, viewport, x_axis.start));
    workspace.update_node_gizmo_drag(viewport, x_axis.start + unit * 8.0, false);
    workspace.end_node_gizmo_drag();

    let node = workspace.project.active_scene().node(arch).unwrap();
    let NodeKind::ArchProp { geometry, .. } = &node.kind else {
        panic!("expected arch prop");
    };
    assert_eq!(geometry.span_tiles, 3);
    assert_eq!(node.transform.scale, [1.0, 1.0, 1.0]);
    assert!(workspace.is_dirty());
}

#[test]
fn node_gizmo_scales_image_prop_width() {
    let mut project = ProjectDocument::new("image-prop-gizmo-scale");
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Section {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    let prop = project.active_scene_mut().add_node(
        room,
        "Banner",
        NodeKind::ImageProp {
            material: None,
            width: 1024,
            height: 1024,
            cylindrical_billboard: false,
            collision_enabled: false,
            collision_size: [1024, 1024, 1024],
        },
    );
    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("image-prop-gizmo-scale"), project);
    set_gizmo_test_camera(&mut workspace);
    workspace.replace_node_selection(prop);
    workspace.transform_gizmo_mode = TransformGizmoMode::Scale;

    let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
    let x_axis = projected_node_gizmo_axis(&workspace, viewport, PrimitiveGizmoAxis::X);
    let unit = (x_axis.end - x_axis.start).normalized();
    assert!(workspace.begin_node_gizmo_drag(PrimitiveGizmoAxis::X, viewport, x_axis.start));
    workspace.update_node_gizmo_drag(viewport, x_axis.start + unit * 8.0, false);
    workspace.end_node_gizmo_drag();

    let node = workspace.project.active_scene().node(prop).unwrap();
    let NodeKind::ImageProp { width, height, .. } = &node.kind else {
        panic!("expected image prop");
    };
    assert_eq!(*width, 1024 + HEIGHT_QUANTUM as u16);
    assert_eq!(*height, 1024);
    assert!(workspace.is_dirty());

    workspace.do_undo();
    let node = workspace.project.active_scene().node(prop).unwrap();
    let NodeKind::ImageProp { width, height, .. } = &node.kind else {
        panic!("expected image prop");
    };
    assert_eq!(*width, 1024);
    assert_eq!(*height, 1024);
}

#[test]
fn node_gizmo_scales_box_prop_width() {
    let mut project = ProjectDocument::new("box-prop-gizmo-scale");
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
    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("box-prop-gizmo-scale"), project);
    set_gizmo_test_camera(&mut workspace);
    workspace.replace_node_selection(prop);
    workspace.transform_gizmo_mode = TransformGizmoMode::Scale;

    let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
    let x_axis = projected_node_gizmo_axis(&workspace, viewport, PrimitiveGizmoAxis::X);
    let unit = (x_axis.end - x_axis.start).normalized();
    assert!(workspace.begin_node_gizmo_drag(PrimitiveGizmoAxis::X, viewport, x_axis.start));
    workspace.update_node_gizmo_drag(viewport, x_axis.start + unit * 8.0, false);
    workspace.end_node_gizmo_drag();

    let node = workspace.project.active_scene().node(prop).unwrap();
    let NodeKind::BoxProp { vertices, .. } = &node.kind else {
        panic!("expected box prop");
    };
    let min_x = vertices.iter().map(|v| v[0]).min().unwrap();
    let max_x = vertices.iter().map(|v| v[0]).max().unwrap();
    let min_y = vertices.iter().map(|v| v[1]).min().unwrap();
    let max_y = vertices.iter().map(|v| v[1]).max().unwrap();
    assert_eq!(min_x, -544);
    assert_eq!(max_x, 544);
    assert_eq!(min_y, 0);
    assert_eq!(max_y, 1024);
    assert!(workspace.is_dirty());

    workspace.do_undo();
    let node = workspace.project.active_scene().node(prop).unwrap();
    let NodeKind::BoxProp { vertices, .. } = &node.kind else {
        panic!("expected box prop");
    };
    assert_eq!(*vertices, psxed_project::box_prop_vertices_for_size(1024));
}

#[test]
fn box_prop_one_to_one_uv_span_tracks_face_size_and_native_texture() {
    let mut vertices = psxed_project::box_prop_vertices_for_size(1024);
    assert_eq!(
        box_prop_face_native_texel_span(vertices, 0, 1024, [64, 32]),
        [63, 31]
    );

    for vertex in &mut vertices {
        vertex[0] *= 2;
    }
    assert_eq!(
        box_prop_face_native_texel_span(vertices, 0, 1024, [64, 32]),
        [127, 31],
        "a two-sector face repeats a 64px texture twice without stretching it"
    );
}

#[test]
fn box_prop_face_resize_keeps_the_opposite_face_fixed() {
    let start_vertices = psxed_project::box_prop_vertices_for_size(1024);
    let mut project = ProjectDocument::new("anchored-box-resize");
    let node_id = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Anchored Crate",
        NodeKind::BoxProp {
            materials: [None; psxed_project::BOX_PROP_FACE_COUNT],
            uvs: [GridUvTransform::IDENTITY; psxed_project::BOX_PROP_FACE_COUNT],
            vertices: start_vertices,
            collision_enabled: true,
            break_flags: 0,
            erosion: psxed_project::BoxPropErosion::default(),
        },
    );
    let start_translation = [3.0, 0.0, 2.0];
    let node = project.active_scene_mut().node_mut(node_id).unwrap();
    node.transform.translation = start_translation;

    apply_box_prop_face_gizmo_resize(node, start_translation, Some(start_vertices), 1, 1, 1024);

    let NodeKind::BoxProp { vertices, .. } = &node.kind else {
        unreachable!();
    };
    assert_eq!(vertices.iter().map(|vertex| vertex[0]).min(), Some(-544));
    assert_eq!(vertices.iter().map(|vertex| vertex[0]).max(), Some(544));
    assert_eq!(node.transform.translation[0], 3.03125);
    let left_world = node.transform.translation[0] * 1024.0 - 544.0;
    let right_world = node.transform.translation[0] * 1024.0 + 544.0;
    assert_eq!(left_world, 2560.0, "the opposite (left) face stays fixed");
    assert_eq!(
        right_world, 3648.0,
        "the dragged face moves one 64-unit step"
    );
}

#[test]
fn duplicate_wall_cook_error_marks_both_authored_faces() {
    let mut project = ProjectDocument::new("duplicate-wall");
    let mut grid = WorldGrid::empty(4, 2, 1024);
    grid.add_wall(3, 1, GridDirection::South, 0, 1024, None);
    grid.add_wall(3, 0, GridDirection::North, 0, 1024, None);
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.view_2d = false;
    workspace.camera_rig.target = [99_000, 99_000, 99_000];

    workspace.record_world_cook_error(
        room,
        &WorldGridCookError::DuplicatePhysicalWall {
            x: 3,
            z: 1,
            direction: GridDirection::South,
            other_x: 3,
            other_z: 0,
            other_direction: GridDirection::North,
        },
        [0, 0],
    );

    let south = Selection::Face(FaceRef {
        room,
        sx: 3,
        sz: 1,
        kind: FaceKind::Wall {
            dir: GridDirection::South,
            stack: 0,
        },
    });
    let north = Selection::Face(FaceRef {
        room,
        sx: 3,
        sz: 0,
        kind: FaceKind::Wall {
            dir: GridDirection::North,
            stack: 0,
        },
    });
    assert!(workspace.validation_issue_primitives.contains(&south));
    assert!(workspace.validation_issue_primitives.contains(&north));
    assert!(workspace.validation_issue_rooms.is_empty());
    assert_eq!(workspace.selection.selected_primitive, Some(south));
    assert_eq!(workspace.selection.selected_primitives, vec![south, north]);
    let (center, _) = workspace
        .selected_frame_bounds_3d()
        .expect("duplicate wall faces frame in 3D");
    assert_eq!(
        workspace.camera_rig.target,
        [
            round_to_i32(center[0]),
            round_to_i32(center[1]),
            round_to_i32(center[2])
        ]
    );
}

#[test]
fn runtime_vram_budget_counts_compact_room_texture_and_model_atlas() {
    let mut project = ProjectDocument::new("vram-budget");
    let floor = project.add_resource(
        "Floor Texture",
        ResourceData::Material(psxed_project::MaterialResource::opaque(Some(
            "assets/textures/delven_01_slateflr1a_q2.psxt".to_string(),
        ))),
    );
    let model = project.add_resource(
        "Obsidian Wraith",
        ResourceData::Model(psxed_project::ModelResource {
            model_path: "assets/models/obsidian_wraith/obsidian_wraith.psxmdl".to_string(),
            source_path: None,
            texture_path: Some(
                "assets/models/obsidian_wraith/obsidian_wraith_128x128_8bpp.psxt".to_string(),
            ),
            skeleton: None,
            world_height: 1024,
            collision_radius: default_model_collision_radius_for_height(1024),
            scale_q8: [MODEL_SCALE_ONE_Q8; 3],
            default_visual_yaw_q12: 0,
            attachments: Vec::new(),
        }),
    );
    let resource_use = SceneResourceUse {
        textures: vec![floor],
        models: vec![model],
        ..SceneResourceUse::default()
    };

    let budget = runtime_vram_budget(
        &project,
        &psxed_project::legacy_grid_starter_dir(),
        &resource_use,
    );

    assert_eq!(budget.textures, 2);
    assert_eq!(budget.room_textures, 1);
    assert_eq!(budget.model_textures, 1);
    assert_eq!(budget.missing, 0);
    assert_eq!(budget.room_bytes, 8 * 32 * 2 + 16 * 2);
    assert_eq!(budget.model_bytes, 64 * 128 * 2 + 256 * 2);
    assert_eq!(budget.bytes, 8 * 32 * 2 + 16 * 2 + 64 * 128 * 2 + 256 * 2);
}

#[test]
fn material_click_assignment_updates_all_faces_in_selected_sectors() {
    let mut project = ProjectDocument::new("materials");
    let original = project.add_resource(
        "Original",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let target = project.add_resource(
        "Target",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let mut grid = WorldGrid::empty(2, 1, 1024);
    for sx in 0..=1 {
        grid.set_floor(sx, 0, 0, Some(original));
        grid.ensure_sector(sx, 0).unwrap().ceiling =
            Some(GridHorizontalFace::flat(1024, Some(original)));
        grid.add_wall(sx, 0, GridDirection::North, 0, 2048, Some(original));
        grid.sector_mut(sx, 0)
            .unwrap()
            .walls
            .get_mut(GridDirection::North)[0]
            .uv = GridUvTransform {
            offset: [9, 11],
            span: [22, 33],
            rotation: GridUvRotation::Deg90,
            flip_u: true,
            flip_v: false,
        };
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

    let selected = workspace.selected_face_targets();
    assert_eq!(selected.len(), 6);
    assert_eq!(workspace.assign_selected_faces_material(Some(target)), 6);

    for sx in 0..=1 {
        assert_eq!(
            workspace.face_material(FaceRef {
                room,
                sx,
                sz: 0,
                kind: FaceKind::Floor,
            }),
            Some(target)
        );
        let wall = &workspace
            .room_grid_view(room)
            .unwrap()
            .sector(sx, 0)
            .unwrap()
            .walls
            .get(GridDirection::North)[0];
        assert_eq!(
            wall.uv.span,
            [0, 128],
            "assigning a material autotiles every selected wall"
        );
        assert_eq!(wall.uv.offset, [9, 11]);
        assert_eq!(wall.uv.rotation, GridUvRotation::Deg90);
        assert!(wall.uv.flip_u);
        assert!(!wall.uv.flip_v);
        assert_eq!(
            workspace.face_material(FaceRef {
                room,
                sx,
                sz: 0,
                kind: FaceKind::Ceiling,
            }),
            Some(target)
        );
        assert_eq!(
            workspace.face_material(FaceRef {
                room,
                sx,
                sz: 0,
                kind: FaceKind::Wall {
                    dir: GridDirection::North,
                    stack: 0,
                },
            }),
            Some(target)
        );
    }
    assert!(workspace.is_dirty());
}

#[test]
fn face_uv_rotation_and_flip_apply_to_every_selected_face() {
    let mut project = ProjectDocument::new("multi-face-uv");
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

    let floor = FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Floor,
    };
    let ceiling = FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Ceiling,
    };
    let wall = FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Wall {
            dir: GridDirection::North,
            stack: 0,
        },
    };
    {
        let grid = workspace.room_floor_grid_mut(room).unwrap();
        let sector = grid.sector_mut(0, 0).unwrap();
        sector.floor.as_mut().unwrap().uv = GridUvTransform {
            offset: [11, 12],
            span: [21, 22],
            rotation: GridUvRotation::Deg45,
            flip_u: false,
            flip_v: true,
        };
        sector.ceiling.as_mut().unwrap().uv = GridUvTransform {
            offset: [31, 32],
            span: [41, 42],
            rotation: GridUvRotation::Deg90,
            flip_u: false,
            flip_v: false,
        };
        sector.walls.get_mut(GridDirection::North)[0].uv = GridUvTransform {
            offset: [51, 52],
            span: [61, 62],
            rotation: GridUvRotation::Deg135,
            flip_u: false,
            flip_v: true,
        };
        grid.sector_mut(1, 0).unwrap().floor.as_mut().unwrap().uv = GridUvTransform {
            offset: [71, 72],
            span: [81, 82],
            rotation: GridUvRotation::Deg180,
            flip_u: false,
            flip_v: false,
        };
    }
    workspace.selection.selected_primitives = vec![
        Selection::Face(floor),
        Selection::Face(ceiling),
        Selection::Face(wall),
    ];
    workspace.selection.selected_primitive = Some(Selection::Face(wall));

    let before = workspace
        .room_grid_view(room)
        .unwrap()
        .sector(0, 0)
        .unwrap()
        .walls
        .get(GridDirection::North)[0]
        .uv;
    let mut after = before;
    after.flip_u = true;
    workspace
        .room_floor_grid_mut(room)
        .unwrap()
        .sector_mut(0, 0)
        .unwrap()
        .walls
        .get_mut(GridDirection::North)[0]
        .uv = after;

    assert_eq!(
        workspace.apply_selected_face_uv_change_no_undo(
            wall,
            GridUvTransformEdit {
                rotation: true,
                flip_u: true,
                ..Default::default()
            },
            after,
        ),
        (3, 2)
    );

    let grid = workspace.room_grid_view(room).unwrap();
    let sector = grid.sector(0, 0).unwrap();
    let floor_uv = sector.floor.as_ref().unwrap().uv;
    let ceiling_uv = sector.ceiling.as_ref().unwrap().uv;
    let wall_uv = sector.walls.get(GridDirection::North)[0].uv;
    let unselected_uv = grid.sector(1, 0).unwrap().floor.as_ref().unwrap().uv;

    assert_eq!(floor_uv.rotation, GridUvRotation::Deg135);
    assert!(floor_uv.flip_u);
    assert_eq!(floor_uv.offset, [11, 12]);
    assert_eq!(floor_uv.span, [21, 22]);
    assert!(floor_uv.flip_v);
    assert_eq!(ceiling_uv.rotation, GridUvRotation::Deg135);
    assert!(ceiling_uv.flip_u);
    assert_eq!(ceiling_uv.offset, [31, 32]);
    assert_eq!(ceiling_uv.span, [41, 42]);
    assert!(!ceiling_uv.flip_v);
    assert_eq!(wall_uv, after);
    assert_eq!(unselected_uv.rotation, GridUvRotation::Deg180);
    assert!(!unselected_uv.flip_u);
}

#[test]
fn face_uv_offset_span_and_flip_v_apply_without_replacing_untouched_fields() {
    let mut project = ProjectDocument::new("multi-face-uv-fields");
    let mut grid = WorldGrid::empty(2, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    grid.set_floor(1, 0, 0, None);
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    let first = FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Floor,
    };
    let active = FaceRef {
        room,
        sx: 1,
        sz: 0,
        kind: FaceKind::Floor,
    };
    let first_uv = GridUvTransform {
        offset: [3, 4],
        span: [5, 6],
        rotation: GridUvRotation::Deg225,
        flip_u: true,
        flip_v: false,
    };
    let before = GridUvTransform {
        offset: [10, 20],
        span: [30, 40],
        rotation: GridUvRotation::Deg45,
        flip_u: false,
        flip_v: false,
    };
    let mut after = before;
    after.offset[0] = 90;
    after.span[1] = 120;
    after.flip_v = true;
    {
        let grid = workspace.room_floor_grid_mut(room).unwrap();
        grid.sector_mut(0, 0).unwrap().floor.as_mut().unwrap().uv = first_uv;
        grid.sector_mut(1, 0).unwrap().floor.as_mut().unwrap().uv = after;
    }
    workspace.selection.selected_primitives = vec![Selection::Face(first), Selection::Face(active)];
    workspace.selection.selected_primitive = Some(Selection::Face(active));

    assert_eq!(
        workspace.apply_selected_face_uv_change_no_undo(
            active,
            GridUvTransformEdit {
                offset: [true, false],
                span: [false, true],
                flip_v: true,
                ..Default::default()
            },
            after,
        ),
        (2, 1)
    );

    let first_after = workspace
        .room_grid_view(room)
        .unwrap()
        .sector(0, 0)
        .unwrap()
        .floor
        .as_ref()
        .unwrap()
        .uv;
    assert_eq!(first_after.offset, [90, 4]);
    assert_eq!(first_after.span, [5, 120]);
    assert_eq!(first_after.rotation, GridUvRotation::Deg225);
    assert!(first_after.flip_u);
    assert!(first_after.flip_v);
}

#[test]
fn material_click_assignment_updates_selected_box_prop_faces() {
    let mut project = ProjectDocument::new("box-prop-materials");
    let target = project.add_resource(
        "Target",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
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
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.replace_node_selection(prop);

    let assignment = workspace
        .assign_selected_box_props_resource(target)
        .expect("material applies to selected box prop");
    assert_eq!(assignment.updated, 1);
    assert_eq!(assignment.targets, 1);

    let node = workspace.project.active_scene().node(prop).unwrap();
    let NodeKind::BoxProp { materials, .. } = &node.kind else {
        panic!("expected box prop");
    };
    assert!(materials.iter().all(|material| *material == Some(target)));
    assert!(workspace.is_dirty());
}

#[test]
fn material_click_assignment_applies_to_selected_box_prop() {
    let mut project = ProjectDocument::new("box-prop-texture");
    let material_id = project.add_resource(
        "Brick",
        ResourceData::Material(psxed_project::MaterialResource::opaque(Some(
            "assets/textures/brick.psxt".to_string(),
        ))),
    );
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
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.replace_node_selection(prop);

    let assignment = workspace
        .assign_selected_box_props_resource(material_id)
        .expect("material applies to selected box prop");
    assert_eq!(assignment.updated, 1);
    assert_eq!(assignment.material, material_id);

    let node = workspace.project.active_scene().node(prop).unwrap();
    let NodeKind::BoxProp { materials, .. } = &node.kind else {
        panic!("expected box prop");
    };
    assert!(materials
        .iter()
        .all(|material| *material == Some(assignment.material)));
    assert!(workspace.is_dirty());
}

#[test]
fn box_prop_resource_click_keeps_node_selection_active() {
    let mut project = ProjectDocument::new("box-prop-click-selection");
    let target = project.add_resource(
        "Target",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
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
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.replace_node_selection(prop);
    workspace.replace_resource_selection(target);

    assert!(
        workspace.apply_selected_box_prop_resource_click(ResourceClick {
            id: target,
            modifiers: egui::Modifiers::NONE,
        })
    );

    assert_eq!(workspace.selection.selected_node, prop);
    assert!(workspace.selection.selected_nodes.contains(&prop));
    assert_eq!(workspace.selection.selected_resource, None);
    assert!(workspace.selection.selected_resources.is_empty());
}

#[test]
fn selected_material_resource_paints_new_floor_ceiling_and_wall() {
    let mut project = ProjectDocument::new("paint-selected-material");
    project.add_resource(
        "Other",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let selected = project.add_resource(
        "Selected",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Section {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.replace_resource_selection(selected);

    workspace.run_paint_action(ViewTool::PaintFloor, room, 0, 0, None, [512.0, 0.0, 512.0]);
    workspace.run_paint_action(
        ViewTool::PaintCeiling,
        room,
        0,
        0,
        None,
        [512.0, 1024.0, 512.0],
    );
    workspace.run_paint_action(
        ViewTool::PaintWall,
        room,
        0,
        0,
        Some(FaceRef {
            room,
            sx: 0,
            sz: 0,
            kind: FaceKind::Wall {
                dir: GridDirection::North,
                stack: 0,
            },
        }),
        [512.0, 0.0, 1024.0],
    );

    let grid = workspace.room_grid_view(room).unwrap();
    let sector = grid.sector(0, 0).unwrap();
    assert_eq!(sector.floor.as_ref().unwrap().material, Some(selected));
    assert_eq!(sector.ceiling.as_ref().unwrap().material, Some(selected));
    assert_eq!(
        sector.walls.get(GridDirection::North)[0].material,
        Some(selected)
    );
    assert_eq!(
        sector.walls.get(GridDirection::North)[0].uv.span,
        [0, 128],
        "new textured walls autotile without a second Inspector action"
    );
}

#[test]
fn place_image_prop_with_selected_material_creates_node() {
    let mut project = ProjectDocument::new("image-prop-material-place");
    let material = project.add_resource(
        "Banner",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Section {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.place_kind = PlaceKind::ImageProp;
    workspace.replace_resource_selection(material);

    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [512.0, 384.0, 512.0]);

    let node = workspace
        .project
        .active_scene()
        .node(workspace.selected_node_id())
        .expect("placed image prop is selected");
    assert_eq!(node.name, "Banner Image");
    assert_eq!(workspace.active_tool, ViewTool::Select);
    assert_eq!(node.transform.translation[1], 384.0 / 1024.0);
    let NodeKind::ImageProp {
        material: Some(actual),
        width,
        height,
        cylindrical_billboard,
        ..
    } = &node.kind
    else {
        panic!("expected image prop node");
    };
    assert_eq!(*actual, material);
    assert_eq!(*width, psxed_project::DEFAULT_IMAGE_PROP_SIZE);
    assert_eq!(*height, psxed_project::DEFAULT_IMAGE_PROP_SIZE);
    assert!(!*cylindrical_billboard);
    assert_eq!(workspace.status, "Placed Image Prop at 0,0");
    assert!(workspace.is_dirty());
}

#[test]
fn water_tool_paints_one_selected_volume_and_erases_by_cell() {
    let mut project = ProjectDocument::new("water-paint");
    let material = project.add_resource(
        "Water",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let mut grid = WorldGrid::empty(2, 1, 1024);
    grid.set_floor(0, 0, -256, None);
    grid.set_floor(1, 0, 0, None);
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.replace_resource_selection(material);

    workspace.run_paint_action(ViewTool::Water, room, 0, 0, None, [512.0, -256.0, 512.0]);
    let volume = workspace.selected_node_id();
    assert_eq!(
        workspace.selection.selected_resource, None,
        "the painted volume, not its brush material, must own the Inspector"
    );
    let node = workspace
        .project
        .active_scene()
        .node(volume)
        .expect("water node");
    let NodeKind::WaterVolume {
        material: actual,
        cells,
        settings,
    } = &node.kind
    else {
        panic!("water node expected");
    };
    assert_eq!(*actual, Some(material));
    assert_eq!(cells, &[WaterVolumeCell::new(0, 0)]);
    assert_eq!(
        settings.height_above_floor, 256,
        "adjacent rim becomes height above the clicked floor"
    );

    workspace.run_paint_action(ViewTool::Water, room, 1, 0, None, [1536.0, 0.0, 512.0]);
    let NodeKind::WaterVolume { cells, .. } =
        &workspace.project.active_scene().node(volume).unwrap().kind
    else {
        unreachable!()
    };
    assert_eq!(
        cells.len(),
        2,
        "selected volume grows instead of duplicating"
    );

    workspace.water_tool_mode = WaterToolMode::Select;
    workspace.replace_resource_selection(material);
    workspace.replace_node_selection(room);
    workspace.run_paint_action(ViewTool::Water, room, 0, 0, None, [512.0, 0.0, 512.0]);
    assert_eq!(workspace.selected_node_id(), volume);
    assert_eq!(
        workspace.selection.selected_resource, None,
        "selecting water must reveal the WaterVolume node Inspector"
    );
    assert!(workspace.selection.selected_resources.is_empty());
    assert_eq!(workspace.status, "Selected Water Volume at 0,0");
    let NodeKind::WaterVolume { cells, .. } =
        &workspace.project.active_scene().node(volume).unwrap().kind
    else {
        unreachable!()
    };
    assert_eq!(cells.len(), 2, "selection must not mutate the footprint");

    workspace.water_tool_mode = WaterToolMode::Erase;
    workspace.run_paint_action(ViewTool::Water, room, 0, 0, None, [512.0, 0.0, 512.0]);
    let NodeKind::WaterVolume { cells, .. } =
        &workspace.project.active_scene().node(volume).unwrap().kind
    else {
        unreachable!()
    };
    assert_eq!(cells, &[WaterVolumeCell::new(1, 0)]);

    // The real keyboard path must treat the selected water footprint as its
    // owning scene node. Delete removes the complete WaterVolume, not merely
    // the cell under the cursor or the brush material resource.
    workspace.water_tool_mode = WaterToolMode::Select;
    workspace.active_tool = ViewTool::Water;
    workspace.run_paint_action(ViewTool::Water, room, 1, 0, None, [1536.0, 0.0, 512.0]);
    assert_eq!(workspace.selected_node_id(), volume);
    let ctx = egui::Context::default();
    let _ = ctx.run(
        egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Delete,
                physical_key: Some(egui::Key::Delete),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..egui::RawInput::default()
        },
        |ctx| workspace.handle_global_shortcuts(ctx, EditorPlaytestStatus::Idle),
    );
    assert!(workspace.project.active_scene().node(volume).is_none());
    assert_eq!(workspace.status, "Deleted node");
}

#[test]
fn water_tool_derives_initial_height_from_slope_low_points() {
    let mut project = ProjectDocument::new("water-slope-anchor");
    let mut grid = WorldGrid::empty(2, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    grid.set_floor(1, 0, 0, None);
    grid.sector_mut(0, 0)
        .and_then(|sector| sector.floor.as_mut())
        .expect("sloped floor")
        .heights = [-256, -128, 0, 128];
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);

    workspace.run_paint_action(ViewTool::Water, room, 0, 0, None, [512.0, 0.0, 512.0]);

    let NodeKind::WaterVolume { settings, .. } = &workspace
        .project
        .active_scene()
        .node(workspace.selected_node_id())
        .expect("painted water volume")
        .kind
    else {
        panic!("water volume expected");
    };
    assert_eq!(
        settings.height_above_floor, 256,
        "the neighbouring zero-height rim is measured from the slope's -256 low point"
    );
}

/// Four clockwise quarter turns must be the identity. Rotating a sector means
/// rotating the `[NW, NE, SE, SW]` height array, the diagonal split, the wall
/// direction and the per-wall corner references, all consistently -- an
/// asymmetric piece is the only kind that notices when one of those leaves
/// disagrees with the others.
#[test]
fn four_quarter_turns_return_an_asymmetric_sector_unchanged() {
    let mut sector = GridSector::empty();
    let mut floor = GridHorizontalFace::flat(0, None);
    floor.heights = [0, 32, 64, 96];
    floor.split = GridSplit::NorthEastSouthWest;
    floor.dropped_corner = Some(Corner::NE);
    floor.triangle_override_mut(0).heights = Some([0, 32, 96]);
    sector.floor = Some(floor);
    let mut ceiling = GridHorizontalFace::flat(1024, None);
    ceiling.heights = [1024, 960, 1024, 896];
    sector.ceiling = Some(ceiling);
    let mut wall = GridVerticalFace::flat(0, 512, None);
    wall.heights = [0, 64, 512, 448];
    wall.dropped_corner = Some(WallCorner::BL);
    sector.walls.west.push(wall);

    let mut rotated = sector.clone();
    for turn in 1..=4 {
        rotated = rotate_sector_cw(&rotated);
        if turn < 4 {
            assert_ne!(rotated, sector, "turn {turn} should not be the identity");
        }
    }
    assert_eq!(rotated, sector, "four quarter turns must round trip");
}

fn save_single_cell_prefab(workspace: &mut EditorWorkspace, room: NodeId, label: &str) -> PathBuf {
    workspace.select_sector((room, 0, 0), egui::Modifiers::NONE);
    let path = test_temp_dir(label).join("drag_drop_prefab.ron");
    workspace
        .capture_selection_as_prefab("Drag Drop Prefab")
        .expect("selection captures")
        .save_to_path(&path)
        .expect("prefab saves");
    path
}

#[test]
fn dropping_a_prefab_in_2d_starts_preview_at_the_pointer_cell() {
    let (mut workspace, room) = workspace_with_populated_grid("prefab-drop-2d", 4, 4);
    let path = save_single_cell_prefab(&mut workspace, room, "prefab-drop-2d");
    let editor_world = workspace
        .room_grid_view(room)
        .unwrap()
        .world_cells_to_editor([2.25, 1.25]);

    workspace.drop_prefab_2d(&path, editor_world);

    let preview = workspace
        .floating_geometry
        .as_ref()
        .expect("drop enters the floating preview loop");
    assert_eq!(preview.room, room);
    assert_eq!(preview.origin, [2, 1]);

    let _ = std::fs::remove_file(path);
}

#[test]
fn dropping_a_prefab_in_3d_starts_preview_at_the_hit_face_cell() {
    let (mut workspace, room) = workspace_with_populated_grid("prefab-drop-3d", 4, 4);
    let path = save_single_cell_prefab(&mut workspace, room, "prefab-drop-3d");
    let grid_origin = workspace.room_grid_view(room).unwrap().origin;
    let face = FaceRef {
        room,
        sx: 3,
        sz: 2,
        kind: FaceKind::Floor,
    };

    workspace.drop_prefab_3d(&path, Some((face, [0.0; 3])), None);

    let preview = workspace
        .floating_geometry
        .as_ref()
        .expect("drop enters the floating preview loop");
    assert_eq!(preview.room, room);
    assert_eq!(preview.origin, [grid_origin[0] + 3, grid_origin[1] + 2]);

    let _ = std::fs::remove_file(path);
}

#[test]
fn stock_prefab_stamps_ceilings_with_front_facing_destination_materials() {
    let path = test_temp_dir("roofed-prefab-stamp").join("connector_straight.ron");
    std::fs::create_dir_all(path.parent().expect("fixture has a parent"))
        .expect("fixture directory creates");
    std::fs::write(
        &path,
        psxed_project::prefab_kit_body("connector_straight")
            .expect("connector is embedded in the stock kit"),
    )
    .expect("embedded connector fixture writes");

    let mut project = ProjectDocument::new("roofed-prefab-destination");
    project.add_resource(
        "COBBLES_1A Material",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    project.add_resource(
        "BLOCK_1A Material",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let ceiling_material = project.add_resource(
        "BRICK_1A Material",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Section {
            grid: WorldGrid::empty(1, 1, 1792),
        },
    );
    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("roofed-prefab-destination"), project);
    workspace.replace_node_selection(room);

    workspace.stamp_prefab(&path);
    assert!(workspace.commit_floating_geometry());

    let grid = workspace.room_grid_view(room).expect("destination room");
    for z in 0..3 {
        let ceiling = grid
            .sector(0, z)
            .and_then(|sector| sector.ceiling.as_ref())
            .unwrap_or_else(|| panic!("stock connector cell {z} has a ceiling"));
        assert_eq!(ceiling.material, Some(ceiling_material));
    }
    let ResourceData::Material(material) = &workspace
        .project
        .resource(ceiling_material)
        .expect("destination ceiling material")
        .data
    else {
        panic!("ceiling resource changed kind");
    };
    assert_eq!(material.sidedness(), MaterialFaceSidedness::Front);

    let _ = std::fs::remove_file(path);
}

/// The whole point of a prefab: geometry authored in one project lands in
/// another, on that project's own materials. Binding by raw `ResourceId` would
/// pass this test's shape while painting the wrong texture, so the destination
/// deliberately hands the source's id to a different material.
#[test]
fn a_saved_prefab_stamps_into_another_project_on_its_own_materials() {
    let mut source = ProjectDocument::new("prefab-stamp-source");
    let stone = source.add_resource(
        "Stone",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.set_floor(0, 0, 0, Some(stone));
    grid.sector_mut(0, 0)
        .unwrap()
        .floor
        .as_mut()
        .unwrap()
        .heights = [0, 32, 64, 96];
    let source_room =
        source
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("prefab-stamp-source"), source);
    workspace.select_sector((source_room, 0, 0), egui::Modifiers::NONE);

    let prefab = workspace
        .capture_selection_as_prefab("Stair Block")
        .expect("selection captures");
    let path = test_temp_dir("prefab-stamp").join("stair_block.ron");
    prefab.save_to_path(&path).expect("prefab saves");

    // Destination: "Dirt" occupies the id the source used for "Stone".
    let mut destination = ProjectDocument::new("prefab-stamp-destination");
    let dirt = destination.add_resource(
        "Dirt",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let stone_here = destination.add_resource(
        "Stone",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    assert_eq!(dirt, stone, "the destination reuses the source's Stone id");
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.set_floor(0, 0, 0, Some(stone_here));
    let room =
        destination
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("prefab-stamp-destination"), destination);
    workspace.replace_node_selection(room);

    workspace.stamp_prefab(&path);
    assert!(
        workspace.floating_geometry.is_some(),
        "stamping enters the same preview loop as Duplicate: {}",
        workspace.status
    );
    assert!(workspace.update_floating_geometry_origin([0, 1]));
    assert!(workspace.commit_floating_geometry());

    let grid = workspace.room_grid_view(room).unwrap();
    let stamped = grid.sector(0, 1).expect("stamped cell exists");
    let floor = stamped.floor.as_ref().expect("stamped floor exists");
    assert_eq!(floor.heights, [0, 32, 64, 96]);
    assert_eq!(
        floor.material,
        Some(stone_here),
        "rebound by name, not by id"
    );

    let _ = std::fs::remove_file(&path);
}

/// Tiling pieces edge to edge is the point of prefabs, and it is also the one
/// thing that breaks the cook: `East(0, 0)` and `West(1, 0)` are one physical
/// face, and a grid claiming both is rejected outright. The stamp drops the
/// incoming side. Without that, this cooks to `DuplicatePhysicalWall` and the
/// level does not build at all.
#[test]
fn stamping_a_walled_piece_against_its_neighbour_drops_the_shared_edge() {
    let mut project = ProjectDocument::new("prefab-seam");
    let stone = project.add_resource(
        "Stone",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let mut grid = WorldGrid::empty(2, 1, 1024);
    grid.set_floor(0, 0, 0, Some(stone));
    for direction in GridDirection::CARDINAL {
        grid.add_wall(0, 0, direction, 0, 1024, Some(stone));
    }
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("prefab-seam"), project);
    workspace.select_sector((room, 0, 0), egui::Modifiers::NONE);

    let path = test_temp_dir("prefab-seam").join("corridor.ron");
    workspace
        .capture_selection_as_prefab("Corridor")
        .expect("selection captures")
        .save_to_path(&path)
        .expect("prefab saves");

    workspace.stamp_prefab(&path);
    assert!(workspace.update_floating_geometry_origin([1, 0]));
    assert!(workspace.commit_floating_geometry());

    let grid = workspace.room_grid_view(room).unwrap();
    psxed_project::world_cook::cook_world_grid(&workspace.project, grid).expect("the grid cooks");
    assert!(
        workspace.status.contains("1 wall dropped"),
        "the drop is reported, not silent: {}",
        workspace.status
    );
    assert!(
        !workspace.status.contains("Portal"),
        "a stamp well inside the caps must not cry portal: {}",
        workspace.status
    );

    let stamped = grid.sector(1, 0).expect("stamped cell exists");
    assert!(
        stamped.walls.get(GridDirection::West).is_empty(),
        "the incoming side of the shared edge loses"
    );
    assert!(
        !stamped.walls.get(GridDirection::East).is_empty(),
        "the piece keeps every wall that collides with nothing"
    );
    assert!(
        !grid
            .sector(0, 0)
            .unwrap()
            .walls
            .get(GridDirection::East)
            .is_empty(),
        "the destination keeps the edge it already owned"
    );

    let _ = std::fs::remove_file(&path);
}

/// The motivating case for the floor axis: a stair block spanning two floors.
/// The selection is made on one floor, so capture takes that footprint up the
/// whole stack, and the stamp has to rebuild the stack in the destination.
/// The floor link is the part that silently rots: it carries a `NodeId`, and a
/// copied one addresses a room that need not exist where the piece lands.
#[test]
fn a_two_floor_piece_captures_and_stamps_its_stack_and_relinks() {
    let mut project = ProjectDocument::new("prefab-stack");
    let stone = project.add_resource(
        "Stone",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let mut grid = WorldGrid::empty(2, 1, 1024);
    grid.set_floor(0, 0, 0, Some(stone));
    grid.push_floor();
    let upper = grid.floor_mut(1).expect("floor 1 exists");
    upper.set_floor(0, 0, 0, Some(stone));
    let upper_elevation = upper.elevation;
    assert_ne!(upper_elevation, 0, "push_floor stacks above the base");

    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    // Link the two floors of the piece together, the way a stair would.
    if let Some(NodeKind::Section { grid }) = project
        .active_scene_mut()
        .node_mut(room)
        .map(|node| &mut node.kind)
    {
        grid.sector_mut(0, 0).unwrap().floor_above = Some(psxed_project::GridFloorLink {
            target_room: Some(room),
            target_floor: 1,
        });
    }

    let mut workspace = EditorWorkspace::with_project(test_temp_dir("prefab-stack"), project);
    workspace.select_sector((room, 0, 0), egui::Modifiers::NONE);
    let prefab = workspace
        .capture_selection_as_prefab("Stair Block")
        .expect("selection captures");
    assert_eq!(
        prefab.floors.len(),
        2,
        "the stack above the selection came too"
    );
    assert_eq!(prefab.floors[1].relative_elevation, upper_elevation);

    let path = test_temp_dir("prefab-stack").join("stair.ron");
    prefab.save_to_path(&path).expect("prefab saves");

    // Stamp into a fresh single-floor room: the upper floor must be created.
    let mut destination = ProjectDocument::new("prefab-stack-dest");
    let stone_here = destination.add_resource(
        "Stone",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let dest_room = destination.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Section {
            grid: WorldGrid::empty(2, 1, 1024),
        },
    );
    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("prefab-stack-dest"), destination);
    workspace.replace_node_selection(dest_room);
    workspace.stamp_prefab(&path);
    assert!(
        workspace.floating_geometry.is_some(),
        "stamp entered preview: {}",
        workspace.status
    );
    assert!(workspace.update_floating_geometry_origin([1, 0]));
    assert!(workspace.commit_floating_geometry());

    let Some(NodeKind::Section { grid }) = workspace
        .project()
        .active_scene()
        .node(dest_room)
        .map(|node| &node.kind)
    else {
        panic!("destination room survived");
    };
    assert_eq!(grid.floor_count(), 2, "the stamp grew the floor stack");
    let created = grid.floor(1).expect("floor 1 was created");
    assert_eq!(
        created.elevation, upper_elevation,
        "the created floor takes the piece's own spacing"
    );
    let upper_cell = created
        .world_cell_to_array(1, 0)
        .and_then(|(sx, sz)| created.sector(sx, sz))
        .expect("upper floor geometry landed");
    assert_eq!(
        upper_cell.floor.as_ref().unwrap().material,
        Some(stone_here),
        "rebound onto the destination's own material"
    );

    let base_cell = grid
        .world_cell_to_array(1, 0)
        .and_then(|(sx, sz)| grid.sector(sx, sz))
        .expect("base floor geometry landed");
    let link = base_cell.floor_above.expect("the stair link survived");
    assert_eq!(
        link.target_room,
        Some(dest_room),
        "the link points at the room it landed in, not the one it was authored in"
    );
    assert_eq!(link.target_floor, 1);

    let _ = std::fs::remove_file(&path);
}

/// Heights are absolute world units, so a piece captured at ground level and
/// stamped onto a terrace arrives buried. The nudge has to move the whole
/// sector -- floor, ceiling and every wall together -- because raising only
/// the floor would flatten the piece against its own walls. It also has to
/// land on `HEIGHT_QUANTUM`, since the cooker rejects any height that does not.
#[test]
fn raising_a_placement_moves_floor_ceiling_and_walls_as_one() {
    let mut project = ProjectDocument::new("prefab-lift");
    let stone = project.add_resource(
        "Stone",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let mut grid = WorldGrid::empty(1, 2, 1024);
    grid.set_floor(0, 0, 0, Some(stone));
    grid.sector_mut(0, 0).unwrap().ceiling = Some(GridHorizontalFace::flat(1024, Some(stone)));
    grid.add_wall(0, 0, GridDirection::West, 0, 1024, Some(stone));
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("prefab-lift"), project);
    workspace.select_sector((room, 0, 0), egui::Modifiers::NONE);

    let path = test_temp_dir("prefab-lift").join("pad.ron");
    workspace
        .capture_selection_as_prefab("Pad")
        .expect("selection captures")
        .save_to_path(&path)
        .expect("prefab saves");

    workspace.stamp_prefab(&path);
    assert!(workspace.update_floating_geometry_origin([0, 1]));
    workspace.nudge_floating_geometry_elevation(3);
    assert!(workspace.commit_floating_geometry());

    let lift = 3 * HEIGHT_QUANTUM;
    let grid = workspace.room_grid_view(room).unwrap();
    let raised = grid.sector(0, 1).expect("stamped cell exists");
    assert_eq!(raised.floor.as_ref().unwrap().heights, [lift; 4]);
    assert_eq!(
        raised.ceiling.as_ref().unwrap().heights,
        [1024 + lift; 4],
        "the ceiling rides up with the floor, keeping the room's headroom"
    );
    assert_eq!(
        raised.walls.get(GridDirection::West)[0].heights,
        [lift, lift, 1024 + lift, 1024 + lift],
        "the wall spans the same gap at the new elevation"
    );
    assert!(
        lift % HEIGHT_QUANTUM == 0,
        "the cooker rejects heights off the quantum"
    );
    // The source is untouched, so the nudge is placement state, not an edit.
    assert_eq!(
        grid.sector(0, 0).unwrap().floor.as_ref().unwrap().heights,
        [0; 4]
    );

    let _ = std::fs::remove_file(&path);
}

/// A runtime room is capped at `MAX_ROOM_WIDTH` sectors and the portal planner
/// will not invent a seam to stay under it -- only an authored `Portal` splits
/// a grid. Stamping grows the grid with no cap of its own, so the placement
/// that busts it has to say so; otherwise the first news of it is a failed
/// build after an afternoon of stamping.
#[test]
fn a_stamp_past_the_runtime_room_cap_says_a_portal_is_needed() {
    let mut project = ProjectDocument::new("prefab-cap");
    let stone = project.add_resource(
        "Stone",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let mut grid = WorldGrid::empty(psxed_project::MAX_ROOM_WIDTH, 1, 1024);
    for x in 0..psxed_project::MAX_ROOM_WIDTH {
        grid.set_floor(x, 0, 0, Some(stone));
    }
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("prefab-cap"), project);

    // Exactly at the cap, so the room is still legal before the stamp.
    workspace.select_sector((room, 0, 0), egui::Modifiers::NONE);
    let path = test_temp_dir("prefab-cap").join("pad.ron");
    workspace
        .capture_selection_as_prefab("Pad")
        .expect("selection captures")
        .save_to_path(&path)
        .expect("prefab saves");

    workspace.stamp_prefab(&path);
    assert!(workspace.update_floating_geometry_origin([psxed_project::MAX_ROOM_WIDTH as i32, 0]));
    assert!(workspace.commit_floating_geometry());
    assert!(
        workspace.status.contains("Portal"),
        "one cell past the cap has to point at the fix: {}",
        workspace.status
    );

    let _ = std::fs::remove_file(&path);
}

/// A sealed room with no light cooks to a black box, which reads as broken
/// geometry rather than as an unlit room, so every prefab carries one. The
/// light has to ride the placement: same origin, same rotation, same lift.
#[test]
fn a_stamped_prefab_brings_its_light_along() {
    let mut project = ProjectDocument::new("prefab-light");
    let stone = project.add_resource(
        "Stone",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let mut grid = WorldGrid::empty(4, 4, 1024);
    for x in 0..2 {
        for z in 0..2 {
            grid.set_floor(x, z, 0, Some(stone));
        }
    }
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("prefab-light"), project);
    for (i, (x, z)) in [(0u16, 0u16), (1, 0), (0, 1), (1, 1)]
        .into_iter()
        .enumerate()
    {
        let modifiers = if i == 0 {
            egui::Modifiers::NONE
        } else {
            egui::Modifiers::COMMAND
        };
        workspace.select_sector((room, x, z), modifiers);
    }
    let mut prefab = workspace
        .capture_selection_as_prefab("Lit Block")
        .expect("selection captures");
    // A corner light, so rotation is observable.
    prefab.lights.push(psxed_project::PrefabLight {
        cell: [0, 0],
        height_sectors: 1.0,
        color: [255, 255, 255],
        intensity: 1.0,
        radius: 3.0,
    });
    let path = test_temp_dir("prefab-light").join("lit.ron");
    prefab.save_to_path(&path).expect("prefab saves");

    let lights_of = |w: &EditorWorkspace| -> Vec<[f32; 3]> {
        w.project()
            .active_scene()
            .nodes()
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::PointLight { .. }))
            .map(|n| n.transform.translation)
            .collect()
    };
    assert!(
        lights_of(&workspace).is_empty(),
        "no lights before stamping"
    );

    workspace.stamp_prefab(&path);
    assert!(
        workspace.floating_geometry.is_some(),
        "{}",
        workspace.status
    );
    assert!(workspace.update_floating_geometry_origin([2, 2]));
    assert!(workspace.commit_floating_geometry());

    let placed = lights_of(&workspace);
    assert_eq!(placed.len(), 1, "exactly one light: {}", workspace.status);
    assert!(
        workspace.status.contains("1 light placed"),
        "the light is reported: {}",
        workspace.status
    );
    let parented = workspace
        .project()
        .active_scene()
        .nodes()
        .iter()
        .any(|n| matches!(n.kind, NodeKind::PointLight { .. }) && n.parent == Some(room));
    assert!(parented, "the light is a child of the room it lit");

    // Rotating moves the corner light to a different corner. Same piece, same
    // origin, so any difference is the transform being applied to the light.
    let mut rotated = EditorWorkspace::with_project(
        test_temp_dir("prefab-light-rot"),
        workspace.project().clone(),
    );
    rotated.replace_node_selection(room);
    rotated.stamp_prefab(&path);
    rotated.rotate_floating_geometry_cw();
    assert!(rotated.update_floating_geometry_origin([2, 2]));
    assert!(rotated.commit_floating_geometry());
    let after = lights_of(&rotated);
    assert_eq!(after.len(), 2, "the second stamp added its own light");
    assert_ne!(
        after[1], placed[0],
        "a rotated piece puts its light in a different corner"
    );

    let _ = std::fs::remove_file(&path);
}

/// The shared library is cached as editor state and must never create project
/// resources. Refreshing updates that cache without dirtying `project.ron`.
#[test]
fn refreshing_the_prefab_library_does_not_mutate_project_resources() {
    let project = ProjectDocument::new("prefab-library");
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("prefab-library"), project);
    let resources_before = workspace.project.resources.clone();
    let ron_before = workspace
        .project
        .to_ron_string()
        .expect("project serializes");

    let count = workspace
        .refresh_prefab_library()
        .expect("shared library refreshes");

    assert!(count > 0, "the embedded library is not empty");
    assert_eq!(workspace.prefab_library.len(), count);
    assert_eq!(workspace.project.resources, resources_before);
    assert_eq!(
        workspace
            .project
            .to_ron_string()
            .expect("project serializes"),
        ron_before
    );
    assert!(!workspace.dirty);
}

#[test]
fn selecting_a_prefab_preserves_the_stamp_room_and_cell() {
    let project = ProjectDocument::legacy_grid_starter();
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("prefab-target"), project);
    let room = workspace.active_room_id().expect("starter room");
    workspace.select_sector((room, 0, 0), egui::Modifiers::NONE);
    let path = workspace
        .prefab_library
        .first()
        .expect("embedded prefab library")
        .path
        .clone();

    workspace.replace_prefab_selection(path.clone());

    assert_eq!(workspace.active_room_id(), Some(room));
    assert_eq!(workspace.selection.selected_sector, Some((0, 0)));
    assert_eq!(workspace.selection.selected_prefab, Some(path));
}
