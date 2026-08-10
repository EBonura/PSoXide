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

    /// Nearest brush face under the pointer, via the kernel's convex
    /// raycast: `(brush_index, face_index)`.
    fn pick_brush_face(&self, rect: egui::Rect, pointer: egui::Pos2) -> Option<(usize, usize)> {
        let (origin, dir) = self.camera_ray_for_pointer(rect, pointer)?;
        let origin = origin.map(f64::from);
        let dir = dir.map(f64::from);
        let mut best: Option<(f64, usize, usize)> = None;
        for (index, brush) in self.project.active_scene().brushes.iter().enumerate() {
            if let Some((t, face)) = brush.raycast(origin, dir) {
                if best.is_none_or(|(best_t, _, _)| t < best_t) {
                    best = Some((t, index, face));
                }
            }
        }
        best.map(|(_, index, face)| (index, face))
    }

    fn pick_brush(&self, rect: egui::Rect, pointer: egui::Pos2) -> Option<usize> {
        self.pick_brush_face(rect, pointer).map(|(index, _)| index)
    }

    /// Unsnapped camera-ray ground intersection (y = 0).
    fn brush_ground_point_raw(&self, rect: egui::Rect, pointer: egui::Pos2) -> Option<[f32; 3]> {
        let (origin, dir) = self.camera_ray_for_pointer(rect, pointer)?;
        if dir[1].abs() < 1e-6 {
            return None;
        }
        let t = -origin[1] / dir[1];
        (t > 0.0).then(|| {
            [
                origin[0] + dir[0] * t,
                0.0,
                origin[2] + dir[2] * t,
            ]
        })
    }

    /// Replace the selected brush with six hollow wall slabs (one undo
    /// step); the first slab stays selected. No-op when not hollowable.
    pub(crate) fn hollow_selected_brush(&mut self, thickness: i32) {
        let Some(index) = self.selected_brush else {
            return;
        };
        let Some(slabs) = self
            .project
            .active_scene()
            .brushes
            .get(index)
            .and_then(|brush| brush.hollow(thickness))
        else {
            return;
        };
        self.push_undo();
        let scene = self.project.active_scene_mut();
        let mut slabs = slabs.into_iter();
        scene.brushes[index] = slabs.next().expect("hollow returns six slabs");
        scene.brushes.extend(slabs);
        self.selected_brush_face = None;
    }

    /// Snap every point of the selected brush to the editor grid step,
    /// as one undo step.
    pub(crate) fn snap_selected_brush(&mut self) {
        let Some(index) = self.selected_brush else {
            return;
        };
        let step = (self.snap_units.max(1)) as i32;
        let Some(current) = self.project.active_scene().brushes.get(index).cloned() else {
            return;
        };
        let mut snapped = current.clone();
        snapped.snap_to_grid(step);
        if snapped == current || !snapped.solve().is_valid() {
            return;
        }
        self.push_undo();
        self.project.active_scene_mut().brushes[index] = snapped;
    }

    /// Numeric UV controls for the selected brush face: offset, rotation,
    /// per-axis scale, reset.
    // ponytail: one undo per widget change; a slider drag records several
    // steps. Coalesce with the inspector-transaction wrapper when brush
    // UVs move into a proper inspector panel.
    pub(crate) fn draw_brush_face_uv_controls(&mut self, ui: &mut egui::Ui) {
        let (Some(index), Some(face)) = (self.selected_brush, self.selected_brush_face) else {
            return;
        };
        let Some(current) = self
            .project
            .active_scene()
            .brushes
            .get(index)
            .and_then(|brush| brush.faces.get(face))
            .map(|face| face.uv)
        else {
            return;
        };
        let mut edited = current;
        ui.horizontal(|ui| {
            ui.label("UV");
            let mut off_u = i32::from(edited.offset_texels[0]);
            let mut off_v = i32::from(edited.offset_texels[1]);
            let mut rot = i32::from(edited.rotation_deg);
            let mut scale_u = i32::from(edited.scale_q8[0]) * 100 / 256;
            let mut scale_v = i32::from(edited.scale_q8[1]) * 100 / 256;
            ui.add(egui::DragValue::new(&mut off_u).speed(1).prefix("U "));
            ui.add(egui::DragValue::new(&mut off_v).speed(1).prefix("V "));
            ui.add(
                egui::DragValue::new(&mut rot)
                    .speed(1)
                    .range(-359..=359)
                    .suffix("\u{b0}"),
            );
            ui.add(
                egui::DragValue::new(&mut scale_u)
                    .speed(1)
                    .range(10..=1600)
                    .suffix("% U"),
            );
            ui.add(
                egui::DragValue::new(&mut scale_v)
                    .speed(1)
                    .range(10..=1600)
                    .suffix("% V"),
            );
            edited.offset_texels = [
                off_u.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
                off_v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
            ];
            edited.rotation_deg = rot as i16;
            edited.scale_q8 = [
                (scale_u * 256 / 100).clamp(1, i32::from(i16::MAX)) as i16,
                (scale_v * 256 / 100).clamp(1, i32::from(i16::MAX)) as i16,
            ];
            if ui.button("Reset").clicked() {
                edited = psxed_project::brush::FaceUv::default();
            }
        });
        if edited != current {
            self.push_undo();
            self.project.active_scene_mut().brushes[index].faces[face].uv = edited;
        }
    }

    /// Apply the paint material to the selected brush face, as one undo
    /// step. No-op when no brush face is selected or no material chosen.
    pub(crate) fn apply_material_to_selected_brush_face(&mut self) {
        let (Some(index), Some(face), Some(material)) = (
            self.selected_brush,
            self.selected_brush_face,
            self.brush_material,
        ) else {
            return;
        };
        let scene = self.project.active_scene();
        if scene
            .brushes
            .get(index)
            .and_then(|brush| brush.faces.get(face))
            .is_none()
        {
            return;
        }
        self.push_undo();
        self.project.active_scene_mut().brushes[index].faces[face].material = Some(material);
    }

    /// The cuboid a brush drag would commit, if it has area.
    fn brush_drag_cuboid(drag: BrushDrag) -> Option<psxed_project::brush::Brush> {
        psxed_project::brush::Brush::cuboid_from_corners(
            [drag.anchor[0], 0, drag.anchor[2]],
            [drag.current[0], BRUSH_CREATE_HEIGHT, drag.current[2]],
        )
    }

    /// Snap a 2D top-view world point (XZ) to the brush grid at y = 0.
    pub(crate) fn brush_snap_2d(&self, world: [f32; 2]) -> [i32; 3] {
        let step = (self.snap_units.max(1)) as f32;
        let snap = |v: f32| ((v / step).round() * step) as i32;
        [snap(world[0]), 0, snap(world[1])]
    }

    /// Start a brush-create drag at a snapped point (2D view entry).
    pub(crate) fn begin_brush_drag_2d(&mut self, world: [f32; 2]) {
        let point = self.brush_snap_2d(world);
        self.brush_drag = Some(BrushDrag {
            anchor: point,
            current: point,
        });
    }

    /// Update the in-flight brush-create drag (2D view entry).
    pub(crate) fn update_brush_drag_2d(&mut self, world: [f32; 2]) {
        if let Some(drag) = self.brush_drag {
            self.brush_drag = Some(BrushDrag {
                current: self.brush_snap_2d(world),
                ..drag
            });
        }
    }

    /// Commit the in-flight create drag as a brush, one undo step.
    /// Shared by the 3D tool release and the 2D view release.
    pub(crate) fn commit_brush_drag(&mut self) {
        let Some(drag) = self.brush_drag.take() else {
            return;
        };
        let Some(brush) = Self::brush_drag_cuboid(drag) else {
            return; // zero-area drag: nothing to commit
        };
        self.push_undo();
        let scene = self.project.active_scene_mut();
        scene.brushes.push(brush);
        self.selected_brush = Some(self.project.active_scene().brushes.len() - 1);
        self.selected_brush_face = None;
    }

    /// One clip click at a snapped ground point: the first click stores
    /// the point, the second splits the selected brush by the vertical
    /// plane through both, honouring `brush_clip_keep`. Shared by the 3D
    /// tool and the 2D view.
    pub(crate) fn brush_clip_click(&mut self, point: [i32; 3]) {
        let Some(selected) = self.selected_brush else {
            return;
        };
        match self.brush_clip_start.take() {
            None => self.brush_clip_start = Some(point),
            Some(start) if start == point => {}
            Some(start) => {
                let up = [start[0], BRUSH_CREATE_HEIGHT, start[2]];
                let clipped = self.project.active_scene().brushes[selected].clip([
                    start, point, up,
                ]);
                match (self.brush_clip_keep, clipped.back, clipped.front) {
                    (BrushClipKeep::Both, Some(back), Some(front)) => {
                        self.push_undo();
                        let scene = self.project.active_scene_mut();
                        scene.brushes[selected] = back;
                        scene.brushes.push(front);
                    }
                    (BrushClipKeep::Back, Some(back), Some(_)) => {
                        self.push_undo();
                        self.project.active_scene_mut().brushes[selected] = back;
                    }
                    (BrushClipKeep::Front, Some(_), Some(front)) => {
                        self.push_undo();
                        self.project.active_scene_mut().brushes[selected] = front;
                    }
                    // Plane missed the brush: nothing to keep or drop.
                    _ => {}
                }
            }
        }
    }

    /// Select the brush whose footprint contains the 2D world point,
    /// preferring the smallest footprint (topmost in intent). Returns
    /// whether a brush was hit.
    pub(crate) fn select_brush_at_2d(&mut self, world: [f32; 2]) -> bool {
        let mut best: Option<(f64, usize)> = None;
        for (index, brush) in self.project.active_scene().brushes.iter().enumerate() {
            let solved = brush.solve();
            let inside = f64::from(world[0]) >= solved.min[0]
                && f64::from(world[0]) <= solved.max[0]
                && f64::from(world[1]) >= solved.min[2]
                && f64::from(world[1]) <= solved.max[2];
            if inside {
                let area = (solved.max[0] - solved.min[0]) * (solved.max[2] - solved.min[2]);
                if best.is_none_or(|(best_area, _)| area < best_area) {
                    best = Some((area, index));
                }
            }
        }
        match best {
            Some((_, index)) => {
                self.selected_brush = Some(index);
                self.selected_brush_face = None;
                true
            }
            None => {
                self.selected_brush = None;
                self.selected_brush_face = None;
                false
            }
        }
    }

    /// Brush footprints in the 2D top view: bounds rectangles, the
    /// in-flight create preview, and the pending clip point.
    pub(crate) fn draw_brush_footprints_2d(
        &self,
        painter: &egui::Painter,
        transform: crate::viewport2d::ViewportTransform,
    ) {
        let rect_for = |min: [f64; 3], max: [f64; 3]| {
            let a = transform.world_to_screen([min[0] as f32, min[2] as f32]);
            let b = transform.world_to_screen([max[0] as f32, max[2] as f32]);
            egui::Rect::from_two_pos(a, b)
        };
        for (index, brush) in self.project.active_scene().brushes.iter().enumerate() {
            let solved = brush.solve();
            let selected = self.selected_brush == Some(index);
            let stroke = if selected {
                egui::Stroke::new(2.0, STUDIO_ACCENT)
            } else {
                egui::Stroke::new(1.0, EDITOR_OUTLINE_ACCENT)
            };
            painter.rect_stroke(
                rect_for(solved.min, solved.max),
                0.0,
                stroke,
                egui::StrokeKind::Middle,
            );
        }
        if let Some(preview) = self.brush_drag.and_then(Self::brush_drag_cuboid) {
            let solved = preview.solve();
            painter.rect_stroke(
                rect_for(solved.min, solved.max),
                0.0,
                egui::Stroke::new(1.5, STUDIO_ACCENT),
                egui::StrokeKind::Middle,
            );
        }
        if let Some(start) = self.brush_clip_start {
            let center = transform.world_to_screen([start[0] as f32, start[2] as f32]);
            painter.circle_stroke(center, 4.0, egui::Stroke::new(1.5, STUDIO_ACCENT));
        }
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
            // Emphasize the selected face's polygon on the selected brush.
            if selected {
                if let Some(face) = self.selected_brush_face {
                    if let Some(Some(polygon)) = brush.solve().polygons.get(face) {
                        let count = polygon.verts.len();
                        for i in 0..count {
                            let a = project(polygon.verts[i]);
                            let b = project(polygon.verts[(i + 1) % count]);
                            if let (Some(a), Some(b)) = (a, b) {
                                painter.line_segment(
                                    [a, b],
                                    egui::Stroke::new(3.5, STUDIO_ACCENT),
                                );
                            }
                        }
                    }
                }
            }
        }
        if let Some(preview) = self.brush_drag.and_then(Self::brush_drag_cuboid) {
            draw(&preview, egui::Stroke::new(1.5, STUDIO_ACCENT));
        }
    }
}

/// Vertical extrude sensitivity, world units per pixel (matches the
/// primitive height-drag feel: 8 px per 64-unit quantum).
const EXTRUDE_UNITS_PER_PIXEL: f32 = 8.0;

impl ViewportTool3d for BrushTool {
    fn primary_pressed(&self, ws: &mut EditorWorkspace, frame: &ToolFrame3d) {
        let Some(pointer) = frame.pointer_interact else {
            return;
        };
        // Shift-press on a brush starts a whole-brush move; a plain press
        // on a brush face starts a face extrude; pressing empty ground
        // starts a create drag.
        if frame.modifiers.shift {
            if let Some((index, _)) = ws.pick_brush_face(frame.rect, pointer) {
                let base = ws.project.active_scene().brushes[index].clone();
                let press_ground = ws
                    .brush_ground_point_raw(frame.rect, pointer)
                    .unwrap_or([0.0; 3]);
                ws.selected_brush = Some(index);
                ws.brush_move = Some(BrushMove {
                    index,
                    base,
                    press_ground,
                    applied: [0, 0],
                });
            }
            return;
        }
        if let Some((index, face)) = ws.pick_brush_face(frame.rect, pointer) {
            let base = ws.project.active_scene().brushes[index].clone();
            let Some(plane) =
                psxed_project::brush::Plane::from_points(base.faces[face].points)
            else {
                return;
            };
            let n = plane.normal.map(|v| v as f64);
            let mut axis = 0;
            for candidate in 1..3 {
                if n[candidate].abs() > n[axis].abs() {
                    axis = candidate;
                }
            }
            let dir = if n[axis] >= 0.0 { 1 } else { -1 };
            let press_ground = ws
                .brush_ground_point_raw(frame.rect, pointer)
                .unwrap_or([0.0; 3]);
            ws.selected_brush = Some(index);
            ws.selected_brush_face = Some(face);
            ws.brush_extrude = Some(BrushExtrude {
                index,
                face,
                base,
                axis,
                dir,
                press_y: pointer.y,
                press_ground,
                applied: 0,
            });
        } else if let Some(point) = ws.brush_ground_point(frame.rect, pointer) {
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
        if let Some(mv) = ws.brush_move.clone() {
            let Some(ground) = ws.brush_ground_point_raw(frame.rect, pointer) else {
                return;
            };
            let step = (ws.snap_units.max(1)) as f32;
            let snap = |v: f32| ((v / step).round() * step) as i32;
            let applied = [
                snap(ground[0] - mv.press_ground[0]),
                snap(ground[2] - mv.press_ground[2]),
            ];
            if applied != mv.applied {
                let mut preview = mv.base.clone();
                preview.translate([applied[0], 0, applied[1]]);
                ws.project.active_scene_mut().brushes[mv.index] = preview;
                if let Some(state) = ws.brush_move.as_mut() {
                    state.applied = applied;
                }
            }
            return;
        }
        if let Some(extrude) = ws.brush_extrude.clone() {
            let step = (ws.snap_units.max(1)) as f32;
            let raw_units = if extrude.axis == 1 {
                // Vertical faces follow pixel drag (up = out for +Y).
                (extrude.press_y - pointer.y) * EXTRUDE_UNITS_PER_PIXEL * extrude.dir as f32
            } else {
                // Horizontal faces follow the ground-plane pointer along
                // the face's dominant axis.
                match ws.brush_ground_point_raw(frame.rect, pointer) {
                    Some(ground) => ground[extrude.axis] - extrude.press_ground[extrude.axis],
                    None => return,
                }
            };
            let snapped = ((raw_units / step).round() * step) as i32;
            if snapped == extrude.applied {
                return;
            }
            let mut delta = [0i32; 3];
            // ponytail: dominant-axis extrude; exact for axis faces (all
            // created cuboids), approximate for slopes until face-normal
            // stepping is needed.
            delta[extrude.axis] = if extrude.axis == 1 {
                snapped * extrude.dir
            } else {
                snapped
            };
            let mut preview = extrude.base.clone();
            preview.translate_face(extrude.face, delta);
            if preview.solve().is_valid() {
                ws.project.active_scene_mut().brushes[extrude.index] = preview;
                if let Some(state) = ws.brush_extrude.as_mut() {
                    state.applied = snapped;
                }
            }
        } else if let (Some(drag), Some(point)) =
            (ws.brush_drag, ws.brush_ground_point(frame.rect, pointer))
        {
            ws.brush_drag = Some(BrushDrag {
                current: point,
                ..drag
            });
        }
    }

    fn primary_released(&self, ws: &mut EditorWorkspace, _frame: &ToolFrame3d) {
        if let Some(mv) = ws.brush_move.take() {
            // Restore the base, then rebuild the final position through
            // the lock-aware translate so the undo step captures it.
            ws.project.active_scene_mut().brushes[mv.index] = mv.base.clone();
            if mv.applied != [0, 0] {
                ws.push_undo();
                let mut moved = mv.base;
                let delta = [mv.applied[0], 0, mv.applied[1]];
                if ws.brush_texture_lock {
                    moved.translate_with_uv_lock(
                        delta,
                        psxed_project::brush::BRUSH_UV_UNITS_PER_TEXEL,
                    );
                } else {
                    moved.translate(delta);
                }
                ws.project.active_scene_mut().brushes[mv.index] = moved;
            }
            return;
        }
        if let Some(extrude) = ws.brush_extrude.take() {
            // Restore the base, then record one undo step and re-apply
            // the final shape so a full drag is a single undo entry.
            let live = ws.project.active_scene().brushes[extrude.index].clone();
            ws.project.active_scene_mut().brushes[extrude.index] = extrude.base;
            if extrude.applied != 0 {
                ws.push_undo();
                ws.project.active_scene_mut().brushes[extrude.index] = live;
            }
            return;
        }
        ws.commit_brush_drag();
    }

    fn primary_clicked(&self, ws: &mut EditorWorkspace, frame: &ToolFrame3d) {
        let Some(pointer) = frame.pointer_interact.or(frame.pointer_hover) else {
            return;
        };
        // Modifier-click places clip points: two ground points define a
        // vertical clip plane that splits the selected brush in two.
        if frame.modifiers.command {
            if let Some(point) = ws.brush_ground_point(frame.rect, pointer) {
                ws.brush_clip_click(point);
            }
            return;
        }
        ws.brush_clip_start = None;
        match ws.pick_brush_face(frame.rect, pointer) {
            Some((index, face)) => {
                ws.selected_brush = Some(index);
                ws.selected_brush_face = Some(face);
            }
            None => {
                ws.selected_brush = None;
                ws.selected_brush_face = None;
            }
        }
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
