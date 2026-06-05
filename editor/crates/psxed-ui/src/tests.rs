use super::*;

#[test]
fn ui_font_atlas_has_expected_dimensions() {
    let atlas = rasterize_ui_font_atlas(UiFontChoice::Basic);
    let basic = ui_preview_font_spec(UiFontChoice::Basic);
    let basic_rows = basic.glyph_count.div_ceil(basic.cols);
    assert_eq!(
        atlas.size,
        [basic.cols * basic.glyph_w, basic_rows * basic.glyph_h]
    );
    // A real font has many lit pixels; an all-transparent atlas would mean
    // the rasterizer read the source wrong.
    let opaque = atlas.pixels.iter().filter(|p| p.a() == 255).count();
    assert!(opaque > 500, "atlas looks empty ({opaque} opaque px)");

    let tall = rasterize_ui_font_atlas(UiFontChoice::Basic8x16);
    let tall_spec = ui_preview_font_spec(UiFontChoice::Basic8x16);
    let tall_rows = tall_spec.glyph_count.div_ceil(tall_spec.cols);
    assert_eq!(
        tall.size,
        [
            tall_spec.cols * tall_spec.glyph_w,
            tall_rows * tall_spec.glyph_h
        ]
    );
    let tall_opaque = tall.pixels.iter().filter(|p| p.a() == 255).count();
    assert!(
        tall_opaque > opaque,
        "8x16 atlas should carry more lit rows than 8x8",
    );

    let orbitron = rasterize_ui_font_atlas(UiFontChoice::Orbitron);
    let orbitron_spec = ui_preview_font_spec(UiFontChoice::Orbitron);
    let orbitron_rows = orbitron_spec.glyph_count.div_ceil(orbitron_spec.cols);
    assert_eq!(
        orbitron.size,
        [
            orbitron_spec.cols * orbitron_spec.glyph_w,
            orbitron_rows * orbitron_spec.glyph_h
        ]
    );
    assert!(orbitron_spec.glyph_w > basic.glyph_w);
    let orbitron_opaque = orbitron.pixels.iter().filter(|p| p.a() == 255).count();
    assert!(
        orbitron_opaque > 500,
        "imported TTF atlas looks empty ({orbitron_opaque} opaque px)",
    );
}

#[test]
fn ui_font_atlas_pixels_match_the_source_font_bits() {
    use psx_font::fonts::BASIC;
    let atlas = rasterize_ui_font_atlas(UiFontChoice::Basic);
    let spec = ui_preview_font_spec(UiFontChoice::Basic);
    let aw = spec.cols * spec.glyph_w;
    // Check every glyph cell against the source bitmap: an atlas pixel is
    // opaque exactly when the corresponding source bit is set. This proves
    // the rasterizer (grid placement + bit-order via glyph_row_packed) is
    // faithful, not just non-empty.
    for glyph in 0..BASIC.glyph_count {
        let gx = (glyph as usize % spec.cols) * spec.glyph_w;
        let gy = (glyph as usize / spec.cols) * spec.glyph_h;
        for row in 0..spec.glyph_h {
            let bits = BASIC.glyph_row_packed(glyph, row as u8);
            for col in 0..spec.glyph_w {
                let lit = bits & (1 << col) != 0;
                let px = atlas.pixels[(gy + row) * aw + (gx + col)];
                assert_eq!(
                    px.a() == 255,
                    lit,
                    "glyph {glyph} row {row} col {col} mismatch",
                );
            }
        }
    }
}

#[test]
fn ui_font_glyph_uv_is_in_unit_range_and_advances_by_one_cell() {
    let a = ui_font_glyph_uv(UiFontChoice::Basic, b'A');
    assert!(a.min.x >= 0.0 && a.max.x <= 1.0 && a.min.y >= 0.0 && a.max.y <= 1.0);
    // Adjacent codes on the same row differ by exactly one column step.
    let b = ui_font_glyph_uv(UiFontChoice::Basic, b'B');
    let step = 1.0 / UI_FONT_COLS as f32;
    assert!((b.min.x - a.min.x - step).abs() < 1e-6);
    // Out-of-range codes clamp into the atlas (no panic, stays in 0..1).
    let oob = ui_font_glyph_uv(UiFontChoice::Basic8x16, 255);
    assert!(oob.max.x <= 1.0 && oob.max.y <= 1.0);
}

#[test]
fn ui_preview_text_width_applies_letter_spacing_between_glyphs() {
    assert_eq!(
        ui_preview_text_width(UiFontChoice::Basic, "ABC", 1.0, 2, 1.0),
        28.0
    );
    assert_eq!(
        ui_preview_text_width(UiFontChoice::Basic, "ABC", 2.0, -1, 1.0),
        46.0
    );
    assert_eq!(
        ui_preview_text_width(UiFontChoice::Basic, "A", 1.0, 9, 1.0),
        8.0
    );
}

#[test]
fn ui_preview_image_effect_colors_animate_and_keep_split_edge_continuous() {
    let left = UiRect::new(0, 0, 160, 100);
    let right = UiRect::new(160, 0, 160, 100);

    assert_eq!(
        ui_preview_image_effect_overlay_colors(UiImageEffect::None, 0, left),
        [Color32::TRANSPARENT; 4]
    );
    assert_ne!(
        ui_preview_image_effect_overlay_colors(UiImageEffect::Shimmer, 0, left),
        ui_preview_image_effect_overlay_colors(UiImageEffect::Shimmer, 64, left)
    );

    let left_colors = ui_preview_image_effect_overlay_colors(UiImageEffect::Shimmer, 48, left);
    let right_colors = ui_preview_image_effect_overlay_colors(UiImageEffect::Shimmer, 48, right);
    assert_eq!(left_colors[1], right_colors[0]);
    assert_eq!(left_colors[3], right_colors[2]);

    let pulse = ui_preview_image_effect_overlay_colors(UiImageEffect::SoftPulse, 12, left);
    assert_eq!(pulse[0], pulse[3]);
}

#[test]
fn preview_wrap_hard_split_counts_spacing_between_included_glyphs_only() {
    assert_eq!(
        preview_wrap_hard_split("ABC", UiFontChoice::Basic, 18.0, 1.0, 2, 1.0),
        2
    );
}

/// The shared multi-select math (`apply_range_modifiers`) backs both scene
/// node and resource selection. Exercise it directly over plain ids so the
/// branching is covered without a project, a workspace, or egui.
#[test]
fn range_modifiers_cover_replace_toggle_and_shift() {
    let order = [10u64, 20, 30, 40, 50];
    let mut set = HashSet::new();
    let mut anchor = None;

    // Plain click replaces the selection and sets the anchor.
    let primary = apply_range_modifiers(&mut set, &mut anchor, 30, false, false, &order, 0);
    assert_eq!(set, HashSet::from([30]));
    assert_eq!(anchor, Some(30));
    assert_eq!(primary, Some(30));

    // Toggle adds without clearing.
    let primary = apply_range_modifiers(&mut set, &mut anchor, 10, false, true, &order, 0);
    assert_eq!(set, HashSet::from([10, 30]));
    assert_eq!(anchor, Some(10));
    assert_eq!(primary, Some(10));

    // Toggling a selected id removes it; the primary falls back to the
    // first still-selected id in order.
    let primary = apply_range_modifiers(&mut set, &mut anchor, 30, false, true, &order, 0);
    assert_eq!(set, HashSet::from([10]));
    assert_eq!(primary, Some(10));

    // Shift without toggle clears, then selects the inclusive range from
    // the existing anchor; the anchor is preserved.
    anchor = Some(20);
    let primary = apply_range_modifiers(&mut set, &mut anchor, 50, true, false, &order, 0);
    assert_eq!(set, HashSet::from([20, 30, 40, 50]));
    assert_eq!(anchor, Some(20));
    assert_eq!(primary, Some(50));

    // Shift with toggle keeps the prior selection and unions the range.
    let mut set = HashSet::from([10u64]);
    let mut anchor = Some(20);
    apply_range_modifiers(&mut set, &mut anchor, 40, true, true, &order, 0);
    assert_eq!(set, HashSet::from([10, 20, 30, 40]));

    // With no anchor yet, the fallback anchors the range.
    let mut set = HashSet::new();
    let mut anchor = None;
    apply_range_modifiers(&mut set, &mut anchor, 30, true, false, &order, 10);
    assert_eq!(set, HashSet::from([10, 20, 30]));
    assert_eq!(anchor, Some(10));
}

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
    rig.radius = 4096;
    rig.scroll(1.0); // zoom in: radius *= 0.92
    assert_eq!(rig.radius, (4096.0_f32 * 0.92) as i32);
    rig.radius = 512;
    rig.scroll(1.0); // 512 * 0.92 = 471, clamped back up to the 512 floor
    assert_eq!(rig.radius, 512);
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
                .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
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
            Some(Viewport3dPointerTarget::PrimitiveGizmo(_)) => 'P',
            Some(Viewport3dPointerTarget::Entity(_)) => 'E',
            Some(Viewport3dPointerTarget::Surface { .. }) => '#',
            None => '.',
        }
    }
}

/// Floor-aware selection: with floor 1 active, geometry reads must
/// address floor 1, not the floor 0 grid sitting underneath it. The
/// two floors carry DISTINCT geometry (floor 0 has a floor face at
/// (0,0); floor 1 has a north wall there and no floor), so reading
/// the wrong floor is observable. Before the fix, `face_world_corners`
/// destructured `NodeKind::Room { grid }` directly (always floor 0);
/// now it routes through `room_grid_view`, which honours `active_floor`.
#[test]
fn face_corner_reads_address_the_active_floor() {
    let mut project = ProjectDocument::new("active-floor-pick");
    let mut grid = WorldGrid::empty(2, 2, 1024);
    grid.set_floor(0, 0, 0, None);
    grid.push_floor();
    let floor1 = grid.floor_mut(1).expect("floor 1");
    floor1.add_wall(0, 0, GridDirection::North, 0, 1024, None);
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("active-floor-pick"), project);

    let floor_face = FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Floor,
    };
    let wall_face = FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Wall {
            dir: GridDirection::North,
            stack: 0,
        },
    };

    // Base floor active: floor face present, wall absent.
    workspace.active_floor = 0;
    assert!(workspace.face_world_corners(floor_face).is_some());
    assert!(workspace.face_world_corners(wall_face).is_none());

    // Floor 1 active: the reads must follow the active floor.
    workspace.active_floor = 1;
    assert!(
        workspace.face_world_corners(wall_face).is_some(),
        "floor 1's wall should be addressable when floor 1 is active"
    );
    assert!(
        workspace.face_world_corners(floor_face).is_none(),
        "floor 0's floor face must not leak through when floor 1 is active"
    );

    // Selection-set readers route too: select-all enumerates the
    // active floor's faces, so on floor 1 it returns the wall, not
    // floor 0's floor face.
    let faces = workspace.all_faces_in_room(room);
    assert!(
        faces.contains(&wall_face) && !faces.contains(&floor_face),
        "all_faces_in_room must enumerate the active floor: {faces:?}"
    );
}

/// Object selection is floor-tied: an entity on floor 0 and one on
/// floor 1 must each only be selectable when their floor is active (or
/// below it), and their pick bounds sit at the floor's drawn Y. This
/// is the user-reported bug ("selecting on floor 2 hits the room
/// below") and the payoff of routing selection through the shared
/// floor_view resolver.
#[test]
fn entity_selection_respects_active_floor() {
    let mut project = ProjectDocument::new("sel-floor");
    let mut grid = WorldGrid::empty(2, 2, 1024);
    grid.set_floor(0, 0, 0, None);
    grid.push_floor();
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
    let scene = project.active_scene_mut();
    let ground = scene.add_node(room, "Ground", NodeKind::Entity);
    scene.node_mut(ground).unwrap().floor = 0;
    let upper = scene.add_node(room, "Upper", NodeKind::Entity);
    scene.node_mut(upper).unwrap().floor = 1;
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("sel-floor"), project);

    let bound_nodes = |ws: &EditorWorkspace| -> Vec<NodeId> {
        ws.collect_entity_bounds(Some(room))
            .into_iter()
            .map(|b| b.node)
            .collect()
    };

    // Active floor 0: only the ground entity is selectable; the upper
    // floor is hidden (above), so its entity is not.
    workspace.active_floor = 0;
    let f0 = bound_nodes(&workspace);
    assert!(f0.contains(&ground), "ground selectable on floor 0: {f0:?}");
    assert!(
        !f0.contains(&upper),
        "upper-floor entity not selectable from floor 0: {f0:?}"
    );

    // Active floor 1: both are selectable (active + below for Sims
    // context), and the ground entity's bound is offset below.
    workspace.active_floor = 1;
    let bounds = workspace.collect_entity_bounds(Some(room));
    let nodes: Vec<NodeId> = bounds.iter().map(|b| b.node).collect();
    assert!(
        nodes.contains(&ground) && nodes.contains(&upper),
        "both floors selectable from floor 1: {nodes:?}"
    );
    let upper_y = bounds.iter().find(|b| b.node == upper).unwrap().center[1];
    let ground_y = bounds.iter().find(|b| b.node == ground).unwrap().center[1];
    assert!(
            ground_y < upper_y,
            "ground entity bound sits below the upper one (offset by floor): ground={ground_y} upper={upper_y}"
        );
}

/// Diagnostic (not a strict assertion): print an ASCII map of what a
/// click resolves to across the gizmo region, at several zoom levels.
/// Run with `cargo test gizmo_pick_map -- --nocapture` to eyeball
/// where the tile (`#`) leaks in among the handles.
#[test]
fn gizmo_pick_map_diagnostic() {
    // Big room so the floor fills the view behind the gizmo at every
    // zoom -- gaps in the handle pick then show as '#' (tile), the way
    // they do in a real level, not '.' (ray missed the small floor).
    let mut harness = ViewportHarness::floored_room("gizmo-pick-map", 24);
    let light = harness.add_centre_light(0.25);
    harness.select(light);
    let target = harness.room_center();

    // Sweep camera elevation (degrees above the horizon) at a fixed
    // moderate distance: the flat XZ ground plane foreshortens as the
    // angle gets shallow, the regime the user hits when orbiting to a
    // near-horizontal view.
    println!("##### ELEVATION SWEEP (dist 12000) #####");
    for &deg in &[60.0_f32, 40.0, 25.0, 15.0, 8.0, 4.0] {
        harness.frame_at_elevation(target, 12_000.0, deg.to_radians());
        let xz_area = harness
            .workspace
            .node_gizmo_screen_planes(harness.viewport)
            .into_iter()
            .find(|p| p.plane == NodeGizmoPlane::XZ)
            .map(|p| polygon_area_2d(&p.corners).abs());
        // Where the XZ plane centre projects, whether or not it is
        // culled (a point lerped along +X/+Z from the pivot).
        let pivot = harness
            .workspace
            .node_gizmo_bounds_3d(&[harness.workspace.selection.selected_node])
            .map(|(p, _)| p)
            .unwrap_or([0.0, 0.0, 0.0]);
        let probe_world = [pivot[0] + 300.0, pivot[1], pivot[2] + 300.0];
        let at_center = project_world_to_viewport_screen(
            harness.workspace.viewport_3d_camera(),
            harness.viewport,
            probe_world,
        )
        .map(|p| harness.classify(p));
        println!(
            "elev {deg:>4}deg: XZ area = {:>8}  click-at-XZ-region = {:?}",
            xz_area
                .map(|a| format!("{a:.1}"))
                .unwrap_or_else(|| "CULLED".to_string()),
            at_center,
        );
    }

    for &distance in &[4_000.0, 8_000.0, 16_000.0, 32_000.0, 60_000.0] {
        harness.frame(target, distance);
        // Bounding box over all axis + plane handles, with margin.
        let mut pts: Vec<Pos2> = harness
            .workspace
            .node_gizmo_screen_axes(harness.viewport)
            .into_iter()
            .flat_map(|a| [a.start, a.end])
            .collect();
        for plane in NodeGizmoPlane::ALL {
            if let Some(q) = harness.plane_quad(plane) {
                pts.extend(q);
            }
        }
        if pts.is_empty() {
            println!("dist {distance}: no gizmo projected");
            continue;
        }
        let pad = 16.0;
        let min_x = pts.iter().map(|p| p.x).fold(f32::INFINITY, f32::min) - pad;
        let max_x = pts.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max) + pad;
        let min_y = pts.iter().map(|p| p.y).fold(f32::INFINITY, f32::min) - pad;
        let max_y = pts.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max) + pad;
        let cols = 70usize;
        let rows = 30usize;
        println!("=== dist {distance} | x[{min_x:.0}..{max_x:.0}] y[{min_y:.0}..{max_y:.0}] ===",);
        for r in 0..rows {
            let mut line = String::with_capacity(cols);
            for c in 0..cols {
                let probe = Pos2::new(
                    min_x + (max_x - min_x) * c as f32 / (cols - 1) as f32,
                    min_y + (max_y - min_y) * r as f32 / (rows - 1) as f32,
                );
                line.push(harness.classify(probe));
            }
            println!("{line}");
        }
    }
}

#[test]
fn node_gizmo_no_tile_inside_plane_footprint() {
    // Regression for "clicking near the plane grabs the tile when
    // zoomed out". A move plane is drawn as a small inset square in
    // the corner between its two axes, but the user reads the whole
    // corner as the handle. The pick covered only the small square (+
    // a few px) and the axis tubes, leaving wedges of bare tile
    // between them. This sweeps each plane's footprint triangle
    // (pivot + the two axis endpoints) and requires every interior
    // point to resolve to a gizmo handle, never a floor Surface.
    // Big room so the floor is always behind the gizmo.
    let mut harness = ViewportHarness::floored_room("gizmo-footprint", 24);
    let light = harness.add_centre_light(0.25);
    harness.select(light);
    let target = harness.room_center();

    let mut checked = 0;
    for &distance in &[6_000.0, 10_000.0, 16_000.0, 24_000.0] {
        harness.frame(target, distance);
        for plane in NodeGizmoPlane::ALL {
            let Some(tri) = harness.plane_footprint_triangle(plane) else {
                continue;
            };
            // Barycentric sweep of the triangle interior.
            let steps = 12;
            for i in 0..=steps {
                for j in 0..=(steps - i) {
                    let a = i as f32 / steps as f32;
                    let b = j as f32 / steps as f32;
                    let c = 1.0 - a - b;
                    // Stay strictly interior so we test the wedge, not
                    // the axis edges themselves.
                    if a < 0.08 || b < 0.08 || c < 0.08 {
                        continue;
                    }
                    let probe = Pos2::new(
                        tri[0].x * c + tri[1].x * a + tri[2].x * b,
                        tri[0].y * c + tri[1].y * a + tri[2].y * b,
                    );
                    checked += 1;
                    let resolved = harness.resolve(probe);
                    assert!(
                        !matches!(resolved, Some(Viewport3dPointerTarget::Surface { .. })),
                        "dist {distance}, {plane:?} footprint: click at {probe:?} \
                             grabbed the tile ({resolved:?}) instead of a gizmo handle",
                    );
                }
            }
        }
    }
    assert!(
        checked >= 50,
        "footprint sweep covered too few points ({checked})"
    );
}

#[test]
fn node_gizmo_xz_plane_grabbable_across_zoom_out() {
    let mut harness = ViewportHarness::floored_room("gizmo-zoom-grab", 4);
    let light = harness.add_centre_light(0.25);
    harness.select(light);
    let target = harness.room_center();

    // Regression: the green XZ ground plane must stay grabbable as the
    // camera pulls back. The bug was a click inside the plane quad
    // grabbing an axis (which, on a foreshortened quad, read as the
    // floor tile behind it) because axes were picked first and
    // short-circuited the plane test.
    //
    // The three move-plane quads overlap on screen once foreshortened,
    // so in the overlap zone which plane wins is genuinely ambiguous
    // and not worth asserting. We instead sample a dense grid over the
    // XZ quad's bounding box, keep only points that are inside XZ and
    // outside XY and YZ -- the region unambiguously "on the XZ handle"
    // -- and require every one of those to grab XZ. The centre is
    // always required to be such a point and to grab XZ.
    for &distance in &[3_000.0, 6_000.0, 12_000.0, 20_000.0] {
        harness.frame(target, distance);
        let xz = harness.plane_quad(NodeGizmoPlane::XZ).unwrap_or_else(|| {
            panic!("XZ plane should still project when framed from {distance} units")
        });
        let others: Vec<[Pos2; 4]> = [NodeGizmoPlane::XY, NodeGizmoPlane::YZ]
            .into_iter()
            .filter_map(|plane| harness.plane_quad(plane))
            .collect();
        let exclusive_to_xz = |p: Pos2| {
            point_in_polygon_2d(p, &xz) && others.iter().all(|quad| !point_in_polygon_2d(p, quad))
        };

        let center = {
            let sum = xz.iter().fold(Vec2::ZERO, |acc, c| acc + c.to_vec2());
            Pos2::new(sum.x / 4.0, sum.y / 4.0)
        };
        assert!(
            exclusive_to_xz(center),
            "framed from {distance} units, the XZ quad centre {center:?} is not \
                 exclusively on XZ -- the quads overlap their own centre, revisit the test",
        );

        let min_x = xz.iter().map(|c| c.x).fold(f32::INFINITY, f32::min);
        let max_x = xz.iter().map(|c| c.x).fold(f32::NEG_INFINITY, f32::max);
        let min_y = xz.iter().map(|c| c.y).fold(f32::INFINITY, f32::min);
        let max_y = xz.iter().map(|c| c.y).fold(f32::NEG_INFINITY, f32::max);
        let mut checked = 0;
        for ix in 0..=10 {
            for iy in 0..=10 {
                let probe = Pos2::new(
                    min_x + (max_x - min_x) * ix as f32 / 10.0,
                    min_y + (max_y - min_y) * iy as f32 / 10.0,
                );
                if !exclusive_to_xz(probe) {
                    continue;
                }
                checked += 1;
                assert_eq!(
                    harness.gizmo_handle_at(probe),
                    Some(NodeGizmoHandle::Plane(NodeGizmoPlane::XZ)),
                    "framed from {distance} units, a click at {probe:?} \
                         exclusively on the XZ quad did not grab the XZ plane",
                );
            }
        }
        assert!(
            checked >= 4,
            "framed from {distance} units, only {checked} probes were exclusively \
                 on XZ -- too few to be a meaningful regression check",
        );
    }
}

#[test]
fn node_gizmo_plane_click_never_falls_through_to_tile() {
    // The user-visible symptom was the click "grabbing the underlying
    // tile". That happened in the thin band *just outside* a
    // foreshortened plane quad: too far for strict polygon containment
    // (so the old plane test missed) and too far from any axis (so the
    // axis test missed too), leaving `pick_node_gizmo_handle` returning
    // None and the click falling through to the floor Surface.
    //
    // We reconstruct exactly that band: scan a grid around the XZ quad
    // and keep points that are outside every plane quad AND outside the
    // axis pick radius but within the plane pick tolerance of XZ. Under
    // the old code each such point resolved to a Surface (tile); with
    // the fix each must resolve to the XZ plane handle. The final
    // assert that the band was non-empty stops this silently becoming a
    // no-op if the projection ever changes.
    let mut harness = ViewportHarness::floored_room("gizmo-no-tile-fallthrough", 4);
    let light = harness.add_centre_light(0.25);
    harness.select(light);
    let target = harness.room_center();

    let mut total_band_points = 0;
    for &distance in &[6_000.0, 12_000.0, 20_000.0] {
        harness.frame(target, distance);
        let xz = harness
            .plane_quad(NodeGizmoPlane::XZ)
            .unwrap_or_else(|| panic!("XZ plane should project when framed from {distance} units"));
        let other_quads: Vec<[Pos2; 4]> = [NodeGizmoPlane::XY, NodeGizmoPlane::YZ]
            .into_iter()
            .filter_map(|plane| harness.plane_quad(plane))
            .collect();

        let min_x = xz.iter().map(|c| c.x).fold(f32::INFINITY, f32::min) - GIZMO_PLANE_PICK_RADIUS;
        let max_x =
            xz.iter().map(|c| c.x).fold(f32::NEG_INFINITY, f32::max) + GIZMO_PLANE_PICK_RADIUS;
        let min_y = xz.iter().map(|c| c.y).fold(f32::INFINITY, f32::min) - GIZMO_PLANE_PICK_RADIUS;
        let max_y =
            xz.iter().map(|c| c.y).fold(f32::NEG_INFINITY, f32::max) + GIZMO_PLANE_PICK_RADIUS;

        for ix in 0..=24 {
            for iy in 0..=24 {
                let probe = Pos2::new(
                    min_x + (max_x - min_x) * ix as f32 / 24.0,
                    min_y + (max_y - min_y) * iy as f32 / 24.0,
                );
                // The fallthrough band: outside every quad (strict),
                // beyond axis radius, but within plane tolerance of XZ.
                let outside_all_quads = !point_in_polygon_2d(probe, &xz)
                    && other_quads
                        .iter()
                        .all(|quad| !point_in_polygon_2d(probe, quad));
                let beyond_axes = harness.nearest_axis_distance(probe) > GIZMO_AXIS_PICK_RADIUS;
                let within_xz_tolerance =
                    distance_to_polygon_edges_2d(probe, &xz) <= GIZMO_PLANE_PICK_RADIUS;
                if !(outside_all_quads && beyond_axes && within_xz_tolerance) {
                    continue;
                }
                total_band_points += 1;
                // The symptom was grabbing the tile, so the invariant
                // is "not a Surface", not a specific plane: a band
                // point can sit within tolerance of XZ and a
                // neighbouring plane at once, and either gizmo handle is
                // a correct, non-fallthrough result.
                let resolved = harness.resolve(probe);
                assert!(
                    matches!(resolved, Some(Viewport3dPointerTarget::NodeGizmo(_))),
                    "framed from {distance} units, a click at {probe:?} in the XZ \
                         tolerance band fell through to {resolved:?} instead of a gizmo handle",
                );
            }
        }
    }
    assert!(
        total_band_points >= 3,
        "expected the XZ fallthrough band to contain probe points across zoom levels, \
             found {total_band_points} -- the test is no longer exercising the bug",
    );
}

fn assert_pos_approx(actual: Pos2, expected: Pos2) {
    assert!((actual.x - expected.x).abs() < 0.001);
    assert!((actual.y - expected.y).abs() < 0.001);
}

fn assert_size_approx(actual: Vec2, expected: Vec2) {
    assert!((actual.x - expected.x).abs() < 0.001);
    assert!((actual.y - expected.y).abs() < 0.001);
}

#[test]
fn ui_resize_handles_remain_hittable_outside_canvas_for_border_images() {
    let mut scene = psxed_project::UiScene::default_hud();
    let image = scene.add_node(
        scene.root,
        "Image".to_string(),
        UiNodeKind::Image {
            rect: UiRect::new(0, 0, 64, 64),
            texture: None,
            tint: [128, 128, 128],
            effect: UiImageEffect::None,
        },
    );
    let canvas = Rect::from_min_size(Pos2::ZERO, Vec2::new(320.0, 240.0));
    let hidden_ui_nodes = HashSet::new();

    assert_eq!(
        ui_scene_resize_handle_target(
            &scene,
            &hidden_ui_nodes,
            image,
            canvas,
            [320, 240],
            Pos2::new(-4.0, -4.0)
        ),
        Some((image, UiResizeHandle::TopLeft))
    );
}

#[test]
fn ui_center_snap_aligns_node_to_canvas_midpoints() {
    let result = snap_ui_rect_to_canvas_center(UiRect::new(121, 109, 80, 20), [320, 240]);

    assert!(result.snap_x);
    assert!(result.snap_y);
    assert_eq!(result.rect, UiRect::new(120, 110, 80, 20));
}

#[test]
fn ui_center_snap_can_snap_one_axis_without_the_other() {
    let result = snap_ui_rect_to_canvas_center(UiRect::new(117, 114, 80, 20), [320, 240]);

    assert!(result.snap_x);
    assert!(!result.snap_y);
    assert_eq!(result.rect, UiRect::new(120, 114, 80, 20));
}

#[test]
fn ui_center_snap_applies_absolute_delta_to_anchored_local_rects() {
    let local = UiRect::new(0, 0, 80, 20).with_anchor(UiAnchor::Center);
    let absolute = UiRect::new(121, 109, 80, 20);

    let result = snap_moved_ui_rect_to_canvas_center(local, absolute, [320, 240]);

    assert!(result.snap_x);
    assert!(result.snap_y);
    assert_eq!(
        result.rect,
        UiRect::new(-1, 1, 80, 20).with_anchor(UiAnchor::Center)
    );
}

#[test]
fn ui_center_snap_leaves_rects_outside_tolerance_unchanged() {
    let rect = UiRect::new(112, 120, 80, 20);

    let result = snap_ui_rect_to_canvas_center(rect, [320, 240]);

    assert!(!result.snap_x);
    assert!(!result.snap_y);
    assert_eq!(result.rect, rect);
}

fn test_node_preview_origin(project: &ProjectDocument, room: NodeId, node: NodeId) -> [i32; 3] {
    let scene = project.active_scene();
    let room_node = scene.node(room).expect("room exists");
    let NodeKind::Room { grid } = &room_node.kind else {
        panic!("expected room");
    };
    let node = scene.node(node).expect("node exists");
    psxed_project::spatial::node_preview_origin(grid, &node.transform)
}

#[test]
fn room_grid_grow_preserves_spatial_descendant_preview_position() {
    let mut project = ProjectDocument::new("grid-grow");
    let scene = project.active_scene_mut();
    let room = scene.add_node(
        scene.root,
        "Room",
        NodeKind::Room {
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
fn focus_shortcut_preserves_orbit_distance() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    workspace.camera_rig.mode = ViewportCameraMode::Orbit;
    workspace.camera_rig.radius = 12_345;
    workspace.camera_rig.yaw = 256;
    workspace.camera_rig.pitch = 256;

    workspace.focus_3d_on_point_preserving_distance([4096.0, 512.0, -2048.0]);

    assert_eq!(workspace.camera_rig.target, [4096, 512, -2048]);
    assert_eq!(workspace.camera_rig.radius, 12_345);
    assert_eq!(workspace.camera_rig.yaw, 256);
    assert_eq!(workspace.camera_rig.pitch, 256);
}

#[test]
fn focus_shortcut_in_free_mode_keeps_position_and_points_at_target() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    workspace.camera_rig.mode = ViewportCameraMode::Free;
    workspace.camera_rig.free_position = [0, 0, 0];
    workspace.camera_rig.free_yaw = 1024;
    workspace.camera_rig.free_pitch = signed_to_q12(300);
    workspace.camera_rig.free_initialized = true;

    workspace.focus_3d_on_point_preserving_distance([0.0, 0.0, -4096.0]);

    assert_eq!(workspace.camera_rig.free_position, [0, 0, 0]);
    assert_eq!(workspace.camera_rig.target, [0, 0, -4096]);
    assert_eq!(workspace.camera_rig.radius, 4096);
    assert_eq!(workspace.camera_rig.free_yaw, 0);
    assert_eq!(workspace.camera_rig.free_pitch, 0);
}

#[test]
fn editor_camera_saves_with_project_and_restores_on_open() {
    let project_dir = test_temp_dir("editor-camera");
    let mut project = ProjectDocument::new("editor-camera");
    project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Room {
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
    workspace.active_workspace = WorkspaceView::Ui;

    workspace.save().unwrap();

    let reopened = EditorWorkspace::open_directory(&project_dir).unwrap();
    assert_eq!(reopened.active_workspace, WorkspaceView::Ui);

    let _ = std::fs::remove_dir_all(project_dir);
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
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });

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
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });

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
        NodeKind::Room {
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
        NodeKind::Room {
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
fn menu_labels_include_discoverable_shortcut_text() {
    assert_eq!(menu_label("Save", "Cmd+S"), "Save    Cmd+S");
}

#[test]
fn available_animation_clips_scan_project_relative_psxanim_files() {
    let dir = test_temp_dir("animation-clips-scan");
    let model_dir = dir.join("assets/models/wraith");
    std::fs::create_dir_all(&model_dir).unwrap();
    std::fs::write(model_dir.join("idle.psxanim"), []).unwrap();
    std::fs::write(model_dir.join("walk.PSXANIM"), []).unwrap();
    std::fs::write(model_dir.join("notes.txt"), []).unwrap();
    std::fs::create_dir_all(dir.join(".hidden")).unwrap();
    std::fs::write(dir.join(".hidden/ghost.psxanim"), []).unwrap();
    std::fs::create_dir_all(dir.join("target/debug")).unwrap();
    std::fs::write(dir.join("target/debug/generated.psxanim"), []).unwrap();

    let clips = available_animation_clips(&dir);
    let paths: Vec<&str> = clips.iter().map(|clip| clip.stored_path.as_str()).collect();

    assert_eq!(
        paths,
        vec![
            "assets/models/wraith/idle.psxanim",
            "assets/models/wraith/walk.PSXANIM"
        ]
    );
    assert_eq!(clips[0].default_name, "idle");
    assert_eq!(clips[0].label, "idle (wraith)");

    let _ = std::fs::remove_dir_all(dir);
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
fn selecting_animation_clip_source_updates_placeholder_names_only() {
    let mut placeholder = psxed_project::ModelAnimationClip {
        name: "clip_0".to_string(),
        psxanim_path: String::new(),
        calibration: Default::default(),
    };
    assert!(set_model_animation_clip_source(
        &mut placeholder,
        "assets/models/wraith/run.psxanim"
    ));
    assert_eq!(placeholder.psxanim_path, "assets/models/wraith/run.psxanim");
    assert_eq!(placeholder.name, "run");

    let mut default_named = psxed_project::ModelAnimationClip {
        name: "idle".to_string(),
        psxanim_path: "assets/models/wraith/idle.psxanim".to_string(),
        calibration: Default::default(),
    };
    assert!(set_model_animation_clip_source(
        &mut default_named,
        "assets/models/wraith/walk.psxanim"
    ));
    assert_eq!(default_named.name, "walk");

    let mut custom_named = psxed_project::ModelAnimationClip {
        name: "Combat Idle".to_string(),
        psxanim_path: "assets/models/wraith/idle.psxanim".to_string(),
        calibration: Default::default(),
    };
    assert!(set_model_animation_clip_source(
        &mut custom_named,
        "assets/models/wraith/walk.psxanim"
    ));
    assert_eq!(custom_named.name, "Combat Idle");
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
            clips: vec![psxed_project::ModelAnimationClip {
                name: "walking".to_string(),
                psxanim_path: "assets/models/obsidian_warden/obsidian_warden_walking.psxanim"
                    .to_string(),
                calibration: Default::default(),
            }],
            default_clip: Some(0),
            preview_clip: Some(0),
            world_height: 1024,
            collision_radius: default_model_collision_radius_for_height(1024),
            scale_q8: [psxed_project::MODEL_SCALE_ONE_Q8; 3],
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
    let saved = ProjectDocument::load_from_path(target.join("project.ron")).unwrap();
    assert_eq!(saved.name, ws.project().name);
    let _ = std::fs::remove_dir_all(target);
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

    assert!(error.contains("editor/projects"));
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

#[test]
fn dragging_selected_node_moves_it_in_xz_space() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    let spawn = starter_player_entity(workspace.project.active_scene()).id;
    let sector_size = node_enclosing_sector_size(workspace.project.active_scene(), spawn);
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
        node_transform_inspector(&NodeKind::Room {
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
        .find(|node| matches!(node.kind, NodeKind::Room { .. }))
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
        NodeKind::Room {
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
        NodeKind::Room {
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
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
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
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
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
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
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
    let NodeKind::Room { grid } = &node.kind else {
        panic!("active room is a room node");
    };
    assert!(grid.sector(coords[0].0, coords[0].1).is_none());
    assert!(grid.sector(coords[1].0, coords[1].1).is_none());
    assert!(workspace.selection.selected_sectors.is_empty());
}

#[test]
fn autotile_selected_sector_walls_updates_all_selected_tiles() {
    let mut project = ProjectDocument::new("autotile-selected-tiles");
    let mut grid = WorldGrid::empty(2, 1, 1024);
    for sx in 0..=1 {
        grid.add_wall(sx, 0, GridDirection::North, 0, 2048, None);
    }
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
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
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
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
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
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
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
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
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
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
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
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
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
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
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
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

    assert!(workspace.commit_floating_geometry());
    assert!(workspace.floating_geometry.is_none());
    assert!(workspace.is_dirty());

    workspace.do_undo();
    let grid = workspace.room_grid_view(room).unwrap();
    assert_eq!((grid.width, grid.depth), (1, 1));
    assert!(grid.sector(0, 1).is_none());
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
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
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
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
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
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
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
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
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
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
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
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
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
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
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
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
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
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
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
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
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
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
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
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
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
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
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
        NodeKind::Room {
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
        NodeKind::Room {
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
        NodeKind::Room {
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

    workspace.update_node_gizmo_drag(viewport, target_pointer);
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
fn node_gizmo_moves_entity_on_selected_axis() {
    let mut project = ProjectDocument::new("entity-gizmo-x");
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Room {
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
    workspace.update_node_gizmo_drag(viewport, x_axis.start + unit * 4.0);
    workspace.end_node_gizmo_drag();

    let node = workspace.project.active_scene().node(entity).unwrap();
    assert!((node.transform.translation[0] - HEIGHT_QUANTUM as f32 / 1024.0).abs() < 0.001);
    assert_eq!(node.transform.translation[1], 0.0);
    assert_eq!(node.transform.translation[2], 0.0);
    let scene = workspace.project.active_scene();
    let room_node = scene.node(room).unwrap();
    let NodeKind::Room { grid } = &room_node.kind else {
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
        NodeKind::Room {
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
    workspace.update_node_gizmo_drag(viewport, y_axis.start + unit * 4.0);
    workspace.end_node_gizmo_drag();

    let node = workspace.project.active_scene().node(light).unwrap();
    assert_vec3_approx(
        node.transform.translation,
        [0.0, HEIGHT_QUANTUM as f32 / 1024.0, 0.0],
    );
    let scene = workspace.project.active_scene();
    let room_node = scene.node(room).unwrap();
    let NodeKind::Room { grid } = &room_node.kind else {
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
        NodeKind::Room {
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
    let start = ring.points[0];
    let unit = (start - ring.center).normalized();
    assert!(workspace.begin_node_gizmo_drag(PrimitiveGizmoAxis::Y, viewport, start));
    workspace.update_node_gizmo_drag(viewport, start + unit * 24.0);
    workspace.end_node_gizmo_drag();

    let node = workspace.project.active_scene().node(prop).unwrap();
    assert_eq!(node.transform.rotation_degrees, [0.0, 2.0, 0.0]);
    assert!(workspace.is_dirty());

    workspace.do_undo();
    let node = workspace.project.active_scene().node(prop).unwrap();
    assert_eq!(node.transform.rotation_degrees, [0.0, 0.0, 0.0]);
}

#[test]
fn node_gizmo_scales_image_prop_width() {
    let mut project = ProjectDocument::new("image-prop-gizmo-scale");
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Room {
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
    workspace.update_node_gizmo_drag(viewport, x_axis.start + unit * 8.0);
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
    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("box-prop-gizmo-scale"), project);
    set_gizmo_test_camera(&mut workspace);
    workspace.replace_node_selection(prop);
    workspace.transform_gizmo_mode = TransformGizmoMode::Scale;

    let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
    let x_axis = projected_node_gizmo_axis(&workspace, viewport, PrimitiveGizmoAxis::X);
    let unit = (x_axis.end - x_axis.start).normalized();
    assert!(workspace.begin_node_gizmo_drag(PrimitiveGizmoAxis::X, viewport, x_axis.start));
    workspace.update_node_gizmo_drag(viewport, x_axis.start + unit * 8.0);
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
fn duplicate_wall_cook_error_marks_both_authored_faces() {
    let mut project = ProjectDocument::new("duplicate-wall");
    let mut grid = WorldGrid::empty(4, 2, 1024);
    grid.add_wall(3, 1, GridDirection::South, 0, 1024, None);
    grid.add_wall(3, 0, GridDirection::North, 0, 1024, None);
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
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
        ResourceData::Texture {
            psxt_path: "assets/textures/delven_01_slateflr1a_q2.psxt".to_string(),
        },
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
            clips: Vec::new(),
            default_clip: None,
            preview_clip: None,
            world_height: 1024,
            collision_radius: default_model_collision_radius_for_height(1024),
            scale_q8: [MODEL_SCALE_ONE_Q8; 3],
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
        &psxed_project::default_project_dir(),
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
        grid.add_wall(sx, 0, GridDirection::North, 0, 1024, Some(original));
    }
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
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
fn material_click_assignment_updates_selected_box_prop_faces() {
    let mut project = ProjectDocument::new("box-prop-materials");
    let target = project.add_resource(
        "Target",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
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
fn texture_click_assignment_creates_material_for_selected_box_prop() {
    let mut project = ProjectDocument::new("box-prop-texture");
    let texture = project.add_resource(
        "Brick",
        ResourceData::Texture {
            psxt_path: "assets/textures/brick.psxt".to_string(),
        },
    );
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
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.replace_node_selection(prop);

    let assignment = workspace
        .assign_selected_box_props_resource(texture)
        .expect("texture wraps into a material for selected box prop");
    assert_eq!(assignment.updated, 1);
    assert_ne!(assignment.material, texture);
    let material = workspace.project.resource(assignment.material).unwrap();
    assert!(matches!(
        &material.data,
        ResourceData::Material(material) if material.texture == Some(texture)
    ));

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
        NodeKind::Room {
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
        NodeKind::Room {
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
fn place_player_spawn_refuses_second_player_source() {
    let (mut workspace, room) = workspace_with_populated_grid("single-player-spawn", 1, 1);
    workspace.active_tool = ViewTool::Place;
    workspace.place_kind = PlaceKind::PlayerSpawn;

    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [512.0, 0.0, 512.0]);
    let first_spawn = workspace.selected_node_id();
    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [768.0, 0.0, 768.0]);

    let player_sources: Vec<_> = workspace
        .project
        .active_scene()
        .nodes()
        .iter()
        .filter(|node| node_kind_is_player_source(&node.kind))
        .collect();
    assert_eq!(player_sources.len(), 1);
    assert_eq!(player_sources[0].id, first_spawn);
    assert_eq!(workspace.selected_node_id(), first_spawn);
    assert!(workspace
        .status
        .contains("Only one player source is allowed per world"));
}

#[test]
fn place_prop_refuses_duplicate_resource_at_same_position() {
    let (mut workspace, room) = workspace_with_populated_grid("duplicate-prop-place", 1, 1);
    let model = workspace
        .project
        .add_resource("Crate", ResourceData::Model(test_model_resource("crate")));
    workspace.active_tool = ViewTool::Place;
    workspace.place_kind = PlaceKind::ModelInstance;
    workspace.place_resource = Some(model);

    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [512.0, 0.0, 512.0]);
    let first = workspace.selected_node_id();
    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [512.0, 0.0, 512.0]);

    let scene = workspace.project.active_scene();
    let props: Vec<_> = scene
        .nodes()
        .iter()
        .filter(|node| {
            node.parent == Some(room)
                && matches!(node.kind, NodeKind::Entity)
                && entity_model_resource_id(scene, node) == Some(model)
                && entity_character_component_resource_id(scene, node).is_none()
                && entity_weapon_resource_id(scene, node).is_none()
        })
        .collect();
    assert_eq!(props.len(), 1);
    assert_eq!(props[0].id, first);
    assert_eq!(workspace.selected_node_id(), first);
    assert_eq!(workspace.status, "Prop already exists at this position");
}

#[test]
fn place_spawn_marker_refuses_duplicate_at_same_position() {
    let (mut workspace, room) = workspace_with_populated_grid("duplicate-spawn-place", 1, 1);
    workspace.active_tool = ViewTool::Place;
    workspace.place_kind = PlaceKind::SpawnMarker;

    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [512.0, 0.0, 512.0]);
    let first = workspace.selected_node_id();
    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [512.0, 0.0, 512.0]);

    let spawns: Vec<_> = workspace
        .project
        .active_scene()
        .nodes()
        .iter()
        .filter(|node| {
            node.parent == Some(room)
                && matches!(
                    node.kind,
                    NodeKind::SpawnPoint {
                        player: false,
                        character: None
                    }
                )
        })
        .collect();
    assert_eq!(spawns.len(), 1);
    assert_eq!(spawns[0].id, first);
    assert_eq!(workspace.selected_node_id(), first);
    assert_eq!(workspace.status, "Spawn already exists at this position");
}

#[test]
fn place_image_prop_defaults_to_room_sector_size() {
    let mut project = ProjectDocument::new("image-prop-sector-size-place");
    let material = project.add_resource(
        "Banner",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Room {
            grid: WorldGrid::empty(1, 1, 1536),
        },
    );
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.place_kind = PlaceKind::ImageProp;
    workspace.replace_resource_selection(material);

    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [768.0, 0.0, 768.0]);

    let node = workspace
        .project
        .active_scene()
        .node(workspace.selected_node_id())
        .expect("placed image prop is selected");
    let NodeKind::ImageProp { width, height, .. } = &node.kind else {
        panic!("expected image prop node");
    };
    assert_eq!(*width, 1536);
    assert_eq!(*height, 1536);
}

#[test]
fn place_image_prop_with_texture_creates_material_wrapper() {
    let mut project = ProjectDocument::new("image-prop-texture-place");
    let texture = project.add_resource(
        "Crimson Banner",
        ResourceData::Texture {
            psxt_path: "assets/textures/crimson_banner.psxt".to_string(),
        },
    );
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Room {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.place_kind = PlaceKind::ImageProp;
    workspace.place_resource = Some(texture);

    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [512.0, 128.0, 512.0]);

    let node = workspace
        .project
        .active_scene()
        .node(workspace.selected_node_id())
        .expect("placed image prop is selected");
    let NodeKind::ImageProp {
        material: Some(material),
        ..
    } = &node.kind
    else {
        panic!("expected image prop node");
    };
    assert_ne!(*material, texture);
    assert_eq!(workspace.place_resource, Some(*material));
    let Some(resource) = workspace.project.resource(*material) else {
        panic!("created material resource exists");
    };
    let ResourceData::Material(material_resource) = &resource.data else {
        panic!("created resource is a material");
    };
    assert_eq!(resource.name, "Crimson Banner");
    assert_eq!(material_resource.texture, Some(texture));
    assert_eq!(workspace.status, "Placed Image Prop at 0,0");
    assert!(workspace.is_dirty());
}

#[test]
fn portal_icon_place_writes_edge_midpoint_marker() {
    let mut project = ProjectDocument::new("portal-edge-place");
    let mut grid = WorldGrid::empty(2, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    grid.set_floor(1, 0, 0, None);
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.active_tool = ViewTool::Place;
    workspace.place_kind = PlaceKind::Portal;
    workspace.portal_place_direction = GridDirection::East;

    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [512.0, 0.0, 512.0]);

    let scene = workspace.project.active_scene();
    let node = scene
        .node(workspace.selected_node_id())
        .expect("placed portal is selected");
    assert!(matches!(node.kind, NodeKind::Portal { .. }));
    assert_eq!(node.transform.translation, [0.0, 0.0, 0.0]);
    assert_eq!(workspace.active_tool, ViewTool::Select);
    let grid = workspace.room_grid_view(room).unwrap();
    let edge = portal_edge_for_node(grid, node).expect("portal snaps to edge");
    assert_eq!(edge.x, 0);
    assert_eq!(edge.z, 0);
    assert_eq!(edge.direction, GridDirection::East);
    assert_eq!(workspace.status, "Placed Portal on East edge at 0,0");
    assert!(workspace.is_dirty());
}

#[test]
fn portal_icon_place_refuses_duplicate_edge_marker() {
    let mut project = ProjectDocument::new("portal-edge-duplicate");
    let mut grid = WorldGrid::empty(2, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    grid.set_floor(1, 0, 0, None);
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.active_tool = ViewTool::Place;
    workspace.place_kind = PlaceKind::Portal;
    workspace.portal_place_direction = GridDirection::East;

    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [512.0, 0.0, 512.0]);
    let first = workspace.selected_node_id();
    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [512.0, 0.0, 512.0]);

    let portals: Vec<_> = workspace
        .project
        .active_scene()
        .nodes()
        .iter()
        .filter(|node| node.parent == Some(room) && matches!(node.kind, NodeKind::Portal { .. }))
        .collect();
    assert_eq!(portals.len(), 1);
    assert_eq!(portals[0].id, first);
    assert_eq!(workspace.selected_node_id(), first);
    assert_eq!(
        workspace.status,
        "Portal already exists on East edge at 0,0"
    );
}

#[test]
fn visible_portal_bounds_follow_the_authored_seam() {
    let mut project = ProjectDocument::new("portal-seam-bounds");
    let mut grid = WorldGrid::empty(2, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    grid.set_floor(1, 0, 0, None);
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.active_tool = ViewTool::Place;
    workspace.place_kind = PlaceKind::Portal;
    workspace.portal_place_direction = GridDirection::East;
    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [512.0, 0.0, 512.0]);

    let portal = workspace.selected_node_id();
    let bounds = workspace
        .collect_entity_bounds(Some(room))
        .into_iter()
        .find(|bounds| bounds.node == portal)
        .expect("visible portal has seam bounds");
    assert_eq!(bounds.kind, EntityBoundKind::Portal);
    assert!((bounds.center[0] - 1024.0).abs() < 0.001);
    assert!((bounds.center[2] - 512.0).abs() < 0.001);
    assert!(bounds.half_extents[0] >= 48.0);
    assert!(bounds.half_extents[2] >= 512.0);
    let t = ray_intersects_aabb(
        [
            bounds.center[0] - 4096.0,
            bounds.center[1],
            bounds.center[2],
        ],
        [1.0, 0.0, 0.0],
        bounds.center,
        bounds.half_extents,
    );
    assert!(t.is_some(), "portal seam bound is pickable");

    workspace.show_portals = false;
    assert!(workspace
        .collect_entity_bounds(Some(room))
        .into_iter()
        .all(|bounds| bounds.node != portal));
}

#[test]
fn portal_icon_place_rejects_edges_without_populated_neighbour() {
    let mut project = ProjectDocument::new("portal-edge-invalid");
    let mut grid = WorldGrid::empty(2, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.active_tool = ViewTool::Place;
    workspace.place_kind = PlaceKind::Portal;
    workspace.portal_place_direction = GridDirection::East;

    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [512.0, 0.0, 512.0]);

    let portal_count = workspace
        .project
        .active_scene()
        .nodes()
        .iter()
        .filter(|node| matches!(node.kind, NodeKind::Portal { .. }))
        .count();
    assert_eq!(portal_count, 0);
    assert_eq!(
        workspace.status,
        "Portal needs populated sectors on both sides of the East edge"
    );
    assert!(!workspace.is_dirty());
}

#[test]
fn material_assignment_updates_selected_triangle_override() {
    let mut project = ProjectDocument::new("triangle-materials");
    let original = project.add_resource(
        "Original",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let target = project.add_resource(
        "Target",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.set_floor(0, 0, 0, Some(original));
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    let triangle = HorizontalTriangleRef {
        room,
        sx: 0,
        sz: 0,
        surface: HorizontalSurfaceKind::Floor,
        index: HorizontalTriangleIndex::A,
        corners: [Corner::NW, Corner::NE, Corner::SE],
    };
    workspace.selection.selected_primitive = Some(Selection::Triangle(triangle));

    assert_eq!(workspace.assign_selected_faces_material(Some(target)), 1);

    let grid = workspace.room_grid_view(room).unwrap();
    let floor = grid.sector(0, 0).unwrap().floor.as_ref().unwrap();
    assert_eq!(floor.material, Some(original));
    assert_eq!(floor.triangle_material(0), Some(target));
    assert_eq!(floor.triangle_material(1), Some(original));
    assert!(workspace.is_dirty());
}

#[test]
fn paint_floor_in_triangle_mode_targets_clicked_triangle_only() {
    let mut project = ProjectDocument::new("triangle-floor-paint");
    let original = project.add_resource(
        "Original",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let target = project.add_resource(
        "Target",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.set_floor(0, 0, 0, Some(original));
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.horizontal_edit_mode = HorizontalEditMode::Triangle;
    workspace.brush_material = Some(target);

    workspace.run_paint_action(ViewTool::PaintFloor, room, 0, 0, None, [800.0, 0.0, 300.0]);

    let grid = workspace.room_grid_view(room).unwrap();
    let floor = grid.sector(0, 0).unwrap().floor.as_ref().unwrap();
    assert_eq!(floor.material, Some(original));
    assert_eq!(floor.triangle_material(0), Some(target));
    assert_eq!(floor.triangle_material(1), Some(original));
    assert!(matches!(
        workspace.selection.selected_primitive,
        Some(Selection::Triangle(HorizontalTriangleRef {
            surface: HorizontalSurfaceKind::Floor,
            index: HorizontalTriangleIndex::A,
            ..
        }))
    ));
    assert_eq!(workspace.selection.selected_resource, Some(target));
    assert!(workspace.is_dirty());
}

#[test]
fn paint_ceiling_in_triangle_mode_targets_clicked_triangle_only() {
    let mut project = ProjectDocument::new("triangle-ceiling-paint");
    let original = project.add_resource(
        "Original",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let target = project.add_resource(
        "Target",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.ensure_sector(0, 0).unwrap().ceiling =
        Some(GridHorizontalFace::flat(1024, Some(original)));
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.horizontal_edit_mode = HorizontalEditMode::Triangle;
    workspace.brush_material = Some(target);

    workspace.run_paint_action(
        ViewTool::PaintCeiling,
        room,
        0,
        0,
        None,
        [200.0, 1024.0, 300.0],
    );

    let grid = workspace.room_grid_view(room).unwrap();
    let ceiling = grid.sector(0, 0).unwrap().ceiling.as_ref().unwrap();
    assert_eq!(ceiling.material, Some(original));
    assert_eq!(ceiling.triangle_material(0), Some(original));
    assert_eq!(ceiling.triangle_material(1), Some(target));
    assert!(matches!(
        workspace.selection.selected_primitive,
        Some(Selection::Triangle(HorizontalTriangleRef {
            surface: HorizontalSurfaceKind::Ceiling,
            index: HorizontalTriangleIndex::B,
            ..
        }))
    ));
    assert_eq!(workspace.selection.selected_resource, Some(target));
    assert!(workspace.is_dirty());
}

#[test]
fn wall_paint_shape_stamps_diagonal_wall() {
    let mut project = ProjectDocument::new("diagonal-wall-paint");
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Room {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);

    workspace.wall_paint_shape = WallPaintShape::NorthEastSouthWest;
    workspace.run_paint_action(ViewTool::PaintWall, room, 0, 0, None, [512.0, 0.0, 512.0]);

    let grid = workspace.room_grid_view(room).unwrap();
    let sector = grid.sector(0, 0).unwrap();
    assert!(sector.walls.get(GridDirection::North).is_empty());
    assert_eq!(sector.walls.get(GridDirection::NorthEastSouthWest).len(), 1);
}

#[test]
fn paint_wall_on_existing_wall_adds_next_stack_entry() {
    let mut project = ProjectDocument::new("stacked-wall-paint");
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.add_wall(0, 0, GridDirection::North, 0, 1024, None);
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    let picked = FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Wall {
            dir: GridDirection::North,
            stack: 0,
        },
    };

    workspace.run_paint_action(
        ViewTool::PaintWall,
        room,
        0,
        0,
        Some(picked),
        [512.0, 512.0, 1024.0],
    );

    let grid = workspace.room_grid_view(room).unwrap();
    let walls = grid.sector(0, 0).unwrap().walls.get(GridDirection::North);
    assert_eq!(walls.len(), 2);
    assert_eq!(walls[1].heights, [1024, 1024, 3072, 3072]);
}

#[test]
fn paint_wall_stamp_ignores_stack_to_prevent_drag_restacking() {
    let mut project = ProjectDocument::new("wall-stamp-stack-dedupe");
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.add_wall(0, 0, GridDirection::North, 0, 1024, None);
    grid.add_wall(0, 0, GridDirection::North, 1024, 2048, None);
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.active_tool = ViewTool::PaintWall;
    let face = |stack| FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Wall {
            dir: GridDirection::North,
            stack,
        },
    };

    let first = workspace.paint_stamp_for(
        room,
        0,
        0,
        Some((face(0), [512.0, 512.0, 1024.0])),
        [512.0, 512.0, 1024.0],
    );
    let second = workspace.paint_stamp_for(
        room,
        0,
        0,
        Some((face(1), [512.0, 1536.0, 1024.0])),
        [512.0, 1536.0, 1024.0],
    );

    assert_eq!(first, second);
    assert_eq!(first.stack, None);
}

#[test]
fn horizontal_edit_mode_picks_triangle_face_halves() {
    let mut project = ProjectDocument::new("triangle-face-pick");
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    let face = FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Floor,
    };

    workspace.horizontal_edit_mode = HorizontalEditMode::Triangle;
    workspace.selection_mode = SelectionMode::Face;
    let picked = workspace.pick_primitive_from_hit(face, [900.0, 0.0, 900.0]);
    assert_eq!(
        picked,
        Selection::Triangle(HorizontalTriangleRef {
            room,
            sx: 0,
            sz: 0,
            surface: HorizontalSurfaceKind::Floor,
            index: HorizontalTriangleIndex::A,
            corners: [Corner::NW, Corner::NE, Corner::SE],
        })
    );

    workspace.horizontal_edit_mode = HorizontalEditMode::Quad;
    assert_eq!(
        workspace.pick_primitive_from_hit(face, [900.0, 0.0, 900.0]),
        Selection::Face(face)
    );
}

#[test]
fn triangle_edit_edge_mode_can_pick_split_diagonal() {
    let mut project = ProjectDocument::new("triangle-edge-pick");
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    let face = FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Floor,
    };

    workspace.horizontal_edit_mode = HorizontalEditMode::Triangle;
    workspace.selection_mode = SelectionMode::Edge;
    let picked = workspace.pick_primitive_from_hit(face, [512.0, 0.0, 512.0]);

    assert_eq!(
        picked,
        Selection::Edge(EdgeRef {
            room,
            anchor: EdgeAnchor::Floor {
                sx: 0,
                sz: 0,
                dir: GridDirection::NorthWestSouthEast,
            },
        })
    );
}

#[test]
fn dragging_triangle_face_moves_only_its_three_corners() {
    let mut project = ProjectDocument::new("triangle-drag");
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    let triangle = HorizontalTriangleRef {
        room,
        sx: 0,
        sz: 0,
        surface: HorizontalSurfaceKind::Floor,
        index: HorizontalTriangleIndex::A,
        corners: [Corner::NW, Corner::NE, Corner::SE],
    };

    workspace.selection.hovered_primitive = Some(Selection::Triangle(triangle));
    workspace.begin_primitive_drag(egui::Modifiers::NONE);
    workspace.update_primitive_drag(-8.0);
    workspace.end_primitive_drag();

    let parent_after = floor_heights(&workspace, room, 0, 0);
    assert_eq!(parent_after, [0; 4]);
    let triangle_after = floor_triangle_heights(&workspace, room, 0, 0, HorizontalTriangleIndex::A);
    assert_eq!(triangle_after[0], HEIGHT_QUANTUM);
    assert_eq!(triangle_after[1], HEIGHT_QUANTUM);
    assert_eq!(triangle_after[2], HEIGHT_QUANTUM);
}

#[test]
fn selected_room_bounds_follow_authored_tiles() {
    let mut project = ProjectDocument::new("bounds");
    let mut grid = WorldGrid::empty(6, 6, 1024);
    grid.set_floor(1, 2, 0, None);
    grid.set_floor(3, 4, 0, None);
    let room = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Room", NodeKind::Room { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);

    workspace.replace_node_selection(room);

    let (center, half) = workspace
        .selected_bounds_3d()
        .expect("selected room has bounds");
    assert_eq!(center, [2560.0, 512.0, 3584.0]);
    assert_eq!(half, [1536.0, 512.0, 1536.0]);
}

#[test]
fn ctrl_selected_vertices_drag_together() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    let room = workspace.active_room_id().expect("starter has room");
    let (sx, sz) = first_floor_sector(&workspace, room);
    let nw = Selection::Vertex(VertexRef {
        room,
        anchor: VertexAnchor::Floor {
            sx,
            sz,
            corner: Corner::NW,
        },
    });
    let ne = Selection::Vertex(VertexRef {
        room,
        anchor: VertexAnchor::Floor {
            sx,
            sz,
            corner: Corner::NE,
        },
    });
    let before = floor_heights(&workspace, room, sx, sz);

    let mut ctrl = egui::Modifiers::NONE;
    ctrl.ctrl = true;
    workspace.apply_primitive_selection_modifiers(nw, egui::Modifiers::NONE);
    workspace.apply_primitive_selection_modifiers(ne, ctrl);

    assert_eq!(workspace.selection.selected_primitives.len(), 2);

    workspace.selection.hovered_primitive = Some(nw);
    workspace.begin_primitive_drag(egui::Modifiers::NONE);
    workspace.update_primitive_drag(-8.0);
    workspace.end_primitive_drag();

    let after = floor_heights(&workspace, room, sx, sz);
    assert_eq!(
        after[Corner::NW.idx()],
        snap_height(before[Corner::NW.idx()] + HEIGHT_QUANTUM)
    );
    assert_eq!(
        after[Corner::NE.idx()],
        snap_height(before[Corner::NE.idx()] + HEIGHT_QUANTUM)
    );
    assert_eq!(after[Corner::SE.idx()], before[Corner::SE.idx()]);
    assert_eq!(after[Corner::SW.idx()], before[Corner::SW.idx()]);
}

#[test]
fn welded_vertex_drag_moves_coincident_grid_corners() {
    let (mut workspace, room) = workspace_with_populated_grid("welded-vertex-drag", 2, 2);
    let target = Selection::Vertex(VertexRef {
        room,
        anchor: VertexAnchor::Floor {
            sx: 0,
            sz: 0,
            corner: Corner::NE,
        },
    });

    workspace.selection.hovered_primitive = Some(target);
    workspace.begin_primitive_drag(egui::Modifiers::NONE);
    workspace.update_primitive_drag(-8.0);
    workspace.end_primitive_drag();

    assert_eq!(
        floor_heights(&workspace, room, 0, 0)[Corner::NE.idx()],
        HEIGHT_QUANTUM
    );
    assert_eq!(
        floor_heights(&workspace, room, 1, 0)[Corner::NW.idx()],
        HEIGHT_QUANTUM
    );
    assert_eq!(
        floor_heights(&workspace, room, 0, 1)[Corner::SE.idx()],
        HEIGHT_QUANTUM
    );
    assert_eq!(
        floor_heights(&workspace, room, 1, 1)[Corner::SW.idx()],
        HEIGHT_QUANTUM
    );
}

#[test]
fn detached_vertex_drag_moves_only_seed_corner() {
    let (mut workspace, room) = workspace_with_populated_grid("detached-vertex-drag", 2, 2);
    workspace.vertex_connectivity = VertexConnectivity::Detached;
    let target = Selection::Vertex(VertexRef {
        room,
        anchor: VertexAnchor::Floor {
            sx: 0,
            sz: 0,
            corner: Corner::NE,
        },
    });

    workspace.selection.hovered_primitive = Some(target);
    workspace.begin_primitive_drag(egui::Modifiers::NONE);
    workspace.update_primitive_drag(-8.0);
    workspace.end_primitive_drag();

    assert_eq!(
        floor_heights(&workspace, room, 0, 0)[Corner::NE.idx()],
        HEIGHT_QUANTUM
    );
    assert_eq!(floor_heights(&workspace, room, 1, 0)[Corner::NW.idx()], 0);
    assert_eq!(floor_heights(&workspace, room, 0, 1)[Corner::SE.idx()], 0);
    assert_eq!(floor_heights(&workspace, room, 1, 1)[Corner::SW.idx()], 0);
}

#[test]
fn ctrl_selected_edges_drag_together() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    let room = workspace.active_room_id().expect("starter has room");
    let (sx, sz) = first_floor_sector(&workspace, room);
    let north = Selection::Edge(EdgeRef {
        room,
        anchor: EdgeAnchor::Floor {
            sx,
            sz,
            dir: GridDirection::North,
        },
    });
    let east = Selection::Edge(EdgeRef {
        room,
        anchor: EdgeAnchor::Floor {
            sx,
            sz,
            dir: GridDirection::East,
        },
    });
    let before = floor_heights(&workspace, room, sx, sz);

    let mut ctrl = egui::Modifiers::NONE;
    ctrl.ctrl = true;
    workspace.apply_primitive_selection_modifiers(north, egui::Modifiers::NONE);
    workspace.apply_primitive_selection_modifiers(east, ctrl);

    assert_eq!(workspace.selection.selected_primitives.len(), 2);

    workspace.selection.hovered_primitive = Some(north);
    workspace.begin_primitive_drag(egui::Modifiers::NONE);
    workspace.update_primitive_drag(-8.0);
    workspace.end_primitive_drag();

    let after = floor_heights(&workspace, room, sx, sz);
    assert_eq!(
        after[Corner::NW.idx()],
        snap_height(before[Corner::NW.idx()] + HEIGHT_QUANTUM)
    );
    assert_eq!(
        after[Corner::NE.idx()],
        snap_height(before[Corner::NE.idx()] + HEIGHT_QUANTUM)
    );
    assert_eq!(
        after[Corner::SE.idx()],
        snap_height(before[Corner::SE.idx()] + HEIGHT_QUANTUM)
    );
    assert_eq!(after[Corner::SW.idx()], before[Corner::SW.idx()]);
}

fn floor_heights(workspace: &EditorWorkspace, room: NodeId, sx: u16, sz: u16) -> [i32; 4] {
    let scene = workspace.project.active_scene();
    let node = scene.node(room).expect("room node exists");
    let NodeKind::Room { grid } = &node.kind else {
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
    let NodeKind::Room { grid } = &node.kind else {
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
    let NodeKind::Room { grid } = &node.kind else {
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
        NodeKind::Room {
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
        clips: Vec::new(),
        default_clip: None,
        preview_clip: None,
        world_height: 1024,
        collision_radius: default_model_collision_radius_for_height(1024),
        scale_q8: [MODEL_SCALE_ONE_Q8; 3],
        attachments: Vec::new(),
    }
}

#[test]
fn physical_vertex_isolated_corner_returns_self_only() {
    let grid = populated_grid(1, 1);
    let seed = FaceCornerRef::Floor {
        sx: 0,
        sz: 0,
        corner: Corner::NW,
    };
    let pv = physical_vertex(&grid, seed).unwrap();
    assert_eq!(pv.members, vec![seed]);
}

#[test]
fn physical_vertex_interior_grid_corner_returns_four_floors() {
    let grid = populated_grid(2, 2);
    // Cell (0, 0) NE shares its world position with three
    // other cells' corresponding corners.
    let seed = FaceCornerRef::Floor {
        sx: 0,
        sz: 0,
        corner: Corner::NE,
    };
    let pv = physical_vertex(&grid, seed).unwrap();
    assert_eq!(pv.members.len(), 4, "{:?}", pv.members);
    // Spot-check that the expected siblings are in the set.
    assert!(pv.members.contains(&FaceCornerRef::Floor {
        sx: 1,
        sz: 0,
        corner: Corner::NW,
    }));
    assert!(pv.members.contains(&FaceCornerRef::Floor {
        sx: 0,
        sz: 1,
        corner: Corner::SE,
    }));
    assert!(pv.members.contains(&FaceCornerRef::Floor {
        sx: 1,
        sz: 1,
        corner: Corner::SW,
    }));
}

#[test]
fn physical_vertex_skips_unpopulated_cells() {
    // 2×2 grid with only three cells populated. The corner
    // they all share should yield exactly 3 members.
    let mut grid = WorldGrid::empty(2, 2, 1024);
    for (sx, sz) in [(0u16, 0u16), (1, 0), (0, 1)] {
        if let Some(s) = grid.ensure_sector(sx, sz) {
            *s = cell_with_floor(None);
        }
    }
    let seed = FaceCornerRef::Floor {
        sx: 0,
        sz: 0,
        corner: Corner::NE,
    };
    let pv = physical_vertex(&grid, seed).unwrap();
    assert_eq!(pv.members.len(), 3);
}

#[test]
fn apply_vertex_height_writes_every_member() {
    let mut grid = populated_grid(2, 2);
    let seed = FaceCornerRef::Floor {
        sx: 0,
        sz: 0,
        corner: Corner::NE,
    };
    let pv = physical_vertex(&grid, seed).unwrap();
    apply_vertex_height(&mut grid, &pv, 64);
    for member in &pv.members {
        let world = face_corner_world(&grid, *member).unwrap();
        assert_eq!(world[1], 64, "{:?}", member);
    }
}

#[test]
fn apply_vertex_height_break_action_separates_seed() {
    let mut grid = populated_grid(2, 2);
    let seed = FaceCornerRef::Floor {
        sx: 0,
        sz: 0,
        corner: Corner::NE,
    };
    // Capture the pre-break member set so we can confirm
    // exactly one corner left (the seed) when the break
    // mutates only the seed's height.
    let before = physical_vertex(&grid, seed).unwrap();
    assert_eq!(before.members.len(), 4);
    // Move only the seed by writing directly via the helper.
    write_face_corner_height(&mut grid, seed, 32);
    // Re-resolve from a former neighbour. Should now contain
    // 3 members (the seed has departed).
    let neighbour = FaceCornerRef::Floor {
        sx: 1,
        sz: 0,
        corner: Corner::NW,
    };
    let after = physical_vertex(&grid, neighbour).unwrap();
    assert_eq!(after.members.len(), 3);
    assert!(!after.members.contains(&seed));
}

#[test]
fn closest_corner_idx_picks_nearest_corner() {
    let corners = [
        [0.0_f32, 0.0, 0.0],
        [10.0, 0.0, 0.0],
        [10.0, 0.0, 10.0],
        [0.0, 0.0, 10.0],
    ];
    // Each quadrant of the unit square should resolve to
    // the nearest corner.
    assert_eq!(closest_corner_idx(&corners, [1.0, 0.0, 1.0]), 0);
    assert_eq!(closest_corner_idx(&corners, [9.0, 0.0, 1.0]), 1);
    assert_eq!(closest_corner_idx(&corners, [9.0, 0.0, 9.0]), 2);
    assert_eq!(closest_corner_idx(&corners, [1.0, 0.0, 9.0]), 3);
}

#[test]
fn closest_edge_idx_picks_nearest_edge() {
    let corners = [
        [0.0_f32, 0.0, 0.0],
        [10.0, 0.0, 0.0],
        [10.0, 0.0, 10.0],
        [0.0, 0.0, 10.0],
    ];
    // (5, 0, 0.5) → near edge 0 (corners 0–1).
    assert_eq!(closest_edge_idx(&corners, [5.0, 0.0, 0.5]), 0);
    // (9.5, 0, 5) → near edge 1 (corners 1–2).
    assert_eq!(closest_edge_idx(&corners, [9.5, 0.0, 5.0]), 1);
    // (5, 0, 9.5) → near edge 2 (corners 2–3).
    assert_eq!(closest_edge_idx(&corners, [5.0, 0.0, 9.5]), 2);
    // (0.5, 0, 5) → near edge 3 (corners 3–0).
    assert_eq!(closest_edge_idx(&corners, [0.5, 0.0, 5.0]), 3);
}

#[test]
fn action_bar_height_expands_for_wrapped_status() {
    assert_eq!(
        action_bar_height_for_status("Ready"),
        ACTION_BAR_COMPACT_HEIGHT
    );
    assert_eq!(
            action_bar_height_for_status(
                "Embedded Play failed while cooking assets: playtest validation failed: No player source. Place one Player Spawn, or select a Character Controller and enable Player controlled."
            ),
            ACTION_BAR_EXPANDED_HEIGHT
        );
    assert_eq!(
        action_bar_height_for_status("First line\nSecond line"),
        ACTION_BAR_EXPANDED_HEIGHT
    );
}

/// End-to-end of the multi-scene UX on the editor side: create a
/// scene, switch to it, confirm edits land only in the selected
/// scene, then delete it and confirm the active index clamps and the
/// scene list never empties.
#[test]
fn ui_scene_create_switch_edit_isolated_delete_clamps() {
    let mut project = ProjectDocument::new("ui-scene-crud");
    project.normalize_loaded();
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("ui-scene-crud"), project);

    // One default scene to start; index points at it.
    assert_eq!(workspace.project.ui_scenes.len(), 1);
    assert_eq!(workspace.current_ui_scene_index(), 0);
    let first_id = workspace.current_ui_scene().unwrap().id;
    let first_node_count = workspace.current_ui_scene().unwrap().nodes().len();

    // Create -> the new scene becomes active and selection resets to
    // its root canvas (no stale node id from scene 0).
    workspace.add_ui_scene_action();
    assert_eq!(workspace.project.ui_scenes.len(), 2);
    assert_eq!(workspace.current_ui_scene_index(), 1);
    let second_id = workspace.current_ui_scene().unwrap().id;
    assert_ne!(first_id, second_id, "new scene gets a fresh stable id");
    let second_root = workspace.current_ui_scene().unwrap().root;
    assert_eq!(workspace.selection.selected_ui_node, second_root);

    // Edit isolation: add a node into the active (second) scene.
    workspace.add_ui_child(
        UiNodeKind::Rect {
            rect: UiRect::new(8, 8, 32, 16),
            color: [10, 20, 30],
            gradient: None,
        },
        "Probe",
    );
    let added = workspace.selection.selected_ui_node;
    assert!(workspace.current_ui_scene().unwrap().node(added).is_some());
    let second_node_count = workspace.current_ui_scene().unwrap().nodes().len();

    // Switch back to scene 0: its structure is untouched, and the
    // selection snaps to scene 0's root rather than carrying the
    // second scene's node over. Node ids are per-scene, so isolation
    // is asserted structurally (count + the absence of "Probe")
    // rather than by id, which can legitimately repeat across scenes.
    workspace.switch_ui_scene(0);
    assert_eq!(workspace.current_ui_scene_index(), 0);
    let first_scene = workspace.current_ui_scene().unwrap();
    assert_eq!(first_scene.id, first_id);
    assert_eq!(
        first_scene.nodes().len(),
        first_node_count,
        "edit must not change the other scene's node count"
    );
    assert!(
        first_scene.nodes().iter().all(|node| node.name != "Probe"),
        "edit must not leak into the other scene"
    );
    assert_eq!(
        workspace.selection.selected_ui_node, first_scene.root,
        "selection resets on scene switch"
    );

    // The second scene still holds its extra node.
    assert_eq!(
        workspace.project.ui_scene(second_id).unwrap().nodes().len(),
        second_node_count
    );

    // Point the active index at the last scene, then delete it:
    // the index must clamp back into range and the list stays
    // non-empty.
    workspace.switch_ui_scene(1);
    assert_eq!(workspace.current_ui_scene_index(), 1);
    workspace.delete_ui_scene_action(1);
    assert_eq!(workspace.project.ui_scenes.len(), 1);
    assert_eq!(
        workspace.current_ui_scene_index(),
        0,
        "active index clamps after deleting the last scene"
    );
    assert_eq!(workspace.current_ui_scene().unwrap().id, first_id);

    // Deleting the final remaining scene is forbidden (never empty).
    workspace.delete_ui_scene_action(0);
    assert_eq!(
        workspace.project.ui_scenes.len(),
        1,
        "the last UI scene cannot be deleted"
    );
}

#[test]
fn button_and_slider_are_addable_and_options_crud_round_trips() {
    // Both new interactive kinds appear in the add-node menu.
    let addable = default_addable_ui_kinds();
    assert!(
        addable
            .iter()
            .any(|(label, kind)| *label == "Button" && matches!(kind, UiNodeKind::Button { .. })),
        "Button must be addable"
    );
    assert!(
        addable
            .iter()
            .any(|(label, kind)| *label == "Slider" && matches!(kind, UiNodeKind::Slider { .. })),
        "Slider must be addable"
    );

    let mut project = ProjectDocument::new("ui-button-slider");
    project.normalize_loaded();
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("ui-button-slider"), project);

    // Options CRUD: add two, remove the first, ids stay distinct and
    // a slider can bind to a surviving option.
    let first = workspace.project.add_option("Volume");
    let second = workspace.project.add_option("Brightness");
    assert_ne!(first, second);
    assert_eq!(workspace.project.options.len(), 2);
    assert!(workspace.project.remove_option(0));
    assert_eq!(workspace.project.options.len(), 1);
    assert_eq!(workspace.project.options[0].id, second);
    // A newly added option after a removal must not collide with a
    // surviving id (so a slider bound to `second` is never shadowed).
    let third = workspace.project.add_option("Contrast");
    assert_ne!(third, second);

    // Add a Slider bound to the surviving option and confirm the
    // authored binding round-trips through the scene tree.
    workspace.add_ui_child(
        UiNodeKind::Slider {
            rect: UiRect::new(8, 8, 96, 8),
            option: second,
            track: [11, 12, 13],
            track_gradient: None,
            fill: [21, 22, 23],
            fill_gradient: None,
            knob: [31, 32, 33],
            knob_gradient: None,
            sfx: UiSfxBindings::default(),
        },
        "Brightness",
    );
    let added = workspace.selection.selected_ui_node;
    let node = workspace
        .current_ui_scene()
        .unwrap()
        .node(added)
        .expect("slider node added");
    match &node.kind {
        UiNodeKind::Slider { option, knob, .. } => {
            assert_eq!(*option, second);
            assert_eq!(*knob, [31, 32, 33]);
        }
        other => panic!("expected slider, got {other:?}"),
    }
    // The bound option still resolves to a name in the project.
    assert_eq!(
        workspace.project.option(second).map(|o| o.name.as_str()),
        Some("Brightness")
    );
}
