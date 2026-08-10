use super::*;

#[test]
fn room_grid_grow_preserves_spatial_descendant_preview_position() {
    let mut project = ProjectDocument::new("grid-grow");
    let scene = project.active_scene_mut();
    let room = scene.add_node(
        scene.root,
        "Room",
        NodeKind::Section {
            grid: WorldGrid::empty(2, 2, 1024),
        },
    );
    let entity = scene.add_node(room, "Entity", NodeKind::Entity);
    scene
        .node_mut(entity)
        .expect("entity exists")
        .transform
        .translation = [0.0, 0.0, 0.0];

    let before = test_node_preview_origin(&project, room, entity);
    assert_eq!(before, [1024, 0, 1024]);

    assert_eq!(
        extend_room_grid_to_include_preserving_child_positions(
            project.active_scene_mut(),
            room,
            2,
            0,
            0,
        ),
        Some((2, 0))
    );
    assert_eq!(test_node_preview_origin(&project, room, entity), before);

    assert_eq!(
        extend_room_grid_to_include_preserving_child_positions(
            project.active_scene_mut(),
            room,
            -1,
            0,
            0,
        ),
        Some((0, 0))
    );
    assert_eq!(test_node_preview_origin(&project, room, entity), before);
}

#[test]
fn centered_aspect_rect_centers_wide_preview_box() {
    let container = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(800.0, 240.0));

    let rect = centered_aspect_rect(container, VIEWPORT_PREVIEW_ASPECT);

    assert_size_approx(rect.size(), Vec2::new(320.0, 240.0));
    assert_pos_approx(rect.center(), container.center());
}

#[test]
fn centered_aspect_rect_centers_tall_preview_box() {
    let container = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(320.0, 800.0));

    let rect = centered_aspect_rect(container, VIEWPORT_PREVIEW_ASPECT);

    assert_size_approx(rect.size(), Vec2::new(320.0, 240.0));
    assert_pos_approx(rect.center(), container.center());
}

#[test]
fn screen_offset_preview_shift_scales_device_px_to_canvas_px() {
    // No offset -> no shift, regardless of scale.
    assert_eq!(screen_offset_preview_shift(0, 640.0, 320), 0.0);
    // 320-logical canvas drawn at 640 egui px is 2x, so 32 device px -> 64.
    assert_eq!(screen_offset_preview_shift(32, 640.0, 320), 64.0);
    // 1:1 scale passes the device offset straight through, sign preserved.
    assert_eq!(screen_offset_preview_shift(-16, 320.0, 320), -16.0);
    // Degenerate logical width is clamped, never divides by zero.
    assert_eq!(screen_offset_preview_shift(10, 320.0, 0), 3200.0);
}

#[test]
fn free_camera_center_ray_uses_position_and_forward_basis() {
    let camera = ViewportCameraState {
        mode: ViewportCameraMode::Free,
        yaw_q12: 0,
        pitch_q12: 0,
        radius: 1000,
        target: [0, 0, 0],
        position: [10, 20, 30],
    };

    let (origin, dir) = camera.ray_for_normalized_panel_point(0.0, 0.0);

    assert_vec3_approx(origin, [10.0, 20.0, 30.0]);
    assert_vec3_approx(dir, [0.0, 0.0, -1.0]);
    assert_eq!(camera.anchor_i32(), [10, 20, 30]);
    assert_eq!(camera.position_i32(), [10, 20, 30]);
}

#[test]
fn orbit_camera_keeps_target_anchor() {
    let camera = ViewportCameraState {
        mode: ViewportCameraMode::Orbit,
        yaw_q12: 0,
        pitch_q12: 0,
        radius: 1000,
        target: [10, 20, 30],
        position: [0, 0, 0],
    };

    let (origin, dir) = camera.ray_for_normalized_panel_point(0.0, 0.0);

    assert_vec3_approx(origin, [10.0, 20.0, 1030.0]);
    assert_vec3_approx(dir, [0.0, 0.0, -1.0]);
    assert_eq!(camera.anchor_i32(), [10, 20, 30]);
    assert_eq!(camera.position_i32(), [10, 20, 1030]);
}

#[test]
fn orbit_camera_quarter_turn_uses_q12_units() {
    let position = orbit_camera_position_i32(1024, 0, 1000, [10, 20, 30]);

    assert_eq!(position, [1010, 20, 30]);
}

#[test]
fn free_camera_forward_quarter_turn_uses_q12_units() {
    let forward = camera_forward_from_angles(1024, 0);

    assert_vec3_approx(forward, [-1.0, 0.0, 0.0]);
}

#[test]
fn focus_shortcut_fits_orbit_bounds_and_preserves_angle() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    workspace.camera_rig.mode = ViewportCameraMode::Orbit;
    workspace.camera_rig.radius = 12_345;
    workspace.camera_rig.yaw = 256;
    workspace.camera_rig.pitch = 256;

    workspace.frame_3d_bounds([4096.0, 512.0, -2048.0], [100.0, 500.0, 250.0]);

    assert_eq!(workspace.camera_rig.target, [4096, 512, -2048]);
    assert_eq!(workspace.camera_rig.radius, 1600);
    assert_eq!(workspace.camera_rig.yaw, 256);
    assert_eq!(workspace.camera_rig.pitch, 256);
}

#[test]
fn focus_shortcut_in_free_mode_fits_bounds_and_preserves_direction() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    workspace.camera_rig.mode = ViewportCameraMode::Free;
    workspace.camera_rig.free_position = [0, 0, 0];
    workspace.camera_rig.free_yaw = 1024;
    workspace.camera_rig.free_pitch = signed_to_q12(300);
    workspace.camera_rig.free_initialized = true;

    workspace.frame_3d_bounds([0.0, 0.0, -4096.0], [256.0, 256.0, 256.0]);

    assert_eq!(workspace.camera_rig.free_position, [734, -364, -4096]);
    assert_eq!(workspace.camera_rig.target, [0, 0, -4096]);
    assert_eq!(workspace.camera_rig.radius, 819);
    assert_eq!(workspace.camera_rig.free_yaw, 1024);
    assert_eq!(workspace.camera_rig.free_pitch, signed_to_q12(300));
}

#[test]
fn editor_camera_saves_with_project_and_restores_on_open() {
    let project_dir = test_temp_dir("editor-camera");
    let mut project = ProjectDocument::new("editor-camera");
    project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Section {
            grid: populated_grid(2, 2),
        },
    );
    let mut workspace = EditorWorkspace::with_project(project_dir.clone(), project);
    workspace.camera_rig.mode = ViewportCameraMode::Free;
    workspace.camera_rig.yaw = 384;
    workspace.camera_rig.pitch = signed_to_q12(-128);
    workspace.camera_rig.radius = 12_288;
    workspace.camera_rig.target = [1024, 512, -2048];
    workspace.camera_rig.free_yaw = 1536;
    workspace.camera_rig.free_pitch = 128;
    workspace.camera_rig.free_position = [-300, 700, 900];
    workspace.camera_rig.free_initialized = true;

    workspace.save().unwrap();

    let reopened = EditorWorkspace::open_directory(&project_dir).unwrap();
    let camera = reopened.viewport_3d_camera();
    assert_eq!(camera.mode, ViewportCameraMode::Free);
    assert_eq!(camera.yaw_q12, 1536);
    assert_eq!(camera.pitch_q12, 128);
    assert_eq!(camera.radius, 12_288);
    assert_eq!(camera.target, [1024, 512, -2048]);
    assert_eq!(camera.position, [-300, 700, 900]);

    let _ = std::fs::remove_dir_all(project_dir);
}

#[test]
fn editor_visibility_saves_with_project_and_restores_on_open() {
    let project_dir = test_temp_dir("editor-visibility");
    let mut workspace =
        EditorWorkspace::with_project(project_dir.clone(), ProjectDocument::new("visibility"));
    workspace.show_grid = false;
    workspace.show_portals = false;
    workspace.show_lights = false;
    workspace.preview_fog = false;
    workspace.preview_backface_wireframe = true;
    workspace.preview_bounds = false;
    workspace.show_play_debug_overlays = false;
    workspace.show_play_debug_map = true;

    workspace.save().unwrap();

    let reopened = EditorWorkspace::open_directory(&project_dir).unwrap();
    assert!(!reopened.show_grid_enabled());
    assert!(!reopened.show_portals_enabled());
    assert!(!reopened.show_lights_enabled());
    assert!(!reopened.preview_fog_enabled());
    assert!(reopened.preview_backface_wireframe_enabled());
    assert!(!reopened.preview_bounds_enabled());
    assert!(!reopened.show_play_debug_overlays);
    assert!(reopened.show_play_debug_map);

    let _ = std::fs::remove_dir_all(project_dir);
}

#[test]
fn editor_workspace_saves_with_project_and_restores_on_open() {
    let project_dir = test_temp_dir("editor-workspace");
    let mut workspace =
        EditorWorkspace::with_project(project_dir.clone(), ProjectDocument::new("workspace"));
    workspace.active_workspace = WorkspaceView::Material;

    workspace.save().unwrap();

    let reopened = EditorWorkspace::open_directory(&project_dir).unwrap();
    assert_eq!(reopened.active_workspace, WorkspaceView::Material);

    let _ = std::fs::remove_dir_all(project_dir);
}

#[test]
fn editor_viewport_saves_with_project_and_restores_on_open() {
    let project_dir = test_temp_dir("editor-viewport");
    let mut workspace =
        EditorWorkspace::with_project(project_dir.clone(), ProjectDocument::new("viewport"));
    workspace.view_2d = true;
    workspace.set_orthographic_view(OrthographicView::Side);
    workspace.orthographic_focus = [64.0, 128.0, 256.0];
    workspace.viewport_zoom = 48.0;
    workspace.snap_units = 32;

    workspace.save().unwrap();

    let reopened = EditorWorkspace::open_directory(&project_dir).unwrap();
    assert!(reopened.view_2d, "2D mode restored");
    assert_eq!(reopened.orthographic_view, OrthographicView::Side);
    assert_eq!(reopened.orthographic_focus, [64.0, 128.0, 256.0]);
    assert_eq!(reopened.viewport_zoom, 48.0);
    assert_eq!(reopened.snap_units, 32);

    // Out-of-range persisted zoom clamps into the interactive range.
    let mut wild = EditorWorkspace::open_directory(&project_dir).unwrap();
    wild.project.editor_viewport.viewport_zoom = 100_000.0;
    wild.project.editor_viewport.snap_units = 0;
    wild.apply_project_editor_viewport();
    assert_eq!(wild.viewport_zoom, MAX_VIEWPORT_ZOOM);
    assert_eq!(wild.snap_units, 1);

    let _ = std::fs::remove_dir_all(project_dir);
}

#[test]
fn typed_brush_cook_issue_selects_and_frames_the_authored_brush() {
    let mut project = ProjectDocument::new("brush diagnostic");
    project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [0, 0, 0],
            [128, 64, 192],
        ));
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.view_2d = false;
    workspace.active_tool = ViewTool::Select;

    assert!(workspace.focus_playtest_validation_target(
        psxed_project::playtest::PlaytestValidationTarget::Brush {
            brush: 0,
            face: Some(3),
        }
    ));

    assert_eq!(workspace.active_tool, ViewTool::Brush);
    assert_eq!(workspace.selected_brush, Some(0));
    assert_eq!(workspace.selected_brush_face, Some(3));
    assert_eq!(workspace.camera_rig.target, [64, 32, 96]);
    assert_eq!(workspace.status, "Framed selection");
}

#[test]
fn stale_typed_cook_issue_target_falls_back_cleanly() {
    let mut workspace = EditorWorkspace::with_project(
        std::env::temp_dir(),
        ProjectDocument::new("stale diagnostic"),
    );
    assert!(!workspace.focus_playtest_validation_target(
        psxed_project::playtest::PlaytestValidationTarget::Brush {
            brush: 99,
            face: None,
        }
    ));
    assert_eq!(workspace.selected_brush, None);
}

#[test]
fn texture_import_resolution_label_marks_presets_and_custom_sizes() {
    assert_eq!(texture_import_resolution_label(32, 32), "32 x 32");
    assert_eq!(texture_import_resolution_label(40, 24), "Custom 40 x 24");
}

#[test]
fn viewport_3d_pan_delta_tracks_pointer_drag_plane() {
    let camera = ViewportCameraState {
        mode: ViewportCameraMode::Orbit,
        yaw_q12: 0,
        pitch_q12: 0,
        radius: 1000,
        target: [0, 0, 0],
        position: [0, 0, 0],
    };

    let delta = viewport_3d_pan_delta(camera, Vec2::new(1000.0, 750.0), Vec2::new(100.0, 100.0));

    assert_vec3_approx(delta, [-100.0, 100.0, 0.0]);
}

#[test]
fn orbit_camera_rotation_uses_slow_step_and_clamps_pitch() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    workspace.camera_rig.yaw = 0;
    workspace.camera_rig.pitch = signed_to_q12(940);

    workspace.rotate_viewport_3d_camera(Vec2::new(100.0, 200.0));

    assert_eq!(workspace.camera_rig.yaw, 400);
    assert_eq!(workspace.camera_rig.pitch, signed_to_q12(960));
}

#[test]
fn free_camera_rotation_uses_q12_drag_sensitivity() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    workspace.camera_rig.mode = ViewportCameraMode::Free;
    workspace.camera_rig.free_yaw = 1024;
    workspace.camera_rig.free_pitch = 0;

    workspace.rotate_viewport_3d_camera(Vec2::new(100.0, 50.0));

    assert_eq!(workspace.camera_rig.free_yaw, 624);
    assert_eq!(workspace.camera_rig.free_pitch, signed_to_q12(-200));
    assert!(workspace.camera_rig.free_initialized);
}

#[test]
fn select_pick_passes_through_culled_wall_front_material() {
    let mut project = ProjectDocument::new("visible-pick");
    let mut one_sided = MaterialResource::opaque(None);
    one_sided.face_sidedness = MaterialFaceSidedness::Front;
    one_sided.sync_legacy_sidedness();
    let material = project.add_resource("one-sided", ResourceData::Material(one_sided));

    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.add_wall(0, 0, GridDirection::North, 0, 1024, Some(material));
    grid.add_wall(0, 0, GridDirection::South, 0, 1024, Some(material));
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });

    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.replace_node_selection(room);
    workspace.camera_rig.mode = ViewportCameraMode::Free;
    workspace.camera_rig.free_initialized = true;
    workspace.camera_rig.free_position = [512, 512, 2048];
    workspace.camera_rig.free_yaw = 0;
    workspace.camera_rig.free_pitch = 0;

    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(320.0, 240.0));
    let (face, hit) = workspace
        .pick_face_with_hit(rect, rect.center())
        .expect("ray should pass through hidden north wall to visible south wall");

    assert_eq!(
        face.kind,
        FaceKind::Wall {
            dir: GridDirection::South,
            stack: 0,
        }
    );
    assert!(hit[2].abs() < 0.001, "expected south wall hit, got {hit:?}");
}

#[test]
fn select_pick_passes_through_culled_ceiling_to_visible_floor() {
    let mut project = ProjectDocument::new("horizontal-visible-pick");
    let mut one_sided = MaterialResource::opaque(None);
    one_sided.face_sidedness = MaterialFaceSidedness::Front;
    one_sided.sync_legacy_sidedness();
    let material = project.add_resource("one-sided", ResourceData::Material(one_sided));

    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.set_floor(0, 0, 0, Some(material));
    grid.ensure_sector(0, 0).unwrap().ceiling =
        Some(GridHorizontalFace::flat(1024, Some(material)));
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });

    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.replace_node_selection(room);
    workspace.camera_rig.mode = ViewportCameraMode::Free;
    workspace.camera_rig.free_initialized = true;
    workspace.camera_rig.free_position = [512, 2048, 512];
    workspace.camera_rig.free_yaw = 0;
    workspace.camera_rig.free_pitch = signed_to_q12(-960);

    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(320.0, 240.0));
    let (_, dir) = workspace
        .camera_ray_for_pointer(rect, rect.center())
        .unwrap();
    assert!(dir[1] < -0.9, "expected downward ray, got {dir:?}");
    let (face, hit) = workspace
        .pick_face_with_hit(rect, rect.center())
        .expect("ray should pass through hidden ceiling top to visible floor top");

    assert_eq!(face.kind, FaceKind::Floor);
    assert!(hit[1].abs() < 0.001, "expected floor hit, got {hit:?}");
}

#[test]
fn paint_ceiling_ignores_floor_face_hit_for_targeting() {
    let mut project = ProjectDocument::new("ceiling-paint-face-filter");
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Section {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.active_tool = ViewTool::PaintCeiling;

    let floor_hit = Some((
        FaceRef {
            room,
            sx: 0,
            sz: 0,
            kind: FaceKind::Floor,
        },
        [512.0, 0.0, 512.0],
    ));
    let ceiling_hit = Some((
        FaceRef {
            room,
            sx: 0,
            sz: 0,
            kind: FaceKind::Ceiling,
        },
        [512.0, 2048.0, 512.0],
    ));

    assert_eq!(workspace.face_hit_for_paint_tool(floor_hit), None);
    assert_eq!(workspace.face_hit_for_paint_tool(ceiling_hit), ceiling_hit);
}

#[test]
fn paint_ceiling_fallback_pick_uses_ceiling_plane() {
    let mut project = ProjectDocument::new("ceiling-paint-plane");
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Section {
            grid: WorldGrid::empty(8, 8, 1024),
        },
    );
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.replace_node_selection(room);
    workspace.active_tool = ViewTool::PaintCeiling;
    workspace.camera_rig.mode = ViewportCameraMode::Free;
    workspace.camera_rig.free_initialized = true;
    workspace.camera_rig.free_position = [2048, 4096, 4096];
    workspace.camera_rig.free_yaw = 0;
    workspace.camera_rig.free_pitch = signed_to_q12(-960);

    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(320.0, 240.0));
    let pointer = rect.center() + egui::vec2(80.0, 0.0);
    let floor_pick = workspace
        .pick_3d_world_on_room_plane(rect, pointer, room, 0.0)
        .unwrap();
    let ceiling_pick = workspace.pick_3d_paint_world(rect, pointer, room).unwrap();

    let delta = (ceiling_pick[0] - floor_pick[0]).abs() + (ceiling_pick[1] - floor_pick[1]).abs();
    assert!(
            delta > 0.1,
            "ceiling pick should resolve on a different plane than floor pick: ceiling={ceiling_pick:?}, floor={floor_pick:?}"
        );
}

#[test]
fn command_modifier_blocks_bare_shortcuts() {
    assert!(bare_shortcuts_available(false, egui::Modifiers::NONE));
    assert!(!bare_shortcuts_available(true, egui::Modifiers::NONE));
    assert!(!bare_shortcuts_available(false, egui::Modifiers::COMMAND));
    assert!(!bare_shortcuts_available(false, egui::Modifiers::CTRL));
}

#[test]
fn command_shortcut_consumes_but_ignores_key_repeat() {
    let mut input = egui::InputState::default();
    let shortcut = command_shortcut(egui::Key::Z);
    input.events.push(egui::Event::Key {
        key: egui::Key::Z,
        physical_key: Some(egui::Key::Z),
        pressed: true,
        repeat: true,
        modifiers: egui::Modifiers::COMMAND,
    });
    assert!(!consume_shortcut_once(&mut input, &shortcut));
    assert!(input.events.is_empty());

    input.events.push(egui::Event::Key {
        key: egui::Key::Z,
        physical_key: Some(egui::Key::Z),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::COMMAND,
    });
    assert!(consume_shortcut_once(&mut input, &shortcut));
    assert!(input.events.is_empty());
}

#[test]
fn cycle_value_wraps_forward_and_backward() {
    const VALUES: &[u8] = &[1, 2, 3];

    assert_eq!(cycle_value(VALUES, 1, false), 2);
    assert_eq!(cycle_value(VALUES, 3, false), 1);
    assert_eq!(cycle_value(VALUES, 1, true), 3);
    assert_eq!(cycle_value(VALUES, 9, false), 1);
}

#[test]
fn tool_group_cycle_includes_explicit_add_slots() {
    let (mut workspace, room) = workspace_with_populated_grid("tool-group-cycle", 1, 1);
    workspace.replace_node_selection(room);
    workspace.active_tool = ViewTool::Erase;
    workspace.place_kind = PlaceKind::Character;

    workspace.cycle_tool_group(false);
    assert_eq!(workspace.active_tool, ViewTool::Place);
    assert_eq!(workspace.place_kind, PlaceKind::PlayerSpawn);

    workspace.cycle_tool_group(false);
    assert_eq!(workspace.active_tool, ViewTool::Place);
    assert_eq!(workspace.place_kind, PlaceKind::SpawnMarker);

    for expected in [
        PlaceKind::ModelInstance,
        PlaceKind::Character,
        PlaceKind::ImageProp,
        PlaceKind::BoxProp,
        PlaceKind::CylinderProp,
        PlaceKind::PointLightMarker,
        PlaceKind::ParticleEmitter,
        PlaceKind::Portal,
    ] {
        workspace.cycle_tool_group(false);
        assert_eq!(workspace.active_tool, ViewTool::Place);
        assert_eq!(workspace.place_kind, expected);
    }

    workspace.cycle_tool_group(false);
    assert_eq!(workspace.active_tool, ViewTool::Select);

    workspace.cycle_tool_group(true);
    assert_eq!(workspace.active_tool, ViewTool::Place);
    assert_eq!(workspace.place_kind, PlaceKind::Portal);
}

#[test]
fn place_kind_selection_updates_toolbar_label() {
    let mut workspace = EditorWorkspace::with_project(
        test_temp_dir("place-kind-toolbar"),
        ProjectDocument::new("place-kind-toolbar"),
    );
    workspace.set_active_tool_cycle_value((ViewTool::Place, Some(PlaceKind::ImageProp)));

    assert_eq!(workspace.place_kind, PlaceKind::ImageProp);
    assert_eq!(workspace.active_tool_group_label(), "Image Prop");
    assert_eq!(workspace.active_tool_group_icon(), icons::PALETTE);
    assert_eq!(workspace.status, "Tool: Image Prop");
}

#[test]
fn material_paint_is_a_separate_tool_with_an_explicit_blend_state() {
    let mut workspace = EditorWorkspace::with_project(
        test_temp_dir("terrain-mode-toolbar"),
        ProjectDocument::new("terrain-mode-toolbar"),
    );
    workspace.active_tool = ViewTool::PaintMaterial;

    assert_eq!(workspace.active_tool_group_label(), "Paint");
    workspace.material_paint_blend = true;
    assert_eq!(workspace.active_tool_group_label(), "Paint");
    workspace.active_tool = ViewTool::PaintFloor;
    assert_eq!(workspace.active_tool_group_label(), "Floor");
}

#[test]
fn entering_material_paint_clears_geometry_selection_and_syncs_the_material() {
    let mut project = ProjectDocument::new("paint-modal-selection");
    let material = project.add_resource(
        "Sand",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.set_floor(0, 0, 0, Some(material));
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("paint-modal-selection"), project);
    workspace.replace_node_selection(room);
    workspace.replace_primitive_selection(Selection::Face(FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Floor,
    }));
    workspace.replace_resource_selection(material);

    workspace.set_active_tool_cycle_value((ViewTool::PaintMaterial, None));

    assert!(workspace.selection.selected_primitive.is_none());
    assert!(workspace.selection.selected_primitives.is_empty());
    assert!(workspace.selection.selected_sector.is_none());
    assert_eq!(workspace.brush_material, Some(material));
    assert_eq!(workspace.selection.selected_resource, Some(material));
}

#[test]
fn visibility_cycle_only_changes_editor_view_items() {
    let mut workspace = EditorWorkspace::with_project(
        test_temp_dir("visibility-cycle"),
        ProjectDocument::new("visibility"),
    );
    workspace.show_grid = true;
    workspace.show_portals = true;
    workspace.show_lights = true;
    workspace.preview_fog = true;
    workspace.preview_backface_wireframe = true;
    workspace.preview_bounds = true;
    workspace.show_play_debug_overlays = false;
    workspace.show_play_debug_map = true;

    workspace.cycle_visibility_group(false);

    assert!(!workspace.show_grid);
    assert!(!workspace.show_portals);
    assert!(!workspace.show_lights);
    assert!(!workspace.preview_fog);
    assert!(!workspace.preview_backface_wireframe);
    assert!(!workspace.preview_bounds);
    assert!(!workspace.show_play_debug_overlays);
    assert!(workspace.show_play_debug_map);
}

#[test]
fn debug_snapshot_writes_portal_runtime_log() {
    let (mut workspace, room) = workspace_with_populated_grid("debug-snapshot", 2, 1);
    workspace.active_tool = ViewTool::Place;
    workspace.place_kind = PlaceKind::Portal;
    workspace.portal_place_direction = GridDirection::East;
    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [512.0, 0.0, 512.0]);

    let metrics = EditorPlaytestMetrics {
        sample_serial: 0,
        host_fps: 0.0,
        host_ms: 0.0,
        emu_hz: 0.0,
        visual_hz: None,
        draw_hz: 0.0,
        visual_frames: 0,
        visual_interval_vblanks: 0.0,
        visual_frame_times_ms: [0.0; 4],
        visual_frame_time_count: 0,
        visual_deadline_misses: 0,
        visual_lateness_vblanks: 0,
        total_ms: 0.0,
        frame_ms: 0.0,
        emu_ms: 0.0,
        hw_ms: 0.0,
        ui_ms: 0.0,
        step_budget_percent: 0.0,
        fixed_update_task_ms: 0.0,
        fixed_update_task_max_ms: 0.0,
        visual_render_task_ms: 0.0,
        visual_render_task_max_ms: 0.0,
        chunk_visible: 1,
        chunk_loaded: 1,
        chunk_candidates: 0,
        chunk_built: 0,
        chunk_cache_skips: 0,
        portal_visible_rooms: 1,
        portal_frontier_rooms: 0,
        portal_missing_resident: 0,
        portal_build_failed: 0,
        portal_tests: 1,
        portal_accepts: 1,
        portal_bounds_fallbacks: 0,
        portal_rejects: [0, 0, 0],
        portal_caps: [0, 0, 0],
        stream_priorities: [0, 0, 0],
        stream_requests: 0,
        stream_misses: 0,
        stream_prefetches: 0,
        stream_evictions: 0,
        stream_slot_limit: 0,
        stream_pending: 0,
        stream_failed: 0,
        stream_protected_full: 0,
        vram_texture_drops: 0,
        vram_caps_full: [0, 0, 0, 0],
        room_material_slot_overflow: 0,
        room_visibility_fallback_draws: 0,
        chunk_loaded_mask: 1,
        chunk_loading_mask: 0,
        chunk_active_mask: 1,
        chunk_drawn_mask: 1,
        portal_visible_mask: 1,
        portal_frontier_mask: 0,
        portal_missing_mask: 0,
        portal_build_failed_mask: 0,
        portal_tested_mask: 1,
        portal_accepted_mask: 1,
        portal_reject_frustum_mask: 0,
        portal_bounds_fallback_mask: 0,
        portal_tested_portal_mask: 1,
        portal_accepted_portal_mask: 1,
        portal_reject_frustum_portal_mask: 0,
        portal_bounds_fallback_portal_mask: 0,
        player_map_valid: true,
        player_room_index: 0,
        portal_current_room_index: 0,
        player_local_x: 512,
        player_local_z: 512,
        player_view_yaw_q12: 1024,
        camera_view_basis_valid: true,
        camera_view_sin_yaw_q12: 4096,
        camera_view_cos_yaw_q12: 0,
        camera_view_sin_pitch_q12: 0,
        camera_view_cos_pitch_q12: 4096,
        camera_map_valid: true,
        camera_global_valid: true,
        camera_local_x: 520,
        camera_local_y: 1024,
        camera_local_z: 500,
        camera_global_x: 520,
        camera_global_y: 1024,
        camera_global_z: 500,
    };
    let path = workspace.debug_log_path();

    workspace
        .write_debug_snapshot(&path, Some(metrics))
        .unwrap();

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("scheduler_tasks:"));
    assert!(content.contains("runtime_player: valid=true room_index=0"));
    assert!(content.contains("connected_portals: count="));
    assert!(content.contains("portal #0:"));

    let _ = std::fs::remove_dir_all(workspace.project_dir);
}

#[test]
fn play_frame_time_history_uses_measured_guest_intervals() {
    let (mut workspace, _) = workspace_with_populated_grid("frame-time-history", 1, 1);
    let metrics = EditorPlaytestMetrics {
        sample_serial: 7,
        visual_frames: 2,
        frame_ms: 99.0,
        visual_frame_times_ms: [33.25, 34.5, 0.0, 0.0],
        visual_frame_time_count: 2,
        ..EditorPlaytestMetrics::default()
    };

    workspace.record_play_frame_time(metrics);
    workspace.record_play_frame_time(metrics);

    assert_eq!(
        workspace
            .play_frame_times_ms
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![33.25, 34.5]
    );
}

#[test]
fn menu_labels_include_discoverable_shortcut_text() {
    assert_eq!(menu_label("Save", "Cmd+S"), "Save    Cmd+S");
}

#[test]
fn animation_source_catalogue_scans_synty_source_tree() {
    let dir = test_temp_dir("animation-source-catalogue");
    let anim_dir = dir.join("SourceFiles/Animations/Polygon/Dodge");
    let model_dir = dir.join("SourceFiles/Models");
    std::fs::create_dir_all(&anim_dir).unwrap();
    std::fs::create_dir_all(&model_dir).unwrap();
    std::fs::write(anim_dir.join("A_DodgeRoll_F_RootMotion_Sword.fbx"), []).unwrap();
    std::fs::write(anim_dir.join("A_Block_Loop_Sword.fbx"), []).unwrap();
    std::fs::write(model_dir.join("POLYGONRig_01.fbx"), []).unwrap();

    let mut project = ProjectDocument::new("source catalogue");
    let report = catalogue_animation_sources_from_path(&mut project, &dir, &dir).unwrap();

    assert_eq!(report.source_candidates, 2);
    assert_eq!(report.sources_added, 2);
    assert_eq!(report.sources_updated, 0);
    let sources: Vec<_> = project
        .resources
        .iter()
        .filter_map(|resource| match &resource.data {
            ResourceData::AnimationSource(source) => Some((resource.name.as_str(), source)),
            _ => None,
        })
        .collect();
    assert_eq!(sources.len(), 2);
    let roll = sources
        .iter()
        .find(|(_, source)| source.clip_name == "A_DodgeRoll_F_RootMotion_Sword")
        .expect("roll source catalogued")
        .1;
    assert_eq!(roll.provider, psxed_project::AnimationSourceProvider::Synty);
    assert_eq!(roll.role, psxed_project::AnimationRole::Roll);
    assert!(!roll.looping);
    assert!(roll.tags.iter().any(|tag| tag == "dodge"));
    assert!(roll.tags.iter().any(|tag| tag == "root_motion"));
    assert_eq!(
        roll.source_path,
        "SourceFiles/Animations/Polygon/Dodge/A_DodgeRoll_F_RootMotion_Sword.fbx"
    );

    let second = catalogue_animation_sources_from_path(&mut project, &dir, &dir).unwrap();
    assert_eq!(second.source_candidates, 2);
    assert_eq!(second.sources_added, 0);
    assert_eq!(second.sources_updated, 0);

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn materialize_authoring_source_path_extracts_deflated_zip_entry() {
    use std::io::Write;

    let dir = test_temp_dir("animation-source-zip");
    let temp_dir = dir.join("tmp");
    std::fs::create_dir_all(&temp_dir).unwrap();
    let zip_path = dir.join("sources.zip");
    let file = std::fs::File::create(&zip_path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    writer
        .start_file("SourceFiles/Animations/Test/test_clip.fbx", options)
        .unwrap();
    writer.write_all(b"fake-fbx-data").unwrap();
    writer.finish().unwrap();

    let source_path = format!(
        "{}::SourceFiles/Animations/Test/test_clip.fbx",
        zip_path.display()
    );
    let extracted = materialize_authoring_source_path(&source_path, &dir, &temp_dir).unwrap();

    assert_eq!(std::fs::read(extracted).unwrap(), b"fake-fbx-data");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn open_directory_saves_and_reloads_project() {
    let dir = std::env::temp_dir().join(format!(
        "psxed-ui-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let project_file = dir.join("project.ron");
    std::fs::write(
        &project_file,
        ProjectDocument::starter().to_ron_string().unwrap(),
    )
    .unwrap();

    let mut workspace = EditorWorkspace::open_directory(&dir).unwrap();
    assert!(!workspace.is_dirty());
    assert_eq!(workspace.project_root(), dir);
    workspace.save().unwrap();
    assert!(project_file.is_file());

    let loaded = EditorWorkspace::open_directory(&dir).unwrap();
    assert!(!loaded.is_dirty());
    assert_eq!(
        loaded.project().resources.len(),
        workspace.project().resources.len()
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn open_directory_syncs_legacy_starter_character_catalogue() {
    let dir = std::env::temp_dir().join(format!(
        "psxed-ui-test-character-sync-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let starter = ProjectDocument::starter();
    let mut legacy = ProjectDocument::new("legacy-starter");
    let mut wraith_model = starter
        .resources
        .iter()
        .find_map(|resource| match &resource.data {
            ResourceData::Model(model) if resource.name == "Obsidian Wraith" => Some(model.clone()),
            _ => None,
        })
        .expect("starter has wraith model");
    wraith_model.skeleton = None;
    let model = legacy.add_resource("Obsidian Wraith", ResourceData::Model(wraith_model));
    let mut character = psxed_project::CharacterResource::defaults();
    character.model = Some(model);
    legacy.add_resource(
        LEGACY_WRAITH_HERO_PROFILE_NAME,
        ResourceData::Character(character),
    );
    legacy.save_to_path(dir.join("project.ron")).unwrap();

    let workspace = EditorWorkspace::open_directory(&dir).unwrap();

    assert!(!workspace.is_dirty());
    for name in STARTER_CHARACTER_PROFILE_NAMES {
        assert!(
            project_has_resource_name(workspace.project(), name, |data| {
                matches!(data, ResourceData::Character(_))
            }),
            "missing {name}"
        );
    }
    assert!(!project_has_resource_name(
        workspace.project(),
        LEGACY_WRAITH_HERO_PROFILE_NAME,
        |data| matches!(data, ResourceData::Character(_))
    ));
    assert!(project_has_resource_name(
        workspace.project(),
        "Crimson Cross Knight",
        |data| { matches!(data, ResourceData::Model(_)) }
    ));
    assert!(dir
        .join("assets/models/crimson_cross_knight/crimson_cross_knight.psxmdl")
        .is_file());

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn open_directory_purges_legacy_obsidian_warden_catalogue() {
    let dir = test_temp_dir("purge-obsidian-warden");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mut project = ProjectDocument::starter();
    let skeleton = project
        .resources
        .iter()
        .find_map(|resource| (resource.name == "Meshy Biped Skeleton").then_some(resource.id))
        .expect("starter skeleton");
    let legacy_model = project.add_resource(
        "Obsidian Warden",
        ResourceData::Model(psxed_project::ModelResource {
            model_path: "assets/models/obsidian_warden/obsidian_warden.psxmdl".to_string(),
            source_path: None,
            texture_path: Some(
                "assets/models/obsidian_warden/obsidian_warden_128x128_8bpp.psxt".to_string(),
            ),
            skeleton: Some(skeleton),
            world_height: 1024,
            collision_radius: default_model_collision_radius_for_height(1024),
            scale_q8: [psxed_project::MODEL_SCALE_ONE_Q8; 3],
            default_visual_yaw_q12: 0,
            attachments: Vec::new(),
        }),
    );
    let legacy_set = project.add_resource(
        "Obsidian Warden Enemy Set",
        ResourceData::AnimationSet(psxed_project::AnimationSetResource {
            skeleton: Some(skeleton),
            ..psxed_project::AnimationSetResource::default()
        }),
    );
    let mut legacy_character = psxed_project::CharacterResource::defaults();
    legacy_character.model = Some(legacy_model);
    legacy_character.animation_set = Some(legacy_set);
    project.add_resource(
        "Obsidian Warden Enemy",
        ResourceData::Character(legacy_character),
    );

    let legacy_asset_dir = dir.join(LEGACY_OBSIDIAN_WARDEN_ASSET_DIR);
    std::fs::create_dir_all(&legacy_asset_dir).unwrap();
    std::fs::write(legacy_asset_dir.join("obsidian_warden.psxmdl"), b"old").unwrap();
    project.save_to_path(dir.join("project.ron")).unwrap();

    let workspace = EditorWorkspace::open_directory(&dir).unwrap();

    assert!(!workspace.is_dirty());
    assert!(!workspace.project().resources.iter().any(|resource| {
        resource.name.contains("Obsidian Warden") || legacy_obsidian_warden_resource(resource)
    }));
    assert!(project_has_resource_name(
        workspace.project(),
        "Crowned Wraith Enemy",
        |data| matches!(data, ResourceData::Character(_))
    ));
    assert!(!legacy_asset_dir.exists());
    assert!(dir
        .join("assets/animations/standalone_fbx/neutral_idle.psxanim")
        .is_file());

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn open_directory_does_not_resurrect_deleted_starter_characters() {
    let dir = test_temp_dir("no-resurrect-starter-catalogue");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mut project = ProjectDocument::starter();
    project.resources.retain(|resource| {
        resource.name == "Crimson Cross Knight"
            || resource.name == "Crimson Cross Knight Player"
            || resource.name == "Crimson Cross Knight Player Set"
            || !STARTER_CHARACTER_PROFILE_NAMES.contains(&resource.name.as_str())
                && !STARTER_CHARACTER_MODEL_NAMES.contains(&resource.name.as_str())
                && !STARTER_ANIMATION_SET_NAMES.contains(&resource.name.as_str())
    });
    project.save_to_path(dir.join("project.ron")).unwrap();

    let workspace = EditorWorkspace::open_directory(&dir).unwrap();

    assert!(!workspace.is_dirty());
    for name in [
        "Obsidian Wraith Enemy",
        "Hooded Wretch Enemy",
        "Crowned Wraith Enemy",
        "Obsidian Wraith",
        "Hooded Wretch",
        "Crowned Wraith",
    ] {
        assert!(
            !workspace
                .project()
                .resources
                .iter()
                .any(|resource| resource.name == name),
            "{name} should stay deleted"
        );
    }
    assert!(!dir.join("assets/models/obsidian_wraith").exists());

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn open_directory_errors_when_project_ron_missing() {
    let dir = std::env::temp_dir().join(format!(
        "psxed-ui-test-missing-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let err = match EditorWorkspace::open_directory(&dir) {
        Ok(_) => panic!("expected open_directory to fail on missing project.ron"),
        Err(e) => e,
    };
    assert!(err.contains("project.ron"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn create_and_open_project_validates_non_empty_name() {
    let mut ws = EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    assert!(ws.create_and_open_project("").is_err());
    // "default" is a real existing dir, so this hits the "already exists" branch.
    assert!(ws.create_and_open_project("default").is_err());
}

#[test]
fn create_and_open_project_sets_document_name_and_derived_directory() {
    let mut ws = EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    let name = format!(
        "Project Rename {} {}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let target = psxed_project::projects_dir().join(psxed_project::project_file_stem(&name));
    let _ = std::fs::remove_dir_all(&target);

    ws.create_and_open_project(&name).unwrap();

    assert_eq!(ws.project().name, name);
    assert_eq!(ws.project_root(), target);
    assert!(!ws.is_dirty());
    assert!(ws.view_2d);
    assert_eq!(ws.active_tool, ViewTool::Brush);
    assert!(
        !ws.project().active_scene().brushes.is_empty(),
        "new projects use the BSP-first brush template"
    );
    assert!(
        ws.project()
            .active_scene()
            .nodes()
            .iter()
            .all(|node| !matches!(node.kind, NodeKind::Section { .. })),
        "new projects do not inherit legacy grid sections"
    );
    assert!(ws
        .project()
        .resources
        .iter()
        .all(|resource| match &resource.data {
            ResourceData::Material(material) => {
                material.face_sidedness == MaterialFaceSidedness::Front && !material.double_sided
            }
            _ => true,
        }));
    let saved = ProjectDocument::load_from_path(target.join("project.ron")).unwrap();
    assert_eq!(saved.name, ws.project().name);
    assert_eq!(
        saved.bsp_cook_mode,
        psxed_project::brush_world::BrushWorldCookMode::Draft
    );
    let _ = std::fs::remove_dir_all(target);
}

#[test]
fn new_project_release_choice_is_saved_with_the_bsp_first_template() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    let name = format!(
        "Release BSP Project {} {}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let target = psxed_project::projects_dir().join(psxed_project::project_file_stem(&name));
    let _ = std::fs::remove_dir_all(&target);

    workspace
        .create_and_open_project_with_mode(
            &name,
            psxed_project::brush_world::BrushWorldCookMode::Release,
        )
        .unwrap();

    assert_eq!(
        workspace.project().bsp_cook_mode,
        psxed_project::brush_world::BrushWorldCookMode::Release
    );
    assert!(!workspace.project().active_scene().brushes.is_empty());
    let saved = ProjectDocument::load_from_path(target.join("project.ron")).unwrap();
    assert_eq!(
        saved.bsp_cook_mode,
        psxed_project::brush_world::BrushWorldCookMode::Release
    );
    let _ = std::fs::remove_dir_all(target);
}

#[test]
fn one_click_play_and_rebuild_recook_the_persisted_bsp_mode() {
    let fixture_dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../projects/brush-first-playable");
    let project = ProjectDocument::load_from_path(fixture_dir.join("project.ron")).unwrap();
    let output = test_temp_dir("bsp-mode-replay-output");
    let _ = std::fs::remove_dir_all(&output);
    let mut workspace = EditorWorkspace::with_project(fixture_dir, project);

    workspace.set_bsp_cook_mode(psxed_project::brush_world::BrushWorldCookMode::Release);
    workspace.request_play_or_rebuild(EditorPlaytestStatus::Idle);
    assert_eq!(
        workspace.take_playtest_request(),
        Some(EditorPlaytestRequest::Play)
    );
    workspace
        .cook_playtest_to_dir(&output)
        .expect("Release Play cook");
    let release =
        std::fs::read(output.join(psxed_project::brush_playtest::BRUSH_WORLD_FILENAME)).unwrap();
    assert_eq!(
        workspace.playtest_budget_report().unwrap().stage,
        psxed_project::playtest::PlaytestBudgetStage::Cooked
    );
    assert_eq!(
        workspace.playtest_budget_report().unwrap().mode,
        psxed_project::brush_world::BrushWorldCookMode::Release
    );

    workspace.set_bsp_cook_mode(psxed_project::brush_world::BrushWorldCookMode::Draft);
    assert!(
        workspace.playtest_budget_report().is_none(),
        "editing the project must discard stale exact cook diagnostics"
    );
    workspace.request_play_or_rebuild(EditorPlaytestStatus::Running {
        input_captured: false,
    });
    assert_eq!(
        workspace.take_playtest_request(),
        Some(EditorPlaytestRequest::Rebuild)
    );
    workspace
        .cook_playtest_to_dir(&output)
        .expect("Draft re-Play cook");
    let draft =
        std::fs::read(output.join(psxed_project::brush_playtest::BRUSH_WORLD_FILENAME)).unwrap();
    let manifest =
        std::fs::read_to_string(output.join(psxed_project::playtest::COOKED_MANIFEST_FILENAME))
            .unwrap();

    assert_ne!(release, draft, "Rebuild must replace the previous PXBSP");
    assert!(manifest.contains("pub const BSP_COOK_IS_RELEASE: bool = false;"));
    assert_eq!(
        workspace.playtest_budget_report().unwrap().mode,
        psxed_project::brush_world::BrushWorldCookMode::Draft
    );
    let _ = std::fs::remove_dir_all(output);
}

#[test]
fn exceeded_budget_target_can_focus_the_offending_brush() {
    let mut project = ProjectDocument::new("budget target");
    // Enough cuboids to overflow even the derived-capacity ceiling.
    for index in 0..400 {
        project
            .active_scene_mut()
            .brushes
            .push(psxed_project::brush::Brush::cuboid(
                [index * 4, 0, 0],
                [index * 4 + 2, 2, 2],
            ));
    }
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    let budget = psxed_project::playtest::estimate_playtest_budgets(
        workspace.project(),
        workspace.project_root(),
    );
    let target = budget
        .first_actionable_issue()
        .and_then(|issue| issue.target)
        .expect("actionable packet target");

    assert!(workspace.focus_playtest_validation_target(target));
    assert_eq!(workspace.selected_brush, Some(399));
    assert_eq!(workspace.active_tool, ViewTool::Brush);
}

#[test]
fn save_renames_project_directory_when_project_name_changes() {
    let parent = test_temp_dir("rename-project-parent");
    let source = parent.join("old_project");
    std::fs::create_dir_all(&source).unwrap();
    let project_file = source.join("project.ron");
    std::fs::write(
        &project_file,
        ProjectDocument::new("Old Project").to_ron_string().unwrap(),
    )
    .unwrap();
    let mut workspace = EditorWorkspace::open_directory(&source).unwrap();

    workspace.project.name = "New Project".to_string();
    workspace.mark_dirty();
    workspace.save().unwrap();

    let target = parent.join(psxed_project::project_file_stem("New Project"));
    assert_eq!(workspace.project_root(), target);
    assert!(!source.exists());
    let saved = ProjectDocument::load_from_path(target.join("project.ron")).unwrap();
    assert_eq!(saved.name, "New Project");
    let _ = std::fs::remove_dir_all(parent);
}

#[test]
fn save_rejects_project_rename_collision() {
    let parent = test_temp_dir("rename-project-collision");
    let source = parent.join("old_project");
    let target = parent.join(psxed_project::project_file_stem("New Project"));
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(
        source.join("project.ron"),
        ProjectDocument::new("Old Project").to_ron_string().unwrap(),
    )
    .unwrap();
    let mut workspace = EditorWorkspace::open_directory(&source).unwrap();

    workspace.project.name = "New Project".to_string();
    workspace.mark_dirty();
    let error = workspace.save().unwrap_err();

    assert!(error.contains("already exists"));
    assert_eq!(workspace.project_root(), source);
    let _ = std::fs::remove_dir_all(parent);
}

#[test]
fn delete_current_project_refuses_default_project() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();

    let error = workspace.delete_current_project().unwrap_err();

    assert!(error.contains("default project"));
    assert!(psxed_project::default_project_dir()
        .join("project.ron")
        .is_file());
}

#[test]
fn delete_current_project_removes_directory_and_loads_default() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    let name = format!(
        "Delete Project {} {}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let target = psxed_project::projects_dir().join(psxed_project::project_file_stem(&name));
    let _ = std::fs::remove_dir_all(&target);

    workspace.create_and_open_project(&name).unwrap();
    assert!(target.join("project.ron").is_file());

    workspace.delete_current_project().unwrap();

    assert!(!target.exists());
    assert!(paths_equivalent(
        workspace.project_root(),
        &psxed_project::default_project_dir()
    ));
    assert!(!workspace.is_dirty());
}

#[test]
fn delete_current_project_refuses_directory_outside_projects_root() {
    let dir = test_temp_dir("delete-outside-project-root");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("project.ron"),
        ProjectDocument::new("External Project")
            .to_ron_string()
            .unwrap(),
    )
    .unwrap();
    let mut workspace = EditorWorkspace::open_directory(&dir).unwrap();

    let error = workspace.delete_current_project().unwrap_err();

    // The guard reports the root it actually enforced, and `projects_dir()`
    // builds that from CARGO_MANIFEST_DIR without normalising the `..`
    // segments, so match the resolved value rather than a literal path.
    assert!(
        error.contains(&psxed_project::projects_dir().display().to_string()),
        "{error}"
    );
    assert!(dir.join("project.ron").is_file());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn create_and_open_project_keeps_old_texture_handles_alive_temporarily() {
    let mut ws = EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    let ctx = egui::Context::default();
    let texture_id = ws.project().resources[0].id;
    let handle = ctx.load_texture(
        "project-switch-thumb",
        ColorImage {
            size: [1, 1],
            pixels: vec![Color32::WHITE],
        },
        egui::TextureOptions::NEAREST,
    );
    ws.texture_thumbs.insert(
        texture_id,
        ThumbnailEntry {
            signature: "test.psxt".to_string(),
            handle,
            image: ColorImage {
                size: [1, 1],
                pixels: vec![Color32::WHITE],
            },
            stats: PsxtStats {
                width: 1,
                height: 1,
                depth_bits: 4,
                clut_entries: 16,
                index_zero_transparent: false,
                pixel_bytes: 1,
                clut_bytes: 32,
                file_bytes: 45,
            },
        },
    );
    ws.psoxide_logo_texture = Some(ctx.load_texture(
        "project-switch-logo",
        ColorImage {
            size: [1, 1],
            pixels: vec![Color32::WHITE],
        },
        egui::TextureOptions::NEAREST,
    ));
    ws.model_resource_preview_texture = Some(ctx.load_texture(
        "project-switch-model-preview",
        ColorImage {
            size: [1, 1],
            pixels: vec![Color32::WHITE],
        },
        egui::TextureOptions::NEAREST,
    ));
    ws.animation_viewer_preview_texture = Some(ctx.load_texture(
        "project-switch-animation-preview",
        ColorImage {
            size: [1, 1],
            pixels: vec![Color32::WHITE],
        },
        egui::TextureOptions::NEAREST,
    ));

    let name = format!(
        "texture-retire-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let target = psxed_project::projects_dir().join(psxed_project::project_file_stem(&name));
    let _ = std::fs::remove_dir_all(&target);

    ws.create_and_open_project(&name).unwrap();

    assert!(ws.texture_thumbs.is_empty());
    assert_eq!(ws.import_retired_textures.len(), 4);
    assert!(ws
        .import_retired_textures
        .iter()
        .all(|(frames, _)| *frames == EGUI_TEXTURE_RETIRE_FRAMES));
    let _ = std::fs::remove_dir_all(target);
}

#[test]
fn switch_project_opens_target_and_retains_old_texture_handles() {
    let source_dir = test_temp_dir("switch-source");
    let target_dir = test_temp_dir("switch-target");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::create_dir_all(&target_dir).unwrap();
    let mut source_project = ProjectDocument::starter();
    source_project.name = "Source".to_string();
    let mut target_project = ProjectDocument::starter();
    target_project.name = "Target".to_string();
    std::fs::write(
        source_dir.join("project.ron"),
        source_project.to_ron_string().unwrap(),
    )
    .unwrap();
    std::fs::write(
        target_dir.join("project.ron"),
        target_project.to_ron_string().unwrap(),
    )
    .unwrap();

    let mut ws = EditorWorkspace::open_directory(&source_dir).unwrap();
    let ctx = egui::Context::default();
    let texture_id = ws.project().resources[0].id;
    let handle = ctx.load_texture(
        "switch-project-thumb",
        ColorImage {
            size: [1, 1],
            pixels: vec![Color32::WHITE],
        },
        egui::TextureOptions::NEAREST,
    );
    ws.texture_thumbs.insert(
        texture_id,
        ThumbnailEntry {
            signature: "test.psxt".to_string(),
            handle,
            image: ColorImage {
                size: [1, 1],
                pixels: vec![Color32::WHITE],
            },
            stats: PsxtStats {
                width: 1,
                height: 1,
                depth_bits: 4,
                clut_entries: 16,
                index_zero_transparent: false,
                pixel_bytes: 1,
                clut_bytes: 32,
                file_bytes: 45,
            },
        },
    );

    ws.switch_project(&target_dir).unwrap();

    assert_eq!(ws.project().name, "Target");
    assert_eq!(ws.project_root(), target_dir);
    assert!(ws.texture_thumbs.is_empty());
    assert_eq!(ws.import_retired_textures.len(), 1);
    assert_eq!(ws.import_retired_textures[0].0, EGUI_TEXTURE_RETIRE_FRAMES);

    let _ = std::fs::remove_dir_all(source_dir);
    let _ = std::fs::remove_dir_all(target_dir);
}

#[test]
fn viewport_transform_roundtrips_world_and_screen_points() {
    let transform = ViewportTransform::new(
        Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(300.0, 200.0)),
        Vec2::new(12.0, -8.0),
        40.0,
    );

    let world = [1.25, -0.5];
    let screen = transform.world_to_screen(world);
    let roundtrip = transform.screen_to_world(screen);

    assert!((roundtrip[0] - world[0]).abs() < 0.001);
    assert!((roundtrip[1] - world[1]).abs() < 0.001);
}

#[test]
fn viewport_hits_rectangles_and_circles() {
    let rect = ViewportHit::rect(NodeId::ROOT, "Rect", [0.0, 0.0], [1.0, 0.5]);
    assert!(rect.contains([0.25, 0.25]));
    assert!(!rect.contains([1.25, 0.25]));

    let circle = ViewportHit::circle(NodeId::ROOT, "Circle", [2.0, 2.0], 0.5);
    assert!(circle.contains([2.25, 2.25]));
    assert!(!circle.contains([2.6, 2.0]));

    let segment = ViewportHit::segment(NodeId::ROOT, "Segment", [0.0, 0.0], [2.0, 0.0], 0.25);
    assert!(segment.contains([1.0, 0.2]));
    assert!(!segment.contains([1.0, 0.3]));
}

#[test]
fn inspector_changes_are_undoable_as_discrete_steps() {
    let mut workspace =
        EditorWorkspace::with_project(std::env::temp_dir(), ProjectDocument::new("inspector-undo"));
    let root = workspace.project.active_scene().root;
    let original = workspace
        .project
        .active_scene()
        .node(root)
        .unwrap()
        .name
        .clone();

    for name in ["First inspector edit", "Second inspector edit"] {
        let before = workspace.project.clone();
        let history_epoch = workspace.history.epoch();
        workspace
            .project
            .active_scene_mut()
            .node_mut(root)
            .unwrap()
            .name = name.to_string();
        workspace.finish_inspector_undo(before, history_epoch, InspectorUndoInput::default());
    }

    workspace.do_undo();
    assert_eq!(
        workspace.project.active_scene().node(root).unwrap().name,
        "First inspector edit"
    );
    workspace.do_undo();
    assert_eq!(
        workspace.project.active_scene().node(root).unwrap().name,
        original
    );
}

#[test]
fn global_ctrl_or_cmd_z_reverts_an_inspector_change() {
    let mut workspace = EditorWorkspace::with_project(
        std::env::temp_dir(),
        ProjectDocument::new("inspector-shortcut-undo"),
    );
    let root = workspace.project.active_scene().root;
    let original = workspace
        .project
        .active_scene()
        .node(root)
        .unwrap()
        .name
        .clone();
    let before = workspace.project.clone();
    let history_epoch = workspace.history.epoch();
    workspace
        .project
        .active_scene_mut()
        .node_mut(root)
        .unwrap()
        .name = "Changed in Inspector".to_string();
    workspace.finish_inspector_undo(before, history_epoch, InspectorUndoInput::default());

    let ctx = egui::Context::default();
    let _ = ctx.run(
        egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Z,
                physical_key: Some(egui::Key::Z),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::COMMAND,
            }],
            ..egui::RawInput::default()
        },
        |ctx| workspace.handle_global_shortcuts(ctx, EditorPlaytestStatus::Idle),
    );

    assert_eq!(
        workspace.project.active_scene().node(root).unwrap().name,
        original
    );
    assert_eq!(workspace.status, "Undo");
}

#[test]
fn inspector_pointer_drag_coalesces_to_one_undo_step() {
    let mut workspace = EditorWorkspace::with_project(
        std::env::temp_dir(),
        ProjectDocument::new("inspector-drag-undo"),
    );
    let root = workspace.project.active_scene().root;
    let original = workspace
        .project
        .active_scene()
        .node(root)
        .unwrap()
        .name
        .clone();
    let drag = InspectorUndoInput {
        pointer_down: true,
        ..InspectorUndoInput::default()
    };

    for name in ["Drag frame one", "Drag frame two", "Drag frame three"] {
        let before = workspace.project.clone();
        let history_epoch = workspace.history.epoch();
        workspace
            .project
            .active_scene_mut()
            .node_mut(root)
            .unwrap()
            .name = name.to_string();
        workspace.finish_inspector_undo(before, history_epoch, drag);
    }
    workspace.prepare_inspector_undo(InspectorUndoInput::default());

    workspace.do_undo();
    assert_eq!(
        workspace.project.active_scene().node(root).unwrap().name,
        original
    );
    workspace.do_redo();
    assert_eq!(
        workspace.project.active_scene().node(root).unwrap().name,
        "Drag frame three"
    );
}

#[test]
fn inspector_respects_explicit_history_and_deliberate_clears() {
    let mut workspace = EditorWorkspace::with_project(
        std::env::temp_dir(),
        ProjectDocument::new("inspector-explicit-history"),
    );
    let root = workspace.project.active_scene().root;
    let original = workspace
        .project
        .active_scene()
        .node(root)
        .unwrap()
        .name
        .clone();

    let before = workspace.project.clone();
    let history_epoch = workspace.history.epoch();
    workspace.push_undo();
    workspace
        .project
        .active_scene_mut()
        .node_mut(root)
        .unwrap()
        .name = "Explicit".to_string();
    workspace.finish_inspector_undo(before, history_epoch, InspectorUndoInput::default());
    workspace.do_undo();
    assert_eq!(
        workspace.project.active_scene().node(root).unwrap().name,
        original
    );
    workspace.do_undo();
    assert_eq!(workspace.status, "Nothing to undo");

    let before = workspace.project.clone();
    let history_epoch = workspace.history.epoch();
    workspace.history.clear();
    workspace
        .project
        .active_scene_mut()
        .node_mut(root)
        .unwrap()
        .name = "Filesystem edit".to_string();
    workspace.finish_inspector_undo(before, history_epoch, InspectorUndoInput::default());
    workspace.do_undo();
    assert_eq!(workspace.status, "Nothing to undo");
}
