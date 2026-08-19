use super::*;

#[derive(Clone, Copy)]
struct CharacterControllerOverlay {
    origin: [f32; 3],
    settings: CharacterControllerSettings,
}

impl EditorWorkspace {
    /// Optional clean scene render from the selected gameplay Camera.
    pub fn selected_camera_preview_request(&self) -> Option<EditorCameraPreviewRequest> {
        const EDITOR_PLAYTEST_CAMERA_START_YAW_Q12: u16 = 220;

        let scene = self.project.active_scene();
        let selected = scene.node(self.selection.selected_node)?;
        let NodeKind::Camera { settings } = selected.kind else {
            return None;
        };
        let host_id = selected.parent?;
        let host = scene.node(host_id)?;
        if !matches!(host.kind, NodeKind::Entity) {
            return None;
        }

        let room = self.active_room_id();
        let floor = psxed_project::floor_view::node_floor(scene, host_id);
        let settings = settings.normalized();
        let player = room
            .and_then(|room_id| {
                let NodeKind::Section { grid } = &scene.node(room_id)?.kind else {
                    return None;
                };
                let floor_grid = grid.floor(floor)?;
                let mut origin = psxed_project::spatial::floor_anchored_node_preview_origin(
                    floor_grid,
                    &host.transform,
                );
                origin[1] = origin[1]
                    .saturating_add(psxed_project::floor_view::floor_offset(grid, floor, floor));
                Some(origin)
            })
            .unwrap_or_else(|| host.transform.translation.map(round_to_i32));
        let target = [
            player[0],
            player[1].saturating_add(settings.target_height),
            player[2],
        ];
        let vertical = settings.height.saturating_sub(settings.target_height);
        let runtime_pitch = camera_pitch_q12_from_vertical_distance(vertical, settings.distance);
        let orbit_pitch = ((-(runtime_pitch as i32)) & 0x0FFF) as u16;

        Some(EditorCameraPreviewRequest {
            camera: ViewportCameraState {
                mode: ViewportCameraMode::Orbit,
                yaw_q12: EDITOR_PLAYTEST_CAMERA_START_YAW_Q12,
                pitch_q12: orbit_pitch,
                radius: settings.distance.max(1),
                target,
                position: target,
            },
            active_room: room,
            active_floor: floor,
        })
    }

    /// 3D viewport body -- paints the HwRenderer texture into the
    /// central area's working space and turns pointer input into
    /// camera updates. Called from `draw_viewport` when the
    /// user has toggled the 2D / 3D switch on the toolbar to 3D.
    pub(crate) fn draw_viewport_3d_body(
        &mut self,
        ui: &mut egui::Ui,
        viewport_3d: EditorViewport3dPresentation,
    ) {
        let (rect, response) =
            allocate_centered_preview_rect(ui, "viewport_3d_canvas", egui::Sense::click_and_drag());
        let dnd_active = egui::DragAndDrop::has_any_payload(ui.ctx());
        let resource_drop_hovered = response.dnd_hover_payload::<ResourceId>().is_some();
        let prefab_drop_hovered = response.dnd_hover_payload::<PrefabDragPayload>().is_some();

        // Sims-style: primary button always belongs to the active
        // tool -- click-and-drag floors / walls / entities into the
        // world. Camera movement lives on middle / secondary so the
        // user can reframe mid-edit without giving up the tool.
        let camera_drag = !dnd_active
            && (response.dragged_by(egui::PointerButton::Middle)
                || response.dragged_by(egui::PointerButton::Secondary));
        if camera_drag {
            let (delta, shift) = ui.input(|input| (input.pointer.delta(), input.modifiers.shift));
            if shift {
                self.pan_viewport_3d_camera(delta, rect.size());
            } else {
                self.rotate_viewport_3d_camera(delta);
            }
        }

        // Hover tracking: every frame the pointer is over the panel,
        // ray-pick which cell it's on so the renderer can stamp a
        // translucent overlay there. Cleared when the pointer leaves.
        // For PaintWall, ALSO track which edge of the cell the pointer
        // is closest to so the renderer can preview the targeted wall
        // edge before the click.
        let hover_world = response
            .hover_pos()
            .and_then(|pointer| self.pick_3d_world(rect, pointer));
        let hover_room = self
            .active_room_id()
            .filter(|id| !self.scene_node_effectively_hidden(*id))
            .or_else(|| {
                self.project
                    .active_scene()
                    .nodes()
                    .iter()
                    .find(|n| {
                        matches!(n.kind, NodeKind::Section { .. })
                            && !self.scene_node_effectively_hidden(n.id)
                    })
                    .map(|n| n.id)
            });
        let paint_tool = matches!(
            self.active_tool,
            ViewTool::PaintFloor
                | ViewTool::PaintWall
                | ViewTool::PaintCeiling
                | ViewTool::PaintMaterial
                | ViewTool::Water
                | ViewTool::Erase
                | ViewTool::Place
        );
        let select_tool = matches!(self.active_tool, ViewTool::Select);
        let select_drag_active = matches!(
            self.interaction,
            Interaction::PrimitiveHeight(_)
                | Interaction::PrimitiveGrid(_)
                | Interaction::PrimitiveGizmo(_)
                | Interaction::NodeGizmo(_)
                | Interaction::Node(_)
                | Interaction::BoxSelect3d(_)
        );
        let pointer_target = if select_tool && select_drag_active {
            None
        } else {
            response
                .hover_pos()
                .or_else(|| response.interact_pointer_pos())
                .and_then(|pointer| {
                    self.resolve_viewport_3d_pointer_target(
                        rect,
                        pointer,
                        hover_room,
                        select_tool && !dnd_active,
                    )
                })
        };
        self.selection.hovered_brush_handle = None;
        if response.hovered()
            && (matches!(self.active_tool, ViewTool::Brush)
                || (select_tool && self.selected_brush.is_some()))
        {
            if let Some(pointer) = response.hover_pos() {
                let hovered_handle = self.pick_brush_handle_3d(rect, pointer);
                self.selection.hovered_brush_handle =
                    hovered_handle.map(|(_, handle)| handle.element());
                let over_brush = self
                    .pick_brush_face_nearest_for_selection_3d(rect, pointer)
                    .is_some();
                if hovered_handle.is_some()
                    || (self.brush_edit_mode == BrushEditMode::Move && over_brush)
                {
                    ui.ctx().set_cursor_icon(match self.brush_edit_mode {
                        BrushEditMode::Move => egui::CursorIcon::Grab,
                        BrushEditMode::Face => egui::CursorIcon::ResizeVertical,
                        BrushEditMode::Edge | BrushEditMode::Vertex | BrushEditMode::Clip => {
                            egui::CursorIcon::Crosshair
                        }
                    });
                }
            }
        }
        let hover_entity_hit = pointer_target.and_then(|target| target.entity_hit());
        // Face hover ray-tests every floor / wall / ceiling in the
        // active Room and reports the closest hit. Used by Select
        // for the outline UI, AND by paint tools to anchor their
        // dispatch onto the actual face the user clicked rather
        // than the floor-plane projection (which lies under wall
        // surfaces and gets the wrong cell for back-row clicks).
        let face_hit = pointer_target.and_then(|target| target.face_hit());
        // Hover-track via the same target resolver used for clicks
        // and drags, so foreground gizmos/entities consume the
        // pointer before scene faces behind them can highlight.
        self.selection.hovered_primitive = if self.portal_place_active() {
            None
        } else {
            pointer_target.and_then(|target| target.primitive_selection())
        };
        // Paint preview: world-cell coords let the ghost outline
        // appear over cells outside the current grid, exactly
        // where the auto-grow would create them.
        self.paint_target_preview = if paint_tool {
            hover_room.and_then(|room| {
                let paint_world = response
                    .hover_pos()
                    .and_then(|pointer| self.pick_3d_paint_world(rect, pointer, room));
                self.compute_paint_target_preview(room, face_hit, paint_world)
            })
        } else {
            None
        };
        if let Some(room) = self.floating_geometry.as_ref().map(|preview| preview.room) {
            if let Some(origin) = self.floating_origin_from_3d_hover(room, face_hit, hover_world) {
                self.track_floating_geometry_pointer_origin(origin);
            }
        }
        let dropped_resource = resource_drop_hovered
            .then(|| response.dnd_release_payload::<ResourceId>())
            .flatten()
            .map(|payload| *payload);
        let dropped_prefab = prefab_drop_hovered
            .then(|| response.dnd_release_payload::<PrefabDragPayload>())
            .flatten()
            .map(|payload| payload.path.clone());
        if self.floating_geometry.is_none() {
            if let Some(resource_id) = dropped_resource {
                let bsp_drop = self.active_room_id().is_none()
                    && self.bsp_authoring_root().is_some()
                    && response.hover_pos().is_some();
                if bsp_drop {
                    if let Some(pointer) = response.hover_pos() {
                        self.drop_resource_bsp_3d(resource_id, rect, pointer);
                    }
                } else {
                    self.drop_resource_3d(resource_id, face_hit, hover_world);
                }
            } else if let Some(path) = dropped_prefab {
                self.drop_prefab_3d(&path, face_hit, hover_world);
            }
        }

        // Primary click / drag: ray-pick the cell under the cursor
        // and dispatch to the active tool. Click starts a fresh
        // drag; drag fires every frame the pointer moves; per-cell
        // dedupe keeps walls / placements from stacking when the
        // pointer dwells inside the same cell across frames.
        if !dnd_active {
            if self.floating_geometry.is_some() {
                if response.clicked_by(egui::PointerButton::Primary) {
                    self.commit_floating_geometry();
                }
                if response.clicked_by(egui::PointerButton::Secondary) {
                    self.cancel_floating_geometry();
                }
                egui::Image::new((viewport_3d.texture, rect.size()))
                    .uv(viewport_3d.uv)
                    .paint_at(ui, rect);
                Self::draw_viewport_3d_overlay_lines(&ui.painter_at(rect), rect, &viewport_3d);
                return;
            }

            if response.drag_started_by(egui::PointerButton::Primary)
                || response.clicked_by(egui::PointerButton::Primary)
            {
                self.last_paint_stamp = None;
                self.brush_face_paint_stroke = false;
            }

            // Hover-track entity bounds in Select mode so the
            // overlay can highlight the bound under the cursor
            // before the user clicks.
            if select_tool {
                self.selection.hovered_entity_node = hover_entity_hit.map(|hit| hit.node);
            } else {
                self.selection.hovered_entity_node = None;
            }

            // Tool dispatch: translate this frame's primary-button state
            // into `ToolFrame3d` events for the active tool object
            // (workspace/tools.rs). Select keeps its drag flows and
            // lazy-undo semantics; paint tools keep click-or-drag
            // painting; brush tools plug in as new arms of
            // `tools::tool_impl_3d` without growing this function.
            let frame = tools::ToolFrame3d {
                rect,
                pointer_interact: response.interact_pointer_pos(),
                pointer_hover: response.hover_pos(),
                modifiers: ui.input(|input| input.modifiers),
                pointer_target,
                hover_room,
                drag_delta_y: response.drag_delta().y,
            };
            let tool = tools::tool_impl_3d(self.active_tool);
            if response.drag_started_by(egui::PointerButton::Primary) {
                let press_pointer = ui
                    .input(|input| input.pointer.press_origin())
                    .or(frame.pointer_interact);
                let mut press_frame = frame;
                press_frame.pointer_interact = press_pointer;
                press_frame.pointer_hover = press_pointer;
                press_frame.pointer_target = press_pointer.and_then(|pointer| {
                    self.resolve_viewport_3d_pointer_target(
                        rect,
                        pointer,
                        hover_room,
                        select_tool && !dnd_active,
                    )
                });
                tool.primary_pressed(self, &press_frame);
            }
            if response.dragged_by(egui::PointerButton::Primary) {
                tool.primary_dragged(self, &frame);
            }
            if response.drag_stopped_by(egui::PointerButton::Primary) {
                tool.primary_released(self, &frame);
            }
            if response.clicked_by(egui::PointerButton::Primary) {
                tool.primary_clicked(self, &frame);
            }
        } else {
            self.selection.hovered_entity_node = None;
        }

        if response.hovered() {
            self.brush_tool_keyboard(ui);
            self.update_free_camera_keyboard(ui);
            let (scroll, shift) = ui.input(|i| (i.raw_scroll_delta, i.modifiers.shift));
            // Shift+scroll drills the selection through overlapping
            // brushes under the cursor instead of moving the camera.
            // Trackpads report shifted scrolls on the x axis, so take
            // the dominant component.
            let drill_delta = if scroll.y.abs() >= scroll.x.abs() {
                scroll.y
            } else {
                scroll.x
            };
            if shift && drill_delta.abs() > f32::EPSILON {
                if let Some(pointer) = response.hover_pos() {
                    self.drill_selection_3d(rect, pointer, if drill_delta < 0.0 { 1 } else { -1 });
                }
            } else if scroll.y.abs() > f32::EPSILON {
                self.scroll_viewport_3d_camera(scroll.y);
            }
        }

        let hovered_primitive_axis = pointer_target.and_then(|target| target.primitive_axis());
        let hovered_node_handle = pointer_target.and_then(|target| target.node_handle());

        egui::Image::new((viewport_3d.texture, rect.size()))
            .uv(viewport_3d.uv)
            .paint_at(ui, rect);
        let painter = ui.painter_at(rect);
        Self::draw_viewport_3d_overlay_lines(&painter, rect, &viewport_3d);
        self.draw_character_behavior_overlay(&painter, rect);
        self.draw_primitive_gizmo(&painter, rect, hovered_primitive_axis);
        self.draw_node_gizmo(&painter, rect, hovered_node_handle);
        draw_viewport_box_select_marquee(&painter, self.viewport_3d_box_select_rect());
        self.draw_brush_overlay(&painter, rect);
        if resource_drop_hovered || prefab_drop_hovered {
            painter.rect_stroke(
                rect.shrink(2.0),
                2.0,
                Stroke::new(EDITOR_OUTLINE_STROKE_WIDTH, EDITOR_OUTLINE_ACCENT),
                StrokeKind::Inside,
            );
            painter.text(
                rect.center_top() + Vec2::new(0.0, 16.0),
                Align2::CENTER_TOP,
                if prefab_drop_hovered {
                    "Drop prefab into scene"
                } else {
                    "Drop resource into scene"
                },
                FontId::proportional(13.0),
                STUDIO_ACCENT,
            );
        }
    }

    pub(crate) fn rotate_viewport_3d_camera(&mut self, delta: Vec2) {
        self.camera_rig.rotate(delta);
        self.persist_editor_camera_state();
    }

    pub(crate) fn pan_viewport_3d_camera(&mut self, delta: Vec2, panel_size: Vec2) {
        self.camera_rig.pan(delta, panel_size);
        self.persist_editor_camera_state();
    }

    pub(crate) fn scroll_viewport_3d_camera(&mut self, scroll: f32) {
        self.camera_rig.scroll(scroll);
        self.persist_editor_camera_state();
    }

    pub(crate) fn update_free_camera_keyboard(&mut self, ui: &egui::Ui) {
        if self.camera_rig.mode != ViewportCameraMode::Free {
            return;
        }
        if ui.ctx().memory(|memory| memory.focused().is_some()) {
            return;
        }

        // WASD only: Q/E are deliberately unbound here so E can be the
        // face-extrude key everywhere (vertical flight comes from aiming
        // the camera and flying forward).
        let (forward, right, speed) = ui.input(|input| {
            let axis = |positive: egui::Key, negative: egui::Key| {
                (input.key_down(positive) as i8 - input.key_down(negative) as i8) as f32
            };
            let speed = if input.modifiers.shift { 512.0 } else { 128.0 };
            (
                axis(egui::Key::W, egui::Key::S),
                axis(egui::Key::D, egui::Key::A),
                speed,
            )
        });
        if forward.abs() <= f32::EPSILON && right.abs() <= f32::EPSILON {
            return;
        }

        self.camera_rig
            .move_free_local(forward * speed, right * speed, 0.0);
        self.persist_editor_camera_state();
        ui.ctx().request_repaint();
    }

    pub(crate) fn set_viewport_3d_camera_mode(&mut self, mode: ViewportCameraMode) {
        if self.camera_rig.set_mode(mode) {
            self.persist_editor_camera_state();
            self.mark_shortcut_group_changed(ShortcutGroup::Camera);
        }
    }

    pub(crate) fn draw_viewport_3d_overlay_lines(
        painter: &egui::Painter,
        rect: Rect,
        viewport_3d: &EditorViewport3dPresentation,
    ) {
        let source = viewport_3d.overlay_source_size;
        if source.x <= 0.0 || source.y <= 0.0 {
            return;
        }
        let to_screen = |p: Pos2| {
            Pos2::new(
                rect.left() + (p.x / source.x) * rect.width(),
                rect.top() + (p.y / source.y) * rect.height(),
            )
        };
        for line in &viewport_3d.overlay_lines {
            painter.line_segment(
                [to_screen(line.a), to_screen(line.b)],
                Stroke::new(line.width, line.color),
            );
        }
    }

    fn selected_character_controller_overlay(&self) -> Option<CharacterControllerOverlay> {
        let scene = self.project.active_scene();
        let selected = scene.node(self.selection.selected_node)?;
        let host_id = if matches!(selected.kind, NodeKind::Entity) {
            selected.id
        } else if matches!(selected.kind, NodeKind::CharacterController { .. }) {
            selected.parent?
        } else {
            return None;
        };
        let host = scene.node(host_id)?;
        let settings = self.character_controller_settings(host_id)?;
        let bounds = self
            .collect_entity_bounds(None)
            .into_iter()
            .find(|bounds| bounds.node == host_id)?;
        let mut origin = [
            bounds.center[0],
            bounds.center[1] - bounds.half_extents[1],
            bounds.center[2],
        ];
        if let Some(preview) = self
            .character_motion_preview()
            .filter(|preview| preview.entity == host_id)
        {
            origin = preview.origin.map(|value| value as f32);
        }
        Some(CharacterControllerOverlay { origin, settings })
    }

    fn draw_character_behavior_overlay(&self, painter: &egui::Painter, rect: Rect) {
        let Some(overlay) = self.selected_character_controller_overlay() else {
            return;
        };
        let camera = self.viewport_3d_camera();
        draw_character_capsule(
            painter,
            rect,
            camera,
            overlay.origin,
            f32::from(overlay.settings.radius),
            f32::from(overlay.settings.height),
        );
        let Some(enemy) = overlay.settings.enemy else {
            return;
        };
        draw_world_xz_ring(
            painter,
            rect,
            camera,
            overlay.origin,
            f32::from(enemy.aggro_radius),
            Stroke::new(1.8, Color32::from_rgb(83, 165, 255)),
        );

        let preferred = f32::from(enemy.preferred_distance);
        let tolerance = f32::from(enemy.spacing_tolerance);
        let inner = (preferred - tolerance).max(0.0);
        let outer = preferred + tolerance;
        if inner > 0.0 {
            draw_world_xz_ring(
                painter,
                rect,
                camera,
                overlay.origin,
                inner,
                Stroke::new(1.25, Color32::from_rgb(225, 178, 92)),
            );
        }
        draw_world_xz_ring(
            painter,
            rect,
            camera,
            overlay.origin,
            outer,
            Stroke::new(1.25, Color32::from_rgb(225, 178, 92)),
        );

        let patrol = [
            overlay.origin[0] + enemy.patrol_offset[0] as f32,
            overlay.origin[1] + enemy.patrol_offset[1] as f32,
            overlay.origin[2] + enemy.patrol_offset[2] as f32,
        ];
        if patrol != overlay.origin {
            if let (Some(start), Some(end)) = (
                project_world_to_viewport_screen(camera, rect, overlay.origin),
                project_world_to_viewport_screen(camera, rect, patrol),
            ) {
                let color = Color32::from_rgb(108, 220, 171);
                painter.line_segment([start, end], Stroke::new(1.75, color));
                painter.circle_filled(end, 4.0, color);
                painter.circle_stroke(end, 8.0, Stroke::new(1.25, color));
                painter.text(
                    end + Vec2::new(8.0, -8.0),
                    Align2::LEFT_BOTTOM,
                    "Patrol",
                    FontId::proportional(11.0),
                    color,
                );
            }
        }

        let legend_pos = rect.left_bottom() + Vec2::new(12.0, -10.0);
        painter.text(
            legend_pos,
            Align2::LEFT_BOTTOM,
            format!("Aggro {}", enemy.aggro_radius),
            FontId::monospace(11.0),
            Color32::from_rgb(83, 165, 255),
        );
        painter.text(
            legend_pos + Vec2::new(0.0, -15.0),
            Align2::LEFT_BOTTOM,
            format!(
                "Preferred {} ± {}",
                enemy.preferred_distance, enemy.spacing_tolerance
            ),
            FontId::monospace(11.0),
            Color32::from_rgb(225, 178, 92),
        );
    }

    /// Play-mode 3D body -- paints the live emulator framebuffer into
    /// the viewport and suppresses all authoring hit-testing.
    pub(crate) fn record_play_frame_time(&mut self, metrics: EditorPlaytestMetrics) {
        if self.play_frame_last_sample_serial == Some(metrics.sample_serial) {
            return;
        }
        self.play_frame_last_sample_serial = Some(metrics.sample_serial);
        for &frame_ms in metrics
            .visual_frame_times_ms
            .iter()
            .take(metrics.visual_frame_time_count as usize)
        {
            if !frame_ms.is_finite() || frame_ms <= 0.0 {
                continue;
            }
            if self.play_frame_times_ms.len() >= PLAY_FRAME_HISTORY_CAP {
                self.play_frame_times_ms.pop_front();
            }
            self.play_frame_times_ms
                .push_back(frame_ms.clamp(0.0, 120.0));
        }
    }

    pub(crate) fn draw_viewport_3d_play_body(
        &mut self,
        ui: &mut egui::Ui,
        viewport_3d: EditorViewport3dPresentation,
        playtest_status: EditorPlaytestStatus,
    ) {
        let captured = matches!(
            playtest_status,
            EditorPlaytestStatus::Running {
                input_captured: true
            }
        );
        let (rect, response) =
            allocate_centered_preview_rect(ui, "viewport_3d_play_canvas", egui::Sense::click());
        egui::Image::new((viewport_3d.texture, rect.size()))
            .uv(viewport_3d.uv)
            .paint_at(ui, rect);

        let control_size = Vec2::splat(28.0);
        let control_gap = 6.0;
        let controls_origin = rect.left_top() + Vec2::new(8.0, 8.0);
        let visibility_rect = Rect::from_min_size(controls_origin, control_size);
        let wireframe_rect = Rect::from_min_size(
            visibility_rect.left_bottom() + Vec2::new(0.0, control_gap),
            control_size,
        );
        let record_rect = Rect::from_min_size(
            wireframe_rect.left_bottom() + Vec2::new(0.0, control_gap),
            control_size,
        );
        let replay_rect = Rect::from_min_size(
            record_rect.left_bottom() + Vec2::new(0.0, control_gap),
            control_size,
        );
        let dump_rect = Rect::from_min_size(
            replay_rect.left_bottom() + Vec2::new(0.0, control_gap),
            control_size,
        );
        let controls_rect = Rect::from_min_max(visibility_rect.min, dump_rect.max);
        let recording = viewport_3d.play_tape.mode == EditorPlaytestTapeMode::Recording;
        let replaying = viewport_3d.play_tape.mode == EditorPlaytestTapeMode::Replaying;
        let can_record = !replaying;
        let can_replay = !recording;

        let show_debug_overlays = self.show_play_debug_overlays;
        let show_debug_map = self.show_play_debug_map;
        let debug_rect = Rect::from_min_size(
            rect.left_top() + Vec2::new(44.0, 8.0),
            Vec2::new(320.0, 171.0),
        );
        let clicked_overlay = response.interact_pointer_pos().is_some_and(|pos| {
            controls_rect.contains(pos) || (show_debug_overlays && debug_rect.contains(pos))
        });
        if response.clicked() && !clicked_overlay {
            self.pending_playtest_request = Some(EditorPlaytestRequest::CaptureInput);
        }

        let painter = ui.painter_at(rect);
        if !captured {
            painter.rect_filled(rect, 0.0, Color32::from_black_alpha(112));
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Click to capture game input",
                FontId::proportional(16.0),
                STUDIO_TEXT,
            );
        }
        if show_debug_overlays {
            if let Some(metrics) = viewport_3d.play_metrics {
                self.record_play_frame_time(metrics);
            }
            painter.rect_filled(debug_rect, 4.0, Color32::from_black_alpha(164));
            let mut y = debug_rect.top() + 7.0;
            draw_play_metric_line(
                &painter,
                debug_rect.left() + 8.0,
                &mut y,
                "Play profiler",
                STUDIO_TEXT,
            );
            if let Some(metrics) = viewport_3d.play_metrics {
                let visual_hz = metrics.visual_hz.unwrap_or(metrics.draw_hz);
                let frame_ms = self
                    .play_frame_times_ms
                    .back()
                    .copied()
                    .unwrap_or(metrics.frame_ms);
                draw_play_metric_line(
                    &painter,
                    debug_rect.left() + 8.0,
                    &mut y,
                    &format!("HOST {:>5.1} fps", metrics.host_fps),
                    STUDIO_TEXT_WEAK,
                );
                draw_play_metric_line(
                    &painter,
                    debug_rect.left() + 8.0,
                    &mut y,
                    &format!(
                        "VIS {:>5.1}Hz {:>5.1}ms M/L {}/{}",
                        visual_hz,
                        frame_ms,
                        metrics.visual_deadline_misses,
                        metrics.visual_lateness_vblanks
                    ),
                    STUDIO_TEXT,
                );
                draw_play_metric_line(
                    &painter,
                    debug_rect.left() + 8.0,
                    &mut y,
                    &format!(
                        "AVG {:>5.1} ms  EMU/HW/UI {:>4.1}/{:>4.1}/{:>4.1}",
                        metrics.total_ms, metrics.emu_ms, metrics.hw_ms, metrics.ui_ms
                    ),
                    STUDIO_TEXT_WEAK,
                );
                draw_play_metric_line(
                    &painter,
                    debug_rect.left() + 8.0,
                    &mut y,
                    &format!(
                        "TASK fix {:>4.1}/{:>4.1}  vis {:>4.1}/{:>4.1} ms",
                        metrics.fixed_update_task_ms,
                        metrics.fixed_update_task_max_ms,
                        metrics.visual_render_task_ms,
                        metrics.visual_render_task_max_ms
                    ),
                    STUDIO_TEXT,
                );
                draw_play_metric_line(
                    &painter,
                    debug_rect.left() + 8.0,
                    &mut y,
                    &format!(
                        "PORT vis {:>2} front {:>2} tests {:>2} rej {:>2}",
                        metrics.portal_visible_rooms,
                        metrics.portal_frontier_rooms,
                        metrics.portal_tests,
                        metrics.portal_rejects.iter().copied().sum::<u32>()
                    ),
                    STUDIO_TEXT,
                );
                draw_play_metric_line(
                    &painter,
                    debug_rect.left() + 8.0,
                    &mut y,
                    &format!(
                        "STRM {:>2}/{:<2} load {:>2} evict {:>2} pre {:>2}",
                        metrics.chunk_loaded,
                        metrics.stream_slot_limit,
                        metrics.stream_pending,
                        metrics.stream_evictions,
                        metrics.stream_prefetches
                    ),
                    STUDIO_TEXT_WEAK,
                );
                // Correctness / over-budget signals: every value should sit at 0
                // on a healthy stream. The line lights up when streaming breaks
                // (visible geometry not resident/built) or the resident budget
                // is exceeded (more high-priority rooms than slots).
                let stream_warnings = metrics.portal_missing_resident
                    + metrics.portal_build_failed
                    + metrics.stream_failed
                    + metrics.stream_protected_full;
                let warn_color = if stream_warnings > 0 {
                    Color32::from_rgb(255, 120, 120)
                } else {
                    STUDIO_TEXT_WEAK
                };
                draw_play_metric_line(
                    &painter,
                    debug_rect.left() + 8.0,
                    &mut y,
                    &format!(
                        "WARN miss {:>2} bfail {:>2} fail {:>2} full {:>2}",
                        metrics.portal_missing_resident,
                        metrics.portal_build_failed,
                        metrics.stream_failed,
                        metrics.stream_protected_full
                    ),
                    warn_color,
                );
                let chart_rect = Rect::from_min_size(
                    Pos2::new(debug_rect.left() + 8.0, y + 2.0),
                    Vec2::new(debug_rect.width() - 16.0, 42.0),
                );
                draw_play_frame_rate_chart(&painter, chart_rect, &self.play_frame_times_ms);
            } else {
                draw_play_metric_line(
                    &painter,
                    debug_rect.left() + 8.0,
                    &mut y,
                    "collecting...",
                    STUDIO_TEXT_WEAK,
                );
            }
            y = (debug_rect.bottom() - 18.0).max(y);
            let tape_line = match viewport_3d.play_tape.mode {
                EditorPlaytestTapeMode::Idle if viewport_3d.play_tape.frames == 0 => {
                    "Tape empty".to_string()
                }
                EditorPlaytestTapeMode::Idle => {
                    format!("Tape {:>5} fr", viewport_3d.play_tape.frames)
                }
                EditorPlaytestTapeMode::Recording => {
                    format!("Rec  {:>5} fr", viewport_3d.play_tape.frames)
                }
                EditorPlaytestTapeMode::Replaying => format!(
                    "Replay {:>5}/{:<5}",
                    viewport_3d.play_tape.cursor, viewport_3d.play_tape.frames
                ),
            };
            draw_play_metric_line(
                &painter,
                debug_rect.left() + 8.0,
                &mut y,
                &tape_line,
                STUDIO_TEXT,
            );
        }
        if show_debug_map {
            if let Some(metrics) = viewport_3d.play_metrics {
                draw_play_chunk_debug_map(
                    &painter,
                    rect,
                    &self.project,
                    metrics,
                    self.play_debug_map_view,
                    self.active_floor,
                );
            }
        }
        self.draw_play_overlay_visibility_menu(ui, visibility_rect);
        if draw_play_overlay_icon_button(
            ui,
            wireframe_rect,
            "play_wireframe_toggle",
            icons::GRID,
            if viewport_3d.play_wireframe {
                "Disable wireframe view"
            } else {
                "Enable wireframe view (polygon edges only)"
            },
            viewport_3d.play_wireframe,
            true,
            Some(STUDIO_ACCENT_DIM),
        ) {
            self.pending_playtest_request = Some(EditorPlaytestRequest::SetWireframe {
                enabled: !viewport_3d.play_wireframe,
            });
        }
        if draw_play_overlay_icon_button(
            ui,
            record_rect,
            "play_input_record_toggle",
            icons::CIRCLE_DOT,
            if recording {
                "Stop and save input plus whole-run profile"
            } else {
                "Record embedded play input plus whole-run profile"
            },
            recording,
            can_record,
            Some(Color32::from_rgb(92, 34, 34)),
        ) {
            self.pending_playtest_request = Some(if recording {
                EditorPlaytestRequest::StopInputRecording
            } else {
                EditorPlaytestRequest::StartInputRecording
            });
        }
        if draw_play_overlay_icon_button(
            ui,
            replay_rect,
            "play_input_replay_toggle",
            if replaying {
                icons::SQUARE
            } else {
                icons::PLAY
            },
            if replaying {
                "Stop replaying input"
            } else {
                "Replay saved input and capture a deterministic profile"
            },
            replaying,
            can_replay,
            Some(STUDIO_ACCENT_DIM),
        ) {
            self.pending_playtest_request = Some(if replaying {
                EditorPlaytestRequest::StopInputReplay
            } else {
                EditorPlaytestRequest::StartInputReplay
            });
        }
        if draw_play_overlay_icon_button(
            ui,
            dump_rect,
            "play_profiler_history_dump",
            icons::FILE,
            "Dump last profiler frames",
            false,
            true,
            Some(STUDIO_ACCENT_DIM),
        ) {
            self.pending_playtest_request = Some(EditorPlaytestRequest::DumpProfilerHistory);
        }
    }

    pub(crate) fn draw_play_overlay_visibility_menu(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let icon = if !self.show_play_debug_overlays || !self.show_play_debug_map {
            icons::EYE_OFF
        } else {
            icons::EYE
        };
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect), |ui| {
            ui.set_min_size(rect.size());
            let response = egui::menu::menu_custom_button(
                ui,
                egui::Button::new(icons::text(icon, 14.0)).min_size(rect.size()),
                |ui| {
                    ui.set_min_width(190.0);
                    let mut changed = false;
                    egui::Grid::new("play-overlay-visibility-menu-grid")
                        .num_columns(2)
                        .spacing(Vec2::new(14.0, 5.0))
                        .show(ui, |ui| {
                            changed |= visibility_menu_row(
                                ui,
                                "play-overlay-profiler",
                                "Profiler",
                                &mut self.show_play_debug_overlays,
                            );
                            changed |= visibility_menu_row(
                                ui,
                                "play-overlay-map",
                                "Portal map",
                                &mut self.show_play_debug_map,
                            );
                            ui.label("Map view");
                            ui.horizontal(|ui| {
                                for view in PlayDebugMapView::ALL {
                                    ui.selectable_value(
                                        &mut self.play_debug_map_view,
                                        view,
                                        view.label(),
                                    );
                                }
                            });
                        });
                    if changed {
                        self.persist_editor_visibility_state();
                    }
                },
            )
            .response;
            response.on_hover_text("Visibility");
        });
    }

    /// Snapshot of the 3D camera the frontend needs to drive the
    /// editor's HwRenderer this frame.
    pub fn viewport_3d_camera(&self) -> ViewportCameraState {
        let mut camera = self.camera_rig.camera();
        let Some(preview) = self.character_motion_preview() else {
            return camera;
        };
        let settings = self.character_controller_settings(preview.entity);
        let height = settings.map_or(1024, |settings| i32::from(settings.height));
        let sector_size = self
            .project
            .world_sector_size_for_node(preview.entity)
            .max(1);
        camera.mode = ViewportCameraMode::Orbit;
        camera.yaw_q12 = self.camera_rig.yaw;
        camera.pitch_q12 = self.camera_rig.pitch;
        camera.target = [
            preview.origin[0],
            preview.origin[1].saturating_add(height / 2),
            preview.origin[2],
        ];
        let comfortable_radius = height.saturating_mul(3).max(sector_size.saturating_mul(2));
        camera.radius = camera.radius.min(comfortable_radius).max(height.max(512));
        camera
    }

    fn character_controller_settings(&self, entity: NodeId) -> Option<CharacterControllerSettings> {
        let scene = self.project.active_scene();
        let host = scene.node(entity)?;
        host.children.iter().find_map(|child| {
            scene.node(*child).and_then(|node| match &node.kind {
                // Resolve the same way the cook does, so the overlay draws the
                // capsule the game will actually use.
                NodeKind::CharacterController {
                    character, settings, ..
                } => settings.or_else(|| {
                    character
                        .and_then(|id| self.project.resource(id))
                        .and_then(|resource| match &resource.data {
                            psxed_project::ResourceData::Character(character) => {
                                Some(CharacterControllerSettings::from_character(character))
                            }
                            _ => None,
                        })
                }),
                _ => None,
            })
        })
    }

    /// Transient controller pose for the native preview renderer. Values are
    /// derived from Inspector settings and elapsed real time; project data is
    /// never modified.
    pub fn character_motion_preview(&self) -> Option<EditorCharacterMotionPreview> {
        let state = self.character_motion_preview?;
        let scene = self.project.active_scene();
        let selected = scene.node(self.selection.selected_node)?;
        let selected_host = if matches!(selected.kind, NodeKind::Entity) {
            selected.id
        } else if matches!(selected.kind, NodeKind::CharacterController { .. }) {
            selected.parent?
        } else {
            return None;
        };
        if selected_host != state.entity {
            return None;
        }
        let host = scene.node(state.entity)?;
        let settings = self.character_controller_settings(state.entity)?;
        let bounds = self
            .collect_entity_bounds(None)
            .into_iter()
            .find(|bounds| bounds.node == state.entity)?;
        let base_origin = [
            bounds.center[0].round() as i32,
            (bounds.center[1] - bounds.half_extents[1]).round() as i32,
            bounds.center[2].round() as i32,
        ];
        let base_yaw_q12 =
            psxed_project::spatial::euler_degrees_to_q12(host.transform.rotation_degrees[1]);
        let sector_size = self.project.world_sector_size_for_node(state.entity).max(1);
        let (offset, yaw_offset_q12) = character_motion_delta(
            state.action,
            settings,
            state.started_at.elapsed(),
            sector_size,
            base_yaw_q12,
        );
        Some(EditorCharacterMotionPreview {
            entity: state.entity,
            origin: [
                base_origin[0].saturating_add(offset[0]),
                base_origin[1].saturating_add(offset[1]),
                base_origin[2].saturating_add(offset[2]),
            ],
            yaw_q12: base_yaw_q12.wrapping_add(yaw_offset_q12),
            clip: state.clip,
        })
    }

    /// Whether the editor preview should visualize authored room fog.
    /// This is an editor-only view option; it does not change the
    /// room's cooked `fog_enabled` setting.
    pub fn preview_fog_enabled(&self) -> bool {
        self.preview_fog
    }

    /// Whether the editor preview should draw passive outlines for
    /// one-sided faces whose rendered side is currently culled.
    pub fn preview_backface_wireframe_enabled(&self) -> bool {
        self.preview_backface_wireframe
    }

    /// Whether the editor preview should draw entity/image/collision
    /// bounds. Picking stays enabled; this only hides the visual boxes.
    pub fn preview_bounds_enabled(&self) -> bool {
        self.preview_bounds
    }

    /// Whether the Grid toolbar toggle is currently on. In 2D this
    /// controls the editor's world-grid overlay; in 3D the frontend
    /// uses it to gate the streaming chunk-boundary overlay so the
    /// same button hides both grid-style affordances at once.
    pub fn show_grid_enabled(&self) -> bool {
        self.show_grid
    }

    pub fn show_portals_enabled(&self) -> bool {
        self.show_portals
    }

    pub fn show_lights_enabled(&self) -> bool {
        self.show_lights
    }

    /// Whether the central editor surface is currently showing the
    /// editable 3D viewport. The frontend uses this to avoid spending
    /// host frame time rebuilding the PSX-style preview while the user
    /// is in 2D map or Animation Viewer.
    pub fn editor_3d_preview_visible(&self) -> bool {
        self.active_workspace == WorkspaceView::Room && !self.view_2d
    }

    /// Currently-selected scene node. The frontend reads this so the
    /// 3D preview can highlight the selected entity.
    pub fn selected_node_id(&self) -> NodeId {
        self.selection.selected_node
    }

    /// Editor-local hidden scene nodes from the Scene tree eye toggles.
    /// The frontend preview uses this to keep 2D, picking, and 3D
    /// visibility consistent without serialising temporary editor state.
    pub fn hidden_scene_nodes(&self) -> &HashSet<NodeId> {
        &self.hidden_scene_nodes
    }

    /// Primitive under the 3D pointer when the Select tool is
    /// active -- face / edge / vertex of a floor, wall, or
    /// ceiling on the active Room. Frontend reads this every
    /// frame to draw a light hover outline.
    pub fn hovered_primitive(&self) -> Option<Selection> {
        self.selection.hovered_primitive
    }

    /// Primitive the user clicked with the Select tool. Frontend
    /// draws a bold outline; the inspector reads it to surface
    /// per-primitive editable fields.
    pub fn selected_primitive(&self) -> Option<Selection> {
        self.selection.selected_primitive
    }

    /// All selected grid primitives, excluding floor-tile sector
    /// selections which are exposed separately as floor faces.
    pub fn selected_primitives(&self) -> Vec<Selection> {
        self.selected_primitive_targets()
    }

    /// Grid primitives currently flagged by the last failed cook or
    /// playtest validation pass. The frontend draws these in red.
    pub fn validation_issue_primitives(&self) -> Vec<Selection> {
        self.validation_issue_primitives.clone()
    }

    /// World-space selected bounds for the 3D preview. Unlike
    /// viewport framing, this intentionally does not fall back to
    /// the active Room when nothing is selected.
    pub fn selected_bounds_3d(&self) -> Option<([f32; 3], [f32; 3])> {
        self.selected_frame_bounds_3d()
    }

    /// Selected cells expanded to every authored face they contain.
    /// 2D tile selection stores sector cells; the 3D preview,
    /// material tools, and drag code work on concrete face refs.
    pub fn selected_sector_faces(&self) -> Vec<FaceRef> {
        let mut sectors: Vec<_> = self.selection.selected_sectors.iter().copied().collect();
        sectors.sort_by_key(|(room, sx, sz)| (room.raw(), *sx, *sz));
        let mut faces = Vec::new();
        for (room, sx, sz) in sectors {
            let Some(grid) = self.room_grid_view(room) else {
                continue;
            };
            let Some(sector) = grid.sector(sx, sz) else {
                continue;
            };
            if sector.floor.is_some() {
                faces.push(FaceRef {
                    room,
                    sx,
                    sz,
                    kind: FaceKind::Floor,
                });
            }
            if sector.ceiling.is_some() {
                faces.push(FaceRef {
                    room,
                    sx,
                    sz,
                    kind: FaceKind::Ceiling,
                });
            }
            for dir in GridDirection::ALL {
                for (stack, _) in sector.walls.get(dir).iter().enumerate() {
                    let Ok(stack) = u8::try_from(stack) else {
                        continue;
                    };
                    faces.push(FaceRef {
                        room,
                        sx,
                        sz,
                        kind: FaceKind::Wall { dir, stack },
                    });
                }
            }
        }
        faces
    }

    /// Active selection mode (Face / Edge / Vertex).
    pub fn selection_mode(&self) -> SelectionMode {
        self.selection_mode
    }

    /// What the next paint click would target. Frontend reads
    /// this every frame for paint tools and outlines either a
    /// cell ghost (Floor / Ceiling / Erase / Place) or a wall
    /// ghost (PaintWall) at the world position the click would
    /// commit to.
    pub fn paint_target_preview(&self) -> Option<PaintTargetPreview> {
        self.paint_target_preview
    }

    /// Scene node whose 3D bounding box currently sits under
    /// the pointer (Select tool only). Frontend reads it each
    /// frame so the editor preview can highlight the box and
    /// the click handler can promote it to a selection.
    pub fn hovered_entity_node(&self) -> Option<NodeId> {
        self.selection.hovered_entity_node
    }

    /// Commit the most recent hover-tracked face to `selected_face`.
    /// Called from the click handler when the Select tool is active,
    /// independent of `dispatch_3d_tool`'s ground-plane sector
    /// requirement so wall / ceiling clicks register even when the
    /// ray-on-Y=0 hit lands beyond the room. Also surfaces the
    /// face's material in the resources panel so the user sees
    /// which material is on the picked surface.
    pub(crate) fn face_hit_for_paint_tool(
        &self,
        face_hit: Option<(FaceRef, [f32; 3])>,
    ) -> Option<(FaceRef, [f32; 3])> {
        match self.active_tool {
            ViewTool::PaintCeiling => {
                face_hit.filter(|(face, _)| matches!(face.kind, FaceKind::Ceiling))
            }
            _ => face_hit,
        }
    }

    /// Resolve what the next paint click would target. World-cell
    /// coords (which can be negative) let the preview track cells
    /// outside the current grid -- exactly the cases `auto-grow`
    /// would rescue at click time. `fallback_hit` is already picked
    /// on the tool's target plane, so ceiling paint targets the
    /// ceiling plane instead of the floor under it.
    pub(crate) fn compute_paint_target_preview(
        &self,
        room_id: NodeId,
        face_hit: Option<(FaceRef, [f32; 3])>,
        fallback_hit: Option<[f32; 2]>,
    ) -> Option<PaintTargetPreview> {
        let grid = self.room_grid_view(room_id)?;
        if self.active_tool == ViewTool::PaintMaterial {
            // The regular hovered-primitive overlay already outlines the
            // exact ray-picked face. Never draw a cell/add-geometry ghost for
            // material painting.
            return None;
        }
        if self.portal_place_active() {
            let (world_cell_x, world_cell_z) =
                self.paint_preview_world_cell(room_id, grid, face_hit, fallback_hit)?;
            return Some(PaintTargetPreview::PortalEdge {
                world_cell_x,
                world_cell_z,
                dir: self.portal_place_direction,
                valid: portal_edge_valid_for_world_cell(
                    grid,
                    world_cell_x,
                    world_cell_z,
                    self.portal_place_direction,
                ),
            });
        }
        let is_paint_wall = matches!(self.active_tool, ViewTool::PaintWall);
        let face_hit = self.face_hit_for_paint_tool(face_hit);

        // Cursor over an existing wall while PaintWall is active --
        // the click adds the next stack entry on that same edge, so
        // preview the next-free stack at its array-derived world cell.
        if is_paint_wall {
            if let Some((
                FaceRef {
                    sx,
                    sz,
                    kind: FaceKind::Wall { dir, .. },
                    ..
                },
                _,
            )) = face_hit
            {
                let stack = grid
                    .sector(sx, sz)
                    .map(|sector| sector.walls.get(dir).len() as u8)
                    .unwrap_or(0);
                return Some(PaintTargetPreview::Wall {
                    world_cell_x: grid.origin[0] + sx as i32,
                    world_cell_z: grid.origin[1] + sz as i32,
                    dir,
                    stack,
                });
            }
        }

        // Compute the world cell the cursor is over. Use the face
        // hit when present (works for walls / floors / ceilings of
        // existing cells); otherwise fall back to the floor-plane
        // hit, which can land on cells the grid doesn't cover yet.
        let (world_cell_x, world_cell_z, hit_world) = if let Some((face, hit)) = face_hit {
            (
                grid.origin[0] + face.sx as i32,
                grid.origin[1] + face.sz as i32,
                hit,
            )
        } else {
            let editor = fallback_hit?;
            let hit = self.editor_world_to_world3(room_id, editor);
            (
                grid.world_x_to_cell(hit[0]),
                grid.world_z_to_cell(hit[2]),
                hit,
            )
        };

        if is_paint_wall {
            // Cell centre in raw world units -- the inferred edge
            // matches the dispatch's `run_paint_action` because
            // both use the same axis convention.
            let s = grid.sector_size as f32;
            let cell_center_x = (world_cell_x as f32 + 0.5) * s;
            let cell_center_z = (world_cell_z as f32 + 0.5) * s;
            let dir = self
                .wall_paint_shape
                .direction(hit_world[0] - cell_center_x, hit_world[2] - cell_center_z);
            // Stack index points just past any existing walls on
            // that edge -- `add_wall` will append there.
            let stack = grid
                .world_cell_to_array(world_cell_x, world_cell_z)
                .and_then(|(sx, sz)| grid.sector(sx, sz))
                .map(|sector| sector.walls.get(dir).len() as u8)
                .unwrap_or(0);
            Some(PaintTargetPreview::Wall {
                world_cell_x,
                world_cell_z,
                dir,
                stack,
            })
        } else {
            let kind = match self.active_tool {
                ViewTool::PaintFloor => PaintCellPreviewKind::Floor,
                ViewTool::PaintCeiling => PaintCellPreviewKind::Ceiling,
                _ => PaintCellPreviewKind::Ground,
            };
            Some(PaintTargetPreview::Cell {
                world_cell_x,
                world_cell_z,
                kind,
            })
        }
    }
}

fn draw_world_xz_ring(
    painter: &egui::Painter,
    rect: Rect,
    camera: ViewportCameraState,
    center: [f32; 3],
    radius: f32,
    stroke: Stroke,
) {
    if !radius.is_finite() || radius <= 0.0 {
        return;
    }
    let mut previous = None;
    for step in 0..=64 {
        let angle = step as f32 / 64.0 * std::f32::consts::TAU;
        let (sin, cos) = angle.sin_cos();
        let world = [
            center[0] + cos * radius,
            center[1],
            center[2] + sin * radius,
        ];
        let Some(screen) = project_world_to_viewport_screen(camera, rect, world) else {
            previous = None;
            continue;
        };
        if let Some(previous) = previous {
            painter.line_segment([previous, screen], stroke);
        }
        previous = Some(screen);
    }
}

fn draw_character_capsule(
    painter: &egui::Painter,
    rect: Rect,
    camera: ViewportCameraState,
    origin: [f32; 3],
    radius: f32,
    height: f32,
) {
    if !radius.is_finite() || !height.is_finite() || radius <= 0.0 || height <= 0.0 {
        return;
    }
    let radius = radius.min(height * 0.5);
    let lower = [origin[0], origin[1] + radius, origin[2]];
    let upper = [origin[0], origin[1] + height - radius, origin[2]];
    let color = Color32::from_rgb(95, 224, 185);
    let stroke = Stroke::new(1.8, color);
    draw_world_xz_ring(painter, rect, camera, lower, radius, stroke);
    draw_world_xz_ring(painter, rect, camera, upper, radius, stroke);

    for [dx, dz] in [[radius, 0.0], [-radius, 0.0], [0.0, radius], [0.0, -radius]] {
        draw_world_polyline(
            painter,
            rect,
            camera,
            &[
                [lower[0] + dx, lower[1], lower[2] + dz],
                [upper[0] + dx, upper[1], upper[2] + dz],
            ],
            stroke,
        );
    }

    for axis in 0..2 {
        let mut top = Vec::with_capacity(17);
        let mut bottom = Vec::with_capacity(17);
        for step in 0..=16 {
            let angle = step as f32 / 16.0 * std::f32::consts::PI;
            let (sin, cos) = angle.sin_cos();
            let mut top_point = upper;
            let mut bottom_point = lower;
            let horizontal_axis = if axis == 0 { 0 } else { 2 };
            top_point[horizontal_axis] += cos * radius;
            top_point[1] += sin * radius;
            bottom_point[horizontal_axis] += cos * radius;
            bottom_point[1] -= sin * radius;
            top.push(top_point);
            bottom.push(bottom_point);
        }
        draw_world_polyline(painter, rect, camera, &top, stroke);
        draw_world_polyline(painter, rect, camera, &bottom, stroke);
    }

    if let Some(label_pos) = project_world_to_viewport_screen(
        camera,
        rect,
        [origin[0] + radius, origin[1] + height, origin[2]],
    ) {
        painter.text(
            label_pos + Vec2::new(7.0, -4.0),
            Align2::LEFT_BOTTOM,
            format!(
                "Capsule  r{}  h{}",
                radius.round() as i32,
                height.round() as i32
            ),
            FontId::monospace(10.0),
            color,
        );
    }
}

fn draw_world_polyline(
    painter: &egui::Painter,
    rect: Rect,
    camera: ViewportCameraState,
    points: &[[f32; 3]],
    stroke: Stroke,
) {
    let mut previous = None;
    for point in points {
        let Some(screen) = project_world_to_viewport_screen(camera, rect, *point) else {
            previous = None;
            continue;
        };
        if let Some(previous) = previous {
            painter.line_segment([previous, screen], stroke);
        }
        previous = Some(screen);
    }
}

fn character_motion_delta(
    action: psxed_project::CharacterAnimationAction,
    settings: CharacterControllerSettings,
    elapsed: std::time::Duration,
    sector_size: i32,
    base_yaw_q12: u16,
) -> ([i32; 3], u16) {
    let seconds = elapsed.as_secs_f64();
    if action == psxed_project::CharacterAnimationAction::Turn {
        let degrees = seconds * f64::from(settings.turn_speed_degrees_per_second);
        let yaw = (degrees * (4096.0 / 360.0)).round() as i64;
        return ([0; 3], yaw.rem_euclid(4096) as u16);
    }

    let frames = seconds * 60.0;
    let (distance, lateral) = match action {
        psxed_project::CharacterAnimationAction::Walk => (
            looping_motion_distance(settings.walk_speed, frames, sector_size),
            0.0,
        ),
        psxed_project::CharacterAnimationAction::Run => (
            looping_motion_distance(settings.run_speed, frames, sector_size),
            0.0,
        ),
        psxed_project::CharacterAnimationAction::WalkBackward => (
            -looping_motion_distance(settings.walk_speed, frames, sector_size),
            0.0,
        ),
        psxed_project::CharacterAnimationAction::StrafeLeft => (
            0.0,
            -looping_motion_distance(settings.walk_speed, frames, sector_size),
        ),
        psxed_project::CharacterAnimationAction::StrafeRight => (
            0.0,
            looping_motion_distance(settings.walk_speed, frames, sector_size),
        ),
        psxed_project::CharacterAnimationAction::Roll => (
            action_motion_distance(
                settings.roll_speed,
                settings.roll_active_frames,
                settings.roll_recovery_frames,
                frames,
            ),
            0.0,
        ),
        psxed_project::CharacterAnimationAction::Backstep => (
            // Legacy action slot 5 is authored and previewed as the locked
            // forward quickstep; the serialized discriminant remains stable.
            action_motion_distance(
                settings.backstep_speed,
                settings.backstep_active_frames,
                settings.backstep_recovery_frames,
                frames,
            ),
            0.0,
        ),
        _ => (0.0, 0.0),
    };
    let radians = f64::from(base_yaw_q12) * std::f64::consts::TAU / 4096.0;
    let (sin, cos) = radians.sin_cos();
    let forward = [sin, cos];
    let right = [cos, -sin];
    let x = forward[0] * distance + right[0] * lateral;
    let z = forward[1] * distance + right[1] * lateral;
    ([x.round() as i32, 0, z.round() as i32], 0)
}

fn looping_motion_distance(speed: i32, frames: f64, sector_size: i32) -> f64 {
    let speed = speed.unsigned_abs().max(1) as f64;
    let max_distance = f64::from(sector_size.max(1)) * 2.0;
    let loop_frames = (max_distance / speed).clamp(30.0, 120.0);
    (frames % loop_frames) * speed
}

fn action_motion_distance(speed: i32, active_frames: u8, recovery_frames: u8, frames: f64) -> f64 {
    let active = f64::from(active_frames.max(1));
    let cycle = active + f64::from(recovery_frames) + 12.0;
    (frames % cycle).min(active) * f64::from(speed.unsigned_abs())
}
