mod resources;
use super::*;
use psxed_project::UiFocusEffect;

impl EditorWorkspace {
    pub(crate) fn draw_build_status_strip(
        &self,
        ui: &mut egui::Ui,
        playtest_status: EditorPlaytestStatus,
    ) {
        let status = self.action_bar_status(playtest_status);
        let strip_width = ui.available_width().max(1.0);
        let compact = strip_width < 180.0;
        let strip_height = ui.available_height().max(26.0);
        let frame = egui::Frame::new()
            .fill(STUDIO_PANEL_HEADER)
            .corner_radius(egui::CornerRadius::same(6))
            .inner_margin(egui::Margin::symmetric(9, 5))
            .show(ui, |ui| {
                ui.set_min_width((strip_width - 18.0).max(80.0));
                ui.set_min_height((strip_height - 10.0).max(26.0));
                ui.horizontal_top(|ui| {
                    ui.label(icons::text(status.icon, 14.0).color(status.accent));
                    ui.label(
                        RichText::new(status.badge)
                            .small()
                            .strong()
                            .color(status.accent),
                    );
                    if !compact {
                        ui.separator();
                        ui.add(
                            egui::Label::new(
                                RichText::new(status.message)
                                    .small()
                                    .color(STUDIO_TEXT_WEAK),
                            )
                            .wrap(),
                        );
                    }
                });
            });
        let rail = Rect::from_min_max(
            frame.response.rect.left_top() + Vec2::new(1.0, 6.0),
            frame.response.rect.left_bottom() + Vec2::new(4.0, -6.0),
        );
        ui.painter()
            .rect_filled(rail, egui::CornerRadius::same(2), status.border);
    }

    pub(crate) fn action_bar_status(
        &self,
        playtest_status: EditorPlaytestStatus,
    ) -> ActionBarStatus<'_> {
        match playtest_status {
            EditorPlaytestStatus::Cooking => ActionBarStatus {
                icon: icons::BOX,
                badge: "COOKING",
                message: &self.status,
                accent: STUDIO_ACCENT,
                border: STUDIO_ACCENT_DIM,
            },
            EditorPlaytestStatus::Building => ActionBarStatus {
                icon: icons::TERMINAL,
                badge: "BUILDING",
                message: &self.status,
                accent: STUDIO_ACCENT,
                border: STUDIO_ACCENT_DIM,
            },
            EditorPlaytestStatus::Running { input_captured } => ActionBarStatus {
                icon: icons::PLAY,
                badge: if input_captured { "PLAY INPUT" } else { "PLAY" },
                message: &self.status,
                accent: STUDIO_SUCCESS,
                border: STUDIO_SUCCESS_DIM,
            },
            EditorPlaytestStatus::Failed => ActionBarStatus {
                icon: icons::TERMINAL,
                badge: "FAILED",
                message: &self.status,
                accent: STUDIO_ERROR,
                border: STUDIO_ERROR_DIM,
            },
            EditorPlaytestStatus::Idle if self.dirty => ActionBarStatus {
                icon: icons::SAVE,
                badge: "UNSAVED",
                message: &self.status,
                accent: STUDIO_ACCENT,
                border: STUDIO_ACCENT_DIM,
            },
            EditorPlaytestStatus::Idle => ActionBarStatus {
                icon: icons::CIRCLE_DOT,
                badge: "READY",
                message: &self.status,
                accent: STUDIO_TEXT_WEAK,
                border: STUDIO_BORDER,
            },
        }
    }

    pub(crate) fn draw_left_dock(&mut self, ctx: &egui::Context) {
        if !self.left_dock_open {
            return;
        }
        let max_width = max_resizable_side_dock_width(ctx, self.inspector_open);
        egui::SidePanel::left("psxed_left_dock")
            .resizable(true)
            .default_width(280.0)
            .min_width(RESIZABLE_DOCK_MIN_WIDTH)
            .max_width(max_width)
            .frame(dock_frame())
            .show(ctx, |ui| {
                fixed_panel_content(ui, "psxed_left_dock_fixed_content", |ui| {
                    let content_width = ui.available_width().max(1.0);
                    constrain_resizable_dock_content(ui, content_width);
                    self.draw_scene_filesystem_split(ui);
                });
            });
    }

    pub(crate) fn draw_scene_filesystem_split(&mut self, ui: &mut egui::Ui) {
        let content_width = ui.available_width().max(1.0);
        let total_height = ui.available_height().max(1.0);
        let splitter_height = LEFT_DOCK_SPLITTER_HEIGHT.min(total_height);
        let panel_height = (total_height - splitter_height).max(1.0);
        let min_panel = LEFT_DOCK_MIN_SPLIT_PANEL_HEIGHT.min(panel_height * 0.5);
        let min_fraction = (min_panel / panel_height).clamp(0.0, 0.5);
        self.left_dock_scene_fraction = self
            .left_dock_scene_fraction
            .clamp(min_fraction, 1.0 - min_fraction);

        let scene_height = (panel_height * self.left_dock_scene_fraction)
            .clamp(min_panel, panel_height - min_panel);
        let filesystem_height = (panel_height - scene_height).max(0.0);

        ui.allocate_ui_with_layout(
            Vec2::new(content_width, scene_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_min_size(Vec2::new(content_width, scene_height));
                ui.set_max_width(content_width);
                if self.active_workspace == WorkspaceView::Ui {
                    self.draw_ui_tree_panel(ui);
                } else {
                    self.draw_scene_tree_panel(ui);
                }
            },
        );
        self.draw_scene_filesystem_splitter(ui, content_width, panel_height);
        ui.allocate_ui_with_layout(
            Vec2::new(content_width, filesystem_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_min_size(Vec2::new(content_width, filesystem_height));
                ui.set_max_width(content_width);
                self.draw_filesystem_panel(ui);
            },
        );
    }

    pub(crate) fn draw_scene_filesystem_splitter(
        &mut self,
        ui: &mut egui::Ui,
        width: f32,
        panel_height: f32,
    ) {
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(width, LEFT_DOCK_SPLITTER_HEIGHT), Sense::drag());
        let response = response.on_hover_cursor(egui::CursorIcon::ResizeVertical);
        if response.dragged() && panel_height > 1.0 {
            let delta = ui.input(|input| input.pointer.delta().y);
            let min_fraction = (LEFT_DOCK_MIN_SPLIT_PANEL_HEIGHT.min(panel_height * 0.5)
                / panel_height)
                .clamp(0.0, 0.5);
            self.left_dock_scene_fraction = (self.left_dock_scene_fraction + delta / panel_height)
                .clamp(min_fraction, 1.0 - min_fraction);
        }

        let color = if response.dragged() || response.hovered() {
            STUDIO_ACCENT
        } else {
            STUDIO_BORDER
        };
        let y = rect.center().y;
        ui.painter().line_segment(
            [
                Pos2::new(rect.left() + 10.0, y),
                Pos2::new(rect.right() - 10.0, y),
            ],
            Stroke::new(1.0, color),
        );
    }

    pub(crate) fn draw_scene_tree_panel(&mut self, ui: &mut egui::Ui) {
        tool_panel_frame().show(ui, |ui| {
            ui.set_min_height(ui.available_height());
            tool_panel_header(ui, icons::LAYERS, "Scene Graph", |ui| {
                ui.menu_button(icons::text(icons::PLUS, 14.0), |ui| {
                    for (label, kind) in scene_graph_addable_kinds() {
                        if ui.button(label).clicked() {
                            self.add_child(kind, label);
                            ui.close_menu();
                        }
                    }
                })
                .response
                .on_hover_text(
                    "Add structural scene nodes. Place runtime objects from the toolbar Add menu.",
                );
            });
            tool_panel_body(ui, |ui| self.draw_scene_tree_panel_body(ui));
        });
    }

    pub(crate) fn draw_scene_tree_panel_body(&mut self, ui: &mut egui::Ui) {
        ui.add(
            egui::TextEdit::singleline(&mut self.scene_filter)
                .hint_text("Filter scene graph")
                .desired_width(f32::INFINITY),
        );
        ui.separator();

        let rows = self.project.active_scene().hierarchy_rows();
        let filter = self.scene_filter.to_ascii_lowercase();
        let display_rows = scene_tree_display_rows(&rows, &filter, &self.collapsed_scene_nodes);
        let visible_node_order: Vec<NodeId> = display_rows.iter().map(|row| row.id).collect();
        let mut actions: Vec<TreeAction> = Vec::new();
        let mut connection_select: Option<NodeId> = None;
        let mut connection_repair: Option<NodeId> = None;
        let selected_node = self.selection.selected_node;
        let selected_nodes = self.selection.selected_nodes.clone();
        let collapsed_scene_nodes = self.collapsed_scene_nodes.clone();
        let hidden_scene_nodes = self.hidden_scene_nodes.clone();
        let scene = self.project.active_scene();
        let connections = derive_room_connections(scene);
        let renaming = &mut self.renaming;
        let pending_focus = &mut self.pending_rename_focus;
        let tree_scroll_height = (ui.available_height() - 30.0).max(24.0);
        egui::ScrollArea::vertical()
            .id_salt("psxed_scene_tree")
            .auto_shrink([false, false])
            .max_height(tree_scroll_height)
            .show(ui, |ui| {
                for row in display_rows {
                    draw_scene_node_row(
                        ui,
                        row,
                        selected_nodes.contains(&row.id)
                            || (selected_nodes.is_empty() && selected_node == row.id),
                        collapsed_scene_nodes.contains(&row.id),
                        hidden_scene_nodes.contains(&row.id),
                        scene_node_hidden(scene, &hidden_scene_nodes, row.id),
                        renaming,
                        pending_focus,
                        &mut actions,
                    );
                }
                ui.add_space(4.0);
                draw_room_connections_rows(
                    ui,
                    scene,
                    &connections,
                    &filter,
                    selected_node,
                    &selected_nodes,
                    &mut connection_select,
                    &mut connection_repair,
                );
                draw_room_floor_link_rows(ui, scene, &filter);
                autoscroll_tree_drag::<NodeId>(ui);
            });

        for action in actions {
            self.apply_tree_action(action, &visible_node_order);
        }
        if let Some(portal) = connection_select {
            self.replace_node_selection(portal);
            self.clear_resource_selection_state();
            self.clear_primitive_selection_state();
            self.clear_sector_selection();
        }
        if let Some(portal) = connection_repair {
            self.create_reciprocal_portal(portal);
        }

        ui.horizontal(|ui| {
            if ui.button(icons::label(icons::COPY, "Duplicate")).clicked() {
                self.duplicate_selected();
            }
            let can_delete = self.selection.selected_node != NodeId::ROOT;
            if ui
                .add_enabled(
                    can_delete,
                    egui::Button::new(icons::label(icons::TRASH, "Delete")),
                )
                .clicked()
            {
                self.delete_selected();
            }
        });
    }

    pub(crate) fn draw_ui_tree_panel(&mut self, ui: &mut egui::Ui) {
        tool_panel_frame().show(ui, |ui| {
            ui.set_min_height(ui.available_height());
            tool_panel_header(ui, icons::SQUARE, "UI", |ui| {
                ui.menu_button(icons::text(icons::PLUS, 14.0), |ui| {
                    for (label, kind) in default_addable_ui_kinds() {
                        if ui.button(label).clicked() {
                            self.add_ui_child(kind, label);
                            ui.close_menu();
                        }
                    }
                })
                .response
                .on_hover_text("Add UI node to the selected UI node");
            });
            tool_panel_body(ui, |ui| self.draw_ui_tree_panel_body(ui));
        });
    }

    pub(crate) fn draw_ui_tree_panel_body(&mut self, ui: &mut egui::Ui) {
        self.draw_scene_state_arranger(ui);
        ui.separator();
        self.draw_ui_scene_strip(ui);
        ui.separator();
        let Some(scene) = self.current_ui_scene() else {
            ui.weak("No UI scene");
            return;
        };
        let rows = scene.hierarchy_rows();
        let selected = self.selection.selected_ui_node;
        let hidden_ui_nodes = self.hidden_ui_nodes.clone();
        let mut actions = Vec::new();
        let tree_scroll_height = (ui.available_height() - 30.0).max(24.0);
        egui::ScrollArea::vertical()
            .id_salt("psxed_ui_tree")
            .auto_shrink([false, false])
            .max_height(tree_scroll_height)
            .show(ui, |ui| {
                for row in rows {
                    draw_ui_node_row(
                        ui,
                        &row,
                        scene.id,
                        selected == row.id,
                        hidden_ui_nodes.contains(&(scene.id, row.id)),
                        ui_node_hidden(scene, &hidden_ui_nodes, row.id),
                        self.ui_node_clipboard.is_some(),
                        &mut actions,
                    );
                }
                autoscroll_tree_drag::<UiNodeId>(ui);
            });
        for action in actions {
            self.apply_ui_tree_action(action);
        }
        ui.horizontal(|ui| {
            if ui
                .add(egui::Button::new(icons::text(icons::FOCUS, 14.0)).min_size(Vec2::splat(28.0)))
                .on_hover_text("Select canvas root")
                .clicked()
            {
                if let Some(scene) = self.current_ui_scene() {
                    self.selection.selected_ui_node = scene.root;
                }
            }
            let can_copy = self.current_ui_scene().is_some_and(|scene| {
                self.selection.selected_ui_node != scene.root
                    && scene.node(self.selection.selected_ui_node).is_some()
            });
            if ui
                .add_enabled(
                    can_copy,
                    egui::Button::new(icons::text(icons::COPY, 14.0)).min_size(Vec2::splat(28.0)),
                )
                .on_hover_text("Copy selected UI node")
                .clicked()
            {
                self.copy_selected_ui_node();
            }
            let can_paste = self.ui_node_clipboard.is_some() && self.current_ui_scene().is_some();
            if ui
                .add_enabled(
                    can_paste,
                    egui::Button::new(icons::text(icons::FILE_PLUS, 14.0))
                        .min_size(Vec2::splat(28.0)),
                )
                .on_hover_text("Paste as child of selected UI node")
                .clicked()
            {
                self.paste_ui_node();
            }
            let can_delete = self
                .current_ui_scene()
                .is_some_and(|scene| self.selection.selected_ui_node != scene.root);
            if ui
                .add_enabled(
                    can_delete,
                    egui::Button::new(icons::text(icons::TRASH, 14.0)).min_size(Vec2::splat(28.0)),
                )
                .on_hover_text("Delete selected UI node")
                .clicked()
            {
                self.delete_selected_ui_node();
            }
        });
        ui.separator();
        self.draw_ui_options_editor(ui);
    }

    /// Project-level options editor shown under the UI tree. Lists the
    /// project's [`OptionDef`]s with an inline name + kind editor and
    /// Add / Remove controls so [`UiNodeKind::Slider`] and
    /// `SetOption` button actions have bindable options. Kept compact:
    /// one collapsing header, rename in place, per-row kind controls.
    pub(crate) fn draw_ui_options_editor(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        let mut remove_index: Option<usize> = None;
        egui::CollapsingHeader::new(format!("Options ({})", self.project.options.len()))
            .id_salt("psxed_ui_options")
            .default_open(false)
            .show(ui, |ui| {
                for (index, option) in self.project.options.iter_mut().enumerate() {
                    ui.push_id(option.id.raw(), |ui| {
                        ui.horizontal(|ui| {
                            ui.label(icons::text(icons::WAYPOINT, 12.0).color(STUDIO_TEXT_WEAK));
                            changed |= ui.text_edit_singleline(&mut option.name).changed();
                            if ui
                                .button(icons::text(icons::TRASH, 12.0))
                                .on_hover_text("Remove option")
                                .clicked()
                            {
                                remove_index = Some(index);
                            }
                        });
                        changed |= draw_option_kind_editor(ui, &mut option.kind);
                    });
                    ui.separator();
                }
                if ui.button(icons::label(icons::PLUS, "Add Option")).clicked() {
                    self.project.add_option("Option");
                    changed = true;
                }
            });
        if let Some(index) = remove_index {
            self.project.remove_option(index);
            changed = true;
        }
        if changed {
            self.mark_dirty();
        }
    }

    pub(crate) fn draw_scene_state_arranger(&mut self, ui: &mut egui::Ui) {
        let state_count = self.project.scene_states.len();
        let mut switch_to: Option<usize> = None;
        let mut add_state = false;
        let mut delete_state = false;

        ui.horizontal(|ui| {
            ui.label(icons::text(icons::LAYERS, 14.0).color(STUDIO_ACCENT));
            ui.label(
                RichText::new(format!("Screen States ({state_count})"))
                    .color(STUDIO_TEXT_WEAK)
                    .small(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let action_btn = egui::Vec2::new(26.0, 22.0);
                if ui
                    .add(
                        egui::Button::new(icons::text(icons::FILE_PLUS, 14.0)).min_size(action_btn),
                    )
                    .on_hover_text("Create a new screen state")
                    .clicked()
                {
                    add_state = true;
                }
                if ui
                    .add_enabled(
                        state_count > 1,
                        egui::Button::new(icons::text(icons::TRASH, 14.0)).min_size(action_btn),
                    )
                    .on_hover_text("Delete selected screen state")
                    .clicked()
                {
                    delete_state = true;
                }
            });
        });

        let active = self.current_scene_state_index();
        let list_height = (ui.available_height() * 0.22).clamp(42.0, 96.0);
        egui::ScrollArea::vertical()
            .id_salt("psxed_scene_state_arranger")
            .auto_shrink([false, false])
            .max_height(list_height)
            .show(ui, |ui| {
                for (index, state) in self.project.scene_states.iter().enumerate() {
                    let layer = match (state.world, state.ui_scene) {
                        (SceneWorldLayer::Gameplay, Some(_)) => "3D+2D",
                        (SceneWorldLayer::Gameplay, None) => "3D",
                        (SceneWorldLayer::None, Some(_)) => "2D",
                        (SceneWorldLayer::None, None) => "Empty",
                    };
                    let selected = index == active;
                    let label = format!("{}  {}", compact_middle(&state.name, 22), layer);
                    if ui
                        .selectable_label(selected, icons::label(icons::LAYERS, &label))
                        .clicked()
                    {
                        switch_to = Some(index);
                    }
                }
            });

        if add_state {
            self.push_undo();
            let name = self.unique_scene_state_name("Screen State");
            self.project.add_scene_state(name);
            self.active_scene_state_index = self.project.scene_states.len().saturating_sub(1);
            if let Some(ui_scene) = self
                .project
                .scene_state_at(self.active_scene_state_index)
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
            self.status = "Added screen state".to_string();
            self.mark_dirty();
        }
        if delete_state {
            self.push_undo();
            let removed = self.project.remove_scene_state(active);
            if removed {
                let count = self.project.scene_states.len();
                self.active_scene_state_index =
                    self.active_scene_state_index.min(count.saturating_sub(1));
                self.status = "Deleted screen state".to_string();
                self.mark_dirty();
            }
        }
        if let Some(index) = switch_to {
            self.switch_scene_state(index);
        }

        let active = self.current_scene_state_index();
        let ui_scene_options: Vec<(UiSceneId, String)> = self
            .project
            .ui_scenes
            .iter()
            .map(|scene| (scene.id, scene.name.clone()))
            .collect();
        let boot_state = self
            .project
            .scene_state_at(active)
            .map(|state| matches!(self.project.boot, BootTarget::SceneState(id) if id == state.id))
            .unwrap_or(false);
        let mut set_boot: Option<SceneStateId> = None;
        let mut overlay_to_switch: Option<UiSceneId> = None;
        let mut changed = false;

        if let Some(state) = self.project.scene_state_at_mut(active) {
            ui.horizontal(|ui| {
                ui.label("Name");
                changed |= ui.text_edit_singleline(&mut state.name).changed();
            });
            ui.horizontal(|ui| {
                ui.label("World");
                egui::ComboBox::from_id_salt(ui.id().with("scene_state_world"))
                    .selected_text(state.world.label())
                    .show_ui(ui, |ui| {
                        for world in [SceneWorldLayer::None, SceneWorldLayer::Gameplay] {
                            if ui
                                .selectable_label(state.world == world, world.label())
                                .clicked()
                                && state.world != world
                            {
                                state.world = world;
                                changed = true;
                            }
                        }
                    });
            });
            ui.horizontal(|ui| {
                ui.label("2D Overlay");
                let preview = state
                    .ui_scene
                    .and_then(|id| {
                        ui_scene_options
                            .iter()
                            .find(|(scene_id, _)| *scene_id == id)
                            .map(|(_, name)| name.as_str())
                    })
                    .unwrap_or("None");
                let previous = state.ui_scene;
                if searchable_picker(
                    ui,
                    ui.id().with("scene_state_ui_overlay"),
                    &mut state.ui_scene,
                    preview,
                    &ui_scene_options,
                    SearchablePickerConfig::optional("None").with_search_hint("Search UI scenes…"),
                ) {
                    overlay_to_switch = state.ui_scene;
                    changed |= state.ui_scene != previous;
                }
            });
            changed |= ui
                .checkbox(&mut state.ui_input, "UI captures input")
                .changed();
            changed |= ui.checkbox(&mut state.pause_world, "Pause world").changed();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!boot_state, egui::Button::new("Boot Here"))
                    .clicked()
                {
                    set_boot = Some(state.id);
                }
                if boot_state {
                    ui.label(RichText::new("Boot").color(STUDIO_ACCENT).small());
                }
            });
        }

        if let Some(scene_id) = overlay_to_switch {
            if let Some(index) = self
                .project
                .ui_scenes
                .iter()
                .position(|scene| scene.id == scene_id)
            {
                self.switch_ui_scene(index);
            }
        }
        if let Some(state_id) = set_boot {
            self.project.boot = BootTarget::SceneState(state_id);
            self.status = "Boot target set to screen state".to_string();
            changed = true;
        }
        if changed {
            self.mark_dirty();
        }
    }

    /// Multi-scene browser shown at the top of the UI tree panel. Lists
    /// the project's UI scenes by name, lets the user click to switch the
    /// authored scene, and exposes New / Duplicate / Rename / Delete. The
    /// active row supports inline rename (commit on Enter / blur, cancel
    /// on Escape) and Delete is a two-step confirm, disabled when only one
    /// scene remains.
    pub(crate) fn draw_ui_scene_strip(&mut self, ui: &mut egui::Ui) {
        let active = self.current_ui_scene_index();
        let scene_count = self.project.ui_scenes.len();
        let mut switch_to: Option<usize> = None;
        let mut commit_rename: Option<(usize, String)> = None;
        let mut cancel_rename = false;

        ui.horizontal(|ui| {
            ui.label(icons::text(icons::LAYERS, 14.0).color(STUDIO_ACCENT));
            ui.label(
                RichText::new(format!("Scenes ({scene_count})"))
                    .color(STUDIO_TEXT_WEAK)
                    .small(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Compact icon-only actions: a narrow side panel cannot fit four
                // text-labelled buttons in one row (they collapse to vertical,
                // one-glyph-per-line text), so each action is the icon plus its
                // hover tooltip.
                let action_btn = egui::Vec2::new(26.0, 22.0);
                let can_delete = scene_count > 1;
                let delete_pending = self.ui_scene_delete_confirm == Some(active);
                let trash = if delete_pending {
                    icons::text(icons::TRASH, 14.0).color(egui::Color32::from_rgb(220, 96, 96))
                } else {
                    icons::text(icons::TRASH, 14.0)
                };
                if ui
                    .add_enabled(can_delete, egui::Button::new(trash).min_size(action_btn))
                    .on_hover_text(if delete_pending {
                        "Click again to delete this scene"
                    } else {
                        "Delete this UI scene"
                    })
                    .clicked()
                {
                    if delete_pending {
                        self.delete_ui_scene_action(active);
                    } else {
                        self.ui_scene_delete_confirm = Some(active);
                    }
                }
                if ui
                    .add(egui::Button::new(icons::text(icons::PEN_LINE, 14.0)).min_size(action_btn))
                    .on_hover_text("Rename this UI scene")
                    .clicked()
                {
                    self.begin_ui_scene_rename(active);
                }
                if ui
                    .add(egui::Button::new(icons::text(icons::COPY, 14.0)).min_size(action_btn))
                    .on_hover_text("Duplicate this UI scene")
                    .clicked()
                {
                    self.duplicate_ui_scene_action(active);
                }
                if ui
                    .add(
                        egui::Button::new(icons::text(icons::FILE_PLUS, 14.0)).min_size(action_btn),
                    )
                    .on_hover_text("Create a new UI scene")
                    .clicked()
                {
                    self.add_ui_scene_action();
                }
            });
        });

        let renaming_index = match &self.ui_scene_renaming {
            Some((index, _)) if *index < scene_count => Some(*index),
            _ => None,
        };
        let strip_height = (ui.available_height() * 0.32).clamp(48.0, 132.0);
        egui::ScrollArea::vertical()
            .id_salt("psxed_ui_scene_strip")
            .auto_shrink([false, false])
            .max_height(strip_height)
            .show(ui, |ui| {
                for index in 0..scene_count {
                    if renaming_index == Some(index) {
                        if let Some((_, buffer)) = self.ui_scene_renaming.as_mut() {
                            let response = ui.add(
                                egui::TextEdit::singleline(buffer)
                                    .desired_width(f32::INFINITY)
                                    .margin(egui::Vec2::new(4.0, 2.0)),
                            );
                            if self.ui_scene_rename_focus_pending {
                                response.request_focus();
                                self.ui_scene_rename_focus_pending = false;
                            }
                            let lost_focus = response.lost_focus();
                            let pressed_enter =
                                lost_focus && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            let pressed_esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
                            if pressed_esc {
                                cancel_rename = true;
                            } else if pressed_enter || lost_focus {
                                commit_rename = Some((index, buffer.clone()));
                            }
                        }
                        continue;
                    }
                    let name = self
                        .project
                        .ui_scene_at(index)
                        .map(|scene| scene.name.clone())
                        .unwrap_or_else(|| format!("Scene {index}"));
                    let selected = index == active;
                    let response = ui.selectable_label(
                        selected,
                        icons::label(icons::SQUARE, &compact_middle(&name, 28)),
                    );
                    let response = if name.chars().count() > 28 {
                        response.on_hover_text(name.clone())
                    } else {
                        response
                    };
                    if response.clicked() {
                        switch_to = Some(index);
                    }
                    if response.double_clicked() {
                        switch_to = Some(index);
                        self.begin_ui_scene_rename(index);
                    }
                }
            });

        if let Some((index, name)) = commit_rename {
            self.commit_ui_scene_rename(index, name);
        } else if cancel_rename {
            self.ui_scene_renaming = None;
            self.ui_scene_rename_focus_pending = false;
        }
        if let Some(index) = switch_to {
            self.switch_ui_scene(index);
        }
    }

    /// Create a new empty UI scene, make it active, and select its root.
    pub(crate) fn add_ui_scene_action(&mut self) {
        self.push_undo();
        let name = self.unique_ui_scene_name("UI Scene");
        self.project.add_ui_scene(name);
        let index = self.project.ui_scenes.len().saturating_sub(1);
        self.active_ui_scene_index = index;
        self.ui_scene_renaming = None;
        self.ui_scene_delete_confirm = None;
        self.reset_ui_node_selection();
        self.status = self
            .current_ui_scene()
            .map(|scene| format!("Added UI scene {}", scene.name))
            .unwrap_or_else(|| "Added UI scene".to_string());
        self.mark_dirty();
    }

    /// Duplicate the UI scene at `index`, select the copy, and reset the
    /// node selection so no node id leaks across from the source.
    pub(crate) fn duplicate_ui_scene_action(&mut self, index: usize) {
        self.push_undo();
        let Some(_) = self.project.duplicate_ui_scene(index) else {
            self.status = "No UI scene to duplicate".to_string();
            return;
        };
        self.active_ui_scene_index = index + 1;
        self.ui_scene_renaming = None;
        self.ui_scene_delete_confirm = None;
        self.reset_ui_node_selection();
        self.status = self
            .current_ui_scene()
            .map(|scene| format!("Duplicated UI scene as {}", scene.name))
            .unwrap_or_else(|| "Duplicated UI scene".to_string());
        self.mark_dirty();
    }

    /// Delete the UI scene at `index`. The project layer keeps the list
    /// non-empty; this clamps the active index and resets the node
    /// selection against whatever scene becomes active.
    pub(crate) fn delete_ui_scene_action(&mut self, index: usize) {
        self.ui_scene_delete_confirm = None;
        self.ui_scene_renaming = None;
        if self.project.ui_scenes.len() <= 1 {
            self.status = "Cannot delete the only UI scene".to_string();
            return;
        }
        let removed_name = self
            .project
            .ui_scene_at(index)
            .map(|scene| scene.name.clone());
        self.push_undo();
        if !self.project.remove_ui_scene(index) {
            self.status = "No UI scene to delete".to_string();
            return;
        }
        let count = self.project.ui_scenes.len();
        if self.active_ui_scene_index >= count {
            self.active_ui_scene_index = count.saturating_sub(1);
        } else if index < self.active_ui_scene_index {
            self.active_ui_scene_index -= 1;
        }
        self.reset_ui_node_selection();
        self.retain_hidden_ui_nodes_for_project();
        self.status = match removed_name {
            Some(name) => format!("Deleted UI scene {name}"),
            None => "Deleted UI scene".to_string(),
        };
        self.mark_dirty();
    }

    /// Enter inline-rename mode for the UI scene at `index`.
    pub(crate) fn begin_ui_scene_rename(&mut self, index: usize) {
        let Some(scene) = self.project.ui_scene_at(index) else {
            return;
        };
        self.ui_scene_renaming = Some((index, scene.name.clone()));
        self.ui_scene_rename_focus_pending = true;
        self.ui_scene_delete_confirm = None;
    }

    /// Commit a UI scene rename. Empty names revert to the current name;
    /// a real change is one undo step.
    pub(crate) fn commit_ui_scene_rename(&mut self, index: usize, name: String) {
        self.ui_scene_renaming = None;
        self.ui_scene_rename_focus_pending = false;
        let trimmed = name.trim();
        let Some(current) = self
            .project
            .ui_scene_at(index)
            .map(|scene| scene.name.clone())
        else {
            return;
        };
        if trimmed.is_empty() || trimmed == current {
            return;
        }
        let final_name = trimmed.to_string();
        self.push_undo();
        if let Some(scene) = self.project.ui_scene_at_mut(index) {
            scene.name = final_name.clone();
        }
        self.status = format!("Renamed UI scene {final_name}");
        self.mark_dirty();
    }

    /// Build a UI scene name not already taken, e.g. "UI Scene",
    /// "UI Scene 2". Keeps the strip readable when several are created.
    pub(crate) fn unique_ui_scene_name(&self, base: &str) -> String {
        let taken: HashSet<&str> = self
            .project
            .ui_scenes
            .iter()
            .map(|scene| scene.name.as_str())
            .collect();
        if !taken.contains(base) {
            return base.to_string();
        }
        let mut suffix = 2;
        loop {
            let candidate = format!("{base} {suffix}");
            if !taken.contains(candidate.as_str()) {
                return candidate;
            }
            suffix += 1;
        }
    }

    pub(crate) fn unique_scene_state_name(&self, base: &str) -> String {
        let taken: HashSet<&str> = self
            .project
            .scene_states
            .iter()
            .map(|state| state.name.as_str())
            .collect();
        if !taken.contains(base) {
            return base.to_string();
        }
        let mut suffix = 2;
        loop {
            let candidate = format!("{base} {suffix}");
            if !taken.contains(candidate.as_str()) {
                return candidate;
            }
            suffix += 1;
        }
    }

    pub(crate) fn draw_filesystem_panel(&mut self, ui: &mut egui::Ui) {
        tool_panel_frame().show(ui, |ui| {
            ui.set_min_height(ui.available_height());
            tool_panel_header(ui, icons::FOLDER, "FileSystem", |_| {});
            tool_panel_body(ui, |ui| self.draw_filesystem_panel_body(ui));
        });
    }

    pub(crate) fn draw_filesystem_panel_body(&mut self, ui: &mut egui::Ui) {
        let rows = project_filesystem_rows(&self.project, &self.prefab_library);
        let filter = self.file_filter.to_ascii_lowercase();
        let visible_rows =
            project_filesystem_display_rows(&rows, &filter, &self.collapsed_file_folders);
        let visible_resource_order: Vec<ResourceId> =
            visible_rows.iter().filter_map(|row| row.resource).collect();
        let mut clicked_resource = None;
        let mut toggled_folder = None;
        let selected_resource = self.selection.selected_resource;
        let selected_prefab = self.selection.selected_prefab.clone();
        let selected_resources = self.selection.selected_resources.clone();
        let collapsed_folders = self.collapsed_file_folders.clone();
        let file_scroll_height = (ui.available_height() - 28.0).max(24.0);
        egui::ScrollArea::vertical()
            .id_salt("psxed_filesystem")
            .auto_shrink([false, false])
            .max_height(file_scroll_height)
            .show(ui, |ui| {
                for row in visible_rows {
                    match draw_project_file_row(
                        ui,
                        row,
                        selected_resource,
                        &selected_resources,
                        selected_prefab.as_deref(),
                        &filter,
                        &collapsed_folders,
                    ) {
                        Some(ProjectFileRowAction::Select(click)) => {
                            clicked_resource = Some(click);
                        }
                        Some(ProjectFileRowAction::SelectPrefab(path)) => {
                            self.replace_prefab_selection(path);
                        }
                        Some(ProjectFileRowAction::ToggleFolder(key)) => {
                            toggled_folder = Some(key);
                        }
                        None => {}
                    }
                }
            });
        if let Some(key) = toggled_folder {
            if !self.collapsed_file_folders.insert(key.clone()) {
                self.collapsed_file_folders.remove(&key);
            }
        }
        if let Some(click) = clicked_resource {
            if !self.apply_selected_box_prop_resource_click(click) {
                self.apply_resource_selection_modifiers(
                    click.id,
                    click.modifiers,
                    &visible_resource_order,
                );
            }
        }
        ui.add(
            egui::TextEdit::singleline(&mut self.file_filter)
                .hint_text("Filter files")
                .desired_width(f32::INFINITY),
        );
    }

    pub(crate) fn draw_ui_inspector(&mut self, ui: &mut egui::Ui) {
        self.refresh_texture_thumbs(ui.ctx());
        let requested = self.selection.selected_ui_node;
        let mut changed = false;
        let Some(scene) = self.current_ui_scene() else {
            ui.weak("No UI scene");
            return;
        };
        let selected = if scene.node(requested).is_some() {
            requested
        } else {
            scene.root
        };
        let absolute_rect = scene.absolute_rect(selected);
        let parent_name = scene
            .node(selected)
            .and_then(|node| node.parent)
            .and_then(|parent| scene.node(parent))
            .map(|parent| parent.name.clone())
            .unwrap_or_else(|| "None".to_string());
        let selected_uses_wav = scene.node(selected).is_some_and(|node| {
            matches!(
                node.kind,
                UiNodeKind::Button { .. } | UiNodeKind::Slider { .. } | UiNodeKind::Music { .. }
            )
        });
        if selected != self.selection.selected_ui_node {
            self.selection.selected_ui_node = selected;
        }
        let texture_options: Vec<(ResourceId, String)> = self
            .project
            .resources
            .iter()
            .filter_map(|resource| match &resource.data {
                ResourceData::Material(material) if material.psxt_path.is_some() => {
                    Some((resource.id, resource.name.clone()))
                }
                _ => None,
            })
            .collect();
        let texture_sizes: HashMap<ResourceId, (u16, u16)> = self
            .texture_thumbs
            .iter()
            .map(|(id, entry)| (*id, (entry.stats.width, entry.stats.height)))
            .collect();
        // Scene + option pick-lists are gathered before the mutable
        // node borrow below so the Button/Slider editors can offer
        // dropdowns without re-borrowing `self.project`.
        let scene_options: Vec<(UiSceneId, String)> = self
            .project
            .ui_scenes
            .iter()
            .map(|scene| (scene.id, scene.name.clone()))
            .collect();
        let state_options: Vec<(SceneStateId, String)> = self
            .project
            .scene_states
            .iter()
            .map(|state| (state.id, state.name.clone()))
            .collect();
        let option_choices: Vec<(OptionId, String)> = self
            .project
            .options
            .iter()
            .map(|option| (option.id, option.name.clone()))
            .collect();
        let wav_options = if selected_uses_wav {
            collect_project_wav_options(&self.project_dir)
        } else {
            Vec::new()
        };
        let project_root = self.project_dir.clone();
        let mut preview_message: Option<String> = None;

        let Some(scene) = self.current_ui_scene_mut() else {
            ui.weak("No UI scene");
            return;
        };
        let selected_is_root = selected == scene.root;
        let Some(node) = scene.node_mut(selected) else {
            ui.weak("No UI node selected");
            return;
        };

        let kind_label = node.kind.label();
        inspector_identity_header(
            ui,
            ui_node_kind_icon(kind_label),
            STUDIO_ACCENT,
            &node.name,
            kind_label,
            node.id.raw(),
        );
        changed |= inspector_property_row(ui, "Name", |ui| {
            ui.add(egui::TextEdit::singleline(&mut node.name).desired_width(f32::INFINITY))
                .changed()
        });
        inspector_section(
            ui,
            ("ui-node-context", node.id.raw()),
            icons::LAYERS,
            "Context & Visibility",
            false,
            |ui| {
                inspector_property_row(ui, "Parent", |ui| ui.weak(&parent_name));
                if let Some(rect) = node.kind.rect() {
                    inspector_property_row(ui, "Local bounds", |ui| {
                        ui.weak(format!(
                            "x {}  y {}  w {}  h {}",
                            rect.x, rect.y, rect.width, rect.height
                        ))
                    });
                }
                if let Some(rect) = absolute_rect {
                    inspector_property_row(ui, "Canvas bounds", |ui| {
                        ui.weak(format!(
                            "x {}  y {}  w {}  h {}",
                            rect.x, rect.y, rect.width, rect.height
                        ))
                    });
                }
                changed |= draw_ui_visibility_editor(ui, &mut node.visible_when);
            },
        );

        match &mut node.kind {
            UiNodeKind::Canvas { width, height } => {
                inspector_section(ui, "ui-canvas-layout", icons::MOVE, "Canvas", true, |ui| {
                    ui.weak("Root screen-space canvas.");
                    changed |= drag_u16(ui, "Width", width, 1, 4096);
                    changed |= drag_u16(ui, "Height", height, 1, 4096);
                });
            }
            UiNodeKind::Group { rect } => {
                inspector_section(ui, "ui-group-layout", icons::MOVE, "Layout", true, |ui| {
                    ui.weak("Organizes child UI nodes.");
                    changed |= draw_ui_rect_editor(ui, rect);
                });
            }
            UiNodeKind::Rect {
                rect,
                color,
                gradient,
            } => {
                inspector_section(ui, "ui-rect-layout", icons::MOVE, "Layout", true, |ui| {
                    changed |= draw_ui_rect_editor(ui, rect);
                });
                inspector_section(
                    ui,
                    "ui-rect-appearance",
                    icons::PALETTE,
                    "Appearance",
                    true,
                    |ui| {
                        changed |= color_editor(ui, "Color", color);
                        changed |= draw_ui_gradient_editor(ui, "Gradient", color, gradient);
                    },
                );
            }
            UiNodeKind::Label {
                rect,
                text,
                random_message,
                messages,
                tag,
                align,
                wrap,
                font,
                font_scale,
                letter_spacing,
                color,
                gradient,
                effect,
            } => {
                inspector_section(ui, "ui-label-layout", icons::MOVE, "Layout", true, |ui| {
                    changed |= draw_ui_rect_editor(ui, rect);
                });
                inspector_section(
                    ui,
                    "ui-label-content",
                    icons::PEN_LINE,
                    "Content",
                    true,
                    |ui| {
                        changed |= inspector_property_row(ui, "Text", |ui| {
                            ui.add(
                                egui::TextEdit::multiline(text)
                                    .desired_rows(2)
                                    .desired_width(f32::INFINITY),
                            )
                            .changed()
                        });
                        changed |= ui
                            .checkbox(random_message, "Random message on scene entry")
                            .changed();
                        if *random_message {
                            ui.weak(
                                "One message is chosen when the scene appears and stays fixed.",
                            );
                            let mut remove = None;
                            for (index, message) in messages.iter_mut().enumerate() {
                                ui.horizontal(|ui| {
                                    ui.label(format!("{}", index + 1));
                                    changed |= ui
                                        .add(
                                            egui::TextEdit::multiline(message)
                                                .desired_rows(2)
                                                .desired_width(f32::INFINITY),
                                        )
                                        .changed();
                                    if ui
                                        .small_button("−")
                                        .on_hover_text("Remove message")
                                        .clicked()
                                    {
                                        remove = Some(index);
                                    }
                                });
                            }
                            if let Some(index) = remove {
                                messages.remove(index);
                                changed = true;
                            }
                            if ui.button("+ Add message").clicked() {
                                messages.push(String::new());
                                changed = true;
                            }
                        }
                        changed |= inspector_property_row(ui, "Tag", |ui| {
                            ui.add(egui::TextEdit::singleline(tag).desired_width(f32::INFINITY))
                                .changed()
                        });
                        changed |= ui.checkbox(wrap, "Wrap text").changed();
                    },
                );
                inspector_section(
                    ui,
                    "ui-label-typography",
                    icons::FILE,
                    "Typography",
                    true,
                    |ui| {
                        changed |= draw_ui_text_align_editor(ui, align);
                        changed |= draw_ui_font_choice_editor(ui, font);
                        changed |= draw_ui_font_scale_editor(ui, font_scale);
                        changed |= draw_ui_letter_spacing_editor(ui, letter_spacing);
                    },
                );
                inspector_section(
                    ui,
                    "ui-label-appearance",
                    icons::PALETTE,
                    "Appearance",
                    true,
                    |ui| {
                        changed |= color_editor(ui, "Color", color);
                        changed |= draw_ui_gradient_editor(ui, "Gradient", color, gradient);
                        // Shimmer/FastShimmer sweep a sheen across the glyphs
                        // at runtime (the "Built with PSoXide" boot tag);
                        // other effects have no meaning on text.
                        changed |= draw_ui_image_effect_picker(ui, effect);
                    },
                );
            }
            UiNodeKind::Image {
                rect,
                texture,
                tint,
                effect,
            } => {
                let native_size = texture.and_then(|id| texture_sizes.get(&id).copied());
                inspector_section(ui, "ui-image-layout", icons::MOVE, "Layout", true, |ui| {
                    changed |= draw_ui_rect_editor(ui, rect);
                    ui.horizontal(|ui| {
                        let response = ui.add_enabled(
                            native_size.is_some(),
                            egui::Button::new("Use texture size").small(),
                        );
                        let response = response.on_hover_text(
                            "Resize this Image node to the selected texture's native PS1 texel size.",
                        );
                        if response.clicked() {
                            if let Some((width, height)) = native_size {
                                if rect.width != width || rect.height != height {
                                    rect.width = width;
                                    rect.height = height;
                                    changed = true;
                                }
                            }
                        }
                        if let Some((width, height)) = native_size {
                            ui.label(
                                RichText::new(format!("{width}x{height} texture"))
                                    .color(STUDIO_TEXT_WEAK)
                                    .small(),
                            );
                        } else {
                            ui.label(
                                RichText::new("No decoded texture size")
                                    .color(STUDIO_TEXT_WEAK)
                                    .small(),
                            );
                        }
                    });
                });
                inspector_section(
                    ui,
                    "ui-image-appearance",
                    icons::PALETTE,
                    "Appearance",
                    true,
                    |ui| {
                        changed |=
                            ui_texture_resource_picker(ui, "Texture", texture, &texture_options);
                        changed |= color_editor(ui, "Tint", tint);
                        changed |= draw_ui_image_effect_picker(ui, effect);
                        ui.horizontal(|ui| {
                            if ui.small_button("Dim").clicked() {
                                *tint = [96, 96, 96];
                                changed = true;
                            }
                            if ui.small_button("Neutral").clicked() {
                                *tint = [128, 128, 128];
                                changed = true;
                            }
                            if ui.small_button("Bright").clicked() {
                                *tint = [192, 192, 192];
                                changed = true;
                            }
                            ui.label(RichText::new("128 neutral").color(STUDIO_TEXT_WEAK).small());
                        });
                    },
                );
            }
            UiNodeKind::Bar {
                rect,
                value,
                max,
                fill,
                fill_gradient,
                background,
                background_gradient,
            } => {
                inspector_section(ui, "ui-bar-layout", icons::MOVE, "Layout", true, |ui| {
                    changed |= draw_ui_rect_editor(ui, rect);
                });
                inspector_section(ui, "ui-bar-data", icons::WAYPOINT, "Data", true, |ui| {
                    changed |= draw_ui_value_binding_editor(ui, "Value", value, &option_choices);
                    changed |= draw_ui_value_binding_editor(ui, "Max", max, &option_choices);
                });
                inspector_section(
                    ui,
                    "ui-bar-appearance",
                    icons::PALETTE,
                    "Appearance",
                    true,
                    |ui| {
                        changed |= color_editor(ui, "Fill", fill);
                        changed |=
                            draw_ui_gradient_editor(ui, "Fill Gradient", fill, fill_gradient);
                        changed |= color_editor(ui, "Background", background);
                        changed |= draw_ui_gradient_editor(
                            ui,
                            "Background Gradient",
                            background,
                            background_gradient,
                        );
                    },
                );
            }
            UiNodeKind::Button {
                rect,
                label,
                align,
                font,
                font_scale,
                letter_spacing,
                color,
                background_gradient,
                text_color,
                text_gradient,
                transparent,
                action,
                sfx,
            } => {
                inspector_section(ui, "ui-button-layout", icons::MOVE, "Layout", true, |ui| {
                    changed |= draw_ui_rect_editor(ui, rect);
                });
                inspector_section(
                    ui,
                    "ui-button-content",
                    icons::PEN_LINE,
                    "Content",
                    true,
                    |ui| {
                        changed |= inspector_property_row(ui, "Label", |ui| {
                            ui.add(egui::TextEdit::singleline(label).desired_width(f32::INFINITY))
                                .changed()
                        });
                    },
                );
                inspector_section(
                    ui,
                    "ui-button-typography",
                    icons::FILE,
                    "Typography",
                    false,
                    |ui| {
                        changed |= draw_ui_text_align_editor(ui, align);
                        changed |= draw_ui_font_choice_editor(ui, font);
                        changed |= draw_ui_font_scale_editor(ui, font_scale);
                        changed |= draw_ui_letter_spacing_editor(ui, letter_spacing);
                    },
                );
                inspector_section(
                    ui,
                    "ui-button-appearance",
                    icons::PALETTE,
                    "Appearance",
                    true,
                    |ui| {
                        changed |= color_editor(ui, "Text", text_color);
                        changed |= ui.checkbox(transparent, "Transparent background").changed();
                        changed |= color_editor(ui, "Background", color);
                        changed |= draw_ui_gradient_editor(
                            ui,
                            "Background Gradient",
                            color,
                            background_gradient,
                        );
                        changed |=
                            draw_ui_gradient_editor(ui, "Text Gradient", text_color, text_gradient);
                    },
                );
                inspector_section(
                    ui,
                    "ui-button-action",
                    icons::POINTER,
                    "Interaction",
                    true,
                    |ui| {
                        changed |= draw_ui_action_editor(
                            ui,
                            action,
                            &state_options,
                            &scene_options,
                            &option_choices,
                        );
                    },
                );
                inspector_section(
                    ui,
                    "ui-button-audio",
                    icons::AUDIO_LINES,
                    "Audio",
                    false,
                    |ui| {
                        changed |= draw_button_sfx_editor(
                            ui,
                            sfx,
                            &wav_options,
                            &project_root,
                            &mut preview_message,
                        );
                    },
                );
            }
            UiNodeKind::Slider {
                rect,
                option,
                track,
                track_gradient,
                fill,
                fill_gradient,
                knob,
                knob_gradient,
                sfx,
            } => {
                inspector_section(ui, "ui-slider-layout", icons::MOVE, "Layout", true, |ui| {
                    changed |= draw_ui_rect_editor(ui, rect);
                });
                inspector_section(ui, "ui-slider-data", icons::WAYPOINT, "Data", true, |ui| {
                    changed |= draw_ui_option_picker(ui, "Option", option, &option_choices);
                });
                inspector_section(
                    ui,
                    "ui-slider-appearance",
                    icons::PALETTE,
                    "Appearance",
                    true,
                    |ui| {
                        changed |= color_editor(ui, "Track", track);
                        changed |=
                            draw_ui_gradient_editor(ui, "Track Gradient", track, track_gradient);
                        changed |= color_editor(ui, "Fill", fill);
                        changed |=
                            draw_ui_gradient_editor(ui, "Fill Gradient", fill, fill_gradient);
                        changed |= color_editor(ui, "Knob", knob);
                        changed |=
                            draw_ui_gradient_editor(ui, "Knob Gradient", knob, knob_gradient);
                    },
                );
                inspector_section(
                    ui,
                    "ui-slider-audio",
                    icons::AUDIO_LINES,
                    "Audio",
                    false,
                    |ui| {
                        changed |= draw_slider_sfx_editor(
                            ui,
                            sfx,
                            &wav_options,
                            &project_root,
                            &mut preview_message,
                        );
                    },
                );
            }
            UiNodeKind::Timer {
                millis,
                skippable,
                action,
            } => {
                inspector_section(ui, "ui-timer", icons::POINTER, "Timer", true, |ui| {
                    // Author in seconds; the field stores integer milliseconds.
                    let mut seconds = *millis as f32 / 1000.0;
                    let response = inspector_property_row(ui, "Seconds", |ui| {
                        ui.add(
                            egui::DragValue::new(&mut seconds)
                                .range(0.1..=600.0)
                                .speed(0.1)
                                .fixed_decimals(1),
                        )
                        .changed()
                    });
                    if response {
                        *millis = (seconds * 1000.0).round().clamp(100.0, 600_000.0) as u32;
                        changed = true;
                    }
                    changed |= ui.checkbox(skippable, "Cross skips the wait").changed();
                });
                inspector_section(
                    ui,
                    "ui-timer-action",
                    icons::POINTER,
                    "Interaction",
                    true,
                    |ui| {
                        changed |= draw_ui_action_editor(
                            ui,
                            action,
                            &state_options,
                            &scene_options,
                            &option_choices,
                        );
                    },
                );
            }
            UiNodeKind::Music {
                wav_path,
                volume,
                volume_option,
                playback_speed_q12,
                loop_track,
            } => {
                inspector_section(
                    ui,
                    "ui-music-playback",
                    icons::AUDIO_LINES,
                    "Playback",
                    true,
                    |ui| {
                        ui.weak("Non-visual CD-DA music cue for this UI scene.");
                        changed |= draw_music_wav_picker(ui, "WAV", wav_path, &wav_options);
                        changed |= ui.checkbox(loop_track, "Loop").changed();
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Volume").color(STUDIO_TEXT_WEAK));
                            let mut value = (*volume).min(100) as i32;
                            if ui
                                .add(egui::Slider::new(&mut value, 0..=100).suffix("%"))
                                .changed()
                            {
                                *volume = value as u8;
                                changed = true;
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Playback speed").color(STUDIO_TEXT_WEAK));
                            let mut speed = ((*playback_speed_q12).max(1) as f32) / 4096.0;
                            if ui
                                .add(egui::Slider::new(&mut speed, 0.25..=2.0).suffix("x"))
                                .changed()
                            {
                                *playback_speed_q12 =
                                    ((speed * 4096.0).round() as i32).clamp(1, 0x3FFF) as u16;
                                changed = true;
                            }
                        });
                        changed |= draw_optional_ui_option_picker(
                            ui,
                            "Volume option",
                            volume_option,
                            &option_choices,
                        );
                    },
                );
            }
        }

        // Scene-level focus-ring style, edited from the root canvas so
        // the scene's shared chrome lives in one place.
        if selected_is_root {
            let style = &mut scene.focus_style;
            inspector_section(
                ui,
                "ui-scene-focus-ring",
                icons::FOCUS,
                "Focus Ring",
                false,
                |ui| {
                    ui.weak("Highlight drawn around the focused button or slider.");
                    changed |= draw_ui_focus_effect_picker(ui, &mut style.effect);
                    changed |= color_editor(ui, "Color A", &mut style.color_a);
                    if style.effect != UiFocusEffect::Solid {
                        changed |= color_editor(ui, "Color B", &mut style.color_b);
                        changed |= drag_u16(ui, "Period (frames)", &mut style.period, 0, 600);
                    }
                    changed |= drag_u8(ui, "Thickness", &mut style.thickness, 1, 4);
                    changed |= drag_u8(ui, "Margin", &mut style.margin, 0, 8);
                    if style.effect == UiFocusEffect::Corners {
                        changed |= drag_u8(ui, "Corner Length", &mut style.corner_len, 2, 64);
                    }
                },
            );
        }

        if changed {
            self.mark_dirty();
        }
        if let Some(message) = preview_message {
            self.status = message;
        }
    }

    pub(crate) fn draw_inspector(
        &mut self,
        ctx: &egui::Context,
        camera_preview: Option<EditorCameraPreviewPresentation>,
    ) {
        if !self.inspector_open {
            self.inspector_undo_transaction = None;
            return;
        }
        self.refresh_texture_thumbs(ctx);
        self.prepare_inspector_undo_frame(ctx);
        let undo_candidate =
            inspector_has_edit_input(ctx).then(|| (self.project.clone(), self.history.epoch()));
        let max_width = max_resizable_side_dock_width(ctx, false);
        egui::SidePanel::right("psxed_inspector")
            .resizable(true)
            .default_width(320.0)
            .min_width(RESIZABLE_DOCK_MIN_WIDTH)
            .max_width(max_width)
            .frame(dock_frame())
            .show(ctx, |ui| {
                fixed_panel_content(ui, "psxed_inspector_fixed_content", |ui| {
                    ui.set_width(ui.available_width().max(1.0));
                    tool_panel_frame().show(ui, |ui| {
                        let content_width = ui.available_width().max(1.0);
                        constrain_resizable_dock_content(ui, content_width);
                        tool_panel_header(ui, icons::SCAN, "Inspector", |_| {});
                        tool_panel_body(ui, |ui| {
                            apply_inspector_layout(ui);
                            let content_width = ui.available_width().max(1.0);
                            constrain_resizable_dock_content(ui, content_width);
                            egui::ScrollArea::vertical()
                                .id_salt("psxed_inspector_scroll")
                                .max_width(content_width)
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    constrain_resizable_dock_content(ui, content_width);
                                    // Selection priority: primitive (Select tool's
                                    // product -- face, edge, or vertex) → resource
                                    // (clicked in the bottom panel) → node (scene
                                    // tree row). The primitive branch wins because
                                    // it's the active edit target during paint and
                                    // height-edit workflows.
                                    if let Some(selection) = self.selection.selected_primitive {
                                        match selection {
                                            Selection::Face(face) => {
                                                self.draw_face_inspector(ui, face)
                                            }
                                            Selection::Triangle(triangle) => {
                                                self.draw_triangle_inspector(ui, triangle)
                                            }
                                            Selection::Edge(edge) => {
                                                self.draw_edge_inspector(ui, edge)
                                            }
                                            Selection::Vertex(vertex) => {
                                                self.draw_vertex_inspector(ui, vertex)
                                            }
                                        }
                                        return;
                                    }

                                if self.active_workspace == WorkspaceView::Ui {
                                    self.draw_ui_inspector(ui);
                                    return;
                                }

                                if let Some(path) = self.selection.selected_prefab.clone() {
                                    self.draw_prefab_inspector(ui, &path);
                                    return;
                                }

                                if let Some(resource_id) = self.selection.selected_resource {
                                    self.draw_resource_inspector(ui, resource_id);
                                    return;
                                }

                                let material_options = self.project.material_options();
                                let material_texture_dimensions: Vec<(ResourceId, [u16; 2])> = self
                                    .texture_thumbs
                                    .iter()
                                    .map(|(id, entry)| {
                                        (*id, [entry.stats.width, entry.stats.height])
                                    })
                                    .collect();
                                let texture_options: Vec<(ResourceId, String)> = self
                                    .project
                                    .resources
                                    .iter()
                                    .filter_map(|resource| match &resource.data {
                                        ResourceData::Texture { .. } => {
                                            Some((resource.id, resource.name.clone()))
                                        }
                                        _ => None,
                                    })
                                    .collect();
                                let room_options = collect_room_options(&self.project);
                                let model_options = collect_model_options(&self.project);
                                let character_options = collect_character_options(&self.project);
                                let weapon_options = collect_weapon_options(&self.project);
                                let selected = self.selection.selected_node;
                                let animator_clip_context = selected_animator_clip_context(
                                    &self.project,
                                    selected,
                                    &self.project_dir,
                                );
                                let selected_sector = self.selection.selected_sector;
                                let selected_sector_count = self.selection.selected_sectors.len();

                                let mut changed = false;
                                // Picker `→` jump-to requests bubble up here.
                                // Applied after both phases release their borrows.
                                let mut nav_target: Option<ResourceId> = None;
                                let mut node_nav_target: Option<NodeId> = None;
                                let mut world_sector_size_change: Option<i32> = None;
                                let mut room_grid_resize: Option<(u16, u16)> = None;
                                let mut character_preview_action = None;
                                let inherited_sector_size =
                                    self.project.world_sector_size_for_node(selected);
                                let selected_kind_before = self
                                    .project
                                    .active_scene()
                                    .node(selected)
                                    .map(|node| node.kind.clone());

                                // Phase 1: mutate the selected node (transform + kind props).
                                {
                                    let scene = self.project.active_scene_mut();
                                    let Some(node) = scene.node_mut(selected) else {
                                        ui.weak("No node selected");
                                        return;
                                    };

                                    let node_kind_label = node.kind.label();
                                    inspector_identity_header(
                                        ui,
                                        node_lucide_icon(
                                            node_kind_label,
                                            node.id == NodeId::ROOT,
                                        ),
                                        node_lucide_color(
                                            node_kind_label,
                                            node.id == NodeId::ROOT,
                                            true,
                                        ),
                                        &node.name,
                                        node_kind_label,
                                        node.id.raw(),
                                    );
                                    changed |= inspector_property_row(ui, "Name", |ui| {
                                        ui.add(
                                            egui::TextEdit::singleline(&mut node.name)
                                                .desired_width(f32::INFINITY),
                                        )
                                        .changed()
                                    });

                                    let is_world = matches!(&node.kind, NodeKind::World { .. });
                                    let transform_kind = node_transform_inspector(&node.kind);
                                    if is_world {
                                        changed |= draw_transform_policy_editor(
                                            ui,
                                            node,
                                            inherited_sector_size,
                                            &texture_options,
                                            &mut nav_target,
                                            &mut world_sector_size_change,
                                        );
                                    } else if transform_kind != NodeTransformInspector::Hidden {
                                        inspector_section(
                                            ui,
                                            ("node-transform", node.id.raw()),
                                            icons::MOVE,
                                            "Transform",
                                            true,
                                            |ui| {
                                                changed |= draw_transform_policy_editor(
                                                    ui,
                                                    node,
                                                    inherited_sector_size,
                                                    &texture_options,
                                                    &mut nav_target,
                                                    &mut world_sector_size_change,
                                                );
                                            },
                                        );
                                    }
                                    if !is_world {
                                        let properties_label = if node.kind.is_component() {
                                            "Component Settings"
                                        } else {
                                            "Properties"
                                        };
                                        inspector_section(
                                            ui,
                                            ("node-properties", node.id.raw()),
                                            icons::CIRCLE_DOT,
                                            properties_label,
                                            true,
                                            |ui| {
                                            changed |= draw_node_kind_editor(
                                                ui,
                                                &mut node.kind,
                                            NodeKindEditorContext {
                                                material_options: &material_options,
                                                material_texture_dimensions:
                                                    &material_texture_dimensions,
                                                texture_options: &texture_options,
                                                room_options: &room_options,
                                                model_options: &model_options,
                                                character_options: &character_options,
                                                weapon_options: &weapon_options,
                                                animator_clip_context: animator_clip_context
                                                    .as_ref(),
                                                inherited_sector_size,
                                                room_grid_resize: &mut room_grid_resize,
                                                nav_target: &mut nav_target,
                                                character_preview_action:
                                                    &mut character_preview_action,
                                                    camera_preview,
                                                },
                                            );
                                            },
                                        );
                                    }
                                }

                                if let Some(before) = selected_kind_before.as_ref() {
                                    self.reconcile_character_preview_after_node_kind_edit(
                                        selected,
                                        before,
                                    );
                                }

                                if let Some(new_sector_size) = world_sector_size_change {
                                    if let Some(applied) =
                                        self.project.set_world_sector_size(selected, new_sector_size)
                                    {
                                        self.status = format!("World grid size set to {applied}");
                                        changed = true;
                                    }
                                }
                                if let Some((new_w, new_d)) = room_grid_resize {
                                    if resize_room_grid_preserving_child_positions(
                                        self.project.active_scene_mut(),
                                        selected,
                                        new_w,
                                        new_d,
                                        self.active_floor,
                                    ) {
                                        changed = true;
                                    }
                                }

                                // A ModelRenderer's selected Material is
                                // editable in place. The picker above still
                                // owns selection/navigation; this panel edits
                                // the canonical resource after the scene-node
                                // borrow has been released.
                                let inline_model_material = self
                                    .project
                                    .active_scene()
                                    .node(selected)
                                    .and_then(|node| match node.kind {
                                        NodeKind::ModelRenderer {
                                            material: Some(material),
                                            ..
                                        } => Some(material),
                                        _ => None,
                                });
                                if let Some(material_id) = inline_model_material {
                                    let mut open_material_lab = false;
                                    egui::CollapsingHeader::new(icons::label(
                                        icons::BLEND,
                                        "Material Override",
                                    ))
                                    .default_open(true)
                                    .show(ui, |ui| {
                                        if ui
                                            .button(icons::label(
                                                icons::PALETTE,
                                                "Open full Material Lab",
                                            ))
                                            .clicked()
                                        {
                                            open_material_lab = true;
                                        }
                                        let Some(resource) =
                                            self.project.resource_mut(material_id)
                                        else {
                                            ui.colored_label(
                                                Color32::from_rgb(220, 120, 100),
                                                "Material resource is missing.",
                                            );
                                            return;
                                        };
                                        let ResourceData::Material(material) =
                                            &mut resource.data
                                        else {
                                            ui.colored_label(
                                                Color32::from_rgb(220, 120, 100),
                                                "Selected resource is not a Material.",
                                            );
                                            return;
                                        };
                                        changed |= draw_model_material_override_editor(
                                            ui,
                                            material,
                                            &material_options,
                                            material_id,
                                        );
                                    });
                                    if open_material_lab {
                                        self.material_lab.focused_material = Some(material_id);
                                        self.active_workspace = WorkspaceView::Material;
                                        self.status = "Opened Material Lab".to_string();
                                    }
                                }
                                if changed && self.selected_node_is_player_source() {
                                    self.demote_player_sources_except(Some(selected));
                                }
                                let selected_is_world = self
                                    .project
                                    .active_scene()
                                    .node(selected)
                                    .is_some_and(|node| matches!(node.kind, NodeKind::World { .. }));
                                if selected_is_world {
                                    changed |= draw_playtest_render_settings(
                                        ui,
                                        &mut self.project.runtime_depth_sort_mode,
                                        &mut self.project.runtime_texture_split_mode,
                                        &mut self.project.runtime_room_draw_order_mode,
                                        &mut self.project.runtime_texture_split_max_edge,
                                    );
                                }

                                // Phase 2: component host/member authoring. This uses
                                // its own borrow so adding/selecting component nodes does
                                // not fight the selected node's property editor above.
                                changed |= self.draw_component_authoring_panel(
                                    ui,
                                    selected,
                                    &character_options,
                                    &mut nav_target,
                                    &mut character_preview_action,
                                );

                                if let Some(action) = character_preview_action {
                                    changed |= self.preview_character_action(selected, action);
                                }

                                // Phase 3: per-sector authoring appears only when a sector is
                                // actively selected. Do not attach room/sector diagnostics to
                                // every node that happens to have an active room ancestor.
                                if let Some((sx, sz)) = selected_sector {
                                    let room_id = self
                                        .selection
                                        .selected_sectors
                                        .iter()
                                        .find_map(|(room, x, z)| {
                                            (*x == sx && *z == sz).then_some(*room)
                                        })
                                        .or_else(|| self.active_room_id());
                                    if selected_sector_count > 1 {
                                        egui::CollapsingHeader::new(icons::label(
                                            icons::GRID,
                                            "Sector Selection",
                                        ))
                                        .default_open(true)
                                        .show(ui, |ui| {
                                            ui.label(format!(
                                                "{selected_sector_count} sectors selected"
                                            ));
                                            if ui
                                                .button("Autotile")
                                                .on_hover_text(
                                                    "Autotile every wall in the selected sector tiles.",
                                                )
                                                .clicked()
                                            {
                                                self.autotile_selected_sector_walls();
                                            }
                                            ui.weak("The detailed inspector edits the last selected sector for now.");
                                        });
                                    }
                                    if let Some(room_id) = room_id {
                                        if draw_sector_inspector(
                                            ui,
                                            &mut self.project,
                                            room_id,
                                            sx,
                                            sz,
                                            self.active_floor,
                                            &material_options,
                                            &mut nav_target,
                                        ) {
                                            changed = true;
                                        }
                                    }
                                }

                                // Phase 4: relationship panels that need a fresh scene borrow.
                                let scene = self.project.active_scene();
                                let Some(node) = scene.node(selected) else {
                                    if changed {
                                        self.mark_dirty();
                                    }
                                    return;
                                };

                                if matches!(node.kind, NodeKind::Portal { .. }) {
                                    if let Some(connection) = connection_for_portal(scene, selected)
                                    {
                                        node_nav_target =
                                            draw_portal_connection_inspector(ui, scene, &connection);
                                    }
                                }

                                if changed {
                                    self.mark_dirty();
                                    // The native viewport render happens before this inspector
                                    // pass. Schedule one more frame so it consumes the authored
                                    // clip/animation change immediately instead of waiting for an
                                    // unrelated event such as Save, Build, or project reopen.
                                    ui.ctx().request_repaint();
                                }

                                // Apply any picker `→` jump-to. Phase 1 / 2 borrows
                                // are released by the time the closure body reaches
                                // here, and the next frame the inspector will see
                                // `selected_resource = Some(target)` and route to
                                // `draw_resource_inspector`.
                                if let Some(target) = nav_target {
                                    self.replace_resource_selection(target);
                                    self.clear_node_selection_state();
                                    self.clear_primitive_selection_state();
                                    self.clear_sector_selection();
                                }
                                if let Some(target) = node_nav_target {
                                    self.replace_node_selection(target);
                                    self.clear_resource_selection_state();
                                    self.clear_primitive_selection_state();
                                    self.clear_sector_selection();
                                }
                            });
                        reserve_remaining_panel_space(ui);
                    });
                });
                });
            });
        if let Some((project_before, history_epoch_before)) = undo_candidate {
            self.finish_inspector_undo_frame(project_before, history_epoch_before, ctx);
        }
    }

    /// Build the breadcrumb crumbs shown above the face inspector.
    /// Always starts with the face itself; appends a clickable
    /// `Material: <name>` crumb when the face has a material, and
    /// further appends a clickable `Texture: <name>` crumb when
    /// that material wraps one. The chain shortens naturally for
    /// partially-assigned faces.
    pub(crate) fn face_breadcrumb(
        &self,
        face: FaceRef,
        current_material: Option<ResourceId>,
    ) -> Vec<BreadcrumbCrumb> {
        let mut crumbs = vec![BreadcrumbCrumb {
            label: format!("Face: {}", describe_face(face)),
            nav: None,
        }];
        if let Some(material_id) = current_material {
            if let Some(material_resource) = self.project.resource(material_id) {
                crumbs.push(BreadcrumbCrumb {
                    label: format!("Material: {}", material_resource.name),
                    nav: Some(material_id),
                });
            }
        }
        crumbs
    }

    /// Inspector panel for the face currently selected by the
    /// Select tool. Surfaces material picker, height fields, and a
    /// preview thumbnail of the linked texture so the user can
    /// retarget materials without opening the resources panel.
    pub(crate) fn draw_triangle_inspector(
        &mut self,
        ui: &mut egui::Ui,
        triangle: HorizontalTriangleRef,
    ) {
        let mut nav_target: Option<ResourceId> = None;
        let material_options = self.project.material_options();
        let Some((parent_material, parent_uv, parent_walkable)) =
            self.triangle_parent_values(triangle)
        else {
            ui.weak("Triangle target is gone");
            return;
        };
        let current_material = self.triangle_material(triangle);
        let preview_thumb = current_material
            .and_then(|id| self.project.resource(id))
            .and_then(|resource| self.texture_thumb_entry(resource))
            .map(|entry| (entry.handle.id(), entry.stats));
        let mut crumbs = vec![
            BreadcrumbCrumb {
                label: format!("Triangle: {}", describe_triangle(triangle)),
                nav: None,
            },
            BreadcrumbCrumb {
                label: format!("Face: {}", describe_face(triangle.parent_face())),
                nav: None,
            },
        ];
        if let Some(material_id) = current_material {
            if let Some(material_resource) = self.project.resource(material_id) {
                crumbs.push(BreadcrumbCrumb {
                    label: format!("Material: {}", material_resource.name),
                    nav: Some(material_id),
                });
            }
        }

        ui.horizontal(|ui| {
            draw_inline_icon(ui, icons::GRID, STUDIO_ACCENT);
            ui.strong(describe_triangle(triangle));
        });
        draw_breadcrumb(ui, &crumbs, &mut nav_target);
        ui.separator();
        draw_psxt_preview_block(ui, preview_thumb);

        let mut changed = false;
        {
            let Some(grid) = self.room_floor_grid_mut(triangle.room) else {
                ui.weak("Selected triangle's Room is gone");
                return;
            };
            let Some(sector) = grid.sector_mut(triangle.sx, triangle.sz) else {
                ui.weak("Cell out of grid bounds");
                return;
            };
            let face_data = match triangle.surface {
                HorizontalSurfaceKind::Floor => sector.floor.as_mut(),
                HorizontalSurfaceKind::Ceiling => sector.ceiling.as_mut(),
            };
            let Some(face_data) = face_data else {
                ui.weak("Triangle's parent face was removed");
                return;
            };
            let parent_heights = Self::triangle_parent_heights(face_data, triangle.index);
            let override_data = face_data.triangle_override_mut(triangle.index.idx());

            egui::CollapsingHeader::new(icons::label(icons::BLEND, "Material"))
                .default_open(true)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("    Parent");
                        ui.weak(material_option_label(parent_material, &material_options));
                    });
                    let mut enabled = override_data.material.is_some();
                    if ui.checkbox(&mut enabled, "    Override").changed() {
                        override_data.material = enabled
                            .then(|| GridTriangleMaterialOverride::from_material(parent_material));
                        changed = true;
                    }
                    if enabled {
                        let mut material = override_data.material.and_then(|m| m.material());
                        if material_picker(
                            ui,
                            "    Material",
                            &mut material,
                            &material_options,
                            &mut nav_target,
                        ) {
                            override_data.material =
                                Some(GridTriangleMaterialOverride::from_material(material));
                            changed = true;
                        }
                    }
                });

            egui::CollapsingHeader::new(icons::label(icons::GRID, "UV"))
                .default_open(false)
                .show(ui, |ui| {
                    let mut enabled = override_data.uv.is_some();
                    if ui.checkbox(&mut enabled, "    Override").changed() {
                        override_data.uv = enabled.then_some(parent_uv);
                        changed = true;
                    }
                    if let Some(uv) = override_data.uv.as_mut() {
                        let uv_before = *uv;
                        let _edit = uv_transform_controls(uv, ui);
                        changed |= *uv != uv_before;
                    }
                });

            egui::CollapsingHeader::new(icons::label(icons::MOVE, "Height"))
                .default_open(false)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("    Parent");
                        ui.weak(format!(
                            "{} / {} / {}",
                            parent_heights[0], parent_heights[1], parent_heights[2]
                        ));
                    });
                    let mut enabled = override_data.heights.is_some();
                    if ui.checkbox(&mut enabled, "    Split").changed() {
                        override_data.heights = enabled.then_some(parent_heights);
                        changed = true;
                    }
                    if let Some(heights) = override_data.heights.as_mut() {
                        let corners = triangle.corners;
                        for idx in 0..3 {
                            ui.horizontal(|ui| {
                                ui.label(format!("    {}", corner_label(corners[idx])));
                                let mut height = heights[idx];
                                if ui
                                    .add(
                                        egui::DragValue::new(&mut height)
                                            .speed(HEIGHT_QUANTUM as f32),
                                    )
                                    .changed()
                                {
                                    heights[idx] = snap_height(height);
                                    changed = true;
                                }
                            });
                        }
                    }
                });

            egui::CollapsingHeader::new(icons::label(icons::WAYPOINT, "Collision"))
                .default_open(false)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("    Parent");
                        ui.weak(if parent_walkable {
                            "Walkable"
                        } else {
                            "Blocked"
                        });
                    });
                    let mut enabled = override_data.walkable.is_some();
                    if ui.checkbox(&mut enabled, "    Override").changed() {
                        override_data.walkable = enabled.then_some(parent_walkable);
                        changed = true;
                    }
                    if let Some(walkable) = override_data.walkable.as_mut() {
                        if ui.checkbox(walkable, "    Walkable").changed() {
                            changed = true;
                        }
                    }
                });

            if ui
                .button("Clear triangle overrides")
                .on_hover_text(
                    "Return this triangle to the parent face material, UV, height, and walkability.",
                )
                .clicked()
            {
                override_data.material = None;
                override_data.uv = None;
                override_data.heights = None;
                override_data.walkable = None;
                changed = true;
            }
        }

        if changed {
            self.mark_dirty();
        }
        if let Some(target) = nav_target {
            self.clear_primitive_selection_state();
            self.replace_resource_selection(target);
            self.clear_node_selection_state();
            self.clear_sector_selection();
        }
    }

    pub(crate) fn triangle_parent_values(
        &self,
        triangle: HorizontalTriangleRef,
    ) -> Option<(Option<ResourceId>, GridUvTransform, bool)> {
        let grid = self.room_grid_view(triangle.room)?;
        let sector = grid.sector(triangle.sx, triangle.sz)?;
        match triangle.surface {
            HorizontalSurfaceKind::Floor => {
                let face = sector.floor.as_ref()?;
                Some((face.material, face.uv, face.walkable))
            }
            HorizontalSurfaceKind::Ceiling => {
                let face = sector.ceiling.as_ref()?;
                Some((face.material, face.uv, face.walkable))
            }
        }
    }

    pub(crate) fn triangle_parent_heights(
        face: &GridHorizontalFace,
        triangle: HorizontalTriangleIndex,
    ) -> [i32; 3] {
        let corners = horizontal_triangle_corners(face.split, triangle);
        [
            face.heights[corners[0].idx()],
            face.heights[corners[1].idx()],
            face.heights[corners[2].idx()],
        ]
    }

    /// Edge inspector -- height of both endpoint vertices. In
    /// Welded mode each endpoint resolves through `physical_vertex`;
    /// in Detached mode it resolves to only the edge's own corner.
    pub(crate) fn draw_edge_inspector(&mut self, ui: &mut egui::Ui, edge: EdgeRef) {
        // Resolve both endpoints up front while the project is
        // borrowed immutably. Keeps the edit phase below clear
        // of cross-borrow tangles.
        let (mut endpoint_a, mut endpoint_b) = match self
            .room_grid_view(edge.room)
            .and_then(|grid| edge_endpoints_with_connectivity(grid, edge, self.vertex_connectivity))
        {
            Some(pair) => pair,
            None => {
                ui.weak("Edge target is gone");
                return;
            }
        };

        ui.horizontal(|ui| {
            draw_inline_icon(ui, icons::GRID, STUDIO_ACCENT);
            ui.strong(describe_edge(edge));
        });
        ui.separator();

        let mut new_a = endpoint_a.world[1];
        let mut new_b = endpoint_b.world[1];
        let mut changed_a = false;
        let mut changed_b = false;
        ui.horizontal(|ui| {
            ui.label("    A");
            if ui
                .add(egui::DragValue::new(&mut new_a).speed(HEIGHT_QUANTUM as f32))
                .changed()
            {
                changed_a = true;
            }
        });
        ui.horizontal(|ui| {
            ui.label("    B");
            if ui
                .add(egui::DragValue::new(&mut new_b).speed(HEIGHT_QUANTUM as f32))
                .changed()
            {
                changed_b = true;
            }
        });

        if !(changed_a || changed_b) {
            return;
        }

        // The Inspector's shared transaction boundary records the pre-edit
        // document and coalesces a multi-frame drag into one undo step.
        let new_a = snap_height(new_a);
        let new_b = snap_height(new_b);
        endpoint_a.world[1] = new_a;
        endpoint_b.world[1] = new_b;
        let Some(grid) = self.room_floor_grid_mut(edge.room) else {
            return;
        };
        if changed_a {
            apply_vertex_height(grid, &endpoint_a, new_a);
        }
        if changed_b {
            apply_vertex_height(grid, &endpoint_b, new_b);
        }
        let total_members = endpoint_a.members.len() + endpoint_b.members.len();
        self.status = format!(
            "Moved edge endpoints ({} face-corners follow)",
            total_members
        );
        self.mark_dirty();
    }

    /// Vertex inspector -- one Y handle for the resolved vertex
    /// group. Lists every member so the user sees what will move;
    /// a `Break` button raises the seed alone by `HEIGHT_QUANTUM`
    /// so the user can split a shared vertex into two.
    pub(crate) fn draw_vertex_inspector(&mut self, ui: &mut egui::Ui, vertex: VertexRef) {
        let mut physical = match self.room_grid_view(vertex.room).and_then(|grid| {
            vertex_for_seed(
                grid,
                vertex.anchor.as_face_corner(),
                self.vertex_connectivity,
            )
        }) {
            Some(pv) => pv,
            None => {
                ui.weak("Vertex target is gone");
                return;
            }
        };

        ui.horizontal(|ui| {
            draw_inline_icon(ui, icons::GRID, STUDIO_ACCENT);
            ui.strong(describe_vertex(vertex));
        });
        ui.label(format!(
            "world {} {} {}",
            physical.world[0], physical.world[1], physical.world[2]
        ));
        ui.separator();

        let mut new_y = physical.world[1];
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label("    Y");
            if ui
                .add(egui::DragValue::new(&mut new_y).speed(HEIGHT_QUANTUM as f32))
                .changed()
            {
                changed = true;
            }
        });

        let break_clicked = ui
            .add_enabled(
                matches!(self.vertex_connectivity, VertexConnectivity::Welded),
                egui::Button::new("Break"),
            )
            .on_hover_text(
                "Move this corner alone by one quantum, splitting it from the shared group.",
            )
            .clicked();

        egui::CollapsingHeader::new(format!("Members ({})", physical.members.len()))
            .default_open(false)
            .show(ui, |ui| {
                for member in &physical.members {
                    ui.label(face_corner_label(*member));
                }
            });

        if changed {
            let new_y = snap_height(new_y);
            physical.world[1] = new_y;
            let moved = if let Some(grid) = self.room_floor_grid_mut(vertex.room) {
                apply_vertex_height(grid, &physical, new_y);
                true
            } else {
                false
            };
            if moved {
                self.status = format!(
                    "Moved vertex ({} face-corners follow)",
                    physical.members.len()
                );
                self.mark_dirty();
            }
        } else if break_clicked {
            let new_y = snap_height(physical.world[1] + HEIGHT_QUANTUM);
            // Apply only to the seed -- the rest of the group
            // stays put, so they cease being coincident.
            let seed = vertex.anchor.as_face_corner();
            let broke = if let Some(grid) = self.room_floor_grid_mut(vertex.room) {
                write_face_corner_height(grid, seed, new_y);
                true
            } else {
                false
            };
            if broke {
                self.status = "Broke vertex; seed corner moved by one quantum".to_string();
                self.mark_dirty();
            }
        }
    }

    pub(crate) fn draw_face_inspector(&mut self, ui: &mut egui::Ui, face: FaceRef) {
        // Deferred navigation request: pickers fill this in when
        // the user clicks the `→` jump button. Applied after the
        // mutable scene borrow below releases so we never mutate
        // `self.selected_*` while the project is borrowed.
        let mut nav_target: Option<ResourceId> = None;
        let mut material_options = self.project.material_options();
        // Snapshot the face's current material id BEFORE we borrow
        // the scene mutably, so the preview lookup below can run
        // without fighting the inspector's `&mut` on resource.data.
        let current_material = self
            .project
            .active_scene()
            .node(face.room)
            .and_then(|node| match &node.kind {
                NodeKind::Section { grid } => Some(grid),
                _ => None,
            })
            .and_then(|grid| grid.sector(face.sx, face.sz))
            .and_then(|sector| match face.kind {
                FaceKind::Floor => sector.floor.as_ref().and_then(|f| f.material),
                FaceKind::Ceiling => sector.ceiling.as_ref().and_then(|c| c.material),
                FaceKind::Wall { dir, stack } => sector
                    .walls
                    .get(dir)
                    .get(stack as usize)
                    .and_then(|w| w.material),
            });
        if let Some(id) = current_material {
            if !material_options
                .iter()
                .any(|(candidate, _)| *candidate == id)
                && self
                    .project
                    .resource(id)
                    .is_some_and(|resource| resource.name.starts_with(AUTO_PAINT_BLEND_PREFIX))
            {
                material_options.push((id, "Paint blend (generated)".to_string()));
            }
        }
        let preview_thumb = current_material
            .and_then(|id| self.project.resource(id))
            .and_then(|resource| self.texture_thumb_entry(resource))
            .map(|entry| (entry.handle.id(), entry.stats));

        // Build the Face › Material › Texture breadcrumb up
        // front while the project is still only borrowed
        // immutably. Crumbs link to whatever's reachable; the
        // chain auto-shortens when the face has no material or
        // the material has no texture.
        let crumbs = self.face_breadcrumb(face, current_material);

        ui.horizontal(|ui| {
            draw_inline_icon(ui, icons::GRID, STUDIO_ACCENT);
            ui.strong(describe_face(face));
        });
        draw_breadcrumb(ui, &crumbs, &mut nav_target);
        ui.separator();
        draw_psxt_preview_block(ui, preview_thumb);

        let Some(grid) = self.room_floor_grid_mut(face.room) else {
            ui.weak("Selected face's Room is gone");
            return;
        };
        if face.sx >= grid.width || face.sz >= grid.depth {
            ui.weak("Cell out of grid bounds");
            return;
        }
        let sector_size = grid.sector_size;
        let Some(sector) = grid.ensure_sector(face.sx, face.sz) else {
            ui.weak("Cell not authored");
            return;
        };

        let mut changed = false;
        let mut status_message: Option<String> = None;
        let mut selected_uv_change: Option<(GridUvTransformEdit, GridUvTransform)> = None;
        match face.kind {
            FaceKind::Floor => {
                let Some(face_data) = sector.floor.as_mut() else {
                    ui.weak("Floor was removed");
                    return;
                };
                let uv_before = face_data.uv;
                egui::CollapsingHeader::new(icons::label(icons::BLEND, "Material"))
                    .default_open(true)
                    .show(ui, |ui| {
                        changed |= material_picker(
                            ui,
                            "    Material",
                            &mut face_data.material,
                            &material_options,
                            &mut nav_target,
                        );
                    });
                egui::CollapsingHeader::new(icons::label(icons::MOVE, "Heights"))
                    .default_open(true)
                    .show(ui, |ui| {
                        changed |= height_row("Height", &mut face_data.heights, ui);
                        changed |= split_row("Split", &mut face_data.split, ui);
                    });
                let mut uv_edit = GridUvTransformEdit::default();
                egui::CollapsingHeader::new(icons::label(icons::GRID, "UV"))
                    .default_open(false)
                    .show(ui, |ui| {
                        uv_edit = uv_transform_controls(&mut face_data.uv, ui);
                    });
                changed |= face_data.uv != uv_before;
                if uv_edit.changed() {
                    selected_uv_change = Some((uv_edit, face_data.uv));
                }
            }
            FaceKind::Ceiling => {
                let Some(face_data) = sector.ceiling.as_mut() else {
                    ui.weak("Ceiling was removed");
                    return;
                };
                let uv_before = face_data.uv;
                egui::CollapsingHeader::new(icons::label(icons::BLEND, "Material"))
                    .default_open(true)
                    .show(ui, |ui| {
                        changed |= material_picker(
                            ui,
                            "    Material",
                            &mut face_data.material,
                            &material_options,
                            &mut nav_target,
                        );
                    });
                egui::CollapsingHeader::new(icons::label(icons::MOVE, "Heights"))
                    .default_open(true)
                    .show(ui, |ui| {
                        changed |= height_row("Height", &mut face_data.heights, ui);
                        changed |= split_row("Split", &mut face_data.split, ui);
                    });
                let mut uv_edit = GridUvTransformEdit::default();
                egui::CollapsingHeader::new(icons::label(icons::GRID, "UV"))
                    .default_open(false)
                    .show(ui, |ui| {
                        uv_edit = uv_transform_controls(&mut face_data.uv, ui);
                    });
                changed |= face_data.uv != uv_before;
                if uv_edit.changed() {
                    selected_uv_change = Some((uv_edit, face_data.uv));
                }
            }
            FaceKind::Wall { dir, stack } => {
                let walls = sector.walls.get_mut(dir);
                let mut split_wall = false;
                {
                    let Some(wall) = walls.get_mut(stack as usize) else {
                        ui.weak("Wall stack entry was removed");
                        return;
                    };
                    let uv_before = wall.uv;
                    let material_before = wall.material;
                    let mut uv_edit = GridUvTransformEdit::default();
                    egui::CollapsingHeader::new(icons::label(icons::BLEND, "Material"))
                        .default_open(true)
                        .show(ui, |ui| {
                            changed |= material_picker(
                                ui,
                                "    Material",
                                &mut wall.material,
                                &material_options,
                                &mut nav_target,
                            );
                            if wall.material != material_before && wall.material.is_some() {
                                wall.autotile_uv(sector_size);
                            }
                        });
                    egui::CollapsingHeader::new(icons::label(icons::MOVE, "Span"))
                        .default_open(true)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label("    Bottom");
                                let mut bot = wall.heights[0];
                                if ui
                                    .add(
                                        egui::DragValue::new(&mut bot).speed(HEIGHT_QUANTUM as f32),
                                    )
                                    .changed()
                                {
                                    let bot = snap_height(bot);
                                    wall.heights[0] = bot;
                                    wall.heights[1] = bot;
                                    changed = true;
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.label("    Top");
                                let mut top = wall.heights[2];
                                if ui
                                    .add(
                                        egui::DragValue::new(&mut top).speed(HEIGHT_QUANTUM as f32),
                                    )
                                    .changed()
                                {
                                    let top = snap_height(top);
                                    wall.heights[2] = top;
                                    wall.heights[3] = top;
                                    changed = true;
                                }
                            });
                            if ui
                                .button("Split by sector height")
                                .on_hover_text(
                                    "Replace this wall with sector-height stack segments. UV settings are preserved.",
                                )
                                .clicked()
                            {
                                split_wall = true;
                            }
                        });
                    egui::CollapsingHeader::new(icons::label(icons::GRID, "UV"))
                        .default_open(false)
                        .show(ui, |ui| {
                            uv_edit = uv_transform_controls(&mut wall.uv, ui);
                            if ui
                                .button("Autotile")
                                .on_hover_text(
                                    "Set this wall's UV span so one grid sector maps to one texture tile. Geometry is unchanged.",
                                )
                                .clicked()
                            {
                                let before = wall.uv;
                                let clamped = wall.autotile_uv(sector_size);
                                if wall.uv != before {
                                    changed = true;
                                }
                                status_message = Some(if clamped {
                                    "Autotiled wall UV span; V was clamped to the PS1 8-bit UV range"
                                        .to_string()
                                } else {
                                    "Autotiled wall UV span".to_string()
                                });
                            }
                        });
                    changed |= wall.uv != uv_before;
                    uv_edit.include_value_changes(uv_before, wall.uv);
                    if uv_edit.changed() {
                        selected_uv_change = Some((uv_edit, wall.uv));
                    }
                }
                if split_wall {
                    if let Some(wall) = walls.get(stack as usize).cloned() {
                        let segments = wall.split_into_height_segments(sector_size);
                        let replacement_count = segments.len();
                        if replacement_count > 1 {
                            walls.splice(stack as usize..=stack as usize, segments);
                            status_message =
                                Some(format!("Split wall into {replacement_count} segment(s)"));
                            changed = true;
                        } else {
                            status_message = Some("Wall did not need splitting".to_string());
                        }
                    }
                }
            }
        }

        if let Some((edit, authored)) = selected_uv_change {
            let (affected, propagated) =
                self.apply_selected_face_uv_change_no_undo(face, edit, authored);
            changed |= propagated > 0;
            if affected > 1 && status_message.is_none() {
                status_message = Some(format!("Updated UV settings on {affected} selected faces"));
            }
        }

        if let Some(message) = status_message {
            self.status = message;
        }
        if changed {
            self.mark_dirty();
        }

        // Apply any deferred nav request from the picker `→`
        // buttons. Safe here because the mutable scene borrow
        // ended at the end of the match block above.
        if let Some(target) = nav_target {
            self.clear_primitive_selection_state();
            self.replace_resource_selection(target);
            self.clear_node_selection_state();
            self.clear_sector_selection();
        }
    }
}
