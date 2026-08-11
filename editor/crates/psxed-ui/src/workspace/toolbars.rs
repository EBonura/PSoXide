use super::*;

impl EditorWorkspace {
    pub(crate) fn draw_viewport(
        &mut self,
        ctx: &egui::Context,
        viewport_3d: EditorViewport3dPresentation,
        playtest_status: EditorPlaytestStatus,
    ) {
        egui::CentralPanel::default()
            .frame(viewport_frame())
            .show(ctx, |ui| {
                tool_panel_frame().show(ui, |ui| {
                    ui.expand_to_include_rect(ui.max_rect());
                    let material_undo_candidate =
                        if self.active_workspace == WorkspaceView::Material {
                            self.prepare_inspector_undo_frame(ctx);
                            inspector_has_edit_input(ctx)
                                .then(|| (self.project.clone(), self.history.epoch()))
                        } else {
                            None
                        };
                    self.draw_viewport_header_toolbar(ui);
                    tool_panel_body(ui, |ui| {
                        if viewport_3d.mode == EditorViewport3dMode::Play {
                            self.draw_viewport_3d_play_body(ui, viewport_3d, playtest_status);
                            return;
                        }

                        if self.active_workspace == WorkspaceView::Material {
                            self.draw_material_lab(ui);
                            if let Some((project_before, history_epoch_before)) =
                                material_undo_candidate
                            {
                                self.finish_inspector_undo_frame(
                                    project_before,
                                    history_epoch_before,
                                    ctx,
                                );
                            }
                            return;
                        }

                        if self.active_workspace == WorkspaceView::Animation {
                            self.prepare_inspector_undo_frame(ctx);
                            let undo_candidate = inspector_has_edit_input(ctx)
                                .then(|| (self.project.clone(), self.history.epoch()));
                            let changed = model_animation_viewer::draw_model_animation_viewer(
                                ui,
                                &mut self.project,
                                &self.project_dir,
                                &mut self.animation_viewer,
                                &mut self.animation_viewer_preview_texture,
                            );
                            if changed {
                                self.mark_dirty();
                            }
                            if let Some((project_before, history_epoch_before)) = undo_candidate {
                                self.finish_inspector_undo_frame(
                                    project_before,
                                    history_epoch_before,
                                    ctx,
                                );
                            }
                            return;
                        }

                        if self.active_workspace == WorkspaceView::Ui {
                            self.draw_ui_workspace_body(ui);
                            return;
                        }

                        if !self.view_2d {
                            self.draw_viewport_3d_body(ui, viewport_3d);
                            return;
                        }

                        let size = ui.available_size();
                        let size = Vec2::new(size.x.max(320.0), size.y.max(240.0));
                        let (rect, response) =
                            ui.allocate_exact_size(size, Sense::click_and_drag());
                        self.last_viewport_size = rect.size();
                        #[cfg(test)]
                        {
                            self.last_orthographic_viewport_rect = rect;
                            self.last_orthographic_response = Some((
                                response.id,
                                response.hovered(),
                                response.drag_started_by(egui::PointerButton::Primary),
                                response.dragged_by(egui::PointerButton::Primary),
                            ));
                        }
                        let orthographic_view = self.orthographic_view;
                        let top_view = orthographic_view == OrthographicView::Top;
                        let dnd_active = egui::DragAndDrop::has_any_payload(ui.ctx());
                        let resource_drop_hovered =
                            top_view && response.dnd_hover_payload::<ResourceId>().is_some();
                        let prefab_drop_hovered =
                            top_view && response.dnd_hover_payload::<PrefabDragPayload>().is_some();

                        if !dnd_active
                            && (response.dragged_by(egui::PointerButton::Middle)
                                || response.dragged_by(egui::PointerButton::Secondary))
                        {
                            let delta = ui.input(|input| input.pointer.delta());
                            let [horizontal, vertical] = orthographic_view.plane_axes();
                            self.orthographic_focus[horizontal] -= delta.x / self.viewport_zoom;
                            self.orthographic_focus[vertical] += delta.y / self.viewport_zoom;
                        }

                        if !dnd_active && response.hovered() {
                            let scroll = ui.input(|input| input.raw_scroll_delta.y);
                            if scroll.abs() > f32::EPSILON {
                                let pointer = ui
                                    .input(|input| input.pointer.hover_pos())
                                    .unwrap_or_else(|| rect.center());
                                let before = ViewportTransform::from_focus(
                                    rect,
                                    orthographic_view.project_f32(self.orthographic_focus),
                                    self.viewport_zoom,
                                )
                                .screen_to_world(pointer);
                                let zoom_factor = (1.0 + scroll * 0.0015).clamp(0.75, 1.25);
                                self.viewport_zoom = (self.viewport_zoom * zoom_factor)
                                    .clamp(MIN_VIEWPORT_ZOOM, MAX_VIEWPORT_ZOOM);
                                let after = ViewportTransform::from_focus(
                                    rect,
                                    orthographic_view.project_f32(self.orthographic_focus),
                                    self.viewport_zoom,
                                )
                                .screen_to_world(pointer);
                                let projected_focus =
                                    orthographic_view.project_f32(self.orthographic_focus);
                                self.orthographic_focus = orthographic_view.with_projected_focus(
                                    self.orthographic_focus,
                                    [
                                        projected_focus[0] + before[0] - after[0],
                                        projected_focus[1] + before[1] - after[1],
                                    ],
                                );
                            }
                        }

                        let transform = ViewportTransform::from_focus(
                            rect,
                            orthographic_view.project_f32(self.orthographic_focus),
                            self.viewport_zoom,
                        );
                        if top_view && self.floating_geometry.is_none() {
                            let dropped_resource = resource_drop_hovered
                                .then(|| response.dnd_release_payload::<ResourceId>())
                                .flatten()
                                .map(|payload| *payload);
                            let dropped_prefab = prefab_drop_hovered
                                .then(|| response.dnd_release_payload::<PrefabDragPayload>())
                                .flatten()
                                .map(|payload| payload.path.clone());
                            if let Some(pointer) =
                                response.interact_pointer_pos().or(response.hover_pos())
                            {
                                let world = transform.screen_to_world(pointer);
                                if let Some(resource_id) = dropped_resource {
                                    self.drop_resource_2d(resource_id, world);
                                } else if let Some(path) = dropped_prefab {
                                    self.drop_prefab_2d(&path, world);
                                }
                            }
                        }
                        let painter = ui.painter_at(rect);
                        painter.rect_filled(rect, 0.0, STUDIO_VIEWPORT);
                        if self.show_grid {
                            let bsp_only = self.active_room_id().is_none()
                                && !self.project.active_scene().brushes.is_empty();
                            let base_step = if bsp_only {
                                self.snap_units.max(1) as f32
                            } else {
                                1.0
                            };
                            draw_world_grid(&painter, transform, base_step);
                        }

                        let hits = if top_view {
                            draw_scene_viewport(
                                &painter,
                                transform,
                                &self.project,
                                SceneViewportContext {
                                    hidden_scene_nodes: &self.hidden_scene_nodes,
                                    selected: self.selection.selected_node,
                                    selected_nodes: &self.selection.selected_nodes,
                                    selected_sectors: &self.selection.selected_sectors,
                                    validation_issue_primitives: &self.validation_issue_primitives,
                                    validation_issue_rooms: &self.validation_issue_rooms,
                                    show_portals: self.show_portals,
                                    show_lights: self.show_lights,
                                },
                            )
                        } else {
                            Vec::new()
                        };

                        let pointer_world = response
                            .hover_pos()
                            .or_else(|| response.interact_pointer_pos())
                            .map(|pos| transform.screen_to_world(pos));
                        if top_view {
                            if let Some(world) = pointer_world {
                                self.draw_portal_place_preview_2d(&painter, transform, world);
                            }
                        }
                        if top_view {
                            if let (Some(room), Some(world)) = (
                                self.floating_geometry.as_ref().map(|preview| preview.room),
                                pointer_world,
                            ) {
                                if let Some(origin) =
                                    self.floating_origin_from_2d_world(room, world)
                                {
                                    self.track_floating_geometry_pointer_origin(origin);
                                }
                            }
                        }
                        let top_hit = pointer_world
                            .and_then(|world| hits.iter().rev().find(|hit| hit.contains(world)))
                            .map(|hit| hit.id);
                        let top_hit_is_room = top_hit
                            .and_then(|id| self.project.active_scene().node(id))
                            .is_some_and(|node| matches!(node.kind, NodeKind::Section { .. }));
                        let primary_down = ui
                            .input(|input| input.pointer.button_down(egui::PointerButton::Primary));
                        if !primary_down {
                            self.interaction.take_box_select_2d();
                        }
                        if !dnd_active && top_view && self.floating_geometry.is_some() {
                            if response.clicked_by(egui::PointerButton::Primary) {
                                self.commit_floating_geometry();
                            }
                            if response.clicked_by(egui::PointerButton::Secondary) {
                                self.cancel_floating_geometry();
                            }
                            draw_viewport_overlay(
                                &painter,
                                rect,
                                &self.project,
                                self.viewport_zoom,
                                self.snap_units,
                                orthographic_view,
                            );
                            draw_axes_gizmo(&painter, rect, orthographic_view);
                            return;
                        }
                        let brush_edit_active = matches!(self.active_tool, ViewTool::Brush)
                            || (matches!(self.active_tool, ViewTool::Select)
                                && self.selected_brush.is_some());
                        if !dnd_active && brush_edit_active {
                            if response.hovered()
                                && pointer_world.is_some_and(|world| {
                                    self.brush_edit_mode != BrushEditMode::Move
                                        || self.pick_brush_face_for_move_at_2d(world).is_some()
                                })
                            {
                                ui.ctx().set_cursor_icon(match self.brush_edit_mode {
                                    BrushEditMode::Move => egui::CursorIcon::Grab,
                                    BrushEditMode::Face => egui::CursorIcon::ResizeHorizontal,
                                    BrushEditMode::Edge | BrushEditMode::Vertex => {
                                        egui::CursorIcon::Crosshair
                                    }
                                });
                            }
                            if response.hovered() {
                                self.brush_tool_keyboard(ui);
                            }
                            if response.drag_started_by(egui::PointerButton::Primary) {
                                let modifiers = ui.input(|input| input.modifiers);
                                let additive_select = matches!(self.active_tool, ViewTool::Select)
                                    && (modifiers.shift || modifiers.command || modifiers.ctrl);
                                if !additive_select {
                                    if let Some(pos) = ui
                                        .input(|input| input.pointer.press_origin())
                                        .or_else(|| response.interact_pointer_pos())
                                    {
                                        let world = transform.screen_to_world(pos);
                                        let tolerance = 8.0 / self.viewport_zoom;
                                        // Brush keeps its legacy Shift group
                                        // move; Select reserves modifiers for
                                        // additive selection and marquee.
                                        let grabbed = if modifiers.shift
                                            || self.brush_edit_mode == BrushEditMode::Move
                                        {
                                            self.begin_brush_move_2d(world)
                                        } else {
                                            match self.brush_edit_mode {
                                                BrushEditMode::Move => unreachable!(),
                                                BrushEditMode::Face => {
                                                    self.begin_brush_resize_2d(world, tolerance)
                                                }
                                                BrushEditMode::Edge => {
                                                    self.begin_brush_edge_drag_2d(world, tolerance)
                                                }
                                                BrushEditMode::Vertex => self
                                                    .begin_brush_vertex_drag_2d(world, tolerance),
                                            }
                                        };
                                        if !grabbed
                                            && matches!(self.active_tool, ViewTool::Brush)
                                            && self.pick_brush_face_at_2d(world).is_none()
                                        {
                                            self.begin_brush_drag_2d(world);
                                        }
                                    }
                                }
                            }
                            if response.dragged_by(egui::PointerButton::Primary) {
                                if let Some(pos) =
                                    response.interact_pointer_pos().or(response.hover_pos())
                                {
                                    let world = transform.screen_to_world(pos);
                                    if self.brush_move.is_some() {
                                        self.update_brush_move_2d(world);
                                    } else if self.brush_vertex_drag.is_some() {
                                        self.update_brush_vertex_drag_2d(world);
                                    } else if self.brush_extrude.is_some() {
                                        self.update_brush_resize_2d(world);
                                    } else {
                                        self.update_brush_drag_2d(world);
                                    }
                                }
                            }
                            if response.drag_stopped_by(egui::PointerButton::Primary) {
                                self.commit_brush_gesture_2d();
                            }
                        }
                        let bsp_brush_marquee = !self.project.active_scene().brushes.is_empty();
                        if !dnd_active
                            && matches!(self.active_tool, ViewTool::Select)
                            && self.brush_move.is_none()
                            && self.brush_vertex_drag.is_none()
                            && self.brush_extrude.is_none()
                            && response.drag_started_by(egui::PointerButton::Primary)
                        {
                            let can_box_select = bsp_brush_marquee
                                || (top_view && (top_hit.is_none() || top_hit_is_room));
                            if can_box_select {
                                if let Some(start) = ui
                                    .input(|input| input.pointer.press_origin())
                                    .or_else(|| response.interact_pointer_pos())
                                {
                                    let modifiers = ui.input(|input| input.modifiers);
                                    self.begin_viewport_box_select(
                                        start,
                                        top_hit.or_else(|| self.active_room_id()),
                                        modifiers,
                                    );
                                }
                            }
                        }
                        if !dnd_active
                            && matches!(self.active_tool, ViewTool::Select)
                            && response.dragged_by(egui::PointerButton::Primary)
                        {
                            if let Some(current) =
                                response.interact_pointer_pos().or(response.hover_pos())
                            {
                                self.update_viewport_box_select(current, transform);
                            }
                        }
                        if top_view
                            && !dnd_active
                            && self.interaction.box_select_2d().is_none()
                            && self.brush_move.is_none()
                            && self.brush_vertex_drag.is_none()
                            && self.brush_extrude.is_none()
                            && response.dragged_by(egui::PointerButton::Primary)
                        {
                            self.drag_selected_node(ui.input(|input| input.pointer.delta()));
                        }

                        if !dnd_active && response.clicked_by(egui::PointerButton::Primary) {
                            if let Some(pos) = response.interact_pointer_pos() {
                                let world = transform.screen_to_world(pos);
                                let modifiers = ui.input(|input| input.modifiers);
                                self.handle_viewport_click(world, &hits, modifiers);
                            }
                        }

                        draw_viewport_overlay(
                            &painter,
                            rect,
                            &self.project,
                            self.viewport_zoom,
                            self.snap_units,
                            orthographic_view,
                        );
                        draw_viewport_box_select_marquee(
                            &painter,
                            (top_view || bsp_brush_marquee)
                                .then(|| self.viewport_box_select_rect())
                                .flatten(),
                        );
                        self.draw_brush_footprints_2d(&painter, transform);
                        draw_axes_gizmo(&painter, rect, orthographic_view);
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
                    });
                });
            });
    }

    pub(crate) fn draw_viewport_header_toolbar(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .fill(STUDIO_PANEL_HEADER)
            .inner_margin(egui::Margin::symmetric(8, 3))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = Vec2::new(4.0, 3.0);
                ui.horizontal(|ui| {
                    self.draw_workspace_tabs(ui);
                });
                ui.separator();
                if self.active_workspace == WorkspaceView::Material {
                    ui.horizontal_wrapped(|ui| self.draw_material_lab_toolbar(ui));
                } else {
                    ui.horizontal(|ui| match self.active_workspace {
                        WorkspaceView::Animation => {
                            let action =
                                model_animation_viewer::draw_model_animation_viewer_toolbar(
                                    ui,
                                    &mut self.project,
                                    &self.project_dir,
                                    &mut self.animation_viewer,
                                    &mut self.animation_viewer_preview_texture,
                                );
                            if let Some(action) = action {
                                self.handle_animation_viewer_action(action);
                            }
                        }
                        _ => self.draw_viewport_toolbar(ui),
                    });
                }
            });
    }

    /// Always-visible top-level workspaces. Their row stays separate from
    /// each workspace's tools so every mode has the same selector -> toolbar
    /// -> content rhythm.
    pub(crate) fn draw_workspace_tabs(&mut self, ui: &mut egui::Ui) {
        for view in [
            WorkspaceView::Room,
            WorkspaceView::Ui,
            WorkspaceView::Animation,
            WorkspaceView::Material,
        ] {
            let selected = self.active_workspace == view;
            let response = ui
                .add(
                    egui::Button::new(icons::label(view.icon(), view.label()))
                        .selected(selected)
                        .min_size(Vec2::new(82.0, 23.0)),
                )
                .on_hover_text(format!("Open the {} workspace", view.label()));
            if response.clicked() && !selected {
                self.active_workspace = view;
                self.status = format!("Workspace: {}", view.label());
                self.mark_shortcut_group_changed(ShortcutGroup::Workspace);
                if view == WorkspaceView::Material {
                    if let Some(material) = self.selected_material_resource() {
                        self.material_lab.focused_material = Some(material);
                    }
                }
            }
        }
    }

    pub(crate) fn mark_shortcut_group_changed(&mut self, group: ShortcutGroup) {
        self.shortcut_group_flash = Some((group, Instant::now()));
    }

    pub(crate) fn retain_shortcut_group_flash(&mut self) {
        if self.shortcut_group_flash.is_some_and(|(_, started)| {
            started.elapsed().as_secs_f32() >= SHORTCUT_GROUP_FLASH_SECONDS
        }) {
            self.shortcut_group_flash = None;
        }
    }

    pub(crate) fn shortcut_group_glow(&self, group: ShortcutGroup) -> f32 {
        let Some((active, started)) = self.shortcut_group_flash else {
            return 0.0;
        };
        if active != group {
            return 0.0;
        }
        let elapsed = started.elapsed().as_secs_f32();
        (1.0 - elapsed / SHORTCUT_GROUP_FLASH_SECONDS).clamp(0.0, 1.0)
    }

    pub(crate) fn draw_viewport_toolbar(&mut self, ui: &mut egui::Ui) {
        let room_active = self.active_room_id().is_some();
        let bsp_place_active = self.active_tool == ViewTool::Place
            && self.bsp_authoring_root().is_some()
            && self.place_kind != PlaceKind::Portal;
        if !room_active
            && !bsp_place_active
            && !self.bsp_face_paint_active()
            && self.active_tool.requires_room_context()
        {
            self.active_tool = ViewTool::Select;
            self.material_paint_sampling = false;
        }
        self.retain_shortcut_group_flash();
        if self.shortcut_group_flash.is_some() {
            ui.ctx().request_repaint();
        }
        ui.spacing_mut().item_spacing.x = 4.0;
        if self.active_workspace == WorkspaceView::Ui {
            self.draw_ui_viewport_toolbar(ui);
            return;
        }

        let active_tool_label = self.active_tool_group_label();
        toolbar_group_menu(
            ui,
            2,
            self.shortcut_group_glow(ShortcutGroup::Tool),
            self.active_tool_group_icon(),
            "Tool",
            &active_tool_label,
            |ui| self.draw_tool_group_menu(ui),
        );
        match self.active_tool {
            ViewTool::Brush => {
                ui.separator();
                self.draw_brush_edit_mode_controls(ui);
                if ui
                    .button(format!("Clip keeps: {}", self.brush_clip_keep.label()))
                    .on_hover_text("Which side(s) a two-point clip keeps")
                    .clicked()
                {
                    self.brush_clip_keep = self.brush_clip_keep.next();
                }
                // Gesture-mode state only; per-brush and per-face editing
                // lives in the inspector (draw_brush_inspector).
                ui.checkbox(&mut self.brush_texture_lock, "Tex lock")
                    .on_hover_text("Keep face textures anchored to the brush when it moves");
                // The one grid step every brush drag/create/clip snaps to
                // (shared with the Tool menu's Snap interval).
                ui.label("Grid");
                ui.add_sized(
                    [52.0, 22.0],
                    egui::DragValue::new(&mut self.snap_units)
                        .speed(1.0)
                        .range(1..=256),
                )
                .on_hover_text("Grid snap step, world units. All brush drags snap to it.");
            }
            ViewTool::Select if self.selected_brush.is_some() => {
                // A brush selected through the general Select tool remains
                // directly editable. Do not make the user discover that the
                // separate Brush tool owns otherwise-identical gestures.
                ui.separator();
                self.draw_brush_edit_mode_controls(ui);
                ui.label("Grid");
                ui.add_sized(
                    [52.0, 22.0],
                    egui::DragValue::new(&mut self.snap_units)
                        .speed(1.0)
                        .range(1..=256),
                )
                .on_hover_text("Grid snap step, world units. All brush drags snap to it.");
            }
            ViewTool::Select => self.draw_select_tool_toolbar_controls(ui),
            ViewTool::PaintMaterial => self.draw_material_paint_toolbar_controls(ui),
            ViewTool::Water => self.draw_water_toolbar_controls(ui),
            ViewTool::PaintFloor | ViewTool::PaintCeiling => {
                ui.separator();
                self.draw_brush_material_picker(ui);
            }
            ViewTool::PaintWall => {
                ui.separator();
                toolbar_option_menu(
                    ui,
                    icons::BRICK_WALL,
                    "Wall shape",
                    "Wall shape",
                    self.wall_paint_shape.label(),
                    false,
                    |ui| self.draw_wall_paint_shape_picker(ui),
                );
                self.draw_brush_material_picker(ui);
            }
            ViewTool::Erase | ViewTool::Place => {}
        }
        ui.separator();
        toolbar_group_menu_icon_only(
            ui,
            7,
            self.shortcut_group_glow(ShortcutGroup::Visibility),
            self.visibility_group_icon(),
            "Visibility",
            self.visibility_group_label(),
            |ui| self.draw_visibility_menu_contents(ui),
        );
        toolbar_group_menu(
            ui,
            8,
            self.shortcut_group_glow(ShortcutGroup::Camera),
            self.camera_rig.mode.icon(),
            "Camera",
            self.viewport_camera_mode_label(),
            |ui| self.draw_camera_group_menu(ui),
        );
        if ui
            .add(egui::Button::new(icons::text(icons::FOCUS, 14.0)).min_size(Vec2::new(28.0, 23.0)))
            .on_hover_text("Frame selection (.)")
            .clicked()
        {
            self.frame_viewport();
        }
        toolbar_group_menu(
            ui,
            9,
            self.shortcut_group_glow(ShortcutGroup::Viewport),
            self.view_dimension_group_icon(),
            "Viewport",
            self.view_dimension_group_label(),
            |ui| self.draw_view_dimension_group_menu(ui),
        );

        // Compact layer stepper. Navigation stays on the toolbar; footprint
        // extrusion and slab/portal actions live in one adjacent menu so
        // stacked-room authoring does not consume another toolbar row.
        if matches!(self.active_workspace, WorkspaceView::Room) {
            if let Some(room_id) = self.floors_target_room() {
                let floor_count = self
                    .room_base_grid(room_id)
                    .map(|grid| grid.floor_count())
                    .unwrap_or(1);
                let active = self.active_floor.min(floor_count.saturating_sub(1));
                let has_footprint = self.can_author_selected_layer_footprint();
                let can_delete_empty_layer = self.can_delete_active_empty_layer();
                ui.separator();
                if ui
                    .add_enabled(
                        active > 0,
                        egui::Button::new(icons::text(icons::CHEVRON_LEFT, 14.0))
                            .min_size(Vec2::new(24.0, 23.0)),
                    )
                    .on_hover_text("Edit the layer below")
                    .clicked()
                {
                    self.floor_down();
                }
                ui.label(
                    RichText::new(format!("L{}/{}", active + 1, floor_count))
                        .monospace()
                        .color(STUDIO_TEXT_WEAK),
                );
                if ui
                    .add(
                        egui::Button::new(icons::text(icons::CHEVRON_RIGHT, 14.0))
                            .min_size(Vec2::new(24.0, 23.0)),
                    )
                    .on_hover_text("Edit the layer above (adds an empty layer at the top)")
                    .clicked()
                {
                    self.floor_up();
                }
                toolbar_option_menu(
                    ui,
                    icons::LAYERS,
                    "Layers",
                    "Layer actions",
                    format!("Layer {} of {}", active + 1, floor_count),
                    false,
                    |ui| {
                        ui.set_min_width(230.0);
                        ui.label(
                            RichText::new("EXTRUDE SELECTION")
                                .small()
                                .color(STUDIO_TEXT_WEAK),
                        );
                        if ui
                            .add_enabled(has_footprint, egui::Button::new("Above with opening"))
                            .on_hover_text(
                                "Build a closed volume above the selected footprint and remove the slab between layers",
                            )
                            .clicked()
                        {
                            self.extrude_selected_layer_above(true);
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(has_footprint, egui::Button::new("Above as solid layer"))
                            .clicked()
                        {
                            self.extrude_selected_layer_above(false);
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(has_footprint, egui::Button::new("Below with opening"))
                            .on_hover_text(
                                "Build below the selected footprint; at layer one this inserts a new base without moving existing content",
                            )
                            .clicked()
                        {
                            self.extrude_selected_layer_below(true);
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(has_footprint, egui::Button::new("Below as solid layer"))
                            .clicked()
                        {
                            self.extrude_selected_layer_below(false);
                            ui.close_menu();
                        }

                        ui.separator();
                        ui.label(
                            RichText::new("SELECTED SLAB")
                                .small()
                                .color(STUDIO_TEXT_WEAK),
                        );
                        if ui
                            .add_enabled(
                                has_footprint && active + 1 < floor_count,
                                egui::Button::new("Open to layer above"),
                            )
                            .clicked()
                        {
                            self.set_selected_slab_above(true);
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(
                                has_footprint && active + 1 < floor_count,
                                egui::Button::new("Seal layer above"),
                            )
                            .clicked()
                        {
                            self.set_selected_slab_above(false);
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(
                                has_footprint && active > 0,
                                egui::Button::new("Open to layer below"),
                            )
                            .clicked()
                        {
                            self.set_selected_slab_below(true);
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(
                                has_footprint && active > 0,
                                egui::Button::new("Seal layer below"),
                            )
                            .clicked()
                        {
                            self.set_selected_slab_below(false);
                            ui.close_menu();
                        }

                        if !has_footprint {
                            ui.separator();
                            ui.label(
                                RichText::new("Select tiles or 3D faces to enable layer actions.")
                                    .small()
                                    .color(STUDIO_TEXT_WEAK),
                            );
                        }

                        ui.separator();
                        ui.label(RichText::new("LAYER").small().color(STUDIO_TEXT_WEAK));
                        if ui
                            .add_enabled(
                                can_delete_empty_layer,
                                egui::Button::new(icons::label(
                                    icons::TRASH,
                                    "Delete empty layer",
                                )),
                            )
                            .on_hover_text(
                                "Remove this layer after all of its tile geometry is deleted; objects are preserved on the nearest surviving layer",
                            )
                            .clicked()
                        {
                            self.delete_active_empty_layer();
                            ui.close_menu();
                        }
                    },
                );
            }
        }
    }

    pub(crate) fn draw_brush_edit_mode_controls(&mut self, ui: &mut egui::Ui) {
        for mode in BrushEditMode::ALL {
            let response = ui
                .add(
                    egui::Button::new(mode.label())
                        .selected(self.brush_edit_mode == mode)
                        .min_size(Vec2::new(48.0, 23.0)),
                )
                .on_hover_text(format!("{}; snaps to the active grid", mode.gesture_hint()));
            if response.clicked() {
                self.set_brush_edit_mode(mode);
            }
        }
        ui.label(
            RichText::new(self.brush_edit_mode.toolbar_hint())
                .small()
                .color(STUDIO_TEXT_WEAK),
        );
    }

    fn set_brush_edit_mode(&mut self, mode: BrushEditMode) {
        if self.brush_edit_mode == mode {
            return;
        }
        self.cancel_brush_gestures();
        self.brush_edit_mode = mode;
        if let Some(selection_mode) = mode.selection_mode() {
            self.set_selection_mode(selection_mode);
        }
        self.status = format!(
            "Brush {}: {}; grid snap {}",
            mode.label(),
            mode.gesture_hint(),
            self.snap_units.max(1)
        );
    }

    fn draw_select_tool_toolbar_controls(&mut self, ui: &mut egui::Ui) {
        toolbar_group_menu(
            ui,
            3,
            self.shortcut_group_glow(ShortcutGroup::Transform),
            self.transform_gizmo_mode.icon(),
            "Transform",
            self.transform_gizmo_mode.label(),
            |ui| self.draw_transform_group_menu(ui),
        );
        let space = self.gizmo_space;
        let space_response = ui
            .add(
                egui::Button::new(icons::label(space.icon(), space.label()))
                    .min_size(Vec2::new(30.0, 23.0)),
            )
            .on_hover_text(
                "Gizmo orientation. Global aligns handles to the world axes; \
                 Local aligns them to the selected node's own rotation.",
            );
        if space_response.clicked() {
            self.gizmo_space = space.toggled();
            self.status = format!("Gizmo orientation: {}", self.gizmo_space.label());
        }
        toolbar_group_menu(
            ui,
            4,
            self.shortcut_group_glow(ShortcutGroup::Selection),
            self.selection_mode.icon(),
            "Selection",
            self.selection_mode.label(),
            |ui| self.draw_selection_group_menu(ui),
        );
        toolbar_group_menu(
            ui,
            5,
            self.shortcut_group_glow(ShortcutGroup::Surface),
            self.horizontal_edit_mode.icon(),
            "Surface",
            self.horizontal_edit_mode.label(),
            |ui| self.draw_horizontal_edit_group_menu(ui),
        );
        toolbar_group_menu(
            ui,
            6,
            self.shortcut_group_glow(ShortcutGroup::Vertex),
            self.vertex_connectivity.icon(),
            "Vertex Edits",
            self.vertex_connectivity.label(),
            |ui| self.draw_vertex_connectivity_group_menu(ui),
        );
    }

    fn draw_material_paint_toolbar_controls(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        self.draw_brush_material_picker(ui);
        let eyedropper = ui
            .toggle_value(
                &mut self.material_paint_sampling,
                icons::text(icons::PIPETTE, 14.0),
            )
            .on_hover_text(
                "Eyedropper: sample the material from the next floor, ceiling, or wall you click.",
            );
        if eyedropper.changed() {
            self.status = if self.material_paint_sampling {
                "Eyedropper: click a surface to sample its material".to_string()
            } else {
                "Eyedropper cancelled".to_string()
            };
            self.mark_shortcut_group_changed(ShortcutGroup::Tool);
        }
        let blend = ui
            .toggle_value(
                &mut self.material_paint_blend,
                icons::label(icons::BLEND, "Blend"),
            )
            .on_hover_text(
                "Blend the painted tile into its surroundings. Connected painted tiles remain continuous.",
            );
        if blend.changed() {
            self.status = if self.material_paint_blend {
                "Material Paint: Blend".to_string()
            } else {
                "Material Paint: Direct".to_string()
            };
            self.mark_shortcut_group_changed(ShortcutGroup::Tool);
        }
        if self.material_paint_blend {
            let coverage = self.material_paint_blend_coverage_percent;
            let detail = self.material_paint_blend_edge_detail;
            toolbar_option_menu(
                ui,
                icons::BLEND,
                "Details",
                "Blend details",
                format!("{coverage}% coverage, {detail} detail"),
                false,
                |ui| {
                    ui.set_min_width(260.0);
                    ui.label(RichText::new("Blend details").strong());
                    let coverage_changed = ui
                        .add(
                            egui::Slider::new(
                                &mut self.material_paint_blend_coverage_percent,
                                5..=95,
                            )
                            .text("Coverage")
                            .suffix("%"),
                        )
                        .on_hover_text("How much of each tile is occupied by the painted material.")
                        .changed();
                    let detail_changed = ui
                        .add(
                            egui::Slider::new(
                                &mut self.material_paint_blend_edge_detail,
                                0..=96,
                            )
                            .text("Edge detail"),
                        )
                        .on_hover_text(
                            "Adds organic breakup to exposed blend edges while keeping connected edges seamless.",
                        )
                        .changed();
                    ui.horizontal(|ui| {
                        if ui.button("Reset").clicked() {
                            self.material_paint_blend_coverage_percent = 50;
                            self.material_paint_blend_edge_detail = 20;
                            self.status = "Reset Paint blend details".to_string();
                        }
                        ui.weak("Applied on the next stroke");
                    });
                    if coverage_changed || detail_changed {
                        self.status = format!(
                            "Paint blend: {}% coverage, {} edge detail",
                            self.material_paint_blend_coverage_percent,
                            self.material_paint_blend_edge_detail
                        );
                    }
                },
            );
        }
    }

    fn draw_water_toolbar_controls(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        ui.selectable_value(&mut self.water_tool_mode, WaterToolMode::Add, "Add")
            .on_hover_text("Add the clicked cell to the selected water volume.");
        ui.selectable_value(&mut self.water_tool_mode, WaterToolMode::Erase, "Erase")
            .on_hover_text("Remove water from the clicked cell.");
        ui.selectable_value(&mut self.water_tool_mode, WaterToolMode::Select, "Select")
            .on_hover_text("Select the water volume that owns the clicked cell.");
        if self.water_tool_mode == WaterToolMode::Add {
            self.draw_brush_material_picker(ui);
        }
        let node_id = self.selection.selected_node;
        if node_id != NodeId::ROOT {
            let current =
                self.project
                    .active_scene()
                    .node(node_id)
                    .and_then(|node| match &node.kind {
                        NodeKind::WaterVolume { settings, .. } => Some(*settings),
                        _ => None,
                    });
            if let Some(mut edited) = current {
                ui.separator();
                let changed = ui
                    .add(
                        egui::DragValue::new(&mut edited.height_above_floor)
                            .range(1..=8192)
                            .speed(8.0)
                            .prefix("Height "),
                    )
                    .on_hover_text(
                        "Water surface height above the lowest point of each painted floor tile. The volume bottom always follows the terrain.",
                    )
                    .changed();
                if changed {
                    self.push_undo();
                    if let Some(NodeKind::WaterVolume { settings, .. }) = self
                        .project
                        .active_scene_mut()
                        .node_mut(node_id)
                        .map(|node| &mut node.kind)
                    {
                        *settings = edited.normalized();
                    }
                    self.mark_dirty();
                }
            }
        }
    }

    pub(crate) fn draw_ui_viewport_toolbar(&mut self, ui: &mut egui::Ui) {
        toolbar_group_menu(
            ui,
            2,
            self.shortcut_group_glow(ShortcutGroup::Transform),
            self.ui_transform_mode.icon(),
            "Transform",
            self.ui_transform_mode.label(),
            |ui| {
                ui.set_min_width(160.0);
                for mode in [UiTransformMode::Move, UiTransformMode::Rotate] {
                    if toolbar_menu_choice(
                        ui,
                        icons::label(mode.icon(), mode.label()),
                        self.ui_transform_mode == mode,
                    ) {
                        self.ui_transform_mode = mode;
                        self.status = format!("UI transform: {}", mode.label());
                        self.mark_shortcut_group_changed(ShortcutGroup::Transform);
                    }
                }
            },
        );

        toolbar_option_menu(ui, icons::PLUS, "Add", "Add UI Node", "Add", false, |ui| {
            ui.set_min_width(160.0);
            for (label, kind) in default_addable_ui_kinds() {
                if ui.button(label).clicked() {
                    self.add_ui_child(kind, label);
                    ui.close_menu();
                }
            }
        });

        let center_snap_label = if self.ui_center_snap { "On" } else { "Off" };
        toolbar_option_menu(
            ui,
            icons::FOCUS,
            "Snap",
            "Center Snap",
            center_snap_label,
            self.ui_center_snap,
            |ui| {
                ui.set_min_width(240.0);
                if ui
                    .checkbox(
                        &mut self.ui_center_snap,
                        "Snap moved nodes to canvas centre",
                    )
                    .changed()
                {
                    self.status = if self.ui_center_snap {
                        "UI centre snap enabled".to_string()
                    } else {
                        "UI centre snap disabled".to_string()
                    };
                }
                ui.weak("Shows blue centre guides while dragging.");
            },
        );

        let nav_preview_label = if self.ui_nav_preview { "On" } else { "Off" };
        toolbar_option_menu(
            ui,
            icons::PLAY,
            "Preview",
            "Navigation Preview",
            nav_preview_label,
            self.ui_nav_preview,
            |ui| {
                ui.set_min_width(228.0);
                if ui
                    .checkbox(&mut self.ui_nav_preview, "Preview UI navigation")
                    .changed()
                {
                    self.status = if self.ui_nav_preview {
                        "UI navigation preview enabled".to_string()
                    } else {
                        "UI navigation preview disabled".to_string()
                    };
                }
                if self.ui_nav_preview {
                    ui.weak("Arrow keys move focus; Enter activates.");
                }
            },
        );

        let screen_offset_label = if self.screen_offset_sim_px == 0 {
            "0 px".to_string()
        } else {
            format!("{:+} px", self.screen_offset_sim_px)
        };
        toolbar_option_menu(
            ui,
            icons::MOVE,
            &screen_offset_label,
            "Screen Offset",
            screen_offset_label.clone(),
            self.screen_offset_sim_px != 0,
            |ui| {
                ui.set_min_width(220.0);
                ui.horizontal(|ui| {
                    ui.label("Screen offset");
                    let response = ui.add(
                        egui::DragValue::new(&mut self.screen_offset_sim_px)
                            .range(-48..=48)
                            .suffix(" px"),
                    );
                    if response.changed() {
                        self.status = format!(
                            "UI screen offset preview {:+} px",
                            self.screen_offset_sim_px
                        );
                    }
                });
                if self.screen_offset_sim_px != 0 && ui.button("Centre").clicked() {
                    self.screen_offset_sim_px = 0;
                    self.status = "UI screen offset preview centred".to_string();
                    ui.close_menu();
                }
                ui.weak("TV-position preview; authored positions stay centred.");
            },
        );
    }

    pub(crate) fn draw_tool_group_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(236.0);
        let room_active = self.active_room_id().is_some();
        let bsp_active = self.bsp_authoring_root().is_some();
        ui.label("Edit surfaces");
        for (tool, icon, label) in [
            (ViewTool::Select, ViewTool::Select.icon(), "Select"),
            (
                ViewTool::PaintMaterial,
                ViewTool::PaintMaterial.icon(),
                "Paint",
            ),
            (ViewTool::Water, ViewTool::Water.icon(), "Water"),
            (ViewTool::Erase, ViewTool::Erase.icon(), "Erase"),
        ] {
            // Material Paint also works without a Room in a brush scene,
            // where it addresses BSP brush faces instead of grid cells.
            let enabled = room_active
                || !tool.requires_room_context()
                || (tool == ViewTool::PaintMaterial && bsp_active);
            let selected = self.active_tool_cycle_value() == (tool, None);
            ui.add_enabled_ui(enabled, |ui| {
                if toolbar_menu_choice(ui, icons::label(icon, label), selected) {
                    self.set_active_tool_cycle_value((tool, None));
                }
            });
        }

        ui.separator();
        ui.label("Create geometry");
        for (tool, icon, label) in [
            (ViewTool::PaintFloor, ViewTool::PaintFloor.icon(), "Floor"),
            (ViewTool::PaintWall, ViewTool::PaintWall.icon(), "Wall"),
            (
                ViewTool::PaintCeiling,
                ViewTool::PaintCeiling.icon(),
                "Ceiling",
            ),
        ] {
            let selected = self.active_tool_cycle_value() == (tool, None);
            ui.add_enabled_ui(room_active, |ui| {
                if toolbar_menu_choice(ui, icons::label(icon, label), selected) {
                    self.set_active_tool_cycle_value((tool, None));
                }
            });
        }

        {
            let selected = self.active_tool_cycle_value() == (ViewTool::Brush, None);
            if toolbar_menu_choice(ui, icons::label(ViewTool::Brush.icon(), "Brush"), selected) {
                self.set_active_tool_cycle_value((ViewTool::Brush, None));
            }
        }

        ui.separator();
        ui.label("Add");
        for kind in PlaceKind::ALL {
            let selected = self.active_tool_cycle_value() == (ViewTool::Place, Some(kind));
            let enabled = room_active || (bsp_active && kind != PlaceKind::Portal);
            ui.add_enabled_ui(enabled, |ui| {
                if toolbar_menu_choice(ui, icons::label(kind.icon(), kind.label()), selected) {
                    self.set_active_tool_cycle_value((ViewTool::Place, Some(kind)));
                }
            });
        }

        ui.separator();
        ui.horizontal(|ui| {
            if ui.checkbox(&mut self.snap_to_grid, "Snap").changed() {
                self.mark_shortcut_group_changed(ShortcutGroup::Tool);
            }
            let snap_response = ui.add_sized(
                [64.0, 22.0],
                egui::DragValue::new(&mut self.snap_units)
                    .speed(1.0)
                    .range(1..=256),
            );
            if snap_response.changed() {
                self.mark_shortcut_group_changed(ShortcutGroup::Tool);
            }
            snap_response.on_hover_text("Snap interval.");
        });

        if self.active_tool == ViewTool::Place && self.place_kind != PlaceKind::Portal {
            ui.separator();
            self.draw_active_place_options(ui);
        }
    }

    pub(crate) fn draw_transform_group_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(176.0);
        for mode in [
            TransformGizmoMode::Move,
            TransformGizmoMode::Rotate,
            TransformGizmoMode::Scale,
        ] {
            if toolbar_menu_choice(
                ui,
                icons::label(mode.icon(), mode.label()),
                self.transform_gizmo_mode == mode,
            ) {
                self.set_transform_gizmo_mode(mode);
            }
        }
    }

    pub(crate) fn draw_selection_group_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(156.0);
        for mode in [
            SelectionMode::Face,
            SelectionMode::Edge,
            SelectionMode::Vertex,
        ] {
            if toolbar_menu_choice(
                ui,
                icons::label(mode.icon(), mode.label()),
                self.selection_mode == mode,
            ) {
                self.set_selection_mode(mode);
            }
        }
    }

    pub(crate) fn draw_horizontal_edit_group_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(156.0);
        for mode in [HorizontalEditMode::Quad, HorizontalEditMode::Triangle] {
            if toolbar_menu_choice(
                ui,
                icons::label(mode.icon(), mode.label()),
                self.horizontal_edit_mode == mode,
            ) {
                self.set_horizontal_edit_mode(mode);
            }
        }
    }

    pub(crate) fn draw_vertex_connectivity_group_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(168.0);
        for mode in [VertexConnectivity::Welded, VertexConnectivity::Detached] {
            if toolbar_menu_choice(
                ui,
                icons::label(mode.icon(), mode.label()),
                self.vertex_connectivity == mode,
            ) {
                self.set_vertex_connectivity(mode);
            }
        }
    }

    pub(crate) fn draw_visibility_menu_contents(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(224.0);
        let mut changed = false;
        egui::Grid::new("viewport-visibility-menu-grid")
            .num_columns(2)
            .spacing(Vec2::new(14.0, 5.0))
            .show(ui, |ui| {
                changed |= visibility_menu_row(ui, "grid", "Grid", &mut self.show_grid);
                changed |= visibility_menu_row(ui, "portals", "Portals", &mut self.show_portals);
                changed |= visibility_menu_row(ui, "lights", "Lights", &mut self.show_lights);
                changed |= visibility_menu_row(ui, "fog", "Fog", &mut self.preview_fog);
                changed |= visibility_menu_row(
                    ui,
                    "backfaces",
                    "Backfaces",
                    &mut self.preview_backface_wireframe,
                );
                changed |= visibility_menu_row(ui, "bounds", "Bounds", &mut self.preview_bounds);
            });
        if changed {
            self.persist_editor_visibility_state();
            self.mark_shortcut_group_changed(ShortcutGroup::Visibility);
        }
    }

    pub(crate) fn draw_camera_group_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(148.0);
        for mode in [ViewportCameraMode::Orbit, ViewportCameraMode::Free] {
            if toolbar_menu_choice(
                ui,
                icons::label(mode.icon(), viewport_camera_mode_label(mode)),
                self.camera_rig.mode == mode,
            ) {
                self.set_viewport_3d_camera_mode(mode);
                self.status = format!("Camera: {}", viewport_camera_mode_label(mode));
            }
        }
        ui.separator();
        ui.label("Zoom speed");
        let mut zoom_speed = self.camera_rig.zoom_speed();
        if ui
            .add(egui::Slider::new(&mut zoom_speed, 0.2..=3.0).fixed_decimals(1))
            .changed()
        {
            self.camera_rig.set_zoom_speed(zoom_speed);
            self.persist_editor_camera_state();
        }
    }

    pub(crate) fn draw_view_dimension_group_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(140.0);
        for view in OrthographicView::ALL {
            if toolbar_menu_choice(
                ui,
                icons::label(icons::SQUARE, view.label()),
                self.view_2d && self.orthographic_view == view,
            ) && (!self.view_2d || self.orthographic_view != view)
            {
                self.set_orthographic_view(view);
            }
        }
        if toolbar_menu_choice(ui, icons::label(icons::BOX, "3D"), !self.view_2d) && self.view_2d {
            self.view_2d = false;
            self.status = "Viewport: 3D".to_string();
            self.mark_shortcut_group_changed(ShortcutGroup::Viewport);
        }
    }

    pub(crate) fn active_tool_group_icon(&self) -> char {
        match self.active_tool {
            ViewTool::Place => self.place_kind.icon(),
            tool => tool.icon(),
        }
    }

    pub(crate) fn active_tool_group_label(&self) -> String {
        if self.active_tool == ViewTool::Place {
            self.place_kind.label().to_string()
        } else {
            self.active_tool.label().to_string()
        }
    }

    pub(crate) fn visibility_group_icon(&self) -> char {
        if self.editor_visibility_has_hidden_items() {
            icons::EYE_OFF
        } else {
            icons::EYE
        }
    }

    pub(crate) fn visibility_group_label(&self) -> &'static str {
        if self.editor_visibility_has_hidden_items() {
            "Some Hidden"
        } else {
            "All Shown"
        }
    }

    pub(crate) fn viewport_camera_mode_label(&self) -> &'static str {
        viewport_camera_mode_label(self.camera_rig.mode)
    }

    pub(crate) fn view_dimension_group_icon(&self) -> char {
        if self.view_2d {
            icons::SQUARE
        } else {
            icons::BOX
        }
    }

    pub(crate) fn view_dimension_group_label(&self) -> &'static str {
        if self.view_2d {
            self.orthographic_view.label()
        } else {
            "3D"
        }
    }

    pub(crate) fn editor_visibility_has_hidden_items(&self) -> bool {
        !self.show_grid
            || !self.show_portals
            || !self.show_lights
            || !self.preview_fog
            || !self.preview_backface_wireframe
            || !self.preview_bounds
    }

    /// Toolbar combobox for the active brush material. Selecting
    /// "Auto" leaves `brush_material = None` so paint can use the
    /// selected Material resource first, then a per-tool name hint;
    /// picking a specific entry pins every Floor / Wall / Ceiling
    /// stroke to that material.
    /// Toolbar selector for the Place tool's node kind. Shown
    /// only while `active_tool == Place` -- otherwise the brush
    /// material picker takes the same slot.
    /// Toolbar selector for the Select tool's primitive mode.
    /// Visible only while `active_tool == Select`. Clicking goes
    /// through `set_selection_mode` so the existing selection adapts.
    pub(crate) fn draw_wall_paint_shape_picker(&mut self, ui: &mut egui::Ui) {
        for shape in [
            WallPaintShape::Cardinal,
            WallPaintShape::NorthWestSouthEast,
            WallPaintShape::NorthEastSouthWest,
        ] {
            if toolbar_menu_choice(ui, shape.label(), self.wall_paint_shape == shape)
                && self.wall_paint_shape != shape
            {
                self.wall_paint_shape = shape;
                self.mark_shortcut_group_changed(ShortcutGroup::Tool);
            }
        }
    }

    pub(crate) fn draw_active_place_options(&mut self, ui: &mut egui::Ui) {
        match self.place_kind {
            PlaceKind::ModelInstance => {
                self.draw_place_resource_picker(ui, ResourceFilter::Model, "Model", false)
            }
            PlaceKind::Character => {
                self.draw_place_resource_picker(ui, ResourceFilter::Character, "Profile", false)
            }
            PlaceKind::ImageProp => {
                self.draw_place_resource_picker(ui, ResourceFilter::ImagePropSource, "Image", false)
            }
            PlaceKind::BoxProp | PlaceKind::CylinderProp | PlaceKind::ArchProp => self
                .draw_place_resource_picker(ui, ResourceFilter::ImagePropSource, "Material", true),
            PlaceKind::ParticleEmitter => {
                ui.weak(
                    "Point-projected sprite emitter. Configure texture and budget after placement.",
                );
            }
            _ => {}
        }
    }

    pub(crate) fn draw_place_resource_picker(
        &mut self,
        ui: &mut egui::Ui,
        filter: ResourceFilter,
        label: &str,
        allow_unassigned: bool,
    ) {
        let options: Vec<(ResourceId, String)> = self
            .project
            .resources
            .iter()
            .filter(|resource| filter.matches(&resource.data))
            .map(|resource| (resource.id, resource.name.clone()))
            .collect();
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(icons::text(filter.icon(), 14.0).color(STUDIO_TEXT_WEAK))
                .on_hover_text(label);
            ui.label(label);
        });

        let before = self.place_resource;
        let auto_label = if options.len() == 1 {
            format!("Auto ({})", options[0].1)
        } else if allow_unassigned {
            "None (assign later)".to_string()
        } else {
            format!("Auto {label}")
        };
        let selected_label = self
            .place_resource
            .and_then(|selected| {
                options
                    .iter()
                    .find(|(id, _)| *id == selected)
                    .map(|(_, name)| name.as_str())
            })
            .unwrap_or(&auto_label);
        let search_hint = format!("Search {}…", label.to_ascii_lowercase());
        let picker_changed = searchable_picker(
            ui,
            ui.id().with(("place-resource-picker", label)),
            &mut self.place_resource,
            selected_label,
            &options,
            SearchablePickerConfig::optional(&auto_label)
                .with_width(220.0)
                .with_popup_min_width(360.0)
                .with_search_hint(&search_hint),
        );
        if options.is_empty() {
            ui.weak(format!("No {} resources", label.to_ascii_lowercase()));
        }
        if picker_changed {
            if let Some(id) = self.place_resource {
                self.replace_resource_selection(id);
            }
        }
        if self.place_resource != before {
            let name = self
                .place_resource
                .and_then(|id| {
                    self.project
                        .resource(id)
                        .map(|resource| resource.name.clone())
                })
                .unwrap_or_else(|| "Auto".to_string());
            self.status = format!("Place {label}: {name}");
            self.mark_shortcut_group_changed(ShortcutGroup::Tool);
        }
    }

    pub(crate) fn place_resource_candidates(&self) -> impl Iterator<Item = ResourceId> {
        [self.place_resource, self.selection.selected_resource]
            .into_iter()
            .flatten()
    }

    /// Pick the Model resource a `PlaceKind::ModelInstance` prop click
    /// should bind to. Returns `(id, default_node_name)` on
    /// success or an actionable status message on failure. The
    /// caller renders the failure into `self.status` and skips
    /// the place altogether -- never silently substitutes a
    /// generic marker.
    pub(crate) fn resolve_place_model_resource(&self) -> Result<(ResourceId, String), String> {
        // (a) Chosen Place toolbar resource, or selected resource,
        // is a Model? Use it.
        for id in self.place_resource_candidates() {
            if let Some(resource) = self.project.resource(id) {
                if matches!(resource.data, ResourceData::Model(_)) {
                    return Ok((id, resource.name.clone()));
                }
            }
        }
        // (b) Exactly one Model resource? Auto-pick.
        let models: Vec<&Resource> = self
            .project
            .resources
            .iter()
            .filter(|r| matches!(r.data, ResourceData::Model(_)))
            .collect();
        match models.len() {
            0 => Err("No Model resources exist. Register or import a model first.".to_string()),
            1 => Ok((models[0].id, models[0].name.clone())),
            n => Err(format!(
                "Select a Model resource before placing a prop ({n} available)"
            )),
        }
    }

    pub(crate) fn resolve_place_image_prop_material(
        &mut self,
    ) -> Result<(ResourceId, String), String> {
        for id in self.place_resource_candidates().collect::<Vec<_>>() {
            let Some(resource) = self.project.resource(id) else {
                continue;
            };
            if matches!(&resource.data, ResourceData::Material(_)) {
                return Ok((id, resource.name.clone()));
            }
        }

        let image_sources: Vec<(ResourceId, String)> = self
            .project
            .resources
            .iter()
            .filter_map(|resource| match &resource.data {
                ResourceData::Material(_) => Some((resource.id, resource.name.clone())),
                _ => None,
            })
            .collect();
        match image_sources.len() {
            0 => Err("No Material resources exist. Import a texture first.".to_string()),
            1 => {
                let (id, name) = &image_sources[0];
                Ok((*id, name.clone()))
            }
            n => Err(format!(
                "Select a Material before placing an image prop ({n} available)"
            )),
        }
    }

    /// Resolve an optional material for a new Box Prop. A Box Prop remains
    /// useful editable geometry without a material, so an ambiguous material
    /// choice must not block placement.
    pub(crate) fn resolve_place_box_prop_material(&self) -> Option<(ResourceId, String)> {
        for id in self.place_resource_candidates() {
            let Some(resource) = self.project.resource(id) else {
                continue;
            };
            if matches!(&resource.data, ResourceData::Material(_)) {
                return Some((id, resource.name.clone()));
            }
        }

        let mut materials = self
            .project
            .resources
            .iter()
            .filter(|&resource| matches!(&resource.data, ResourceData::Material(_)))
            .map(|resource| (resource.id, resource.name.clone()));
        let only = materials.next()?;
        materials.next().is_none().then_some(only)
    }

    pub(crate) fn resolve_place_character_resource(
        &self,
    ) -> Result<(ResourceId, String, psxed_project::CharacterResource), String> {
        for id in self.place_resource_candidates() {
            if let Some(resource) = self.project.resource(id) {
                if let ResourceData::Character(character) = &resource.data {
                    return Ok((id, resource.name.clone(), character.clone()));
                }
            }
        }

        let characters: Vec<&Resource> = self
            .project
            .resources
            .iter()
            .filter(|r| matches!(r.data, ResourceData::Character(_)))
            .collect();
        match characters.len() {
            0 => Err("No Character Profile resources exist. Sync starter characters or add a profile first.".to_string()),
            1 => {
                let ResourceData::Character(character) = &characters[0].data else {
                    unreachable!("filtered to character resources");
                };
                Ok((characters[0].id, characters[0].name.clone(), character.clone()))
            }
            n => Err(format!(
                "Select a Character Profile resource before placing a character ({n} available)"
            )),
        }
    }

    pub(crate) fn draw_brush_material_picker(&mut self, ui: &mut egui::Ui) {
        let materials = self.project.material_options();
        if self.active_tool == ViewTool::PaintMaterial && self.brush_material.is_none() {
            let material = self
                .selected_material_resource()
                .or_else(|| materials.first().map(|(id, _)| *id));
            if let Some(material) = material {
                self.brush_material = Some(material);
                self.replace_resource_selection(material);
            }
        }
        let label = match self.brush_material {
            Some(id) => materials
                .iter()
                .find(|(mid, _)| *mid == id)
                .map(|(_, name)| name.clone())
                .unwrap_or_else(|| "(missing)".to_string()),
            None => {
                if self.active_tool == ViewTool::PaintMaterial {
                    "Select material".to_string()
                } else {
                    "Auto".to_string()
                }
            }
        };
        ui.label(icons::text(icons::PALETTE, 14.0).color(STUDIO_TEXT_WEAK))
            .on_hover_text("Brush material");
        let before = self.brush_material;
        let config = if self.active_tool == ViewTool::PaintMaterial {
            SearchablePickerConfig::required()
                .with_width(220.0)
                .with_popup_min_width(360.0)
                .with_search_hint("Search materials…")
        } else {
            SearchablePickerConfig::optional("Auto")
                .with_width(180.0)
                .with_popup_min_width(360.0)
                .with_search_hint("Search materials…")
        };
        searchable_picker(
            ui,
            "brush-material-picker",
            &mut self.brush_material,
            &label,
            &materials,
            config,
        );
        if self.brush_material != before {
            if let Some(material) = self.brush_material {
                self.material_paint_sampling = false;
                self.replace_resource_selection(material);
                self.status = format!(
                    "Paint material: {}",
                    self.project.resource_name(material).unwrap_or("(missing)")
                );
            }
            self.mark_shortcut_group_changed(ShortcutGroup::Tool);
        }
    }

    /// Resolve the Room node that owns the current selection, if any.
    ///
    /// Order: selected face's room → climb the selected node's
    /// Walk the active scene and collect a selectable AABB for
    /// every entity-kind node -- every node that's neither the
    /// world root, nor a structural Node/World, nor a Room.
    ///
    /// `room_filter` confines the walk to descendants of one
    /// Room (Some(id)) or includes everything (None). The 3D
    /// click handler uses Some(active_room) so a click in the
    /// active room can't pick lights from another room.
    pub fn collect_entity_bounds(&self, room_filter: Option<NodeId>) -> Vec<EntityBounds> {
        let scene = self.project.active_scene();
        let mut out = Vec::new();
        for node in scene.nodes() {
            if node.id == scene.root {
                continue;
            }
            if self.scene_node_effectively_hidden(node.id) {
                continue;
            }
            if matches!(node.kind, NodeKind::PointLight { .. }) && !self.show_lights {
                continue;
            }
            if matches!(node.kind, NodeKind::Portal { .. }) && !self.show_portals {
                continue;
            }
            // Find this node's enclosing Room.
            let enclosing_room = enclosing_room_id(scene, node.id);
            if let (Some(want), Some(actual)) = (room_filter, enclosing_room) {
                if want != actual {
                    continue;
                }
            }
            // Floor-aware selection: in the active room, a node is only
            // interactable when its floor is visible in the Sims view
            // (active floor or below), and its bounds must sit at the same
            // Y the renderer drew it. `node_draw_offset` is the shared
            // source of truth that the render pass also uses, so selection
            // and render can't disagree. Nodes in other rooms (or rooms
            // with a single floor) get offset 0.
            let floor_y_offset = match enclosing_room {
                Some(room) => {
                    match psxed_project::floor_view::node_draw_offset(
                        scene,
                        room,
                        self.active_floor,
                        node.id,
                    ) {
                        Some(offset) => offset,
                        // Floor hidden (above the active floor): not
                        // selectable this frame.
                        None => continue,
                    }
                }
                None => 0,
            };
            let Some((kind, mut half_extents)) = entity_bound_kind_and_size(self, node) else {
                continue;
            };
            // World position. Entities under a Room use the
            // canonical room-local convention so bounds line up
            // with the rendered marker / model exactly.
            let center_world = match enclosing_room.and_then(|id| scene.node(id)) {
                Some(room_node) => match &room_node.kind {
                    NodeKind::Section { grid } => {
                        // A stacked floor can grow independently from the
                        // base floor, so its width/origin (and therefore its
                        // editor-to-preview conversion) can differ. The 3D
                        // renderer places node markers with the node's own
                        // floor grid; picking must use that same grid or the
                        // clickable bound drifts away from the visible node.
                        let node_floor = psxed_project::floor_view::node_floor(scene, node.id);
                        let Some(node_grid) = grid.floor(node_floor) else {
                            continue;
                        };
                        if kind == EntityBoundKind::Portal {
                            let Some((center, half)) = portal_seam_bounds_3d(node_grid, node)
                            else {
                                continue;
                            };
                            half_extents = half;
                            center
                        } else if node_is_floor_anchored(&node.kind) {
                            psxed_project::spatial::floor_anchored_node_preview_bounds_center(
                                node_grid,
                                &node.transform,
                                half_extents,
                            )
                        } else if kind == EntityBoundKind::PointLight {
                            // The light bulb gizmo is centred exactly on the
                            // authored transform. Keep its pick box symmetric
                            // around that visible marker instead of treating
                            // the transform as the bottom of a standing prop.
                            psxed_project::spatial::node_preview_origin_f32(
                                node_grid,
                                &node.transform,
                            )
                        } else {
                            psxed_project::spatial::node_preview_bounds_center(
                                node_grid,
                                &node.transform,
                                half_extents,
                            )
                        }
                    }
                    _ => continue,
                },
                None => {
                    // No enclosing Room -- node lives in raw
                    // world space. Use translation directly so
                    // the bound at least lands somewhere
                    // pickable.
                    let p = node.transform.translation;
                    [p[0], p[1] + half_extents[1], p[2]]
                }
            };
            // Lift the bound to the floor's drawn elevation so the pick
            // box / gizmo coincides with the rendered node on a stacked
            // floor.
            let center_world = [
                center_world[0],
                center_world[1] + floor_y_offset as f32,
                center_world[2],
            ];
            out.push(EntityBounds {
                node: node.id,
                room: enclosing_room,
                kind,
                center: center_world,
                half_extents,
                yaw_degrees: node.transform.rotation_degrees[1],
            });
        }
        out
    }

    /// Pick the nearest entity bound under the camera ray.
    /// Returns the `EntityBoundHit` plus its world distance --
    /// the 3D click handler compares this against grid hits to
    /// pick whichever is closer.
    pub fn pick_entity_bound(
        &self,
        rect: egui::Rect,
        pointer: egui::Pos2,
        room_filter: Option<NodeId>,
    ) -> Option<EntityBoundHit> {
        let (origin, dir) = self.camera_ray_for_pointer(rect, pointer)?;
        let bounds = self.collect_entity_bounds(room_filter);
        let mut best: Option<EntityBoundHit> = None;
        for b in &bounds {
            let Some(t) = ray_intersects_aabb(origin, dir, b.center, b.half_extents) else {
                continue;
            };
            if best.as_ref().is_some_and(|h| h.distance <= t) {
                continue;
            }
            best = Some(EntityBoundHit {
                node: b.node,
                distance: t,
                point: [
                    origin[0] + dir[0] * t,
                    origin[1] + dir[1] * t,
                    origin[2] + dir[2] * t,
                ],
                bounds: *b,
            });
        }
        best
    }

    /// parent chain → fall back to the active scene's first Room.
    /// The fallback keeps paint tools enabled even when the
    /// selection sits outside the scene tree (e.g. a face the user
    /// just picked, which clears `selected_node` to ROOT).
    pub fn active_room_id(&self) -> Option<NodeId> {
        if let Some(selection) = self.selection.selected_primitive {
            let room = selection.room();
            if !self.scene_node_effectively_hidden(room) {
                return Some(room);
            }
        }
        let scene = self.project.active_scene();
        let mut current = self.selection.selected_node;
        while let Some(node) = scene.node(current) {
            if matches!(node.kind, NodeKind::Section { .. })
                && !self.scene_node_effectively_hidden(current)
            {
                return Some(current);
            }
            let Some(parent) = node.parent else { break };
            current = parent;
        }
        scene
            .nodes()
            .iter()
            .find(|node| {
                matches!(node.kind, NodeKind::Section { .. })
                    && !self.scene_node_effectively_hidden(node.id)
            })
            .map(|node| node.id)
    }

    /// Translate a 2D-viewport-space click into a sector cell on
    /// `room`. The viewport draws cells around `node_world(room)`
    /// with 1 unit = 1 sector, so the click is first re-expressed
    /// as editor coords (room-centre-relative) and then routed
    /// through `WorldGrid::editor_cells_to_array`. `origin` enters
    /// the conversion via the canonical helper, keeping 2D and 3D
    /// picks consistent after a negative-side grow.
    pub(crate) fn world_to_sector(&self, room_id: NodeId, world: [f32; 2]) -> Option<(u16, u16)> {
        let room = self.project.active_scene().node(room_id)?;
        let center = node_world(room);
        let grid = self.room_grid_view(room_id)?;
        let editor = [world[0] - center[0], world[1] - center[1]];
        grid.editor_cells_to_array(editor)
    }

    /// Default material id for a brushed surface, picked by name from
    /// the project's material list. The cooker rejects unassigned
    /// surfaces, so authors are expected to wire real materials in
    /// resources before serious painting -- this fallback at least
    /// keeps the brush usable while iterating.
    pub(crate) fn default_brush_material(&self, needle: &str) -> Option<ResourceId> {
        let lower = needle.to_ascii_lowercase();
        let materials = self.project.material_options();
        materials
            .iter()
            .find(|(_, name)| name.to_ascii_lowercase().contains(&lower))
            .or_else(|| materials.first())
            .map(|(id, _)| *id)
    }

    pub(crate) fn selected_material_resource(&self) -> Option<ResourceId> {
        let id = self.selection.selected_resource?;
        matches!(
            self.project.resource(id).map(|resource| &resource.data),
            Some(ResourceData::Material(_))
        )
        .then_some(id)
    }

    pub(crate) fn paint_material_for(&self, default_name_hint: &str) -> Option<ResourceId> {
        self.brush_material
            .or_else(|| self.selected_material_resource())
            .or_else(|| self.default_brush_material(default_name_hint))
    }

    pub(crate) fn first_material(&self) -> Option<ResourceId> {
        self.project.material_options().first().map(|(id, _)| *id)
    }

    pub(crate) fn has_player_source(&self) -> bool {
        self.project
            .active_scene()
            .nodes()
            .iter()
            .any(|node| node_kind_is_player_source(&node.kind))
    }

    pub(crate) fn selected_node_is_player_source(&self) -> bool {
        self.project
            .active_scene()
            .node(self.selection.selected_node)
            .is_some_and(|node| node_kind_is_player_source(&node.kind))
    }

    pub(crate) fn demote_player_sources_except(&mut self, keep: Option<NodeId>) {
        let scene = self.project.active_scene_mut();
        let ids: Vec<NodeId> = scene
            .nodes()
            .iter()
            .filter(|node| Some(node.id) != keep && node_kind_is_player_source(&node.kind))
            .map(|node| node.id)
            .collect();
        for id in ids {
            let Some(node) = scene.node_mut(id) else {
                continue;
            };
            match &mut node.kind {
                NodeKind::SpawnPoint { player, character } => {
                    *player = false;
                    *character = None;
                }
                NodeKind::CharacterController { player, .. } => {
                    *player = false;
                }
                _ => {}
            }
        }
    }
}
