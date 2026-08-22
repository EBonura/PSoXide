use super::*;

impl EditorWorkspace {
    /// Select a UI scene by exact/case-insensitive name or zero-based index.
    /// Intended for deterministic headless captures and other scripted editor
    /// entry points; this only changes transient workspace state.
    pub fn focus_ui_scene(&mut self, selector: &str) -> bool {
        let numeric_index = selector.parse::<usize>().ok();
        let index = self
            .project
            .ui_scenes
            .iter()
            .enumerate()
            .find(|(index, scene)| {
                numeric_index.is_some_and(|candidate| candidate == *index)
                    || scene.name == selector
                    || scene.name.eq_ignore_ascii_case(selector)
            })
            .map(|(index, _)| index);
        let Some(index) = index else {
            return false;
        };
        self.switch_ui_scene(index);
        true
    }

    /// List position of the UI scene the editor is currently authoring,
    /// clamped into range. Returns 0 when there are no scenes. Read this
    /// first (by value) before taking a mutable borrow of `self.project`
    /// so the borrow stays disjoint.
    pub(crate) fn current_ui_scene_index(&self) -> usize {
        let count = self.project.ui_scenes.len();
        if count == 0 {
            0
        } else {
            self.active_ui_scene_index.min(count - 1)
        }
    }

    /// The UI scene the editor is currently authoring, resolved through
    /// the clamped active index. Replaces `active_ui_scene()` at read
    /// sites so the editor reads the selected scene.
    pub(crate) fn current_ui_scene(&self) -> Option<&UiScene> {
        self.project.ui_scene_at(self.current_ui_scene_index())
    }

    /// Mutable counterpart to [`Self::current_ui_scene`]. Reads the
    /// clamped index by value first so the mutable borrow is purely
    /// `&mut self.project`.
    pub(crate) fn current_ui_scene_mut(&mut self) -> Option<&mut UiScene> {
        let index = self.current_ui_scene_index();
        self.project.ui_scene_at_mut(index)
    }

    /// List position of the screen state currently selected in the arranger.
    pub(crate) fn current_scene_state_index(&self) -> usize {
        let count = self.project.scene_states.len();
        if count == 0 {
            0
        } else {
            self.active_scene_state_index.min(count - 1)
        }
    }

    pub(crate) fn switch_scene_state(&mut self, index: usize) {
        let count = self.project.scene_states.len();
        if count == 0 {
            return;
        }
        let clamped = index.min(count - 1);
        self.active_scene_state_index = clamped;
        if let Some(ui_scene) = self
            .project
            .scene_state_at(clamped)
            .and_then(|state| state.ui_scene)
            .and_then(|id| {
                self.project
                    .ui_scenes
                    .iter()
                    .position(|scene| scene.id == id)
            })
        {
            self.switch_ui_scene(ui_scene);
        }
        self.status = self
            .project
            .scene_state_at(clamped)
            .map(|state| format!("Screen state: {}", state.name))
            .unwrap_or_else(|| "Screen state".to_string());
    }

    /// Switch the editor to author the UI scene at `index`. Clamps the
    /// index, resets the UI-node selection so a stale node id from the
    /// previous scene is never carried over, and cancels any in-flight
    /// scene-strip rename / delete-confirm. No document mutation, so no
    /// undo step is pushed.
    pub(crate) fn switch_ui_scene(&mut self, index: usize) {
        let count = self.project.ui_scenes.len();
        if count == 0 {
            return;
        }
        let clamped = index.min(count - 1);
        if clamped != self.active_ui_scene_index {
            self.ui_scene_renaming = None;
            self.ui_scene_delete_confirm = None;
        }
        self.active_ui_scene_index = clamped;
        self.reset_ui_node_selection();
        self.status = self
            .current_ui_scene()
            .map(|scene| format!("UI scene: {}", scene.name))
            .unwrap_or_else(|| "UI scene".to_string());
    }

    /// Reset the UI-node selection to the current scene's root canvas so
    /// a node id authored in another scene is never carried across a
    /// scene switch or CRUD edit. Also drops any in-flight canvas drag.
    pub(crate) fn reset_ui_node_selection(&mut self) {
        let root = self
            .current_ui_scene()
            .map(|scene| scene.root)
            .unwrap_or(UiNodeId::ROOT);
        self.selection.selected_ui_node = root;
        self.interaction.take_ui_canvas_drag();
    }

    pub(crate) fn draw_ui_workspace_body(&mut self, ui: &mut egui::Ui) {
        self.refresh_texture_thumbs(ui.ctx());
        let Some(scene) = self.current_ui_scene().cloned() else {
            ui.centered_and_justified(|ui| {
                ui.weak("No UI scene");
            });
            return;
        };
        let (canvas_w, canvas_h) = ui_scene_canvas_size(&scene);
        let aspect = (canvas_w as f32 / canvas_h.max(1) as f32).max(0.01);
        let avail = ui.available_size();
        let container_size = Vec2::new(avail.x.max(1.0), avail.y.max(1.0));
        let (container, _) = ui.allocate_exact_size(container_size, Sense::hover());
        let canvas_rect = centered_aspect_rect(container, aspect);
        let canvas_interact_rect = canvas_rect.expand(UI_RESIZE_HANDLE_HIT_SIZE);
        let canvas_size = [canvas_w, canvas_h];
        let hidden_ui_nodes = self.hidden_ui_nodes.clone();
        let response = ui.interact(
            canvas_interact_rect,
            ui.id().with("ui_canvas_preview"),
            Sense::click_and_drag(),
        );
        let pointer = response
            .hover_pos()
            .or_else(|| response.interact_pointer_pos());
        let hovered_resize_target = pointer.and_then(|pos| {
            ui_scene_resize_handle_target(
                &scene,
                &hidden_ui_nodes,
                self.selection.selected_ui_node,
                canvas_rect,
                canvas_size,
                pos,
            )
        });
        let hovered_node = pointer.and_then(|pos| {
            ui_scene_hit_test(&scene, &hidden_ui_nodes, canvas_rect, canvas_size, pos)
        });
        if response.hovered() {
            if let Some((_, handle)) = hovered_resize_target {
                ui.output_mut(|output| output.cursor_icon = handle.cursor());
            } else if hovered_node.is_some() {
                ui.output_mut(|output| output.cursor_icon = egui::CursorIcon::Grab);
            }
        }

        if response.drag_started_by(egui::PointerButton::Primary) {
            if let Some(pos) = response.interact_pointer_pos() {
                if self.ui_transform_mode == UiTransformMode::Move {
                    let resize_target = ui_scene_resize_handle_target(
                        &scene,
                        &hidden_ui_nodes,
                        self.selection.selected_ui_node,
                        canvas_rect,
                        canvas_size,
                        pos,
                    );
                    if let Some((node, handle)) = resize_target {
                        self.begin_ui_canvas_drag(
                            node,
                            UiCanvasDragMode::Resize(handle),
                            canvas_rect,
                            canvas_size,
                            pos,
                        );
                    } else if let Some(id) =
                        ui_scene_hit_test(&scene, &hidden_ui_nodes, canvas_rect, canvas_size, pos)
                    {
                        self.begin_ui_canvas_drag(
                            id,
                            UiCanvasDragMode::Move,
                            canvas_rect,
                            canvas_size,
                            pos,
                        );
                    }
                } else if let Some(id) =
                    ui_scene_hit_test(&scene, &hidden_ui_nodes, canvas_rect, canvas_size, pos)
                {
                    self.begin_ui_canvas_drag(
                        id,
                        UiCanvasDragMode::Rotate,
                        canvas_rect,
                        canvas_size,
                        pos,
                    );
                }
            }
        }
        if response.dragged_by(egui::PointerButton::Primary) {
            if let Some(pos) = response.interact_pointer_pos().or(response.hover_pos()) {
                self.update_ui_canvas_drag(canvas_rect, canvas_size, pos);
            }
        }
        if response.clicked_by(egui::PointerButton::Primary) {
            let picked = response
                .interact_pointer_pos()
                .or(response.hover_pos())
                .and_then(|pos| {
                    ui_scene_hit_test(&scene, &hidden_ui_nodes, canvas_rect, canvas_size, pos)
                })
                .unwrap_or(scene.root);
            self.select_ui_node(picked);
        }
        let primary_down =
            ui.input(|input| input.pointer.button_down(egui::PointerButton::Primary));
        if !primary_down {
            self.interaction.take_ui_canvas_drag();
        }

        let painter = ui.painter_at(container.expand(UI_RESIZE_HANDLE_SIZE));
        painter.rect_filled(container, 0.0, STUDIO_VIEWPORT);
        painter.rect_filled(canvas_rect, 0.0, Color32::from_rgb(10, 12, 18));
        painter.rect_stroke(
            canvas_rect,
            0.0,
            Stroke::new(1.0, STUDIO_BORDER),
            StrokeKind::Inside,
        );
        let preview_scene = self.current_ui_scene().cloned().unwrap_or(scene);
        // Resolve the bitmap-font texture before the immutable project borrows
        // below, since rasterizing it needs `&mut self`.
        let ui_fonts = self.ui_font_textures(ui.ctx());
        // Screen-offset (TV-centring) simulation: slide the previewed picture
        // horizontally within the fixed screen, clipped at the bezel, so the
        // authored GP1(06h) offset can be eyeballed here. Node hit-testing keeps
        // using the centred `canvas_rect` above, so authoring is unaffected.
        let display_canvas = canvas_rect.translate(Vec2::new(
            screen_offset_preview_shift(
                self.screen_offset_sim_px,
                canvas_rect.width(),
                canvas_size[0],
            ),
            0.0,
        ));
        let ui_preview_frame = ui.input(|input| ((input.time * 60.0) as u64 & 0xffff) as u16);
        if ui_scene_has_animated_image_effect(&preview_scene, &hidden_ui_nodes) {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(16));
        }
        let scene_painter = ui.painter_at(canvas_rect);
        draw_ui_scene_preview(
            &scene_painter,
            &preview_scene,
            UiScenePreviewContext {
                project: &self.project,
                texture_thumbs: &self.texture_thumbs,
                font_textures: &ui_fonts,
                canvas: display_canvas,
                canvas_size,
                hidden_ui_nodes: &hidden_ui_nodes,
                selected: self.selection.selected_ui_node,
                hovered_handle: hovered_resize_target.and_then(|(node, handle)| {
                    (node == self.selection.selected_ui_node).then_some(handle)
                }),
                frame: ui_preview_frame,
            },
        );
        draw_ui_center_snap_guides(
            &scene_painter,
            canvas_rect,
            self.interaction.ui_canvas_drag(),
        );
        if self.screen_offset_sim_px != 0 {
            painter.text(
                canvas_rect.left_top() + Vec2::new(4.0, 4.0),
                egui::Align2::LEFT_TOP,
                format!("screen offset {:+} px", self.screen_offset_sim_px),
                egui::FontId::proportional(11.0),
                Color32::from_rgb(180, 190, 210),
            );
        }

        if self.ui_nav_preview {
            // Drive in-editor focus through the SAME resolver the runtime uses,
            // so navigation authored here matches the console exactly.
            let mut focus_ids: Vec<UiNodeId> = Vec::new();
            let mut rects: Vec<psx_level::NavRect> = Vec::new();
            for id in preview_scene.hierarchy_node_ids() {
                if ui_node_hidden(&preview_scene, &hidden_ui_nodes, id) {
                    continue;
                }
                let Some(node) = preview_scene.node(id) else {
                    continue;
                };
                if !matches!(
                    node.kind,
                    UiNodeKind::Button { .. } | UiNodeKind::Slider { .. }
                ) {
                    continue;
                }
                if let Some(r) = preview_scene.absolute_rect(id) {
                    focus_ids.push(id);
                    rects.push(psx_level::NavRect {
                        x: r.x,
                        y: r.y,
                        w: r.width,
                        h: r.height,
                    });
                }
            }
            if focus_ids.is_empty() {
                self.ui_nav_focus = None;
            } else {
                let mut cur = self
                    .ui_nav_focus
                    .and_then(|f| focus_ids.iter().position(|&id| id == f))
                    .or_else(|| psx_level::first_focus(&rects))
                    .unwrap_or(0);
                let typing = ui.ctx().wants_keyboard_input();
                let dir = if typing {
                    None
                } else {
                    ui.input(|i| {
                        if i.key_pressed(egui::Key::ArrowUp) {
                            Some(psx_level::NavDir::Up)
                        } else if i.key_pressed(egui::Key::ArrowDown) {
                            Some(psx_level::NavDir::Down)
                        } else if i.key_pressed(egui::Key::ArrowLeft) {
                            Some(psx_level::NavDir::Left)
                        } else if i.key_pressed(egui::Key::ArrowRight) {
                            Some(psx_level::NavDir::Right)
                        } else {
                            None
                        }
                    })
                };
                if let Some(dir) = dir {
                    if let Some(next) = psx_level::next_focus(&rects, cur, dir) {
                        cur = next;
                    }
                }
                let focus_id = focus_ids[cur];
                self.ui_nav_focus = Some(focus_id);
                let activate = !typing && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let goto = activate
                    .then(|| preview_scene.node(focus_id))
                    .flatten()
                    .and_then(|node| match &node.kind {
                        UiNodeKind::Button {
                            action: UiAction::GotoScene(target),
                            ..
                        } => Some(*target),
                        _ => None,
                    });
                if let Some(target) = goto {
                    if let Some(idx) = self.project.ui_scenes.iter().position(|s| s.id == target) {
                        self.switch_ui_scene(idx);
                    }
                }
                if let Some(preview) =
                    ui_scene_preview_node(&preview_scene, focus_id, display_canvas, canvas_size)
                {
                    draw_ui_preview_quad_stroke(
                        &painter,
                        preview.quad,
                        Stroke::new(2.0, Color32::from_rgb(248, 224, 96)),
                    );
                }
            }
        } else {
            self.ui_nav_focus = None;
        }

        let label = format!("{}  {}x{}", preview_scene.name, canvas_w, canvas_h);
        painter.text(
            canvas_rect.left_top() + egui::vec2(8.0, 8.0),
            Align2::LEFT_TOP,
            label,
            FontId::monospace(11.0),
            STUDIO_TEXT_WEAK,
        );
    }

    pub(crate) fn select_ui_node(&mut self, id: UiNodeId) {
        self.selection.selected_ui_node = id;
        self.clear_resource_selection_state();
        self.clear_primitive_selection_state();
        self.clear_sector_selection();
    }

    pub(crate) fn begin_ui_canvas_drag(
        &mut self,
        node: UiNodeId,
        mode: UiCanvasDragMode,
        canvas: Rect,
        canvas_size: [u16; 2],
        pointer: Pos2,
    ) {
        let Some(start_pointer_canvas) = ui_screen_to_canvas(pointer, canvas, canvas_size) else {
            return;
        };
        let Some(start_rect) = self
            .current_ui_scene()
            .and_then(|scene| scene.node(node))
            .and_then(|node| node.kind.rect())
        else {
            self.select_ui_node(node);
            return;
        };
        let start_absolute_rect = self
            .current_ui_scene()
            .and_then(|scene| scene.absolute_rect(node))
            .unwrap_or(start_rect);
        self.select_ui_node(node);
        self.interaction = Interaction::UiCanvas(UiCanvasDrag {
            node,
            mode,
            start_pointer_canvas,
            start_rect,
            start_absolute_rect,
            snapshot_pushed: false,
            snap_center_x: false,
            snap_center_y: false,
        });
        self.status = match mode {
            UiCanvasDragMode::Move => "Move UI node".to_string(),
            UiCanvasDragMode::Rotate => "Rotate UI node".to_string(),
            UiCanvasDragMode::Resize(handle) => format!("Resize UI node from {}", handle.label()),
        };
    }

    pub(crate) fn update_ui_canvas_drag(
        &mut self,
        canvas: Rect,
        canvas_size: [u16; 2],
        pointer: Pos2,
    ) {
        let Some(drag) = self.interaction.ui_canvas_drag().cloned() else {
            return;
        };
        let Some(pointer_canvas) = ui_screen_to_canvas(pointer, canvas, canvas_size) else {
            return;
        };
        let delta = [
            (pointer_canvas[0] - drag.start_pointer_canvas[0]).round() as i32,
            (pointer_canvas[1] - drag.start_pointer_canvas[1]).round() as i32,
        ];
        let mut snap_center_x = false;
        let mut snap_center_y = false;
        let next_rect = match drag.mode {
            UiCanvasDragMode::Move => {
                let moved = move_ui_rect(drag.start_rect, delta);
                if self.ui_center_snap {
                    let moved_absolute = move_ui_rect(drag.start_absolute_rect, delta);
                    let snapped =
                        snap_moved_ui_rect_to_canvas_center(moved, moved_absolute, canvas_size);
                    snap_center_x = snapped.snap_x;
                    snap_center_y = snapped.snap_y;
                    snapped.rect
                } else {
                    moved
                }
            }
            UiCanvasDragMode::Resize(handle) => resize_ui_rect(drag.start_rect, handle, delta),
            UiCanvasDragMode::Rotate => {
                let center_x =
                    drag.start_absolute_rect.x as f32 + drag.start_absolute_rect.width as f32 * 0.5;
                let center_y = drag.start_absolute_rect.y as f32
                    + drag.start_absolute_rect.height as f32 * 0.5;
                let start_angle = (drag.start_pointer_canvas[1] - center_y)
                    .atan2(drag.start_pointer_canvas[0] - center_x);
                let current_angle =
                    (pointer_canvas[1] - center_y).atan2(pointer_canvas[0] - center_x);
                let delta_degrees = (current_angle - start_angle).to_degrees().round() as i32;
                let mut next = drag.start_rect;
                next.rotation_degrees =
                    normalize_ui_rotation_degrees(i32::from(next.rotation_degrees) + delta_degrees);
                next
            }
        };
        if let Some(active) = self.interaction.ui_canvas_drag_mut() {
            active.snap_center_x = snap_center_x;
            active.snap_center_y = snap_center_y;
        }

        let current_rect = self
            .current_ui_scene()
            .and_then(|scene| scene.node(drag.node))
            .and_then(|node| node.kind.rect());
        if current_rect == Some(next_rect) {
            return;
        }
        if !drag.snapshot_pushed {
            self.push_undo();
            if let Some(active) = self.interaction.ui_canvas_drag_mut() {
                active.snapshot_pushed = true;
            }
        }
        if let Some(rect) = self
            .current_ui_scene_mut()
            .and_then(|scene| scene.node_mut(drag.node))
            .and_then(|node| node.kind.rect_mut())
        {
            *rect = next_rect;
            self.mark_dirty();
            self.status = format!(
                "UI node {} at {},{} {}x{}",
                drag.node.raw(),
                next_rect.x,
                next_rect.y,
                next_rect.width,
                next_rect.height
            );
        }
    }

    pub(crate) fn nudge_selected_ui_node(&mut self, dx: i32, dy: i32) -> bool {
        if dx == 0 && dy == 0 {
            return false;
        }
        let selected = self.selection.selected_ui_node;
        let Some(rect) = self
            .current_ui_scene()
            .and_then(|scene| scene.node(selected))
            .and_then(|node| node.kind.rect())
        else {
            return false;
        };
        let next = move_ui_rect(rect, [dx, dy]);
        if next == rect {
            return false;
        }
        self.push_undo();
        if let Some(rect) = self
            .current_ui_scene_mut()
            .and_then(|scene| scene.node_mut(selected))
            .and_then(|node| node.kind.rect_mut())
        {
            *rect = next;
            self.mark_dirty();
            self.status = format!("Nudged UI node #{}", selected.raw());
            return true;
        }
        false
    }

    pub(crate) fn delete_selected_ui_node(&mut self) -> bool {
        let selected = self.selection.selected_ui_node;
        let Some((root, parent, name)) = self.current_ui_scene().and_then(|scene| {
            let node = scene.node(selected)?;
            Some((
                scene.root,
                node.parent.unwrap_or(scene.root),
                node.name.clone(),
            ))
        }) else {
            self.status = "No UI node selected".to_string();
            return false;
        };
        if selected == root {
            self.status = "Canvas cannot be deleted".to_string();
            return false;
        }

        self.push_undo();
        let removed = self
            .current_ui_scene_mut()
            .is_some_and(|scene| scene.remove_node(selected));
        if !removed {
            self.status = "UI node no longer exists".to_string();
            return false;
        }
        self.selection.selected_ui_node = parent;
        self.interaction.take_ui_canvas_drag();
        self.retain_hidden_ui_nodes_for_project();
        self.mark_dirty();
        self.status = format!("Deleted UI {name}");
        true
    }

    pub(crate) fn copy_selected_ui_node(&mut self) -> bool {
        self.copy_ui_node(self.selection.selected_ui_node)
    }

    pub(crate) fn copy_ui_node(&mut self, id: UiNodeId) -> bool {
        let Some(scene) = self.current_ui_scene() else {
            self.status = "No UI scene available".to_string();
            return false;
        };
        if id == scene.root {
            self.status = "Canvas cannot be copied".to_string();
            return false;
        }
        let Some(node) = scene.node(id) else {
            self.status = "UI node no longer exists".to_string();
            return false;
        };
        let Some(nodes) = scene.subtree_nodes(id) else {
            self.status = "UI node cannot be copied".to_string();
            return false;
        };
        let root_name = node.name.clone();
        let count = nodes.len();
        self.ui_node_clipboard = Some(UiNodeClipboard {
            root: id,
            root_name: root_name.clone(),
            nodes,
        });
        self.status = if count == 1 {
            format!("Copied UI {root_name}")
        } else {
            format!("Copied UI {root_name} ({count} nodes)")
        };
        true
    }

    pub(crate) fn paste_ui_node(&mut self) -> bool {
        let Some(scene) = self.current_ui_scene() else {
            self.status = "No UI scene available".to_string();
            return false;
        };
        let parent = if scene.node(self.selection.selected_ui_node).is_some() {
            self.selection.selected_ui_node
        } else {
            scene.root
        };
        self.paste_ui_node_under(parent)
    }

    pub(crate) fn paste_ui_node_under(&mut self, parent: UiNodeId) -> bool {
        let Some(clipboard) = self.ui_node_clipboard.clone() else {
            self.status = "No copied UI node".to_string();
            return false;
        };
        let Some(parent) = self.current_ui_scene().map(|scene| {
            if scene.node(parent).is_some() {
                parent
            } else {
                scene.root
            }
        }) else {
            self.status = "No UI scene available".to_string();
            return false;
        };

        self.push_undo();
        let Some(pasted) = self
            .current_ui_scene_mut()
            .and_then(|scene| scene.paste_subtree(parent, &clipboard.nodes, clipboard.root))
        else {
            self.status = "Paste UI node failed".to_string();
            return false;
        };
        self.selection.selected_ui_node = pasted;
        self.clear_resource_selection_state();
        self.clear_primitive_selection_state();
        self.clear_sector_selection();
        self.interaction.take_ui_canvas_drag();
        self.mark_dirty();
        self.status = if clipboard.nodes.len() == 1 {
            format!("Pasted UI {}", clipboard.root_name)
        } else {
            format!(
                "Pasted UI {} ({} nodes)",
                clipboard.root_name,
                clipboard.nodes.len()
            )
        };
        true
    }
}
