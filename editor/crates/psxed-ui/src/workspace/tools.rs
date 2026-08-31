//! 3D-viewport tool dispatch (docs/brush-editor-integration.md, step 1).
//!
//! Each tool is a stateless dispatch object; all in-flight state stays on
//! the workspace (`Interaction`, selection, previews), so tool switches can
//! never orphan private state. egui stays outside: `draw_viewport_3d_body`
//! translates the frame's response into `ToolFrame3d` + event calls, which
//! keeps every tool drivable headlessly through the ViewportHarness.

use super::*;

type BrushGizmoContext = (
    [f64; 3],
    Vec<[f64; 3]>,
    Vec<usize>,
    Vec<BrushElementDragMember>,
);

struct BrushPickSolvedCache {
    key: Option<u64>,
    brushes: Vec<psxed_project::brush::SolvedBrush>,
}

static BRUSH_PICK_SOLVED_CACHE: std::sync::OnceLock<std::sync::Mutex<BrushPickSolvedCache>> =
    std::sync::OnceLock::new();

fn brush_pick_geometry_key(project: &ProjectDocument) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let brushes = &project.active_scene().brushes;
    brushes.len().hash(&mut hasher);
    for brush in brushes {
        brush.faces.len().hash(&mut hasher);
        for face in &brush.faces {
            face.points.hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn with_cached_brush_pick_solves<R>(
    project: &ProjectDocument,
    visit: impl FnOnce(&[psxed_project::brush::SolvedBrush]) -> R,
) -> R {
    let key = brush_pick_geometry_key(project);
    let cache = BRUSH_PICK_SOLVED_CACHE.get_or_init(|| {
        std::sync::Mutex::new(BrushPickSolvedCache {
            key: None,
            brushes: Vec::new(),
        })
    });
    let mut cache = cache.lock().expect("brush pick solved cache");
    if cache.key != Some(key) {
        cache.brushes = project
            .active_scene()
            .brushes
            .iter()
            .map(psxed_project::brush::Brush::solve)
            .collect();
        cache.key = Some(key);
    }
    visit(&cache.brushes)
}

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

/// Initial preview height before the second brush-create gesture authors it.
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
        if ws.brush_vertex_snap_key_down {
            ws.begin_brush_vertex_snap_3d(frame.rect, pointer);
            return;
        }
        let additive = frame.modifiers.shift || frame.modifiers.command || frame.modifiers.ctrl;
        // Cmd+drag on a FACE handle extrudes a new brush out of the face
        // (TrenchBroom-style); a Cmd CLICK still toggles selection via
        // the no-op-release synthesis.
        if frame.modifiers.command && ws.brush_edit_mode == BrushEditMode::Face {
            if let Some((index, BrushHandle3d::Face { face, .. })) =
                ws.pick_brush_handle_3d(frame.rect, pointer)
            {
                if ws.begin_brush_face_extrude_new(frame.rect, pointer, index, face) {
                    return;
                }
            }
        }
        // An additive press on a handle or the gizmo must NOT start the
        // box-select marquee (its live updates would fight the element
        // toggle the click performs on release).
        if additive
            && ws.selected_brush.is_some()
            && (ws
                .pick_brush_element_gizmo_axis_3d(frame.rect, pointer)
                .is_some()
                || ws.pick_brush_handle_3d(frame.rect, pointer).is_some())
        {
            return;
        }
        // The element gizmo owns its screen area: an axis grab starts an
        // axis-constrained group drag of the selected elements.
        if !additive {
            if let Some(axis) = ws.pick_brush_element_gizmo_axis_3d(frame.rect, pointer) {
                if ws.begin_brush_element_gizmo_drag(frame.rect, pointer, axis) {
                    return;
                }
            }
        }
        // Reshape handles (Face/Edge/Vertex) stick out past the brush
        // silhouette, so a handle hit forwards to the Brush gestures even
        // when the pick ray misses the solid itself. Whole objects never
        // start moving from their body: select first, then grab the gizmo.
        if !additive
            && ws.selected_brush.is_some()
            && ws.brush_edit_mode != BrushEditMode::Move
            && ws.pick_brush_handle_3d(frame.rect, pointer).is_some()
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
            || ws.brush_element_transform.is_some()
            || ws.brush_extrude_new.is_some()
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
        // A wiggled human click crosses egui's drag threshold and arrives
        // as a no-op drag; treat it as the click it was meant to be.
        let synthesize_click = ws.brush_release_was_noop_click();
        if ws.brush_move.is_some()
            || ws.brush_vertex_drag.is_some()
            || ws.brush_extrude.is_some()
            || ws.brush_drag.is_some()
            || ws.brush_element_transform.is_some()
            || ws.brush_extrude_new.is_some()
        {
            BrushTool.primary_released(ws, _frame);
            if synthesize_click {
                self.primary_clicked(ws, _frame);
            }
            return;
        }
        if synthesize_click {
            self.primary_clicked(ws, _frame);
            return;
        }
        match ws.interaction {
            Interaction::PrimitiveGizmo(_) => ws.end_primitive_gizmo_drag(),
            Interaction::NodeGizmo(_) => ws.end_node_gizmo_drag(),
            Interaction::PrimitiveGrid(_) => ws.end_primitive_grid_drag(),
            Interaction::BoxSelect3d(_) => ws.end_viewport_3d_box_select(),
            _ => ws.end_primitive_drag(),
        }
    }

    fn primary_clicked(&self, ws: &mut EditorWorkspace, frame: &ToolFrame3d) {
        if ws.brush_vertex_snap_key_down {
            return;
        }
        // Clip mode: clicks place clip points on brush faces.
        if ws.brush_edit_mode == BrushEditMode::Clip && ws.selected_brush.is_some() {
            if let Some(pointer) = frame.pointer_interact.or(frame.pointer_hover) {
                ws.brush_clip_click_3d(frame.rect, pointer);
            }
            return;
        }
        // A click on the element gizmo is a no-op (the gizmo is for
        // dragging); without this it would fall through to face picking.
        if let Some(pointer) = frame.pointer_interact.or(frame.pointer_hover) {
            if ws
                .pick_brush_element_gizmo_axis_3d(frame.rect, pointer)
                .is_some()
            {
                return;
            }
        }
        // Sub-element clicks come first: a click never fires the drag-start
        // event, so this is the ONLY path that can select the handle under
        // the cursor. Running it before the pointer_target match also stops
        // a handle that pokes past the brush silhouette (target None) from
        // falling into the clear-everything arm.
        if ws.selected_brush.is_some() && ws.brush_edit_mode != BrushEditMode::Move {
            if let Some(pointer) = frame.pointer_interact.or(frame.pointer_hover) {
                if let Some((_, handle)) = ws.pick_brush_handle_3d(frame.rect, pointer) {
                    ws.apply_brush_element_selection(handle.element(), frame.modifiers);
                    return;
                }
            }
        }
        // Click selection consumes the same topmost target as hover and
        // drag start, so gizmo clicks never fall through to a face behind.
        match frame.pointer_target {
            Some(Viewport3dPointerTarget::Entity(hit)) => {
                ws.select_node_with_group_semantics(hit.node, frame.modifiers, false);
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
                if !matches!(ws.brush_group_pick(brush), BrushGroupPick::Brush) {
                    ws.select_brush_with_group_semantics(brush, Some(face), frame.modifiers, false);
                    return;
                }
                // Face and Edge mode can jump directly between brushes.
                // Deliberately use the NEAREST face, not the click-through
                // cycle: cycling made repeat clicks land on back faces.
                if matches!(
                    ws.brush_edit_mode,
                    BrushEditMode::Face | BrushEditMode::Edge
                ) {
                    let (brush, face) = frame
                        .pointer_interact
                        .or(frame.pointer_hover)
                        .and_then(|pointer| {
                            ws.pick_brush_face_nearest_for_selection_3d(frame.rect, pointer)
                        })
                        .map(|(brush, face, _)| (brush, face))
                        .unwrap_or((brush, face));
                    // Alt+click remains the Face-mode attribute eyedropper.
                    if ws.brush_edit_mode == BrushEditMode::Face
                        && frame.modifiers.alt
                        && ws.apply_face_attributes_to(brush, face)
                    {
                        return;
                    }
                    if let Some(pointer) = frame.pointer_interact.or(frame.pointer_hover) {
                        if ws.select_brush_element_from_3d_hit(
                            brush,
                            face,
                            frame.rect,
                            pointer,
                            frame.modifiers,
                        ) {
                            return;
                        }
                    }
                }
                ws.select_brush_with_group_semantics(brush, Some(face), frame.modifiers, false);
                ws.status = format!("Selected BSP brush {}", brush + 1);
            }
            Some(Viewport3dPointerTarget::Surface { .. }) => {
                ws.commit_face_selection(frame.modifiers);
            }
            // An additive click that resolves to nothing adds nothing. It
            // must not wipe the multi-selection the user is assembling, so
            // only a plain click on empty space clears.
            None if frame.modifiers.shift || frame.modifiers.command || frame.modifiers.ctrl => {}
            None => {
                ws.commit_face_selection(frame.modifiers);
            }
            Some(
                Viewport3dPointerTarget::PrimitiveGizmo(_) | Viewport3dPointerTarget::NodeGizmo(_),
            ) => {}
        }
    }
}

/// BSP material-paint and entity-placement dispatcher.
pub(crate) struct PaintDispatchTool;

impl PaintDispatchTool {
    fn paint(ws: &mut EditorWorkspace, frame: &ToolFrame3d) {
        let Some(pos) = frame.pointer_interact else {
            return;
        };
        if ws.active_tool == ViewTool::Place {
            let Some((brush, face, hit)) = ws.pick_brush_face_with_hit(frame.rect, pos) else {
                ws.status = "Place on an upward-facing BSP brush surface".to_string();
                return;
            };
            ws.place_bsp_on_brush_face(brush, face, hit);
            return;
        }
        if ws.active_tool == ViewTool::PaintMaterial && ws.bsp_face_paint_active() {
            ws.paint_bsp_brush_face(frame.rect, pos);
            return;
        }
        ws.status = "Material Paint needs a BSP brush face".to_string();
    }
}

/// Brush tool: drag a footprint on the ground plane to create a cuboid
/// brush; click to select the nearest brush under the pointer.
pub(crate) struct BrushTool;

/// One repeatable brush action for the Cmd+R chain.
#[derive(Clone, Copy)]
pub(crate) enum BrushRepeatAction {
    /// Duplicate the selection and move the copies.
    Duplicate([i32; 3]),
    /// Move the selection.
    Nudge([i32; 3]),
}

/// Justify targets for the face-UV align buttons.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum UvAlign {
    Left,
    Right,
    Top,
    Bottom,
    Center,
    Fit,
}

impl UvAlign {
    fn label(self) -> &'static str {
        match self {
            Self::Left => "justified left",
            Self::Right => "justified right",
            Self::Top => "justified top",
            Self::Bottom => "justified bottom",
            Self::Center => "centred",
            Self::Fit => "fitted to the face",
        }
    }
}

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

    /// Shift+scroll drill: cycle the selection through every brush the
    /// cursor ray passes through, nearest first. Scrolling down goes
    /// deeper, up comes back; the cycle wraps. Moving the pointer
    /// re-anchors from wherever the current selection sits in the list.
    pub(crate) fn drill_selection_3d(
        &mut self,
        rect: egui::Rect,
        pointer: egui::Pos2,
        direction: i32,
    ) {
        let hits = self.brush_face_hits_with_hit(rect, pointer);
        if hits.is_empty() {
            return;
        }
        let anchored = self
            .brush_drill
            .filter(|(anchor, _)| (*anchor - pointer).length() <= 6.0)
            .map(|(_, depth)| depth as i32);
        let start = anchored.unwrap_or_else(|| {
            hits.iter()
                .position(|(brush, _, _)| Some(*brush) == self.selected_brush)
                .map_or(-direction.signum(), |position| position as i32)
        });
        let depth = (start + direction).rem_euclid(hits.len() as i32) as usize;
        let (brush, face, _) = hits[depth];
        self.clear_node_selection_state();
        self.clear_resource_selection_state();
        self.clear_primitive_selection_state();
        self.clear_sector_selection();
        self.replace_brush_selection(brush, Some(face));
        self.brush_drill = Some((pointer, depth));
        self.status = format!("Drill {}/{}", depth + 1, hits.len());
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
            if self.brush_effectively_hidden(index)
                || matches!(self.brush_group_pick(index), BrushGroupPick::Locked)
            {
                continue;
            }
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
        let mut projected = Vec::with_capacity(16);
        with_cached_brush_pick_solves(&self.project, |solved_brushes| {
            for (index, solved) in solved_brushes.iter().enumerate() {
                if self.brush_effectively_hidden(index)
                    || matches!(self.brush_group_pick(index), BrushGroupPick::Locked)
                {
                    continue;
                }
                if !solved.is_valid() {
                    continue;
                }
                let mut best: Option<(f32, usize, [f32; 3])> = None;
                for (face, polygon) in solved.polygons.iter().enumerate() {
                    let Some(polygon) = polygon else { continue };
                    projected.clear();
                    for &point in &polygon.verts {
                        let Some(point) = self.project_brush_point_3d(rect, point) else {
                            projected.clear();
                            break;
                        };
                        projected.push(point);
                    }
                    if projected.len() != polygon.verts.len()
                        || (!point_in_polygon_2d(pointer, &projected)
                            && distance_to_polygon_edges_2d(pointer, &projected)
                                > BRUSH_SCREEN_PICK_RADIUS)
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
        });
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

    /// Exact solved bounds of the complete brush selection. Every selected
    /// brush must be valid so transforms either act on the whole set or do
    /// nothing; silently dropping one member would be much worse than
    /// refusing the gesture.
    fn selected_brush_bounds(&self) -> Option<([f64; 3], [f64; 3])> {
        let selected = self.selected_brush_set();
        let mut selected = selected.into_iter();
        let first = self
            .project
            .active_scene()
            .brushes
            .get(selected.next()?)?
            .solve();
        if !first.is_valid() {
            return None;
        }
        let (mut min, mut max) = (first.min, first.max);
        for index in selected {
            let solved = self.project.active_scene().brushes.get(index)?.solve();
            if !solved.is_valid() {
                return None;
            }
            for axis in 0..3 {
                min[axis] = min[axis].min(solved.min[axis]);
                max[axis] = max[axis].max(solved.max[axis]);
            }
        }
        Some((min, max))
    }

    /// Whether `index` is the primary selection or a multi-selection
    /// member (drives the highlight in every view).
    pub(crate) fn brush_is_selected(&self, index: usize) -> bool {
        self.selected_brush == Some(index) || self.selected_brushes.contains(&index)
    }

    /// Plain-click selection: exactly one brush (and optional face),
    /// dropping any multi-selection.
    pub(crate) fn replace_brush_selection(&mut self, index: usize, face: Option<usize>) {
        self.reset_brush_drill();
        if self.selected_brush != Some(index) || self.selected_brush_face != face {
            self.clear_uv_edit_transaction();
        }
        self.selected_brush_elements.clear();
        self.selected_brush_faces.clear();
        self.selected_brush = Some(index);
        self.selected_brushes = vec![index];
        self.selected_brush_face = face;
        if self.brush_edit_mode == BrushEditMode::Face {
            if let Some(face) = face {
                self.selected_brush_faces.push((index, face));
                self.selected_brush_elements.push(BrushElement::Face(face));
            }
        }
    }

    /// Any deliberate selection change invalidates the drill anchor,
    /// so the next Shift+scroll re-anchors from the fresh selection.
    /// It also ends the Cmd+R repeat chain: repeating is always
    /// relative to one continuously-edited selection.
    pub(crate) fn reset_brush_drill(&mut self) {
        self.brush_drill = None;
        self.brush_repeat_chain.clear();
    }

    pub(crate) fn clear_brush_selection(&mut self) {
        self.clear_uv_edit_transaction();
        self.selected_brush = None;
        self.selected_brushes.clear();
        self.selected_brush_face = None;
        self.selected_brush_faces.clear();
        self.selected_brush_elements.clear();
    }

    /// Face-mode selection owns a document-wide set of `(brush, face)`
    /// pairs. The legacy element list remains a primary-brush projection so
    /// the face Inspector and existing single-brush gizmo code keep working.
    fn apply_brush_face_selection(
        &mut self,
        brush: usize,
        face: usize,
        modifiers: egui::Modifiers,
    ) -> bool {
        if self
            .project
            .active_scene()
            .brushes
            .get(brush)
            .and_then(|brush| brush.faces.get(face))
            .is_none()
        {
            return false;
        }

        let additive = modifiers.shift || modifiers.command || modifiers.ctrl;
        let old_active = (self.selected_brush, self.selected_brush_face);
        self.reset_brush_drill();
        if additive {
            if let Some(position) = self
                .selected_brush_faces
                .iter()
                .position(|selected| *selected == (brush, face))
            {
                self.selected_brush_faces.remove(position);
            } else {
                self.selected_brush_faces.push((brush, face));
            }
        } else {
            self.selected_brush_faces.clear();
            self.selected_brush_faces.push((brush, face));
        }

        if self.selected_brush_faces.contains(&(brush, face)) {
            self.selected_brush = Some(brush);
            self.selected_brush_face = Some(face);
        } else if let Some(&(brush, face)) = self.selected_brush_faces.last() {
            self.selected_brush = Some(brush);
            self.selected_brush_face = Some(face);
        } else {
            // Toggling the final face off leaves the owning brush selected,
            // but with no accidental whole-brush material target.
            self.selected_brush = Some(brush);
            self.selected_brush_face = None;
        }

        self.selected_brushes = self
            .selected_brush_faces
            .iter()
            .map(|(brush, _)| *brush)
            .collect();
        self.selected_brushes.sort_unstable();
        self.selected_brushes.dedup();
        if self.selected_brushes.is_empty() {
            self.selected_brushes.push(brush);
        }

        self.selected_brush_elements = self.selected_brush.map_or_else(Vec::new, |active| {
            self.selected_brush_faces
                .iter()
                .filter_map(|(brush, face)| (*brush == active).then_some(BrushElement::Face(*face)))
                .collect()
        });
        if old_active != (self.selected_brush, self.selected_brush_face) {
            self.clear_uv_edit_transaction();
        }
        self.status = self.brush_element_status();
        true
    }

    /// Click selection for a brush sub-element: plain click replaces the
    /// element set, shift/cmd/ctrl toggles membership. `Face` elements
    /// mirror into `selected_brush_face` so the face inspector and UV
    /// transaction keep working untouched.
    pub(crate) fn apply_brush_element_selection(
        &mut self,
        element: BrushElement,
        modifiers: egui::Modifiers,
    ) {
        if let (BrushElement::Face(face), Some(brush)) = (element, self.selected_brush) {
            self.apply_brush_face_selection(brush, face, modifiers);
            return;
        }
        self.selected_brush_faces.clear();
        let additive = modifiers.shift || modifiers.command || modifiers.ctrl;
        if additive {
            if let Some(position) = self
                .selected_brush_elements
                .iter()
                .position(|existing| *existing == element)
            {
                self.selected_brush_elements.remove(position);
            } else {
                self.selected_brush_elements.push(element);
            }
        } else {
            self.selected_brush_elements = vec![element];
        }
        if let BrushElement::Face(face) = element {
            if self.selected_brush_elements.contains(&element) {
                if self.selected_brush_face != Some(face) {
                    self.clear_uv_edit_transaction();
                }
                self.selected_brush_face = Some(face);
            } else if self.selected_brush_face == Some(face) {
                // Deselected the mirrored face: fall back to the most
                // recent remaining Face element, if any.
                self.selected_brush_face =
                    self.selected_brush_elements
                        .iter()
                        .rev()
                        .find_map(|element| match element {
                            BrushElement::Face(face) => Some(*face),
                            _ => None,
                        });
                self.clear_uv_edit_transaction();
            }
        }
        self.status = self.brush_element_status();
    }

    /// Select the Face/Edge under a hit even when it belongs to a different
    /// brush. Faces retain their brush identity for additive selection;
    /// edges remain scoped to the active brush.
    fn select_brush_element_from_3d_hit(
        &mut self,
        brush: usize,
        face: usize,
        rect: egui::Rect,
        pointer: egui::Pos2,
        modifiers: egui::Modifiers,
    ) -> bool {
        if self.brush_edit_mode == BrushEditMode::Face {
            if self.selected_brush != Some(brush) {
                self.clear_node_selection_state();
                self.clear_resource_selection_state();
                self.clear_primitive_selection_state();
                self.clear_sector_selection();
            }
            return self.apply_brush_face_selection(brush, face, modifiers);
        }
        let element = match self.brush_edit_mode {
            BrushEditMode::Edge => {
                let Some(element) = self.pick_brush_face_edge_on_3d(brush, face, rect, pointer)
                else {
                    return false;
                };
                element
            }
            BrushEditMode::Move
            | BrushEditMode::Face
            | BrushEditMode::Vertex
            | BrushEditMode::Clip => return false,
        };
        let crossing_brushes = self.selected_brush != Some(brush);
        if crossing_brushes
            && !self.select_brush_with_group_semantics(
                brush,
                Some(face),
                egui::Modifiers::NONE,
                false,
            )
        {
            return false;
        }
        self.apply_brush_element_selection(
            element,
            if crossing_brushes {
                egui::Modifiers::NONE
            } else {
                modifiers
            },
        );
        true
    }

    /// Orthographic counterpart of `select_brush_element_from_3d_hit`.
    /// Face mode can use the solved hit directly; Edge mode switches brush
    /// first and then resolves the projected edge at the same click point.
    pub(crate) fn select_brush_element_from_2d_hit(
        &mut self,
        brush: usize,
        face: usize,
        world: [f32; 2],
        modifiers: egui::Modifiers,
    ) -> bool {
        if !matches!(
            self.brush_edit_mode,
            BrushEditMode::Face | BrushEditMode::Edge
        ) {
            return false;
        }
        if self.brush_edit_mode == BrushEditMode::Face {
            if self.selected_brush != Some(brush) {
                self.clear_node_selection_state();
                self.clear_resource_selection_state();
                self.clear_primitive_selection_state();
                self.clear_sector_selection();
            }
            return self.apply_brush_face_selection(brush, face, modifiers);
        }
        let crossing_brushes = self.selected_brush != Some(brush);
        if crossing_brushes
            && !self.select_brush_with_group_semantics(
                brush,
                Some(face),
                egui::Modifiers::NONE,
                false,
            )
        {
            return false;
        }
        let modifiers = if crossing_brushes {
            egui::Modifiers::NONE
        } else {
            modifiers
        };
        match self.brush_edit_mode {
            BrushEditMode::Edge => self.select_brush_elements_2d(world, modifiers),
            BrushEditMode::Move
            | BrushEditMode::Face
            | BrushEditMode::Vertex
            | BrushEditMode::Clip => false,
        }
    }

    pub(crate) fn brush_element_status(&self) -> String {
        if self.brush_edit_mode == BrushEditMode::Face && !self.selected_brush_faces.is_empty() {
            let faces = self.selected_brush_faces.len();
            let brushes = self
                .selected_brush_faces
                .iter()
                .map(|(brush, _)| *brush)
                .collect::<std::collections::BTreeSet<_>>()
                .len();
            return format!(
                "Selected {faces} face{} across {brushes} brush{}",
                if faces == 1 { "" } else { "s" },
                if brushes == 1 { "" } else { "es" }
            );
        }
        let (mut faces, mut edges, mut vertices) = (0usize, 0usize, 0usize);
        for element in &self.selected_brush_elements {
            match element {
                BrushElement::Face(_) => faces += 1,
                BrushElement::Edge(..) => edges += 1,
                BrushElement::Vertex(_) => vertices += 1,
            }
        }
        let brush = self.selected_brush.map_or(0, |index| index + 1);
        let mut parts = Vec::new();
        if faces > 0 {
            parts.push(format!("{faces} face{}", if faces == 1 { "" } else { "s" }));
        }
        if edges > 0 {
            parts.push(format!("{edges} edge{}", if edges == 1 { "" } else { "s" }));
        }
        if vertices > 0 {
            parts.push(format!(
                "{vertices} vert{}",
                if vertices == 1 { "ex" } else { "ices" }
            ));
        }
        if parts.is_empty() {
            format!("Cleared element selection on brush {brush}")
        } else {
            format!("Selected {} on brush {brush}", parts.join(", "))
        }
    }

    /// Elements under `world` in the 2D view for the active edit mode:
    /// the whole depth column behind one projected vertex or edge, since
    /// a 2D click cannot distinguish depth. Empty when nothing is within
    /// `tolerance` (projected units) or the mode has no point handles.
    pub(crate) fn pick_brush_elements_2d(
        &self,
        world: [f32; 2],
        tolerance: f32,
    ) -> Vec<BrushElement> {
        let Some((index, verts)) = self.selected_brush_solved_verts() else {
            return Vec::new();
        };
        let view = self.orthographic_view;
        let point = world.map(f64::from);
        let tolerance2 = f64::from(tolerance.max(0.0)).powi(2);
        match self.brush_edit_mode {
            BrushEditMode::Vertex => {
                let mut best: Option<(f64, [f64; 2])> = None;
                for vert in &verts {
                    let projected = view.project_f64(*vert);
                    let d2 = (projected[0] - point[0]).powi(2) + (projected[1] - point[1]).powi(2);
                    if d2 <= tolerance2 && best.is_none_or(|(best_d2, _)| d2 < best_d2) {
                        best = Some((d2, projected));
                    }
                }
                let Some((_, projected)) = best else {
                    return Vec::new();
                };
                Self::brush_vertex_column(view, &verts, projected)
                    .into_iter()
                    .map(|vertex| {
                        BrushElement::Vertex(brush_elements::quantize_element_point(vertex))
                    })
                    .collect()
            }
            BrushEditMode::Edge => {
                let solved = self.project.active_scene().brushes[index].solve();
                let edges = brush_elements::unique_edges(&solved);
                let mut best: Option<(f64, [f64; 2], [f64; 2])> = None;
                for (a, b) in &edges {
                    let pa = view.project_f64(*a);
                    let pb = view.project_f64(*b);
                    let d2 = point_segment_distance2(point, pa, pb);
                    if d2 <= tolerance2 && best.is_none_or(|(best_d2, _, _)| d2 < best_d2) {
                        best = Some((d2, pa, pb));
                    }
                }
                let Some((_, pa, pb)) = best else {
                    return Vec::new();
                };
                // Every 3D edge whose projection coincides with the found
                // segment (either endpoint order): the edge depth column.
                let near =
                    |a: [f64; 2], b: [f64; 2]| (0..2).all(|axis| (a[axis] - b[axis]).abs() <= 0.5);
                edges
                    .iter()
                    .filter(|(a, b)| {
                        let qa = view.project_f64(*a);
                        let qb = view.project_f64(*b);
                        (near(qa, pa) && near(qb, pb)) || (near(qa, pb) && near(qb, pa))
                    })
                    .map(|(a, b)| {
                        let (ka, kb) = brush_elements::edge_element_key(*a, *b);
                        BrushElement::Edge(ka, kb)
                    })
                    .collect()
            }
            BrushEditMode::Move | BrushEditMode::Face | BrushEditMode::Clip => Vec::new(),
        }
    }

    /// 2D handle-first click routing: select the element column under
    /// `world`. Returns true when something was picked, so callers skip
    /// face/body selection and silhouette handle clicks never clear.
    pub(crate) fn select_brush_elements_2d(
        &mut self,
        world: [f32; 2],
        modifiers: egui::Modifiers,
    ) -> bool {
        if self.selected_brush.is_none()
            || matches!(
                self.brush_edit_mode,
                BrushEditMode::Move | BrushEditMode::Face
            )
        {
            return false;
        }
        let tolerance = 8.0 / self.viewport_zoom.max(f32::EPSILON);
        let elements = self.pick_brush_elements_2d(world, tolerance);
        if elements.is_empty() {
            return false;
        }
        self.apply_brush_element_selection_set(&elements, modifiers);
        true
    }

    /// Apply a 2D column pick: plain click replaces the element set with
    /// the column, additive toggles the whole column as one unit.
    pub(crate) fn apply_brush_element_selection_set(
        &mut self,
        elements: &[BrushElement],
        modifiers: egui::Modifiers,
    ) {
        if elements.is_empty() {
            return;
        }
        let additive = modifiers.shift || modifiers.command || modifiers.ctrl;
        if additive {
            let all_present = elements
                .iter()
                .all(|element| self.selected_brush_elements.contains(element));
            if all_present {
                self.selected_brush_elements
                    .retain(|element| !elements.contains(element));
            } else {
                for element in elements {
                    if !self.selected_brush_elements.contains(element) {
                        self.selected_brush_elements.push(*element);
                    }
                }
            }
        } else {
            self.selected_brush_elements = elements.to_vec();
        }
        self.status = self.brush_element_status();
    }

    /// Solved world positions of every selected vertex and edge endpoint,
    /// deduped: the drag target set for a group move.
    pub(crate) fn selected_brush_element_targets(&self) -> Vec<[f64; 3]> {
        let Some(brush) = self
            .selected_brush
            .and_then(|index| self.project.active_scene().brushes.get(index))
        else {
            return Vec::new();
        };
        Self::brush_element_targets(brush, &self.selected_brush_elements)
    }

    /// Resolve one brush's component selection into solved world vertices.
    /// Keeping this owner-aware is what lets a document-wide face selection
    /// participate in one gizmo gesture without losing the earlier brushes.
    fn brush_element_targets(
        brush: &psxed_project::brush::Brush,
        elements: &[BrushElement],
    ) -> Vec<[f64; 3]> {
        let solved = brush.solve();
        let vertices = brush_elements::unique_vertices(&solved);
        let edges = brush_elements::unique_edges(&solved);
        let mut targets: Vec<[f64; 3]> = Vec::new();
        let push = |point: [f64; 3], targets: &mut Vec<[f64; 3]>| {
            if !targets
                .iter()
                .any(|seen| (0..3).all(|axis| (seen[axis] - point[axis]).abs() <= 0.5))
            {
                targets.push(point);
            }
        };
        for element in elements {
            match element {
                BrushElement::Vertex(key) => {
                    if let Some(vertex) = vertices
                        .iter()
                        .find(|vertex| brush_elements::quantize_element_point(**vertex) == *key)
                    {
                        push(*vertex, &mut targets);
                    }
                }
                BrushElement::Edge(ka, kb) => {
                    if let Some((a, b)) = edges
                        .iter()
                        .find(|(a, b)| brush_elements::edge_element_key(*a, *b) == (*ka, *kb))
                    {
                        push(*a, &mut targets);
                        push(*b, &mut targets);
                    }
                }
                BrushElement::Face(face) => {
                    if let Some(Some(polygon)) = solved.polygons.get(*face) {
                        for vertex in &polygon.verts {
                            push(*vertex, &mut targets);
                        }
                    }
                }
            }
        }
        targets
    }

    /// Exact face subsets grouped by owning brush, with the active brush
    /// first and every other brush retained as an element-level companion.
    fn selected_face_gizmo_context(&self) -> Option<BrushGizmoContext> {
        let primary_index = self.selected_brush?;
        let mut faces_by_brush = std::collections::BTreeMap::<usize, Vec<usize>>::new();
        for &(brush, face) in &self.selected_brush_faces {
            let faces = faces_by_brush.entry(brush).or_default();
            if !faces.contains(&face) {
                faces.push(face);
            }
        }
        let mut members = Vec::new();
        for (index, faces) in faces_by_brush {
            let base = self.project.active_scene().brushes.get(index)?.clone();
            let elements: Vec<_> = faces.iter().copied().map(BrushElement::Face).collect();
            let targets = Self::brush_element_targets(&base, &elements);
            if targets.is_empty() {
                continue;
            }
            members.push(BrushElementDragMember {
                index,
                base,
                targets,
                faces,
            });
        }
        let primary_position = members
            .iter()
            .position(|member| member.index == primary_index)?;
        let primary = members.remove(primary_position);

        let mut all_targets = primary.targets.clone();
        for member in &members {
            for point in &member.targets {
                if !all_targets
                    .iter()
                    .any(|seen| (0..3).all(|axis| (seen[axis] - point[axis]).abs() <= 0.5))
                {
                    all_targets.push(*point);
                }
            }
        }
        let count = all_targets.len() as f64;
        let mut anchor = [0.0; 3];
        for point in &all_targets {
            for axis in 0..3 {
                anchor[axis] += point[axis] / count;
            }
        }
        Some((anchor, primary.targets, primary.faces, members))
    }

    /// Selected Face element indices (whole planes ride face gestures).
    pub(crate) fn selected_brush_element_faces(&self) -> Vec<usize> {
        self.selected_brush_elements
            .iter()
            .filter_map(|element| match element {
                BrushElement::Face(face) => Some(*face),
                _ => None,
            })
            .collect()
    }

    /// What the transform gizmo acts on for the current edit mode:
    /// `(anchor, targets, faces)`. Brush mode targets the whole primary
    /// brush (every corner, every face plane: rigid moves/rotates/
    /// scales); Face/Edge/Vertex modes target the element selection.
    pub(crate) fn brush_gizmo_context(&self) -> Option<BrushGizmoContext> {
        match self.brush_edit_mode {
            BrushEditMode::Clip => None,
            BrushEditMode::Move => {
                let index = self.selected_brush?;
                let brush = self.project.active_scene().brushes.get(index)?;
                let solved = brush.solve();
                if !solved.is_valid() {
                    return None;
                }
                let (selection_min, selection_max) = self.selected_brush_bounds()?;
                let anchor = [
                    (selection_min[0] + selection_max[0]) * 0.5,
                    (selection_min[1] + selection_max[1]) * 0.5,
                    (selection_min[2] + selection_max[2]) * 0.5,
                ];
                let targets = brush_elements::unique_vertices(&solved);
                let faces = (0..brush.faces.len()).collect();
                Some((anchor, targets, faces, Vec::new()))
            }
            _ => {
                if self.selected_brush_elements.is_empty() {
                    return None;
                }
                if self.brush_edit_mode == BrushEditMode::Face
                    && !self.selected_brush_faces.is_empty()
                {
                    return self.selected_face_gizmo_context();
                }
                let anchor = self.selected_brush_element_centroid()?;
                let targets = self.selected_brush_element_targets();
                if targets.is_empty() {
                    return None;
                }
                Some((
                    anchor,
                    targets,
                    self.selected_brush_element_faces(),
                    Vec::new(),
                ))
            }
        }
    }

    /// Centroid of the selected element targets: the element gizmo anchor.
    pub(crate) fn selected_brush_element_centroid(&self) -> Option<[f64; 3]> {
        let targets = self.selected_brush_element_targets();
        if targets.is_empty() {
            return None;
        }
        let count = targets.len() as f64;
        let mut centroid = [0.0; 3];
        for target in &targets {
            for axis in 0..3 {
                centroid[axis] += target[axis] / count;
            }
        }
        Some(centroid)
    }

    /// Screen polylines of the element gizmo, one per world axis, shaped
    /// by the active Transform mode: straight arrows for Move and Scale
    /// (Scale draws a box tip), rotation RINGS for Rotate (the ring
    /// around axis N lies in the plane of the other two axes). Sizing is
    /// screen-constant so the gizmo stays grabbable at any distance.
    pub(crate) fn brush_element_gizmo_polylines_3d(
        &self,
        rect: egui::Rect,
    ) -> Option<[Vec<egui::Pos2>; 3]> {
        const TARGET_PX: f32 = 72.0;
        const PROBE_WORLD: f64 = 64.0;
        const RING_SEGMENTS: usize = 24;
        let (centroid, _, _, _) = self.brush_gizmo_context()?;
        let origin = self.project_brush_point_3d(rect, centroid)?;
        let mut world_len = [0.0f64; 3];
        for axis in 0..3 {
            let mut probe = centroid;
            probe[axis] += PROBE_WORLD;
            let probe_screen = self.project_brush_point_3d(rect, probe)?;
            let px_per_probe = origin.distance(probe_screen).max(0.5);
            // Foreshortened axes (pointing at the camera) would solve to
            // enormous world lengths; the clamp keeps them finite.
            world_len[axis] =
                (PROBE_WORLD * f64::from(TARGET_PX / px_per_probe)).clamp(16.0, 16384.0);
        }
        let mut polylines: [Vec<egui::Pos2>; 3] = Default::default();
        for axis in 0..3 {
            if self.transform_gizmo_mode == TransformGizmoMode::Rotate {
                let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
                let radius = [world_len[u], world_len[v]];
                let mut ring = Vec::with_capacity(RING_SEGMENTS + 1);
                for segment in 0..=RING_SEGMENTS {
                    let angle = segment as f64 / RING_SEGMENTS as f64 * core::f64::consts::TAU;
                    let mut point = centroid;
                    point[u] += angle.cos() * radius[0];
                    point[v] += angle.sin() * radius[1];
                    if let Some(screen) = self.project_brush_point_3d(rect, point) {
                        ring.push(screen);
                    }
                }
                if ring.len() < 2 {
                    return None;
                }
                polylines[axis] = ring;
            } else {
                let mut tip = centroid;
                tip[axis] += world_len[axis];
                polylines[axis] = vec![origin, self.project_brush_point_3d(rect, tip)?];
            }
        }
        Some(polylines)
    }

    /// The gizmo axis under the pointer, if any (9px pick radius).
    pub(crate) fn pick_brush_element_gizmo_axis_3d(
        &self,
        rect: egui::Rect,
        pointer: egui::Pos2,
    ) -> Option<usize> {
        let polylines = self.brush_element_gizmo_polylines_3d(rect)?;
        // Dead zone at the shared origin: all axes meet there, so a grab
        // is ambiguous and center clicks belong to body selection/move.
        let (anchor, _, _, _) = self.brush_gizmo_context()?;
        if let Some(origin) = self.project_brush_point_3d(rect, anchor) {
            if origin.distance(pointer) <= 12.0 {
                return None;
            }
        }
        let mut best: Option<(f32, usize)> = None;
        for (axis, polyline) in polylines.iter().enumerate() {
            for pair in polyline.windows(2) {
                let d2 = point_segment_distance2(
                    [f64::from(pointer.x), f64::from(pointer.y)],
                    [f64::from(pair[0].x), f64::from(pair[0].y)],
                    [f64::from(pair[1].x), f64::from(pair[1].y)],
                ) as f32;
                if d2 <= 81.0 && best.is_none_or(|(best_d2, _)| d2 < best_d2) {
                    best = Some((d2, axis));
                }
            }
        }
        best.map(|(_, axis)| axis)
    }

    /// Begin a gesture on the element gizmo: what the grab does follows
    /// the Transform group (Move = axis-constrained translate, Rotate =
    /// spin about the axis, Scale = stretch along it, both about the
    /// selection centroid). Returns false when nothing is selected.
    fn begin_brush_element_gizmo_drag(
        &mut self,
        rect: egui::Rect,
        pointer: egui::Pos2,
        axis: usize,
    ) -> bool {
        let Some(index) = self.selected_brush else {
            return false;
        };
        let Some((anchor, targets, faces, element_others)) = self.brush_gizmo_context() else {
            return false;
        };
        if targets.is_empty() {
            return false;
        }
        // Rotating or scaling a single point about itself is the
        // identity; such selections always move instead.
        let mode = if targets.len() < 2 {
            TransformGizmoMode::Move
        } else {
            self.transform_gizmo_mode
        };
        let whole_brush_others = if self.brush_edit_mode == BrushEditMode::Move {
            self.brush_move_others(index)
        } else {
            Vec::new()
        };
        match mode {
            TransformGizmoMode::Move => {
                if !self.begin_brush_vertex_drag_3d(rect, pointer, index, targets, anchor) {
                    return false;
                }
                if let Some(drag) = self.brush_vertex_drag.as_mut() {
                    let mut mask = [false; 3];
                    mask[axis] = true;
                    drag.axis_mask = mask;
                    drag.faces = faces;
                    drag.others = whole_brush_others;
                    drag.element_others = element_others;
                }
                true
            }
            TransformGizmoMode::Rotate | TransformGizmoMode::Scale => {
                let Some(base) = self.project.active_scene().brushes.get(index).cloned() else {
                    return false;
                };
                let Some(center_screen) = self.project_brush_point_3d(rect, anchor) else {
                    return false;
                };
                let screen_axis = {
                    let mut probe = anchor;
                    probe[axis] += 64.0;
                    self.project_brush_point_3d(rect, probe)
                        .map(|tip| {
                            let direction = tip - center_screen;
                            if direction.length_sq() > f32::EPSILON {
                                direction.normalized()
                            } else {
                                Vec2::RIGHT
                            }
                        })
                        .unwrap_or(Vec2::RIGHT)
                };
                let offset = pointer - center_screen;
                self.brush_element_transform = Some(BrushElementTransformDrag {
                    index,
                    base,
                    others: whole_brush_others,
                    element_others,
                    targets,
                    faces,
                    center: anchor,
                    axis,
                    rotate: mode == TransformGizmoMode::Rotate,
                    grid_safe_rotation: self.brush_edit_mode == BrushEditMode::Move,
                    rotation_snap_degrees: if self.brush_edit_mode == BrushEditMode::Move {
                        BRUSH_ROTATION_SNAP_DEGREES
                    } else {
                        5
                    },
                    start_pointer: pointer,
                    screen_axis,
                    center_screen,
                    start_angle: offset.y.atan2(offset.x),
                    applied: 0,
                });
                true
            }
        }
    }

    /// Advance the rotate/scale gesture. Rigid BSP rotation uses lattice-safe
    /// quarter turns; deliberate face/edge reshaping retains 5° (or fine 1°)
    /// angular steps. Previews rebuild from base and must stay bounded/valid.
    pub(crate) fn update_brush_element_transform(&mut self, pointer: egui::Pos2, fine: bool) {
        let Some(drag) = self.brush_element_transform.clone() else {
            return;
        };
        let rotation_snap_degrees = if drag.grid_safe_rotation {
            BRUSH_ROTATION_SNAP_DEGREES
        } else if fine {
            1
        } else {
            5
        };
        if let Some(state) = self.brush_element_transform.as_mut() {
            state.rotation_snap_degrees = rotation_snap_degrees;
        }
        let applied = if drag.rotate {
            // Angular tracking around the projected centroid: the pointer
            // sweeps the ring, whatever the screen direction.
            let offset = pointer - drag.center_screen;
            if offset.length_sq() < 16.0 {
                return;
            }
            let sweep = crate::workspace::editing::wrap_angle_radians(
                offset.y.atan2(offset.x) - drag.start_angle,
            );
            let degrees = f64::from(sweep).to_degrees();
            if drag.grid_safe_rotation {
                snap_brush_rotation_degrees(degrees)
            } else {
                let step = f64::from(rotation_snap_degrees);
                ((degrees / step).round() * step) as i32
            }
        } else {
            // Pointer travel projected onto the grabbed axis's screen
            // direction: along the arrow grows, against it shrinks.
            let travel = (pointer - drag.start_pointer).dot(drag.screen_axis);
            let step = 5.0;
            ((f64::from(travel) / 2.56 / step).round() * step) as i32
        };
        if applied == drag.applied {
            return;
        }
        let map = if drag.rotate {
            let radians = f64::from(applied).to_radians();
            let (sin, cos) = radians.sin_cos();
            let mut map = [[0.0; 3]; 3];
            let a = drag.axis;
            let (u, v) = ((a + 1) % 3, (a + 2) % 3);
            map[a][a] = 1.0;
            map[u][u] = cos;
            map[u][v] = -sin;
            map[v][u] = sin;
            map[v][v] = cos;
            map
        } else {
            let factor = (1.0 + f64::from(applied) / 100.0).clamp(0.05, 16.0);
            let mut map = [[0.0; 3]; 3];
            for (axis, row) in map.iter_mut().enumerate() {
                row[axis] = if axis == drag.axis { factor } else { 1.0 };
            }
            map
        };
        let snap_step = i32::from(self.snap_units.max(1));
        let mut previews = Vec::with_capacity(drag.others.len() + drag.element_others.len() + 1);
        let mut primary = drag.base.clone();
        if primary.transform_selected_snapped(
            &drag.faces,
            &drag.targets,
            drag.center,
            map,
            0.5,
            snap_step,
        ) == 0
            || !brush_preview_ok(&primary)
        {
            self.status = if drag.rotate {
                format!(
                    "Rotate {applied}° rejected: result would not form a valid solid on Grid {}",
                    self.snap_units
                )
            } else {
                format!("Scale rejected on Grid {}", self.snap_units)
            };
            return;
        }
        if drag.grid_safe_rotation {
            let Some(snapped) = primary.snapped_solved_to_grid(snap_step) else {
                self.status = format!(
                    "Rotate {applied}° rejected: result cannot be re-snapped as a valid solid on Grid {}",
                    self.snap_units
                );
                return;
            };
            primary = snapped;
        }
        previews.push((drag.index, primary));
        for (index, base) in &drag.others {
            let mut preview = base.clone();
            let faces: Vec<usize> = (0..preview.faces.len()).collect();
            if preview.transform_selected_snapped(&faces, &[], drag.center, map, 0.5, snap_step)
                == 0
                || !brush_preview_ok(&preview)
            {
                self.status = if drag.rotate {
                    format!(
                        "Rotate {applied}° rejected: result would not form a valid solid on Grid {}",
                        self.snap_units
                    )
                } else {
                    format!("Scale rejected on Grid {}", self.snap_units)
                };
                return;
            }
            if drag.grid_safe_rotation {
                let Some(snapped) = preview.snapped_solved_to_grid(snap_step) else {
                    self.status = format!(
                        "Rotate {applied}° rejected: result cannot be re-snapped as a valid solid on Grid {}",
                        self.snap_units
                    );
                    return;
                };
                preview = snapped;
            }
            previews.push((*index, preview));
        }
        for member in &drag.element_others {
            let mut preview = member.base.clone();
            if preview.transform_selected_snapped(
                &member.faces,
                &member.targets,
                drag.center,
                map,
                0.5,
                snap_step,
            ) == 0
                || !brush_preview_ok(&preview)
            {
                self.status = if drag.rotate {
                    format!(
                        "Rotate {applied}° rejected: result would not form a valid solid on Grid {}",
                        self.snap_units
                    )
                } else {
                    format!("Scale rejected on Grid {}", self.snap_units)
                };
                return;
            }
            if drag.grid_safe_rotation {
                let Some(snapped) = preview.snapped_solved_to_grid(snap_step) else {
                    self.status = format!(
                        "Rotate {applied}° rejected: result cannot be re-snapped as a valid solid on Grid {}",
                        self.snap_units
                    );
                    return;
                };
                preview = snapped;
            }
            previews.push((member.index, preview));
        }
        {
            let scene = self.project.active_scene_mut();
            for (index, preview) in previews {
                scene.brushes[index] = preview;
            }
            if let Some(state) = self.brush_element_transform.as_mut() {
                state.applied = applied;
            }
            self.status = if drag.rotate {
                format!(
                    "Rotate {}° about {} (snap {}°)",
                    applied,
                    ["X", "Y", "Z"][drag.axis],
                    rotation_snap_degrees
                )
            } else {
                format!(
                    "Scale {}% along {}",
                    100 + applied,
                    ["X", "Y", "Z"][drag.axis]
                )
            };
        }
    }

    /// True when a press-release ran through the drag machinery but
    /// applied nothing: egui suppresses the click event once its 6 px
    /// drag threshold is crossed, so a slightly-wiggled human click
    /// arrives ONLY as drag-start/drag-stop. Release handlers use this
    /// to synthesize the click the user actually meant.
    pub(crate) fn brush_release_was_noop_click(&self) -> bool {
        let gesture_active = self.brush_move.is_some()
            || self.brush_vertex_drag.is_some()
            || self.brush_extrude.is_some()
            || self.brush_element_transform.is_some()
            || self.brush_extrude_new.is_some();
        if gesture_active {
            return self
                .brush_move
                .as_ref()
                .is_none_or(|gesture| gesture.applied == [0; 3])
                && self
                    .brush_vertex_drag
                    .as_ref()
                    .is_none_or(|gesture| gesture.applied == [0; 3])
                && self
                    .brush_extrude
                    .as_ref()
                    .is_none_or(|gesture| gesture.applied == [0; 3])
                && self
                    .brush_element_transform
                    .as_ref()
                    .is_none_or(|gesture| gesture.applied == 0)
                && self
                    .brush_extrude_new
                    .as_ref()
                    .is_none_or(|gesture| gesture.applied == 0);
        }
        // No gesture and no interaction: the press landed on a brush
        // body arm that consumes nothing (non-Move edit modes).
        self.brush_drag.is_none() && matches!(self.interaction, Interaction::Idle)
    }

    /// Begin extruding a NEW brush out of `face` (Cmd+drag its handle):
    /// pointer travel along the projected outward normal sets the
    /// snapped extrusion distance.
    fn begin_brush_face_extrude_new(
        &mut self,
        rect: egui::Rect,
        pointer: egui::Pos2,
        index: usize,
        face: usize,
    ) -> bool {
        let Some(brush) = self.project.active_scene().brushes.get(index) else {
            return false;
        };
        let Some((center, normal)) = Self::face_center_and_normal(brush, face) else {
            return false;
        };
        let Some(center_screen) = self.project_brush_point_3d(rect, center) else {
            return false;
        };
        let probe = [
            center[0] + normal[0] * 64.0,
            center[1] + normal[1] * 64.0,
            center[2] + normal[2] * 64.0,
        ];
        let Some(probe_screen) = self.project_brush_point_3d(rect, probe) else {
            return false;
        };
        let px = center_screen.distance(probe_screen).max(0.5);
        let direction = probe_screen - center_screen;
        let screen_dir = if direction.length_sq() > f32::EPSILON {
            direction.normalized()
        } else {
            Vec2::RIGHT
        };
        self.brush_extrude_new = Some(BrushFaceExtrudeNew {
            source: index,
            face,
            screen_dir,
            units_per_px: 64.0 / px,
            start_pointer: pointer,
            applied: 0,
        });
        true
    }

    fn update_brush_face_extrude_new(&mut self, pointer: egui::Pos2) {
        let Some(gesture) = self.brush_extrude_new.clone() else {
            return;
        };
        let travel = (pointer - gesture.start_pointer).dot(gesture.screen_dir);
        let step = f32::from(self.snap_units.max(1));
        let units = (travel * gesture.units_per_px / step).round() * step;
        let applied = (units as i32).max(0);
        if applied == gesture.applied {
            return;
        }
        if let Some(state) = self.brush_extrude_new.as_mut() {
            state.applied = applied;
        }
        self.status = if applied > 0 {
            format!("Extrude new brush: {applied} units (release to create)")
        } else {
            "Extrude new brush: drag outward".to_string()
        };
    }

    /// Create and select the prism extruded out of `face` (one undo
    /// step). Shared by the Cmd+drag gesture and the toolbar button.
    fn apply_face_extrusion(&mut self, index: usize, face: usize, distance: i32) -> bool {
        let Some(candidate) = self
            .project
            .active_scene()
            .brushes
            .get(index)
            .and_then(|brush| brush.extruded_from_face(face, distance))
        else {
            self.status = "Extrusion produced no solid".to_string();
            return false;
        };
        self.push_undo();
        let scene = self.project.active_scene_mut();
        scene.brushes.push(candidate);
        let new_index = scene.brushes.len() - 1;
        self.replace_brush_selection(new_index, None);
        self.clear_node_selection_state();
        self.mark_dirty();
        self.status = format!("Extruded brush {} out {distance} units", new_index + 1);
        true
    }

    /// Toolbar button: extrude the selected face by one grid step.
    pub(crate) fn extrude_selected_face_one_step(&mut self) {
        let (Some(index), Some(face)) = (self.selected_brush, self.selected_brush_face) else {
            self.status = "Select a face to extrude".to_string();
            return;
        };
        let distance = i32::from(self.snap_units.max(1));
        self.apply_face_extrusion(index, face, distance);
    }

    /// Release: create and select the extruded brush (one undo step).
    fn commit_brush_face_extrude_new(&mut self) -> bool {
        let Some(gesture) = self.brush_extrude_new.take() else {
            return false;
        };
        if gesture.applied <= 0 {
            return true;
        }
        self.apply_face_extrusion(gesture.source, gesture.face, gesture.applied);
        true
    }

    /// End the rotate/scale gesture: one undo step when anything applied.
    pub(crate) fn commit_brush_element_transform(&mut self) -> bool {
        let Some(drag) = self.brush_element_transform.take() else {
            return false;
        };
        let live = self.project.active_scene().brushes[drag.index].clone();
        let live_others: Vec<_> = drag
            .others
            .iter()
            .map(|(index, _)| (*index, self.project.active_scene().brushes[*index].clone()))
            .chain(drag.element_others.iter().map(|member| {
                (
                    member.index,
                    self.project.active_scene().brushes[member.index].clone(),
                )
            }))
            .collect();
        self.project.active_scene_mut().brushes[drag.index] = drag.base;
        for (index, base) in &drag.others {
            self.project.active_scene_mut().brushes[*index] = base.clone();
        }
        for member in &drag.element_others {
            self.project.active_scene_mut().brushes[member.index] = member.base.clone();
        }
        if drag.applied != 0 {
            self.push_undo();
            self.project.active_scene_mut().brushes[drag.index] = live;
            for (index, live) in live_others {
                self.project.active_scene_mut().brushes[index] = live;
            }
            self.mark_dirty();
            self.reconcile_brush_elements();
        }
        true
    }

    /// Drag targets for a grabbed vertex/edge handle: the whole selected
    /// element set when the grabbed element belongs to it, otherwise the
    /// grab replaces the selection first (TrenchBroom behaviour) and
    /// drags alone.
    fn brush_drag_targets_for_grab(
        &mut self,
        grabbed: BrushElement,
        fallback: Vec<[f64; 3]>,
    ) -> (Vec<[f64; 3]>, Vec<usize>) {
        if self.selected_brush_elements.contains(&grabbed) {
            let targets = self.selected_brush_element_targets();
            let faces = self.selected_brush_element_faces();
            if targets.is_empty() {
                (fallback, faces)
            } else {
                (targets, faces)
            }
        } else {
            self.selected_brush_elements = vec![grabbed];
            (fallback, Vec::new())
        }
    }

    /// Drop selected elements that no longer resolve against the primary
    /// brush's current solved geometry (after undo/redo, clips, drags).
    fn reconcile_brush_elements(&mut self) {
        if self.selected_brush_elements.is_empty() {
            return;
        }
        let Some(brush) = self
            .selected_brush
            .and_then(|index| self.project.active_scene().brushes.get(index))
        else {
            self.selected_brush_elements.clear();
            return;
        };
        let solved = brush.solve();
        let vertex_keys: Vec<[i64; 3]> = brush_elements::unique_vertices(&solved)
            .into_iter()
            .map(brush_elements::quantize_element_point)
            .collect();
        let edge_keys: Vec<([i64; 3], [i64; 3])> = brush_elements::unique_edges(&solved)
            .into_iter()
            .map(|(a, b)| brush_elements::edge_element_key(a, b))
            .collect();
        let face_count = brush.faces.len();
        self.selected_brush_elements
            .retain(|element| match element {
                BrushElement::Face(face) => {
                    *face < face_count && solved.polygons.get(*face).is_some_and(Option::is_some)
                }
                BrushElement::Edge(a, b) => edge_keys.contains(&(*a, *b)),
                BrushElement::Vertex(key) => vertex_keys.contains(key),
            });
    }

    /// Shift-click selection: toggle membership. The primary follows the
    /// toggled brush, or the last remaining member after a removal.
    pub(crate) fn toggle_brush_selection(&mut self, index: usize) {
        // Brush-level multi-select moves the primary around; sub-element
        // selection is scoped to one primary and cannot survive that.
        self.selected_brush_faces.clear();
        self.selected_brush_elements.clear();
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
        self.selected_brush_faces.retain(|(brush, face)| {
            self.project
                .active_scene()
                .brushes
                .get(*brush)
                .and_then(|brush| brush.faces.get(*face))
                .is_some()
        });
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
            self.selected_brush_faces.clear();
        } else if self.brush_edit_mode == BrushEditMode::Face
            && !self.selected_brush_faces.is_empty()
        {
            if !self
                .selected_brush
                .zip(self.selected_brush_face)
                .is_some_and(|pair| self.selected_brush_faces.contains(&pair))
            {
                let (brush, face) = *self
                    .selected_brush_faces
                    .last()
                    .expect("non-empty face selection");
                self.selected_brush = Some(brush);
                self.selected_brush_face = Some(face);
            }
            self.selected_brushes = self
                .selected_brush_faces
                .iter()
                .map(|(brush, _)| *brush)
                .collect();
            self.selected_brushes.sort_unstable();
            self.selected_brushes.dedup();
        }
        self.reconcile_brush_elements();
    }

    /// Snap the selected brush's visible solved corners to the editor grid,
    /// as one undo step.
    pub(crate) fn snap_selected_brush(&mut self) {
        let Some(index) = self.selected_brush else {
            return;
        };
        let step = (self.snap_units.max(1)) as i32;
        let Some(current) = self.project.active_scene().brushes.get(index).cloned() else {
            return;
        };
        let Some(snapped) = current.snapped_solved_to_grid(step) else {
            self.status = format!(
                "Snap brush rejected: the {step}-unit grid cannot represent it as a valid solid"
            );
            return;
        };
        if snapped == current {
            self.status = format!("Snap brush: visible corners are already on Grid {step}");
            return;
        }
        self.push_undo();
        self.project.active_scene_mut().brushes[index] = snapped;
        self.mark_dirty();
        self.status = format!("Snapped brush to Grid {step}; one undo step");
    }

    /// Snap every visible BSP brush corner in the active level to the current
    /// absolute grid. This is deliberately atomic: coarse grids can collapse
    /// thin brushes or make a convex plane set invalid, and applying only the
    /// surviving subset would replace small existing seams with much larger
    /// ones. Authors can lower the grid interval and retry without any partial
    /// mutation. A successful repair is one undo step.
    pub(crate) fn snap_all_brushes_to_grid(&mut self) {
        let step = i32::from(self.snap_units.max(1));
        let brushes = &self.project.active_scene().brushes;
        if brushes.is_empty() {
            self.status = "Snap level: no BSP brushes to quantise".to_string();
            return;
        }

        let mut replacements = Vec::new();
        let mut invalid = Vec::new();
        let mut changed_points = 0usize;
        let mut changed_coordinates = 0usize;
        for (index, current) in brushes.iter().enumerate() {
            let Some(snapped) = current.snapped_solved_to_grid(step) else {
                invalid.push(index);
                continue;
            };
            if snapped == *current {
                continue;
            }
            for (before_face, after_face) in current.faces.iter().zip(&snapped.faces) {
                for (before, after) in before_face.points.iter().zip(after_face.points.iter()) {
                    if before != after {
                        changed_points += 1;
                    }
                    changed_coordinates += before
                        .iter()
                        .zip(after)
                        .filter(|(before, after)| **before != **after)
                        .count();
                }
            }
            replacements.push((index, snapped));
        }

        if !invalid.is_empty() {
            let preview = invalid
                .iter()
                .take(8)
                .map(|index| (index + 1).to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let remaining = invalid.len().saturating_sub(8);
            let more = (remaining > 0)
                .then(|| format!(" (+{remaining} more)"))
                .unwrap_or_default();
            self.status = format!(
                "Snap level aborted: the {step}-unit grid cannot represent {} brush{} as valid grid-aligned solids ({preview}{more}); choose a finer grid",
                invalid.len(),
                if invalid.len() == 1 { "" } else { "es" },
            );
            return;
        }
        if replacements.is_empty() {
            self.status = format!("Snap level: every brush is already on the {step}-unit grid");
            return;
        }

        self.push_undo();
        let changed_brushes = replacements.len();
        for (index, snapped) in replacements {
            self.project.active_scene_mut().brushes[index] = snapped;
        }
        self.reconcile_brush_elements();
        self.mark_dirty();
        self.status = format!(
            "Snapped {changed_brushes} brush{} to the {step}-unit grid ({changed_points} plane points, {changed_coordinates} coordinates); one undo step",
            if changed_brushes == 1 { "" } else { "es" },
        );
    }

    /// Cancel every in-flight brush gesture: the create drag, a pending
    /// clip point, and live extrude/move/vertex previews (restoring
    /// their base).
    pub(crate) fn cancel_brush_gestures(&mut self) {
        self.brush_drag = None;
        self.brush_extrude_new = None;
        self.brush_clip_points.clear();
        self.brush_vertex_snap_hover = None;
        if let Some(transform) = self.brush_element_transform.take() {
            if let Some(slot) = self
                .project
                .active_scene_mut()
                .brushes
                .get_mut(transform.index)
            {
                *slot = transform.base;
            }
            for (index, base) in transform.others {
                if let Some(slot) = self.project.active_scene_mut().brushes.get_mut(index) {
                    *slot = base;
                }
            }
            for member in transform.element_others {
                if let Some(slot) = self
                    .project
                    .active_scene_mut()
                    .brushes
                    .get_mut(member.index)
                {
                    *slot = member.base;
                }
            }
        }
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
            let vertex_snap = drag.snap_source.is_some();
            if let Some(slot) = self.project.active_scene_mut().brushes.get_mut(drag.index) {
                *slot = drag.base;
            }
            for (index, base) in drag.others {
                if let Some(slot) = self.project.active_scene_mut().brushes.get_mut(index) {
                    *slot = base;
                }
            }
            for member in drag.element_others {
                if let Some(slot) = self
                    .project
                    .active_scene_mut()
                    .brushes
                    .get_mut(member.index)
                {
                    *slot = member.base;
                }
            }
            if vertex_snap {
                self.status = "Vertex Snap: canceled".to_string();
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
        self.prune_empty_groups();
        self.mark_dirty();
    }

    /// Keyboard for the Brush tool: Escape cancels the in-flight gesture,
    /// Delete or Backspace removes the selected brush. Inert while a text
    /// field owns focus.
    pub(crate) fn brush_tool_keyboard(&mut self, ui: &egui::Ui) {
        // Select with a brush selected owns the same brush keys as the
        // Draw tool: Esc/Delete, plus the Clip mode's Enter and Tab.
        let select_brush_context =
            self.active_tool == ViewTool::Select && self.selected_brush.is_some();
        if self.active_tool != ViewTool::Brush && !select_brush_context {
            return;
        }
        if ui.ctx().memory(|memory| memory.focused().is_some()) {
            return;
        }
        // Arrow nudges and Cmd+R repeat the brush editing chain. Shift is
        // deliberately ignored here: it must never turn an arrow nudge into
        // an implicit duplicate command. Arrows move on the ground plane
        // relative to the camera (Up is away from the camera),
        // PageUp/PageDown move vertically, all by one grid step.
        if self.selected_brush.is_some() && self.floating_geometry.is_none() {
            let step = i32::from(self.snap_units.max(1));
            let (forward, right) = self.camera_ground_axes();
            let delta = ui.input(|input| {
                let mut delta = [0i32; 3];
                let mut add = |axis: [i32; 3], sign: i32| {
                    for component in 0..3 {
                        delta[component] += axis[component] * sign * step;
                    }
                };
                if input.key_pressed(egui::Key::ArrowUp) {
                    add(forward, 1);
                }
                if input.key_pressed(egui::Key::ArrowDown) {
                    add(forward, -1);
                }
                if input.key_pressed(egui::Key::ArrowRight) {
                    add(right, 1);
                }
                if input.key_pressed(egui::Key::ArrowLeft) {
                    add(right, -1);
                }
                if input.key_pressed(egui::Key::PageUp) {
                    add([0, 1, 0], 1);
                }
                if input.key_pressed(egui::Key::PageDown) {
                    add([0, 1, 0], -1);
                }
                delta
            });
            if delta != [0, 0, 0] {
                self.nudge_selected_brushes(delta);
            }
            let repeat =
                ui.input(|input| input.modifiers.command && input.key_pressed(egui::Key::R));
            if repeat {
                self.repeat_brush_actions();
            }
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
        // E extrudes the selected face by one grid step (the free camera
        // deliberately no longer binds Q/E).
        if self.brush_edit_mode == BrushEditMode::Face
            && ui.input(|input| input.key_pressed(egui::Key::E))
        {
            self.extrude_selected_face_one_step();
        }
        if self.brush_edit_mode == BrushEditMode::Clip {
            let (enter, cycle) = ui.input(|input| {
                (
                    input.key_pressed(egui::Key::Enter),
                    // X is the reliable cycle key; Tab also works but
                    // egui's focus traversal eats it in busy layouts.
                    input.key_pressed(egui::Key::X) || input.key_pressed(egui::Key::Tab),
                )
            });
            if enter {
                self.apply_brush_clip();
            }
            if cycle {
                self.brush_clip_keep = self.brush_clip_keep.next();
                self.status = format!("Clip keeps: {}", self.brush_clip_keep.label());
            }
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
        ui.label(
            egui::RichText::new(format!(
                "{} mode: {}. Dragging snaps to {} units.",
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
                    "{selection_count} brushes selected. Move, Rotate, Scale, Size, and material assignment change the full selection."
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
                self.status = if selection_count > 1 {
                    "Size edit rejected: the full brush selection could not be scaled".to_string()
                } else {
                    "Size edit rejected: use Face mode for non-axis-aligned brushes".to_string()
                };
            }
        }
        ui.horizontal_wrapped(|ui| {
            if ui.button("Duplicate").clicked() {
                self.duplicate_selected_brushes();
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

        let owners: Vec<_> = self
            .project
            .active_scene()
            .nodes()
            .iter()
            .filter(|node| {
                matches!(
                    &node.kind,
                    psxed_project::NodeKind::Logic {
                        kind: psxed_project::LogicNodeKind::Door { .. },
                        ..
                    } | psxed_project::NodeKind::Destructible { .. }
                )
            })
            .map(|node| (node.id, format!("{} ({})", node.name, node.kind.label())))
            .collect();
        let mut mover = brush.mover;
        let mover_label = mover
            .and_then(|id| {
                owners
                    .iter()
                    .find(|(candidate, _)| *candidate == id)
                    .map(|(_, name)| name.as_str())
            })
            .unwrap_or(if mover.is_some() {
                "Missing owner"
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
                        for (node, name) in &owners {
                            ui.selectable_value(&mut mover, Some(*node), name);
                        }
                    });
            });
        });
        if mover != brush.mover {
            self.set_selected_brush_mover(mover);
        }
        if brush.mover.is_some() && mover_label == "Missing owner" {
            ui.colored_label(
                egui::Color32::from_rgb(220, 120, 100),
                "The bound brush-model owner no longer exists. Rebind this brush before cooking.",
            );
        }

        ui.separator();
        // Above the per-face early return on purpose: texture lock applies
        // to the whole brush and has to be reachable with no face selected.
        self.draw_brush_texture_mapping_header(ui);
        let whole_brush_material_scope = self.brush_edit_mode == BrushEditMode::Move;
        if whole_brush_material_scope {
            let targets = self.selected_brush_material_targets();
            let first = targets
                .first()
                .and_then(|target| self.material_target_value(*target));
            let mixed = targets
                .iter()
                .skip(1)
                .any(|target| self.material_target_value(*target) != first);
            let scope_label = if selection_count == 1 {
                format!("Material for all {} brush faces", targets.len())
            } else {
                format!(
                    "Material for {} faces across {selection_count} brushes",
                    targets.len()
                )
            };
            ui.label(egui::RichText::new(scope_label).strong());
            let materials = self.project.material_options();
            let mut selected_material = (!mixed).then_some(first).flatten();
            let selected_label = if mixed {
                "Mixed"
            } else {
                selected_material
                    .and_then(|selected| {
                        materials
                            .iter()
                            .find(|(id, _)| *id == selected)
                            .map(|(_, name)| name.as_str())
                    })
                    .unwrap_or("None (flat grey)")
            };
            let changed = searchable_picker(
                ui,
                ui.id().with(("whole-brush-material-picker", index)),
                &mut selected_material,
                selected_label,
                &materials,
                SearchablePickerConfig::optional("None (flat grey)")
                    .with_width(180.0)
                    .with_popup_min_width(360.0)
                    .with_search_hint("Search materials…"),
            );
            if changed {
                self.brush_material = selected_material;
                if let Some(material) = selected_material {
                    self.material_paint_sampling = false;
                    self.replace_resource_selection(material);
                }
                let updated = self.apply_material_to_selected_brush_surfaces();
                self.status = if selection_count == 1 {
                    format!("Assigned material to all {updated} brush faces")
                } else {
                    format!("Assigned material to {updated} faces across {selection_count} brushes")
                };
            }
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button(if selection_count == 1 {
                        "Apply current to brush"
                    } else {
                        "Apply current to selection"
                    })
                    .clicked()
                {
                    let updated = self.apply_material_to_selected_brush_surfaces();
                    self.status = if selection_count == 1 {
                        format!("Assigned material to all {updated} brush faces")
                    } else {
                        format!(
                            "Assigned material to {updated} faces across {selection_count} brushes"
                        )
                    };
                }
                if ui
                    .button(if selection_count == 1 {
                        "Clear brush materials"
                    } else {
                        "Clear selection materials"
                    })
                    .clicked()
                {
                    self.brush_material = None;
                    let updated = self.apply_material_to_selected_brush_surfaces();
                    self.status = if selection_count == 1 {
                        format!("Cleared material from all {updated} brush faces")
                    } else {
                        format!(
                            "Cleared material from {updated} faces across {selection_count} brushes"
                        )
                    };
                }
            });
            return;
        }
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
        // This inspector-local picker edits the selected face directly. The
        // toolbar's brush material remains a separate reusable paint choice,
        // while a successful face edit also updates it for the next stroke.
        let materials = self.project.material_options();
        let mut selected_material = face_data.material;
        let selected_label = selected_material
            .and_then(|selected| {
                materials
                    .iter()
                    .find(|(id, _)| *id == selected)
                    .map(|(_, name)| name.as_str())
            })
            .unwrap_or("None (flat grey)");
        ui.label(icons::text(icons::PALETTE, 14.0).color(STUDIO_TEXT_WEAK))
            .on_hover_text("Face material");
        let material_changed = searchable_picker(
            ui,
            ui.id().with(("brush-face-material-picker", index, face)),
            &mut selected_material,
            selected_label,
            &materials,
            SearchablePickerConfig::optional("None (flat grey)")
                .with_width(180.0)
                .with_popup_min_width(360.0)
                .with_search_hint("Search materials…"),
        );
        if material_changed {
            self.brush_material = selected_material;
            if let Some(material) = selected_material {
                self.material_paint_sampling = false;
                self.replace_resource_selection(material);
            }
            let updated = self.apply_material_to_selected_brush_surfaces();
            self.status = if selection_count > 1 {
                format!("Assigned material to {updated} faces across {selection_count} brushes")
            } else {
                match selected_material {
                    Some(material) => format!(
                        "Assigned {} to brush {} face {}",
                        self.project.resource_name(material).unwrap_or("material"),
                        index + 1,
                        face + 1
                    ),
                    None => format!("Cleared brush {} face {} material", index + 1, face + 1),
                }
            };
        }
        if ui
            .button(icons::label(
                icons::PALETTE,
                if selection_count > 1 {
                    "Apply to selection"
                } else {
                    "Apply to face"
                },
            ))
            .clicked()
        {
            self.apply_material_to_selected_brush_surfaces();
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

    /// Bind the selected brush to one Door or Destructible node, or return it
    /// to static world model zero.
    pub(crate) fn set_selected_brush_mover(&mut self, mover: Option<NodeId>) {
        let targets = self.selected_brush_set();
        if targets.is_empty() {
            return;
        }
        if mover.is_some()
            && targets.iter().any(|&index| {
                self.project
                    .active_scene()
                    .brushes
                    .get(index)
                    .is_some_and(|brush| !brush.contents.is_solid())
            })
        {
            self.status =
                "Liquid brushes are static BSP contents and cannot be assigned to a brush-model owner".to_string();
            return;
        }
        if let Some(mover) = mover {
            let valid = self.project.active_scene().node(mover).is_some_and(|node| {
                matches!(
                    &node.kind,
                    psxed_project::NodeKind::Logic {
                        kind: psxed_project::LogicNodeKind::Door { .. },
                        ..
                    } | psxed_project::NodeKind::Destructible { .. }
                )
            });
            if !valid {
                return;
            }
        }
        if targets.iter().all(|&index| {
            self.project
                .active_scene()
                .brushes
                .get(index)
                .is_some_and(|brush| brush.mover == mover)
        }) {
            return;
        }
        self.push_undo();
        let mut changed = 0usize;
        for index in targets {
            if let Some(brush) = self.project.active_scene_mut().brushes.get_mut(index) {
                brush.mover = mover;
                changed += 1;
            }
        }
        self.mark_dirty();
        self.status = match mover {
            Some(_) => format!(
                "Assigned {changed} brush{} to model owner",
                if changed == 1 { "" } else { "es" }
            ),
            None => format!(
                "Returned {changed} brush{} to the static world",
                if changed == 1 { "" } else { "es" }
            ),
        };
    }

    /// Duplicate every selected brush in place and select the copies. One
    /// undo step; the primary selection follows its own copy.
    pub(crate) fn duplicate_selected_brushes(&mut self) {
        self.push_undo();
        if self.duplicate_selected_brushes_offset([0, 0, 0]) {
            self.record_brush_repeat(BrushRepeatAction::Duplicate([0, 0, 0]));
        }
    }

    /// Core duplicate-with-offset; no undo push, no repeat recording.
    fn duplicate_selected_brushes_offset(&mut self, offset: [i32; 3]) -> bool {
        let targets = self.selected_brush_set();
        if targets.is_empty() {
            return false;
        }
        let primary = self.selected_brush;
        let texture_lock = self.brush_texture_lock;
        let mut new_primary = None;
        let mut new_selection = Vec::new();
        for &index in &targets {
            let mut copy = self.project.active_scene().brushes[index].clone();
            if texture_lock {
                copy.translate_with_uv_lock(offset, psxed_project::brush::BRUSH_UV_UNITS_PER_TEXEL);
            } else {
                copy.translate(offset);
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
        self.selected_brush_faces.clear();
        self.selected_brush_elements.clear();
        self.mark_dirty();
        true
    }

    /// Nudge every selected brush by `delta` world units, honouring
    /// texture lock. One undo step; records for Cmd+R.
    pub(crate) fn nudge_selected_brushes(&mut self, delta: [i32; 3]) -> bool {
        if self.selected_brush_set().is_empty() {
            return false;
        }
        self.push_undo();
        if !self.nudge_selected_brushes_impl(delta) {
            return false;
        }
        self.record_brush_repeat(BrushRepeatAction::Nudge(delta));
        self.status = format!("Nudged by [{} {} {}]", delta[0], delta[1], delta[2]);
        true
    }

    fn nudge_selected_brushes_impl(&mut self, delta: [i32; 3]) -> bool {
        let targets = self.selected_brush_set();
        if targets.is_empty() {
            return false;
        }
        let texture_lock = self.brush_texture_lock;
        let scene = self.project.active_scene_mut();
        for &index in &targets {
            let Some(brush) = scene.brushes.get_mut(index) else {
                continue;
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

    fn record_brush_repeat(&mut self, action: BrushRepeatAction) {
        self.brush_repeat_chain.push(action);
    }

    /// Cmd+R: replay every recorded duplicate/nudge since the chain was
    /// last reset, as one undo step.
    pub(crate) fn repeat_brush_actions(&mut self) -> bool {
        let chain = self.brush_repeat_chain.clone();
        if chain.is_empty() || self.selected_brush_set().is_empty() {
            self.status = "Nothing to repeat".to_string();
            return false;
        }
        self.push_undo();
        let mut applied = false;
        for action in &chain {
            applied |= match *action {
                BrushRepeatAction::Duplicate(delta) => {
                    self.duplicate_selected_brushes_offset(delta)
                }
                BrushRepeatAction::Nudge(delta) => self.nudge_selected_brushes_impl(delta),
            };
        }
        if applied {
            self.status = format!(
                "Repeated {} action{}",
                chain.len(),
                if chain.len() == 1 { "" } else { "s" }
            );
        }
        applied
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
                        .range(-1600..=1600)
                        .suffix("% U"),
                ),
            );
            scale_live |= live(
                &ui.add(
                    egui::DragValue::new(&mut scale_v)
                        .speed(1)
                        .range(-1600..=1600)
                        .suffix("% V"),
                ),
            );
            edited.offset_texels = [
                off_u.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
                off_v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
            ];
            edited.rotation_deg = rot as i16;
            // Negative percentages mirror the axis (Flip H/V); zero is
            // nudged to the smallest positive step.
            let percent_to_q8 = |percent: i32| -> i16 {
                let q8 = (percent * 256 / 100).clamp(i32::from(i16::MIN) + 1, i32::from(i16::MAX))
                    as i16;
                if q8 == 0 {
                    1
                } else {
                    q8
                }
            };
            edited.scale_q8 = [percent_to_q8(scale_u), percent_to_q8(scale_v)];
            // Uniquely labelled: the Inspector shows several bare "Reset"
            // buttons, and this one only ever restores the face UV mapping.
            // Folded in after the DragValues so a Reset wins over them.
            if ui.button("Reset UV").clicked() {
                edited = psxed_project::brush::FaceUv::default();
                reset = true;
            }
            scale_live
        });
        // TrenchBroom-style canvas below the numeric rows: the face
        // projected into repeating texture space. While one of its
        // gestures is live it owns the mapping for the frame.
        let canvas_edit = self.draw_face_uv_canvas(ui, index, face, current);
        let mut interacting = rot_live || scale_live.inner;
        if let Some((canvas_uv, canvas_live)) = canvas_edit {
            edited = canvas_uv;
            reset = false;
            interacting = canvas_live;
        }
        let edited = self.apply_face_uv_edit(index, face, current, edited, reset, interacting);
        if edited != current {
            self.project.active_scene_mut().brushes[index].faces[face].uv = edited;
            self.mark_dirty();
        }
        ui.horizontal(|ui| {
            ui.label("UV align");
            for (label, action, hint) in [
                ("L", UvAlign::Left, "Tile seam at the face's left edge"),
                ("R", UvAlign::Right, "Tile seam at the face's right edge"),
                ("T", UvAlign::Top, "Tile seam at the face's top edge"),
                ("B", UvAlign::Bottom, "Tile seam at the face's bottom edge"),
                ("C", UvAlign::Center, "Texture centre on the face centre"),
                ("Fit", UvAlign::Fit, "One texture repeat spans the face"),
            ] {
                if ui.small_button(label).on_hover_text(hint).clicked() {
                    self.justify_selected_face_uv(action);
                }
            }
            if ui
                .small_button("Flip H")
                .on_hover_text("Mirror the texture horizontally in place")
                .clicked()
            {
                self.flip_selected_face_uv(0);
            }
            if ui
                .small_button("Flip V")
                .on_hover_text("Mirror the texture vertically in place")
                .clicked()
            {
                self.flip_selected_face_uv(1);
            }
        });
    }

    /// TrenchBroom's copy-face-attributes: paint `target` with the
    /// selected face's material and UV mapping (Alt+click in Face mode).
    pub(crate) fn apply_face_attributes_to(
        &mut self,
        target_brush: usize,
        target_face: usize,
    ) -> bool {
        let (Some(source_brush), Some(source_face)) =
            (self.selected_brush, self.selected_brush_face)
        else {
            return false;
        };
        if (source_brush, source_face) == (target_brush, target_face) {
            return false;
        }
        let scene = self.project.active_scene();
        let Some((material, uv)) = scene
            .brushes
            .get(source_brush)
            .and_then(|brush| brush.faces.get(source_face))
            .map(|face| (face.material, face.uv))
        else {
            return false;
        };
        let Some(target) = scene
            .brushes
            .get(target_brush)
            .and_then(|brush| brush.faces.get(target_face))
        else {
            return false;
        };
        if target.material == material && target.uv == uv {
            self.status = "Face already has these attributes".to_string();
            return true;
        }
        self.push_undo();
        let face = &mut self.project.active_scene_mut().brushes[target_brush].faces[target_face];
        face.material = material;
        face.uv = uv;
        self.clear_uv_edit_transaction();
        self.mark_dirty();
        self.status = "Applied the selected face's material and UV".to_string();
        true
    }

    /// Select every brush with at least one face using `id`.
    pub(crate) fn select_brushes_using_material(&mut self, id: psxed_project::ResourceId) -> usize {
        let indices: Vec<usize> = self
            .project
            .active_scene()
            .brushes
            .iter()
            .enumerate()
            .filter(|(_, brush)| brush.faces.iter().any(|face| face.material == Some(id)))
            .map(|(index, _)| index)
            .collect();
        if indices.is_empty() {
            self.status = "No brush uses this material".to_string();
            return 0;
        }
        self.clear_node_selection_state();
        self.clear_resource_selection_state();
        self.clear_primitive_selection_state();
        self.clear_sector_selection();
        self.selected_brush = indices.first().copied();
        self.selected_brushes = indices.clone();
        self.selected_brush_face = None;
        self.selected_brush_faces.clear();
        self.selected_brush_elements.clear();
        self.status = format!(
            "Selected {} brush{} using the material",
            indices.len(),
            if indices.len() == 1 { "" } else { "es" }
        );
        indices.len()
    }

    /// Replace every brush-face use of `from` with `to`, one undo step.
    pub(crate) fn replace_material_uses(
        &mut self,
        from: psxed_project::ResourceId,
        to: psxed_project::ResourceId,
    ) -> usize {
        if from == to {
            return 0;
        }
        let count: usize = self
            .project
            .active_scene()
            .brushes
            .iter()
            .flat_map(|brush| brush.faces.iter())
            .filter(|face| face.material == Some(from))
            .count();
        if count == 0 {
            self.status = "No brush face uses this material".to_string();
            return 0;
        }
        self.push_undo();
        for brush in &mut self.project.active_scene_mut().brushes {
            for face in &mut brush.faces {
                if face.material == Some(from) {
                    face.material = Some(to);
                }
            }
        }
        self.clear_uv_edit_transaction();
        self.mark_dirty();
        self.status = format!(
            "Replaced the material on {count} face{}",
            if count == 1 { "" } else { "s" }
        );
        count
    }

    /// Raw paraxial texel coordinates of the selected face's polygon.
    fn face_raw_uv_polygon(&self, index: usize, face: usize) -> Option<Vec<[f64; 2]>> {
        use psxed_project::brush::{paraxial_uv, Plane, BRUSH_UV_UNITS_PER_TEXEL};
        let brush = self.project.active_scene().brushes.get(index)?;
        let solved = brush.solve();
        let polygon = solved.polygons.get(face)?.as_ref()?;
        let plane = Plane::from_points(brush.faces.get(face)?.points)?;
        Some(
            polygon
                .verts
                .iter()
                .map(|&vertex| {
                    let raw = paraxial_uv(&plane, vertex);
                    [
                        raw[0] / BRUSH_UV_UNITS_PER_TEXEL,
                        raw[1] / BRUSH_UV_UNITS_PER_TEXEL,
                    ]
                })
                .collect(),
        )
    }

    /// Texture repeat size in texels for a face's material; 64x64 when
    /// the material has no decoded thumbnail (matches the preview's
    /// default window).
    fn face_texture_dims(&self, index: usize, face: usize) -> [f64; 2] {
        self.project
            .active_scene()
            .brushes
            .get(index)
            .and_then(|brush| brush.faces.get(face))
            .and_then(|face| face.material)
            .and_then(|id| self.texture_thumbs.get(&id))
            .map(|entry| {
                [
                    f64::from(entry.stats.width.max(8)),
                    f64::from(entry.stats.height.max(8)),
                ]
            })
            .unwrap_or([64.0, 64.0])
    }

    /// Write a face UV directly (justify/fit/flip buttons): these solve
    /// their own offsets, so the reanchoring transaction must not run.
    fn set_selected_face_uv(
        &mut self,
        index: usize,
        face: usize,
        uv: psxed_project::brush::FaceUv,
    ) {
        self.clear_uv_edit_transaction();
        self.project.active_scene_mut().brushes[index].faces[face].uv = uv;
        self.mark_dirty();
    }

    /// TrenchBroom-style justify: align the texture's tile seam (or its
    /// centre) with the face polygon's UV-space bounds.
    pub(crate) fn justify_selected_face_uv(&mut self, action: UvAlign) {
        let (Some(index), Some(face)) = (self.selected_brush, self.selected_brush_face) else {
            return;
        };
        let Some(polygon) = self.face_raw_uv_polygon(index, face) else {
            return;
        };
        let current = self.project.active_scene().brushes[index].faces[face].uv;
        let dims = self.face_texture_dims(index, face);
        let mut edited = current;
        if action == UvAlign::Fit {
            // One texture repeat spans the face: solve scale from the
            // rotation-only span, then pin the min corner to the seam.
            let rotation_only = psxed_project::brush::FaceUv {
                offset_texels: [0, 0],
                rotation_deg: current.rotation_deg,
                scale_q8: [256, 256],
            };
            for (axis, dimension) in dims.iter().enumerate() {
                let mapped = polygon
                    .iter()
                    .map(|&uv| rotation_only.apply_linear(uv)[axis]);
                let min = mapped.clone().fold(f64::MAX, f64::min);
                let max = mapped.fold(f64::MIN, f64::max);
                let span = (max - min).max(1e-6);
                edited.scale_q8[axis] = ((span / *dimension) * 256.0)
                    .round()
                    .clamp(1.0, f64::from(i16::MAX)) as i16;
            }
        }
        let linear: Vec<[f64; 2]> = polygon.iter().map(|&uv| edited.apply_linear(uv)).collect();
        for axis in 0..2 {
            let min = linear.iter().map(|uv| uv[axis]).fold(f64::MAX, f64::min);
            let max = linear.iter().map(|uv| uv[axis]).fold(f64::MIN, f64::max);
            let target = match (action, axis) {
                (UvAlign::Fit, _) => Some(-min),
                (UvAlign::Left, 0) | (UvAlign::Top, 1) => Some(-min),
                (UvAlign::Right, 0) | (UvAlign::Bottom, 1) => Some(-max),
                (UvAlign::Center, _) => Some(dims[axis] * 0.5 - (min + max) * 0.5),
                _ => None,
            };
            if let Some(offset) = target {
                edited.offset_texels[axis] = offset
                    .round()
                    .clamp(f64::from(i16::MIN), f64::from(i16::MAX))
                    as i16;
            }
        }
        if edited != current {
            self.set_selected_face_uv(index, face, edited);
            self.status = format!("UV {}", action.label());
        }
    }

    /// Mirror the texture along one UV axis about the face's anchor, so
    /// it flips in place instead of sliding.
    pub(crate) fn flip_selected_face_uv(&mut self, axis: usize) {
        let (Some(index), Some(face)) = (self.selected_brush, self.selected_brush_face) else {
            return;
        };
        let Some(anchor) = self
            .project
            .active_scene()
            .brushes
            .get(index)
            .and_then(|brush| brush.face_uv_anchor(face))
        else {
            return;
        };
        let current = self.project.active_scene().brushes[index].faces[face].uv;
        let mut edited = current;
        edited.scale_q8[axis] = match edited.scale_q8[axis].checked_neg() {
            Some(0) | None => -1,
            Some(negated) => negated,
        };
        edited.reanchor(&current, anchor, [0.0, 0.0]);
        self.set_selected_face_uv(index, face, edited);
        self.status = format!("UV flipped {}", if axis == 0 { "H" } else { "V" });
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
        self.brush_uv_canvas_drag = None;
    }

    /// Solved axis-aligned min corner of the complete brush selection,
    /// rounded to integers: the numeric inspector's "Origin".
    pub(crate) fn selected_brush_origin(&self) -> Option<[i32; 3]> {
        self.selected_brush_bounds()
            .map(|(min, _)| min.map(|value| value.round() as i32))
    }

    /// Numeric fallback for whole-brush placement: translate the selected
    /// brush set so its shared solved min corner lands exactly on `origin`. No grid
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

    /// Solved axis-aligned bounding-box size of the complete selection shown
    /// by the numeric inspector.
    pub(crate) fn selected_brush_size(&self) -> Option<[i32; 3]> {
        self.selected_brush_bounds().map(|(min, max)| {
            std::array::from_fn(|axis| (max[axis] - min[axis]).round().max(1.0) as i32)
        })
    }

    /// Exact numeric size fallback. A single ordinary axis-aligned brush keeps
    /// its historical face-slide semantics. A multi-selection is scaled as
    /// one object about its shared minimum corner, preserving every member's
    /// relative spacing and proportions.
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
        let targets = self.selected_brush_set();
        if targets.len() > 1 {
            let Some((min, max)) = self.selected_brush_bounds() else {
                return false;
            };
            let mut map = [[0.0; 3]; 3];
            for axis in 0..3 {
                let extent = max[axis] - min[axis];
                if !extent.is_finite() || extent <= f64::EPSILON {
                    return false;
                }
                map[axis][axis] = f64::from(size[axis]) / extent;
            }
            let mut previews = Vec::with_capacity(targets.len());
            for index in targets {
                let Some(mut preview) = self.project.active_scene().brushes.get(index).cloned()
                else {
                    return false;
                };
                let faces: Vec<usize> = (0..preview.faces.len()).collect();
                if preview.transform_selected(&faces, &[], min, map, 0.5) == 0
                    || !brush_preview_ok(&preview)
                {
                    return false;
                }
                previews.push((index, preview));
            }
            let scene = self.project.active_scene_mut();
            for (index, preview) in previews {
                scene.brushes[index] = preview;
            }
            self.mark_dirty();
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
            if plane.normal[axis] > 0 && positive_faces[axis].replace(face_index).is_some() {
                return false;
            }
        }
        if positive_faces.iter().any(Option::is_none) {
            return false;
        }
        for axis in 0..3 {
            let mut delta = [0; 3];
            delta[axis] = size[axis] - current[axis];
            translate_face_locked(
                &mut edited,
                positive_faces[axis].unwrap(),
                delta,
                self.brush_texture_lock,
            );
        }
        if !brush_preview_ok(&edited) {
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
        translate_face_locked(&mut edited, face, delta, self.brush_texture_lock);
        if !brush_preview_ok(&edited) {
            return false;
        }
        self.project.active_scene_mut().brushes[index] = edited;
        self.mark_dirty();
        true
    }

    /// Apply the paint material to the brush surfaces implied by the current
    /// selection as one undo step. A single selected face remains precise;
    /// whole-brush and multi-brush selections cover every selected brush face.
    pub(crate) fn apply_material_to_selected_brush_surfaces(&mut self) -> usize {
        let targets = self.selected_brush_material_targets();
        if targets.is_empty() {
            return 0;
        }
        let material = self.brush_material;
        if targets
            .iter()
            .all(|target| self.material_target_value(*target) == material)
        {
            return 0;
        }
        self.push_undo();
        let updated = targets
            .into_iter()
            .filter(|target| self.assign_material_target_no_undo(*target, material))
            .count();
        if updated > 0 {
            self.mark_dirty();
        }
        updated
    }

    /// Axis-aligned bounds a primitive drag would commit, if it has volume.
    fn brush_drag_bounds(drag: BrushDrag) -> Option<([i32; 3], [i32; 3])> {
        let mut opposite = drag.current;
        let depth_axis = drag.view.depth_axis();
        opposite[depth_axis] = drag.height_end;
        let min = std::array::from_fn(|axis| drag.anchor[axis].min(opposite[axis]));
        let max = std::array::from_fn(|axis| drag.anchor[axis].max(opposite[axis]));
        (0..3)
            .all(|axis| min[axis] < max[axis])
            .then_some((min, max))
    }

    fn primitive_snap(value: f64, step: i32) -> i32 {
        let step = f64::from(step.max(1));
        ((value / step).round() * step).clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
    }

    fn primitive_thickness(settings: BrushDrawSettings, step: i32) -> i32 {
        Self::primitive_snap(f64::from(settings.arch_thickness.max(1)), step).max(step.max(1))
    }

    fn brush_drag_ramp(
        min: [i32; 3],
        max: [i32; 3],
        direction: BrushCardinalDirection,
    ) -> Option<psxed_project::brush::Brush> {
        use BrushCardinalDirection::{East, North, South, West};
        match direction {
            East => psxed_project::brush::Brush::convex_prism(
                &[[min[0], min[1]], [max[0], min[1]], [max[0], max[1]]],
                [0, 1],
                2,
                [min[2], max[2]],
            ),
            West => psxed_project::brush::Brush::convex_prism(
                &[[min[0], min[1]], [max[0], min[1]], [min[0], max[1]]],
                [0, 1],
                2,
                [min[2], max[2]],
            ),
            South => psxed_project::brush::Brush::convex_prism(
                &[[min[2], min[1]], [max[2], min[1]], [max[2], max[1]]],
                [2, 1],
                0,
                [min[0], max[0]],
            ),
            North => psxed_project::brush::Brush::convex_prism(
                &[[min[2], min[1]], [max[2], min[1]], [min[2], max[1]]],
                [2, 1],
                0,
                [min[0], max[0]],
            ),
        }
    }

    fn brush_drag_cylinder(
        min: [i32; 3],
        max: [i32; 3],
        sides: u8,
        step: i32,
    ) -> Option<psxed_project::brush::Brush> {
        let sides = usize::from(sides.clamp(3, 32));
        let center_x = (f64::from(min[0]) + f64::from(max[0])) * 0.5;
        let center_z = (f64::from(min[2]) + f64::from(max[2])) * 0.5;
        let radius_x = f64::from(max[0] - min[0]) * 0.5;
        let radius_z = f64::from(max[2] - min[2]) * 0.5;
        let polygon: Vec<_> = (0..sides)
            .map(|index| {
                let angle = std::f64::consts::TAU * index as f64 / sides as f64;
                [
                    Self::primitive_snap(center_x + radius_x * angle.cos(), step),
                    Self::primitive_snap(center_z + radius_z * angle.sin(), step),
                ]
            })
            .collect();
        psxed_project::brush::Brush::convex_prism(&polygon, [0, 2], 1, [min[1], max[1]])
    }

    fn brush_drag_doorway_arch(
        min: [i32; 3],
        max: [i32; 3],
        settings: BrushDrawSettings,
        step: i32,
    ) -> Vec<psxed_project::brush::Brush> {
        let facing_x = matches!(
            settings.direction,
            BrushCardinalDirection::East | BrushCardinalDirection::West
        );
        let width_axis = if facing_x { 2 } else { 0 };
        let depth_axis = if facing_x { 0 } else { 2 };
        let width_min = min[width_axis];
        let width_max = max[width_axis];
        let center = (f64::from(width_min) + f64::from(width_max)) * 0.5;
        let radius = f64::from(width_max - width_min) * 0.5;
        let vertical_radius = radius.min(f64::from(max[1] - min[1]));
        let thickness = f64::from(Self::primitive_thickness(settings, step));
        if radius <= thickness || vertical_radius <= thickness {
            return Vec::new();
        }
        let spring = f64::from(max[1]) - vertical_radius;
        let inner_radius = radius - thickness;
        let inner_vertical_radius = vertical_radius - thickness;
        let plane_axes = [width_axis, 1];
        let depth = [min[depth_axis], max[depth_axis]];
        let segments = usize::from(settings.arch_segments.clamp(2, 24));
        let point = |angle: f64, horizontal_radius: f64, vertical_radius: f64| {
            [
                Self::primitive_snap(center + horizontal_radius * angle.cos(), step),
                Self::primitive_snap(spring + vertical_radius * angle.sin(), step),
            ]
        };
        let mut brushes = Vec::with_capacity(segments + 2);
        for index in 0..segments {
            let a0 = std::f64::consts::PI * index as f64 / segments as f64;
            let a1 = std::f64::consts::PI * (index + 1) as f64 / segments as f64;
            let polygon = [
                point(a0, inner_radius, inner_vertical_radius),
                point(a0, radius, vertical_radius),
                point(a1, radius, vertical_radius),
                point(a1, inner_radius, inner_vertical_radius),
            ];
            if let Some(brush) =
                psxed_project::brush::Brush::convex_prism(&polygon, plane_axes, depth_axis, depth)
            {
                brushes.push(brush);
            }
        }

        let spring = Self::primitive_snap(spring, step);
        let inner_left = Self::primitive_snap(center - inner_radius, step);
        let inner_right = Self::primitive_snap(center + inner_radius, step);
        for polygon in [
            [
                [width_min, min[1]],
                [inner_left, min[1]],
                [inner_left, spring],
                [width_min, spring],
            ],
            [
                [inner_right, min[1]],
                [width_max, min[1]],
                [width_max, spring],
                [inner_right, spring],
            ],
        ] {
            if let Some(brush) =
                psxed_project::brush::Brush::convex_prism(&polygon, plane_axes, depth_axis, depth)
            {
                brushes.push(brush);
            }
        }
        brushes
    }

    fn brush_drag_curved_wall(
        min: [i32; 3],
        max: [i32; 3],
        settings: BrushDrawSettings,
        step: i32,
    ) -> Vec<psxed_project::brush::Brush> {
        let radius_x = f64::from(max[0] - min[0]) * 0.5;
        let radius_z = f64::from(max[2] - min[2]) * 0.5;
        let thickness = f64::from(Self::primitive_thickness(settings, step));
        if radius_x <= thickness || radius_z <= thickness {
            return Vec::new();
        }
        let center_x = (f64::from(min[0]) + f64::from(max[0])) * 0.5;
        let center_z = (f64::from(min[2]) + f64::from(max[2])) * 0.5;
        let inner_x = radius_x - thickness;
        let inner_z = radius_z - thickness;
        let arc = f64::from(settings.curved_wall_arc_degrees.clamp(90, 360)).to_radians();
        let facing = match settings.direction {
            BrushCardinalDirection::North => -std::f64::consts::FRAC_PI_2,
            BrushCardinalDirection::East => 0.0,
            BrushCardinalDirection::South => std::f64::consts::FRAC_PI_2,
            BrushCardinalDirection::West => std::f64::consts::PI,
        };
        let start = facing - arc * 0.5;
        let segments = usize::from(settings.arch_segments.clamp(2, 32));
        let point = |angle: f64, rx: f64, rz: f64| {
            [
                Self::primitive_snap(center_x + rx * angle.cos(), step),
                Self::primitive_snap(center_z + rz * angle.sin(), step),
            ]
        };
        let mut brushes = Vec::with_capacity(segments);
        for index in 0..segments {
            let a0 = start + arc * index as f64 / segments as f64;
            let a1 = start + arc * (index + 1) as f64 / segments as f64;
            let polygon = [
                point(a0, inner_x, inner_z),
                point(a0, radius_x, radius_z),
                point(a1, radius_x, radius_z),
                point(a1, inner_x, inner_z),
            ];
            if let Some(brush) =
                psxed_project::brush::Brush::convex_prism(&polygon, [0, 2], 1, [min[1], max[1]])
            {
                brushes.push(brush);
            }
        }
        brushes
    }

    fn brush_drag_stairs(
        min: [i32; 3],
        max: [i32; 3],
        settings: BrushDrawSettings,
        step: i32,
    ) -> Vec<psxed_project::brush::Brush> {
        let (run_axis, positive) = match settings.direction {
            BrushCardinalDirection::North => (2, false),
            BrushCardinalDirection::East => (0, true),
            BrushCardinalDirection::South => (2, true),
            BrushCardinalDirection::West => (0, false),
        };
        let run = max[run_axis] - min[run_axis];
        let rise = max[1] - min[1];
        let available_steps = (run / step.max(1)).max(1) as usize;
        let steps = usize::from(settings.stair_steps.clamp(1, 32)).min(available_steps);
        let mut brushes = Vec::with_capacity(steps);
        for index in 0..steps {
            let run0 = Self::primitive_snap(
                f64::from(min[run_axis]) + f64::from(run) * index as f64 / steps as f64,
                step,
            );
            let run1 = Self::primitive_snap(
                f64::from(min[run_axis]) + f64::from(run) * (index + 1) as f64 / steps as f64,
                step,
            );
            let level = if positive { index + 1 } else { steps - index };
            let top = Self::primitive_snap(
                f64::from(min[1]) + f64::from(rise) * level as f64 / steps as f64,
                step,
            );
            let mut step_min = min;
            let mut step_max = max;
            step_min[run_axis] = run0;
            step_max[run_axis] = run1;
            step_max[1] = top;
            if let Some(brush) =
                psxed_project::brush::Brush::cuboid_from_corners(step_min, step_max)
            {
                brushes.push(brush);
            }
        }
        brushes
    }

    /// Ordinary convex brushes a primitive drag would commit. Concave presets
    /// deliberately expand to several brushes so every existing edit, CSG and
    /// cooker path continues to operate on the native brush representation.
    fn brush_drag_brushes(drag: BrushDrag) -> Vec<psxed_project::brush::Brush> {
        let Some((min, max)) = Self::brush_drag_bounds(drag) else {
            return Vec::new();
        };
        let settings = drag.settings;
        let step = drag.grid_step.max(1);
        match settings.shape {
            BrushDrawShape::Box => vec![psxed_project::brush::Brush::cuboid(min, max)],
            BrushDrawShape::Ramp => Self::brush_drag_ramp(min, max, settings.direction)
                .into_iter()
                .collect(),
            BrushDrawShape::Cylinder => {
                Self::brush_drag_cylinder(min, max, settings.cylinder_sides, step)
                    .into_iter()
                    .collect()
            }
            BrushDrawShape::DoorwayArch => Self::brush_drag_doorway_arch(min, max, settings, step),
            BrushDrawShape::CurvedWall => Self::brush_drag_curved_wall(min, max, settings, step),
            BrushDrawShape::Stairs => Self::brush_drag_stairs(min, max, settings, step),
        }
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
        self.clear_brush_selection();
        let point = self.brush_snap_2d(world);
        let depth_axis = self.orthographic_view.depth_axis();
        self.brush_drag = Some(BrushDrag {
            anchor: point,
            current: point,
            view: self.orthographic_view,
            grid_step: i32::from(self.snap_units.max(1)),
            height_end: point[depth_axis].saturating_add(BRUSH_CREATE_HEIGHT),
            stage: BrushCreateStage::Footprint,
            height_press_y: 0,
            height_press_end: 0,
            height_dragging: false,
            settings: self.brush_draw_settings,
        });
    }

    /// Update the in-flight brush-create drag (2D view entry).
    pub(crate) fn update_brush_drag_2d(&mut self, world: [f32; 2]) {
        if let Some(drag) = self
            .brush_drag
            .filter(|drag| drag.stage == BrushCreateStage::Footprint)
        {
            self.brush_drag = Some(BrushDrag {
                current: self.brush_snap_2d(world),
                ..drag
            });
        }
    }

    /// Begin the second brush-create gesture. Vertical pointer travel authors
    /// the hidden-axis endpoint, which is world Y for the Top view used by the
    /// 3D editor.
    pub(crate) fn begin_brush_height_drag(&mut self, pointer_y: f32) -> bool {
        let Some(mut drag) = self.brush_drag else {
            return false;
        };
        if drag.stage != BrushCreateStage::Height || drag.height_dragging {
            return false;
        }
        drag.height_press_y = pointer_y.round() as i32;
        drag.height_press_end = drag.height_end;
        drag.height_dragging = true;
        self.brush_drag = Some(drag);
        self.status = format!(
            "Draw {}: drag vertically to set height, then release",
            drag.settings.shape.label()
        );
        true
    }

    /// Update the snapped endpoint during the second brush-create gesture.
    pub(crate) fn update_brush_height_drag(&mut self, pointer_y: f32) {
        let Some(mut drag) = self
            .brush_drag
            .filter(|drag| drag.stage == BrushCreateStage::Height && drag.height_dragging)
        else {
            return;
        };
        let raw_delta = f64::from(drag.height_press_y) - f64::from(pointer_y);
        let world_delta = raw_delta * f64::from(EXTRUDE_UNITS_PER_PIXEL);
        let snapped_delta = absolute_grid_translation_delta(
            f64::from(drag.height_press_end),
            world_delta,
            self.snap_units,
        );
        drag.height_end = drag.height_press_end.saturating_add(snapped_delta);
        let depth_axis = drag.view.depth_axis();
        let extent = drag.height_end.saturating_sub(drag.anchor[depth_axis]);
        self.brush_drag = Some(drag);
        self.status = format!(
            "Draw {}: extent {extent:+} on Grid {}",
            drag.settings.shape.label(),
            self.snap_units
        );
    }

    /// Finish one of the two brush-create gestures. A valid footprint advances
    /// to height authoring; releasing the height gesture commits the brushes.
    fn finish_brush_creation_gesture(&mut self) -> bool {
        let Some(mut drag) = self.brush_drag else {
            return false;
        };
        match drag.stage {
            BrushCreateStage::Footprint => {
                if Self::brush_drag_bounds(drag).is_none() {
                    self.brush_drag = None;
                    self.status = format!(
                        "Draw {}: drag a larger footprint",
                        drag.settings.shape.label()
                    );
                    return true;
                }
                drag.stage = BrushCreateStage::Height;
                drag.height_dragging = false;
                self.brush_drag = Some(drag);
                self.status = format!(
                    "Draw {}: footprint set; drag vertically to set height",
                    drag.settings.shape.label()
                );
                true
            }
            BrushCreateStage::Height if drag.height_dragging => {
                self.commit_brush_drag();
                true
            }
            BrushCreateStage::Height => false,
        }
    }

    /// Commit the in-flight create drag as ordinary brushes, one undo step.
    /// Shared by the 3D tool release and the 2D view release.
    pub(crate) fn commit_brush_drag(&mut self) {
        let Some(drag) = self.brush_drag.take() else {
            return;
        };
        let mut brushes = Self::brush_drag_brushes(drag);
        if brushes.is_empty() {
            self.status = format!(
                "Draw {}: drag a larger footprint or reduce segments/thickness",
                drag.settings.shape.label()
            );
            return;
        }
        // New brushes should be cookable and visibly textured immediately.
        // This follows the same material resolution as the grid tools: an
        // explicit picker/selected resource wins, then the first project
        // material.
        if let Some(material) = self.paint_material_for("brush") {
            for brush in &mut brushes {
                for face in &mut brush.faces {
                    face.material = Some(material);
                }
            }
        }
        self.push_undo();
        let first = self.project.active_scene().brushes.len();
        if drag.settings.shape.is_multi_brush() && brushes.len() > 1 {
            let parent = self
                .open_group
                .filter(|group| self.node_is_group(*group))
                .unwrap_or(self.project.active_scene().root);
            let group = self.project.active_scene_mut().add_node(
                parent,
                drag.settings.shape.label(),
                NodeKind::Group,
            );
            for brush in &mut brushes {
                brush.group = Some(group);
            }
            self.project.active_scene_mut().brushes.extend(brushes);
            self.clear_brush_selection();
            self.replace_node_selection(group);
        } else {
            self.project.active_scene_mut().brushes.extend(brushes);
            self.replace_brush_selection(first, None);
        }
        self.mark_dirty();
        self.status = format!("Created {}", drag.settings.shape.label());
    }

    /// One clip click at a snapped ground point: the first click stores
    /// the point, the second splits the selected brush by the vertical
    /// plane through both, honouring `brush_clip_keep`. Shared by the 3D
    /// tool and the 2D view.
    pub(crate) fn brush_clip_click(&mut self, point: [i32; 3]) {
        self.brush_clip_click_in_view(point, self.orthographic_view);
    }

    fn brush_clip_click_in_view(&mut self, point: [i32; 3], view: OrthographicView) {
        if self.selected_brush.is_none() {
            return;
        }
        let mut normal = [0.0; 3];
        normal[view.depth_axis()] = 1.0;
        self.push_brush_clip_point(BrushClipPoint { point, normal });
    }

    /// Append a clip point (max 3, duplicates ignored) and narrate.
    fn push_brush_clip_point(&mut self, clip_point: BrushClipPoint) {
        if self
            .brush_clip_points
            .iter()
            .any(|existing| existing.point == clip_point.point)
        {
            return;
        }
        if self.brush_clip_points.len() >= 3 {
            return;
        }
        self.brush_clip_points.push(clip_point);
        self.status = format!(
            "Clip point {}/3 placed; Enter cuts ({}), X flips, Esc clears",
            self.brush_clip_points.len(),
            self.brush_clip_keep.label(),
        );
    }

    /// 3D clip point placement: grid-snapped hit on the brush face under
    /// the pointer (its normal makes two-point cuts perpendicular to the
    /// surface, so sloped cuts work), falling back to the ground plane.
    fn brush_clip_click_3d(&mut self, rect: egui::Rect, pointer: egui::Pos2) {
        if let Some((brush, face, hit)) = self.pick_brush_face_with_hit(rect, pointer) {
            let normal = self
                .project
                .active_scene()
                .brushes
                .get(brush)
                .and_then(|brush| brush.faces.get(face))
                .and_then(|face| psxed_project::brush::Plane::from_points(face.points))
                .map(|plane| {
                    let n = plane.normal.map(|v| v as f64);
                    let length = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-9);
                    [n[0] / length, n[1] / length, n[2] / length]
                })
                .unwrap_or([0.0, 1.0, 0.0]);
            let step = i32::from(self.snap_units.max(1));
            let point = hit.map(|value| {
                let snapped = (f64::from(value) / f64::from(step)).round() * f64::from(step);
                snapped as i32
            });
            self.push_brush_clip_point(BrushClipPoint { point, normal });
            return;
        }
        if let Some(point) = self.brush_ground_point(rect, pointer) {
            self.push_brush_clip_point(BrushClipPoint {
                point,
                normal: [0.0, 1.0, 0.0],
            });
        }
    }

    /// The authored clip plane from the placed points: three points pass
    /// through exactly; two synthesize the third along the first point's
    /// surface normal (perpendicular cut); fewer define no plane yet.
    pub(crate) fn brush_clip_plane_points(&self) -> Option<[[i32; 3]; 3]> {
        match self.brush_clip_points.as_slice() {
            [a, b, c] => Some([a.point, b.point, c.point]),
            [a, b] => {
                let scale = f64::from(BRUSH_CREATE_HEIGHT);
                let third = [
                    a.point[0].saturating_add((a.normal[0] * scale).round() as i32),
                    a.point[1].saturating_add((a.normal[1] * scale).round() as i32),
                    a.point[2].saturating_add((a.normal[2] * scale).round() as i32),
                ];
                if third == a.point {
                    return None;
                }
                Some([a.point, b.point, third])
            }
            _ => None,
        }
    }

    /// Wireframe preview of the pending clip over every selected brush:
    /// kept side(s) in accent, dropped side dimmed red. Never mutates.
    fn draw_brush_clip_preview<F: Fn([f64; 3]) -> Option<egui::Pos2>>(
        &self,
        painter: &egui::Painter,
        project: F,
    ) {
        let Some(points) = self.brush_clip_plane_points() else {
            return;
        };
        let draw_solved = |brush: &psxed_project::brush::Brush, stroke: egui::Stroke| {
            for polygon in brush.solve().polygons.iter().flatten() {
                let count = polygon.verts.len();
                for i in 0..count {
                    if let (Some(a), Some(b)) = (
                        project(polygon.verts[i]),
                        project(polygon.verts[(i + 1) % count]),
                    ) {
                        painter.line_segment([a, b], stroke);
                    }
                }
            }
        };
        let kept = egui::Stroke::new(2.0, STUDIO_ACCENT);
        let dropped = egui::Stroke::new(1.0, CLIP_DROP_COLOR);
        for index in self.selected_brush_set() {
            let Some(brush) = self.project.active_scene().brushes.get(index) else {
                continue;
            };
            let clipped = brush.clip(points);
            let (back_stroke, front_stroke) = match self.brush_clip_keep {
                BrushClipKeep::Both => (kept, kept),
                BrushClipKeep::Back => (kept, dropped),
                BrushClipKeep::Front => (dropped, kept),
            };
            if let Some(back) = &clipped.back {
                draw_solved(back, back_stroke);
            }
            if let Some(front) = &clipped.front {
                draw_solved(front, front_stroke);
            }
        }
    }

    fn draw_brush_clip_preview_3d(
        &self,
        painter: &egui::Painter,
        project: &dyn Fn([f64; 3]) -> Option<egui::Pos2>,
    ) {
        self.draw_brush_clip_preview(painter, project);
    }

    fn draw_brush_clip_preview_2d(
        &self,
        painter: &egui::Painter,
        transform: crate::viewport2d::ViewportTransform,
        view: OrthographicView,
    ) {
        self.draw_brush_clip_preview(painter, |world| {
            let projected = view.project_f64(world);
            Some(transform.world_to_screen([projected[0] as f32, projected[1] as f32]))
        });
    }

    /// Apply the pending clip to every selected brush (one undo step).
    /// Kept sides follow `brush_clip_keep`; brushes the plane misses are
    /// left alone. Returns true when anything was cut.
    pub(crate) fn apply_brush_clip(&mut self) -> bool {
        let Some(points) = self.brush_clip_plane_points() else {
            self.status = "Clip needs two points first".to_string();
            return false;
        };
        let targets = self.selected_brush_set();
        if targets.is_empty() {
            return false;
        }
        let mut replacements: Vec<(usize, psxed_project::brush::Brush)> = Vec::new();
        let mut additions: Vec<psxed_project::brush::Brush> = Vec::new();
        for &index in &targets {
            let Some(brush) = self.project.active_scene().brushes.get(index) else {
                continue;
            };
            let clipped = brush.clip(points);
            match (self.brush_clip_keep, clipped.back, clipped.front) {
                (BrushClipKeep::Both, Some(back), Some(front)) => {
                    replacements.push((index, back));
                    additions.push(front);
                }
                (BrushClipKeep::Back, Some(back), Some(_)) => replacements.push((index, back)),
                (BrushClipKeep::Front, Some(_), Some(front)) => replacements.push((index, front)),
                // Plane missed this brush: nothing to keep or drop.
                _ => {}
            }
        }
        if replacements.is_empty() && additions.is_empty() {
            self.status = "Clip plane misses the selected brushes".to_string();
            return false;
        }
        self.push_undo();
        let cut = replacements.len();
        let scene = self.project.active_scene_mut();
        for (index, brush) in replacements {
            scene.brushes[index] = brush;
        }
        scene.brushes.extend(additions);
        self.brush_clip_points.clear();
        self.reconcile_brush_selection();
        self.mark_dirty();
        self.status = format!(
            "Clipped {cut} brush{} ({})",
            if cut == 1 { "" } else { "es" },
            self.brush_clip_keep.label(),
        );
        true
    }

    /// CSG Subtract: carve every selected brush out of every unselected
    /// brush it intersects, then delete the cutters. One undo step. New
    /// faces take each cutter face's material and UV, so a textured
    /// cutter paints its own reveal.
    pub(crate) fn csg_subtract_selected(&mut self) -> bool {
        let cutter_indices = self.selected_brush_set();
        if cutter_indices.is_empty() {
            self.status = "Subtract needs a selected brush to cut with".to_string();
            return false;
        }
        let cutter_set: std::collections::HashSet<usize> = cutter_indices.iter().copied().collect();
        let scene = self.project.active_scene();
        let cutters: Vec<psxed_project::brush::Brush> = cutter_indices
            .iter()
            .filter_map(|&index| scene.brushes.get(index).cloned())
            .filter(|brush| brush.is_pickable())
            .collect();
        if cutters.is_empty() {
            self.status = "Subtract: the selected brushes are damaged".to_string();
            return false;
        }
        let mut rebuilt: Vec<psxed_project::brush::Brush> = Vec::new();
        let mut carved = 0usize;
        for (index, brush) in scene.brushes.iter().enumerate() {
            if cutter_set.contains(&index) {
                continue;
            }
            let mut pieces = vec![brush.clone()];
            let mut touched = false;
            for cutter in &cutters {
                let mut next = Vec::new();
                for piece in pieces {
                    match piece.subtracted_by(cutter) {
                        Some(parts) => {
                            touched = true;
                            next.extend(parts);
                        }
                        None => next.push(piece),
                    }
                }
                pieces = next;
            }
            if touched {
                carved += 1;
            }
            rebuilt.extend(pieces);
        }
        if carved == 0 {
            self.status = "Subtract: the selected brushes touch nothing".to_string();
            return false;
        }
        self.push_undo();
        self.project.active_scene_mut().brushes = rebuilt;
        self.clear_brush_selection();
        self.reconcile_brush_selection();
        self.mark_dirty();
        self.status = format!(
            "Subtracted {} brush{} from {carved} neighbour{}",
            cutters.len(),
            if cutters.len() == 1 { "" } else { "es" },
            if carved == 1 { "" } else { "s" },
        );
        true
    }

    /// CSG Hollow: replace each selected brush with the walls around an
    /// empty interior, one grid step thick. One undo step.
    pub(crate) fn csg_hollow_selected(&mut self) -> bool {
        let targets = self.selected_brush_set();
        if targets.is_empty() {
            self.status = "Hollow needs a selected brush".to_string();
            return false;
        }
        let thickness = i32::from(self.snap_units.max(1));
        let target_set: std::collections::HashSet<usize> = targets.iter().copied().collect();
        let scene = self.project.active_scene();
        let mut rebuilt: Vec<psxed_project::brush::Brush> = Vec::new();
        let mut hollowed = 0usize;
        let mut refused = 0usize;
        for (index, brush) in scene.brushes.iter().enumerate() {
            if !target_set.contains(&index) {
                rebuilt.push(brush.clone());
                continue;
            }
            match brush.hollowed(thickness) {
                Some(walls) => {
                    hollowed += 1;
                    rebuilt.extend(walls);
                }
                None => {
                    refused += 1;
                    rebuilt.push(brush.clone());
                }
            }
        }
        if hollowed == 0 {
            self.status =
                format!("Hollow: too thin for {thickness}-unit walls (shrink the grid step)");
            return false;
        }
        self.push_undo();
        self.project.active_scene_mut().brushes = rebuilt;
        self.clear_brush_selection();
        self.reconcile_brush_selection();
        self.mark_dirty();
        self.status = if refused == 0 {
            format!(
                "Hollowed {hollowed} brush{} with {thickness}-unit walls",
                if hollowed == 1 { "" } else { "es" }
            )
        } else {
            format!("Hollowed {hollowed}, left {refused} too thin for {thickness}-unit walls")
        };
        true
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
            if self.brush_effectively_hidden(index)
                || matches!(self.brush_group_pick(index), BrushGroupPick::Locked)
            {
                continue;
            }
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
        let snap_anchor = base.solve().min;
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
            snap_anchor,
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
        let mut applied = [0; 3];
        for axis in self.orthographic_view.plane_axes() {
            applied[axis] = absolute_grid_translation_delta(
                mv.snap_anchor[axis],
                f64::from(current[axis] - mv.press_ground[axis]),
                self.snap_units,
            );
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
        let mut best: Option<(f64, bool, usize, usize, usize)> = None;

        for (index, brush) in self.project.active_scene().brushes.iter().enumerate() {
            if self.brush_effectively_hidden(index)
                || matches!(self.brush_group_pick(index), BrushGroupPick::Locked)
            {
                continue;
            }
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
                if best.is_none_or(|(best_distance, best_selected, best_index, best_face, _)| {
                    distance2 < best_distance
                        || (distance2 == best_distance
                            && (selected && !best_selected
                                || (selected == best_selected
                                    && (index < best_index
                                        || (index == best_index && face < best_face)))))
                }) {
                    best = Some((distance2, selected, index, face, axis));
                }
            }
        }

        let Some((_, _, index, face, axis)) = best else {
            return false;
        };
        let base = self.project.active_scene().brushes[index].clone();
        let snap_anchor = base.faces[face].points[0].map(f64::from);
        self.replace_brush_selection(index, Some(face));
        self.brush_extrude = Some(BrushExtrude {
            index,
            face,
            base,
            axis,
            press_y: 0.0,
            press_ground: view.unproject(world, self.orthographic_focus),
            normal_3d: None,
            screen_direction: egui::Vec2::ZERO,
            units_per_pixel: 0.0,
            snap_anchor,
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
        let raw = current[extrude.axis] - extrude.press_ground[extrude.axis];
        let mut delta = [0; 3];
        delta[extrude.axis] = absolute_grid_translation_delta(
            extrude.snap_anchor[extrude.axis],
            f64::from(raw),
            self.snap_units,
        );
        if delta == extrude.applied {
            return;
        }
        let mut preview = extrude.base.clone();
        translate_face_locked(&mut preview, extrude.face, delta, self.brush_texture_lock);
        if brush_preview_ok(&preview) {
            self.project.active_scene_mut().brushes[extrude.index] = preview;
            if let Some(state) = self.brush_extrude.as_mut() {
                state.applied = delta;
            }
            self.status = format!(
                "Moved face on Grid {} ({:+}, {:+}, {:+})",
                self.snap_units, delta[0], delta[1], delta[2]
            );
        } else if let Some(rejection) = brush_preview_rejection(&preview) {
            self.status = rejection.message(self.snap_units);
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

    fn start_brush_vertex_drag(
        &mut self,
        index: usize,
        mut targets: Vec<[f64; 3]>,
        world: [f32; 2],
    ) {
        let mut faces: Vec<usize> = Vec::new();
        // Group drag: when any grabbed target is a selected element, the
        // whole selected set rides along (2D grabs depth columns, 3D
        // single corners; the union covers both semantics).
        if self.selected_brush == Some(index) && !self.selected_brush_elements.is_empty() {
            let element_matches = |key: [i64; 3]| {
                self.selected_brush_elements
                    .iter()
                    .any(|element| match element {
                        BrushElement::Vertex(k) => *k == key,
                        BrushElement::Edge(a, b) => *a == key || *b == key,
                        BrushElement::Face(_) => false,
                    })
            };
            let grabbed_selected = targets
                .iter()
                .any(|target| element_matches(brush_elements::quantize_element_point(*target)));
            if grabbed_selected {
                for extra in self.selected_brush_element_targets() {
                    if !targets
                        .iter()
                        .any(|seen| (0..3).all(|axis| (seen[axis] - extra[axis]).abs() <= 0.5))
                    {
                        targets.push(extra);
                    }
                }
                faces = self.selected_brush_element_faces();
            }
        }
        let base = self.project.active_scene().brushes[index].clone();
        let snap_anchor = targets.first().copied().unwrap_or_else(|| {
            self.orthographic_view
                .unproject(world, self.orthographic_focus)
                .map(f64::from)
        });
        self.selected_brush = Some(index);
        self.brush_vertex_drag = Some(BrushVertexDrag {
            index,
            base,
            others: Vec::new(),
            element_others: Vec::new(),
            targets,
            snap_anchor,
            press_ground: self
                .orthographic_view
                .unproject(world, self.orthographic_focus),
            plane_3d: None,
            applied: [0; 3],
            axis_mask: [true; 3],
            faces,
            snap_source: None,
            snap_target: None,
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
        let mut applied = [0; 3];
        for axis in self.orthographic_view.plane_axes() {
            applied[axis] = absolute_grid_translation_delta(
                drag.snap_anchor[axis],
                f64::from(current[axis] - drag.press_ground[axis]),
                self.snap_units,
            );
        }
        if applied == drag.applied {
            return;
        }
        match self.brush_vertex_drag_previews(&drag, applied) {
            Ok(previews) => {
                let scene = self.project.active_scene_mut();
                for (index, preview) in previews {
                    scene.brushes[index] = preview;
                }
                if let Some(state) = self.brush_vertex_drag.as_mut() {
                    state.applied = applied;
                }
                self.status = format!(
                    "Moved brush element to Grid {} ({:+}, {:+}, {:+})",
                    self.snap_units, applied[0], applied[1], applied[2]
                );
            }
            Err(rejection) => self.status = rejection.message(self.snap_units),
        }
    }

    /// Build every member of an axis-constrained move preview before
    /// touching the scene. A failed member rejects the whole preview, so a
    /// multi-selection can never shear apart because only some brushes were
    /// valid.
    fn brush_vertex_drag_previews(
        &self,
        drag: &BrushVertexDrag,
        applied: [i32; 3],
    ) -> Result<Vec<(usize, psxed_project::brush::Brush)>, BrushPreviewRejection> {
        // Returning the pointer to its press point must restore the base
        // preview. Besides making drag cancellation intuitive, this lets a
        // slightly wiggled click cross egui's drag threshold and still land
        // as selection without leaving one snapped edit behind.
        if applied == [0; 3] {
            return Ok(std::iter::once((drag.index, drag.base.clone()))
                .chain(drag.others.iter().cloned())
                .chain(
                    drag.element_others
                        .iter()
                        .map(|member| (member.index, member.base.clone())),
                )
                .collect());
        }
        let uv_lock = self.brush_texture_lock;
        let mut primary = drag.base.clone();
        let moved = if drag.faces.is_empty() {
            primary.translate_solved_vertices(&drag.targets, applied, 0.5, uv_lock)
        } else {
            primary.translate_selected(&drag.faces, &drag.targets, applied, 0.5, uv_lock)
        };
        if moved == 0 {
            return Err(BrushPreviewRejection::NoEditablePlane);
        }
        if let Some(rejection) = brush_preview_rejection(&primary) {
            return Err(rejection);
        }
        let mut previews = Vec::with_capacity(drag.others.len() + drag.element_others.len() + 1);
        previews.push((drag.index, primary));
        for (index, base) in &drag.others {
            let mut preview = base.clone();
            if uv_lock {
                preview.translate_with_uv_lock(
                    applied,
                    psxed_project::brush::BRUSH_UV_UNITS_PER_TEXEL,
                );
            } else {
                preview.translate(applied);
            }
            if let Some(rejection) = brush_preview_rejection(&preview) {
                return Err(rejection);
            }
            previews.push((*index, preview));
        }
        for member in &drag.element_others {
            let mut preview = member.base.clone();
            let moved = if member.faces.is_empty() {
                preview.translate_solved_vertices(&member.targets, applied, 0.5, uv_lock)
            } else {
                preview.translate_selected(&member.faces, &member.targets, applied, 0.5, uv_lock)
            };
            if moved == 0 {
                return Err(BrushPreviewRejection::NoEditablePlane);
            }
            if let Some(rejection) = brush_preview_rejection(&preview) {
                return Err(rejection);
            }
            previews.push((member.index, preview));
        }
        Ok(previews)
    }

    fn commit_brush_vertex_drag_preview(&mut self) -> bool {
        let Some(drag) = self.brush_vertex_drag.take() else {
            return false;
        };
        let vertex_snap = drag.snap_source.is_some();
        self.brush_vertex_snap_hover = None;
        let live = self.project.active_scene().brushes[drag.index].clone();
        let live_others: Vec<_> = drag
            .others
            .iter()
            .map(|(index, _)| (*index, self.project.active_scene().brushes[*index].clone()))
            .chain(drag.element_others.iter().map(|member| {
                (
                    member.index,
                    self.project.active_scene().brushes[member.index].clone(),
                )
            }))
            .collect();
        self.project.active_scene_mut().brushes[drag.index] = drag.base;
        for (index, base) in &drag.others {
            self.project.active_scene_mut().brushes[*index] = base.clone();
        }
        for member in &drag.element_others {
            self.project.active_scene_mut().brushes[member.index] = member.base.clone();
        }
        if drag.applied != [0; 3] {
            self.push_undo();
            self.project.active_scene_mut().brushes[drag.index] = live;
            for (index, live) in live_others {
                self.project.active_scene_mut().brushes[index] = live;
            }
            self.mark_dirty();
            // Translate the selected element keys that rode this drag so
            // the selection survives its own edit (adding one delta to
            // both edge endpoints preserves canonical order).
            let delta = drag.applied.map(i64::from);
            let dragged = |key: [i64; 3]| {
                drag.targets
                    .iter()
                    .any(|target| brush_elements::quantize_element_point(*target) == key)
            };
            for element in &mut self.selected_brush_elements {
                match element {
                    BrushElement::Vertex(key) if dragged(*key) => {
                        for axis in 0..3 {
                            key[axis] += delta[axis];
                        }
                    }
                    BrushElement::Edge(a, b) if dragged(*a) && dragged(*b) => {
                        for axis in 0..3 {
                            a[axis] += delta[axis];
                            b[axis] += delta[axis];
                        }
                    }
                    _ => {}
                }
            }
            self.reconcile_brush_elements();
            if vertex_snap {
                self.status = "Vertex Snap: committed".to_string();
            }
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
        self.finish_brush_creation_gesture();
    }

    /// Project exact solved brush polygons into the active orthographic
    /// plane, including the selected face and transient create/clip state.
    pub(crate) fn draw_brush_footprints_2d(
        &self,
        painter: &egui::Painter,
        transform: crate::viewport2d::ViewportTransform,
        grid_base_step: Option<f32>,
    ) {
        let view = self.orthographic_view;
        let project_polygon = |polygon: &psxed_project::brush::FacePolygon| {
            polygon
                .verts
                .iter()
                .copied()
                .map(|world| {
                    let projected = view.project_f64(world);
                    transform.world_to_screen([projected[0] as f32, projected[1] as f32])
                })
                .collect::<Vec<_>>()
        };
        let selected_faces_by_brush = self
            .project
            .active_scene()
            .brushes
            .iter()
            .enumerate()
            .map(|(index, _)| {
                let mut selected_faces: Vec<usize> = self
                    .selected_brush_faces
                    .iter()
                    .filter_map(|(brush, face)| (*brush == index).then_some(*face))
                    .collect();
                if selected_faces.is_empty() && self.selected_brush == Some(index) {
                    selected_faces.extend(self.selected_brush_face);
                }
                selected_faces
            })
            .collect::<Vec<_>>();
        let previews = self
            .brush_drag
            .map(Self::brush_drag_brushes)
            .unwrap_or_default();
        let preview_solved: Vec<_> = previews
            .iter()
            .map(psxed_project::brush::Brush::solve)
            .collect();

        with_cached_brush_pick_solves(&self.project, |solved_brushes| {
            // Face fills are material/selection pixels. Like TrenchBroom's
            // face shader, the surface grid is submitted after these pixels
            // so they cannot cover it.
            for (index, solved) in solved_brushes.iter().enumerate() {
                if self.brush_effectively_hidden(index) || !solved.is_valid() {
                    continue;
                }
                let Some(selected_faces) = selected_faces_by_brush.get(index) else {
                    continue;
                };
                for (face, polygon) in solved.polygons.iter().enumerate() {
                    let Some(polygon) = polygon else { continue };
                    if !selected_faces.contains(&face) {
                        continue;
                    }
                    let points = project_polygon(polygon);
                    if points.len() >= 3 && projected_polygon_area(&points).abs() > 0.5 {
                        painter.add(egui::Shape::convex_polygon(
                            points,
                            Color32::from_rgba_unmultiplied(
                                STUDIO_ACCENT.r(),
                                STUDIO_ACCENT.g(),
                                STUDIO_ACCENT.b(),
                                28,
                            ),
                            egui::Stroke::NONE,
                        ));
                    }
                }
            }

            // Reapply the same globally phased grid to every brush face, as
            // TrenchBroom does. Degenerate edge-on projections are ignored by
            // the clipping helper. Tool outlines and handles are drawn later.
            if let Some(base_step) = grid_base_step {
                let draw_surface_grid = |solved: &psxed_project::brush::SolvedBrush| {
                    for polygon in solved.polygons.iter().flatten() {
                        let projected = polygon
                            .verts
                            .iter()
                            .map(|&world| view.project_f64(world))
                            .collect::<Vec<_>>();
                        crate::viewport2d::draw_world_grid_on_convex_polygon(
                            painter, transform, base_step, &projected,
                        );
                    }
                };
                for (index, solved) in solved_brushes.iter().enumerate() {
                    if !self.brush_effectively_hidden(index) && solved.is_valid() {
                        draw_surface_grid(solved);
                    }
                }
                for solved in preview_solved.iter().filter(|solved| solved.is_valid()) {
                    draw_surface_grid(solved);
                }
            }

            // Brush cages and selected-face emphasis are tool geometry and
            // therefore stay above every surface-grid stroke.
            let draw_outline = |solved: &psxed_project::brush::SolvedBrush,
                                selected_faces: &[usize],
                                stroke: egui::Stroke| {
                for (face, polygon) in solved.polygons.iter().enumerate() {
                    let Some(polygon) = polygon else { continue };
                    let points = project_polygon(polygon);
                    if points.len() < 2 {
                        continue;
                    }
                    let face_stroke = if selected_faces.contains(&face) {
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
                if self.brush_effectively_hidden(index) {
                    continue;
                }
                let Some(solved) = solved_brushes.get(index).filter(|solved| solved.is_valid())
                else {
                    continue;
                };
                let selected =
                    self.brush_is_selected(index) || self.brush_selected_through_group(index);
                let stroke = if selected {
                    egui::Stroke::new(2.0, STUDIO_ACCENT)
                } else {
                    egui::Stroke::new(1.0, brush_contents_outline(brush.contents))
                };
                draw_outline(
                    solved,
                    selected_faces_by_brush
                        .get(index)
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                    stroke,
                );
            }
            for solved in preview_solved.iter().filter(|solved| solved.is_valid()) {
                draw_outline(solved, &[], egui::Stroke::new(1.5, STUDIO_ACCENT));
            }
        });
        for (number, clip_point) in self.brush_clip_points.iter().enumerate() {
            let projected = view.project_f32(clip_point.point.map(|value| value as f32));
            let center = transform.world_to_screen(projected);
            painter.circle_stroke(center, 5.0, egui::Stroke::new(1.5, STUDIO_ACCENT));
            painter.text(
                center + Vec2::new(7.0, -7.0),
                egui::Align2::LEFT_BOTTOM,
                format!("{}", number + 1),
                egui::FontId::proportional(11.0),
                STUDIO_ACCENT,
            );
        }
        self.draw_brush_clip_preview_2d(painter, transform, view);
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
                        // Clip markers + preview draw once, outside the
                        // per-brush loop.
                        BrushEditMode::Clip => {}
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
                            let midpoint_matches = |element: &BrushElement, center: Pos2| {
                                let BrushElement::Edge(ka, kb) = element else {
                                    return false;
                                };
                                let mid = to_screen(std::array::from_fn(|axis| {
                                    (ka[axis] as f64 + kb[axis] as f64) * 0.5
                                }));
                                mid.distance(center) <= 1.0
                            };
                            let mut seen = Vec::<Pos2>::new();
                            for (a, b) in brush_elements::unique_edges(&solved) {
                                let center = to_screen(std::array::from_fn(|axis| {
                                    (a[axis] + b[axis]) * 0.5
                                }));
                                if seen.iter().any(|point| point.distance(center) <= 1.0) {
                                    continue;
                                }
                                seen.push(center);
                                let selected = self
                                    .selected_brush_elements
                                    .iter()
                                    .any(|element| midpoint_matches(element, center));
                                let hovered = self
                                    .selection
                                    .hovered_brush_handle
                                    .as_ref()
                                    .is_some_and(|element| midpoint_matches(element, center));
                                let stroke = if selected {
                                    egui::Stroke::new(2.5, egui::Color32::WHITE)
                                } else if hovered {
                                    egui::Stroke::new(2.5, HANDLE_HOVER_COLOR)
                                } else {
                                    egui::Stroke::new(1.5, STUDIO_ACCENT)
                                };
                                painter.rect_stroke(
                                    Rect::from_center_size(center, Vec2::splat(7.0)),
                                    0.0,
                                    stroke,
                                    egui::StrokeKind::Inside,
                                );
                            }
                        }
                        BrushEditMode::Vertex => {
                            if let Some((_, verts)) = self.selected_brush_solved_verts() {
                                // A 2D square represents a whole depth
                                // column, so highlight by projected
                                // position rather than exact key.
                                let projected_matches =
                                    |element: &BrushElement, projected: [f64; 2]| {
                                        let BrushElement::Vertex(key) = element else {
                                            return false;
                                        };
                                        let p = view.project_f64([
                                            key[0] as f64,
                                            key[1] as f64,
                                            key[2] as f64,
                                        ]);
                                        (p[0] - projected[0]).abs() <= 0.5
                                            && (p[1] - projected[1]).abs() <= 0.5
                                    };
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
                                    let selected = self
                                        .selected_brush_elements
                                        .iter()
                                        .any(|element| projected_matches(element, projected));
                                    let hovered =
                                        self.selection.hovered_brush_handle.as_ref().is_some_and(
                                            |element| projected_matches(element, projected),
                                        );
                                    let (size, color) = if selected {
                                        (9.0, egui::Color32::WHITE)
                                    } else if hovered {
                                        (9.0, HANDLE_HOVER_COLOR)
                                    } else {
                                        (7.0, STUDIO_ACCENT)
                                    };
                                    painter.rect_filled(
                                        Rect::from_center_size(to_screen(vert), Vec2::splat(size)),
                                        1.0,
                                        color,
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
            if self.brush_effectively_hidden(index) {
                continue;
            }
            let selected =
                self.brush_is_selected(index) || self.brush_selected_through_group(index);
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
            // Emphasize every selected face polygon, including additive
            // selections whose owner is not the current primary brush.
            let mut emphasized: Vec<usize> = self
                .selected_brush_faces
                .iter()
                .filter_map(|(brush, face)| (*brush == index).then_some(*face))
                .collect();
            if emphasized.is_empty() && self.selected_brush == Some(index) {
                emphasized.extend(self.selected_brush_elements.iter().filter_map(|element| {
                    if let BrushElement::Face(face) = element {
                        Some(*face)
                    } else {
                        None
                    }
                }));
                if let Some(face) = self.selected_brush_face {
                    if !emphasized.contains(&face) {
                        emphasized.push(face);
                    }
                }
            }
            if !emphasized.is_empty() {
                let solved = brush.solve();
                let emphasize = |face: usize| {
                    if let Some(Some(polygon)) = solved.polygons.get(face) {
                        let count = polygon.verts.len();
                        // Translucent fill so a selected face reads as
                        // SELECTED at a glance (matches the 2D views).
                        let screen: Vec<egui::Pos2> = polygon
                            .verts
                            .iter()
                            .filter_map(|vert| project(*vert))
                            .collect();
                        if screen.len() == count {
                            painter.add(egui::Shape::convex_polygon(
                                screen,
                                STUDIO_ACCENT.gamma_multiply(0.18),
                                egui::Stroke::NONE,
                            ));
                        }
                        for i in 0..count {
                            let a = project(polygon.verts[i]);
                            let b = project(polygon.verts[(i + 1) % count]);
                            if let (Some(a), Some(b)) = (a, b) {
                                painter.line_segment([a, b], egui::Stroke::new(3.5, STUDIO_ACCENT));
                            }
                        }
                    }
                };
                emphasized.sort_unstable();
                emphasized.dedup();
                for face in emphasized {
                    emphasize(face);
                }
            }
        }
        if matches!(self.active_tool, ViewTool::Brush | ViewTool::Select) {
            if let Some(index) = self.selected_brush {
                if let Some(brush) = self.project.active_scene().brushes.get(index) {
                    let solved = brush.solve();
                    match self.brush_edit_mode {
                        BrushEditMode::Clip => {
                            for (number, clip_point) in self.brush_clip_points.iter().enumerate() {
                                let world = clip_point.point.map(f64::from);
                                if let Some(center) = project(world) {
                                    painter.circle_stroke(
                                        center,
                                        5.0,
                                        egui::Stroke::new(1.5, STUDIO_ACCENT),
                                    );
                                    painter.text(
                                        center + egui::Vec2::new(7.0, -7.0),
                                        egui::Align2::LEFT_BOTTOM,
                                        format!("{}", number + 1),
                                        egui::FontId::proportional(11.0),
                                        STUDIO_ACCENT,
                                    );
                                }
                            }
                            self.draw_brush_clip_preview_3d(painter, &project);
                        }
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
                                let key = BrushElement::Vertex(
                                    brush_elements::quantize_element_point(vertex),
                                );
                                let selected = self.selected_brush_elements.contains(&key);
                                let hovered = self.selection.hovered_brush_handle == Some(key);
                                if let Some(center) = project(vertex) {
                                    let (size, color) = if selected {
                                        (9.0, egui::Color32::WHITE)
                                    } else if hovered {
                                        (9.0, HANDLE_HOVER_COLOR)
                                    } else {
                                        (7.0, STUDIO_ACCENT)
                                    };
                                    painter.rect_filled(
                                        egui::Rect::from_center_size(
                                            center,
                                            egui::Vec2::splat(size),
                                        ),
                                        1.0,
                                        color,
                                    );
                                }
                            }
                        }
                        BrushEditMode::Edge => {
                            for (a, b) in brush_elements::unique_edges(&solved) {
                                let (ka, kb) = brush_elements::edge_element_key(a, b);
                                let key = BrushElement::Edge(ka, kb);
                                let selected = self.selected_brush_elements.contains(&key);
                                let hovered = self.selection.hovered_brush_handle == Some(key);
                                let midpoint = [
                                    (a[0] + b[0]) * 0.5,
                                    (a[1] + b[1]) * 0.5,
                                    (a[2] + b[2]) * 0.5,
                                ];
                                if let Some(center) = project(midpoint) {
                                    if selected || hovered {
                                        let color = if selected {
                                            egui::Color32::WHITE
                                        } else {
                                            HANDLE_HOVER_COLOR
                                        };
                                        painter.circle_filled(center, 5.5, color);
                                        // The edge itself brightens too.
                                        if let (Some(sa), Some(sb)) = (project(a), project(b)) {
                                            painter.line_segment(
                                                [sa, sb],
                                                egui::Stroke::new(2.5, color),
                                            );
                                        }
                                    } else {
                                        painter.circle_filled(center, 4.0, STUDIO_ACCENT);
                                    }
                                }
                            }
                        }
                        BrushEditMode::Face => {
                            // Every face, including the ones facing away. Culling
                            // them meant a face's handle vanished the moment you
                            // orbited past it, or never appeared at all from
                            // inside a room.
                            for (_, center, normal) in brush_elements::face_handles(brush, &solved)
                            {
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
        for preview in self
            .brush_drag
            .map(Self::brush_drag_brushes)
            .unwrap_or_default()
        {
            draw(&preview, egui::Stroke::new(1.5, STUDIO_ACCENT));
        }
        // Live wireframe of the pending face extrusion.
        if let Some(gesture) = &self.brush_extrude_new {
            if gesture.applied > 0 {
                if let Some(candidate) = self
                    .project
                    .active_scene()
                    .brushes
                    .get(gesture.source)
                    .and_then(|brush| brush.extruded_from_face(gesture.face, gesture.applied))
                {
                    draw(&candidate, egui::Stroke::new(2.0, STUDIO_ACCENT));
                }
            }
        }
        // Transform gizmo: the whole brush in Brush mode, the element
        // selection in Face/Edge/Vertex modes.
        if self.brush_gizmo_context().is_some() {
            if let Some(polylines) = self.brush_element_gizmo_polylines_3d(rect) {
                for (axis, polyline) in polylines.into_iter().enumerate() {
                    let color = ELEMENT_GIZMO_AXIS_COLORS[axis];
                    for pair in polyline.windows(2) {
                        painter.line_segment([pair[0], pair[1]], egui::Stroke::new(2.5, color));
                    }
                    // Mode-distinct tips: circle for Move, box for Scale;
                    // rings are their own shape.
                    if self.transform_gizmo_mode != TransformGizmoMode::Rotate {
                        if let Some(tip) = polyline.last() {
                            match self.transform_gizmo_mode {
                                TransformGizmoMode::Scale => {
                                    painter.rect_filled(
                                        egui::Rect::from_center_size(*tip, egui::Vec2::splat(9.0)),
                                        1.0,
                                        color,
                                    );
                                }
                                _ => {
                                    painter.circle_filled(*tip, 4.5, color);
                                }
                            }
                        }
                    }
                }
            }
            if let Some(drag) = self
                .brush_element_transform
                .as_ref()
                .filter(|drag| drag.rotate)
            {
                paint_rotation_readout(
                    painter,
                    drag.center_screen,
                    drag.applied,
                    drag.rotation_snap_degrees,
                );
            }
        }
        // Godot 4.7-style vertex-snap feedback: source is yellow, acquired
        // target is green, both with a dark outline so they stay legible over
        // any material. These are deliberately drawn last, above handles and
        // the transform gizmo.
        let (snap_source, snap_target) = self.brush_vertex_snap_markers();
        let draw_snap_marker = |point: [f64; 3], color: egui::Color32| {
            if let Some(screen) = self.project_brush_point_3d(rect, point) {
                painter.circle_filled(screen, 8.0, egui::Color32::from_black_alpha(180));
                painter.circle_filled(screen, 6.0, color);
            }
        };
        if let Some(source) = snap_source {
            draw_snap_marker(source, VERTEX_SNAP_SOURCE_COLOR);
        }
        if let Some(target) = snap_target {
            draw_snap_marker(target, VERTEX_SNAP_TARGET_COLOR);
        }
    }
}

/// Vertical extrude sensitivity, world units per pixel (matches the
/// primitive height-drag feel: 8 px per 64-unit quantum).
const EXTRUDE_UNITS_PER_PIXEL: f32 = 8.0;

/// Pre-click hover tint for brush sub-element handles (yellow, matching
/// the entity-bounds hover convention; selected stays white).
const HANDLE_HOVER_COLOR: egui::Color32 = egui::Color32::from_rgb(255, 240, 144);

/// Godot 4.7 uses a 30-device-pixel acquisition radius for both source and
/// target vertices. Keeping this screen-space makes snapping independent of
/// camera distance and world scale.
const VERTEX_SNAP_RADIUS_PX: f32 = 30.0;
const VERTEX_SNAP_SOURCE_COLOR: egui::Color32 = egui::Color32::from_rgb(255, 232, 64);
const VERTEX_SNAP_TARGET_COLOR: egui::Color32 = egui::Color32::from_rgb(80, 232, 112);

/// Translation snapping must quantise the selected geometry's destination,
/// not merely the distance travelled by the pointer. For example, moving an
/// off-grid X=10 corner by roughly six units on a 16-unit grid must apply +6
/// and land at X=16; rounding the delta would apply either 0 or 16 and retain
/// the original ten-unit phase forever.
fn absolute_grid_translation_delta(anchor: f64, raw_delta: f64, step: u16) -> i32 {
    if !anchor.is_finite() || !raw_delta.is_finite() {
        return 0;
    }
    let step = i64::from(step.max(1));
    let destination = (anchor + raw_delta)
        .round()
        .clamp(i32::MIN as f64, i32::MAX as f64) as i64;
    let snapped = ((destination + step / 2).div_euclid(step) * step)
        .clamp(i64::from(i32::MIN), i64::from(i32::MAX));
    (snapped as f64 - anchor)
        .round()
        .clamp(i32::MIN as f64, i32::MAX as f64) as i32
}

/// BSP authored points translate in whole world units. Only advertise a
/// green target when the selected source can land on it exactly in that
/// representation; otherwise the ordinary grid drag remains active instead
/// of displaying a false snap with a sub-unit gap.
fn vertex_snap_integer_delta(source: [f64; 3], target: [f64; 3]) -> Option<[i32; 3]> {
    let mut delta = [0; 3];
    for axis in 0..3 {
        let raw = target[axis] - source[axis];
        let rounded = raw.round();
        if !raw.is_finite()
            || (raw - rounded).abs() > 1.0e-5
            || rounded < f64::from(i32::MIN)
            || rounded > f64::from(i32::MAX)
        {
            return None;
        }
        delta[axis] = rounded as i32;
    }
    Some(delta)
}

/// Plane translate honoring the texture lock: the face keeps its
/// applied texture when the lock is on.
fn translate_face_locked(
    brush: &mut psxed_project::brush::Brush,
    face: usize,
    delta: [i32; 3],
    lock: bool,
) {
    if lock {
        brush.translate_face_with_uv_lock(face, delta);
    } else {
        brush.translate_face(face, delta);
    }
}

/// A reshape preview may be applied only when it still encloses a
/// BOUNDED volume: `is_valid` alone accepts infinite wedges (planes
/// dragged parallel) whose solved vertices sit at the base-winding
/// extent and overflow the preview renderer's i32 camera math.
fn brush_preview_ok(brush: &psxed_project::brush::Brush) -> bool {
    brush_preview_rejection(brush).is_none()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrushPreviewRejection {
    NoEditablePlane,
    NoVolume,
    Unbounded,
}

impl BrushPreviewRejection {
    fn message(self, grid: u16) -> String {
        match self {
            Self::NoEditablePlane => {
                "Move rejected: the selected corner has no editable incident plane".to_string()
            }
            Self::NoVolume => {
                format!("Move rejected on Grid {grid}: the brush would collapse or invert")
            }
            Self::Unbounded => {
                format!("Move rejected on Grid {grid}: the brush would become unbounded")
            }
        }
    }
}

fn brush_preview_rejection(brush: &psxed_project::brush::Brush) -> Option<BrushPreviewRejection> {
    let solved = brush.solve();
    if !solved.is_valid() {
        Some(BrushPreviewRejection::NoVolume)
    } else if !solved.within_extent(psxed_project::brush::BRUSH_EDIT_EXTENT_LIMIT) {
        Some(BrushPreviewRejection::Unbounded)
    } else {
        None
    }
}

/// Dropped-side tint in the clip preview.
const CLIP_DROP_COLOR: egui::Color32 = egui::Color32::from_rgb(220, 96, 96);

/// Element gizmo axis colors (X red, Y green, Z blue).
const ELEMENT_GIZMO_AXIS_COLORS: [egui::Color32; 3] = [
    egui::Color32::from_rgb(232, 84, 84),
    egui::Color32::from_rgb(104, 220, 112),
    egui::Color32::from_rgb(96, 148, 244),
];

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

impl BrushHandle3d {
    /// The stable selection identity of this handle.
    pub(crate) fn element(&self) -> BrushElement {
        match self {
            Self::Vertex(vertex) => {
                BrushElement::Vertex(brush_elements::quantize_element_point(*vertex))
            }
            Self::Edge(a, b) => {
                let (a, b) = brush_elements::edge_element_key(*a, *b);
                BrushElement::Edge(a, b)
            }
            Self::Face { face, .. } => BrushElement::Face(*face),
        }
    }
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

    /// Whether a face's outward normal points back at the 3D camera.
    ///
    /// Face handles are drawn and picked on both sides of a brush, so this no
    /// longer gates visibility: a face you have orbited behind still offers its
    /// handle. It survives as the tie-break for picking, where a face and the
    /// one opposite it can project onto the same screen point.
    pub(crate) fn brush_face_faces_camera(&self, center: [f64; 3], normal: [f64; 3]) -> bool {
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
        let index = self.selected_brush?;
        self.pick_brush_handle_on_3d(index, rect, pointer)
            .map(|handle| (index, handle))
    }

    /// Pick a reshape handle on a specific brush. Keeping the indexed form
    /// separate lets Face/Edge mode switch directly to another brush in one
    /// click instead of forcing a whole-brush selection click first.
    fn pick_brush_handle_on_3d(
        &self,
        index: usize,
        rect: egui::Rect,
        pointer: egui::Pos2,
    ) -> Option<BrushHandle3d> {
        const HANDLE_RADIUS: f32 = 9.0;
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
            BrushEditMode::Move | BrushEditMode::Clip => return None,
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
                // Both sides are pickable, so a face and the one opposite it can
                // land within the same handle radius. `consider` keeps the first
                // of equal distances, so offer the camera-facing ones first and
                // the near face wins.
                let mut faces = brush_elements::face_handles(brush, &solved);
                faces.sort_by_key(|(_, center, normal)| {
                    u8::from(!self.brush_face_faces_camera(*center, *normal))
                });
                for (face, center, normal) in faces {
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
        best.map(|(_, handle)| handle)
    }

    /// Pick anywhere along an edge of the ray-hit face. Unselected brushes do
    /// not draw midpoint handles, so cross-brush Edge mode must use the
    /// boundary the author can actually see rather than an invisible handle.
    fn pick_brush_face_edge_on_3d(
        &self,
        index: usize,
        face: usize,
        rect: egui::Rect,
        pointer: egui::Pos2,
    ) -> Option<BrushElement> {
        const EDGE_PICK_RADIUS: f64 = 9.0;
        let brush = self.project.active_scene().brushes.get(index)?;
        let solved = brush.solve();
        let polygon = solved.polygons.get(face)?.as_ref()?;
        let mut best: Option<(f64, BrushElement)> = None;
        for edge in 0..polygon.verts.len() {
            let a = polygon.verts[edge];
            let b = polygon.verts[(edge + 1) % polygon.verts.len()];
            let (Some(screen_a), Some(screen_b)) = (
                self.project_brush_point_3d(rect, a),
                self.project_brush_point_3d(rect, b),
            ) else {
                continue;
            };
            let distance2 = point_segment_distance2(
                [f64::from(pointer.x), f64::from(pointer.y)],
                [f64::from(screen_a.x), f64::from(screen_a.y)],
                [f64::from(screen_b.x), f64::from(screen_b.y)],
            );
            if distance2 <= EDGE_PICK_RADIUS * EDGE_PICK_RADIUS
                && best.is_none_or(|(best_distance2, _)| distance2 < best_distance2)
            {
                let (a, b) = brush_elements::edge_element_key(a, b);
                best = Some((distance2, BrushElement::Edge(a, b)));
            }
        }
        best.map(|(_, element)| element)
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

    /// Nearest projected solved brush corner within the same fixed
    /// screen-space radius Godot 4.7 uses for 3D vertex snapping. The
    /// predicate separates source acquisition (selected brushes only) from
    /// target acquisition (every visible, non-moving brush).
    fn closest_brush_vertex_3d(
        &self,
        rect: egui::Rect,
        pointer: egui::Pos2,
        accept_vertex: impl Fn(usize, [f64; 3]) -> bool,
    ) -> Option<(usize, [f64; 3])> {
        let mut best: Option<(f32, usize, [f64; 3])> = None;
        with_cached_brush_pick_solves(&self.project, |solved_brushes| {
            for (index, solved) in solved_brushes.iter().enumerate() {
                if self.brush_effectively_hidden(index) || !solved.is_valid() {
                    continue;
                }
                for vertex in brush_elements::unique_vertices(solved) {
                    if !accept_vertex(index, vertex) {
                        continue;
                    }
                    let Some(screen) = self.project_brush_point_3d(rect, vertex) else {
                        continue;
                    };
                    let distance2 = screen.distance_sq(pointer);
                    if distance2 <= VERTEX_SNAP_RADIUS_PX * VERTEX_SNAP_RADIUS_PX
                        && best.is_none_or(|(best_distance2, _, _)| distance2 < best_distance2)
                    {
                        best = Some((distance2, index, vertex));
                    }
                }
            }
        });
        best.map(|(_, index, vertex)| (index, vertex))
    }

    /// Refresh the yellow source marker while B is held. Once a drag starts,
    /// its source is retained by `BrushVertexDrag`, matching Godot's
    /// momentary-key behavior when B is released mid-gesture.
    pub(crate) fn update_brush_vertex_snap_hover_3d(
        &mut self,
        rect: egui::Rect,
        pointer: Option<egui::Pos2>,
    ) {
        if self
            .brush_vertex_drag
            .as_ref()
            .is_some_and(|drag| drag.snap_source.is_some())
        {
            self.brush_vertex_snap_hover = None;
            return;
        }
        if !self.brush_vertex_snap_key_down || self.brush_edit_mode != BrushEditMode::Move {
            self.brush_vertex_snap_hover = None;
            return;
        }
        let Some(pointer) = pointer else {
            self.brush_vertex_snap_hover = None;
            return;
        };
        let selected = self.selected_brush_set();
        self.brush_vertex_snap_hover = self
            .closest_brush_vertex_3d(rect, pointer, |index, _| selected.contains(&index))
            .map(|(_, vertex)| vertex);
    }

    /// Begin Godot-style vertex-to-vertex snapping for a BSP brush set. The
    /// primary brush uses the existing whole-brush vertex-drag transaction;
    /// every other selected brush rides the same delta, so preview, cancel,
    /// undo and texture lock remain shared with ordinary Move-gizmo drags.
    fn begin_brush_vertex_snap_3d(&mut self, rect: egui::Rect, pointer: egui::Pos2) -> bool {
        if !self.brush_vertex_snap_key_down || self.brush_edit_mode != BrushEditMode::Move {
            return false;
        }
        let index = match self.selected_brush {
            Some(index) => index,
            None => return false,
        };
        let selected = self.selected_brush_set();
        let source = self.brush_vertex_snap_hover.or_else(|| {
            self.closest_brush_vertex_3d(rect, pointer, |candidate, _| {
                selected.contains(&candidate)
            })
            .map(|(_, vertex)| vertex)
        });
        let Some(source) = source else {
            self.status = "Vertex Snap: hover a corner on the selected brush".to_string();
            return false;
        };
        let Some((_, targets, faces, _)) = self.brush_gizmo_context() else {
            return false;
        };
        if !self.begin_brush_vertex_drag_3d(rect, pointer, index, targets, source) {
            return false;
        }
        let others = self.brush_move_others(index);
        if let Some(drag) = self.brush_vertex_drag.as_mut() {
            drag.others = others;
            drag.faces = faces;
            drag.snap_source = Some(source);
        }
        self.brush_vertex_snap_hover = None;
        self.status = "Vertex Snap: drag the yellow corner onto another brush corner".to_string();
        true
    }

    fn brush_vertex_snap_markers(&self) -> (Option<[f64; 3]>, Option<[f64; 3]>) {
        let Some(drag) = self
            .brush_vertex_drag
            .as_ref()
            .filter(|drag| drag.snap_source.is_some())
        else {
            return (self.brush_vertex_snap_hover, None);
        };
        let source = drag.snap_source.map(|mut source| {
            for axis in 0..3 {
                source[axis] += f64::from(drag.applied[axis]);
            }
            source
        });
        (source, drag.snap_target)
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
        let snap_anchor = targets.first().copied().unwrap_or(anchor);
        self.brush_vertex_drag = Some(BrushVertexDrag {
            index,
            base: self.project.active_scene().brushes[index].clone(),
            others: Vec::new(),
            element_others: Vec::new(),
            targets,
            snap_anchor,
            // In 3D this otherwise-2D field keeps the press pixel so a drag
            // that settles back inside click slop can reset to zero.
            press_ground: [pointer.x, pointer.y, 0.0],
            plane_3d: Some(plane),
            applied: [0; 3],
            axis_mask: [true; 3],
            faces: Vec::new(),
            snap_source: None,
            snap_target: None,
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
        let mut snap_target = None;
        const CLICK_SLOP_PX: f32 = 4.0;
        let settled_back_to_click = pointer
            .distance(egui::Pos2::new(drag.press_ground[0], drag.press_ground[1]))
            <= CLICK_SLOP_PX;
        let applied = if settled_back_to_click {
            [0; 3]
        } else if let Some(source) = drag.snap_source {
            let moving: Vec<_> = std::iter::once(drag.index)
                .chain(drag.others.iter().map(|(index, _)| *index))
                .chain(drag.element_others.iter().map(|member| member.index))
                .collect();
            if let Some((_, target)) =
                self.closest_brush_vertex_3d(rect, pointer, |index, target| {
                    !moving.contains(&index) && vertex_snap_integer_delta(source, target).is_some()
                })
            {
                snap_target = Some(target);
                self.status = "Vertex Snap: snapped".to_string();
                let exact = vertex_snap_integer_delta(source, target)
                    .expect("target predicate accepted an exact integer delta");
                std::array::from_fn(|axis| drag.axis_mask[axis].then_some(exact[axis]).unwrap_or(0))
            } else {
                self.status = "Vertex Snap: drag onto another brush corner".to_string();
                std::array::from_fn(|axis| {
                    if drag.axis_mask[axis] {
                        absolute_grid_translation_delta(
                            source[axis],
                            f64::from(current[axis] - plane.press_world[axis]),
                            self.snap_units,
                        )
                    } else {
                        0
                    }
                })
            }
        } else {
            std::array::from_fn(|axis| {
                if drag.axis_mask[axis] {
                    absolute_grid_translation_delta(
                        drag.snap_anchor[axis],
                        f64::from(current[axis] - plane.press_world[axis]),
                        self.snap_units,
                    )
                } else {
                    0
                }
            })
        };
        if applied == drag.applied {
            if let Some(state) = self.brush_vertex_drag.as_mut() {
                state.snap_target = snap_target;
            }
            return;
        }
        match self.brush_vertex_drag_previews(&drag, applied) {
            Ok(previews) => {
                let scene = self.project.active_scene_mut();
                for (index, preview) in previews {
                    scene.brushes[index] = preview;
                }
                if let Some(state) = self.brush_vertex_drag.as_mut() {
                    state.applied = applied;
                    state.snap_target = snap_target;
                }
                if drag.snap_source.is_none() {
                    self.status = format!(
                        "Moved brush element to Grid {} ({:+}, {:+}, {:+})",
                        self.snap_units, applied[0], applied[1], applied[2]
                    );
                }
            }
            Err(rejection) => {
                if let Some(state) = self.brush_vertex_drag.as_mut() {
                    state.snap_target = None;
                }
                self.status = rejection.message(self.snap_units);
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
            let snap_anchor =
                self.project.active_scene().brushes[index].faces[face].points[0].map(f64::from);
            self.replace_brush_selection(index, Some(face));
            self.brush_extrude = Some(BrushExtrude {
                index,
                face,
                base: self.project.active_scene().brushes[index].clone(),
                axis,
                press_y: pointer.y,
                press_ground: self
                    .brush_ground_point_raw(rect, pointer)
                    .unwrap_or([0.0; 3]),
                normal_3d: None,
                screen_direction: egui::Vec2::ZERO,
                units_per_pixel: 0.0,
                snap_anchor,
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
        let snap_anchor =
            self.project.active_scene().brushes[index].faces[face].points[0].map(f64::from);
        self.replace_brush_selection(index, Some(face));
        self.brush_extrude = Some(BrushExtrude {
            index,
            face,
            base: self.project.active_scene().brushes[index].clone(),
            axis,
            press_y: pointer.y,
            press_ground: [pointer.x, 0.0, 0.0],
            normal_3d: Some(normal),
            screen_direction,
            units_per_pixel,
            snap_anchor,
            applied: [0; 3],
        });
    }
}

impl ViewportTool3d for BrushTool {
    fn primary_pressed(&self, ws: &mut EditorWorkspace, frame: &ToolFrame3d) {
        let Some(pointer) = frame.pointer_interact else {
            return;
        };
        if ws.brush_vertex_snap_key_down {
            ws.begin_brush_vertex_snap_3d(frame.rect, pointer);
            return;
        }
        if ws.begin_brush_height_drag(pointer.y) {
            return;
        }
        if ws.active_tool == ViewTool::Brush {
            ws.clear_brush_selection();
            if let Some(point) = ws.brush_ground_point(frame.rect, pointer) {
                ws.brush_drag = Some(BrushDrag {
                    anchor: point,
                    current: point,
                    view: OrthographicView::Top,
                    grid_step: i32::from(ws.snap_units.max(1)),
                    height_end: point[1].saturating_add(BRUSH_CREATE_HEIGHT),
                    stage: BrushCreateStage::Footprint,
                    height_press_y: 0,
                    height_press_end: 0,
                    height_dragging: false,
                    settings: ws.brush_draw_settings,
                });
            }
            return;
        }
        // Whole brushes use the transform gizmo exclusively in Select mode.
        if ws.brush_edit_mode == BrushEditMode::Move {
            return;
        }
        if let Some(axis) = ws.pick_brush_element_gizmo_axis_3d(frame.rect, pointer) {
            if ws.begin_brush_element_gizmo_drag(frame.rect, pointer, axis) {
                return;
            }
        }
        if let Some((index, handle)) = ws.pick_brush_handle_3d(frame.rect, pointer) {
            match handle {
                BrushHandle3d::Vertex(vertex) => {
                    let (targets, faces) =
                        ws.brush_drag_targets_for_grab(handle.element(), vec![vertex]);
                    if ws.begin_brush_vertex_drag_3d(frame.rect, pointer, index, targets, vertex) {
                        if let Some(drag) = ws.brush_vertex_drag.as_mut() {
                            drag.faces = faces;
                        }
                        return;
                    }
                }
                BrushHandle3d::Edge(a, b) => {
                    let center = [
                        (a[0] + b[0]) * 0.5,
                        (a[1] + b[1]) * 0.5,
                        (a[2] + b[2]) * 0.5,
                    ];
                    let (targets, faces) =
                        ws.brush_drag_targets_for_grab(handle.element(), vec![a, b]);
                    if ws.begin_brush_vertex_drag_3d(frame.rect, pointer, index, targets, center) {
                        if let Some(drag) = ws.brush_vertex_drag.as_mut() {
                            drag.faces = faces;
                        }
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
        }
    }

    fn primary_dragged(&self, ws: &mut EditorWorkspace, frame: &ToolFrame3d) {
        let Some(pointer) = frame.pointer_interact.or(frame.pointer_hover) else {
            return;
        };
        if ws.brush_extrude_new.is_some() {
            ws.update_brush_face_extrude_new(pointer);
            return;
        }
        if ws.brush_element_transform.is_some() {
            ws.update_brush_element_transform(pointer, frame.modifiers.shift);
            return;
        }
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
            let applied = [
                absolute_grid_translation_delta(
                    mv.snap_anchor[0],
                    f64::from(ground[0] - mv.press_ground[0]),
                    ws.snap_units,
                ),
                0,
                absolute_grid_translation_delta(
                    mv.snap_anchor[2],
                    f64::from(ground[2] - mv.press_ground[2]),
                    ws.snap_units,
                ),
            ];
            if applied != mv.applied {
                ws.apply_brush_move_preview(&mv, applied);
            }
            return;
        }
        if let Some(extrude) = ws.brush_extrude.clone() {
            if let Some(normal) = extrude.normal_3d {
                let press = egui::Pos2::new(extrude.press_ground[0], extrude.press_y);
                let raw_distance =
                    (pointer - press).dot(extrude.screen_direction) * extrude.units_per_pixel;
                let axis = extrude.axis;
                let snapped_axis_delta = absolute_grid_translation_delta(
                    extrude.snap_anchor[axis],
                    f64::from(raw_distance) * normal[axis],
                    ws.snap_units,
                );
                if normal[axis].abs() <= f64::EPSILON {
                    return;
                }
                let snapped_distance = f64::from(snapped_axis_delta) / normal[axis];
                let applied = [
                    (normal[0] * snapped_distance).round() as i32,
                    (normal[1] * snapped_distance).round() as i32,
                    (normal[2] * snapped_distance).round() as i32,
                ];
                if applied == extrude.applied {
                    return;
                }
                let mut preview = extrude.base.clone();
                translate_face_locked(&mut preview, extrude.face, applied, ws.brush_texture_lock);
                if brush_preview_ok(&preview) {
                    ws.project.active_scene_mut().brushes[extrude.index] = preview;
                    if let Some(state) = ws.brush_extrude.as_mut() {
                        state.applied = applied;
                    }
                    ws.status = format!(
                        "Moved face on Grid {} ({:+}, {:+}, {:+})",
                        ws.snap_units, applied[0], applied[1], applied[2]
                    );
                } else if let Some(rejection) = brush_preview_rejection(&preview) {
                    ws.status = rejection.message(ws.snap_units);
                }
                return;
            }
            let raw_axis_delta = if extrude.axis == 1 {
                // Vertical faces follow pixel drag (up = out for +Y).
                (extrude.press_y - pointer.y) * EXTRUDE_UNITS_PER_PIXEL
            } else {
                // Horizontal faces follow the ground-plane pointer along
                // the face's dominant axis.
                match ws.brush_ground_point_raw(frame.rect, pointer) {
                    Some(ground) => ground[extrude.axis] - extrude.press_ground[extrude.axis],
                    None => return,
                }
            };
            let mut delta = [0i32; 3];
            delta[extrude.axis] = absolute_grid_translation_delta(
                extrude.snap_anchor[extrude.axis],
                f64::from(raw_axis_delta),
                ws.snap_units,
            );
            if delta == extrude.applied {
                return;
            }
            let mut preview = extrude.base.clone();
            translate_face_locked(&mut preview, extrude.face, delta, ws.brush_texture_lock);
            if brush_preview_ok(&preview) {
                ws.project.active_scene_mut().brushes[extrude.index] = preview;
                if let Some(state) = ws.brush_extrude.as_mut() {
                    state.applied = delta;
                }
                ws.status = format!(
                    "Moved face on Grid {} ({:+}, {:+}, {:+})",
                    ws.snap_units, delta[0], delta[1], delta[2]
                );
            } else if let Some(rejection) = brush_preview_rejection(&preview) {
                ws.status = rejection.message(ws.snap_units);
            }
        } else if let Some(drag) = ws.brush_drag {
            if drag.stage == BrushCreateStage::Height && drag.height_dragging {
                ws.update_brush_height_drag(pointer.y);
            } else if drag.stage == BrushCreateStage::Footprint {
                if let Some(point) = ws.brush_ground_point(frame.rect, pointer) {
                    ws.brush_drag = Some(BrushDrag {
                        current: point,
                        ..drag
                    });
                }
            }
        }
    }

    fn primary_released(&self, ws: &mut EditorWorkspace, _frame: &ToolFrame3d) {
        let synthesize_click = ws.brush_release_was_noop_click() && ws.brush_drag.is_none();
        let committed = ws.commit_brush_face_extrude_new()
            || ws.commit_brush_element_transform()
            || ws.commit_brush_move_preview()
            || ws.commit_brush_vertex_drag_preview()
            || ws.commit_brush_extrude_preview();
        if !committed {
            ws.finish_brush_creation_gesture();
        }
        if synthesize_click {
            self.primary_clicked(ws, _frame);
        }
    }

    fn primary_clicked(&self, ws: &mut EditorWorkspace, frame: &ToolFrame3d) {
        if ws.brush_vertex_snap_key_down {
            return;
        }
        if ws
            .brush_drag
            .is_some_and(|drag| drag.stage == BrushCreateStage::Height)
        {
            ws.commit_brush_drag();
            return;
        }
        if ws.active_tool == ViewTool::Brush {
            return;
        }
        let Some(pointer) = frame.pointer_interact.or(frame.pointer_hover) else {
            return;
        };
        // Clip mode: clicks place clip points on brush faces.
        if ws.brush_edit_mode == BrushEditMode::Clip && ws.selected_brush.is_some() {
            ws.brush_clip_click_3d(frame.rect, pointer);
            return;
        }
        // Sub-element clicks first, mirroring SelectTool (a click never
        // fires drag-start, so this is the only handle-selection path).
        if ws.selected_brush.is_some() && ws.brush_edit_mode != BrushEditMode::Move {
            if let Some((_, handle)) = ws.pick_brush_handle_3d(frame.rect, pointer) {
                ws.apply_brush_element_selection(handle.element(), frame.modifiers);
                return;
            }
        }
        match ws.pick_brush_face_cycled_for_selection_3d(frame.rect, pointer) {
            Some((index, face, _)) => {
                if !matches!(ws.brush_group_pick(index), BrushGroupPick::Brush) {
                    ws.select_brush_with_group_semantics(index, Some(face), frame.modifiers, false);
                    return;
                }
                if matches!(
                    ws.brush_edit_mode,
                    BrushEditMode::Face | BrushEditMode::Edge
                ) {
                    let (index, face) = ws
                        .pick_brush_face_nearest_for_selection_3d(frame.rect, pointer)
                        .map(|(brush, face, _)| (brush, face))
                        .unwrap_or((index, face));
                    if ws.select_brush_element_from_3d_hit(
                        index,
                        face,
                        frame.rect,
                        pointer,
                        frame.modifiers,
                    ) {
                        return;
                    }
                }
                ws.select_brush_with_group_semantics(index, Some(face), frame.modifiers, false);
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
