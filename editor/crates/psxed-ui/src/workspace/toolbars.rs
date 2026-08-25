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
                        surrender_stale_focus_on_viewport_pointer(ui.ctx(), &response);
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
                            if let Some(pointer) =
                                response.interact_pointer_pos().or(response.hover_pos())
                            {
                                let world = transform.screen_to_world(pointer);
                                if let Some(resource_id) = dropped_resource {
                                    self.drop_resource_2d(resource_id, world);
                                }
                            }
                        }
                        let painter = ui.painter_at(rect);
                        painter.rect_filled(rect, 0.0, STUDIO_VIEWPORT);
                        if self.show_grid {
                            draw_world_grid(&painter, transform, self.snap_units.max(1) as f32);
                        }
                        // Background and projected surface grids are independent:
                        // hiding the viewport grid must not also erase the grid
                        // laid over brush faces.
                        let brush_surface_grid_step = self
                            .show_brush_surface_grid
                            .then_some(self.snap_units.max(1) as f32);

                        let hits = if top_view {
                            draw_scene_viewport(
                                &painter,
                                transform,
                                &self.project,
                                SceneViewportContext {
                                    hidden_scene_nodes: &self.hidden_scene_nodes,
                                    selected: self.selection.selected_node,
                                    selected_nodes: &self.selection.selected_nodes,
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
                            self.draw_bsp_leak_path_2d(&painter, transform, orthographic_view);
                            draw_axes_gizmo(&painter, rect, orthographic_view);
                            self.draw_bsp_leak_notice(&painter, rect);
                            return;
                        }
                        let brush_edit_active = matches!(self.active_tool, ViewTool::Brush)
                            || (matches!(self.active_tool, ViewTool::Select)
                                && self.selected_brush.is_some());
                        if !dnd_active && brush_edit_active {
                            self.selection.hovered_brush_handle = None;
                            if response.hovered() {
                                if let Some(world) = pointer_world {
                                    let tolerance = 8.0 / self.viewport_zoom.max(f32::EPSILON);
                                    self.selection.hovered_brush_handle = self
                                        .pick_brush_elements_2d(world, tolerance)
                                        .first()
                                        .copied();
                                }
                            }
                            if response.hovered()
                                && pointer_world.is_some_and(|world| {
                                    self.brush_edit_mode != BrushEditMode::Move
                                        || self.pick_brush_face_for_move_at_2d(world).is_some()
                                })
                            {
                                ui.ctx().set_cursor_icon(match self.brush_edit_mode {
                                    BrushEditMode::Move => egui::CursorIcon::Grab,
                                    BrushEditMode::Face => egui::CursorIcon::ResizeHorizontal,
                                    BrushEditMode::Edge
                                    | BrushEditMode::Vertex
                                    | BrushEditMode::Clip => egui::CursorIcon::Crosshair,
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
                                                // Clip is click-driven; a drag
                                                // starts nothing.
                                                BrushEditMode::Clip => false,
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
                            let can_box_select = bsp_brush_marquee;
                            if can_box_select {
                                if let Some(start) = ui
                                    .input(|input| input.pointer.press_origin())
                                    .or_else(|| response.interact_pointer_pos())
                                {
                                    let modifiers = ui.input(|input| input.modifiers);
                                    self.begin_viewport_box_select(start, None, modifiers);
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
                                let group_double_click = response
                                    .double_clicked_by(egui::PointerButton::Primary)
                                    && self.handle_group_double_click_2d(world, &hits);
                                if !group_double_click {
                                    self.handle_viewport_click(world, &hits, modifiers);
                                }
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
                        self.draw_brush_footprints_2d(&painter, transform, brush_surface_grid_step);
                        self.draw_bsp_leak_path_2d(&painter, transform, orthographic_view);
                        draw_axes_gizmo(&painter, rect, orthographic_view);
                        self.draw_bsp_leak_notice(&painter, rect);
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
                } else if self.active_workspace == WorkspaceView::Animation {
                    let action = model_animation_viewer::draw_model_animation_viewer_toolbar(
                        ui,
                        &mut self.project,
                        &self.project_dir,
                        &mut self.animation_viewer,
                        &mut self.animation_viewer_preview_texture,
                    );
                    if let Some(action) = action {
                        self.handle_animation_viewer_action(action);
                    }
                } else {
                    ui.horizontal(|ui| self.draw_viewport_toolbar(ui));
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
        self.retain_shortcut_group_flash();
        if self.shortcut_group_flash.is_some() {
            ui.ctx().request_repaint();
        }
        ui.spacing_mut().item_spacing.x = 4.0;
        if self.active_workspace == WorkspaceView::Ui {
            self.draw_ui_viewport_toolbar(ui);
            return;
        }

        self.draw_bsp_mode_strip(ui);
        self.draw_bsp_add_menu(ui);
        self.draw_bsp_context_controls(ui);
        ui.separator();
        self.draw_grid_controls(ui);
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
        toolbar_group_menu_icon_only(
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
        toolbar_group_menu_icon_only(
            ui,
            9,
            self.shortcut_group_glow(ShortcutGroup::Viewport),
            self.view_dimension_group_icon(),
            "Viewport",
            self.view_dimension_group_label(),
            |ui| self.draw_view_dimension_group_menu(ui),
        );
    }

    pub(crate) fn active_bsp_toolbar_mode(&self) -> Option<BspToolbarMode> {
        match self.active_tool {
            ViewTool::Brush => Some(BspToolbarMode::Draw),
            ViewTool::PaintMaterial => Some(BspToolbarMode::Paint),
            ViewTool::Select => Some(match self.brush_edit_mode {
                BrushEditMode::Move => BspToolbarMode::Select,
                BrushEditMode::Face => BspToolbarMode::Face,
                BrushEditMode::Edge => BspToolbarMode::Edge,
                BrushEditMode::Vertex => BspToolbarMode::Vertex,
                BrushEditMode::Clip => BspToolbarMode::Clip,
            }),
            ViewTool::Place => None,
        }
    }

    pub(crate) fn set_bsp_toolbar_mode(&mut self, mode: BspToolbarMode) {
        let tool = match mode {
            BspToolbarMode::Draw => ViewTool::Brush,
            BspToolbarMode::Paint => ViewTool::PaintMaterial,
            BspToolbarMode::Select
            | BspToolbarMode::Face
            | BspToolbarMode::Edge
            | BspToolbarMode::Vertex
            | BspToolbarMode::Clip => ViewTool::Select,
        };
        self.set_active_tool_cycle_value((tool, None));
        if let Some(brush_mode) = mode.brush_edit_mode() {
            self.set_brush_edit_mode(brush_mode);
        }
        self.status = format!("Mode: {}", mode.label());
    }

    fn draw_bsp_mode_strip(&mut self, ui: &mut egui::Ui) {
        for (index, mode) in BspToolbarMode::ALL.into_iter().enumerate() {
            let response = ui
                .add(
                    egui::Button::new(icons::text(mode.icon(), 15.0))
                        .selected(self.active_bsp_toolbar_mode() == Some(mode))
                        .min_size(Vec2::new(28.0, 23.0)),
                )
                .on_hover_text(format!(
                    "{} ({})\n{}",
                    mode.label(),
                    index + 1,
                    match mode {
                        BspToolbarMode::Select => {
                            "Select and transform whole brushes or entities\nUse the Vertex Snap button or hold B, then drag a yellow corner"
                        }
                        BspToolbarMode::Draw => {
                            "Drag the selected brush primitive in an orthographic or 3D view"
                        }
                        BspToolbarMode::Paint => "Paint or sample one brush face material",
                        BspToolbarMode::Face => "Select, resize, rotate, scale or extrude faces",
                        BspToolbarMode::Edge => "Select and reshape brush edges",
                        BspToolbarMode::Vertex => "Select and reshape brush vertices",
                        BspToolbarMode::Clip => "Place a cutting plane and clip selected brushes",
                    }
                ));
            if response.clicked() {
                self.set_bsp_toolbar_mode(mode);
            }
        }
        let vertex_snap = ui
            .add(
                egui::Button::new(RichText::new("V↔V").monospace().size(11.0))
                    .selected(self.brush_vertex_snap_enabled)
                    .min_size(Vec2::new(32.0, 23.0)),
            )
            .on_hover_text(
                "Vertex Snap (B)\nToggle, then drag a yellow corner of the selected brush onto a green corner of another brush. Hold B for momentary use.",
            );
        if vertex_snap.clicked() {
            self.brush_vertex_snap_enabled = !self.brush_vertex_snap_enabled;
            if self.brush_vertex_snap_enabled {
                self.set_bsp_toolbar_mode(BspToolbarMode::Select);
                self.status = "Vertex Snap enabled: drag a yellow corner onto another brush corner"
                    .to_string();
            } else {
                self.brush_vertex_snap_key_down = false;
                self.brush_vertex_snap_hover = None;
                self.status = "Vertex Snap disabled (hold B for momentary use)".to_string();
            }
        }
    }

    fn draw_bsp_add_menu(&mut self, ui: &mut egui::Ui) {
        let button = egui::Button::new(icons::text(icons::PLUS, 15.0))
            .selected(self.active_tool == ViewTool::Place)
            .min_size(Vec2::new(28.0, 23.0));
        let response = egui::menu::menu_custom_button(ui, button, |ui| {
            ui.set_min_width(210.0);
            ui.label(
                RichText::new("ADD TO WORLD")
                    .small()
                    .color(STUDIO_TEXT_WEAK),
            );
            for kind in PlaceKind::ALL {
                let selected = self.active_tool == ViewTool::Place && self.place_kind == kind;
                if toolbar_menu_choice(ui, icons::label(kind.icon(), kind.label()), selected) {
                    self.set_active_tool_cycle_value((ViewTool::Place, Some(kind)));
                }
            }
        })
        .response;
        response.on_hover_text("Add an enemy, spawn, prop, light, particle emitter or logic node");
    }

    fn draw_bsp_context_controls(&mut self, ui: &mut egui::Ui) {
        match self.active_tool {
            ViewTool::Select => match self.brush_edit_mode {
                BrushEditMode::Move | BrushEditMode::Edge | BrushEditMode::Vertex => {
                    ui.separator();
                    self.draw_transform_gizmo_toolbar_controls(ui);
                }
                BrushEditMode::Face => {
                    ui.separator();
                    self.draw_transform_gizmo_toolbar_controls(ui);
                    let enabled =
                        self.selected_brush.is_some() && self.selected_brush_face.is_some();
                    if ui
                        .add_enabled(enabled, egui::Button::new("Extrude"))
                        .on_hover_text("Extrude the selected face by one grid step (E)")
                        .clicked()
                    {
                        self.extrude_selected_face_one_step();
                    }
                }
                BrushEditMode::Clip => {
                    ui.separator();
                    if ui
                        .button(self.brush_clip_keep.label())
                        .on_hover_text("Kept side; X or Tab cycles")
                        .clicked()
                    {
                        self.brush_clip_keep = self.brush_clip_keep.next();
                    }
                    let can_apply = self.brush_clip_plane_points().is_some()
                        && !self.selected_brush_set().is_empty();
                    if ui
                        .add_enabled(can_apply, egui::Button::new("Apply"))
                        .on_hover_text("Apply clip (Enter)")
                        .clicked()
                    {
                        self.apply_brush_clip();
                    }
                    if ui
                        .add_enabled(
                            !self.brush_clip_points.is_empty(),
                            egui::Button::new("Cancel"),
                        )
                        .on_hover_text("Clear clip points (Esc)")
                        .clicked()
                    {
                        self.cancel_brush_gestures();
                    }
                }
            },
            ViewTool::PaintMaterial => {
                self.draw_material_paint_toolbar_controls(ui);
            }
            ViewTool::Brush => {
                ui.separator();
                let shape = self.brush_draw_settings.shape;
                toolbar_option_menu(
                    ui,
                    icons::BOX,
                    shape.label(),
                    "Brush primitive",
                    shape.label(),
                    shape != BrushDrawShape::Box,
                    |ui| self.draw_brush_draw_options(ui),
                );
            }
            ViewTool::Place
                if matches!(
                    self.place_kind,
                    PlaceKind::ModelInstance
                        | PlaceKind::Character
                        | PlaceKind::ImageProp
                        | PlaceKind::BoxProp
                        | PlaceKind::CylinderProp
                        | PlaceKind::ArchProp
                ) =>
            {
                ui.separator();
                toolbar_option_menu(
                    ui,
                    self.place_kind.icon(),
                    "Options",
                    "Placement options",
                    self.place_kind.label(),
                    true,
                    |ui| self.draw_active_place_options(ui),
                );
            }
            ViewTool::Place => {}
        }
    }

    fn draw_brush_draw_options(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(250.0);
        ui.label(
            RichText::new("BRUSH PRIMITIVE")
                .small()
                .color(STUDIO_TEXT_WEAK),
        );
        for shape in BrushDrawShape::ALL {
            if ui
                .selectable_value(
                    &mut self.brush_draw_settings.shape,
                    shape,
                    icons::label(icons::BOX, shape.label()),
                )
                .changed()
            {
                self.status = format!("Draw primitive: {}", shape.label());
            }
        }

        ui.separator();
        let mut changed = false;
        let shape = self.brush_draw_settings.shape;
        if shape == BrushDrawShape::DoorwayArch {
            ui.label("Opening axis");
            ui.horizontal(|ui| {
                let north_south = matches!(
                    self.brush_draw_settings.direction,
                    BrushCardinalDirection::North | BrushCardinalDirection::South
                );
                if ui.selectable_label(north_south, "North / South").clicked() {
                    self.brush_draw_settings.direction = BrushCardinalDirection::North;
                    changed = true;
                }
                if ui.selectable_label(!north_south, "East / West").clicked() {
                    self.brush_draw_settings.direction = BrushCardinalDirection::East;
                    changed = true;
                }
            });
        } else if matches!(
            shape,
            BrushDrawShape::Ramp | BrushDrawShape::CurvedWall | BrushDrawShape::Stairs
        ) {
            let direction_label = match shape {
                BrushDrawShape::Ramp | BrushDrawShape::Stairs => "Rises toward",
                BrushDrawShape::CurvedWall => "Arc faces",
                _ => unreachable!(),
            };
            ui.label(direction_label);
            ui.horizontal_wrapped(|ui| {
                for direction in BrushCardinalDirection::ALL {
                    changed |= ui
                        .selectable_value(
                            &mut self.brush_draw_settings.direction,
                            direction,
                            direction
                                .label()
                                .split(' ')
                                .next()
                                .unwrap_or(direction.label()),
                        )
                        .changed();
                }
            });
        }

        match shape {
            BrushDrawShape::Cylinder => {
                ui.horizontal(|ui| {
                    ui.label("Sides");
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut self.brush_draw_settings.cylinder_sides)
                                .range(3..=32),
                        )
                        .changed();
                });
                ui.weak("World-up prism; 8 sides is the PS1-friendly default.");
            }
            BrushDrawShape::DoorwayArch | BrushDrawShape::CurvedWall => {
                ui.horizontal(|ui| {
                    ui.label("Segments");
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut self.brush_draw_settings.arch_segments)
                                .range(2..=24),
                        )
                        .changed();
                });
                ui.horizontal(|ui| {
                    ui.label("Wall thickness");
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut self.brush_draw_settings.arch_thickness)
                                .range(1..=4096)
                                .speed(f64::from(self.snap_units.max(1)))
                                .suffix(" u"),
                        )
                        .changed();
                });
                if shape == BrushDrawShape::CurvedWall {
                    ui.label("Arc");
                    ui.horizontal(|ui| {
                        for degrees in [90, 180, 270, 360] {
                            changed |= ui
                                .selectable_value(
                                    &mut self.brush_draw_settings.curved_wall_arc_degrees,
                                    degrees,
                                    format!("{degrees}°"),
                                )
                                .changed();
                        }
                    });
                }
                ui.weak("Generated pieces are grouped, but remain ordinary brushes.");
            }
            BrushDrawShape::Stairs => {
                ui.horizontal(|ui| {
                    ui.label("Steps");
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut self.brush_draw_settings.stair_steps)
                                .range(1..=32),
                        )
                        .changed();
                });
                ui.weak("Run and total rise come from the dragged bounds.");
            }
            BrushDrawShape::Ramp => {
                ui.weak("The ramp always rises along world Y.");
            }
            BrushDrawShape::Box => {
                ui.weak("The standard axis-aligned convex brush.");
            }
        }
        if changed {
            self.status = format!("{} primitive options updated", shape.label());
        }
        ui.separator();
        ui.weak("Every generated point is quantised to the active global grid.");
    }

    fn draw_grid_controls(&mut self, ui: &mut egui::Ui) {
        let master_on = self.show_grid || self.show_brush_surface_grid;
        let master_response = ui
            .add(
                egui::Button::new(icons::text(icons::GRID, 14.0))
                    .selected(master_on)
                    .min_size(Vec2::new(28.0, 23.0)),
            )
            .on_hover_text(if master_on {
                "Grid overlays on; click to hide background and surface grids"
            } else {
                "Grid overlays off; click to show background and surface grids"
            });
        if master_response.clicked() {
            self.toggle_grid_overlays();
        }

        let button_text = self.snap_units.max(1).to_string();
        let current = format!(
            "{} units · background {} · surfaces {}",
            self.snap_units.max(1),
            if self.show_grid { "shown" } else { "hidden" },
            if self.show_brush_surface_grid {
                "shown"
            } else {
                "hidden"
            },
        );
        toolbar_option_menu(
            ui,
            icons::CHEVRON_DOWN,
            &button_text,
            "Grid settings",
            current,
            false,
            |ui| {
                ui.set_min_width(238.0);
                let visibility_before = (self.show_grid, self.show_brush_surface_grid);
                ui.checkbox(&mut self.show_grid, "Background grid");
                ui.checkbox(
                    &mut self.show_brush_surface_grid,
                    "Project grid over brush faces",
                );
                if visibility_before != (self.show_grid, self.show_brush_surface_grid) {
                    self.persist_editor_visibility_state();
                    self.mark_shortcut_group_changed(ShortcutGroup::Visibility);
                }

                ui.separator();
                ui.label(
                    RichText::new("QUANTISATION")
                        .small()
                        .color(STUDIO_TEXT_WEAK),
                );
                let snap_before = self.snap_units;
                ui.add(
                    egui::DragValue::new(&mut self.snap_units)
                        .range(1..=256)
                        .speed(1.0)
                        .prefix("Grid "),
                );
                ui.horizontal_wrapped(|ui| {
                    for step in [1_u16, 2, 4, 8, 16, 32, 64, 128, 256] {
                        ui.selectable_value(&mut self.snap_units, step, step.to_string());
                    }
                });
                if self.snap_units != snap_before {
                    self.persist_editor_viewport_state();
                    self.status = format!("Grid: {} units", self.snap_units);
                }
                ui.weak("Brush geometry always snaps to this interval.");
            },
        );
        let can_snap_level = !self.project.active_scene().brushes.is_empty();
        if ui
            .add_enabled(
                can_snap_level,
                egui::Button::new(RichText::new("Snap level").size(11.0))
                    .min_size(Vec2::new(68.0, 23.0)),
            )
            .on_hover_text(format!(
                "Snap every BSP brush vertex in this level to the nearest {}-unit grid line. Preflights the entire level, changes everything as one undo step, and aborts rather than creating an invalid brush.",
                self.snap_units.max(1)
            ))
            .clicked()
        {
            self.snap_all_brushes_to_grid();
        }
    }

    pub(crate) fn toggle_grid_overlays(&mut self) {
        let visible = !(self.show_grid || self.show_brush_surface_grid);
        self.show_grid = visible;
        self.show_brush_surface_grid = visible;
        self.persist_editor_visibility_state();
        self.status = if visible {
            "Grid overlays shown".to_string()
        } else {
            "Grid overlays hidden".to_string()
        };
    }

    pub(crate) fn set_brush_edit_mode(&mut self, mode: BrushEditMode) {
        if self.brush_edit_mode == mode {
            return;
        }
        self.cancel_brush_gestures();
        let previous = self.brush_edit_mode;
        self.brush_edit_mode = mode;
        if previous == BrushEditMode::Face && mode != BrushEditMode::Face {
            self.selected_brush_faces.clear();
            self.selected_brush_elements.clear();
            self.selected_brushes = self.selected_brush.into_iter().collect();
        } else if mode == BrushEditMode::Face {
            self.selected_brush_faces.clear();
            self.selected_brush_elements.clear();
            if let (Some(brush), Some(face)) = (self.selected_brush, self.selected_brush_face) {
                self.selected_brush_faces.push((brush, face));
                self.selected_brush_elements.push(BrushElement::Face(face));
                self.selected_brushes = vec![brush];
            }
        }
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

    fn draw_transform_gizmo_toolbar_controls(&mut self, ui: &mut egui::Ui) {
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
    }

    fn draw_material_paint_toolbar_controls(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        self.draw_brush_material_picker(ui);
        let eyedropper = ui
            .toggle_value(
                &mut self.material_paint_sampling,
                icons::text(icons::PIPETTE, 14.0),
            )
            .on_hover_text("Eyedropper: sample the material from the next BSP face you click.");
        if eyedropper.changed() {
            self.status = if self.material_paint_sampling {
                "Eyedropper: click a surface to sample its material".to_string()
            } else {
                "Eyedropper cancelled".to_string()
            };
            self.mark_shortcut_group_changed(ShortcutGroup::Tool);
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

    pub(crate) fn draw_visibility_menu_contents(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(224.0);
        let mut changed = false;
        egui::Grid::new("viewport-visibility-menu-grid")
            .num_columns(2)
            .spacing(Vec2::new(14.0, 5.0))
            .show(ui, |ui| {
                changed |= visibility_menu_row(ui, "lights", "Lights", &mut self.show_lights);
                changed |= visibility_menu_row(ui, "bounds", "Bounds", &mut self.preview_bounds);
                changed |= visibility_menu_row(
                    ui,
                    "brush-wireframes",
                    "Brush wireframes",
                    &mut self.show_brush_wireframes,
                );
                if self.bsp_leak_path_current && !self.last_bsp_leak_path.is_empty() {
                    changed |= visibility_menu_row(
                        ui,
                        "bsp-leak-path",
                        "Live leak path",
                        &mut self.show_bsp_leak_path,
                    );
                }
            });
        if changed {
            self.persist_editor_visibility_state();
            self.mark_shortcut_group_changed(ShortcutGroup::Visibility);
        }
    }

    pub(crate) fn draw_camera_group_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(238.0);
        for mode in [ViewportCameraMode::Orbit, ViewportCameraMode::Free] {
            if toolbar_menu_choice(
                ui,
                icons::label(mode.icon(), viewport_camera_mode_label(mode)),
                self.camera_rig.mode == mode,
            ) {
                self.set_viewport_3d_camera_mode(mode);
                self.status = match mode {
                    ViewportCameraMode::Orbit => "Camera: Orbit (RMB/MMB drag; Shift pans)".into(),
                    ViewportCameraMode::Free => {
                        "Camera: Free (WASD, Shift faster, RMB/MMB look)".into()
                    }
                };
            }
        }
        ui.add_space(3.0);
        ui.label(
            RichText::new("Free: WASD fly · Shift faster\nRMB/MMB drag look · wheel forward/back")
                .small()
                .color(STUDIO_TEXT_WEAK),
        );
        if self.bsp_leak_path_current && !self.last_bsp_leak_path.is_empty() {
            ui.separator();
            if !self.last_bsp_leak_opening.is_empty()
                && ui
                    .button(icons::label(icons::FOCUS, "Jump to leak region"))
                    .on_hover_text(
                        "Frame the connected coplanar empty-portal region around the route bottleneck; red shows the merged region and green remains the authoritative route",
                    )
                    .clicked()
            {
                self.jump_to_bsp_leak_opening();
                ui.close_menu();
            }
            let next = self.bsp_leak_cursor.min(self.last_bsp_leak_path.len() - 1) + 1;
            if ui
                .button(icons::label(
                    icons::FOCUS,
                    &format!(
                        "Follow leak path ({next}/{})",
                        self.last_bsp_leak_path.len()
                    ),
                ))
                .on_hover_text("Move the Free camera to this point and look toward the next one")
                .clicked()
            {
                self.follow_next_bsp_leak_point();
                ui.close_menu();
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
        !self.show_lights
            || !self.preview_bounds
            || (!self.last_bsp_leak_path.is_empty() && !self.show_bsp_leak_path)
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
            0 => Err("No Character Profile resources exist. Sync starter content or add a profile first.".to_string()),
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
            if matches!(node.kind, NodeKind::Portal { .. }) {
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
