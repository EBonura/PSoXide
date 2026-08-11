use super::*;

#[test]
fn play_chunk_debug_map_follows_player_layer_then_editor_layer() {
    let cell = |runtime_room_index, floor_index, elevation| PlayChunkDebugMapCell {
        runtime_room_index,
        project_room_id: NodeId::ROOT,
        portal_room_index: runtime_room_index,
        array_cell: [0, 0],
        center: [0.0, 0.0],
        half: [0.5, 0.5],
        room_origin: [0.0, 0.0],
        runtime_origin: [0, 0],
        sector_size: 1024.0,
        floor_index,
        elevation,
    };
    let map = PlayChunkDebugMap {
        cells: vec![cell(2, 0, 0), cell(7, 1, 2048)],
        portals: Vec::new(),
    };

    let player_on_upper = EditorPlaytestMetrics {
        player_map_valid: true,
        player_room_index: 7,
        ..Default::default()
    };
    assert_eq!(map.display_floor(player_on_upper, 0), 1);
    assert_eq!(map.display_floor(EditorPlaytestMetrics::default(), 1), 1);
    assert_eq!(map.display_floor(EditorPlaytestMetrics::default(), 99), 0);
    assert_eq!(map.floor_count(), 2);
}

mod brush_tools;
mod entity_resources;
mod geometry_resources;
mod layer_authoring;
mod orthographic_brush;
mod placement_painting;
mod project_workspace;
mod scene_tree_selection;
mod ui_layout;
mod ui_preview;
mod viewport_gizmo;
mod world_editing;

fn orbit_rig() -> CameraRig {
    CameraRig {
        mode: ViewportCameraMode::Orbit,
        yaw: 0,
        pitch: 0,
        radius: 4096,
        target: [0, 0, 0],
        free_yaw: 0,
        free_pitch: 0,
        free_position: [0, 0, 0],
        free_initialized: false,
        zoom_speed: 1.0,
    }
}

/// Camera rig math runs without a workspace or egui.
#[test]
fn camera_rig_orbit_rotate_wraps_yaw_and_clamps_pitch() {
    let mut rig = orbit_rig();
    // 4 q12 units per pixel: a +10px horizontal drag advances yaw 40.
    rig.rotate(Vec2::new(10.0, 0.0));
    assert_eq!(rig.yaw, 40);
    // A large downward drag saturates pitch at the +960 pole clamp.
    rig.rotate(Vec2::new(0.0, 10_000.0));
    assert_eq!(rig.pitch, 960);
}

#[test]
fn camera_rig_orbit_scroll_dollies_and_clamps_radius() {
    let mut rig = orbit_rig();
    // Magnitude-proportional dolly: one 50px notch at speed 1.0 is
    // 1.08^-1, and a tiny 5px trackpad tick moves a tenth of that.
    rig.radius = 4096;
    rig.scroll(50.0);
    assert_eq!(rig.radius, (4096.0_f32 * 1.08_f32.powf(-1.0)) as i32);
    rig.radius = 4096;
    rig.scroll(5.0);
    assert_eq!(rig.radius, (4096.0_f32 * 1.08_f32.powf(-0.1)) as i32);
    // The zoom-speed setting scales the exponent.
    rig.radius = 4096;
    rig.set_zoom_speed(2.0);
    rig.scroll(50.0);
    assert_eq!(rig.radius, (4096.0_f32 * 1.08_f32.powf(-2.0)) as i32);
    rig.set_zoom_speed(1.0);
    // One event is clamped to 4 notches, so momentum cannot teleport.
    rig.radius = 4096;
    rig.scroll(100_000.0);
    assert_eq!(rig.radius, (4096.0_f32 * 1.08_f32.powf(-4.0)) as i32);
    // Radius floor holds.
    rig.radius = 512;
    rig.scroll(50.0);
    assert_eq!(rig.radius, 512);
}

#[test]
fn camera_rig_orbit_rotate_keeps_explicit_focus_target() {
    let mut rig = orbit_rig();
    rig.radius = 8192;
    rig.target = [4096, 512, -2048];
    let target = rig.target;
    for delta in [
        Vec2::new(64.0, 0.0),
        Vec2::new(-31.0, 22.0),
        Vec2::new(5.0, -90.0),
    ] {
        rig.rotate(delta);
        assert_eq!(rig.target, target);
    }
}

#[test]
fn camera_rig_switch_to_free_seeds_from_orbit_once() {
    let mut rig = orbit_rig();
    rig.yaw = 1024;
    rig.pitch = 256;
    assert!(rig.set_mode(ViewportCameraMode::Free));
    assert_eq!(rig.mode, ViewportCameraMode::Free);
    assert!(rig.free_initialized);
    assert_eq!(rig.free_yaw, 1024);
    assert_eq!(rig.free_pitch, 256);
    // Re-selecting the active mode is a no-op.
    assert!(!rig.set_mode(ViewportCameraMode::Free));
}

fn test_temp_dir(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "psxed-ui-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

/// Deletes a test-created project directory on drop, panic path included.
///
/// Tests that exercise `create_and_open_project` have to write where
/// `psxed_project::projects_dir()` points, which in a checkout is the TRACKED
/// `editor/projects/`. That directory's `.gitignore` starts with `*` and
/// allowlists the real projects, so a directory a failing assertion leaves
/// behind is invisible to `git status` and stays forever. A trailing
/// `remove_dir_all` never runs on the panic path; this guard does.
struct ScratchProjectDir(PathBuf);

impl ScratchProjectDir {
    fn new(path: PathBuf) -> Self {
        let _ = std::fs::remove_dir_all(&path);
        Self(path)
    }
}

impl Drop for ScratchProjectDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The guard has to survive a panicking test body, which is the only case
/// that ever leaked a directory into the tracked `editor/projects/`.
#[test]
fn scratch_project_dir_guard_cleans_up_after_a_panic() {
    let dir = test_temp_dir("scratch-guard");
    let probe = dir.clone();
    let result = std::panic::catch_unwind(move || {
        let _guard = ScratchProjectDir::new(dir.clone());
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        assert!(dir.is_dir());
        panic!("test body fails after creating the project");
    });
    assert!(result.is_err(), "the body must have panicked");
    assert!(
        !probe.exists(),
        "the guard must remove {probe:?} on the panic path"
    );
}

fn assert_vec3_approx(actual: [f32; 3], expected: [f32; 3]) {
    for axis in 0..3 {
        assert!(
            (actual[axis] - expected[axis]).abs() < 0.001,
            "axis {axis}: expected {}, got {}",
            expected[axis],
            actual[axis]
        );
    }
}

fn set_gizmo_test_camera(workspace: &mut EditorWorkspace) {
    workspace.camera_rig.mode = ViewportCameraMode::Free;
    workspace.camera_rig.free_position = [512, 768, -2048];
    workspace.camera_rig.free_yaw = 2048;
    workspace.camera_rig.free_pitch = signed_to_q12(-160);
    workspace.camera_rig.free_initialized = true;
}

fn projected_gizmo_axis(
    workspace: &EditorWorkspace,
    viewport: Rect,
    axis: PrimitiveGizmoAxis,
) -> PrimitiveGizmoScreenAxis {
    workspace
        .primitive_gizmo_screen_axes(viewport)
        .into_iter()
        .find(|candidate| candidate.axis == axis)
        .expect("gizmo axis projects")
}

fn projected_node_gizmo_axis(
    workspace: &EditorWorkspace,
    viewport: Rect,
    axis: PrimitiveGizmoAxis,
) -> PrimitiveGizmoScreenAxis {
    workspace
        .node_gizmo_screen_axes(viewport)
        .into_iter()
        .find(|candidate| candidate.axis == axis)
        .expect("node gizmo axis projects")
}

fn projected_node_gizmo_plane(
    workspace: &EditorWorkspace,
    viewport: Rect,
    plane: NodeGizmoPlane,
) -> NodeGizmoScreenPlane {
    workspace
        .node_gizmo_screen_planes(viewport)
        .into_iter()
        .find(|candidate| candidate.plane == plane)
        .expect("node gizmo plane projects")
}

fn screen_plane_center(plane: NodeGizmoScreenPlane) -> Pos2 {
    let sum = plane
        .corners
        .iter()
        .fold(Vec2::ZERO, |acc, corner| acc + corner.to_vec2());
    let average = sum / plane.corners.len() as f32;
    Pos2::new(average.x, average.y)
}

/// Headless driver for the 3D viewport's pointer-resolution path.
///
/// Builds a workspace with one floored room, aims a free-fly camera
/// at the room from a chosen distance, and runs the *real*
/// [`EditorWorkspace::resolve_viewport_3d_pointer_target`] used by
/// click handling. Interaction bugs that only appear at certain
/// zooms or angles (gizmo-vs-tile picking, entity-vs-surface
/// priority) become deterministic tests with no live window or GPU.
/// The floor matters: it guarantees there is always a tile *behind*
/// the gizmo for a failed pick to wrongly fall through to.
struct ViewportHarness {
    workspace: EditorWorkspace,
    room: NodeId,
    viewport: Rect,
}

impl ViewportHarness {
    /// `extent` x `extent` sectors, every cell floored at y=0.
    fn floored_room(label: &str, extent: u16) -> Self {
        let mut project = ProjectDocument::new(label);
        let mut grid = WorldGrid::empty(extent, extent, 1024);
        for sx in 0..extent {
            for sz in 0..extent {
                grid.set_floor(sx, sz, 0, None);
            }
        }
        let room =
            project
                .active_scene_mut()
                .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
        let mut workspace = EditorWorkspace::with_project(test_temp_dir(label), project);
        workspace.transform_gizmo_mode = TransformGizmoMode::Move;
        workspace.camera_rig.mode = ViewportCameraMode::Free;
        workspace.camera_rig.free_initialized = true;
        Self {
            workspace,
            room,
            viewport: Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0)),
        }
    }

    /// World-space centre of the room's floor plane, the natural
    /// camera target and gizmo pivot for a centred node.
    fn room_center(&self) -> [f32; 3] {
        let grid = self.workspace.room_grid_view(self.room).unwrap();
        let s = grid.sector_size as f32;
        [
            grid.width as f32 * 0.5 * s,
            0.0,
            grid.depth as f32 * 0.5 * s,
        ]
    }

    /// Add a point light at the room centre, lifted `lift_sectors`
    /// off the floor. Returns the node so the test can select it.
    fn add_centre_light(&mut self, lift_sectors: f32) -> NodeId {
        let light = self.workspace.project.active_scene_mut().add_node(
            self.room,
            "Light",
            NodeKind::PointLight {
                color: [255, 240, 200],
                intensity: 1.0,
                radius: 4.0,
            },
        );
        if let Some(node) = self.workspace.project.active_scene_mut().node_mut(light) {
            node.transform.translation = [0.0, lift_sectors, 0.0];
        }
        light
    }

    /// Aim the free camera at `target` from `distance` world units
    /// away along a fixed oblique down-and-aside direction. Larger
    /// `distance` = more zoomed out.
    fn frame(&mut self, target: [f32; 3], distance: f32) {
        // ~34deg above the ground.
        self.frame_at_elevation(target, distance, 0.5691_f32.asin());
    }

    /// Aim the free camera at `target` from `distance` units away,
    /// `elevation_rad` above the horizon (0 = looking horizontally
    /// across the ground, PI/2 = straight down). Lower elevation
    /// foreshortens the flat XZ ground plane.
    fn frame_at_elevation(&mut self, target: [f32; 3], distance: f32, elevation_rad: f32) {
        let horiz = elevation_rad.cos();
        // Keep the same azimuth as the original oblique direction.
        let dir = [0.6556 * horiz, elevation_rad.sin(), -0.7551 * horiz];
        let pos = [
            round_to_i32(target[0] + dir[0] * distance),
            round_to_i32(target[1] + dir[1] * distance),
            round_to_i32(target[2] + dir[2] * distance),
        ];
        self.workspace.camera_rig.free_position = pos;
        let tgt = [
            round_to_i32(target[0]),
            round_to_i32(target[1]),
            round_to_i32(target[2]),
        ];
        if let Some((yaw, pitch)) = camera_angles_to_look_at(pos, tgt) {
            self.workspace.camera_rig.free_yaw = yaw;
            self.workspace.camera_rig.free_pitch = pitch;
        }
    }

    fn select(&mut self, node: NodeId) {
        self.workspace.replace_node_selection(node);
    }

    /// Resolve what a click at `pointer` would target, through the
    /// same path the live viewport uses.
    fn resolve(&self, pointer: Pos2) -> Option<Viewport3dPointerTarget> {
        self.workspace.resolve_viewport_3d_pointer_target(
            self.viewport,
            pointer,
            Some(self.room),
            true,
        )
    }

    /// The node-gizmo handle a click at `pointer` would grab, if any.
    fn gizmo_handle_at(&self, pointer: Pos2) -> Option<NodeGizmoHandle> {
        self.resolve(pointer)
            .and_then(|target| target.node_handle())
    }

    /// On-screen quad corners of a move-plane handle, or `None` if
    /// the plane is currently culled (too small / edge-on to
    /// project).
    fn plane_quad(&self, plane: NodeGizmoPlane) -> Option<[Pos2; 4]> {
        self.workspace
            .node_gizmo_screen_planes(self.viewport)
            .into_iter()
            .find(|candidate| candidate.plane == plane)
            .map(|candidate| candidate.corners)
    }

    /// Distance in pixels from `pointer` to the nearest gizmo axis
    /// segment, matching the metric `pick_node_gizmo_handle` uses.
    fn nearest_axis_distance(&self, pointer: Pos2) -> f32 {
        self.workspace
            .node_gizmo_screen_axes(self.viewport)
            .into_iter()
            .map(|axis| {
                distance_to_segment_2d(pointer, axis.start, axis.end)
                    .min((pointer - axis.end).length())
            })
            .fold(f32::INFINITY, f32::min)
    }

    /// Screen triangle a move plane visually occupies: the pivot and
    /// the two axis endpoints that bound it. The drawn plane quad is a
    /// small inset inside this triangle; the user reads the whole
    /// corner as "the plane". `None` if the plane or an axis is culled.
    fn plane_footprint_triangle(&self, plane: NodeGizmoPlane) -> Option<[Pos2; 3]> {
        let axes = self.workspace.node_gizmo_screen_axes(self.viewport);
        let pivot = axes.first().map(|a| a.start)?;
        let [pa, pb] = plane.axes();
        let end_a = axes.iter().find(|a| a.axis == pa).map(|a| a.end)?;
        let end_b = axes.iter().find(|a| a.axis == pb).map(|a| a.end)?;
        Some([pivot, end_a, end_b])
    }

    /// One-character class of what a click at `pointer` resolves to,
    /// for diagnostic maps. Uppercase = node-gizmo plane, lowercase =
    /// node-gizmo axis, `P` = primitive gizmo, `#` = tile/surface (the
    /// bug), `.` = nothing.
    fn classify(&self, pointer: Pos2) -> char {
        match self.resolve(pointer) {
            Some(Viewport3dPointerTarget::NodeGizmo(NodeGizmoHandle::Plane(p))) => match p {
                NodeGizmoPlane::XZ => 'Z',
                NodeGizmoPlane::XY => 'Y',
                NodeGizmoPlane::YZ => 'V',
            },
            Some(Viewport3dPointerTarget::NodeGizmo(NodeGizmoHandle::Axis(a))) => match a {
                PrimitiveGizmoAxis::X => 'x',
                PrimitiveGizmoAxis::Y => 'y',
                PrimitiveGizmoAxis::Z => 'z',
            },
            Some(Viewport3dPointerTarget::NodeGizmo(NodeGizmoHandle::BoxFace(_))) => 'B',
            Some(Viewport3dPointerTarget::PrimitiveGizmo(_)) => 'P',
            Some(Viewport3dPointerTarget::Entity(_)) => 'E',
            Some(Viewport3dPointerTarget::Brush { .. }) => 'B',
            Some(Viewport3dPointerTarget::Surface { .. }) => '#',
            None => '.',
        }
    }
}

fn assert_pos_approx(actual: Pos2, expected: Pos2) {
    assert!((actual.x - expected.x).abs() < 0.001);
    assert!((actual.y - expected.y).abs() < 0.001);
}

fn assert_size_approx(actual: Vec2, expected: Vec2) {
    assert!((actual.x - expected.x).abs() < 0.001);
    assert!((actual.y - expected.y).abs() < 0.001);
}

fn test_node_preview_origin(project: &ProjectDocument, room: NodeId, node: NodeId) -> [i32; 3] {
    let scene = project.active_scene();
    let room_node = scene.node(room).expect("room exists");
    let NodeKind::Section { grid } = &room_node.kind else {
        panic!("expected room");
    };
    let node = scene.node(node).expect("node exists");
    psxed_project::spatial::node_preview_origin(grid, &node.transform)
}

fn starter_player_entity(scene: &psxed_project::Scene) -> &psxed_project::SceneNode {
    scene
        .nodes()
        .iter()
        .find(|node| {
            matches!(node.kind, NodeKind::Entity)
                && node.children.iter().any(|id| {
                    scene.node(*id).is_some_and(|child| {
                        matches!(
                            child.kind,
                            NodeKind::CharacterController { player: true, .. }
                        )
                    })
                })
        })
        .or_else(|| {
            scene
                .nodes()
                .iter()
                .find(|node| matches!(node.kind, NodeKind::Entity))
        })
        .expect("starter has an Entity")
}

fn floor_heights(workspace: &EditorWorkspace, room: NodeId, sx: u16, sz: u16) -> [i32; 4] {
    let scene = workspace.project.active_scene();
    let node = scene.node(room).expect("room node exists");
    let NodeKind::Section { grid } = &node.kind else {
        panic!("active room is a room node");
    };
    grid.sector(sx, sz)
        .and_then(|sector| sector.floor.as_ref())
        .expect("starter floor exists")
        .heights
}

fn floor_triangle_heights(
    workspace: &EditorWorkspace,
    room: NodeId,
    sx: u16,
    sz: u16,
    triangle: HorizontalTriangleIndex,
) -> [i32; 3] {
    let scene = workspace.project.active_scene();
    let node = scene.node(room).expect("room node exists");
    let NodeKind::Section { grid } = &node.kind else {
        panic!("active room is a room node");
    };
    grid.sector(sx, sz)
        .and_then(|sector| sector.floor.as_ref())
        .expect("starter floor exists")
        .triangle_heights(triangle.idx())
}

fn first_floor_sector(workspace: &EditorWorkspace, room: NodeId) -> (u16, u16) {
    let scene = workspace.project.active_scene();
    let node = scene.node(room).expect("room node exists");
    let NodeKind::Section { grid } = &node.kind else {
        panic!("active room is a room node");
    };
    grid.sectors
        .iter()
        .enumerate()
        .find_map(|(index, sector)| {
            sector
                .as_ref()
                .and_then(|sector| sector.floor.as_ref())
                .map(|_| {
                    let index = index as u16;
                    (index / grid.depth, index % grid.depth)
                })
        })
        .expect("starter has a floor sector")
}

fn resource_search_token(resource: &Resource) -> String {
    resource
        .name
        .to_ascii_lowercase()
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .find(|part| !part.is_empty())
        .unwrap_or("")
        .to_string()
}

fn cell_with_floor(material: Option<psxed_project::ResourceId>) -> psxed_project::GridSector {
    psxed_project::GridSector {
        floor: Some(psxed_project::GridHorizontalFace::flat(0, material)),
        ceiling: None,
        walls: psxed_project::GridWalls::default(),
        floor_above: None,
        floor_below: None,
    }
}

fn populated_grid(width: u16, depth: u16) -> WorldGrid {
    let mut grid = WorldGrid::empty(width, depth, 1024);
    for sx in 0..width {
        for sz in 0..depth {
            if let Some(s) = grid.ensure_sector(sx, sz) {
                *s = cell_with_floor(None);
            }
        }
    }
    grid
}

fn workspace_with_populated_grid(label: &str, width: u16, depth: u16) -> (EditorWorkspace, NodeId) {
    let mut project = ProjectDocument::new(label);
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Section {
            grid: populated_grid(width, depth),
        },
    );
    (
        EditorWorkspace::with_project(test_temp_dir(label), project),
        room,
    )
}

fn test_model_resource(name: &str) -> psxed_project::ModelResource {
    psxed_project::ModelResource {
        model_path: format!("assets/models/{name}.psxmdl"),
        source_path: None,
        texture_path: None,
        skeleton: None,
        world_height: 1024,
        collision_radius: default_model_collision_radius_for_height(1024),
        scale_q8: [MODEL_SCALE_ONE_Q8; 3],
        default_visual_yaw_q12: 0,
        attachments: Vec::new(),
    }
}
