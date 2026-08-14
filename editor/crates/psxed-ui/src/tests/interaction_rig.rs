use super::*;
use crate::workspace::tools::BrushHandle3d;

/// End-to-end mouse rig: every interaction goes through real egui raw
/// input and the real `draw_viewport_3d_body`, so egui's click-vs-drag
/// threshold, hover state, and pointer-target resolution all behave
/// exactly as they do live. Nothing calls tool methods directly.
pub(crate) struct MouseRig {
    pub(crate) workspace: EditorWorkspace,
    ctx: egui::Context,
    viewport: EditorViewport3dPresentation,
    time: f64,
    _texture: egui::TextureHandle,
}

/// The viewport rect `allocate_centered_preview_rect` produces for the
/// 800x600 test screen (matches the existing real-egui tests).
pub(crate) const RIG_VIEWPORT: Rect = Rect {
    min: Pos2::new(400.0 - 778.6667 / 2.0, 300.0 - 584.0 / 2.0),
    max: Pos2::new(400.0 + 778.6667 / 2.0, 300.0 + 584.0 / 2.0),
};

impl MouseRig {
    /// Single-cube BSP scene with a free camera aimed at the cube.
    pub(crate) fn single_cube(label: &str) -> Self {
        let brush = psxed_project::brush::Brush::cuboid([0, 0, 0], [512, 256, 256]);
        let mut project = ProjectDocument::new(label);
        project.active_scene_mut().brushes.push(brush);
        let mut workspace = EditorWorkspace::with_project(test_temp_dir(label), project);
        workspace.active_workspace = WorkspaceView::Room;
        workspace.active_tool = ViewTool::Select;
        workspace.view_2d = false;
        workspace.camera_rig.mode = ViewportCameraMode::Free;
        workspace.camera_rig.free_initialized = true;
        workspace.camera_rig.free_position = [1400, 1200, -1400];
        let (yaw, pitch) =
            camera_angles_to_look_at([1400, 1200, -1400], [256, 128, 128]).unwrap();
        workspace.camera_rig.free_yaw = yaw;
        workspace.camera_rig.free_pitch = pitch;

        let ctx = egui::Context::default();
        let texture = ctx.load_texture(
            "mouse-rig-viewport",
            egui::ColorImage::new([1, 1], egui::Color32::BLACK),
            egui::TextureOptions::NEAREST,
        );
        let viewport = EditorViewport3dPresentation::edit(texture.id(), Vec::new());
        let mut rig = Self {
            workspace,
            ctx,
            viewport,
            time: 0.0,
            _texture: texture,
        };
        // Warm-up frame so egui has a layout before the first event.
        rig.pump(vec![]);
        rig
    }

    fn pump(&mut self, events: Vec<egui::Event>) {
        self.pump_with(events, egui::Modifiers::NONE);
    }

    /// Public pump for tests that build the rig by hand.
    pub(crate) fn pump_events(&mut self, events: Vec<egui::Event>) {
        self.pump(events);
    }

    pub(crate) fn press(&mut self, pos: Pos2) {
        self.button(pos, true);
    }

    pub(crate) fn release(&mut self, pos: Pos2) {
        self.button(pos, false);
    }

    fn pump_with(&mut self, events: Vec<egui::Event>, modifiers: egui::Modifiers) {
        self.time += 1.0 / 60.0;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(800.0, 600.0),
            )),
            time: Some(self.time),
            modifiers,
            events,
            ..egui::RawInput::default()
        };
        let workspace = &mut self.workspace;
        let viewport = self.viewport.clone();
        let _ = self.ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                workspace.draw_viewport_3d_body(ui, viewport.clone());
            });
        });
    }

    pub(crate) fn world_to_screen(&self, world: [f64; 3]) -> Pos2 {
        self.workspace
            .project_brush_point_3d(RIG_VIEWPORT, world)
            .expect("world point projects into the rig viewport")
    }

    pub(crate) fn move_to(&mut self, pos: Pos2) {
        self.pump(vec![egui::Event::PointerMoved(pos)]);
    }

    fn button(&mut self, pos: Pos2, pressed: bool) {
        self.pump(vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }]);
    }

    /// A human click: press, a 1-2 px wiggle while held (real hands are
    /// never perfectly still; this stays UNDER egui's drag threshold so
    /// egui still reports a click), release.
    pub(crate) fn click(&mut self, pos: Pos2) {
        self.move_to(pos);
        self.button(pos, true);
        self.move_to(pos + Vec2::new(1.0, 1.0));
        self.button(pos + Vec2::new(1.0, 1.0), false);
    }

    /// A sloppier click that CROSSES the drag threshold (7 px of travel
    /// before settling back): egui reports drag-start + drag-stop and no
    /// click event. Live clicks land like this constantly, so selection
    /// must work through this path too.
    pub(crate) fn sloppy_click(&mut self, pos: Pos2) {
        self.move_to(pos);
        self.button(pos, true);
        self.move_to(pos + Vec2::new(7.0, 0.0));
        self.move_to(pos + Vec2::new(2.0, 0.0));
        self.button(pos + Vec2::new(2.0, 0.0), false);
    }

    /// Press at `from`, drag through intermediate steps to `to`, release.
    pub(crate) fn drag(&mut self, from: Pos2, to: Pos2) {
        self.move_to(from);
        self.button(from, true);
        let steps = 6;
        for step in 1..=steps {
            let t = step as f32 / steps as f32;
            self.move_to(from + (to - from) * t);
        }
        self.button(to, false);
    }

    /// Grab point plus a natural drag destination for a gizmo axis:
    /// along the arrow for Move/Scale, tangential around the projected
    /// centroid for rotation rings.
    pub(crate) fn gizmo_drag_vector(&self, axis: usize) -> (Pos2, Pos2) {
        let polylines = self
            .workspace
            .brush_element_gizmo_polylines_3d(RIG_VIEWPORT)
            .expect("element gizmo visible");
        let polyline = &polylines[axis];
        if polyline.len() == 2 {
            let grab = polyline[0] + (polyline[1] - polyline[0]) * 0.6;
            let direction = (polyline[1] - polyline[0]).normalized();
            (grab, grab + direction * 80.0)
        } else {
            let grab = polyline[polyline.len() / 6];
            let centroid = self
                .workspace
                .brush_gizmo_context()
                .expect("gizmo context")
                .0;
            let center = self.world_to_screen(centroid);
            let radial = (grab - center).normalized();
            let tangent = Vec2::new(-radial.y, radial.x);
            (grab, grab + tangent * 80.0)
        }
    }

    pub(crate) fn gizmo_axis_grab_point(&self, axis: usize) -> Pos2 {
        let polylines = self
            .workspace
            .brush_element_gizmo_polylines_3d(RIG_VIEWPORT)
            .expect("element gizmo visible");
        let polyline = &polylines[axis];
        if polyline.len() == 2 {
            polyline[0] + (polyline[1] - polyline[0]) * 0.6
        } else {
            // Ring: grab a point partway around, away from the seam.
            polyline[polyline.len() / 6]
        }
    }

    /// Screen position of a brush handle for the current edit mode.
    pub(crate) fn handle_screen(&self, handle_world: [f64; 3]) -> Pos2 {
        self.world_to_screen(handle_world)
    }

    pub(crate) fn brush(&self) -> psxed_project::brush::Brush {
        self.workspace.project.active_scene().brushes[0].clone()
    }

    /// Press a key while the pointer hovers `over` (viewport keyboard
    /// handlers require hover).
    pub(crate) fn key(&mut self, over: Pos2, key: egui::Key) {
        self.move_to(over);
        self.pump(vec![egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        self.pump(vec![egui::Event::Key {
            key,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]);
    }

    /// Shift-click (additive selection). Live modifier STATE matters:
    /// handlers read `input.modifiers`, which egui takes from RawInput,
    /// not from the event payload.
    pub(crate) fn shift_click(&mut self, pos: Pos2) {
        let shift = egui::Modifiers::SHIFT;
        self.pump_with(vec![egui::Event::PointerMoved(pos)], shift);
        self.pump_with(
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: shift,
            }],
            shift,
        );
        self.pump_with(
            vec![egui::Event::PointerButton {
                pos: pos + Vec2::new(1.0, 0.0),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: shift,
            }],
            shift,
        );
    }

    pub(crate) fn has_vertex(&self, expect: [f64; 3]) -> bool {
        crate::workspace::brush_elements::unique_vertices(&self.brush().solve())
            .iter()
            .any(|vertex| (0..3).all(|axis| (vertex[axis] - expect[axis]).abs() <= 1.0))
    }
}

/// The full interaction matrix on one cube: select each element kind
/// with the mouse (clean AND sloppy clicks), then run every Transform
/// mode through the gizmo with real drags. Assertions are behavioural:
/// the element selects, the gesture changes the brush (correct axis
/// where cheap to pin), the result stays valid and bounded, undo works.
#[test]
fn mouse_matrix_selects_and_transforms_faces_edges_and_vertices() {
    // (mode, element world anchor, expected element check)
    let cases: [(BrushEditMode, [f64; 3]); 3] = [
        // Top face centre; Face mode selects the face under the cursor.
        (BrushEditMode::Face, [256.0, 256.0, 128.0]),
        // Top-front edge midpoint.
        (BrushEditMode::Edge, [256.0, 256.0, 0.0]),
        // Top-front-left corner.
        (BrushEditMode::Vertex, [0.0, 256.0, 0.0]),
    ];
    // Geometric no-ops the gestures must leave alone: scaling a flat
    // face along its own normal, scaling an edge across its direction,
    // and rotating an edge about an axis parallel to itself.
    let expect_change = |mode: BrushEditMode, gizmo: TransformGizmoMode, axis: usize| match (
        mode, gizmo, axis,
    ) {
        (BrushEditMode::Face, TransformGizmoMode::Scale, 1) => false,
        (BrushEditMode::Edge, TransformGizmoMode::Scale, 1 | 2) => false,
        (BrushEditMode::Edge, TransformGizmoMode::Rotate, 0) => false,
        _ => true,
    };
    for (mode, anchor) in cases {
        for sloppy in [false, true] {
            for gizmo_mode in [
                TransformGizmoMode::Move,
                TransformGizmoMode::Rotate,
                TransformGizmoMode::Scale,
            ] {
                for axis in 0..3 {
                    let label = format!("{mode:?}/{gizmo_mode:?}/axis{axis}/sloppy={sloppy}");
                    let mut rig = MouseRig::single_cube("mouse-matrix");
                    let base = rig.brush();
                    rig.workspace.set_brush_edit_mode(mode);
                    rig.workspace.set_transform_gizmo_mode(gizmo_mode);

                    // 1. Click the cube body to select the brush.
                    let body = rig.world_to_screen([256.0, 256.0, 128.0]);
                    if sloppy {
                        rig.sloppy_click(body);
                    } else {
                        rig.click(body);
                    }
                    assert_eq!(
                        rig.workspace.selected_brush,
                        Some(0),
                        "{label}: body click selects the brush"
                    );

                    // 2. Click the element handle.
                    let handle = rig.handle_screen(anchor);
                    if sloppy {
                        rig.sloppy_click(handle);
                    } else {
                        rig.click(handle);
                    }
                    assert!(
                        !rig.workspace.selected_brush_elements.is_empty(),
                        "{label}: element click selects the element"
                    );

                    // Grab THIS axis and drag along its natural screen
                    // direction (tangential for rings).
                    let (grab, to) = rig.gizmo_drag_vector(axis);
                    rig.drag(grab, to);

                    let after = rig.brush();
                    if expect_change(mode, gizmo_mode, axis) {
                        assert_ne!(after, base, "{label}: gizmo drag changes the brush");
                        let (solved_base, solved) = (base.solve(), after.solve());
                        assert!(
                            solved.is_valid()
                                && solved.within_extent(
                                    psxed_project::brush::BRUSH_EDIT_EXTENT_LIMIT
                                ),
                            "{label}: result stays valid and bounded"
                        );
                        // Face moves translate or shear, never tilt:
                        // the selected plane's normal is invariant.
                        if mode == BrushEditMode::Face
                            && gizmo_mode == TransformGizmoMode::Move
                        {
                            let normal = |brush: &psxed_project::brush::Brush| {
                                psxed_project::brush::Plane::from_points(brush.faces[5].points)
                                    .unwrap()
                                    .normal
                            };
                            assert_eq!(
                                normal(&after),
                                normal(&base),
                                "{label}: face plane must not tilt"
                            );
                        }
                        // Move is axis-constrained: bounds on the two
                        // masked axes must not change.
                        if gizmo_mode == TransformGizmoMode::Move {
                            for other in 0..3 {
                                if other == axis {
                                    continue;
                                }
                                assert!(
                                    (solved_base.min[other] - solved.min[other]).abs() <= 0.5
                                        && (solved_base.max[other] - solved.max[other]).abs()
                                            <= 0.5,
                                    "{label}: axis {other} must stay masked"
                                );
                            }
                        }
                        rig.workspace.do_undo();
                        assert_eq!(rig.brush(), base, "{label}: one undo restores");
                    } else {
                        assert_eq!(
                            after, base,
                            "{label}: geometric no-op must leave the brush alone"
                        );
                    }
                }
            }
        }
    }
}

/// Whole-brush Move-mode body drag translates the cube.
#[test]
fn mouse_move_mode_body_drag_translates_the_brush() {
    let mut rig = MouseRig::single_cube("rig-body-drag");
    let base = rig.brush();
    rig.workspace.set_brush_edit_mode(BrushEditMode::Move);
    let body = rig.world_to_screen([256.0, 256.0, 128.0]);
    rig.click(body);
    assert_eq!(rig.workspace.selected_brush, Some(0));
    // A body point clear of the gizmo arrows (which radiate from the
    // brush's bounds centre): free drags belong to the body, arrows to
    // the axis-constrained gizmo.
    let body = rig.world_to_screen([420.0, 256.0, 210.0]);
    rig.drag(body, body + Vec2::new(120.0, 0.0));
    assert_ne!(rig.brush(), base, "body drag moves the brush");
    let solved = rig.brush().solve();
    assert!(solved.is_valid());
    rig.workspace.do_undo();
    assert_eq!(rig.brush(), base);
}

/// Free handle drags (no gizmo): face normal stalk extrudes, edge and
/// vertex handles reshape, all through real mouse input.
#[test]
fn mouse_free_handle_drags_reshape_the_brush() {
    // (mode, handle world anchor)
    let cases: [(BrushEditMode, [f64; 3]); 3] = [
        (BrushEditMode::Face, [256.0, 256.0, 128.0]),
        (BrushEditMode::Edge, [256.0, 256.0, 0.0]),
        (BrushEditMode::Vertex, [0.0, 256.0, 0.0]),
    ];
    for (mode, anchor) in cases {
        let mut rig = MouseRig::single_cube("rig-free-handles");
        let base = rig.brush();
        rig.workspace.set_brush_edit_mode(mode);
        let body = rig.world_to_screen([256.0, 256.0, 128.0]);
        rig.click(body);
        assert_eq!(rig.workspace.selected_brush, Some(0), "{mode:?}");
        let handle = rig.world_to_screen(anchor);
        rig.drag(handle, handle + Vec2::new(48.0, -48.0));
        assert_ne!(rig.brush(), base, "{mode:?}: handle drag reshapes");
        let solved = rig.brush().solve();
        assert!(
            solved.is_valid()
                && solved.within_extent(psxed_project::brush::BRUSH_EDIT_EXTENT_LIMIT),
            "{mode:?}"
        );
        rig.workspace.do_undo();
        assert_eq!(rig.brush(), base, "{mode:?}: undo restores");
    }
}

/// Shift-click builds a vertex multi-selection and a handle drag moves
/// the whole set; shift-clicking a member again removes it.
#[test]
fn mouse_shift_click_multiselect_and_group_drag() {
    let mut rig = MouseRig::single_cube("rig-group");
    rig.workspace.set_brush_edit_mode(BrushEditMode::Vertex);
    let body = rig.world_to_screen([256.0, 256.0, 128.0]);
    rig.click(body);
    let corner_a = rig.world_to_screen([0.0, 256.0, 0.0]);
    rig.click(corner_a);
    assert_eq!(rig.workspace.selected_brush_elements.len(), 1);
    let corner_b = rig.world_to_screen([512.0, 256.0, 0.0]);
    rig.shift_click(corner_b);
    assert_eq!(
        rig.workspace.selected_brush_elements.len(),
        2,
        "shift-click adds the second corner"
    );
    // Drag corner A upward; both corners must ride (the free handle
    // drag moves on a camera-facing plane, so X/Z drift is expected;
    // what matters is that exactly the two SELECTED corners rose).
    let corner_a = rig.world_to_screen([0.0, 256.0, 0.0]);
    rig.drag(corner_a, corner_a + Vec2::new(0.0, -60.0));
    let verts = crate::workspace::brush_elements::unique_vertices(&rig.brush().solve());
    let lifted: Vec<[f64; 3]> = verts
        .iter()
        .copied()
        .filter(|vertex| vertex[1] > 300.0)
        .collect();
    assert_eq!(lifted.len(), 2, "both selected corners lifted, verts {verts:?}");
    // Selection survived; shift-click removes one member.
    assert_eq!(rig.workspace.selected_brush_elements.len(), 2);
    let corner_b_now = *lifted
        .iter()
        .max_by(|a, b| a[0].total_cmp(&b[0]))
        .expect("moved corner B");
    let corner_b_screen = rig.world_to_screen(corner_b_now);
    rig.shift_click(corner_b_screen);
    assert_eq!(
        rig.workspace.selected_brush_elements.len(),
        1,
        "shift-click removes the member again"
    );
}

/// Clicking empty space clears every selection; a marquee drag across
/// the cube in Move mode selects it again.
#[test]
fn mouse_empty_click_clears_and_marquee_selects() {
    let mut rig = MouseRig::single_cube("rig-empty");
    rig.workspace.set_brush_edit_mode(BrushEditMode::Move);
    let body = rig.world_to_screen([256.0, 256.0, 128.0]);
    rig.click(body);
    assert_eq!(rig.workspace.selected_brush, Some(0));
    // Far corner of the viewport is empty sky.
    let empty = RIG_VIEWPORT.min + Vec2::new(30.0, 30.0);
    rig.click(empty);
    assert_eq!(rig.workspace.selected_brush, None, "empty click clears");
    // Sloppy empty click clears too.
    rig.click(body);
    assert_eq!(rig.workspace.selected_brush, Some(0));
    rig.sloppy_click(empty);
    assert_eq!(rig.workspace.selected_brush, None, "sloppy empty click clears");
}

/// The whole clip flow by mouse and keyboard: two points on the top
/// face, Tab cycles the kept side, Enter cuts, Esc clears pending points.
#[test]
fn mouse_clip_flow_cuts_the_cube() {
    let mut rig = MouseRig::single_cube("rig-clip");
    let body = rig.world_to_screen([256.0, 256.0, 128.0]);
    rig.workspace.set_brush_edit_mode(BrushEditMode::Move);
    rig.click(body);
    assert_eq!(rig.workspace.selected_brush, Some(0));
    rig.workspace.set_brush_edit_mode(BrushEditMode::Clip);

    // Esc clears a pending point.
    rig.click(rig.world_to_screen([128.0, 256.0, 32.0]));
    assert_eq!(rig.workspace.brush_clip_points.len(), 1);
    rig.key(body, egui::Key::Escape);
    assert!(rig.workspace.brush_clip_points.is_empty(), "Esc clears points");

    // Two points across the top face, then Enter cuts into two brushes.
    rig.click(rig.world_to_screen([256.0, 256.0, 16.0]));
    rig.click(rig.world_to_screen([256.0, 256.0, 240.0]));
    assert_eq!(rig.workspace.brush_clip_points.len(), 2);
    rig.key(body, egui::Key::X);
    rig.key(body, egui::Key::X);
    rig.key(body, egui::Key::X);
    assert_eq!(
        rig.workspace.brush_clip_keep,
        BrushClipKeep::Both,
        "three X presses cycle back to Both"
    );
    rig.key(body, egui::Key::Enter);
    assert_eq!(
        rig.workspace.project.active_scene().brushes.len(),
        2,
        "Enter applies the cut"
    );
    rig.workspace.do_undo();
    assert_eq!(rig.workspace.project.active_scene().brushes.len(), 1);
}

/// Entities participate: clicking one selects it (clearing the brush
/// selection) and a body drag moves it in world units.
#[test]
fn mouse_entity_click_and_drag() {
    let mut rig = MouseRig::single_cube("rig-entity");
    let entity = rig
        .workspace
        .project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Entity", NodeKind::Entity);
    if let Some(node) = rig.workspace.project.active_scene_mut().node_mut(entity) {
        node.transform.translation = [900.0, 0.0, 128.0];
    }
    rig.workspace.set_brush_edit_mode(BrushEditMode::Move);
    // Select the brush first so exclusivity is exercised.
    let body = rig.world_to_screen([256.0, 256.0, 128.0]);
    rig.click(body);
    assert_eq!(rig.workspace.selected_brush, Some(0));

    let bounds = rig.workspace.collect_entity_bounds(None);
    let bound = bounds
        .iter()
        .find(|bound| bound.node == entity)
        .expect("entity bound exists");
    let center = [
        f64::from(bound.center[0]),
        f64::from(bound.center[1]),
        f64::from(bound.center[2]),
    ];
    let screen = rig.world_to_screen(center);
    rig.click(screen);
    assert_eq!(
        rig.workspace.selection.selected_node,
        entity,
        "entity click selects the node"
    );
    assert_eq!(
        rig.workspace.selected_brush,
        None,
        "entity click clears the brush selection"
    );

    let before = rig.workspace.project.active_scene().node(entity).unwrap().transform.translation;
    rig.drag(screen, screen + Vec2::new(80.0, 0.0));
    let after = rig.workspace.project.active_scene().node(entity).unwrap().transform.translation;
    assert_ne!(before, after, "entity body drag moves it");
    let delta = ((after[0] - before[0]).powi(2) + (after[2] - before[2]).powi(2)).sqrt();
    assert!(
        delta > 32.0 && delta < 4096.0,
        "entity moved a sane world distance, got {delta}"
    );
}

/// Regression: Move on a selected FACE must translate the plane even
/// when its authored points do not sit at the solved corners (clips and
/// rotations move them off). Matching by corner proximity alone missed
/// authored points and TILTED the plane, which read as rotation.
#[test]
fn mouse_face_move_translates_off_corner_authored_planes() {
    let mut rig = MouseRig::single_cube("rig-face-plane-move");
    // Re-author the top plane (+Y, face 5) with points far outside the
    // solved polygon, same plane.
    rig.workspace.project.active_scene_mut().brushes[0].faces[5] =
        psxed_project::brush::BrushFace::from_points([
            [-4096, 256, 8192],
            [8192, 256, 8192],
            [8192, 256, -4096],
        ]);
    assert!(rig.workspace.project.active_scene().brushes[0].solve().is_valid());

    rig.workspace.set_brush_edit_mode(BrushEditMode::Face);
    rig.workspace.set_transform_gizmo_mode(TransformGizmoMode::Move);
    let body = rig.world_to_screen([256.0, 256.0, 128.0]);
    rig.click(body);
    assert!(matches!(
        rig.workspace.selected_brush_elements.as_slice(),
        [BrushElement::Face(5)]
    ), "top face selected, got {:?}", rig.workspace.selected_brush_elements);

    let normal_before = psxed_project::brush::Plane::from_points(
        rig.workspace.project.active_scene().brushes[0].faces[5].points,
    )
    .unwrap()
    .normal;
    let (grab, to) = rig.gizmo_drag_vector(1);
    rig.drag(grab, to);

    let face = rig.workspace.project.active_scene().brushes[0].faces[5];
    let plane = psxed_project::brush::Plane::from_points(face.points).unwrap();
    assert_eq!(
        plane.normal, normal_before,
        "Move must translate the plane, never tilt it"
    );
    // The whole top rose: every solved top corner shares one Y above 256.
    let verts = crate::workspace::brush_elements::unique_vertices(
        &rig.workspace.project.active_scene().brushes[0].solve(),
    );
    let top_ys: Vec<f64> = verts.iter().map(|v| v[1]).filter(|y| *y > 256.5).collect();
    assert_eq!(top_ys.len(), 4, "all four top corners lifted, verts {verts:?}");
    let spread = top_ys.iter().fold((f64::MAX, f64::MIN), |acc, y| {
        (acc.0.min(*y), acc.1.max(*y))
    });
    assert!(
        spread.1 - spread.0 <= 1.0,
        "corners lifted EQUALLY (no tilt), ys {top_ys:?}"
    );
}

/// Diagnostic reproduction of the live report: a Sanctum-scale wedge
/// (slanted roof), the front WALL face selected, camera facing the
/// wall, Grid 64. Every Move axis must actually move the face. On
/// failure the message says which stage died: the press never started
/// a gesture, the input mapped to zero, or the preview was refused.
#[test]
fn mouse_wall_face_move_works_on_every_axis_at_sanctum_scale() {
    for axis in 0..3 {
        let mut brush = psxed_project::brush::Brush::cuboid([0, 0, 0], [2048, 1024, 2048]);
        // Slant the roof like the Ashen Sanctum boundary wedges.
        brush.faces[5] = psxed_project::brush::BrushFace::from_points([
            [0, 512, 2048],
            [2048, 512, 2048],
            [2048, 1024, 0],
        ]);
        assert!(brush.solve().is_valid());
        let mut project = ProjectDocument::new("rig-wall-face");
        project.active_scene_mut().brushes.push(brush);
        let mut workspace =
            EditorWorkspace::with_project(test_temp_dir("rig-wall-face"), project);
        workspace.active_workspace = WorkspaceView::Room;
        workspace.active_tool = ViewTool::Select;
        workspace.view_2d = false;
        workspace.snap_units = 64;
        workspace.camera_rig.mode = ViewportCameraMode::Free;
        workspace.camera_rig.free_initialized = true;
        workspace.camera_rig.free_position = [700, 900, -2400];
        let (yaw, pitch) =
            camera_angles_to_look_at([700, 900, -2400], [1024, 500, 400]).unwrap();
        workspace.camera_rig.free_yaw = yaw;
        workspace.camera_rig.free_pitch = pitch;

        let ctx = egui::Context::default();
        let texture = ctx.load_texture(
            "wall-face-rig",
            egui::ColorImage::new([1, 1], egui::Color32::BLACK),
            egui::TextureOptions::NEAREST,
        );
        let viewport = EditorViewport3dPresentation::edit(texture.id(), Vec::new());
        let mut rig = MouseRig {
            workspace,
            ctx,
            viewport,
            time: 0.0,
            _texture: texture,
        };
        rig.pump_events(vec![]);

        let base = rig.brush();
        rig.workspace.set_brush_edit_mode(BrushEditMode::Face);
        rig.workspace.set_transform_gizmo_mode(TransformGizmoMode::Move);
        // Click the front wall (z = 0 plane, its centre).
        let wall = rig.world_to_screen([1024.0, 500.0, 0.0]);
        rig.click(wall);
        if !matches!(
            rig.workspace.selected_brush_elements.as_slice(),
            [BrushElement::Face(0)]
        ) {
            let target = rig.workspace.resolve_viewport_3d_pointer_target(
                RIG_VIEWPORT,
                wall,
                None,
                true,
            );
            let nearest =
                rig.workspace
                    .pick_brush_face_nearest_for_selection_3d(RIG_VIEWPORT, wall);
            let handle = rig.workspace.pick_brush_handle_3d(RIG_VIEWPORT, wall);
            panic!(
                "axis {axis}: wall face not selected. elements {:?}, brush {:?},                  pointer_target {:?}, nearest_face {:?}, handle {:?}",
                rig.workspace.selected_brush_elements,
                rig.workspace.selected_brush,
                target,
                nearest.map(|(brush, face, _)| (brush, face)),
                handle.map(|(brush, _)| brush),
            );
        }

        let (grab, to) = rig.gizmo_drag_vector(axis);
        // Press + first move: inspect the gesture state mid-drag.
        rig.move_to(grab);
        rig.press(grab);
        rig.move_to(grab + (to - grab) * 0.5);
        let gesture = rig.workspace.brush_vertex_drag.clone();
        rig.move_to(to);
        let mid_applied = rig
            .workspace
            .brush_vertex_drag
            .as_ref()
            .map(|drag| drag.applied);
        rig.release(to);

        let after = rig.brush();
        assert!(
            gesture.is_some(),
            "axis {axis}: the press never started the gizmo drag (routing/pick died)"
        );
        assert!(
            mid_applied.is_some_and(|applied| applied != [0; 3]),
            "axis {axis}: drag ran but applied stayed zero (input mapping or preview refusal), \
             gesture mask {:?}, targets {}, faces {:?}",
            gesture.as_ref().map(|drag| drag.axis_mask),
            gesture.as_ref().map(|drag| drag.targets.len()).unwrap_or(0),
            gesture.as_ref().map(|drag| drag.faces.clone()),
        );
        assert_ne!(after, base, "axis {axis}: face did not move");
    }
}

/// Project a world point to the screen, cast the editor's pick ray back
/// through that pixel, and measure how far the ray passes from the
/// original point. Draw/pick coherence is the foundation every click
/// stands on; any real distance here breaks selection at scale.
#[test]
fn projection_and_pick_ray_agree_at_sanctum_scale() {
    let mut brush = psxed_project::brush::Brush::cuboid([0, 0, 0], [2048, 1024, 2048]);
    brush.faces[5] = psxed_project::brush::BrushFace::from_points([
        [0, 512, 2048],
        [2048, 512, 2048],
        [2048, 1024, 0],
    ]);
    let mut project = ProjectDocument::new("rig-ray-roundtrip");
    project.active_scene_mut().brushes.push(brush);
    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("rig-ray-roundtrip"), project);
    workspace.active_tool = ViewTool::Select;
    workspace.view_2d = false;
    workspace.camera_rig.mode = ViewportCameraMode::Free;
    workspace.camera_rig.free_initialized = true;
    workspace.camera_rig.free_position = [700, 900, -2400];
    let (yaw, pitch) = camera_angles_to_look_at([700, 900, -2400], [1024, 500, 400]).unwrap();
    workspace.camera_rig.free_yaw = yaw;
    workspace.camera_rig.free_pitch = pitch;

    for world in [
        [1024.0f64, 500.0, 0.0],
        [0.0, 0.0, 0.0],
        [2048.0, 1024.0, 0.0],
        [1024.0, 700.0, 1024.0],
    ] {
        let Some(screen) = workspace.project_brush_point_3d(RIG_VIEWPORT, world) else {
            continue;
        };
        let (origin, dir) = workspace
            .camera_ray_for_pointer(RIG_VIEWPORT, screen)
            .expect("pick ray exists");
        // Distance from `world` to the ray line.
        let to_point = [
            world[0] as f32 - origin[0],
            world[1] as f32 - origin[1],
            world[2] as f32 - origin[2],
        ];
        let t = (to_point[0] * dir[0] + to_point[1] * dir[1] + to_point[2] * dir[2])
            / (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).max(f32::EPSILON);
        let closest = [
            origin[0] + dir[0] * t - world[0] as f32,
            origin[1] + dir[1] * t - world[1] as f32,
            origin[2] + dir[2] * t - world[2] as f32,
        ];
        let miss =
            (closest[0].powi(2) + closest[1].powi(2) + closest[2].powi(2)).sqrt();
        assert!(
            miss <= 8.0,
            "pick ray misses the drawn point {world:?} by {miss:.1} world units \
             (screen {screen:?}); draw and pick disagree"
        );
    }
}


/// A brush whose planes describe a different solid than its visible
/// shell renders normally but eats every pick ray: clicks pass through.
/// The editor must diagnose it on load and warn WHICH brush is damaged.
#[test]
fn unpickable_brushes_are_diagnosed_on_load() {
    // A wedge with one plane re-authored inside-out: still "valid",
    // still renders a shell, but rays at its faces miss.
    let mut bad = psxed_project::brush::Brush::cuboid([0, 0, 0], [2048, 1024, 2048]);
    bad.faces[5] = psxed_project::brush::BrushFace::from_points([
        [0, 1024, 0],
        [2048, 1024, 0],
        [2048, 512, 2048],
    ]);
    assert!(bad.solve().is_valid(), "renders as a solid");
    assert!(!bad.is_pickable(), "diagnosed as unpickable");
    assert!(
        bad.raycast([1024.0, 500.0, -2400.0], [0.0, 0.0, 1.0]).is_none(),
        "the live symptom: a ray straight at the front wall misses"
    );
    // A healthy brush passes.
    assert!(psxed_project::brush::Brush::cuboid([0, 0, 0], [128, 128, 128]).is_pickable());

    let mut project = ProjectDocument::new("winding-diagnosis");
    project.active_scene_mut().brushes.push(bad);
    let workspace = EditorWorkspace::with_project(test_temp_dir("winding-diagnosis"), project);
    assert!(
        workspace.status.contains("damaged"),
        "load surfaces the damage, status: {}",
        workspace.status
    );
}

/// The hierarchy's top tier: in Brush mode the gizmo transforms the
/// WHOLE brush. Move translates it rigidly along each axis, Rotate
/// spins it (90 degrees about Y swaps the X/Z extents of the 512x256
/// box), Scale stretches it along the grabbed axis.
#[test]
fn mouse_whole_brush_gizmo_moves_rotates_and_scales() {
    // Move: each axis translates rigidly (size unchanged).
    for axis in 0..3 {
        let mut rig = MouseRig::single_cube("rig-whole-brush-move");
        let base = rig.brush();
        rig.workspace.set_brush_edit_mode(BrushEditMode::Move);
        rig.workspace.set_transform_gizmo_mode(TransformGizmoMode::Move);
        let body = rig.world_to_screen([256.0, 256.0, 128.0]);
        rig.click(body);
        assert_eq!(rig.workspace.selected_brush, Some(0));
        let (grab, to) = rig.gizmo_drag_vector(axis);
        rig.drag(grab, to);
        let after = rig.brush();
        assert_ne!(after, base, "move axis {axis}: brush translated");
        let (sb, sa) = (base.solve(), after.solve());
        for check in 0..3 {
            let size_before = sb.max[check] - sb.min[check];
            let size_after = sa.max[check] - sa.min[check];
            assert!(
                (size_before - size_after).abs() <= 1.0,
                "move axis {axis}: rigid translate keeps size on {check}"
            );
            if check != axis {
                assert!(
                    (sb.min[check] - sa.min[check]).abs() <= 0.5,
                    "move axis {axis}: axis {check} masked"
                );
            }
        }
        rig.workspace.do_undo();
        assert_eq!(rig.brush(), base);
    }

    // Rotate 90 degrees about Y: X/Z extents swap (512x256 footprint).
    let mut rig = MouseRig::single_cube("rig-whole-brush-rotate");
    let base = rig.brush();
    rig.workspace.set_brush_edit_mode(BrushEditMode::Move);
    rig.workspace.set_transform_gizmo_mode(TransformGizmoMode::Rotate);
    let body = rig.world_to_screen([256.0, 256.0, 128.0]);
    rig.click(body);
    let center = rig.world_to_screen(
        rig.workspace.brush_gizmo_context().expect("context").0,
    );
    let (grab, _) = rig.gizmo_drag_vector(1);
    let radial = grab - center;
    let swept = center + Vec2::new(-radial.y, radial.x);
    rig.move_to(grab);
    rig.press(grab);
    rig.move_to(center + (swept - center) * 0.7 + (grab - center) * 0.3);
    rig.move_to(swept);
    rig.release(swept);
    let after = rig.brush();
    assert_ne!(after, base, "rotate changed the brush");
    let (sb, sa) = (base.solve(), after.solve());
    let (bx, bz) = (sb.max[0] - sb.min[0], sb.max[2] - sb.min[2]);
    let (ax, az) = (sa.max[0] - sa.min[0], sa.max[2] - sa.min[2]);
    assert!(
        (ax - bz).abs() <= 2.0 && (az - bx).abs() <= 2.0,
        "quarter turn swaps footprint extents: before {bx}x{bz}, after {ax}x{az}"
    );
    rig.workspace.do_undo();
    assert_eq!(rig.brush(), base);

    // Scale along X: wider, same height/depth.
    let mut rig = MouseRig::single_cube("rig-whole-brush-scale");
    let base = rig.brush();
    rig.workspace.set_brush_edit_mode(BrushEditMode::Move);
    rig.workspace.set_transform_gizmo_mode(TransformGizmoMode::Scale);
    let body = rig.world_to_screen([256.0, 256.0, 128.0]);
    rig.click(body);
    let (grab, to) = rig.gizmo_drag_vector(0);
    rig.drag(grab, to);
    let after = rig.brush();
    assert_ne!(after, base, "scale changed the brush");
    let (sb, sa) = (base.solve(), after.solve());
    assert!(
        sa.max[0] - sa.min[0] > sb.max[0] - sb.min[0] + 16.0,
        "wider along X"
    );
    assert!(
        (sa.max[1] - sa.min[1] - (sb.max[1] - sb.min[1])).abs() <= 1.0,
        "height untouched"
    );
}

/// Texture lock across gestures: a face moved in-plane by the gizmo
/// keeps its applied texture (the same texel stays at the centroid);
/// with the lock off the mapping stays world-anchored and slides.
#[test]
fn face_moves_keep_the_texture_riding_when_locked() {
    let applied_at_anchor = |workspace: &EditorWorkspace| {
        let brush = &workspace.project.active_scene().brushes[0];
        let anchor = brush.face_uv_anchor(5).expect("anchor");
        brush.faces[5].uv.apply(anchor)
    };
    for lock in [true, false] {
        let mut rig = MouseRig::single_cube("rig-uv-lock");
        rig.workspace.project.active_scene_mut().brushes[0].faces[5].uv =
            psxed_project::brush::FaceUv {
                offset_texels: [10, 5],
                rotation_deg: 0,
                scale_q8: [256, 256],
            };
        rig.workspace.brush_texture_lock = lock;
        rig.workspace.set_brush_edit_mode(BrushEditMode::Face);
        rig.workspace.set_transform_gizmo_mode(TransformGizmoMode::Move);
        let body = rig.world_to_screen([256.0, 256.0, 128.0]);
        rig.click(body);
        assert!(matches!(
            rig.workspace.selected_brush_elements.as_slice(),
            [BrushElement::Face(5)]
        ));
        let before = applied_at_anchor(&rig.workspace);
        // In-plane X move via the gizmo.
        let (grab, to) = rig.gizmo_drag_vector(0);
        rig.drag(grab, to);
        assert_ne!(
            rig.workspace.project.active_scene().brushes[0],
            MouseRig::single_cube("rig-uv-lock-base").brush(),
        );
        let after = applied_at_anchor(&rig.workspace);
        let drift =
            ((after[0] - before[0]).powi(2) + (after[1] - before[1]).powi(2)).sqrt();
        if lock {
            assert!(
                drift <= 1.5,
                "locked: texture rides the face, drifted {drift:.1} texels"
            );
        } else {
            assert!(
                drift > 1.5,
                "unlocked: world-anchored mapping slides under the face, drift {drift:.1}"
            );
        }
    }
}

/// Legacy brushes (plane points authored far off the polygon) normalize
/// on load into fresh-drawn form: same solid, same textures, plane
/// points on the corners. The pass is idempotent so projects only dirty
/// once, and damaged brushes are left untouched.
#[test]
fn legacy_brushes_normalize_on_load_same_solid_same_textures() {
    let mut legacy = psxed_project::brush::Brush::cuboid([0, 0, 0], [512, 256, 256]);
    // Re-author the top plane with far-flung points (same plane) and a
    // non-identity texture mapping that must survive.
    legacy.faces[5] = psxed_project::brush::BrushFace::from_points([
        [-4096, 256, 8192],
        [8192, 256, 8192],
        [8192, 256, -4096],
    ]);
    legacy.faces[5].uv = psxed_project::brush::FaceUv {
        offset_texels: [12, -7],
        rotation_deg: 30,
        scale_q8: [512, 256],
    };
    let source_bounds = legacy.solve();

    let clean = legacy.normalized().expect("legacy brush normalizes");
    let clean_bounds = clean.solve();
    for axis in 0..3 {
        assert!(
            (clean_bounds.min[axis] - source_bounds.min[axis]).abs() <= 1.0
                && (clean_bounds.max[axis] - source_bounds.max[axis]).abs() <= 1.0,
            "same solid"
        );
    }
    // Plane points now sit on the polygon (inside the solid bounds).
    for face in &clean.faces {
        for point in face.points {
            assert!(
                (-1..=513).contains(&point[0])
                    && (-1..=257).contains(&point[1])
                    && (-1..=257).contains(&point[2]),
                "authored point {point:?} lies on the solid"
            );
        }
    }
    // Texture mapping carried over untouched.
    assert!(clean.faces.iter().any(|face| face.uv
        == psxed_project::brush::FaceUv {
            offset_texels: [12, -7],
            rotation_deg: 30,
            scale_q8: [512, 256],
        }));
    // Idempotent: normalizing the normalized brush changes nothing.
    assert_eq!(clean.normalized().as_ref(), Some(&clean));

    // Load path: opening a project normalizes and marks it dirty once.
    let mut project = ProjectDocument::new("legacy-normalize");
    project.active_scene_mut().brushes.push(legacy);
    let workspace =
        EditorWorkspace::with_project(test_temp_dir("legacy-normalize"), project);
    assert!(workspace.is_dirty(), "normalization wants a save");
    assert!(
        workspace.status.contains("Normalized"),
        "status: {}",
        workspace.status
    );
    let reopened = workspace.project.clone();
    let workspace =
        EditorWorkspace::with_project(test_temp_dir("legacy-normalize-2"), reopened);
    assert!(!workspace.is_dirty(), "second open is a no-op");
}
