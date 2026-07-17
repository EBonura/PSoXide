use super::*;

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
                let NodeKind::Room { grid } = &scene.node(room_id)?.kind else {
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
                self.orbit_rotate_pivot = None;
            } else {
                // Fulcrum: on the first rotate frame, pick the point
                // under the cursor on the plane through the orbit
                // target; the whole drag then rotates rigidly around
                // it. Sky miss or Free mode falls back to the target.
                if self.orbit_rotate_pivot.is_none() {
                    let pivot = response
                        .interact_pointer_pos()
                        .and_then(|pos| self.pick_orbit_rotate_pivot(rect, pos))
                        .unwrap_or(self.camera_rig.target);
                    self.orbit_rotate_pivot = Some(pivot);
                }
                self.rotate_viewport_3d_camera(delta);
            }
        } else {
            self.orbit_rotate_pivot = None;
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
                        matches!(n.kind, NodeKind::Room { .. })
                            && !self.scene_node_effectively_hidden(n.id)
                    })
                    .map(|n| n.id)
            });
        let paint_tool = matches!(
            self.active_tool,
            ViewTool::PaintFloor
                | ViewTool::PaintWall
                | ViewTool::PaintCeiling
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
                self.update_floating_geometry_origin(origin);
            }
        }
        let dropped_resource = response
            .dnd_release_payload::<ResourceId>()
            .map(|payload| *payload);
        if self.floating_geometry.is_none() {
            if let Some(resource_id) = dropped_resource {
                self.drop_resource_3d(resource_id, face_hit, hover_world);
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
            }

            // Hover-track entity bounds in Select mode so the
            // overlay can highlight the bound under the cursor
            // before the user clicks.
            if select_tool {
                self.selection.hovered_entity_node = hover_entity_hit.map(|hit| hit.node);
            } else {
                self.selection.hovered_entity_node = None;
            }

            // Select-tool drag-translate. Two distinct drag flows:
            //   1. Entity bound under cursor → start `node_drag`,
            //      move the node on its X/Z plane.
            //   2. Otherwise (face / edge / vertex hit, or empty)
            //      move primitive geometry on the X/Z grid. Alt
            //      keeps the existing primitive vertical drag.
            // Pure clicks (press without movement) just promote
            // the hovered target to the selection -- no undo
            // entry, no mutation. The first drag frame that
            // crosses a threshold lazy-pushes undo so a
            // press-and-release doesn't leave a stale snapshot.
            if select_tool {
                if response.drag_started_by(egui::PointerButton::Primary) {
                    if let Some(pointer) = response.interact_pointer_pos() {
                        match pointer_target {
                            Some(Viewport3dPointerTarget::PrimitiveGizmo(axis)) => {
                                self.begin_primitive_gizmo_drag(axis, rect, pointer);
                            }
                            Some(Viewport3dPointerTarget::NodeGizmo(handle)) => {
                                self.begin_node_gizmo_handle_drag(handle, rect, pointer);
                            }
                            Some(Viewport3dPointerTarget::Entity(hit))
                                if self.transform_gizmo_mode == TransformGizmoMode::Move =>
                            {
                                self.begin_node_drag(hit, rect);
                            }
                            Some(Viewport3dPointerTarget::Entity(_)) => {}
                            Some(Viewport3dPointerTarget::Surface { .. }) => {
                                let modifiers = ui.input(|input| input.modifiers);
                                self.begin_primitive_pointer_drag(rect, pointer, modifiers);
                            }
                            None => {
                                let modifiers = ui.input(|input| input.modifiers);
                                self.begin_viewport_3d_box_select(pointer, hover_room, modifiers);
                            }
                        }
                    }
                }
                if response.dragged_by(egui::PointerButton::Primary) {
                    match self.interaction {
                        Interaction::PrimitiveGizmo(_) => {
                            if let Some(p) = response.interact_pointer_pos() {
                                self.update_primitive_gizmo_drag(p);
                            }
                        }
                        Interaction::NodeGizmo(_) => {
                            if let Some(p) = response.interact_pointer_pos() {
                                self.update_node_gizmo_drag(rect, p);
                            }
                        }
                        Interaction::Node(_) => {
                            if let Some(p) = response.interact_pointer_pos() {
                                self.update_node_drag(rect, p);
                            }
                        }
                        Interaction::PrimitiveGrid(_) => {
                            if let Some(p) = response.interact_pointer_pos() {
                                self.update_primitive_grid_drag(rect, p);
                            }
                        }
                        Interaction::BoxSelect3d(_) => {
                            if let Some(p) =
                                response.interact_pointer_pos().or(response.hover_pos())
                            {
                                self.update_viewport_3d_box_select(p, rect);
                            }
                        }
                        _ => self.update_primitive_drag(response.drag_delta().y),
                    }
                }
                if response.drag_stopped_by(egui::PointerButton::Primary) {
                    match self.interaction {
                        Interaction::PrimitiveGizmo(_) => self.end_primitive_gizmo_drag(),
                        Interaction::NodeGizmo(_) => self.end_node_gizmo_drag(),
                        Interaction::Node(_) => self.end_node_drag(),
                        Interaction::PrimitiveGrid(_) => self.end_primitive_grid_drag(),
                        Interaction::BoxSelect3d(_) => self.end_viewport_3d_box_select(),
                        _ => self.end_primitive_drag(),
                    }
                }
                if response.clicked_by(egui::PointerButton::Primary) {
                    // Click selection consumes the same topmost
                    // target as hover and drag start. Gizmo clicks
                    // are therefore handled by the gizmo path above
                    // and never fall through to a face behind them.
                    let modifiers = ui.input(|input| input.modifiers);
                    match pointer_target {
                        Some(Viewport3dPointerTarget::Entity(hit)) => {
                            let visible_order = self.scene_node_order();
                            self.apply_node_selection_modifiers(
                                hit.node,
                                modifiers,
                                &visible_order,
                            );
                        }
                        Some(Viewport3dPointerTarget::Surface { .. }) | None => {
                            self.commit_face_selection(modifiers);
                        }
                        Some(
                            Viewport3dPointerTarget::PrimitiveGizmo(_)
                            | Viewport3dPointerTarget::NodeGizmo(_),
                        ) => {}
                    }
                }
            } else {
                let primary_active = response.clicked_by(egui::PointerButton::Primary)
                    || response.dragged_by(egui::PointerButton::Primary);
                if primary_active {
                    if let Some(pos) = response.interact_pointer_pos() {
                        let face_hit = self.pick_face_with_hit(rect, pos);
                        let fallback = self
                            .active_room_id()
                            .and_then(|room| self.pick_3d_paint_world(rect, pos, room));
                        self.dispatch_paint_3d(face_hit, fallback);
                    }
                }
            }
        } else {
            self.selection.hovered_entity_node = None;
        }

        if response.hovered() {
            self.update_free_camera_keyboard(ui);
            let scroll = ui.input(|i| i.raw_scroll_delta.y);
            if scroll.abs() > f32::EPSILON {
                self.scroll_viewport_3d_camera(scroll);
            }
        }

        let hovered_primitive_axis = pointer_target.and_then(|target| target.primitive_axis());
        let hovered_node_handle = pointer_target.and_then(|target| target.node_handle());

        egui::Image::new((viewport_3d.texture, rect.size()))
            .uv(viewport_3d.uv)
            .paint_at(ui, rect);
        let painter = ui.painter_at(rect);
        Self::draw_viewport_3d_overlay_lines(&painter, rect, &viewport_3d);
        self.draw_primitive_gizmo(&painter, rect, hovered_primitive_axis);
        self.draw_node_gizmo(&painter, rect, hovered_node_handle);
        if let Some(pivot) = self.orbit_rotate_pivot {
            let world = [pivot[0] as f32, pivot[1] as f32, pivot[2] as f32];
            if let Some(screen) =
                project_world_to_viewport_screen(self.viewport_3d_camera(), rect, world)
            {
                let stroke = egui::Stroke::new(1.5, ui.visuals().strong_text_color());
                painter.circle_stroke(screen, 5.0, stroke);
                painter.line_segment(
                    [screen - egui::vec2(9.0, 0.0), screen + egui::vec2(9.0, 0.0)],
                    stroke,
                );
                painter.line_segment(
                    [screen - egui::vec2(0.0, 9.0), screen + egui::vec2(0.0, 9.0)],
                    stroke,
                );
            }
        }
        draw_viewport_box_select_marquee(&painter, self.viewport_3d_box_select_rect());
        if resource_drop_hovered {
            painter.rect_stroke(
                rect.shrink(2.0),
                2.0,
                Stroke::new(EDITOR_OUTLINE_STROKE_WIDTH, EDITOR_OUTLINE_ACCENT),
                StrokeKind::Inside,
            );
            painter.text(
                rect.center_top() + Vec2::new(0.0, 16.0),
                Align2::CENTER_TOP,
                "Drop resource into scene",
                FontId::proportional(13.0),
                STUDIO_ACCENT,
            );
        }
    }

    pub(crate) fn rotate_viewport_3d_camera(&mut self, delta: Vec2) {
        match self.orbit_rotate_pivot {
            Some(pivot) => self.camera_rig.rotate_about(delta, pivot),
            None => self.camera_rig.rotate(delta),
        }
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

        let (forward, right, vertical, speed) = ui.input(|input| {
            let axis = |positive: egui::Key, negative: egui::Key| {
                (input.key_down(positive) as i8 - input.key_down(negative) as i8) as f32
            };
            let speed = if input.modifiers.shift { 512.0 } else { 128.0 };
            (
                axis(egui::Key::W, egui::Key::S),
                axis(egui::Key::D, egui::Key::A),
                axis(egui::Key::Q, egui::Key::E),
                speed,
            )
        });
        if forward.abs() <= f32::EPSILON
            && right.abs() <= f32::EPSILON
            && vertical.abs() <= f32::EPSILON
        {
            return;
        }

        self.camera_rig
            .move_free_local(forward * speed, right * speed, vertical * speed);
        self.persist_editor_camera_state();
        ui.ctx().request_repaint();
    }

    /// World-space fulcrum for a rotate-drag: the cursor ray
    /// intersected with the horizontal plane through the orbit target
    /// (the focus plane), which anchors rotation to the hovered
    /// content without depending on room-local spaces. Orbit only.
    fn pick_orbit_rotate_pivot(&self, rect: Rect, pointer: Pos2) -> Option<[i32; 3]> {
        if self.camera_rig.mode != ViewportCameraMode::Orbit {
            return None;
        }
        let (origin, dir) = self.camera_ray_for_pointer(rect, pointer)?;
        let plane_y = self.camera_rig.target[1] as f32;
        let hit = ray_intersects_horizontal_plane(origin, dir, plane_y)?;
        Some([
            round_to_i32(hit[0]),
            round_to_i32(hit[1]),
            round_to_i32(hit[2]),
        ])
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
        let record_rect = Rect::from_min_size(
            visibility_rect.left_bottom() + Vec2::new(0.0, control_gap),
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
                draw_play_chunk_debug_map(&painter, rect, &self.project, metrics);
            }
        }
        self.draw_play_overlay_visibility_menu(ui, visibility_rect);
        if draw_play_overlay_icon_button(
            ui,
            record_rect,
            "play_input_record_toggle",
            icons::CIRCLE_DOT,
            if recording {
                "Stop recording input"
            } else {
                "Record embedded play input"
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
                "Replay saved input"
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
        self.camera_rig.camera()
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
