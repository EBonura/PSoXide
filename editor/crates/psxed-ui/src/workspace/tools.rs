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
        self.selected_brush = Some(index);
        self.selected_brushes = vec![index];
        self.selected_brush_face = face;
    }

    pub(crate) fn clear_brush_selection(&mut self) {
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
                input.key_pressed(egui::Key::Delete)
                    || input.key_pressed(egui::Key::Backspace),
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
        let Some(brush) = self.project.active_scene().brushes.get(index).cloned() else {
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
        // Numeric placement fallback: exact (unsnapped) min-corner entry.
        if let Some(origin) = self.selected_brush_origin() {
            let mut edited = origin;
            ui.horizontal(|ui| {
                ui.label("Origin");
                ui.add(egui::DragValue::new(&mut edited[0]).speed(1).prefix("X "));
                ui.add(egui::DragValue::new(&mut edited[1]).speed(1).prefix("Y "));
                ui.add(egui::DragValue::new(&mut edited[2]).speed(1).prefix("Z "));
            });
            if edited != origin {
                self.set_selected_brush_origin(edited);
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
        let Some(face) = self.selected_brush_face else {
            ui.label("Click a face to edit its material and UVs.");
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
        ui.label(
            egui::RichText::new(format!("Face {face} of {}", brush.faces.len())).strong(),
        );
        ui.label(plane_label);
        // Numeric face fallback: slide the plane along its dominant axis.
        if let Some((axis, position)) = self.selected_brush_face_axis() {
            let mut edited = position;
            ui.horizontal(|ui| {
                ui.label(format!("Plane {} at", ["X", "Y", "Z"][axis]));
                ui.add(egui::DragValue::new(&mut edited).speed(1));
            });
            if edited != position && !self.set_selected_brush_face_axis_position(edited) {
                self.status =
                    "Face edit rejected: brush would stop enclosing volume".to_string();
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

    /// Bind the selected brush to one Door logic node, or return it to model 0.
    pub(crate) fn set_selected_brush_mover(&mut self, mover: Option<NodeId>) {
        let Some(index) = self.selected_brush else {
            return;
        };
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
            self.project.active_scene_mut().brushes[index].faces[face].uv = edited;
            self.mark_dirty();
        }
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
    /// brush so its solved min corner lands exactly on `origin`. No grid
    /// snapping: typing exact off-grid coordinates is the point of the
    /// fallback. Inspector-owned mutation: history is recorded by the
    /// inspector transaction wrapper, not here.
    pub(crate) fn set_selected_brush_origin(&mut self, origin: [i32; 3]) -> bool {
        let Some(current) = self.selected_brush_origin() else {
            return false;
        };
        let Some(index) = self.selected_brush else {
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
        let texture_lock = self.brush_texture_lock;
        let Some(brush) = self.project.active_scene_mut().brushes.get_mut(index) else {
            return false;
        };
        if texture_lock {
            brush.translate_with_uv_lock(delta, psxed_project::brush::BRUSH_UV_UNITS_PER_TEXEL);
        } else {
            brush.translate(delta);
        }
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
        let Some(brush) = Self::brush_drag_cuboid(drag) else {
            return; // zero-area drag: nothing to commit
        };
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
        let view = self.orthographic_view;
        let [horizontal, vertical] = view.plane_axes();
        let depth_axis = view.depth_axis();
        let point = world.map(f64::from);
        let mut best: Option<(f64, f64, usize, usize)> = None;

        for (index, brush) in self.project.active_scene().brushes.iter().enumerate() {
            let solved = brush.solve();
            if !solved.is_valid() {
                continue;
            }
            let min = view.project_f64(solved.min);
            let max = view.project_f64(solved.max);
            if point[0] < min[0] || point[0] > max[0] || point[1] < min[1] || point[1] > max[1] {
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
                if !point_in_convex_polygon_2d(point, &projected) {
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
            if best.is_none_or(|(best_area, best_depth, best_index, _)| {
                area < best_area
                    || (area == best_area
                        && (depth > best_depth || (depth == best_depth && index < best_index)))
            }) {
                best = Some((area, depth, index, face));
            }
        }
        best.map(|(_, _, index, face)| (index, face))
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
        let Some((index, face)) = self.pick_brush_face_at_2d(world) else {
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
    fn apply_brush_move_preview(&mut self, mv: &BrushMove, applied: [i32; 3]) {
        let mut preview = mv.base.clone();
        preview.translate(applied);
        self.project.active_scene_mut().brushes[mv.index] = preview;
        for (index, base) in &mv.others {
            let mut preview = base.clone();
            preview.translate(applied);
            self.project.active_scene_mut().brushes[*index] = preview;
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
            applied: 0,
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
        if snapped == extrude.applied {
            return;
        }
        let mut delta = [0; 3];
        delta[extrude.axis] = snapped;
        let mut preview = extrude.base.clone();
        preview.translate_face(extrude.face, delta);
        if preview.solve().is_valid() {
            self.project.active_scene_mut().brushes[extrude.index] = preview;
            if let Some(state) = self.brush_extrude.as_mut() {
                state.applied = snapped;
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
        let mut verts: Vec<[f64; 3]> = Vec::new();
        for vert in solved.polygons.iter().flatten().flat_map(|p| p.verts.iter()) {
            if !verts
                .iter()
                .any(|seen| (0..3).all(|axis| (seen[axis] - vert[axis]).abs() <= 0.5))
            {
                verts.push(*vert);
            }
        }
        Some((index, verts))
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
            let mut commit_one = |ws: &mut Self, index: usize, base: psxed_project::brush::Brush| {
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
        if extrude.applied != 0 {
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
                egui::Stroke::new(1.0, EDITOR_OUTLINE_ACCENT)
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
        // Vertex/Edge mode: square handles on the selected brush's
        // projected corners show what a drag will grab.
        if matches!(
            self.selection_mode,
            SelectionMode::Vertex | SelectionMode::Edge
        ) {
            if let Some((_, verts)) = self.selected_brush_solved_verts() {
                let mut seen: Vec<[f64; 2]> = Vec::new();
                for vert in verts {
                    let projected = view.project_f64(vert);
                    if seen.iter().any(|s| {
                        (s[0] - projected[0]).abs() <= 0.5 && (s[1] - projected[1]).abs() <= 0.5
                    }) {
                        continue;
                    }
                    seen.push(projected);
                    let center =
                        transform.world_to_screen([projected[0] as f32, projected[1] as f32]);
                    painter.rect_stroke(
                        egui::Rect::from_center_size(center, egui::Vec2::splat(6.0)),
                        0.0,
                        egui::Stroke::new(1.5, STUDIO_ACCENT),
                        egui::StrokeKind::Inside,
                    );
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
            let selected = self.brush_is_selected(index);
            let stroke = if selected {
                egui::Stroke::new(2.0, STUDIO_ACCENT)
            } else {
                egui::Stroke::new(1.0, EDITOR_OUTLINE_ACCENT)
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
            ws.replace_brush_selection(index, Some(face));
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
                view: OrthographicView::Top,
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
        if ws.commit_brush_move_preview() {
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
        match ws.pick_brush_face(frame.rect, pointer) {
            Some((index, face)) => {
                if frame.modifiers.shift {
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
