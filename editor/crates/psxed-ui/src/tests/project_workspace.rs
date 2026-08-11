use super::*;

fn save_watch_project(dir: &Path, project: &ProjectDocument) {
    std::fs::create_dir_all(dir).unwrap();
    project.save_to_path(&dir.join("project.ron")).unwrap();
}

#[test]
fn external_project_change_auto_reloads_clean_but_protects_dirty_edits() {
    let dir = test_temp_dir("project-watch-conflict");
    let project = ProjectDocument::new("watch baseline");
    save_watch_project(&dir, &project);
    let mut workspace = EditorWorkspace::open_directory(&dir).unwrap();

    let mut external = project.clone();
    external.name = "clean external change with different length".to_string();
    external.save_to_path(&dir.join("project.ron")).unwrap();
    workspace.poll_project_watch(true);
    assert_eq!(workspace.project().name, external.name);
    assert!(!workspace.is_dirty());
    assert!(!workspace.has_external_project_conflict());

    workspace.project.name = "unsaved local name".to_string();
    workspace.mark_dirty();
    let mut second_external = external.clone();
    second_external.name = "second external edit with another length".to_string();
    second_external
        .save_to_path(&dir.join("project.ron"))
        .unwrap();
    assert!(workspace.save().unwrap_err().contains("changed outside"));
    assert_eq!(workspace.project().name, "unsaved local name");
    assert!(workspace.has_external_project_conflict());

    workspace.reload();
    assert_eq!(workspace.project().name, second_external.name);
    assert!(!workspace.has_external_project_conflict());
    let _ = std::fs::remove_dir_all(dir);
}

/// The conflict latch and the watch baseline have to survive being used over
/// and over, not just once.
///
/// Each round: dirty a local edit, change project.ron externally, watch the
/// Save get blocked, Reload to accept the disk version, edit again, and Save
/// cleanly. That last Save only works if `reload` recaptured the watch
/// baseline; if it did not, round two would latch on a phantom conflict and
/// the project would become unsaveable. Three rounds, because a latch that
/// only sticks on the second use would pass a single-cycle test.
///
/// The edits add brushes rather than renaming: a name change makes `save`
/// rename the project DIRECTORY, which is a different flow entirely.
#[test]
fn repeated_conflict_reload_save_cycles_keep_the_latch_and_baseline_honest() {
    let dir = test_temp_dir("project-watch-cycles");
    let project = ProjectDocument::new("cycle baseline");
    save_watch_project(&dir, &project);
    let mut workspace = EditorWorkspace::open_directory(&dir).unwrap();
    let path = dir.join("project.ron");

    let cuboid = |size: i32| psxed_project::brush::Brush::cuboid([0, 0, 0], [size, size, size]);
    let brush_count = |document: &ProjectDocument| document.active_scene().brushes.len();

    for round in 1..=3usize {
        // A local edit the author has not saved yet.
        let local_before = brush_count(workspace.project());
        workspace
            .project
            .active_scene_mut()
            .brushes
            .push(cuboid(64 + round as i32));
        workspace.mark_dirty();
        assert!(
            !workspace.has_external_project_conflict(),
            "round {round} started latched"
        );

        // Something outside the editor rewrites project.ron. Each round writes
        // a different number of brushes, so neither size nor hash can go stale.
        let mut external = project.clone();
        for extra in 0..round {
            external
                .active_scene_mut()
                .brushes
                .push(cuboid(512 + extra as i32));
        }
        external.save_to_path(&path).unwrap();

        // Save must refuse and must not touch the local document.
        let refusal = workspace.save().unwrap_err();
        assert!(
            refusal.contains("changed outside"),
            "round {round} refusal: {refusal}"
        );
        assert_eq!(
            brush_count(workspace.project()),
            local_before + 1,
            "round {round} lost the unsaved edit"
        );
        assert!(
            workspace.has_external_project_conflict(),
            "round {round} did not latch"
        );
        assert!(workspace.is_dirty());

        // Reload accepts the disk version and clears the latch.
        workspace.reload();
        assert_eq!(
            brush_count(workspace.project()),
            round,
            "round {round} did not adopt the external document"
        );
        assert!(
            !workspace.has_external_project_conflict(),
            "round {round} stayed latched after Reload"
        );

        // And the recaptured baseline lets the very next Save through.
        workspace
            .project
            .active_scene_mut()
            .brushes
            .push(cuboid(1024));
        workspace.mark_dirty();
        workspace
            .save()
            .unwrap_or_else(|error| panic!("round {round} Save after Reload: {error}"));
        assert!(!workspace.is_dirty());
        assert!(!workspace.has_external_project_conflict());

        // The saved bytes are on disk, and polling the freshly captured
        // baseline sees no phantom change.
        assert_eq!(
            brush_count(&ProjectDocument::load_from_path(&path).unwrap()),
            round + 1,
            "round {round} did not reach disk"
        );
        workspace.poll_project_watch(true);
        assert!(
            !workspace.has_external_project_conflict(),
            "round {round} polled a phantom conflict after Save"
        );
        assert_eq!(brush_count(workspace.project()), round + 1);
    }

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn same_length_atomic_project_replace_is_detected_even_when_metadata_looks_unchanged() {
    let dir = test_temp_dir("project-watch-atomic-same-length");
    let path = dir.join("project.ron");
    let project = ProjectDocument::new("baseline-name");
    save_watch_project(&dir, &project);
    let baseline_len = std::fs::metadata(&path).unwrap().len();
    let mut workspace = EditorWorkspace::open_directory(&dir).unwrap();

    let mut external = project.clone();
    external.name = "external-name".to_string();
    let replacement = dir.join("project.ron.atomic-replacement");
    external.save_to_path(&replacement).unwrap();
    assert_eq!(std::fs::metadata(&replacement).unwrap().len(), baseline_len);
    std::fs::rename(&replacement, &path).unwrap();
    // Model the hardest coarse-filesystem case: the watcher sees metadata
    // equal to its baseline. Only the always-bounded project hash can reveal
    // that the atomic replacement contains different bytes.
    workspace.project_watch.project.metadata = watched_file_metadata(&path);
    workspace.poll_project_watch(true);
    assert_eq!(workspace.project().name, external.name);
    assert!(!workspace.has_external_project_conflict());

    workspace.project.name = "local-dirty!".to_string();
    workspace.mark_dirty();
    let mut second_external = external.clone();
    second_external.name = "disk-changed!".to_string();
    second_external.save_to_path(&replacement).unwrap();
    assert_eq!(std::fs::metadata(&replacement).unwrap().len(), baseline_len);
    std::fs::rename(&replacement, &path).unwrap();
    workspace.project_watch.project.metadata = watched_file_metadata(&path);
    let error = workspace.save().unwrap_err();
    assert!(error.contains("changed outside"), "{error}");
    assert_eq!(workspace.project().name, "local-dirty!");
    assert!(workspace.has_external_project_conflict());
    assert_eq!(
        ProjectDocument::load_from_path(&path).unwrap().name,
        second_external.name
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn resource_watch_is_metadata_only_and_ignores_large_raw_source_resources() {
    let dir = test_temp_dir("resource-watch-scope");
    let runtime_path = dir.join("assets/sfx/hit.wav");
    let unreferenced_path = dir.join("source_assets/huge.glb");
    std::fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    std::fs::create_dir_all(unreferenced_path.parent().unwrap()).unwrap();
    std::fs::write(&runtime_path, b"runtime-audio").unwrap();
    std::fs::write(&unreferenced_path, vec![0x5a; 2 * 1024 * 1024]).unwrap();
    let mut project = ProjectDocument::new("watch resources");
    project.add_resource(
        "Hit",
        ResourceData::Audio {
            source_path: "assets/sfx/hit.wav".to_string(),
        },
    );
    project.add_resource(
        "Raw source mesh",
        ResourceData::Mesh {
            source_path: "source_assets/huge.glb".to_string(),
        },
    );
    save_watch_project(&dir, &project);
    let mut workspace = EditorWorkspace::open_directory(&dir).unwrap();
    assert!(workspace
        .project_watch
        .resources
        .contains_key(&runtime_path));
    assert!(!workspace
        .project_watch
        .resources
        .contains_key(&unreferenced_path));

    let baseline = workspace.project_watch.resources.clone();
    let old_status = workspace.status.clone();
    std::fs::write(&unreferenced_path, vec![0x33; 2 * 1024 * 1024 + 1]).unwrap();
    workspace.poll_project_watch(true);
    assert_eq!(workspace.project_watch.resources, baseline);
    assert_eq!(workspace.status, old_status);

    std::fs::write(&runtime_path, b"runtime-audio-changed").unwrap();
    workspace.poll_project_watch(true);
    assert_eq!(
        workspace.status_text(),
        "Reloaded externally changed project resources"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn referenced_runtime_resource_delete_recreate_and_atomic_replace_are_detected() {
    let dir = test_temp_dir("resource-watch-delete-recreate");
    let runtime_path = dir.join("assets/sfx/hit.wav");
    std::fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    std::fs::write(&runtime_path, b"runtime-audio-A").unwrap();
    let mut project = ProjectDocument::new("resource lifecycle");
    project.add_resource(
        "Hit",
        ResourceData::Audio {
            source_path: "assets/sfx/hit.wav".to_string(),
        },
    );
    save_watch_project(&dir, &project);
    let mut workspace = EditorWorkspace::open_directory(&dir).unwrap();
    let original = watched_file_metadata(&runtime_path).unwrap();

    std::fs::remove_file(&runtime_path).unwrap();
    workspace.poll_project_watch(true);
    assert_eq!(workspace.project_watch.resources[&runtime_path], None);
    assert_eq!(
        workspace.status_text(),
        "Reloaded externally changed project resources"
    );

    std::fs::write(&runtime_path, b"runtime-audio-B").unwrap();
    workspace.poll_project_watch(true);
    let recreated = workspace.project_watch.resources[&runtime_path].unwrap();
    assert_ne!(recreated, original, "recreated file identity must change");
    assert_eq!(
        workspace.status_text(),
        "Reloaded externally changed project resources"
    );

    let replacement = runtime_path.with_extension("atomic");
    std::fs::write(&replacement, b"runtime-audio-C").unwrap();
    assert_eq!(
        std::fs::metadata(&replacement).unwrap().len(),
        std::fs::metadata(&runtime_path).unwrap().len()
    );
    std::fs::rename(&replacement, &runtime_path).unwrap();
    let atomically_replaced = watched_file_metadata(&runtime_path).unwrap();
    assert_ne!(atomically_replaced, recreated);
    workspace.poll_project_watch(true);
    assert_eq!(
        workspace.project_watch.resources[&runtime_path],
        Some(atomically_replaced)
    );
    assert_eq!(
        workspace.status_text(),
        "Reloaded externally changed project resources"
    );
    let _ = std::fs::remove_dir_all(dir);
}

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
fn opening_legacy_default_bsp_top_view_frames_authored_brushes_without_dirtying() {
    let project_dir = test_temp_dir("editor-viewport-bsp-default-migration");
    let _ = std::fs::remove_dir_all(&project_dir);
    std::fs::create_dir_all(&project_dir).unwrap();
    let mut project = ProjectDocument::new("legacy default BSP viewport");
    project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [1024, 0, 2048],
            [3072, 512, 4096],
        ));
    project.editor_workspace.active = psxed_project::EditorWorkspaceView::Room;
    project.editor_viewport.view_2d = true;
    project.editor_viewport.orthographic_view = psxed_project::EditorOrthographicView::Top;
    project.editor_viewport.orthographic_focus = [0.0; 3];
    project.editor_viewport.viewport_zoom = DEFAULT_VIEWPORT_ZOOM;
    project
        .save_to_path(project_dir.join("project.ron"))
        .unwrap();

    let reopened = EditorWorkspace::open_directory(&project_dir).unwrap();
    assert!(
        !reopened.is_dirty(),
        "camera migration is editor-only state"
    );
    assert_eq!(reopened.orthographic_focus, [2048.0, 0.0, 3072.0]);
    assert!(reopened.viewport_zoom < DEFAULT_VIEWPORT_ZOOM);

    let _ = std::fs::remove_dir_all(project_dir);
}

#[test]
fn opening_legacy_default_bsp_camera_frames_above_authored_brushes_without_dirtying() {
    let project_dir = test_temp_dir("editor-camera-bsp-default-migration");
    let _ = std::fs::remove_dir_all(&project_dir);
    std::fs::create_dir_all(&project_dir).unwrap();
    let mut project = ProjectDocument::new("legacy default BSP camera");
    project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [0, 0, 0],
            [1024, 512, 768],
        ));
    project.editor_workspace.active = psxed_project::EditorWorkspaceView::Room;
    project.editor_viewport.view_2d = false;
    assert_eq!(project.editor_camera, EditorCameraState::default());
    project
        .save_to_path(project_dir.join("project.ron"))
        .unwrap();

    let reopened = EditorWorkspace::open_directory(&project_dir).unwrap();
    assert!(
        !reopened.is_dirty(),
        "camera migration is session-only state"
    );
    assert_eq!(reopened.camera_rig.target, [512, 256, 384]);
    assert_eq!(reopened.camera_rig.pitch, 3840);
    assert!(
        (512..6144).contains(&reopened.camera_rig.radius),
        "BSP bounds replace the legacy 6144-unit dolly"
    );
    assert_eq!(
        reopened.project().editor_camera,
        EditorCameraState::default(),
        "opening must not fabricate persisted camera authoring"
    );

    let _ = std::fs::remove_dir_all(project_dir);
}

#[test]
fn opening_bsp_project_preserves_a_custom_camera_exactly() {
    let project_dir = test_temp_dir("editor-camera-bsp-custom");
    let _ = std::fs::remove_dir_all(&project_dir);
    std::fs::create_dir_all(&project_dir).unwrap();
    let mut project = ProjectDocument::new("custom BSP camera");
    project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [0, 0, 0],
            [1024, 512, 768],
        ));
    project.editor_camera.orbit_yaw_q12 = 3072;
    project.editor_camera.orbit_pitch_q12 = 3665;
    project.editor_camera.orbit_radius = 550;
    project.editor_camera.orbit_target = [512, 64, 384];
    let expected = project.editor_camera;
    project
        .save_to_path(project_dir.join("project.ron"))
        .unwrap();

    let reopened = EditorWorkspace::open_directory(&project_dir).unwrap();
    assert!(!reopened.is_dirty());
    assert_eq!(reopened.current_editor_camera_state(), expected);
    assert_eq!(reopened.project().editor_camera, expected);

    let _ = std::fs::remove_dir_all(project_dir);
}

#[test]
fn opening_tracked_starter_preserves_its_authored_interior_camera() {
    let fixture_dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../projects/brush-first-playable");
    let authored = ProjectDocument::load_from_path(fixture_dir.join("project.ron"))
        .unwrap()
        .editor_camera;

    let reopened = EditorWorkspace::open_directory(&fixture_dir).unwrap();

    assert!(authored.orbit_radius > 0);
    assert_eq!(reopened.current_editor_camera_state(), authored);
    assert_eq!(reopened.project().editor_camera, authored);
    assert!(!reopened.is_dirty());
}

#[test]
fn showing_room_workspace_migrates_only_an_exact_default_bsp_camera() {
    let mut project = ProjectDocument::new("default BSP camera");
    project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [0, 0, 0],
            [1024, 512, 768],
        ));
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);

    workspace.show_workspace(psxed_project::EditorWorkspaceView::Room);

    assert_eq!(workspace.camera_rig.target, [512, 256, 384]);
    assert_eq!(workspace.camera_rig.pitch, 3840);
    assert!(!workspace.is_dirty());
    assert_eq!(
        workspace.project().editor_camera,
        EditorCameraState::default()
    );
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

/// A cook error raised by a per-kind cook helper (which only ever sees a node
/// NAME) still blames the authoring node, via the caller-side blame scope.
#[test]
fn cook_errors_blame_the_authoring_node_that_raised_them() {
    use psxed_project::playtest::PlaytestValidationTarget;

    // A Trigger Volume with no target is dead content and fails the cook.
    let template = psxed_project::new_project_template_dir();
    let dir = test_temp_dir("per-error-targets");
    crate::starter_catalogue::copy_dir_recursive(&template, &dir).unwrap();
    let mut workspace = EditorWorkspace::open_directory(&dir).unwrap();
    workspace.set_active_tool_cycle_value((ViewTool::Place, Some(PlaceKind::Logic)));
    assert!(workspace.place_bsp_from_top([1024.0, 1024.0]));
    let offender = workspace.selected_node_id();

    let project = workspace.project().clone();
    let (package, report) = psxed_project::playtest::build_package(&project, &dir);
    assert!(package.is_none(), "a targetless volume must fail the cook");
    let blamed: Vec<_> = report
        .errors
        .iter()
        .filter(|error| error.contains("has no target"))
        .map(|error| error.target)
        .collect();
    assert_eq!(
        blamed,
        vec![Some(PlaytestValidationTarget::Node(offender))],
        "the dead volume blames its own node"
    );
    assert_eq!(
        report.focus_target(),
        Some(PlaytestValidationTarget::Node(offender)),
        "the convenience accessor still returns the first focusable error"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// End to end: a failing cook driven through the editor's own cook command
/// auto-focuses the authored node responsible, and keeps every error's target
/// available so the diagnostics list can focus any row.
#[test]
fn failing_cook_auto_focuses_the_offending_node_and_keeps_every_target() {
    use psxed_project::playtest::PlaytestValidationTarget;

    let template = psxed_project::new_project_template_dir();
    let dir = test_temp_dir("failing-cook-focus");
    crate::starter_catalogue::copy_dir_recursive(&template, &dir).unwrap();
    let mut workspace = EditorWorkspace::open_directory(&dir).unwrap();
    workspace.set_active_tool_cycle_value((ViewTool::Place, Some(PlaceKind::Logic)));
    assert!(workspace.place_bsp_from_top([1024.0, 1024.0]));
    let offender = workspace.selected_node_id();
    workspace.replace_node_selection(workspace.project().active_scene().root);

    let cook = test_temp_dir("failing-cook-focus-out");
    let error = workspace
        .cook_playtest_to_dir(&cook)
        .expect_err("a targetless Trigger Volume must fail validation");
    assert!(error.contains("has no target"), "{error}");
    assert_eq!(
        workspace.selected_node_id(),
        offender,
        "the failing cook selected the node the author has to fix"
    );
    let targets: Vec<_> = workspace
        .last_cook_errors
        .iter()
        .map(|error| error.target)
        .collect();
    assert!(
        targets.contains(&Some(PlaytestValidationTarget::Node(offender))),
        "the retained diagnostics keep the offender focusable: {targets:?}"
    );

    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(cook);
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
    // Removes the created project even if an assertion below panics.
    let _scratch = ScratchProjectDir::new(target.clone());
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
fn bsp_new_project_can_author_save_cook_edit_and_recook_without_grid_rooms() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    let name = format!(
        "BSP Authoring Loop {} {}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let project_dir = psxed_project::projects_dir().join(psxed_project::project_file_stem(&name));
    // Removes the created project even if an assertion below panics.
    let _scratch = ScratchProjectDir::new(project_dir.clone());
    let cook_a = test_temp_dir("bsp-authoring-loop-a");
    let cook_b = test_temp_dir("bsp-authoring-loop-b");
    let cook_c = test_temp_dir("bsp-authoring-loop-c");
    let _ = std::fs::remove_dir_all(&project_dir);
    let _ = std::fs::remove_dir_all(&cook_a);
    let _ = std::fs::remove_dir_all(&cook_b);
    let _ = std::fs::remove_dir_all(&cook_c);

    workspace.create_and_open_project(&name).unwrap();
    assert!(workspace.active_room_id().is_none());
    assert_eq!(
        workspace.bsp_authoring_root(),
        Some(workspace.project().active_scene().root)
    );
    workspace.cycle_tool_group(false);
    assert_eq!(workspace.active_tool, ViewTool::Place);
    assert_eq!(workspace.place_kind, PlaceKind::PlayerSpawn);
    workspace.set_active_tool_cycle_value((ViewTool::Brush, None));

    // Remove the template spawn through the normal scene-tree command so the
    // BSP Place tool can prove it creates the replacement itself.
    let old_spawn = workspace
        .project()
        .active_scene()
        .nodes()
        .iter()
        .find(|node| matches!(node.kind, NodeKind::SpawnPoint { player: true, .. }))
        .expect("BSP template player spawn")
        .id;
    workspace.replace_node_selection(old_spawn);
    workspace.delete_selected();
    assert!(!workspace.has_player_source());

    // Real Brush commands: drag one solid, inherit the project's selected
    // material, then Hollow it into a closed six-slab room.
    let original_brushes = workspace.project().active_scene().brushes.len();
    let material = workspace.first_material().expect("template material");
    workspace.set_orthographic_view(OrthographicView::Top);
    // Author the enclosed test room directly on the courtyard's 64-unit
    // floor, then focus its new 80-unit interior floor for point placement.
    workspace.orthographic_focus[1] = 64.0;
    workspace.begin_brush_drag_2d([2048.0, 0.0]);
    workspace.update_brush_drag_2d([2560.0, 512.0]);
    workspace.commit_brush_drag();
    let created = workspace.selected_brush.expect("drag selected new brush");
    assert!(workspace.project().active_scene().brushes[created]
        .faces
        .iter()
        .all(|face| face.material == Some(material)));
    workspace.hollow_selected_brush(16);
    assert_eq!(
        workspace.project().active_scene().brushes.len(),
        original_brushes + 6
    );
    assert!(workspace.project().active_scene().brushes[created..]
        .iter()
        .flat_map(|brush| &brush.faces)
        .all(|face| face.material == Some(material)));
    workspace.orthographic_focus[1] = 80.0;

    // Real Place commands in a scene with no legacy Room/Section. The top
    // view resolves the new room's upward floor at Y=80 and lifts the spawn
    // one unit out of the solid boundary.
    workspace.set_active_tool_cycle_value((ViewTool::Place, Some(PlaceKind::PlayerSpawn)));
    workspace.handle_viewport_click([2304.0, 256.0], &[], egui::Modifiers::default());
    let spawn = workspace
        .project()
        .active_scene()
        .nodes()
        .iter()
        .find(|node| matches!(node.kind, NodeKind::SpawnPoint { player: true, .. }))
        .expect("placed BSP player spawn");
    assert_eq!(spawn.parent, Some(workspace.project().active_scene().root));
    assert_eq!(spawn.transform.translation, [2304.0, 81.0, 256.0]);

    workspace.set_active_tool_cycle_value((ViewTool::Place, Some(PlaceKind::PointLightMarker)));
    workspace.handle_viewport_click([2304.0, 384.0], &[], egui::Modifiers::default());
    assert!(workspace
        .project()
        .active_scene()
        .nodes()
        .iter()
        .any(|node| matches!(node.kind, NodeKind::PointLight { .. })
            && node.transform.translation == [2304.0, 336.0, 384.0]));

    workspace.save().expect("save authored BSP project");
    workspace.request_play_or_rebuild(EditorPlaytestStatus::Idle);
    assert_eq!(
        workspace.take_playtest_request(),
        Some(EditorPlaytestRequest::Play)
    );
    workspace
        .cook_playtest_to_dir(&cook_a)
        .expect("cook first authored revision");
    let first = std::fs::read(cook_a.join(psxed_project::brush_playtest::BRUSH_WORLD_FILENAME))
        .expect("first PXBSP");

    // Reopen the persisted project, perform another real Brush command, and
    // request the same Rebuild path used by Play while already running.
    let mut reopened = EditorWorkspace::open_directory(&project_dir).expect("reopen saved BSP");
    assert!(reopened.has_player_source());
    reopened.set_orthographic_view(OrthographicView::Top);
    reopened.begin_brush_drag_2d([2816.0, 0.0]);
    reopened.update_brush_drag_2d([2880.0, 64.0]);
    reopened.commit_brush_drag();
    reopened.save_if_dirty().expect("save changed BSP");
    reopened.request_play_or_rebuild(EditorPlaytestStatus::Running {
        input_captured: false,
    });
    assert_eq!(
        reopened.take_playtest_request(),
        Some(EditorPlaytestRequest::Rebuild)
    );
    reopened
        .cook_playtest_to_dir(&cook_b)
        .expect("cook edited revision");
    let edited = std::fs::read(cook_b.join(psxed_project::brush_playtest::BRUSH_WORLD_FILENAME))
        .expect("edited PXBSP");
    assert_ne!(first, edited, "the saved brush edit must reach the cook");

    // A second cook of unchanged saved data is byte deterministic.
    reopened
        .cook_playtest_to_dir(&cook_c)
        .expect("repeat unchanged cook");
    assert_eq!(
        edited,
        std::fs::read(cook_c.join(psxed_project::brush_playtest::BRUSH_WORLD_FILENAME))
            .expect("repeat PXBSP")
    );

    let _ = std::fs::remove_dir_all(project_dir);
    let _ = std::fs::remove_dir_all(cook_a);
    let _ = std::fs::remove_dir_all(cook_b);
    let _ = std::fs::remove_dir_all(cook_c);
}

fn action_bar_text_center(
    shapes: &[egui::epaint::ClippedShape],
    label: &str,
) -> Option<egui::Pos2> {
    fn find(shape: &egui::Shape, label: &str) -> Option<egui::Pos2> {
        match shape {
            egui::Shape::Text(text) if text.galley.text() == label => {
                Some(text.pos + text.galley.rect.center().to_vec2())
            }
            egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| find(shape, label)),
            _ => None,
        }
    }
    shapes.iter().find_map(|shape| find(&shape.shape, label))
}

fn click_real_egui_play_control(
    workspace: &mut EditorWorkspace,
    status: EditorPlaytestStatus,
    label: &str,
) {
    let ctx = egui::Context::default();
    let mut fonts = egui::FontDefinitions::default();
    let proportional = fonts
        .families
        .get(&egui::FontFamily::Proportional)
        .cloned()
        .expect("default proportional font family");
    fonts
        .families
        .insert(egui::FontFamily::Name("lucide".into()), proportional);
    ctx.set_fonts(fonts);
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 120.0));
    let input = |time, events| egui::RawInput {
        screen_rect: Some(screen),
        time: Some(time),
        events,
        ..egui::RawInput::default()
    };
    let draw = |ctx: &egui::Context, workspace: &mut EditorWorkspace| {
        egui::CentralPanel::default().show(ctx, |ui| {
            workspace.draw_build_play_controls(ui, status, None);
        });
    };
    let output = ctx.run(input(0.0, vec![]), |ctx| draw(ctx, workspace));
    let rendered = crate::icons::label(crate::icons::PLAY, label);
    let point = action_bar_text_center(&output.shapes, &rendered)
        .unwrap_or_else(|| panic!("action bar did not render {label:?}"));
    let _ = ctx.run(
        input(
            1.0 / 60.0,
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        ),
        |ctx| draw(ctx, workspace),
    );
    let _ = ctx.run(
        input(
            2.0 / 60.0,
            vec![egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        ),
        |ctx| draw(ctx, workspace),
    );
}

#[test]
fn play_and_rebuild_buttons_emit_requests_through_real_egui_input() {
    let mut workspace = EditorWorkspace::with_project(
        test_temp_dir("real-egui-play-control"),
        ProjectDocument::new("Real egui Play control"),
    );

    click_real_egui_play_control(&mut workspace, EditorPlaytestStatus::Idle, "Play");
    assert_eq!(
        workspace.take_playtest_request(),
        Some(EditorPlaytestRequest::Play)
    );

    click_real_egui_play_control(
        &mut workspace,
        EditorPlaytestStatus::Running {
            input_captured: false,
        },
        "Rebuild & Play",
    );
    assert_eq!(
        workspace.take_playtest_request(),
        Some(EditorPlaytestRequest::Rebuild)
    );
}

/// A Trigger Volume placed through the BSP Place lane must fire for a player
/// who simply walks onto the surface it was placed on.
///
/// Two defects made that impossible. The lane lifted every point entity one
/// unit above the surface, but the cook grows a trigger AABB UPWARD from its
/// anchor and the character motor stands its feet exactly on the floor plane,
/// so the volume started one unit above the player. And the placement default
/// was `wait_ticks: 0`, which re-fires every tick the player stands inside and
/// soft-locks whatever overlay the trigger opens.
#[test]
fn placed_trigger_volume_contains_a_player_standing_on_the_placement_surface() {
    let template = psxed_project::new_project_template_dir();
    let dir = test_temp_dir("trigger-anchor");
    crate::starter_catalogue::copy_dir_recursive(&template, &dir).unwrap();
    let mut workspace = EditorWorkspace::open_directory(&dir).unwrap();

    let surface_y = workspace
        .bsp_upward_surface_y([1024.0, 1024.0], workspace.orthographic_focus[1])
        .expect("template courtyard floor");
    workspace.set_active_tool_cycle_value((ViewTool::Place, Some(PlaceKind::Logic)));
    assert!(workspace.place_bsp_from_top([1024.0, 1024.0]));
    let placed = workspace.selected_node_id();

    let node = workspace
        .project()
        .active_scene()
        .node(placed)
        .expect("placed Logic node");
    assert_eq!(
        node.transform.translation[1], surface_y,
        "a Logic anchor sits ON the surface, never lifted above it"
    );
    let NodeKind::Logic {
        kind, wait_ticks, ..
    } = &node.kind
    else {
        panic!("Place must author a Logic node");
    };
    assert!(matches!(
        kind,
        psxed_project::LogicNodeKind::TriggerVolume { .. }
    ));
    assert_eq!(
        *wait_ticks, -1,
        "a freshly placed volume fires once, then retires"
    );

    // Give the volume a target so the cook accepts it, then prove the cooked
    // AABB contains a player whose feet rest on the placement surface.
    workspace.push_undo();
    if let Some(node) = workspace.project.active_scene_mut().node_mut(placed) {
        if let NodeKind::Logic { target, .. } = &mut node.kind {
            *target = "Anything".to_string();
        }
    }
    workspace.mark_dirty();

    let project = workspace.project().clone();
    let (package, report) = psxed_project::playtest::build_package(&project, &dir);
    assert!(report.is_ok(), "cook errors: {:?}", report.errors);
    let package = package.expect("cooks");
    let trigger = package
        .logic
        .iter()
        .find(|record| record.kind == psx_level::logic_kind::TRIGGER_VOLUME)
        .expect("cooked trigger volume");
    assert_eq!(trigger.wait_ticks, -1);
    let feet = surface_y.round() as i32;
    assert!(
        feet >= trigger.min[1] && feet <= trigger.max[1],
        "player feet at y={feet} must be inside the trigger's y span {}..={}",
        trigger.min[1],
        trigger.max[1]
    );

    // What the author sees must be where it fires. The gizmo is floor
    // anchored like the cooked box, so its drawn Y span matches the cooked
    // one; a centred gizmo would sit half the authored height too low.
    let node = workspace
        .project()
        .active_scene()
        .node(placed)
        .expect("placed Logic node");
    assert!(
        crate::editor_helpers::node_is_floor_anchored(&node.kind),
        "a Trigger Volume gizmo must be floor anchored to match its cooked AABB"
    );
    let (_, half) = crate::editor_helpers::entity_bound_kind_and_size(&workspace, node)
        .expect("trigger volumes have a preview bound");
    let drawn_min = node.transform.translation[1];
    let drawn_max = drawn_min + half[1] * 2.0;
    assert!(
        (drawn_min - trigger.min[1] as f32).abs() <= 1.0
            && (drawn_max - trigger.max[1] as f32).abs() <= 1.0,
        "gizmo y span {drawn_min}..={drawn_max} must match the cooked {}..={}",
        trigger.min[1],
        trigger.max[1]
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn bsp_blank_slate_commands_preserve_rooted_prop_door_and_portal_contract() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    let name = format!(
        "BSP Blank Slate {} {}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let project_dir = psxed_project::projects_dir().join(psxed_project::project_file_stem(&name));
    // Removes the created project even if an assertion below panics.
    let _scratch = ScratchProjectDir::new(project_dir.clone());
    let cook_a = test_temp_dir("bsp-blank-slate-a");
    let cook_b = test_temp_dir("bsp-blank-slate-b");
    let _ = std::fs::remove_dir_all(&project_dir);
    let _ = std::fs::remove_dir_all(&cook_a);
    let _ = std::fs::remove_dir_all(&cook_b);

    workspace.create_and_open_project(&name).unwrap();
    let root = workspace.project().active_scene().root;

    // Challenge the template-copy path from a genuine blank slate. Deleting
    // authored children and brushes uses the same selection commands as the
    // scene tree and brush viewport; the World root and Material resource
    // remain the project's ownership anchors.
    let children = workspace
        .project()
        .active_scene()
        .node(root)
        .expect("world root")
        .children
        .clone();
    for child in children {
        workspace.replace_node_selection(child);
        workspace.delete_selected();
    }
    while !workspace.project().active_scene().brushes.is_empty() {
        workspace.selected_brush = Some(0);
        workspace.delete_selected_brushes();
    }
    assert!(workspace.project().active_scene().brushes.is_empty());
    assert_eq!(workspace.bsp_authoring_root(), None);

    let material = workspace.first_material().expect("template material");
    workspace.set_orthographic_view(OrthographicView::Top);
    workspace.set_active_tool_cycle_value((ViewTool::Brush, None));
    workspace.begin_brush_drag_2d([0.0, 0.0]);
    // Keep the normal one-sector Box Prop well clear of the player so this
    // authored artifact can also prove movement after it reaches the guest.
    workspace.update_brush_drag_2d([2048.0, 2048.0]);
    workspace.commit_brush_drag();
    let solid = workspace.selected_brush.expect("new solid selected");
    workspace.hollow_selected_brush(64);
    assert_eq!(workspace.bsp_authoring_root(), Some(root));
    assert_eq!(workspace.project().active_scene().brushes.len(), 6);
    assert!(workspace.project().active_scene().brushes[solid..]
        .iter()
        .flat_map(|brush| &brush.faces)
        .all(|face| face.material == Some(material)
            && face.uv == psxed_project::brush::FaceUv::default()));

    // The BSP place lane must root every point/prop entity directly under the
    // World and preserve world-space coordinates. A portal request is rejected
    // without mutating or dirtying an otherwise saved document.
    workspace.set_active_tool_cycle_value((ViewTool::Place, Some(PlaceKind::PlayerSpawn)));
    assert!(workspace.place_bsp_from_top([192.0, 192.0]));
    let spawn = workspace.selected_node_id();
    workspace.set_active_tool_cycle_value((ViewTool::Place, Some(PlaceKind::PointLightMarker)));
    assert!(workspace.place_bsp_from_top([512.0, 512.0]));
    let light = workspace.selected_node_id();
    workspace.set_active_tool_cycle_value((ViewTool::Place, Some(PlaceKind::BoxProp)));
    // The courtyard intentionally offers two materials, so Box Prop placement
    // requires the same explicit material choice presented by the toolbar.
    workspace.replace_resource_selection(material);
    assert!(workspace.place_bsp_from_top([1536.0, 1536.0]));
    let prop = workspace.selected_node_id();
    for id in [spawn, light, prop] {
        assert_eq!(
            workspace.project().active_scene().node(id).unwrap().parent,
            Some(root)
        );
    }
    assert_eq!(
        workspace
            .project()
            .active_scene()
            .node(spawn)
            .unwrap()
            .transform
            .translation,
        [192.0, 65.0, 192.0]
    );
    workspace.save().expect("save before rejected portal");
    let before_nodes = workspace.project().active_scene().nodes().len();
    workspace.set_active_tool_cycle_value((ViewTool::Place, Some(PlaceKind::Portal)));
    assert!(!workspace.place_bsp_from_top([1024.0, 1024.0]));
    assert_eq!(
        workspace.project().active_scene().nodes().len(),
        before_nodes
    );
    assert!(!workspace.is_dirty(), "rejected BSP portal must not dirty");

    // Place the normal default Logic node, then perform the exact kind change
    // exposed by its inspector before binding one hollowed wall as its model.
    workspace.set_active_tool_cycle_value((ViewTool::Place, Some(PlaceKind::Logic)));
    assert!(workspace.place_bsp_from_top([64.0, 1024.0]));
    let door = workspace.selected_node_id();
    workspace.push_undo();
    let node = workspace
        .project
        .active_scene_mut()
        .node_mut(door)
        .expect("placed Logic node");
    node.name = "Simple Door".to_string();
    node.kind = NodeKind::Logic {
        kind: psxed_project::LogicNodeKind::Door {
            box_prop: String::new(),
            start_open: false,
            open_offset: [0, 192, 0],
            travel_ticks: 60,
        },
        target: String::new(),
        killtarget: String::new(),
        master: String::new(),
        delay_ticks: 0,
        wait_ticks: 0,
        enabled: true,
    };
    workspace.mark_dirty();
    workspace.selected_brush = Some(2);
    workspace.set_selected_brush_mover(Some(door));
    assert_eq!(
        workspace.project().active_scene().brushes[2].mover,
        Some(door)
    );

    workspace.save_if_dirty().expect("persist full BSP project");
    workspace.request_play_or_rebuild(EditorPlaytestStatus::Idle);
    assert_eq!(
        workspace.take_playtest_request(),
        Some(EditorPlaytestRequest::Play)
    );
    workspace
        .cook_playtest_to_dir(&cook_a)
        .expect("cook rooted prop and door");
    let project = ProjectDocument::load_from_path(project_dir.join("project.ron")).unwrap();
    let (package, report) = psxed_project::playtest::build_package(&project, &project_dir);
    assert!(report.is_ok(), "{}", report.error_messages().join("; "));
    let package = package.expect("cooked package");
    assert_eq!(package.box_props.len(), 1);
    assert_ne!(
        package.box_props[0].flags & psx_level::box_prop_flags::COLLISION_ENABLED,
        0,
        "the authored Box Prop must retain its collision contract"
    );
    assert_eq!(package.lights.len(), 1);
    assert!(matches!(
        package.world_geometry,
        psxed_project::playtest::PlaytestWorldGeometry::Pxbsp(ref world)
            if world.movers.len() == 1 && world.movers[0].node == door.raw() as u32
    ));

    // Rebuild an unchanged persisted revision into a separate directory and
    // compare both authoritative generated files byte-for-byte.
    workspace.request_play_or_rebuild(EditorPlaytestStatus::Running {
        input_captured: false,
    });
    assert_eq!(
        workspace.take_playtest_request(),
        Some(EditorPlaytestRequest::Rebuild)
    );
    workspace
        .cook_playtest_to_dir(&cook_b)
        .expect("deterministic BSP rebuild");
    for filename in [
        psxed_project::brush_playtest::BRUSH_WORLD_FILENAME,
        psxed_project::playtest::COOKED_MANIFEST_FILENAME,
    ] {
        assert_eq!(
            std::fs::read(cook_a.join(filename)).unwrap(),
            std::fs::read(cook_b.join(filename)).unwrap(),
            "{filename} drifted across unchanged Rebuild"
        );
    }

    // The opt-in acceptance gate consumes the exact project authored above,
    // rather than regenerating a lookalike through a second construction
    // path. Normal unit-test runs leave no artifact behind. The Make target
    // supplies a fresh temporary destination and carries this persisted
    // project through the real cook, MIPS link, disc build, and emulator boot.
    if let Some(export_dir) = std::env::var_os("PSOXIDE_EDITOR_BLANK_PROJECT_OUT") {
        let export_dir = PathBuf::from(export_dir);
        assert!(
            !export_dir.exists(),
            "blank-slate export destination already exists: {}",
            export_dir.display()
        );
        copy_dir_recursive(&project_dir, &export_dir).expect("export authored BSP project");
        let export_file = export_dir.join("project.ron");
        let mut exported = ProjectDocument::load_from_path(&export_file).expect("load export");
        exported.name = "Editor Blank Playtest Acceptance".to_string();
        exported
            .save_to_path(&export_file)
            .expect("stabilise exported acceptance project name");
        println!("blank-slate project: {}", export_file.display());
    }

    let _ = std::fs::remove_dir_all(project_dir);
    let _ = std::fs::remove_dir_all(cook_a);
    let _ = std::fs::remove_dir_all(cook_b);
}

#[test]
fn new_project_release_choice_copies_the_roofless_open_courtyard() {
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
    // Removes the created project even if an assertion below panics.
    let _scratch = ScratchProjectDir::new(target.clone());
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
    assert_eq!(workspace.project().active_scene().brushes.len(), 5);
    assert!(
        !workspace.is_dirty(),
        "the framed new project is already saved"
    );
    assert!(workspace.view_2d);
    assert_eq!(workspace.orthographic_view, OrthographicView::Top);
    assert_eq!(workspace.active_tool, ViewTool::Brush);
    assert!(
        workspace.viewport_zoom < DEFAULT_VIEWPORT_ZOOM,
        "the starter BSP must be framed instead of opening at 96 px/unit"
    );
    assert_eq!(workspace.orthographic_focus, [8256.0, 0.0, 8256.0]);
    let framed_span =
        psxed_project::NEW_PROJECT_COURTYARD_OUTER_SIZE as f32 * workspace.viewport_zoom;
    assert!(
        framed_span <= workspace.last_viewport_size.x.max(320.0) * 0.72 + 0.01
            && framed_span <= workspace.last_viewport_size.y.max(240.0) * 0.72 + 0.01,
        "the entire courtyard must fit inside the initial Top frame"
    );
    assert!(
        64.0 * workspace.viewport_zoom >= 2.0,
        "the 64-unit perimeter must remain visibly thicker than two pixels"
    );
    let saved = ProjectDocument::load_from_path(target.join("project.ron")).unwrap();
    assert_eq!(
        saved.bsp_cook_mode,
        psxed_project::brush_world::BrushWorldCookMode::Release
    );
    assert!(saved.editor_viewport.view_2d);
    assert_eq!(
        saved.editor_viewport.orthographic_view,
        psxed_project::EditorOrthographicView::Top
    );
    assert_eq!(saved.editor_viewport.viewport_zoom, workspace.viewport_zoom);
    assert_eq!(saved.active_scene().brushes.len(), 5);
    assert_eq!(saved.editor_camera.orbit_target, [8256, 160, 8256]);
    assert_eq!(saved.editor_camera.orbit_radius, 18_000);
    assert_eq!(saved.editor_camera.orbit_yaw_q12, 3584);
    assert_eq!(saved.editor_camera.orbit_pitch_q12, 3712);
    let material_paths = saved
        .resources
        .iter()
        .filter_map(|resource| match &resource.data {
            psxed_project::ResourceData::Material(material) => material.psxt_path.as_deref(),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        material_paths,
        [
            "assets/textures/courtyard_cobbles.psxt",
            "assets/textures/courtyard_brick.psxt"
        ]
    );
    let template = psxed_project::new_project_template_dir();
    for relative in [
        "assets/textures/courtyard_cobbles.psxt",
        "assets/textures/courtyard_brick.psxt",
    ] {
        assert_eq!(
            std::fs::read(target.join(relative)).unwrap(),
            std::fs::read(template.join(relative)).unwrap(),
            "New Project did not copy {relative} byte-for-byte"
        );
    }
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
fn save_renaming_bsp_starter_creates_a_project_copy() {
    let source = psxed_project::new_project_template_dir();
    let source_project = source.join("project.ron");
    let source_bytes = std::fs::read(&source_project).unwrap();
    let name = format!(
        "BSP Starter Copy {} {}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let target = psxed_project::projects_dir().join(psxed_project::project_file_stem(&name));
    // Removes the created project even if an assertion below panics.
    let _scratch = ScratchProjectDir::new(target.clone());
    let _ = std::fs::remove_dir_all(&target);
    let mut workspace = EditorWorkspace::open_directory(&source).unwrap();

    workspace.project.name = name.clone();
    workspace.mark_dirty();
    workspace.save().unwrap();

    assert!(source_project.is_file());
    assert_eq!(std::fs::read(&source_project).unwrap(), source_bytes);
    assert!(target.join("project.ron").is_file());
    assert!(paths_equivalent(workspace.project_root(), &target));
    assert_eq!(
        ProjectDocument::load_from_path(target.join("project.ron"))
            .unwrap()
            .name,
        name
    );
    let _ = std::fs::remove_dir_all(target);
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
fn delete_current_project_refuses_bundled_projects() {
    for project_dir in [
        psxed_project::default_project_dir(),
        psxed_project::new_project_template_dir(),
    ] {
        let mut workspace = EditorWorkspace::open_directory(&project_dir).unwrap();

        let error = workspace.delete_current_project().unwrap_err();

        assert!(error.contains("Bundled starter"), "{error}");
        assert!(project_dir.join("project.ron").is_file());
    }
}

#[test]
fn delete_current_project_removes_directory_and_loads_bsp_starter() {
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
    // Removes the created project even if an assertion below panics.
    let _scratch = ScratchProjectDir::new(target.clone());
    let _ = std::fs::remove_dir_all(&target);

    workspace.create_and_open_project(&name).unwrap();
    assert!(target.join("project.ron").is_file());

    workspace.delete_current_project().unwrap();

    assert!(!target.exists());
    assert!(paths_equivalent(
        workspace.project_root(),
        &psxed_project::new_project_template_dir()
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
    // Removes the created project even if an assertion below panics.
    let _scratch = ScratchProjectDir::new(target.clone());
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

/// The verified combat loadout must survive the production "Starter
/// Characters" sync into a genuinely fresh New Project: sword weapon
/// resources and their assets arrive byte-for-byte, the Aletha and Rust
/// Mantis profiles carry the measured combat capsules, and every synced
/// reference resolves inside the target project (a profile whose model or
/// animation set points at a resource id the sync never copied is exactly
/// the dangling-reference failure this test exists to catch).
#[test]
fn starter_character_sync_arms_a_new_project_with_verified_combat_content() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    let name = format!(
        "Starter Combat Sync {} {}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let project_dir = psxed_project::projects_dir().join(psxed_project::project_file_stem(&name));
    // Removes the created project even if an assertion below panics.
    let _scratch = ScratchProjectDir::new(project_dir.clone());
    let _ = std::fs::remove_dir_all(&project_dir);

    workspace.create_and_open_project(&name).unwrap();
    assert!(
        !workspace
            .project()
            .resources
            .iter()
            .any(|resource| matches!(resource.data, ResourceData::Character(_))),
        "the BSP starter template must arrive character-free"
    );

    // The exact body of the resources-panel "Starter Characters" action.
    workspace.push_undo();
    let target_dir = workspace.project_dir.clone();
    let report = sync_starter_character_catalogue(&mut workspace.project, &target_dir)
        .expect("starter character sync");
    assert!(report.changed());
    workspace.mark_dirty();
    workspace.save().expect("save synced project");

    // Weapon + character assets land as byte-copies of the tracked default
    // project files (themselves byte-copies of the verified cortex_v1 sample).
    let default_root = psxed_project::default_project_dir();
    for relative in [
        "assets/models/sword1_light/sword1_light.psxmdl",
        "assets/models/sword1_light/sword1_light.psxt",
        "assets/models/sword1_light/prop_bind_pose.psxanim",
        "assets/models/sword1_heavy/sword1_heavy.psxmdl",
        "assets/models/ci_player/ci_player.psxmdl",
        "assets/models/rust_mantis/rust_mantis.psxmdl",
        "assets/animations/ci_player_complete/light_attack.psxanim",
        "assets/animations/rust_mantis_starter/idle.psxanim",
    ] {
        assert_eq!(
            std::fs::read(project_dir.join(relative)).unwrap(),
            std::fs::read(default_root.join(relative)).unwrap(),
            "sync did not copy {relative} byte-for-byte"
        );
    }

    let project = workspace.project();
    let resource_by_name = |name: &str, wants: fn(&ResourceData) -> bool| {
        project
            .resources
            .iter()
            .find(|resource| resource.name == name && wants(&resource.data))
            .unwrap_or_else(|| panic!("synced project is missing '{name}'"))
    };

    // Full referential integrity: after syncing into a genuinely fresh
    // project every cook-relevant resource reference must resolve. Only
    // `AnimationClip::source` / `AnimationSource` provenance is exempt: the
    // catalogue deliberately does not carry authoring sources, and the cook
    // never reads them.
    for resource in &project.resources {
        let mut check = |label: &str, reference: Option<psxed_project::ResourceId>| {
            if let Some(id) = reference {
                assert!(
                    project.resource(id).is_some(),
                    "synced '{}' has a dangling {label} reference #{}",
                    resource.name,
                    id.raw()
                );
            }
        };
        match &resource.data {
            ResourceData::Character(character) => {
                check("model", character.model);
                check("material", character.material);
                check("animation set", character.animation_set);
            }
            ResourceData::Model(model) => check("skeleton", model.skeleton),
            ResourceData::Weapon(weapon) => check("weapon model", weapon.model),
            ResourceData::AnimationClip(clip) => {
                check("skeleton", clip.skeleton);
                check("target model", clip.target_model);
            }
            ResourceData::AnimationSet(set) => {
                check("skeleton", set.skeleton);
                for binding in &set.action_clips {
                    check("action clip", Some(binding.clip));
                }
                for clip in &set.clips {
                    check("library clip", Some(*clip));
                }
            }
            _ => {}
        }
    }

    // The verified Aletha loadout: measured capsules, ci_player model with
    // the joint-13 grip socket, the complete action set, and the crystal
    // covering material.
    let aletha = resource_by_name("Aletha", |data| matches!(data, ResourceData::Character(_)));
    let ResourceData::Character(aletha) = &aletha.data else {
        unreachable!();
    };
    assert_eq!(aletha.combat_capsules.len(), 4);
    let capsule = |index: usize| &aletha.combat_capsules[index];
    assert_eq!(capsule(0).name, "Torso Hurtbox");
    assert_eq!(capsule(0).joint, 3);
    assert_eq!(capsule(0).capsule.radius, 180);
    assert_eq!(capsule(0).role, psxed_project::CombatCapsuleRole::Hurtbox);
    for (index, action, window, damage, poise) in [
        (
            1,
            psxed_project::CharacterAnimationAction::LightAttack,
            (12, 15),
            25,
            25,
        ),
        (
            2,
            psxed_project::CharacterAnimationAction::HeavyAttack,
            (11, 14),
            38,
            50,
        ),
        (
            3,
            psxed_project::CharacterAnimationAction::ComboAttack,
            (12, 25),
            30,
            30,
        ),
    ] {
        assert_eq!(capsule(index).joint, 13);
        assert_eq!(capsule(index).capsule.radius, 72);
        assert_eq!(
            capsule(index).role,
            psxed_project::CombatCapsuleRole::Hitbox {
                action,
                active_start_frame: window.0,
                active_end_frame: window.1,
                damage,
                poise_damage: poise,
            }
        );
    }
    let aletha_model = project.resource(aletha.model.unwrap()).unwrap();
    let ResourceData::Model(aletha_model_data) = &aletha_model.data else {
        panic!("Aletha model reference is not a Model");
    };
    assert_eq!(
        aletha_model_data.model_path,
        "assets/models/ci_player/ci_player.psxmdl"
    );
    let socket = aletha_model_data
        .attachments
        .iter()
        .find(|socket| socket.name == "right_hand_grip")
        .expect("Aletha model carries the right_hand_grip socket");
    assert_eq!(socket.joint, 13);
    let aletha_set = project.resource(aletha.animation_set.unwrap()).unwrap();
    assert_eq!(aletha_set.name, "Aletha Complete Animation Set");
    let aletha_material = project.resource(aletha.material.unwrap()).unwrap();
    assert_eq!(aletha_material.name, "Aletha Crystal");

    // The verified Mantis enemy: torso hurtbox only (reciprocal damage is the
    // documented legacy-arc fallback), armed model socket for equipment.
    let mantis = resource_by_name("Rust Mantis Enemy", |data| {
        matches!(data, ResourceData::Character(_))
    });
    let ResourceData::Character(mantis) = &mantis.data else {
        unreachable!();
    };
    assert_eq!(mantis.combat_capsules.len(), 1);
    assert_eq!(mantis.combat_capsules[0].name, "Torso Hurtbox");
    assert_eq!(mantis.combat_capsules[0].joint, 3);
    assert_eq!(mantis.combat_capsules[0].capsule.radius, 184);
    assert_eq!(
        mantis.combat_capsules[0].role,
        psxed_project::CombatCapsuleRole::Hurtbox
    );
    assert_eq!(mantis.spawn_role, psxed_project::CharacterSpawnRole::Enemy);
    let behavior = mantis.enemy_behavior.expect("mantis enemy behavior");
    assert_eq!(behavior.poise, 100);
    assert_eq!(behavior.max_health, 100);
    assert_eq!(behavior.touch_damage, 10);
    let mantis_model = project.resource(mantis.model.unwrap()).unwrap();
    let ResourceData::Model(mantis_model_data) = &mantis_model.data else {
        panic!("Mantis model reference is not a Model");
    };
    assert!(mantis_model_data
        .attachments
        .iter()
        .any(|socket| socket.name == "right_hand_grip" && socket.joint == 13));

    // The verified sword weapons, grips exact (scale-specific measured
    // translations), each resolving to a synced Model resource.
    for (name, grip_translation, hitboxes) in
        [("Sword1 Light", [0, 15077, 0], 1), ("Sword1 Heavy", [0, 18462, 0], 0)]
    {
        assert!(STARTER_WEAPON_NAMES.contains(&name));
        let weapon = resource_by_name(name, |data| matches!(data, ResourceData::Weapon(_)));
        let ResourceData::Weapon(weapon) = &weapon.data else {
            unreachable!();
        };
        assert_eq!(weapon.default_character_socket, "right_hand_grip");
        assert_eq!(weapon.grip.name, "grip");
        assert_eq!(weapon.grip.translation, grip_translation);
        assert_eq!(weapon.grip.rotation_q12, [0, -1024, 0]);
        assert_eq!(weapon.hitboxes.len(), hitboxes);
        assert_eq!(weapon.arc_reach, 640);
        assert_eq!(weapon.damage, 25);
        let model = project
            .resource(weapon.model.expect("starter weapon has a model"))
            .unwrap_or_else(|| panic!("weapon '{name}' has a dangling model reference"));
        assert!(matches!(model.data, ResourceData::Model(_)));
        assert_eq!(model.name, name);
    }
    let light_weapon = resource_by_name("Sword1 Light", |data| {
        matches!(data, ResourceData::Weapon(_))
    });
    let ResourceData::Weapon(light_weapon) = &light_weapon.data else {
        unreachable!();
    };
    assert_eq!(light_weapon.hitboxes[0].active_start_frame, 12);
    assert_eq!(light_weapon.hitboxes[0].active_end_frame, 15);

    // A second sync of an already-armed project is a no-op: the catalogue
    // converged instead of duplicating resources or rewriting files.
    let resources_before = workspace.project().resources.len();
    let repeat = sync_starter_character_catalogue(&mut workspace.project, &target_dir)
        .expect("repeat starter character sync");
    assert!(
        !repeat.changed(),
        "repeat sync must be a no-op (added {}, updated {}, removed {}, copied {}, deleted {})",
        repeat.resources_added,
        repeat.resources_updated,
        repeat.resources_removed,
        repeat.files_copied,
        repeat.files_removed
    );
    assert_eq!(workspace.project().resources.len(), resources_before);

    let _ = std::fs::remove_dir_all(project_dir);
}

/// Souls vertical-slice tape: authored as literals so regeneration is
/// deterministic and drift-diffable. The canonical route: touch the
/// checkpoint trigger, dismiss the sync overlay, open the lift door, kill
/// the Mantis with the verified combo/heavy cadence while taking hits,
/// walk into the lava pool, die, respawn at the checkpoint, dismiss the
/// re-fired sync overlay, and walk a short confirmation leg.
///
/// The tapes are indexed on the PAD-POLL clock, not the video-frame clock.
/// A video-frame tape is applied on the emulator's route-tick clock while
/// the guest reads the pad once per fixed simulation tick, and those two
/// clocks are not phase-locked: their relative phase drifts with guest
/// execution cost, which changes with guest code layout. Measured on this
/// exact route (2026-08-11), the same authored 70-frame doorway retreat
/// reached one guest as 71 held simulation ticks and another as 70, and
/// that one extra tick of backward movement is the difference between the
/// fourth heavy swing reaching the Mantis and missing it. `pad_poll` binds
/// sample N to poll N, so the guest sees the authored press windows exactly,
/// whatever the frame rate.
fn write_souls_slice_canonical_tape(dir: &Path) {
    const UP: u16 = 1 << 4;
    const DOWN: u16 = 1 << 6;
    const CROSS: u16 = 1 << 14;
    const R2: u16 = 1 << 9;
    const L2: u16 = 1 << 8;
    const R3: u16 = 1 << 2;
    const FRAME_COUNT: usize = 3000;
    // Combo (L2) opener pair, then a heavy (R2) tail; spacing mirrors the
    // verified combat fixture (37-tick tail, co-prime with the enemy's
    // 45-tick attack cadence).
    const COMBO_PRESSES: [usize; 2] = [1250, 1390];
    const HEAVY_PRESSES: [usize; 7] = [
        1320, 1450, 1487, 1524, 1561, 1598, 1635,
    ];

    let mut tape = String::with_capacity(FRAME_COUNT * 32);
    use std::fmt::Write as _;
    writeln!(tape, "psoxide-tape,v2,clock=pad_poll,start_poll=0").unwrap();
    writeln!(tape, "frame,buttons,right_x,right_y,left_x,left_y").unwrap();
    for frame in 0..FRAME_COUNT {
        let mut buttons = 0u16;
        // Leg 1: spawn to the checkpoint trigger.
        if (240..430).contains(&frame) {
            buttons |= UP;
        }
        // Dismiss the SYNC RELAY overlay the trigger opened.
        if (450..454).contains(&frame) {
            buttons |= CROSS;
        }
        // Leg 2: trigger to the closed lift door.
        if (470..840).contains(&frame) {
            buttons |= UP;
        }
        // Open the door, wait out its travel, walk through to the fight.
        if (860..864).contains(&frame) {
            buttons |= CROSS;
        }
        // Step into the far room to trip the Mantis aggro, then retreat
        // into the doorway pinch so the fight happens in the frame, the
        // verified fixture pattern. The retreat is 71 ticks, not the 70
        // this leg was authored with: on the video-frame clock the guest
        // that produced the authored outcome had been handed 71 held
        // ticks by clock drift, and 70 leaves the fourth heavy swing
        // short of the Mantis. Moving the tape to the pad-poll clock
        // makes the count exact, so the authored number is the one the
        // route actually needs. Anywhere in 1091..=1112 kills the Mantis;
        // the lava leg that follows is an open-loop stick walk and only
        // reaches the pool from this standoff pose.
        if (940..1010).contains(&frame) {
            buttons |= UP;
        }
        if (1020..1091).contains(&frame) {
            buttons |= DOWN;
        }
        // Lock on to the arriving Mantis so every authored swing faces it.
        if (1100..1104).contains(&frame) {
            buttons |= R3;
        }
        if COMBO_PRESSES
            .iter()
            .any(|press| (*press..press + 4).contains(&frame))
        {
            buttons |= L2;
        }
        if HEAVY_PRESSES
            .iter()
            .any(|press| (*press..press + 4).contains(&frame))
        {
            buttons |= R2;
        }
        // Leg 3: forward-east through the reopened doorway (the fight
        // drifted the pinch back into room one), then strafe-right south
        // into the lava pool; stop inside and die to the 15-tick cadence.
        let mut left_x = 128u8;
        let mut left_y = 128u8;
        if (1750..1880).contains(&frame) {
            left_x = 224;
            left_y = 32;
        }
        if (1880..1960).contains(&frame) {
            left_x = 224;
        }
        // Leg 4: post-respawn overlay dismissal and confirmation walk.
        if (2400..2404).contains(&frame) {
            buttons |= CROSS;
        }
        if (2450..2560).contains(&frame) {
            buttons |= DOWN;
        }
        writeln!(tape, "{frame},{buttons},128,128,{left_x},{left_y}").unwrap();
    }
    std::fs::write(dir.join("souls-canonical.pxitape.csv"), tape)
        .expect("write souls canonical tape");
}

/// Negative tape: the pad stays neutral for the whole run. The player never
/// touches the trigger, never opens the door, never swings; the gate pins
/// every combat and progression counter to zero while PVS suppressions
/// still accumulate from the enemy sealed in the far room.
fn write_souls_slice_negative_tape(dir: &Path) {
    const FRAME_COUNT: usize = 900;
    let mut tape = String::with_capacity(FRAME_COUNT * 24);
    use std::fmt::Write as _;
    // Pad-poll clock for the same reason as the canonical tape: the guest's
    // own input clock is the only one that is build-independent.
    writeln!(tape, "psoxide-tape,v2,clock=pad_poll,start_poll=0").unwrap();
    writeln!(tape, "frame,buttons,right_x,right_y,left_x,left_y").unwrap();
    for frame in 0..FRAME_COUNT {
        writeln!(tape, "{frame},0,128,128,128,128").unwrap();
    }
    std::fs::write(dir.join("souls-negative.pxitape.csv"), tape)
        .expect("write souls negative tape");
}

/// The souls vertical slice authored end to end through production editor
/// command paths: New Project from the courtyard template, the resources
/// panel starter sync, real brush drags/resizes, the BSP place lanes, the
/// scene-tree Add Child component path, and the exact inspector mutations.
/// Save, reopen, cook (twice, byte-deterministic), and verify the cooked
/// world: nonzero PXBSP, the two authored body-hull envelopes, the door
/// mover, both equipment records, and the trigger-to-checkpoint chain.
/// With PSOXIDE_SOULS_SLICE_PROJECT_OUT set, exports the authored project
/// (plus the canonical and negative tapes) for the tracked copy at
/// editor/projects/souls-bsp-vertical-slice.
#[test]
fn souls_slice_project_is_authored_through_production_commands() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    let name = format!(
        "Souls Slice Authoring {} {}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let project_dir = psxed_project::projects_dir().join(psxed_project::project_file_stem(&name));
    // Removes the created project even if an assertion below panics.
    let _scratch = ScratchProjectDir::new(project_dir.clone());
    let cook_a = test_temp_dir("souls-slice-cook-a");
    let cook_b = test_temp_dir("souls-slice-cook-b");
    let _ = std::fs::remove_dir_all(&project_dir);
    let _ = std::fs::remove_dir_all(&cook_a);
    let _ = std::fs::remove_dir_all(&cook_b);

    workspace.create_and_open_project(&name).unwrap();
    let root = workspace.project().active_scene().root;

    // The resources-panel "Starter Characters" action arms the fresh project
    // with the verified combat catalogue.
    workspace.push_undo();
    let target_dir = workspace.project_dir.clone();
    sync_starter_character_catalogue(&mut workspace.project, &target_dir)
        .expect("starter character sync");
    workspace.mark_dirty();

    let resource_id = |workspace: &EditorWorkspace, name: &str, wants: fn(&ResourceData) -> bool| {
        workspace
            .project()
            .resources
            .iter()
            .find(|resource| resource.name == name && wants(&resource.data))
            .unwrap_or_else(|| panic!("missing resource '{name}'"))
            .id
    };
    let cobbles = resource_id(&workspace, "Courtyard Cobbles", |data| {
        matches!(data, ResourceData::Material(_))
    });
    let brick = resource_id(&workspace, "Courtyard Brick", |data| {
        matches!(data, ResourceData::Material(_))
    });
    let aletha_profile = resource_id(&workspace, "Aletha", |data| {
        matches!(data, ResourceData::Character(_))
    });
    let mantis_profile = resource_id(&workspace, "Rust Mantis Enemy", |data| {
        matches!(data, ResourceData::Character(_))
    });
    let sword_light = resource_id(&workspace, "Sword1 Light", |data| {
        matches!(data, ResourceData::Weapon(_))
    });
    let sword_heavy = resource_id(&workspace, "Sword1 Heavy", |data| {
        matches!(data, ResourceData::Weapon(_))
    });

    // Blank slate: the courtyard template's nodes and brushes leave through
    // the same selection commands the scene tree and brush viewport use.
    let children = workspace
        .project()
        .active_scene()
        .node(root)
        .expect("world root")
        .children
        .clone();
    for child in children {
        workspace.replace_node_selection(child);
        workspace.delete_selected();
    }
    while !workspace.project().active_scene().brushes.is_empty() {
        workspace.selected_brush = Some(0);
        workspace.delete_selected_brushes();
    }
    assert!(workspace.project().active_scene().brushes.is_empty());

    // Author the two combat spaces: a sealed 8192 x 3072 envelope with a
    // 1536-unit interior height, split by a thin divider whose doorway gap
    // carries the lift-door brush; a lava pool sits in the far room.
    workspace.set_orthographic_view(OrthographicView::Top);
    workspace.set_active_tool_cycle_value((ViewTool::Brush, None));
    let mut author_box = |workspace: &mut EditorWorkspace,
                          material: ResourceId,
                          mins: [i32; 3],
                          maxs: [i32; 3]|
     -> usize {
        workspace.brush_material = Some(material);
        workspace.orthographic_focus[1] = mins[1] as f32;
        workspace.begin_brush_drag_2d([mins[0] as f32, mins[2] as f32]);
        workspace.update_brush_drag_2d([maxs[0] as f32, maxs[2] as f32]);
        workspace.commit_brush_drag();
        let created = workspace.selected_brush.expect("committed brush");
        assert!(
            workspace.set_selected_brush_size([
                maxs[0] - mins[0],
                maxs[1] - mins[1],
                maxs[2] - mins[2],
            ]),
            "brush resize {mins:?} -> {maxs:?}"
        );
        created
    };
    author_box(&mut workspace, cobbles, [0, 0, 0], [8192, 256, 3072]);
    author_box(&mut workspace, cobbles, [0, 1792, 0], [8192, 2048, 3072]);
    author_box(&mut workspace, brick, [0, 256, 0], [256, 1792, 3072]);
    author_box(&mut workspace, brick, [7936, 256, 0], [8192, 1792, 3072]);
    author_box(&mut workspace, brick, [256, 256, 0], [7936, 1792, 256]);
    author_box(&mut workspace, brick, [256, 256, 2816], [7936, 1792, 3072]);
    author_box(&mut workspace, brick, [4064, 256, 256], [4128, 1792, 1280]);
    author_box(&mut workspace, brick, [4064, 256, 1792], [4128, 1792, 2816]);
    // Interior stub in the far room: movers never enter the static PVS, so
    // the doorway is a permanent visibility hole; this wall seals a pocket
    // no room-one leaf can see into, guaranteeing the enemy stays
    // PVS-suppressed until the player rounds it.
    author_box(&mut workspace, brick, [5120, 256, 256], [5184, 1792, 1280]);
    let door_brush = author_box(&mut workspace, brick, [4064, 256, 1280], [4128, 1024, 1792]);
    let lava_brush = author_box(&mut workspace, cobbles, [4800, 256, 2048], [5824, 512, 2816]);
    // Sealed crypt: the cooked visibility model is portal-component
    // granular, so nothing reachable through the doorway can ever be
    // PVS-suppressed; this hollowed box is its own sealed visibility
    // component and keeps a live sentinel permanently outside the player's
    // row, exercising the suppression counter deterministically.
    author_box(&mut workspace, brick, [6400, 256, 256], [7168, 1792, 1024]);
    workspace.hollow_selected_brush(64);
    workspace.replace_brush_selection(lava_brush, None);
    workspace.set_selected_brush_contents(psxed_project::brush::BrushContents::Lava);
    assert_eq!(
        workspace.project().active_scene().brushes[lava_brush].contents,
        psxed_project::brush::BrushContents::Lava
    );

    // Point placement happens on the interior floor.
    workspace.orthographic_focus[1] = 256.0;

    // The verified player: the Aletha starter profile placed through the
    // Character lane (spawn_role Player makes this entity the player
    // source), then the sample-calibrated renderer scale and spawn facing
    // applied through the inspector mutation path.
    workspace.set_active_tool_cycle_value((ViewTool::Place, Some(PlaceKind::Character)));
    workspace.replace_resource_selection(aletha_profile);
    assert!(workspace.place_bsp_from_top([1024.0, 1536.0]));
    let player_entity = workspace.selected_node_id();
    assert!(workspace.has_player_source());
    workspace.push_undo();
    {
        let scene = workspace.project.active_scene_mut();
        scene.node_mut(player_entity).expect("player entity").name = "Player".to_string();
        scene
            .node_mut(player_entity)
            .expect("player entity")
            .transform
            .rotation_degrees = [0.0, 270.0, 0.0];
        let renderer = scene
            .node(player_entity)
            .expect("player entity")
            .children
            .clone()
            .into_iter()
            .find(|id| {
                matches!(
                    scene.node(*id).map(|node| &node.kind),
                    Some(NodeKind::ModelRenderer { .. })
                )
            })
            .expect("player model renderer");
        if let Some(node) = scene.node_mut(renderer) {
            if let NodeKind::ModelRenderer {
                visual_offset,
                visual_scale_q8,
                ..
            } = &mut node.kind
            {
                *visual_offset = [0, 1, 0];
                *visual_scale_q8 = 360;
            }
        }
    }
    workspace.mark_dirty();

    // The crypt sentinel: a second Mantis sealed inside the hollow box,
    // placed on its interior floor through the same Character lane.
    workspace.orthographic_focus[1] = 320.0;
    workspace.set_active_tool_cycle_value((ViewTool::Place, Some(PlaceKind::Character)));
    workspace.replace_resource_selection(mantis_profile);
    assert!(workspace.place_bsp_from_top([6784.0, 640.0]));
    let sentinel_entity = workspace.selected_node_id();
    workspace.push_undo();
    workspace
        .project
        .active_scene_mut()
        .node_mut(sentinel_entity)
        .expect("sentinel entity")
        .name = "Crypt Sentinel".to_string();
    workspace.mark_dirty();
    workspace.orthographic_focus[1] = 256.0;

    // The verified enemy: the Rust Mantis starter profile in the far room,
    // facing the doorway, with the sample-calibrated renderer scale.
    workspace.set_active_tool_cycle_value((ViewTool::Place, Some(PlaceKind::Character)));
    workspace.replace_resource_selection(mantis_profile);
    assert!(workspace.place_bsp_from_top([6100.0, 800.0]));
    let enemy_entity = workspace.selected_node_id();
    workspace.push_undo();
    {
        let scene = workspace.project.active_scene_mut();
        scene.node_mut(enemy_entity).expect("enemy entity").name = "Mantis Enemy".to_string();
        scene
            .node_mut(enemy_entity)
            .expect("enemy entity")
            .transform
            .rotation_degrees = [0.0, 90.0, 0.0];
        let renderer = scene
            .node(enemy_entity)
            .expect("enemy entity")
            .children
            .clone()
            .into_iter()
            .find(|id| {
                matches!(
                    scene.node(*id).map(|node| &node.kind),
                    Some(NodeKind::ModelRenderer { .. })
                )
            })
            .expect("enemy model renderer");
        if let Some(node) = scene.node_mut(renderer) {
            if let NodeKind::ModelRenderer {
                visual_scale_q8, ..
            } = &mut node.kind
            {
                *visual_scale_q8 = 512;
            }
        }
    }
    workspace.mark_dirty();

    // Equipment components ride both characters through the scene-tree Add
    // Child path, then the inspector's weapon selector binds the swords.
    for (entity, weapon) in [(player_entity, sword_light), (enemy_entity, sword_heavy)] {
        workspace.replace_node_selection(entity);
        workspace.add_child(
            NodeKind::Equipment {
                weapon: None,
                character_socket: "right_hand_grip".to_string(),
                weapon_grip: "grip".to_string(),
            },
            "Equipment",
        );
        let equipment = workspace.selected_node_id();
        workspace.push_undo();
        if let Some(node) = workspace.project.active_scene_mut().node_mut(equipment) {
            if let NodeKind::Equipment {
                weapon: weapon_slot,
                ..
            } = &mut node.kind
            {
                *weapon_slot = Some(weapon);
            }
        }
        workspace.mark_dirty();
    }

    // Decorations and lighting: a collidable image prop in the far room, a
    // box prop and arch in the first room, one light per space.
    workspace.set_active_tool_cycle_value((ViewTool::Place, Some(PlaceKind::BoxProp)));
    workspace.replace_resource_selection(brick);
    assert!(workspace.place_bsp_from_top([1024.0, 2400.0]));
    workspace.set_active_tool_cycle_value((ViewTool::Place, Some(PlaceKind::ArchProp)));
    workspace.replace_resource_selection(brick);
    assert!(workspace.place_bsp_from_top([2900.0, 300.0]));
    workspace.set_active_tool_cycle_value((ViewTool::Place, Some(PlaceKind::ImageProp)));
    workspace.replace_resource_selection(brick);
    assert!(workspace.place_bsp_from_top([6800.0, 1536.0]));
    let image_prop = workspace.selected_node_id();
    workspace.push_undo();
    if let Some(node) = workspace.project.active_scene_mut().node_mut(image_prop) {
        if let NodeKind::ImageProp {
            collision_enabled, ..
        } = &mut node.kind
        {
            *collision_enabled = true;
        }
    }
    workspace.mark_dirty();
    workspace.set_active_tool_cycle_value((ViewTool::Place, Some(PlaceKind::PointLightMarker)));
    assert!(workspace.place_bsp_from_top([2048.0, 1200.0]));
    assert!(workspace.place_bsp_from_top([6100.0, 1536.0]));

    // The lift door: the default Logic placement, the inspector kind switch
    // to Door, and the brush inspector's Model owner binding.
    workspace.set_active_tool_cycle_value((ViewTool::Place, Some(PlaceKind::Logic)));
    assert!(workspace.place_bsp_from_top([4096.0, 1536.0]));
    let door_node = workspace.selected_node_id();
    workspace.push_undo();
    if let Some(node) = workspace.project.active_scene_mut().node_mut(door_node) {
        node.name = "Lift Door".to_string();
        node.kind = NodeKind::Logic {
            kind: psxed_project::LogicNodeKind::Door {
                box_prop: String::new(),
                start_open: false,
                open_offset: [0, 1536, 0],
                travel_ticks: 60,
            },
            target: String::new(),
            killtarget: String::new(),
            master: String::new(),
            delay_ticks: 0,
            wait_ticks: 0,
            enabled: true,
        };
    }
    workspace.mark_dirty();
    workspace.replace_brush_selection(door_brush, None);
    workspace.set_selected_brush_mover(Some(door_node));
    assert_eq!(
        workspace.project().active_scene().brushes[door_brush].mover,
        Some(door_node)
    );

    // The checkpoint: an Entity added through the scene tree, carrying an
    // Interactable switched to Checkpoint exactly as the inspector does.
    workspace.replace_node_selection(root);
    workspace.add_child(NodeKind::Entity, "Entity");
    let checkpoint_entity = workspace.selected_node_id();
    workspace.push_undo();
    if let Some(node) = workspace
        .project
        .active_scene_mut()
        .node_mut(checkpoint_entity)
    {
        node.name = "Sync Relay".to_string();
        node.transform.translation = [2048.0, 257.0, 800.0];
    }
    workspace.mark_dirty();
    workspace.add_child(
        NodeKind::Interactable {
            kind: psxed_project::InteractableKind::default(),
            prompt: "READ ECHO".to_string(),
            radius: 96,
            enabled: true,
        },
        "Interactable",
    );
    let interactable = workspace.selected_node_id();
    workspace.push_undo();
    if let Some(node) = workspace.project.active_scene_mut().node_mut(interactable) {
        if let NodeKind::Interactable { kind, prompt, .. } = &mut node.kind {
            *kind = psxed_project::InteractableKind::Checkpoint {
                checkpoint_id: String::new(),
                title: "SYNC RELAY".to_string(),
                body: "Relay synchronized.".to_string(),
            };
            *prompt = "SYNCHRONIZE".to_string();
        }
    }
    workspace.mark_dirty();

    // The route trigger that chains into the checkpoint on touch.
    workspace.set_active_tool_cycle_value((ViewTool::Place, Some(PlaceKind::Logic)));
    assert!(workspace.place_bsp_from_top([2048.0, 1536.0]));
    let trigger_node = workspace.selected_node_id();
    workspace.push_undo();
    if let Some(node) = workspace.project.active_scene_mut().node_mut(trigger_node) {
        node.name = "Route Trigger".to_string();
        // The anchor and wait now come straight from the Place lane: it parks
        // Logic nodes on the surface (the trigger AABB grows upward from the
        // anchor and the motor stands exactly on the floor plane) and defaults
        // to fire-once. Only the extent and the target are authored here.
        assert_eq!(node.transform.translation[1], 256.0);
        node.kind = NodeKind::Logic {
            kind: psxed_project::LogicNodeKind::TriggerVolume {
                size: [384, 512, 768],
            },
            target: "Sync Relay".to_string(),
            killtarget: String::new(),
            master: String::new(),
            delay_ticks: 0,
            // Fire once then retire (hl's wait -1). Respawn re-arms the
            // record, the souls rule.
            wait_ticks: -1,
            enabled: true,
        };
    }
    workspace.mark_dirty();

    workspace.save_if_dirty().expect("persist authored slice");
    assert!(!workspace.is_dirty());

    // Reopen the persisted project and verify the authored contract before
    // cooking from the reloaded document.
    let reopened = EditorWorkspace::open_directory(&project_dir).expect("reopen slice project");
    assert!(reopened.has_player_source());
    let scene = reopened.project().active_scene();
    assert_eq!(scene.brushes.len(), 17);
    assert_eq!(
        scene.brushes[lava_brush].contents,
        psxed_project::brush::BrushContents::Lava
    );
    assert_eq!(scene.brushes[door_brush].mover, Some(door_node));
    let kind_count = |wants: fn(&NodeKind) -> bool| {
        scene
            .nodes()
            .iter()
            .filter(|node| wants(&node.kind))
            .count()
    };
    assert_eq!(
        kind_count(|kind| matches!(kind, NodeKind::PointLight { .. })),
        2
    );
    assert_eq!(kind_count(|kind| matches!(kind, NodeKind::BoxProp { .. })), 1);
    assert_eq!(
        kind_count(|kind| matches!(kind, NodeKind::ArchProp { .. })),
        1
    );
    assert_eq!(
        kind_count(|kind| matches!(
            kind,
            NodeKind::ImageProp {
                collision_enabled: true,
                ..
            }
        )),
        1
    );
    assert_eq!(
        kind_count(|kind| matches!(kind, NodeKind::Equipment { weapon: Some(_), .. })),
        2
    );
    assert_eq!(
        kind_count(|kind| matches!(
            kind,
            NodeKind::Interactable {
                kind: psxed_project::InteractableKind::Checkpoint { .. },
                ..
            }
        )),
        1
    );

    // Cook the persisted revision twice; the artifacts must be nonzero and
    // byte-deterministic, and the compiled world must carry the authored
    // body hulls (not the characterless debug fallback), the door mover,
    // and the full combat cast.
    let mut reopened = reopened;
    reopened
        .cook_playtest_to_dir(&cook_a)
        .expect("cook souls slice");
    let pxbsp = std::fs::read(cook_a.join(psxed_project::brush_playtest::BRUSH_WORLD_FILENAME))
        .expect("cooked PXBSP");
    assert!(!pxbsp.is_empty(), "PXBSP must not be empty");
    reopened
        .cook_playtest_to_dir(&cook_b)
        .expect("deterministic recook");
    for filename in [
        psxed_project::brush_playtest::BRUSH_WORLD_FILENAME,
        psxed_project::playtest::COOKED_MANIFEST_FILENAME,
    ] {
        assert_eq!(
            std::fs::read(cook_a.join(filename)).unwrap(),
            std::fs::read(cook_b.join(filename)).unwrap(),
            "{filename} drifted across an unchanged recook"
        );
    }

    let project = ProjectDocument::load_from_path(project_dir.join("project.ron")).unwrap();
    let (package, report) = psxed_project::playtest::build_package(&project, &project_dir);
    assert!(report.is_ok(), "{}", report.error_messages().join("; "));
    let package = package.expect("cooked package");
    let psxed_project::playtest::PlaytestWorldGeometry::Pxbsp(ref world) = package.world_geometry
    else {
        panic!("slice must cook as PXBSP");
    };
    assert_eq!(world.movers.len(), 1, "one lift-door mover");
    // Hulls must derive from the two authored character envelopes: hull one
    // for the Aletha body, hull two for the Mantis body, never the 16/56
    // characterless debug fallback.
    let hulls: Vec<(usize, i32, i32)> = world
        .body_hulls
        .iter()
        .map(|hull| (hull.hull_index, hull.radius, hull.height))
        .collect();
    assert_eq!(hulls, vec![(1, 188, 1024), (2, 192, 1024)]);
    assert_eq!(
        package.game_entities.len(),
        2,
        "the fightable Mantis plus the sealed crypt sentinel"
    );
    assert_eq!(package.equipment.len(), 2, "both equipment records cook");
    assert_eq!(
        package
            .equipment
            .iter()
            .filter(|record| record.flags & psx_level::equipment_flags::PLAYER != 0)
            .count(),
        1,
        "exactly one PLAYER-flagged equipment record"
    );
    assert_eq!(package.interactables.len(), 1, "the checkpoint interactable");
    assert!(
        package.logic.len() >= 3,
        "trigger + paired checkpoint + door records"
    );
    assert_eq!(package.lights.len(), 2);
    assert_eq!(package.box_props.len(), 1);

    // The opt-in export consumed by the editor-souls-bsp-check gate: the
    // exact authored project, stabilised name, plus the canonical and
    // negative tapes.
    if let Some(export_dir) = std::env::var_os("PSOXIDE_SOULS_SLICE_PROJECT_OUT") {
        let export_dir = PathBuf::from(export_dir);
        assert!(
            !export_dir.exists(),
            "souls-slice export destination already exists: {}",
            export_dir.display()
        );
        copy_dir_recursive(&project_dir, &export_dir).expect("export authored slice project");
        let export_file = export_dir.join("project.ron");
        let mut exported = ProjectDocument::load_from_path(&export_file).expect("load export");
        exported.name = "Souls BSP Vertical Slice".to_string();
        exported
            .save_to_path(&export_file)
            .expect("stabilise exported slice name");
        write_souls_slice_canonical_tape(&export_dir);
        write_souls_slice_negative_tape(&export_dir);
        println!("souls-slice project: {}", export_file.display());
    }

    let _ = std::fs::remove_dir_all(project_dir);
    let _ = std::fs::remove_dir_all(cook_a);
    let _ = std::fs::remove_dir_all(cook_b);
}
