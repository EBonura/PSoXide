//! 3D-viewport tool dispatch (docs/brush-editor-integration.md, step 1).
//!
//! Each tool is a stateless dispatch object; all in-flight state stays on
//! the workspace (`Interaction`, selection, previews), so tool switches can
//! never orphan private state. egui stays outside: `draw_viewport_3d_body`
//! translates the frame's response into `ToolFrame3d` + event calls, which
//! keeps every tool drivable headlessly through the ViewportHarness.

use super::*;

/// Pre-resolved pointer context for one frame of 3D-viewport input. All
/// fields are `Copy` snapshots so tools take `&mut EditorWorkspace` freely.
#[derive(Clone, Copy)]
pub(crate) struct ToolFrame3d {
    /// Viewport screen rectangle.
    pub rect: egui::Rect,
    /// Pointer position while a press/drag interaction is active.
    pub pointer_interact: Option<egui::Pos2>,
    /// Pointer hover position (may exist without an interaction).
    pub pointer_hover: Option<egui::Pos2>,
    /// Keyboard modifiers this frame.
    pub modifiers: egui::Modifiers,
    /// Topmost pick under the pointer (gizmo > entity > surface).
    pub pointer_target: Option<Viewport3dPointerTarget>,
    /// Room receiving tool actions when no explicit target owns them.
    pub hover_room: Option<NodeId>,
    /// Vertical drag delta this frame (legacy primitive height drags).
    pub drag_delta_y: f32,
}

/// One 3D-viewport tool. Default bodies are no-ops so a tool implements
/// only the events it consumes.
pub(crate) trait ViewportTool3d {
    /// Primary button drag started.
    fn primary_pressed(&self, _ws: &mut EditorWorkspace, _frame: &ToolFrame3d) {}
    /// Primary button held and moving.
    fn primary_dragged(&self, _ws: &mut EditorWorkspace, _frame: &ToolFrame3d) {}
    /// Primary button drag finished.
    fn primary_released(&self, _ws: &mut EditorWorkspace, _frame: &ToolFrame3d) {}
    /// Primary click (press and release without a drag).
    fn primary_clicked(&self, _ws: &mut EditorWorkspace, _frame: &ToolFrame3d) {}
}

/// The active tool's dispatch object. Every non-Select tool routes through
/// the shared paint dispatcher, which re-matches `active_tool` itself;
/// brush/clip/vertex tools will claim their own arms here.
pub(crate) fn tool_impl_3d(tool: ViewTool) -> &'static dyn ViewportTool3d {
    match tool {
        ViewTool::Select => &SelectTool,
        ViewTool::Brush => &BrushTool,
        _ => &PaintDispatchTool,
    }
}

/// Height of a freshly dragged-out brush, world units. Face drags resize
/// it afterwards.
pub(crate) const BRUSH_CREATE_HEIGHT: i32 = 256;

/// Selection, gizmo and drag-translate flows (previously the `select_tool`
/// branch of `draw_viewport_3d_body`).
pub(crate) struct SelectTool;

impl ViewportTool3d for SelectTool {
    fn primary_pressed(&self, ws: &mut EditorWorkspace, frame: &ToolFrame3d) {
        let Some(pointer) = frame.pointer_interact else {
            return;
        };
        match frame.pointer_target {
            Some(Viewport3dPointerTarget::PrimitiveGizmo(axis)) => {
                ws.begin_primitive_gizmo_drag(axis, frame.rect, pointer);
            }
            Some(Viewport3dPointerTarget::NodeGizmo(handle)) => {
                ws.begin_node_gizmo_handle_drag(handle, frame.rect, pointer);
            }
            Some(Viewport3dPointerTarget::Entity(hit))
                if ws.transform_gizmo_mode == TransformGizmoMode::Move =>
            {
                ws.begin_node_drag(hit, frame.rect);
            }
            Some(Viewport3dPointerTarget::Entity(_)) => {}
            Some(Viewport3dPointerTarget::Surface { .. }) => {
                ws.begin_primitive_pointer_drag(frame.rect, pointer, frame.modifiers);
            }
            None => {
                ws.begin_viewport_3d_box_select(pointer, frame.hover_room, frame.modifiers);
            }
        }
    }

    fn primary_dragged(&self, ws: &mut EditorWorkspace, frame: &ToolFrame3d) {
        match ws.interaction {
            Interaction::PrimitiveGizmo(_) => {
                if let Some(p) = frame.pointer_interact {
                    ws.update_primitive_gizmo_drag(p);
                }
            }
            Interaction::NodeGizmo(_) => {
                if let Some(p) = frame.pointer_interact {
                    ws.update_node_gizmo_drag(frame.rect, p);
                }
            }
            Interaction::Node(_) => {
                if let Some(p) = frame.pointer_interact {
                    ws.update_node_drag(frame.rect, p);
                }
            }
            Interaction::PrimitiveGrid(_) => {
                if let Some(p) = frame.pointer_interact {
                    ws.update_primitive_grid_drag(frame.rect, p);
                }
            }
            Interaction::BoxSelect3d(_) => {
                if let Some(p) = frame.pointer_interact.or(frame.pointer_hover) {
                    ws.update_viewport_3d_box_select(p, frame.rect);
                }
            }
            _ => ws.update_primitive_drag(frame.drag_delta_y),
        }
    }

    fn primary_released(&self, ws: &mut EditorWorkspace, _frame: &ToolFrame3d) {
        match ws.interaction {
            Interaction::PrimitiveGizmo(_) => ws.end_primitive_gizmo_drag(),
            Interaction::NodeGizmo(_) => ws.end_node_gizmo_drag(),
            Interaction::Node(_) => ws.end_node_drag(),
            Interaction::PrimitiveGrid(_) => ws.end_primitive_grid_drag(),
            Interaction::BoxSelect3d(_) => ws.end_viewport_3d_box_select(),
            _ => ws.end_primitive_drag(),
        }
    }

    fn primary_clicked(&self, ws: &mut EditorWorkspace, frame: &ToolFrame3d) {
        // Click selection consumes the same topmost target as hover and
        // drag start, so gizmo clicks never fall through to a face behind.
        match frame.pointer_target {
            Some(Viewport3dPointerTarget::Entity(hit)) => {
                let visible_order = ws.scene_node_order();
                ws.apply_node_selection_modifiers(hit.node, frame.modifiers, &visible_order);
            }
            Some(Viewport3dPointerTarget::Surface { .. }) | None => {
                ws.commit_face_selection(frame.modifiers);
            }
            Some(
                Viewport3dPointerTarget::PrimitiveGizmo(_) | Viewport3dPointerTarget::NodeGizmo(_),
            ) => {}
        }
    }
}

/// Paint/erase/water/place tools: one shared dispatcher that re-picks the
/// face under the interact pointer and hands off to `dispatch_paint_3d`
/// (previously the non-select branch of `draw_viewport_3d_body`).
pub(crate) struct PaintDispatchTool;

impl PaintDispatchTool {
    fn paint(ws: &mut EditorWorkspace, frame: &ToolFrame3d) {
        let Some(pos) = frame.pointer_interact else {
            return;
        };
        let face_hit = ws.pick_face_with_hit(frame.rect, pos);
        let fallback = ws
            .active_room_id()
            .and_then(|room| ws.pick_3d_paint_world(frame.rect, pos, room));
        ws.dispatch_paint_3d(face_hit, fallback);
    }
}

/// Brush tool: drag a footprint on the ground plane to create a cuboid
/// brush; click to select the nearest brush under the pointer.
pub(crate) struct BrushTool;

impl EditorWorkspace {
    /// Camera ray intersected with the world ground plane (y = 0),
    /// snapped to the editor grid step.
    fn brush_ground_point(&self, rect: egui::Rect, pointer: egui::Pos2) -> Option<[i32; 3]> {
        let (origin, dir) = self.camera_ray_for_pointer(rect, pointer)?;
        if dir[1].abs() < 1e-6 {
            return None;
        }
        let t = -origin[1] / dir[1];
        if t <= 0.0 {
            return None;
        }
        let step = (self.snap_units.max(1)) as f32;
        let snap = |v: f32| ((v / step).round() * step) as i32;
        Some([snap(origin[0] + dir[0] * t), 0, snap(origin[2] + dir[2] * t)])
    }

    /// Nearest brush under the pointer, via the kernel's convex raycast.
    fn pick_brush(&self, rect: egui::Rect, pointer: egui::Pos2) -> Option<usize> {
        let (origin, dir) = self.camera_ray_for_pointer(rect, pointer)?;
        let origin = origin.map(f64::from);
        let dir = dir.map(f64::from);
        let mut best: Option<(f64, usize)> = None;
        for (index, brush) in self.project.active_scene().brushes.iter().enumerate() {
            if let Some((t, _)) = brush.raycast(origin, dir) {
                if best.is_none_or(|(best_t, _)| t < best_t) {
                    best = Some((t, index));
                }
            }
        }
        best.map(|(_, index)| index)
    }

    /// The cuboid a brush drag would commit, if it has area.
    fn brush_drag_cuboid(drag: BrushDrag) -> Option<psxed_project::brush::Brush> {
        psxed_project::brush::Brush::cuboid_from_corners(
            [drag.anchor[0], 0, drag.anchor[2]],
            [drag.current[0], BRUSH_CREATE_HEIGHT, drag.current[2]],
        )
    }

    /// Wireframe overlay for scene brushes plus the in-flight create
    /// preview, drawn with the editor camera's forward projection.
    // ponytail: re-solves every brush each frame; cache solved polygons
    // on the scene when brush counts grow.
    pub(crate) fn draw_brush_overlay(&self, painter: &egui::Painter, rect: egui::Rect) {
        let camera = self.viewport_3d_camera();
        let project = |w: [f64; 3]| {
            camera
                .normalized_panel_point_for_world([w[0] as f32, w[1] as f32, w[2] as f32])
                .map(|(nx, ny)| {
                    egui::Pos2::new(
                        rect.center().x + nx * rect.width() * 0.5,
                        rect.center().y + ny * rect.height() * 0.5,
                    )
                })
        };
        let mut draw = |brush: &psxed_project::brush::Brush, stroke: egui::Stroke| {
            for polygon in brush.solve().polygons.iter().flatten() {
                let count = polygon.verts.len();
                for i in 0..count {
                    let a = project(polygon.verts[i]);
                    let b = project(polygon.verts[(i + 1) % count]);
                    if let (Some(a), Some(b)) = (a, b) {
                        painter.line_segment([a, b], stroke);
                    }
                }
            }
        };
        for (index, brush) in self.project.active_scene().brushes.iter().enumerate() {
            let selected = self.selected_brush == Some(index);
            let stroke = if selected {
                egui::Stroke::new(2.0, STUDIO_ACCENT)
            } else {
                egui::Stroke::new(1.0, EDITOR_OUTLINE_ACCENT)
            };
            draw(brush, stroke);
        }
        if let Some(preview) = self.brush_drag.and_then(Self::brush_drag_cuboid) {
            draw(&preview, egui::Stroke::new(1.5, STUDIO_ACCENT));
        }
    }
}

impl ViewportTool3d for BrushTool {
    fn primary_pressed(&self, ws: &mut EditorWorkspace, frame: &ToolFrame3d) {
        let Some(pointer) = frame.pointer_interact else {
            return;
        };
        if let Some(point) = ws.brush_ground_point(frame.rect, pointer) {
            ws.brush_drag = Some(BrushDrag {
                anchor: point,
                current: point,
            });
        }
    }

    fn primary_dragged(&self, ws: &mut EditorWorkspace, frame: &ToolFrame3d) {
        let Some(pointer) = frame.pointer_interact.or(frame.pointer_hover) else {
            return;
        };
        if let (Some(drag), Some(point)) = (ws.brush_drag, ws.brush_ground_point(frame.rect, pointer))
        {
            ws.brush_drag = Some(BrushDrag {
                current: point,
                ..drag
            });
        }
    }

    fn primary_released(&self, ws: &mut EditorWorkspace, _frame: &ToolFrame3d) {
        let Some(drag) = ws.brush_drag.take() else {
            return;
        };
        let Some(brush) = EditorWorkspace::brush_drag_cuboid(drag) else {
            return; // zero-area drag: nothing to commit
        };
        ws.push_undo();
        let scene = ws.project.active_scene_mut();
        scene.brushes.push(brush);
        ws.selected_brush = Some(ws.project.active_scene().brushes.len() - 1);
    }

    fn primary_clicked(&self, ws: &mut EditorWorkspace, frame: &ToolFrame3d) {
        let Some(pointer) = frame.pointer_interact.or(frame.pointer_hover) else {
            return;
        };
        ws.selected_brush = ws.pick_brush(frame.rect, pointer);
    }
}

impl ViewportTool3d for PaintDispatchTool {
    fn primary_dragged(&self, ws: &mut EditorWorkspace, frame: &ToolFrame3d) {
        // The eyedropper is deliberately click-only. Keeping it armed
        // throughout a drag prevents a sample gesture from falling
        // through into Paint after the one-shot state clears.
        if !ws.material_paint_sampling {
            Self::paint(ws, frame);
        }
    }

    fn primary_clicked(&self, ws: &mut EditorWorkspace, frame: &ToolFrame3d) {
        Self::paint(ws, frame);
    }
}
