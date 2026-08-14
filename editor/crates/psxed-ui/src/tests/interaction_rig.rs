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
        self.time += 1.0 / 60.0;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(800.0, 600.0),
            )),
            time: Some(self.time),
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

    pub(crate) fn gizmo_axis_grab_point(&self, axis: usize) -> Pos2 {
        let axes = self
            .workspace
            .brush_element_gizmo_axes_3d(RIG_VIEWPORT)
            .expect("element gizmo visible");
        let (origin, tip) = axes[axis];
        origin + (tip - origin) * 0.6
    }

    /// Screen position of a brush handle for the current edit mode.
    pub(crate) fn handle_screen(&self, handle_world: [f64; 3]) -> Pos2 {
        self.world_to_screen(handle_world)
    }

    pub(crate) fn brush(&self) -> psxed_project::brush::Brush {
        self.workspace.project.active_scene().brushes[0].clone()
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
    for (mode, anchor) in cases {
        for sloppy in [false, true] {
            for gizmo_mode in [
                TransformGizmoMode::Move,
                TransformGizmoMode::Rotate,
                TransformGizmoMode::Scale,
            ] {
                let label = format!("{mode:?}/{gizmo_mode:?}/sloppy={sloppy}");
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

                // 3. Grab the gizmo Y axis (X for scale so the change is
                // visible on the long axis) and drag.
                let axis = if gizmo_mode == TransformGizmoMode::Scale {
                    0
                } else {
                    1
                };
                let grab = rig.gizmo_axis_grab_point(axis);
                rig.drag(grab, grab + Vec2::new(90.0, -60.0));

                let after = rig.brush();
                assert_ne!(after, base, "{label}: gizmo drag changes the brush");
                let solved = after.solve();
                assert!(
                    solved.is_valid()
                        && solved
                            .within_extent(psxed_project::brush::BRUSH_EDIT_EXTENT_LIMIT),
                    "{label}: result stays valid and bounded"
                );
                rig.workspace.do_undo();
                assert_eq!(rig.brush(), base, "{label}: one undo restores");
            }
        }
    }
}
