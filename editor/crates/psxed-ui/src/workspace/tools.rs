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

fn brush_contents_outline(contents: psxed_project::brush::BrushContents) -> egui::Color32 {
    match contents {
        psxed_project::brush::BrushContents::Solid => EDITOR_OUTLINE_ACCENT,
        psxed_project::brush::BrushContents::Water => egui::Color32::from_rgb(72, 156, 232),
        psxed_project::brush::BrushContents::Slime => egui::Color32::from_rgb(116, 196, 72),
        psxed_project::brush::BrushContents::Lava => egui::Color32::from_rgb(240, 112, 48),
    }
}

/// Selection, gizmo and drag-translate flows (previously the `select_tool`
/// branch of `draw_viewport_3d_body`).
pub(crate) struct SelectTool;

impl ViewportTool3d for SelectTool {
    fn primary_pressed(&self, ws: &mut EditorWorkspace, frame: &ToolFrame3d) {
        let Some(pointer) = frame.pointer_interact else {
            return;
        };
        let additive = frame.modifiers.shift || frame.modifiers.command || frame.modifiers.ctrl;
        // Reshape handles (Resize/Edge/Vertex) stick out past the brush
        // silhouette, so a handle hit forwards to the Brush gestures even
        // when the pick ray misses the solid itself; whole-brush Move still
        // requires pressing on the brush body.
        if !additive
            && ws.selected_brush.is_some()
            && (ws.pick_brush_handle_3d(frame.rect, pointer).is_some()
                || (ws.brush_edit_mode == BrushEditMode::Move
                    && matches!(
                        frame.pointer_target,
                        Some(Viewport3dPointerTarget::Brush { .. })
                    )))
        {
            BrushTool.primary_pressed(ws, frame);
            return;
        }
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
            Some(Viewport3dPointerTarget::Brush { .. }) if additive => {
                ws.begin_viewport_3d_box_select(pointer, frame.hover_room, frame.modifiers);
            }
            Some(Viewport3dPointerTarget::Brush { .. }) => {}
            Some(Viewport3dPointerTarget::Surface { .. }) => {
                ws.begin_primitive_pointer_drag(frame.rect, pointer, frame.modifiers);
            }
            None => {
                ws.begin_viewport_3d_box_select(pointer, frame.hover_room, frame.modifiers);
            }
        }
    }

    fn primary_dragged(&self, ws: &mut EditorWorkspace, frame: &ToolFrame3d) {
        if ws.brush_move.is_some()
            || ws.brush_vertex_drag.is_some()
            || ws.brush_extrude.is_some()
            || ws.brush_drag.is_some()
        {
            BrushTool.primary_dragged(ws, frame);
            return;
        }
        match ws.interaction {
            Interaction::PrimitiveGizmo(_) => {
                if let Some(p) = frame.pointer_interact {
                    ws.update_primitive_gizmo_drag(p);
                }
            }
            Interaction::NodeGizmo(_) => {
                if let Some(p) = frame.pointer_interact {
                    ws.update_node_gizmo_drag(frame.rect, p, frame.modifiers.shift);
                }
            }
            Interaction::Node(_) => {
                if let Some(p) = frame.pointer_interact {
                    ws.update_node_drag(frame.rect, p, frame.modifiers.shift);
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
        if ws.brush_move.is_some()
            || ws.brush_vertex_drag.is_some()
            || ws.brush_extrude.is_some()
            || ws.brush_drag.is_some()
        {
            BrushTool.primary_released(ws, _frame);
            return;
        }
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
            Some(Viewport3dPointerTarget::Brush { brush, face, .. }) => {
                let (brush, face) = frame
                    .pointer_interact
                    .or(frame.pointer_hover)
                    .and_then(|pointer| {
                        ws.pick_brush_face_cycled_for_selection_3d(frame.rect, pointer)
                    })
                    .map(|(brush, face, _)| (brush, face))
                    .unwrap_or((brush, face));
                ws.clear_node_selection_state();
                ws.clear_resource_selection_state();
                ws.clear_primitive_selection_state();
                ws.clear_sector_selection();
                if frame.modifiers.shift || frame.modifiers.command || frame.modifiers.ctrl {
                    ws.toggle_brush_selection(brush);
                } else {
                    ws.replace_brush_selection(brush, Some(face));
                }
                ws.status = format!("Selected BSP brush {}", brush + 1);
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
        if ws.active_tool == ViewTool::Place
            && ws.active_room_id().is_none()
            && ws.bsp_authoring_root().is_some()
        {
            let Some((brush, face, hit)) = ws.pick_brush_face_with_hit(frame.rect, pos) else {
                ws.status = "Place on an upward-facing BSP brush surface".to_string();
                return;
            };
            ws.place_bsp_on_brush_face(brush, face, hit);
            return;
        }
        // BSP scenes have no grid cells, so Material Paint addresses brush
        // faces directly instead of falling through to the room lane.
        if ws.bsp_face_paint_active() {
            ws.paint_bsp_brush_face(frame.rect, pos);
            return;
        }
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
        Some([
            snap(origin[0] + dir[0] * t),
            0,
            snap(origin[2] + dir[2] * t),
        ])
    }

    /// Nearest brush face under the pointer, via the kernel's convex
    /// raycast: `(brush_index, face_index, world_hit)`.
    ///
    /// The hit point is shared with BSP-native entity placement so the
    /// Place tool and Brush tool cannot disagree about which authored
    /// surface was clicked.
    pub(crate) fn pick_brush_face_with_hit(
        &self,
        rect: egui::Rect,
        pointer: egui::Pos2,
    ) -> Option<(usize, usize, [f32; 3])> {
        self.brush_face_hits_with_hit(rect, pointer)
            .into_iter()
            .next()
    }

    fn brush_face_hits_with_hit(
        &self,
        rect: egui::Rect,
        pointer: egui::Pos2,
    ) -> Vec<(usize, usize, [f32; 3])> {
        let Some((origin, dir)) = self.camera_ray_for_pointer(rect, pointer) else {
            return Vec::new();
        };
        let origin_f64 = origin.map(f64::from);
        let dir_f64 = dir.map(f64::from);
        let mut hits = Vec::new();
        for (index, brush) in self.project.active_scene().brushes.iter().enumerate() {
            if let Some((t, face)) = brush.raycast(origin_f64, dir_f64) {
                hits.push((t, index, face));
            }
        }
        hits.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        hits.into_iter()
            .map(|(t, index, face)| {
                (
                    index,
                    face,
                    [
                        origin[0] + dir[0] * t as f32,
                        origin[1] + dir[1] * t as f32,
                        origin[2] + dir[2] * t as f32,
                    ],
                )
            })
            .collect()
    }

    /// Selection hits use exact convex rays first. Only when no ray lands do
    /// they fall back to an 8 px projected-polygon tolerance, which makes
    /// sub-pixel distant brushes selectable without stealing exact hits from
    /// foreground surfaces.
    fn brush_face_hits_for_selection_3d(
        &self,
        rect: egui::Rect,
        pointer: egui::Pos2,
    ) -> Vec<(usize, usize, [f32; 3])> {
        let exact = self.brush_face_hits_with_hit(rect, pointer);
        if !exact.is_empty() {
            return exact;
        }
        if !rect.contains(pointer) {
            return Vec::new();
        }
        let camera = self.viewport_3d_camera();
        let origin = camera.basis().position;
        let mut hits = Vec::<(f32, usize, usize, [f32; 3])>::new();
        for (index, brush) in self.project.active_scene().brushes.iter().enumerate() {
            let solved = brush.solve();
            if !solved.is_valid() {
                continue;
            }
            let mut best: Option<(f32, usize, [f32; 3])> = None;
            for (face, polygon) in solved.polygons.iter().enumerate() {
                let Some(polygon) = polygon else { continue };
                let projected = polygon
                    .verts
                    .iter()
                    .copied()
                    .map(|point| self.project_brush_point_3d(rect, point))
                    .collect::<Option<Vec<_>>>();
                let Some(projected) = projected else {
                    continue;
                };
                if !point_in_polygon_2d(pointer, &projected)
                    && distance_to_polygon_edges_2d(pointer, &projected) > BRUSH_SCREEN_PICK_RADIUS
                {
                    continue;
                }
                let count = polygon.verts.len() as f32;
                let center: [f32; 3] = std::array::from_fn(|axis| {
                    polygon
                        .verts
                        .iter()
                        .map(|point| point[axis] as f32)
                        .sum::<f32>()
                        / count
                });
                let distance = distance3_f32(origin, center);
                if best.is_none_or(|(best_distance, best_face, _)| {
                    distance < best_distance || (distance == best_distance && face < best_face)
                }) {
                    best = Some((distance, face, center));
                }
            }
            if let Some((distance, face, center)) = best {
                hits.push((distance, index, face, center));
            }
        }
        hits.sort_by(|a, b| {
            a.0.total_cmp(&b.0)
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.2.cmp(&b.2))
        });
        hits.into_iter()
            .map(|(_, index, face, center)| (index, face, center))
            .collect()
    }

    pub(crate) fn pick_brush_face_nearest_for_selection_3d(
        &self,
        rect: egui::Rect,
        pointer: egui::Pos2,
    ) -> Option<(usize, usize, [f32; 3])> {
        self.brush_face_hits_for_selection_3d(rect, pointer)
            .into_iter()
            .next()
    }

    fn pick_brush_face_cycled_for_selection_3d(
        &self,
        rect: egui::Rect,
        pointer: egui::Pos2,
    ) -> Option<(usize, usize, [f32; 3])> {
        let hits = self.brush_face_hits_for_selection_3d(rect, pointer);
        let next = self
            .selected_brush
            .and_then(|selected| hits.iter().position(|(index, _, _)| *index == selected))
            .map_or(0, |position| (position + 1) % hits.len().max(1));
        hits.get(next).copied()
    }

    fn pick_brush_face_for_move_3d(
        &self,
        rect: egui::Rect,
        pointer: egui::Pos2,
    ) -> Option<(usize, usize, [f32; 3])> {
        let hits = self.brush_face_hits_for_selection_3d(rect, pointer);
        self.selected_brush
            .and_then(|selected| {
                hits.iter()
                    .find(|(index, _, _)| *index == selected)
                    .copied()
            })
            .or_else(|| hits.first().copied())
    }

    /// Nearest brush face under the pointer: `(brush_index, face_index)`.
    fn pick_brush_face(&self, rect: egui::Rect, pointer: egui::Pos2) -> Option<(usize, usize)> {
        self.pick_brush_face_with_hit(rect, pointer)
            .map(|(index, face, _)| (index, face))
    }

    /// Unsnapped camera-ray ground intersection (y = 0).
    fn brush_ground_point_raw(&self, rect: egui::Rect, pointer: egui::Pos2) -> Option<[f32; 3]> {
        let (origin, dir) = self.camera_ray_for_pointer(rect, pointer)?;
        if dir[1].abs() < 1e-6 {
            return None;
        }
        let t = -origin[1] / dir[1];
        (t > 0.0).then(|| [origin[0] + dir[0] * t, 0.0, origin[2] + dir[2] * t])
    }

    /// All selected brush indices: the multi-selection when one exists,
    /// else the primary alone. Sorted, deduplicated, stale-free.
    pub(crate) fn selected_brush_set(&self) -> Vec<usize> {
        let mut set: Vec<usize> = if self.selected_brushes.is_empty() {
            self.selected_brush.into_iter().collect()
        } else {
            self.selected_brushes.clone()
        };
        set.sort_unstable();
        set.dedup();
        let count = self.project.active_scene().brushes.len();
        set.retain(|index| *index < count);
        set
    }

    /// Whether `index` is the primary selection or a multi-selection
    /// member (drives the highlight in every view).
    pub(crate) fn brush_is_selected(&self, index: usize) -> bool {
        self.selected_brush == Some(index) || self.selected_brushes.contains(&index)
    }

    /// Plain-click selection: exactly one brush (and optional face),
    /// dropping any multi-selection.
    pub(crate) fn replace_brush_selection(&mut self, index: usize, face: Option<usize>) {
        if self.selected_brush != Some(index) || self.selected_brush_face != face {
            self.clear_uv_edit_transaction();
        }
        self.selected_brush = Some(index);
        self.selected_brushes = vec![index];
        self.selected_brush_face = face;
    }

    pub(crate) fn clear_brush_selection(&mut self) {
        self.clear_uv_edit_transaction();
        self.selected_brush = None;
        self.selected_brushes.clear();
        self.selected_brush_face = None;
    }

    /// Shift-click selection: toggle membership. The primary follows the
    /// toggled brush, or the last remaining member after a removal.
    pub(crate) fn toggle_brush_selection(&mut self, index: usize) {
        if self.selected_brushes.is_empty() {
            if let Some(primary) = self.selected_brush {
                self.selected_brushes.push(primary);
            }
        }
        if let Some(position) = self.selected_brushes.iter().position(|i| *i == index) {
            self.selected_brushes.remove(position);
            if self.selected_brush == Some(index) {
                self.selected_brush = self.selected_brushes.last().copied();
                self.selected_brush_face = None;
            }
        } else {
            self.selected_brushes.push(index);
            self.selected_brush = Some(index);
            self.selected_brush_face = None;
        }
    }

    /// Drop selection entries that no longer resolve to a brush (after
    /// undo/redo or an external document change) and clamp the face.
    pub(crate) fn reconcile_brush_selection(&mut self) {
        let count = self.project.active_scene().brushes.len();
        self.selected_brushes.retain(|index| *index < count);
        if self.selected_brush.is_some_and(|index| index >= count) {
            self.selected_brush = self.selected_brushes.first().copied();
            self.selected_brush_face = None;
        }
        if let (Some(index), Some(face)) = (self.selected_brush, self.selected_brush_face) {
            let faces = self
                .project
                .active_scene()
                .brushes
                .get(index)
                .map_or(0, |brush| brush.faces.len());
            if face >= faces {
                self.selected_brush_face = None;
            }
        }
        if self.selected_brush.is_none() {
            self.selected_brushes.clear();
            self.selected_brush_face = None;
        }
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
        self.mark_dirty();
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
        self.mark_dirty();
    }

    /// Cancel every in-flight brush gesture: the create drag, a pending
    /// clip point, and live extrude/move/vertex previews (restoring
    /// their base).
    pub(crate) fn cancel_brush_gestures(&mut self) {
        self.brush_drag = None;
        self.brush_clip_start = None;
        if let Some(extrude) = self.brush_extrude.take() {
            if let Some(slot) = self
                .project
                .active_scene_mut()
                .brushes
                .get_mut(extrude.index)
            {
                *slot = extrude.base;
            }
        }
        if let Some(mv) = self.brush_move.take() {
            if let Some(slot) = self.project.active_scene_mut().brushes.get_mut(mv.index) {
                *slot = mv.base;
            }
            for (index, base) in mv.others {
                if let Some(slot) = self.project.active_scene_mut().brushes.get_mut(index) {
                    *slot = base;
                }
            }
        }
        if let Some(drag) = self.brush_vertex_drag.take() {
            if let Some(slot) = self.project.active_scene_mut().brushes.get_mut(drag.index) {
                *slot = drag.base;
            }
        }
    }

    /// Delete every selected brush as one undo step.
    pub(crate) fn delete_selected_brushes(&mut self) {
        let targets = self.selected_brush_set();
        self.clear_brush_selection();
        if targets.is_empty() {
            return;
        }
        self.push_undo();
        for index in targets.iter().rev() {
            self.project.active_scene_mut().brushes.remove(*index);
        }
        self.mark_dirty();
    }

    /// Keyboard for the Brush tool: Escape cancels the in-flight gesture,
    /// Delete or Backspace removes the selected brush. Inert while a text
    /// field owns focus.
    pub(crate) fn brush_tool_keyboard(&mut self, ui: &egui::Ui) {
        if self.active_tool != ViewTool::Brush {
            return;
        }
        if ui.ctx().memory(|memory| memory.focused().is_some()) {
            return;
        }
        let (escape, delete) = ui.input(|input| {
            (
                input.key_pressed(egui::Key::Escape),
                input.key_pressed(egui::Key::Delete) || input.key_pressed(egui::Key::Backspace),
            )
        });
        if escape {
            self.cancel_brush_gestures();
        }
        if delete {
            self.delete_selected_brushes();
        }
    }

    /// Inspector content for the selected brush: summary, actions, and
    /// the face section (material + UV placement) when a face is
    /// selected. Mirrors the classic face-inspector shape: material name
    /// and texture size above offset/scale/rotation controls.
    pub(crate) fn draw_brush_inspector(&mut self, ui: &mut egui::Ui) {
        let Some(index) = self.selected_brush else {
            return;
        };
        let Some(mut brush) = self.project.active_scene().brushes.get(index).cloned() else {
            return;
        };
        let solved = brush.solve();
        ui.label(
            egui::RichText::new(format!("Brush {index}"))
                .strong()
                .size(14.0),
        );
        ui.label(format!(
            "{} faces, {:.0} x {:.0} x {:.0}",
            brush.faces.len(),
            solved.max[0] - solved.min[0],
            solved.max[1] - solved.min[1],
            solved.max[2] - solved.min[2],
        ));
        let selection_count = self.selected_brush_set().len();
        ui.label(egui::RichText::new("Brush Transform").strong());
        ui.horizontal_wrapped(|ui| self.draw_brush_edit_mode_controls(ui));
        ui.label(
            egui::RichText::new(format!(
                "{} mode: {}. Dragging snaps to {} units. Ctrl/Cmd+Z undoes one gesture.",
                self.brush_edit_mode.label(),
                self.brush_edit_mode.gesture_hint(),
                self.snap_units.max(1)
            ))
            .small()
            .color(STUDIO_TEXT_WEAK),
        );
        if selection_count > 1 {
            ui.label(
                egui::RichText::new(format!(
                    "{selection_count} brushes selected. Move changes the group; Resize and Size change the primary brush."
                ))
                .small()
                .color(STUDIO_TEXT_WEAK),
            );
        }
        // Numeric placement fallback: exact (unsnapped) min-corner entry.
        if let Some(origin) = self.selected_brush_origin() {
            let mut edited = origin;
            ui.label("Move (origin)");
            ui.columns(3, |columns| {
                for (axis, column) in columns.iter_mut().enumerate() {
                    column.label(["X", "Y", "Z"][axis]);
                    column.add_sized(
                        [column.available_width(), 22.0],
                        egui::DragValue::new(&mut edited[axis]).speed(1),
                    );
                }
            });
            if edited != origin {
                self.set_selected_brush_origin(edited);
            }
        }
        if let Some(size) = self.selected_brush_size() {
            let mut edited = size;
            ui.label("Resize (size)");
            ui.columns(3, |columns| {
                for (axis, column) in columns.iter_mut().enumerate() {
                    column.label(["X", "Y", "Z"][axis]);
                    column.add_sized(
                        [column.available_width(), 22.0],
                        egui::DragValue::new(&mut edited[axis])
                            .speed(1)
                            .range(1..=i32::MAX),
                    );
                }
            });
            if edited != size && !self.set_selected_brush_size(edited) {
                self.status =
                    "Size edit rejected: use Face mode for non-axis-aligned brushes".to_string();
            }
        }
        ui.horizontal_wrapped(|ui| {
            if ui.button("Duplicate").clicked() {
                self.duplicate_selected_brushes();
            }
            if ui.button("Hollow").clicked() {
                let thickness = (self.snap_units.max(1)) as i32;
                self.hollow_selected_brush(thickness);
            }
            if ui.button("Snap to grid").clicked() {
                self.snap_selected_brush();
            }
            if ui.button("Delete").clicked() {
                self.delete_selected_brushes();
            }
        });

        let primary_contents = brush.contents;
        let mixed_contents = self.selected_brush_set().iter().any(|&selected| {
            self.project
                .active_scene()
                .brushes
                .get(selected)
                .is_some_and(|selected_brush| selected_brush.contents != primary_contents)
        });
        let mut requested_contents = None;
        ui.horizontal(|ui| {
            ui.label("BSP contents");
            egui::ComboBox::from_id_salt(("brush-contents", index))
                .selected_text(if mixed_contents {
                    "Mixed"
                } else {
                    primary_contents.label()
                })
                .show_ui(ui, |ui| {
                    for option in psxed_project::brush::BrushContents::ALL {
                        if ui
                            .selectable_label(
                                !mixed_contents && primary_contents == option,
                                option.label(),
                            )
                            .clicked()
                        {
                            requested_contents = Some(option);
                        }
                    }
                });
        });
        if let Some(contents) = requested_contents {
            self.set_selected_brush_contents(contents);
            brush.contents = contents;
            if !contents.is_solid() {
                brush.mover = None;
            }
        }
        if !brush.contents.is_solid() {
            ui.label(
                egui::RichText::new(
                    "Liquid contents are non-blocking. The textured boundary remains visible and the runtime samples the volume inside it.",
                )
                .small()
                .color(STUDIO_TEXT_WEAK),
            );
        }

        let movers: Vec<_> = self
            .project
            .active_scene()
            .nodes()
            .iter()
            .filter_map(|node| {
                matches!(
                    &node.kind,
                    psxed_project::NodeKind::Logic {
                        kind: psxed_project::LogicNodeKind::Door { .. },
                        ..
                    }
                )
                .then(|| (node.id, node.name.clone()))
            })
            .collect();
        let mut mover = brush.mover;
        let mover_label = mover
            .and_then(|id| {
                movers
                    .iter()
                    .find(|(candidate, _)| *candidate == id)
                    .map(|(_, name)| name.as_str())
            })
            .unwrap_or(if mover.is_some() {
                "Missing mover"
            } else {
                "World (static)"
            });
        ui.add_enabled_ui(brush.contents.is_solid(), |ui| {
            ui.horizontal(|ui| {
                ui.label("Model owner");
                egui::ComboBox::from_id_salt(("brush-mover", index))
                    .selected_text(mover_label)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut mover, None, "World (static)");
                        for (node, name) in &movers {
                            ui.selectable_value(&mut mover, Some(*node), name);
                        }
                    });
            });
        });
        if mover != brush.mover {
            self.set_selected_brush_mover(mover);
        }
        if brush.mover.is_some() && mover_label == "Missing mover" {
            ui.colored_label(
                egui::Color32::from_rgb(220, 120, 100),
                "The bound mover no longer exists. Rebind this brush before cooking.",
            );
        }

        ui.separator();
        // Above the per-face early return on purpose: texture lock applies
        // to the whole brush and has to be reachable with no face selected.
        self.draw_brush_texture_mapping_header(ui);
        let Some(face) = self.selected_brush_face else {
            ui.label(
                egui::RichText::new(
                    "Select a face to edit its material and UVs: click the face in the \
                     3D view, or click the brush in a 2D view and then its face in 3D.",
                )
                .weak(),
            );
            return;
        };
        let Some(face_data) = brush.faces.get(face) else {
            return;
        };
        let plane_label = psxed_project::brush::Plane::from_points(face_data.points)
            .map(|plane| {
                let n = plane.normal;
                let abs = [n[0].abs(), n[1].abs(), n[2].abs()];
                let axis = if abs[1] >= abs[0] && abs[1] >= abs[2] {
                    if n[1] > 0 {
                        "floor-facing (+Y)"
                    } else {
                        "ceiling-facing (-Y)"
                    }
                } else if abs[0] >= abs[2] {
                    if n[0] > 0 {
                        "wall (+X)"
                    } else {
                        "wall (-X)"
                    }
                } else if n[2] > 0 {
                    "wall (+Z)"
                } else {
                    "wall (-Z)"
                };
                axis.to_string()
            })
            .unwrap_or_else(|| "degenerate".to_string());
        ui.label(egui::RichText::new(format!("Face {face} of {}", brush.faces.len())).strong());
        ui.label(plane_label);
        // Numeric face fallback: slide the plane along its dominant axis.
        if let Some((axis, position)) = self.selected_brush_face_axis() {
            let mut edited = position;
            ui.horizontal(|ui| {
                ui.label(format!("Plane {} at", ["X", "Y", "Z"][axis]));
                ui.add(egui::DragValue::new(&mut edited).speed(1));
            });
            if edited != position && !self.set_selected_brush_face_axis_position(edited) {
                self.status = "Face edit rejected: brush would stop enclosing volume".to_string();
            }
        }
        let material_name = face_data.material.and_then(|id| {
            self.project
                .material_options()
                .into_iter()
                .find(|(option, _)| *option == id)
                .map(|(_, name)| name)
        });
        ui.label(match material_name {
            Some(name) => format!("Material: {name}"),
            None => "Material: none (flat grey)".to_string(),
        });
        self.draw_brush_material_picker(ui);
        if ui
            .button(icons::label(icons::PALETTE, "Apply to face"))
            .clicked()
        {
            self.apply_material_to_selected_brush_face();
        }
        self.draw_brush_face_uv_controls(ui);
    }

    /// Set every selected brush to one structural/liquid contents kind as one
    /// undo step. Liquids are always static world volumes, so changing a Door
    /// brush to liquid clears its mover binding in the same atomic edit.
    pub(crate) fn set_selected_brush_contents(
        &mut self,
        contents: psxed_project::brush::BrushContents,
    ) {
        let targets = self.selected_brush_set();
        if targets.is_empty()
            || targets.iter().all(|&index| {
                self.project
                    .active_scene()
                    .brushes
                    .get(index)
                    .is_none_or(|brush| brush.contents == contents)
            })
        {
            return;
        }
        self.push_undo();
        let mut unbound = 0usize;
        for index in targets {
            let Some(brush) = self.project.active_scene_mut().brushes.get_mut(index) else {
                continue;
            };
            brush.contents = contents;
            if !contents.is_solid() && brush.mover.take().is_some() {
                unbound += 1;
            }
        }
        self.mark_dirty();
        self.status = if unbound == 0 {
            format!("Brush contents set to {}", contents.label())
        } else {
            format!(
                "Brush contents set to {}; removed {unbound} Door binding{} because liquid movers are unsupported",
                contents.label(),
                if unbound == 1 { "" } else { "s" }
            )
        };
    }

    /// Bind the selected brush to one Door logic node, or return it to model 0.
    pub(crate) fn set_selected_brush_mover(&mut self, mover: Option<NodeId>) {
        let Some(index) = self.selected_brush else {
            return;
        };
        if mover.is_some()
            && self
                .project
                .active_scene()
                .brushes
                .get(index)
                .is_some_and(|brush| !brush.contents.is_solid())
        {
            self.status =
                "Liquid brushes are static BSP contents and cannot be bound to a Door".to_string();
            return;
        }
        if let Some(mover) = mover {
            let valid = self.project.active_scene().node(mover).is_some_and(|node| {
                matches!(
                    &node.kind,
                    psxed_project::NodeKind::Logic {
                        kind: psxed_project::LogicNodeKind::Door { .. },
                        ..
                    }
                )
            });
            if !valid {
                return;
            }
        }
        let current = self
            .project
            .active_scene()
            .brushes
            .get(index)
            .and_then(|brush| brush.mover);
        if current == mover {
            return;
        }
        self.push_undo();
        if let Some(brush) = self.project.active_scene_mut().brushes.get_mut(index) {
            brush.mover = mover;
        }
        self.mark_dirty();
    }

    /// Duplicate every selected brush (offset by one grid step, honouring
    /// texture lock) and select the copies. One undo step; the primary
    /// selection follows its own copy.
    pub(crate) fn duplicate_selected_brushes(&mut self) {
        let targets = self.selected_brush_set();
        if targets.is_empty() {
            return;
        }
        let primary = self.selected_brush;
        self.push_undo();
        let step = (self.snap_units.max(1)) as i32;
        let texture_lock = self.brush_texture_lock;
        let mut new_primary = None;
        let mut new_selection = Vec::new();
        for &index in &targets {
            let mut copy = self.project.active_scene().brushes[index].clone();
            if texture_lock {
                copy.translate_with_uv_lock(
                    [step, 0, step],
                    psxed_project::brush::BRUSH_UV_UNITS_PER_TEXEL,
                );
            } else {
                copy.translate([step, 0, step]);
            }
            let scene = self.project.active_scene_mut();
            scene.brushes.push(copy);
            let new_index = scene.brushes.len() - 1;
            new_selection.push(new_index);
            if primary == Some(index) {
                new_primary = Some(new_index);
            }
        }
        self.selected_brush = new_primary.or_else(|| new_selection.last().copied());
        self.selected_brushes = new_selection;
        self.selected_brush_face = None;
        self.mark_dirty();
    }

    /// Numeric UV controls for the selected brush face: offset, rotation,
    /// per-axis scale, reset. Runs inside the Inspector, so history is
    /// owned by the inspector transaction wrapper: a slider drag or a
    /// focused text edit coalesces into one undo step.
    /// The Texture Coordinates heading and the whole-brush texture lock.
    ///
    /// Texture lock is a workspace preference, not brush data, but it IS a
    /// texture-mapping control, so it belongs with the rest of them. It
    /// draws for any selected BSP brush in Select and Brush mode alike; it
    /// used to hide in the Brush-only toolbar, where the workflow that
    /// needs it could not find it.
    pub(crate) fn draw_brush_texture_mapping_header(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("Texture Coordinates").strong());
        ui.checkbox(&mut self.brush_texture_lock, "Lock texture while moving")
            .on_hover_text(
                "Keep face textures anchored to the brush as it moves. Off leaves the \
                 mapping world-aligned, so the brush slides under its texture.",
            );
    }

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
        let mut reset = false;
        let mut off_u = i32::from(edited.offset_texels[0]);
        let mut off_v = i32::from(edited.offset_texels[1]);
        let mut rot = i32::from(edited.rotation_deg);
        let mut scale_u = i32::from(edited.scale_q8[0]) * 100 / 256;
        let mut scale_v = i32::from(edited.scale_q8[1]) * 100 / 256;
        // Two rows, not one: five DragValues plus a button overflow the
        // Inspector's width, and an overflowing widget is painted outside the
        // panel clip rect where the pointer can never reach it.
        // Whether the pointer or the keyboard is still inside one of the
        // scale/rotation widgets. That is what holds a UV edit transaction
        // open across frames; see `UvEditTransaction`.
        let live = |response: &egui::Response| response.dragged() || response.has_focus();
        let rot_live = ui
            .horizontal(|ui| {
                ui.label("UV offset");
                ui.add(egui::DragValue::new(&mut off_u).speed(1).prefix("U "));
                ui.add(egui::DragValue::new(&mut off_v).speed(1).prefix("V "));
                live(
                    &ui.add(
                        egui::DragValue::new(&mut rot)
                            .speed(1)
                            .range(-359..=359)
                            .suffix("\u{b0}"),
                    ),
                )
            })
            .inner;
        let scale_live = ui.horizontal(|ui| {
            ui.label("UV scale");
            let mut scale_live = live(
                &ui.add(
                    egui::DragValue::new(&mut scale_u)
                        .speed(1)
                        .range(10..=1600)
                        .suffix("% U"),
                ),
            );
            scale_live |= live(
                &ui.add(
                    egui::DragValue::new(&mut scale_v)
                        .speed(1)
                        .range(10..=1600)
                        .suffix("% V"),
                ),
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
            // Uniquely labelled: the Inspector shows several bare "Reset"
            // buttons, and this one only ever restores the face UV mapping.
            // Folded in after the DragValues so a Reset wins over them.
            if ui.button("Reset UV").clicked() {
                edited = psxed_project::brush::FaceUv::default();
                reset = true;
            }
            scale_live
        });
        let interacting = rot_live || scale_live.inner;
        let edited = self.apply_face_uv_edit(index, face, current, edited, reset, interacting);
        if edited != current {
            self.project.active_scene_mut().brushes[index].faces[face].uv = edited;
            self.mark_dirty();
        }
    }

    /// Resolve one frame of face UV editing against the in-flight
    /// [`UvEditTransaction`], and return the mapping to store.
    ///
    /// Split out of the widget code so the multi-frame behaviour can be
    /// driven directly: an interaction is a sequence of these calls with
    /// `interacting` true, and holding the transaction is the whole reason
    /// a hundred one-percent steps do not walk the texture off the face.
    pub(crate) fn apply_face_uv_edit(
        &mut self,
        index: usize,
        face: usize,
        current: psxed_project::brush::FaceUv,
        mut edited: psxed_project::brush::FaceUv,
        reset: bool,
        interacting: bool,
    ) -> psxed_project::brush::FaceUv {
        let shaped =
            edited.scale_q8 != current.scale_q8 || edited.rotation_deg != current.rotation_deg;
        let slid = edited.offset_texels != current.offset_texels;
        // Reset UV means "identity mapping", so it is deliberately not
        // re-anchored, and a deliberate slide re-bases the next shaping
        // interaction on the mapping the slide produced.
        if reset || (slid && !shaped) {
            self.brush_uv_edit = None;
            return edited;
        }
        if !shaped {
            // Release and focus loss normally arrive as exactly this: a
            // frame where nothing changed. Ending the interaction only on a
            // CHANGED frame left the captured target alive across the
            // release, so the next edit on that face solved against a
            // mapping the user had already stopped editing.
            if !interacting {
                self.brush_uv_edit = None;
            }
            return edited;
        }
        let stale = self
            .brush_uv_edit
            .is_some_and(|held| held.brush != index || held.face != face);
        if stale {
            self.brush_uv_edit = None;
        }
        if self.brush_uv_edit.is_none() {
            let Some(anchor) = self
                .project
                .active_scene()
                .brushes
                .get(index)
                .and_then(|brush| brush.face_uv_anchor(face))
            else {
                return edited;
            };
            self.brush_uv_edit = Some(crate::UvEditTransaction {
                brush: index,
                face,
                anchor,
                origin: current,
                target: current.apply(anchor),
            });
        }
        let held = self.brush_uv_edit.expect("just seeded");
        // The target is the phase the interaction STARTED at, never the
        // previous frame's rounded mapping, so the i16 rounding is paid once
        // instead of once per frame.
        let slide = [
            f64::from(edited.offset_texels[0]) - f64::from(current.offset_texels[0]),
            f64::from(edited.offset_texels[1]) - f64::from(current.offset_texels[1]),
        ];
        edited.reanchor_to(held.target, held.anchor, slide);
        // A single edit that is not part of a live interaction is complete
        // the moment it is applied.
        if !interacting {
            self.brush_uv_edit = None;
        }
        edited
    }

    /// Drop any in-flight UV edit interaction. Selection changes, undo,
    /// redo and project loads all invalidate the mapping it captured.
    pub(crate) fn clear_uv_edit_transaction(&mut self) {
        self.brush_uv_edit = None;
    }

    /// Solved axis-aligned min corner of the selected brush, rounded to
    /// integers: the numeric inspector's "Origin".
    pub(crate) fn selected_brush_origin(&self) -> Option<[i32; 3]> {
        let index = self.selected_brush?;
        let brush = self.project.active_scene().brushes.get(index)?;
        let solved = brush.solve();
        solved
            .is_valid()
            .then(|| solved.min.map(|value| value.round() as i32))
    }

    /// Numeric fallback for whole-brush placement: translate the selected
    /// brush set so the primary brush's solved min corner lands exactly on
    /// `origin`. No grid
    /// snapping: typing exact off-grid coordinates is the point of the
    /// fallback. Inspector-owned mutation: history is recorded by the
    /// inspector transaction wrapper, not here.
    pub(crate) fn set_selected_brush_origin(&mut self, origin: [i32; 3]) -> bool {
        let Some(current) = self.selected_brush_origin() else {
            return false;
        };
        let Some(primary) = self.selected_brush else {
            return false;
        };
        let delta = [
            origin[0] - current[0],
            origin[1] - current[1],
            origin[2] - current[2],
        ];
        if delta == [0; 3] {
            return true;
        }
        let mut targets = self.selected_brush_set();
        if !targets.contains(&primary) {
            targets = vec![primary];
        }
        let texture_lock = self.brush_texture_lock;
        for index in targets {
            let Some(brush) = self.project.active_scene_mut().brushes.get_mut(index) else {
                return false;
            };
            if texture_lock {
                brush.translate_with_uv_lock(delta, psxed_project::brush::BRUSH_UV_UNITS_PER_TEXEL);
            } else {
                brush.translate(delta);
            }
        }
        self.mark_dirty();
        true
    }

    /// Solved axis-aligned bounding-box size shown by the numeric inspector.
    pub(crate) fn selected_brush_size(&self) -> Option<[i32; 3]> {
        let index = self.selected_brush?;
        let solved = self.project.active_scene().brushes.get(index)?.solve();
        solved.is_valid().then(|| {
            std::array::from_fn(|axis| {
                (solved.max[axis] - solved.min[axis]).round().max(1.0) as i32
            })
        })
    }

    /// Exact numeric size fallback for ordinary axis-aligned brushes. The
    /// negative faces keep the current Origin while the three positive faces
    /// move to the requested extents. Sloped brushes stay editable through
    /// their visible Face/Edge/Vertex handles instead of being distorted by a
    /// bounding-box scale.
    pub(crate) fn set_selected_brush_size(&mut self, size: [i32; 3]) -> bool {
        if size.into_iter().any(|value| value <= 0) {
            return false;
        }
        let Some(current) = self.selected_brush_size() else {
            return false;
        };
        if size == current {
            return true;
        }
        let Some(index) = self.selected_brush else {
            return false;
        };
        let Some(mut edited) = self.project.active_scene().brushes.get(index).cloned() else {
            return false;
        };
        let mut positive_faces = [None; 3];
        for (face_index, face) in edited.faces.iter().enumerate() {
            let Some(plane) = psxed_project::brush::Plane::from_points(face.points) else {
                return false;
            };
            let non_zero = plane.normal.iter().filter(|value| **value != 0).count();
            if non_zero != 1 {
                return false;
            }
            let axis = dominant_axis(plane.normal);
            if plane.normal[axis] > 0 {
                if positive_faces[axis].replace(face_index).is_some() {
                    return false;
                }
            }
        }
        if positive_faces.iter().any(Option::is_none) {
            return false;
        }
        for axis in 0..3 {
            let mut delta = [0; 3];
            delta[axis] = size[axis] - current[axis];
            edited.translate_face(positive_faces[axis].unwrap(), delta);
        }
        if !edited.solve().is_valid() {
            return false;
        }
        self.project.active_scene_mut().brushes[index] = edited;
        self.mark_dirty();
        true
    }

    /// Dominant-axis descriptor of the selected face's plane: the axis
    /// index and the authored reference coordinate (`points[0]` on that
    /// axis). Exact position for axis-aligned faces; a reference point
    /// for slopes.
    pub(crate) fn selected_brush_face_axis(&self) -> Option<(usize, i32)> {
        let (index, face) = (self.selected_brush?, self.selected_brush_face?);
        let face = self
            .project
            .active_scene()
            .brushes
            .get(index)?
            .faces
            .get(face)?;
        let plane = psxed_project::brush::Plane::from_points(face.points)?;
        Some((
            dominant_axis(plane.normal),
            face.points[0][dominant_axis(plane.normal)],
        ))
    }

    /// Numeric fallback for face manipulation: slide the selected face's
    /// plane so its reference point sits at `value` on its dominant axis.
    /// Returns `false` (leaving the brush untouched) when the edit would
    /// stop the brush enclosing volume. Inspector-owned mutation: no
    /// history record here (see `set_selected_brush_origin`).
    pub(crate) fn set_selected_brush_face_axis_position(&mut self, value: i32) -> bool {
        let Some((axis, current)) = self.selected_brush_face_axis() else {
            return false;
        };
        let (Some(index), Some(face)) = (self.selected_brush, self.selected_brush_face) else {
            return false;
        };
        if value == current {
            return true;
        }
        let mut delta = [0i32; 3];
        delta[axis] = value - current;
        let Some(mut edited) = self.project.active_scene().brushes.get(index).cloned() else {
            return false;
        };
        edited.translate_face(face, delta);
        if !edited.solve().is_valid() {
            return false;
        }
        self.project.active_scene_mut().brushes[index] = edited;
        self.mark_dirty();
        true
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
        self.mark_dirty();
    }

    /// The cuboid a brush drag would commit, if it has area.
    fn brush_drag_cuboid(drag: BrushDrag) -> Option<psxed_project::brush::Brush> {
        let mut opposite = drag.current;
        let depth_axis = drag.view.depth_axis();
        opposite[depth_axis] = drag.anchor[depth_axis].saturating_add(BRUSH_CREATE_HEIGHT);
        psxed_project::brush::Brush::cuboid_from_corners(drag.anchor, opposite)
    }

    /// Snap a point from the active orthographic plane to the brush grid.
    /// Its hidden-axis coordinate comes from the world-space shared focus.
    pub(crate) fn brush_snap_2d(&self, world: [f32; 2]) -> [i32; 3] {
        let step = (self.snap_units.max(1)) as f32;
        let snap = |v: f32| ((v / step).round() * step) as i32;
        self.orthographic_view
            .unproject(world, self.orthographic_focus)
            .map(snap)
    }

    /// Start a brush-create drag at a snapped point (2D view entry).
    pub(crate) fn begin_brush_drag_2d(&mut self, world: [f32; 2]) {
        let point = self.brush_snap_2d(world);
        self.brush_drag = Some(BrushDrag {
            anchor: point,
            current: point,
            view: self.orthographic_view,
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
        let Some(mut brush) = Self::brush_drag_cuboid(drag) else {
            return; // zero-area drag: nothing to commit
        };
        // New brushes should be cookable and visibly textured immediately.
        // This follows the same material resolution as the grid tools: an
        // explicit picker/selected resource wins, then the first project
        // material. Hollow preserves the face materials on all six slabs.
        if let Some(material) = self.paint_material_for("brush") {
            for face in &mut brush.faces {
                face.material = Some(material);
            }
        }
        self.push_undo();
        let scene = self.project.active_scene_mut();
        scene.brushes.push(brush);
        let created = self.project.active_scene().brushes.len() - 1;
        self.replace_brush_selection(created, None);
        self.mark_dirty();
    }

    /// One clip click at a snapped ground point: the first click stores
    /// the point, the second splits the selected brush by the vertical
    /// plane through both, honouring `brush_clip_keep`. Shared by the 3D
    /// tool and the 2D view.
    pub(crate) fn brush_clip_click(&mut self, point: [i32; 3]) {
        self.brush_clip_click_in_view(point, self.orthographic_view);
    }

    fn brush_clip_click_in_view(&mut self, point: [i32; 3], view: OrthographicView) {
        let Some(selected) = self.selected_brush else {
            return;
        };
        match self.brush_clip_start.take() {
            None => self.brush_clip_start = Some(point),
            Some(start) if start == point => {}
            Some(start) => {
                let mut up = start;
                let depth_axis = view.depth_axis();
                up[depth_axis] = up[depth_axis].saturating_add(BRUSH_CREATE_HEIGHT);
                // `.get` rather than indexing: the selection can go stale
                // across undo/redo before the second clip click lands.
                let Some(brush) = self.project.active_scene().brushes.get(selected) else {
                    return;
                };
                let clipped = brush.clip([start, point, up]);
                match (self.brush_clip_keep, clipped.back, clipped.front) {
                    (BrushClipKeep::Both, Some(back), Some(front)) => {
                        self.push_undo();
                        let scene = self.project.active_scene_mut();
                        scene.brushes[selected] = back;
                        scene.brushes.push(front);
                        self.mark_dirty();
                    }
                    (BrushClipKeep::Back, Some(back), Some(_)) => {
                        self.push_undo();
                        self.project.active_scene_mut().brushes[selected] = back;
                        self.mark_dirty();
                    }
                    (BrushClipKeep::Front, Some(_), Some(front)) => {
                        self.push_undo();
                        self.project.active_scene_mut().brushes[selected] = front;
                        self.mark_dirty();
                    }
                    // Plane missed the brush: nothing to keep or drop.
                    _ => {}
                }
            }
        }
    }

    /// Select the visible brush face under the active orthographic point.
    /// Smaller projected brushes retain the old Top-view priority; exact
    /// overlaps then prefer the face nearest the positive-axis viewer.
    #[cfg(test)]
    pub(crate) fn select_brush_at_2d(&mut self, world: [f32; 2]) -> bool {
        match self.pick_brush_face_at_2d(world) {
            Some((index, face)) => {
                self.replace_brush_selection(index, Some(face));
                true
            }
            None => {
                self.clear_brush_selection();
                false
            }
        }
    }

    pub(crate) fn pick_brush_face_at_2d(&self, world: [f32; 2]) -> Option<(usize, usize)> {
        self.brush_face_hits_at_2d(world, 0.0).into_iter().next()
    }

    /// Ordered orthographic brush hits with optional world-space edge
    /// forgiveness. Callers derive that tolerance from a fixed pixel radius,
    /// so a brush remains selectable when zoomed down to a sub-pixel outline.
    fn brush_face_hits_at_2d(&self, world: [f32; 2], tolerance: f32) -> Vec<(usize, usize)> {
        let view = self.orthographic_view;
        let [horizontal, vertical] = view.plane_axes();
        let depth_axis = view.depth_axis();
        let point = world.map(f64::from);
        let tolerance = f64::from(tolerance.max(0.0));
        let tolerance2 = tolerance * tolerance;
        let mut hits = Vec::<(f64, f64, usize, usize)>::new();

        for (index, brush) in self.project.active_scene().brushes.iter().enumerate() {
            let solved = brush.solve();
            if !solved.is_valid() {
                continue;
            }
            let min = view.project_f64(solved.min);
            let max = view.project_f64(solved.max);
            if point[0] < min[0] - tolerance
                || point[0] > max[0] + tolerance
                || point[1] < min[1] - tolerance
                || point[1] > max[1] + tolerance
            {
                continue;
            }
            let area = (max[0] - min[0]) * (max[1] - min[1]);
            let mut nearest_face: Option<(f64, usize)> = None;
            for (face, polygon) in solved.polygons.iter().enumerate() {
                let Some(polygon) = polygon else { continue };
                let projected = polygon
                    .verts
                    .iter()
                    .copied()
                    .map(|vertex| view.project_f64(vertex))
                    .collect::<Vec<_>>();
                if !point_in_convex_polygon_2d(point, &projected)
                    && polygon_edge_distance2(point, &projected) > tolerance2
                {
                    continue;
                }
                let Some(plane) =
                    psxed_project::brush::Plane::from_points(brush.faces[face].points)
                else {
                    continue;
                };
                let denominator = plane.normal[depth_axis] as f64;
                if denominator.abs() < f64::EPSILON {
                    continue;
                }
                let depth = (plane.dist as f64
                    - plane.normal[horizontal] as f64 * point[0]
                    - plane.normal[vertical] as f64 * point[1])
                    / denominator;
                if nearest_face.is_none_or(|(best_depth, best_face)| {
                    depth > best_depth || (depth == best_depth && face < best_face)
                }) {
                    nearest_face = Some((depth, face));
                }
            }
            let Some((depth, face)) = nearest_face else {
                continue;
            };
            hits.push((area, depth, index, face));
        }
        hits.sort_by(|a, b| {
            a.0.total_cmp(&b.0)
                .then_with(|| b.1.total_cmp(&a.1))
                .then_with(|| a.2.cmp(&b.2))
                .then_with(|| a.3.cmp(&b.3))
        });
        hits.into_iter()
            .map(|(_, _, index, face)| (index, face))
            .collect()
    }

    fn brush_pick_tolerance_2d(&self) -> f32 {
        BRUSH_SCREEN_PICK_RADIUS / self.viewport_zoom.max(MIN_VIEWPORT_ZOOM)
    }

    /// Click selection cycles through coincident projected brushes. This keeps
    /// every member of an overlap reachable without hiding or moving the one
    /// currently in front.
    pub(crate) fn pick_brush_face_for_selection_at_2d(
        &self,
        world: [f32; 2],
    ) -> Option<(usize, usize)> {
        let hits = self.brush_face_hits_at_2d(world, self.brush_pick_tolerance_2d());
        let next = self
            .selected_brush
            .and_then(|selected| hits.iter().position(|(index, _)| *index == selected))
            .map_or(0, |position| (position + 1) % hits.len().max(1));
        hits.get(next).copied()
    }

    pub(crate) fn pick_brush_face_for_move_at_2d(&self, world: [f32; 2]) -> Option<(usize, usize)> {
        let hits = self.brush_face_hits_at_2d(world, self.brush_pick_tolerance_2d());
        self.selected_brush
            .and_then(|selected| hits.iter().find(|(index, _)| *index == selected).copied())
            .or_else(|| hits.first().copied())
    }

    /// Other members of the multi-selection that should ride along with
    /// a whole-brush move grabbed on `index`.
    fn brush_move_others(&self, index: usize) -> Vec<(usize, psxed_project::brush::Brush)> {
        if !self.brush_is_selected(index) {
            return Vec::new();
        }
        self.selected_brush_set()
            .into_iter()
            .filter(|other| *other != index)
            .map(|other| (other, self.project.active_scene().brushes[other].clone()))
            .collect()
    }

    /// Shift-drag entry for an in-plane whole-brush move. Grabbing a
    /// brush inside the multi-selection moves the whole selection;
    /// grabbing an unselected brush selects and moves it alone.
    pub(crate) fn begin_brush_move_2d(&mut self, world: [f32; 2]) -> bool {
        let Some((index, face)) = self.pick_brush_face_for_move_at_2d(world) else {
            return false;
        };
        let others = self.brush_move_others(index);
        let base = self.project.active_scene().brushes[index].clone();
        if others.is_empty() {
            self.replace_brush_selection(index, Some(face));
        } else {
            self.selected_brush = Some(index);
            self.selected_brush_face = Some(face);
        }
        self.brush_move = Some(BrushMove {
            index,
            base,
            others,
            press_ground: self
                .orthographic_view
                .unproject(world, self.orthographic_focus),
            applied: [0; 3],
        });
        true
    }

    pub(crate) fn update_brush_move_2d(&mut self, world: [f32; 2]) {
        let Some(mv) = self.brush_move.clone() else {
            return;
        };
        let current = self
            .orthographic_view
            .unproject(world, self.orthographic_focus);
        let step = self.snap_units.max(1) as f32;
        let snap = |value: f32| ((value / step).round() * step) as i32;
        let mut applied = [0; 3];
        for axis in self.orthographic_view.plane_axes() {
            applied[axis] = snap(current[axis] - mv.press_ground[axis]);
        }
        if applied == mv.applied {
            return;
        }
        self.apply_brush_move_preview(&mv, applied);
    }

    /// Write the move preview for the grabbed brush and every rider.
    ///
    /// The preview has to take the SAME path the commit takes, or the drag
    /// shows one mapping and releasing the mouse writes another. Every
    /// preview is rebuilt from its own untouched base, so the lock
    /// compensation is applied once against the total delta rather than
    /// accumulating per mouse move.
    fn apply_brush_move_preview(&mut self, mv: &BrushMove, applied: [i32; 3]) {
        let texture_lock = self.brush_texture_lock;
        let preview_of = |base: &psxed_project::brush::Brush| {
            let mut preview = base.clone();
            if texture_lock {
                preview.translate_with_uv_lock(
                    applied,
                    psxed_project::brush::BRUSH_UV_UNITS_PER_TEXEL,
                );
            } else {
                preview.translate(applied);
            }
            preview
        };
        let primary = preview_of(&mv.base);
        self.project.active_scene_mut().brushes[mv.index] = primary;
        for (index, base) in &mv.others {
            let rider = preview_of(base);
            self.project.active_scene_mut().brushes[*index] = rider;
        }
        if let Some(state) = self.brush_move.as_mut() {
            state.applied = applied;
        }
    }

    /// Start resizing the axis-facing brush face nearest a projected edge.
    /// Faces perpendicular to the current view are deliberately excluded:
    /// their depth motion is ambiguous in a 2D panel.
    pub(crate) fn begin_brush_resize_2d(&mut self, world: [f32; 2], tolerance: f32) -> bool {
        let view = self.orthographic_view;
        let point = world.map(f64::from);
        let tolerance2 = f64::from(tolerance.max(0.0)).powi(2);
        let mut best: Option<(f64, bool, usize, usize, usize, i32)> = None;

        for (index, brush) in self.project.active_scene().brushes.iter().enumerate() {
            let solved = brush.solve();
            for (face, polygon) in solved.polygons.iter().enumerate() {
                let Some(polygon) = polygon else { continue };
                let Some(plane) =
                    psxed_project::brush::Plane::from_points(brush.faces[face].points)
                else {
                    continue;
                };
                let axis = dominant_axis(plane.normal);
                if axis == view.depth_axis() {
                    continue;
                }
                let projected = polygon
                    .verts
                    .iter()
                    .copied()
                    .map(|vertex| view.project_f64(vertex))
                    .collect::<Vec<_>>();
                let distance2 = polygon_edge_distance2(point, &projected);
                if distance2 > tolerance2 {
                    continue;
                }
                let selected = self.selected_brush == Some(index);
                let dir = if plane.normal[axis] >= 0 { 1 } else { -1 };
                if best.is_none_or(
                    |(best_distance, best_selected, best_index, best_face, _, _)| {
                        distance2 < best_distance
                            || (distance2 == best_distance
                                && (selected && !best_selected
                                    || (selected == best_selected
                                        && (index < best_index
                                            || (index == best_index && face < best_face)))))
                    },
                ) {
                    best = Some((distance2, selected, index, face, axis, dir));
                }
            }
        }

        let Some((_, _, index, face, axis, dir)) = best else {
            return false;
        };
        let base = self.project.active_scene().brushes[index].clone();
        self.replace_brush_selection(index, Some(face));
        self.brush_extrude = Some(BrushExtrude {
            index,
            face,
            base,
            axis,
            dir,
            press_y: 0.0,
            press_ground: view.unproject(world, self.orthographic_focus),
            normal_3d: None,
            screen_direction: egui::Vec2::ZERO,
            units_per_pixel: 0.0,
            applied: [0; 3],
        });
        true
    }

    pub(crate) fn update_brush_resize_2d(&mut self, world: [f32; 2]) {
        let Some(extrude) = self.brush_extrude.clone() else {
            return;
        };
        let current = self
            .orthographic_view
            .unproject(world, self.orthographic_focus);
        let step = self.snap_units.max(1) as f32;
        let raw = current[extrude.axis] - extrude.press_ground[extrude.axis];
        let snapped = ((raw / step).round() * step) as i32;
        let mut delta = [0; 3];
        delta[extrude.axis] = snapped;
        if delta == extrude.applied {
            return;
        }
        let mut preview = extrude.base.clone();
        preview.translate_face(extrude.face, delta);
        if preview.solve().is_valid() {
            self.project.active_scene_mut().brushes[extrude.index] = preview;
            if let Some(state) = self.brush_extrude.as_mut() {
                state.applied = delta;
            }
        }
    }

    /// Unique solved vertices of the selected brush, world f64 (welded
    /// within half a unit; authored geometry is integer).
    fn selected_brush_solved_verts(&self) -> Option<(usize, Vec<[f64; 3]>)> {
        let index = self.selected_brush?;
        let brush = self.project.active_scene().brushes.get(index)?;
        let solved = brush.solve();
        if !solved.is_valid() {
            return None;
        }
        Some((index, brush_elements::unique_vertices(&solved)))
    }

    /// Every solved vertex whose projection sits on `projected` (the
    /// whole depth column behind one on-screen corner).
    fn brush_vertex_column(
        view: OrthographicView,
        verts: &[[f64; 3]],
        projected: [f64; 2],
    ) -> Vec<[f64; 3]> {
        verts
            .iter()
            .copied()
            .filter(|vert| {
                let p = view.project_f64(*vert);
                (p[0] - projected[0]).abs() <= 0.5 && (p[1] - projected[1]).abs() <= 0.5
            })
            .collect()
    }

    /// Vertex mode entry: grab the selected brush's projected corner
    /// nearest `world` (within `tolerance` world units) and start
    /// dragging its depth column. `false` when nothing is close enough.
    pub(crate) fn begin_brush_vertex_drag_2d(&mut self, world: [f32; 2], tolerance: f32) -> bool {
        let Some((index, verts)) = self.selected_brush_solved_verts() else {
            return false;
        };
        let view = self.orthographic_view;
        let point = world.map(f64::from);
        let tolerance2 = f64::from(tolerance.max(0.0)).powi(2);
        let mut best: Option<(f64, [f64; 2])> = None;
        for vert in &verts {
            let projected = view.project_f64(*vert);
            let d2 = (projected[0] - point[0]).powi(2) + (projected[1] - point[1]).powi(2);
            if d2 <= tolerance2 && best.is_none_or(|(best_d2, _)| d2 < best_d2) {
                best = Some((d2, projected));
            }
        }
        let Some((_, projected)) = best else {
            return false;
        };
        let targets = Self::brush_vertex_column(view, &verts, projected);
        self.start_brush_vertex_drag(index, targets, world);
        true
    }

    /// Edge mode entry: grab the selected brush's projected edge nearest
    /// `world` and drag both endpoint columns together.
    pub(crate) fn begin_brush_edge_drag_2d(&mut self, world: [f32; 2], tolerance: f32) -> bool {
        let Some((index, verts)) = self.selected_brush_solved_verts() else {
            return false;
        };
        let view = self.orthographic_view;
        let point = world.map(f64::from);
        let tolerance2 = f64::from(tolerance.max(0.0)).powi(2);
        let solved = self.project.active_scene().brushes[index].solve();
        let mut best: Option<(f64, [f64; 2], [f64; 2])> = None;
        for polygon in solved.polygons.iter().flatten() {
            let count = polygon.verts.len();
            for i in 0..count {
                let a = view.project_f64(polygon.verts[i]);
                let b = view.project_f64(polygon.verts[(i + 1) % count]);
                let d2 = point_segment_distance2(point, a, b);
                if d2 <= tolerance2 && best.is_none_or(|(best_d2, _, _)| d2 < best_d2) {
                    best = Some((d2, a, b));
                }
            }
        }
        let Some((_, a, b)) = best else {
            return false;
        };
        let mut targets = Self::brush_vertex_column(view, &verts, a);
        for vert in Self::brush_vertex_column(view, &verts, b) {
            if !targets
                .iter()
                .any(|seen| (0..3).all(|axis| (seen[axis] - vert[axis]).abs() <= 0.5))
            {
                targets.push(vert);
            }
        }
        self.start_brush_vertex_drag(index, targets, world);
        true
    }

    fn start_brush_vertex_drag(&mut self, index: usize, targets: Vec<[f64; 3]>, world: [f32; 2]) {
        let base = self.project.active_scene().brushes[index].clone();
        self.selected_brush = Some(index);
        self.brush_vertex_drag = Some(BrushVertexDrag {
            index,
            base,
            targets,
            press_ground: self
                .orthographic_view
                .unproject(world, self.orthographic_focus),
            plane_3d: None,
            applied: [0; 3],
        });
    }

    /// Advance the vertex/edge drag: snapped in-plane delta applied to
    /// every grabbed vertex's authored points. Previews that stop the
    /// brush enclosing volume are refused (the last valid preview holds).
    pub(crate) fn update_brush_vertex_drag_2d(&mut self, world: [f32; 2]) {
        let Some(drag) = self.brush_vertex_drag.clone() else {
            return;
        };
        let current = self
            .orthographic_view
            .unproject(world, self.orthographic_focus);
        let step = self.snap_units.max(1) as f32;
        let snap = |value: f32| ((value / step).round() * step) as i32;
        let mut applied = [0; 3];
        for axis in self.orthographic_view.plane_axes() {
            applied[axis] = snap(current[axis] - drag.press_ground[axis]);
        }
        if applied == drag.applied {
            return;
        }
        let mut preview = drag.base.clone();
        let moved = preview.translate_points_near(&drag.targets, applied, 0.5);
        if moved > 0 && preview.solve().is_valid() {
            self.project.active_scene_mut().brushes[drag.index] = preview;
            if let Some(state) = self.brush_vertex_drag.as_mut() {
                state.applied = applied;
            }
        }
    }

    fn commit_brush_vertex_drag_preview(&mut self) -> bool {
        let Some(drag) = self.brush_vertex_drag.take() else {
            return false;
        };
        let live = self.project.active_scene().brushes[drag.index].clone();
        self.project.active_scene_mut().brushes[drag.index] = drag.base;
        if drag.applied != [0; 3] {
            self.push_undo();
            self.project.active_scene_mut().brushes[drag.index] = live;
            self.mark_dirty();
        }
        true
    }

    fn commit_brush_move_preview(&mut self) -> bool {
        let Some(mv) = self.brush_move.take() else {
            return false;
        };
        self.project.active_scene_mut().brushes[mv.index] = mv.base.clone();
        for (index, base) in &mv.others {
            self.project.active_scene_mut().brushes[*index] = base.clone();
        }
        if mv.applied != [0; 3] {
            self.push_undo();
            let texture_lock = self.brush_texture_lock;
            let commit_one = |ws: &mut Self, index: usize, base: psxed_project::brush::Brush| {
                let mut moved = base;
                if texture_lock {
                    moved.translate_with_uv_lock(
                        mv.applied,
                        psxed_project::brush::BRUSH_UV_UNITS_PER_TEXEL,
                    );
                } else {
                    moved.translate(mv.applied);
                }
                ws.project.active_scene_mut().brushes[index] = moved;
            };
            commit_one(self, mv.index, mv.base.clone());
            for (index, base) in &mv.others {
                commit_one(self, *index, base.clone());
            }
            self.mark_dirty();
        }
        true
    }

    fn commit_brush_extrude_preview(&mut self) -> bool {
        let Some(extrude) = self.brush_extrude.take() else {
            return false;
        };
        let live = self.project.active_scene().brushes[extrude.index].clone();
        self.project.active_scene_mut().brushes[extrude.index] = extrude.base;
        if extrude.applied != [0; 3] {
            self.push_undo();
            self.project.active_scene_mut().brushes[extrude.index] = live;
            self.mark_dirty();
        }
        true
    }

    pub(crate) fn commit_brush_gesture_2d(&mut self) {
        if self.commit_brush_move_preview()
            || self.commit_brush_vertex_drag_preview()
            || self.commit_brush_extrude_preview()
        {
            return;
        }
        self.commit_brush_drag();
    }

    /// Project exact solved brush polygons into the active orthographic
    /// plane, including the selected face and transient create/clip state.
    pub(crate) fn draw_brush_footprints_2d(
        &self,
        painter: &egui::Painter,
        transform: crate::viewport2d::ViewportTransform,
    ) {
        let view = self.orthographic_view;
        let draw = |brush: &psxed_project::brush::Brush,
                    selected_face: Option<usize>,
                    stroke: egui::Stroke| {
            let solved = brush.solve();
            if !solved.is_valid() {
                return;
            }
            for (face, polygon) in solved.polygons.iter().enumerate() {
                let Some(polygon) = polygon else { continue };
                let points = polygon
                    .verts
                    .iter()
                    .copied()
                    .map(|world| {
                        let projected = view.project_f64(world);
                        transform.world_to_screen([projected[0] as f32, projected[1] as f32])
                    })
                    .collect::<Vec<_>>();
                if points.len() < 2 {
                    continue;
                }
                if selected_face == Some(face) && projected_polygon_area(&points).abs() > 0.5 {
                    painter.add(egui::Shape::convex_polygon(
                        points.clone(),
                        Color32::from_rgba_unmultiplied(
                            STUDIO_ACCENT.r(),
                            STUDIO_ACCENT.g(),
                            STUDIO_ACCENT.b(),
                            28,
                        ),
                        egui::Stroke::NONE,
                    ));
                }
                let face_stroke = if selected_face == Some(face) {
                    egui::Stroke::new(3.0, STUDIO_ACCENT)
                } else {
                    stroke
                };
                for edge in 0..points.len() {
                    painter.line_segment(
                        [points[edge], points[(edge + 1) % points.len()]],
                        face_stroke,
                    );
                }
            }
        };
        for (index, brush) in self.project.active_scene().brushes.iter().enumerate() {
            let selected = self.brush_is_selected(index);
            let stroke = if selected {
                egui::Stroke::new(2.0, STUDIO_ACCENT)
            } else {
                egui::Stroke::new(1.0, brush_contents_outline(brush.contents))
            };
            // The face highlight follows only the primary selection.
            draw(
                brush,
                (self.selected_brush == Some(index))
                    .then_some(self.selected_brush_face)
                    .flatten(),
                stroke,
            );
        }
        if let Some(preview) = self.brush_drag.and_then(Self::brush_drag_cuboid) {
            draw(&preview, None, egui::Stroke::new(1.5, STUDIO_ACCENT));
        }
        if let Some(start) = self.brush_clip_start {
            let projected = view.project_f32(start.map(|value| value as f32));
            let center = transform.world_to_screen(projected);
            painter.circle_stroke(center, 4.0, egui::Stroke::new(1.5, STUDIO_ACCENT));
        }
        let brush_edit_visible = matches!(self.active_tool, ViewTool::Brush | ViewTool::Select);
        if brush_edit_visible {
            if let Some(index) = self.selected_brush {
                if let Some(brush) = self.project.active_scene().brushes.get(index) {
                    let solved = brush.solve();
                    let to_screen = |world: [f64; 3]| {
                        let projected = view.project_f64(world);
                        transform.world_to_screen([projected[0] as f32, projected[1] as f32])
                    };
                    match self.brush_edit_mode {
                        BrushEditMode::Move => {
                            let center = std::array::from_fn(|axis| {
                                (solved.min[axis] + solved.max[axis]) * 0.5
                            });
                            let center = to_screen(center);
                            painter.circle_filled(center, 5.0, STUDIO_ACCENT);
                            painter.circle_stroke(
                                center,
                                8.0,
                                egui::Stroke::new(1.5, STUDIO_ACCENT),
                            );
                        }
                        BrushEditMode::Face => {
                            let mut seen = Vec::<Pos2>::new();
                            for (face_index, polygon) in solved.polygons.iter().enumerate() {
                                let Some(polygon) = polygon else { continue };
                                let Some(plane) = psxed_project::brush::Plane::from_points(
                                    brush.faces[face_index].points,
                                ) else {
                                    continue;
                                };
                                if dominant_axis(plane.normal) == view.depth_axis() {
                                    continue;
                                }
                                let count = polygon.verts.len() as f64;
                                let center = std::array::from_fn(|axis| {
                                    polygon.verts.iter().map(|point| point[axis]).sum::<f64>()
                                        / count
                                });
                                let center = to_screen(center);
                                if seen.iter().any(|point| point.distance(center) <= 1.0) {
                                    continue;
                                }
                                seen.push(center);
                                painter.rect_filled(
                                    Rect::from_center_size(center, Vec2::splat(7.0)),
                                    1.0,
                                    STUDIO_ACCENT,
                                );
                            }
                        }
                        BrushEditMode::Edge => {
                            // Screen-space dedup on top of the canonical
                            // enumeration: distinct 3D edges of one depth
                            // column project onto the same 2D segment.
                            let mut seen = Vec::<Pos2>::new();
                            for (a, b) in brush_elements::unique_edges(&solved) {
                                let center = to_screen(std::array::from_fn(|axis| {
                                    (a[axis] + b[axis]) * 0.5
                                }));
                                if seen.iter().any(|point| point.distance(center) <= 1.0) {
                                    continue;
                                }
                                seen.push(center);
                                painter.rect_stroke(
                                    Rect::from_center_size(center, Vec2::splat(7.0)),
                                    0.0,
                                    egui::Stroke::new(1.5, STUDIO_ACCENT),
                                    egui::StrokeKind::Inside,
                                );
                            }
                        }
                        BrushEditMode::Vertex => {
                            if let Some((_, verts)) = self.selected_brush_solved_verts() {
                                let mut seen: Vec<[f64; 2]> = Vec::new();
                                for vert in verts {
                                    let projected = view.project_f64(vert);
                                    if seen.iter().any(|point| {
                                        (point[0] - projected[0]).abs() <= 0.5
                                            && (point[1] - projected[1]).abs() <= 0.5
                                    }) {
                                        continue;
                                    }
                                    seen.push(projected);
                                    painter.rect_filled(
                                        Rect::from_center_size(to_screen(vert), Vec2::splat(7.0)),
                                        1.0,
                                        STUDIO_ACCENT,
                                    );
                                }
                            }
                        }
                    }
                }
            }
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
        let draw = |brush: &psxed_project::brush::Brush, stroke: egui::Stroke| {
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
            let selected = self.brush_is_selected(index);
            // Unselected brushes render as plain shaded geometry; the full
            // wireframe cage is the opt-in "Brush wireframes" View toggle.
            if !selected && !self.show_brush_wireframes {
                continue;
            }
            let stroke = if selected {
                egui::Stroke::new(2.0, STUDIO_ACCENT)
            } else {
                egui::Stroke::new(1.0, brush_contents_outline(brush.contents))
            };
            draw(brush, stroke);
            // Emphasize the selected face's polygon on the primary brush.
            if self.selected_brush == Some(index) {
                if let Some(face) = self.selected_brush_face {
                    if let Some(Some(polygon)) = brush.solve().polygons.get(face) {
                        let count = polygon.verts.len();
                        for i in 0..count {
                            let a = project(polygon.verts[i]);
                            let b = project(polygon.verts[(i + 1) % count]);
                            if let (Some(a), Some(b)) = (a, b) {
                                painter.line_segment([a, b], egui::Stroke::new(3.5, STUDIO_ACCENT));
                            }
                        }
                    }
                }
            }
        }
        if matches!(self.active_tool, ViewTool::Brush | ViewTool::Select) {
            if let Some(index) = self.selected_brush {
                if let Some(brush) = self.project.active_scene().brushes.get(index) {
                    let solved = brush.solve();
                    match self.brush_edit_mode {
                        BrushEditMode::Move => {
                            let center = std::array::from_fn(|axis| {
                                (solved.min[axis] + solved.max[axis]) * 0.5
                            });
                            if let Some(center) = project(center) {
                                painter.circle_filled(center, 5.0, STUDIO_ACCENT);
                                painter.circle_stroke(
                                    center,
                                    8.0,
                                    egui::Stroke::new(1.5, STUDIO_ACCENT),
                                );
                            }
                        }
                        BrushEditMode::Vertex => {
                            for vertex in brush_elements::unique_vertices(&solved) {
                                if let Some(center) = project(vertex) {
                                    painter.rect_filled(
                                        egui::Rect::from_center_size(
                                            center,
                                            egui::Vec2::splat(7.0),
                                        ),
                                        1.0,
                                        STUDIO_ACCENT,
                                    );
                                }
                            }
                        }
                        BrushEditMode::Edge => {
                            for (a, b) in brush_elements::unique_edges(&solved) {
                                let midpoint = [
                                    (a[0] + b[0]) * 0.5,
                                    (a[1] + b[1]) * 0.5,
                                    (a[2] + b[2]) * 0.5,
                                ];
                                if let Some(center) = project(midpoint) {
                                    painter.circle_filled(center, 4.0, STUDIO_ACCENT);
                                }
                            }
                        }
                        BrushEditMode::Face => {
                            for (_, center, normal) in
                                brush_elements::face_handles(brush, &solved)
                            {
                                if !self.brush_face_handle_visible(center, normal) {
                                    continue;
                                }
                                let normal_end = [
                                    center[0] + normal[0] * 48.0,
                                    center[1] + normal[1] * 48.0,
                                    center[2] + normal[2] * 48.0,
                                ];
                                if let (Some(center), Some(normal_end)) =
                                    (project(center), project(normal_end))
                                {
                                    painter.line_segment(
                                        [center, normal_end],
                                        egui::Stroke::new(1.5, STUDIO_ACCENT),
                                    );
                                    painter.circle_filled(center, 4.5, STUDIO_ACCENT);
                                }
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

fn dominant_axis(normal: [i64; 3]) -> usize {
    let mut axis = 0;
    for candidate in 1..3 {
        if normal[candidate].abs() > normal[axis].abs() {
            axis = candidate;
        }
    }
    axis
}

fn point_in_convex_polygon_2d(point: [f64; 2], polygon: &[[f64; 2]]) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    let mut winding_sign = 0i8;
    let mut has_area = false;
    for index in 0..polygon.len() {
        let a = polygon[index];
        let b = polygon[(index + 1) % polygon.len()];
        let cross = (b[0] - a[0]) * (point[1] - a[1]) - (b[1] - a[1]) * (point[0] - a[0]);
        if cross.abs() <= 1.0e-7 {
            continue;
        }
        has_area = true;
        let sign = if cross > 0.0 { 1 } else { -1 };
        if winding_sign != 0 && winding_sign != sign {
            return false;
        }
        winding_sign = sign;
    }
    has_area
}

fn polygon_edge_distance2(point: [f64; 2], polygon: &[[f64; 2]]) -> f64 {
    if polygon.len() < 2 {
        return f64::INFINITY;
    }
    (0..polygon.len())
        .map(|index| {
            point_segment_distance2(point, polygon[index], polygon[(index + 1) % polygon.len()])
        })
        .fold(f64::INFINITY, f64::min)
}

fn point_segment_distance2(point: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let ap = [point[0] - a[0], point[1] - a[1]];
    let length2 = ab[0] * ab[0] + ab[1] * ab[1];
    let t = if length2 <= f64::EPSILON {
        0.0
    } else {
        ((ap[0] * ab[0] + ap[1] * ab[1]) / length2).clamp(0.0, 1.0)
    };
    let closest = [a[0] + ab[0] * t, a[1] + ab[1] * t];
    let delta = [point[0] - closest[0], point[1] - closest[1]];
    delta[0] * delta[0] + delta[1] * delta[1]
}

fn projected_polygon_area(points: &[egui::Pos2]) -> f32 {
    if points.len() < 3 {
        return 0.0;
    }
    let twice_area = (0..points.len())
        .map(|index| {
            let a = points[index];
            let b = points[(index + 1) % points.len()];
            a.x * b.y - b.x * a.y
        })
        .sum::<f32>();
    twice_area * 0.5
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum BrushHandle3d {
    Vertex([f64; 3]),
    Edge([f64; 3], [f64; 3]),
    Face {
        face: usize,
        center: [f64; 3],
        normal: [f64; 3],
    },
}

impl EditorWorkspace {
    pub(crate) fn project_brush_point_3d(
        &self,
        rect: egui::Rect,
        world: [f64; 3],
    ) -> Option<egui::Pos2> {
        self.viewport_3d_camera()
            .normalized_panel_point_for_world(world.map(|value| value as f32))
            .map(|(nx, ny)| {
                egui::Pos2::new(
                    rect.center().x + nx * rect.width() * 0.5,
                    rect.center().y + ny * rect.height() * 0.5,
                )
            })
    }

    pub(crate) fn face_center_and_normal(
        brush: &psxed_project::brush::Brush,
        face: usize,
    ) -> Option<([f64; 3], [f64; 3])> {
        let solved = brush.solve();
        let polygon = solved.polygons.get(face)?.as_ref()?;
        let count = polygon.verts.len() as f64;
        if count <= 0.0 {
            return None;
        }
        let mut center = [0.0; 3];
        for vertex in &polygon.verts {
            for axis in 0..3 {
                center[axis] += vertex[axis] / count;
            }
        }
        let plane = psxed_project::brush::Plane::from_points(brush.faces.get(face)?.points)?;
        let length = ((plane.normal[0] as f64).powi(2)
            + (plane.normal[1] as f64).powi(2)
            + (plane.normal[2] as f64).powi(2))
        .sqrt();
        (length > f64::EPSILON).then(|| {
            (
                center,
                [
                    plane.normal[0] as f64 / length,
                    plane.normal[1] as f64 / length,
                    plane.normal[2] as f64 / length,
                ],
            )
        })
    }

    fn brush_face_handle_visible(&self, center: [f64; 3], normal: [f64; 3]) -> bool {
        let camera = self.viewport_3d_camera().basis().position;
        let to_camera = [
            f64::from(camera[0]) - center[0],
            f64::from(camera[1]) - center[1],
            f64::from(camera[2]) - center[2],
        ];
        to_camera[0] * normal[0] + to_camera[1] * normal[1] + to_camera[2] * normal[2] > 0.0
    }

    pub(crate) fn pick_brush_handle_3d(
        &self,
        rect: egui::Rect,
        pointer: egui::Pos2,
    ) -> Option<(usize, BrushHandle3d)> {
        const HANDLE_RADIUS: f32 = 9.0;
        let index = self.selected_brush?;
        let brush = self.project.active_scene().brushes.get(index)?;
        let solved = brush.solve();
        if !solved.is_valid() {
            return None;
        }
        let mut best: Option<(f32, BrushHandle3d)> = None;
        let mut consider = |screen: egui::Pos2, handle: BrushHandle3d| {
            let distance2 = screen.distance_sq(pointer);
            if distance2 <= HANDLE_RADIUS * HANDLE_RADIUS
                && best.is_none_or(|(best_distance2, _)| distance2 < best_distance2)
            {
                best = Some((distance2, handle));
            }
        };
        match self.brush_edit_mode {
            BrushEditMode::Move => return None,
            BrushEditMode::Vertex => {
                for vertex in brush_elements::unique_vertices(&solved) {
                    if let Some(screen) = self.project_brush_point_3d(rect, vertex) {
                        consider(screen, BrushHandle3d::Vertex(vertex));
                    }
                }
            }
            BrushEditMode::Edge => {
                for (a, b) in brush_elements::unique_edges(&solved) {
                    let midpoint = [
                        (a[0] + b[0]) * 0.5,
                        (a[1] + b[1]) * 0.5,
                        (a[2] + b[2]) * 0.5,
                    ];
                    if let Some(screen) = self.project_brush_point_3d(rect, midpoint) {
                        consider(screen, BrushHandle3d::Edge(a, b));
                    }
                }
            }
            BrushEditMode::Face => {
                for (face, center, normal) in brush_elements::face_handles(brush, &solved) {
                    if !self.brush_face_handle_visible(center, normal) {
                        continue;
                    }
                    if let Some(screen) = self.project_brush_point_3d(rect, center) {
                        consider(
                            screen,
                            BrushHandle3d::Face {
                                face,
                                center,
                                normal,
                            },
                        );
                    }
                }
            }
        }
        best.map(|(_, handle)| (index, handle))
    }

    fn camera_plane_point(
        &self,
        rect: egui::Rect,
        pointer: egui::Pos2,
        plane: BrushDragPlane3d,
    ) -> Option<[f32; 3]> {
        let (origin, direction) = self.camera_ray_for_pointer(rect, pointer)?;
        let denominator = direction[0] * plane.normal[0]
            + direction[1] * plane.normal[1]
            + direction[2] * plane.normal[2];
        if denominator.abs() < 1.0e-6 {
            return None;
        }
        let offset = [
            plane.anchor[0] - origin[0],
            plane.anchor[1] - origin[1],
            plane.anchor[2] - origin[2],
        ];
        let distance = (offset[0] * plane.normal[0]
            + offset[1] * plane.normal[1]
            + offset[2] * plane.normal[2])
            / denominator;
        (distance > 0.0).then(|| {
            [
                origin[0] + direction[0] * distance,
                origin[1] + direction[1] * distance,
                origin[2] + direction[2] * distance,
            ]
        })
    }

    fn begin_brush_vertex_drag_3d(
        &mut self,
        rect: egui::Rect,
        pointer: egui::Pos2,
        index: usize,
        targets: Vec<[f64; 3]>,
        anchor: [f64; 3],
    ) -> bool {
        let normal = self.viewport_3d_camera().basis().forward;
        let mut plane = BrushDragPlane3d {
            anchor: anchor.map(|value| value as f32),
            normal,
            press_world: anchor.map(|value| value as f32),
        };
        let Some(press_world) = self.camera_plane_point(rect, pointer, plane) else {
            return false;
        };
        plane.press_world = press_world;
        self.brush_vertex_drag = Some(BrushVertexDrag {
            index,
            base: self.project.active_scene().brushes[index].clone(),
            targets,
            press_ground: [pointer.x, 0.0, 0.0],
            plane_3d: Some(plane),
            applied: [0; 3],
        });
        true
    }

    fn update_brush_vertex_drag_3d(&mut self, rect: egui::Rect, pointer: egui::Pos2) {
        let Some(drag) = self.brush_vertex_drag.clone() else {
            return;
        };
        let Some(plane) = drag.plane_3d else {
            return;
        };
        let Some(current) = self.camera_plane_point(rect, pointer, plane) else {
            return;
        };
        let step = self.snap_units.max(1) as f32;
        let mut applied = [0; 3];
        for axis in 0..3 {
            applied[axis] =
                (((current[axis] - plane.press_world[axis]) / step).round() * step) as i32;
        }
        if applied == drag.applied {
            return;
        }
        let mut preview = drag.base.clone();
        if preview.translate_points_near(&drag.targets, applied, 0.5) > 0
            && preview.solve().is_valid()
        {
            self.project.active_scene_mut().brushes[drag.index] = preview;
            if let Some(state) = self.brush_vertex_drag.as_mut() {
                state.applied = applied;
            }
        }
    }

    fn begin_brush_face_drag_3d(
        &mut self,
        rect: egui::Rect,
        pointer: egui::Pos2,
        index: usize,
        face: usize,
        center: [f64; 3],
        normal: [f64; 3],
    ) {
        const NORMAL_PROJECTION_DISTANCE: f64 = 64.0;
        let axis = dominant_axis(normal.map(|value| (value * 4096.0) as i64));
        let axis_aligned =
            (0..3).all(|candidate| candidate == axis || normal[candidate].abs() <= f64::EPSILON);
        if axis_aligned {
            self.replace_brush_selection(index, Some(face));
            self.brush_extrude = Some(BrushExtrude {
                index,
                face,
                base: self.project.active_scene().brushes[index].clone(),
                axis,
                dir: if normal[axis] >= 0.0 { 1 } else { -1 },
                press_y: pointer.y,
                press_ground: self
                    .brush_ground_point_raw(rect, pointer)
                    .unwrap_or([0.0; 3]),
                normal_3d: None,
                screen_direction: egui::Vec2::ZERO,
                units_per_pixel: 0.0,
                applied: [0; 3],
            });
            return;
        }
        let center_screen = self.project_brush_point_3d(rect, center);
        let end = [
            center[0] + normal[0] * NORMAL_PROJECTION_DISTANCE,
            center[1] + normal[1] * NORMAL_PROJECTION_DISTANCE,
            center[2] + normal[2] * NORMAL_PROJECTION_DISTANCE,
        ];
        let end_screen = self.project_brush_point_3d(rect, end);
        let projected = center_screen.zip(end_screen).map(|(a, b)| b - a);
        let (screen_direction, units_per_pixel) = match projected {
            Some(delta) if delta.length() >= 4.0 => (
                delta.normalized(),
                NORMAL_PROJECTION_DISTANCE as f32 / delta.length(),
            ),
            _ => (egui::Vec2::new(0.0, -1.0), EXTRUDE_UNITS_PER_PIXEL),
        };
        self.replace_brush_selection(index, Some(face));
        self.brush_extrude = Some(BrushExtrude {
            index,
            face,
            base: self.project.active_scene().brushes[index].clone(),
            axis,
            dir: 1,
            press_y: pointer.y,
            press_ground: [pointer.x, 0.0, 0.0],
            normal_3d: Some(normal),
            screen_direction,
            units_per_pixel,
            applied: [0; 3],
        });
    }
}

impl ViewportTool3d for BrushTool {
    fn primary_pressed(&self, ws: &mut EditorWorkspace, frame: &ToolFrame3d) {
        let Some(pointer) = frame.pointer_interact else {
            return;
        };
        // Plain press follows the visible brush edit mode. Shift remains a
        // compatible whole-selection move shortcut, but is no longer the
        // only discoverable way to move a brush.
        if frame.modifiers.shift || ws.brush_edit_mode == BrushEditMode::Move {
            if let Some((index, _, _)) = ws.pick_brush_face_for_move_3d(frame.rect, pointer) {
                let others = ws.brush_move_others(index);
                let base = ws.project.active_scene().brushes[index].clone();
                let press_ground = ws
                    .brush_ground_point_raw(frame.rect, pointer)
                    .unwrap_or([0.0; 3]);
                if others.is_empty() {
                    ws.replace_brush_selection(index, ws.selected_brush_face);
                } else {
                    ws.selected_brush = Some(index);
                }
                ws.brush_move = Some(BrushMove {
                    index,
                    base,
                    others,
                    press_ground,
                    applied: [0; 3],
                });
                return;
            }
            if ws.active_tool == ViewTool::Brush {
                if let Some(point) = ws.brush_ground_point(frame.rect, pointer) {
                    ws.brush_drag = Some(BrushDrag {
                        anchor: point,
                        current: point,
                        view: OrthographicView::Top,
                    });
                }
            }
            return;
        }
        if let Some((index, handle)) = ws.pick_brush_handle_3d(frame.rect, pointer) {
            match handle {
                BrushHandle3d::Vertex(vertex) => {
                    if ws.begin_brush_vertex_drag_3d(
                        frame.rect,
                        pointer,
                        index,
                        vec![vertex],
                        vertex,
                    ) {
                        return;
                    }
                }
                BrushHandle3d::Edge(a, b) => {
                    let center = [
                        (a[0] + b[0]) * 0.5,
                        (a[1] + b[1]) * 0.5,
                        (a[2] + b[2]) * 0.5,
                    ];
                    if ws.begin_brush_vertex_drag_3d(frame.rect, pointer, index, vec![a, b], center)
                    {
                        return;
                    }
                }
                BrushHandle3d::Face {
                    face,
                    center,
                    normal,
                } => {
                    ws.begin_brush_face_drag_3d(frame.rect, pointer, index, face, center, normal);
                    return;
                }
            }
        }
        if let Some((index, face)) = ws.pick_brush_face(frame.rect, pointer) {
            let base = ws.project.active_scene().brushes[index].clone();
            let Some((center, normal)) = EditorWorkspace::face_center_and_normal(&base, face)
            else {
                return;
            };
            ws.begin_brush_face_drag_3d(frame.rect, pointer, index, face, center, normal);
        } else if let Some(point) = ws.brush_ground_point(frame.rect, pointer) {
            ws.brush_drag = Some(BrushDrag {
                anchor: point,
                current: point,
                view: OrthographicView::Top,
            });
        }
    }

    fn primary_dragged(&self, ws: &mut EditorWorkspace, frame: &ToolFrame3d) {
        let Some(pointer) = frame.pointer_interact.or(frame.pointer_hover) else {
            return;
        };
        if ws
            .brush_vertex_drag
            .as_ref()
            .is_some_and(|drag| drag.plane_3d.is_some())
        {
            ws.update_brush_vertex_drag_3d(frame.rect, pointer);
            return;
        }
        if let Some(mv) = ws.brush_move.clone() {
            let Some(ground) = ws.brush_ground_point_raw(frame.rect, pointer) else {
                return;
            };
            let step = (ws.snap_units.max(1)) as f32;
            let snap = |v: f32| ((v / step).round() * step) as i32;
            let applied = [
                snap(ground[0] - mv.press_ground[0]),
                0,
                snap(ground[2] - mv.press_ground[2]),
            ];
            if applied != mv.applied {
                ws.apply_brush_move_preview(&mv, applied);
            }
            return;
        }
        if let Some(extrude) = ws.brush_extrude.clone() {
            let step = (ws.snap_units.max(1)) as f32;
            if let Some(normal) = extrude.normal_3d {
                let press = egui::Pos2::new(extrude.press_ground[0], extrude.press_y);
                let raw_units =
                    (pointer - press).dot(extrude.screen_direction) * extrude.units_per_pixel;
                let snapped = ((raw_units / step).round() * step) as i32;
                let applied = [
                    (normal[0] * snapped as f64).round() as i32,
                    (normal[1] * snapped as f64).round() as i32,
                    (normal[2] * snapped as f64).round() as i32,
                ];
                if applied == extrude.applied {
                    return;
                }
                let mut preview = extrude.base.clone();
                preview.translate_face(extrude.face, applied);
                if preview.solve().is_valid() {
                    ws.project.active_scene_mut().brushes[extrude.index] = preview;
                    if let Some(state) = ws.brush_extrude.as_mut() {
                        state.applied = applied;
                    }
                }
                return;
            }
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
            let mut delta = [0i32; 3];
            delta[extrude.axis] = if extrude.axis == 1 {
                snapped * extrude.dir
            } else {
                snapped
            };
            if delta == extrude.applied {
                return;
            }
            let mut preview = extrude.base.clone();
            preview.translate_face(extrude.face, delta);
            if preview.solve().is_valid() {
                ws.project.active_scene_mut().brushes[extrude.index] = preview;
                if let Some(state) = ws.brush_extrude.as_mut() {
                    state.applied = delta;
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
        if ws.commit_brush_move_preview() {
            return;
        }
        if ws.commit_brush_vertex_drag_preview() {
            return;
        }
        if ws.commit_brush_extrude_preview() {
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
                ws.brush_clip_click_in_view(point, OrthographicView::Top);
            }
            return;
        }
        ws.brush_clip_start = None;
        match ws.pick_brush_face_cycled_for_selection_3d(frame.rect, pointer) {
            Some((index, face, _)) => {
                ws.clear_node_selection_state();
                ws.clear_resource_selection_state();
                ws.clear_primitive_selection_state();
                ws.clear_sector_selection();
                if frame.modifiers.shift || frame.modifiers.ctrl {
                    ws.toggle_brush_selection(index);
                } else {
                    ws.replace_brush_selection(index, Some(face));
                }
            }
            None if frame.modifiers.shift => {}
            None => ws.clear_brush_selection(),
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
