use super::*;

impl EditorWorkspace {
    /// Breadcrumb crumbs for the resource inspector. A texture
    /// shows just `Texture: <name>` (we don't track which
    /// materials reference it, so there's no parent crumb to
    /// add). A material shows `Material: <name> › Texture: <name>`
    /// when its texture is set. Other resource kinds get a
    /// single self-crumb.
    pub(crate) fn resource_breadcrumb(&self, id: ResourceId) -> Vec<BreadcrumbCrumb> {
        let Some(resource) = self.project.resource(id) else {
            return Vec::new();
        };
        let label_for = |kind: &str, name: &str| format!("{kind}: {name}");
        let crumbs = vec![BreadcrumbCrumb {
            label: label_for(resource.data.label(), &resource.name),
            nav: None,
        }];
        crumbs
    }

    pub(crate) fn draw_resource_inspector(&mut self, ui: &mut egui::Ui, id: ResourceId) {
        self.refresh_texture_thumbs(ui.ctx());
        // Deferred jump-to navigation, same pattern as
        // `draw_face_inspector`. Applied after the mutable
        // resource borrow releases.
        let mut nav_target: Option<ResourceId> = None;
        // Pull the cached preview before borrowing `self.project`
        // mutably below. `texture_thumb_entry` takes `&self` and
        // walks Texture / Material → cached `.psxt` decode, so this
        // copy is the only way to keep both alive in one inspector.
        let preview_snapshot = self
            .project
            .resource(id)
            .and_then(|resource| self.texture_thumb_entry(resource))
            .map(|entry| TexturePreviewSnapshot {
                texture_id: entry.handle.id(),
                image: entry.image.clone(),
                stats: entry.stats,
            });
        let preview_thumb = preview_snapshot
            .as_ref()
            .map(|entry| (entry.texture_id, entry.stats));
        // Snapshot project_dir for path resolution inside the
        // model inspector (parses .psxmdl / .psxanim for live
        // stats display). Cloned so the mutable resource borrow
        // below doesn't fight `&self.project_dir`.
        let project_root = self.project_dir.clone();
        // Snapshot Model resources + their clip names so the
        // Character Profile inspector can populate model + clip pickers
        // without borrowing `self.project` while the mutable
        // borrow on `resource_mut` is live.
        let character_ctx = build_character_editor_context(&self.project);
        let model_options = collect_model_options(&self.project);
        let model_resource_options: Vec<(ResourceId, String)> = model_options
            .iter()
            .map(|(id, name, _)| (*id, name.clone()))
            .collect();
        let skeleton_options = collect_skeleton_options(&self.project);
        let animation_source_options = collect_animation_source_options(&self.project);
        let animation_clip_options = collect_animation_clip_options(&self.project);
        let attachment_socket_names = collect_attachment_socket_names(&self.project);
        let material_options = self.material_lab_options();

        // Build the breadcrumb before the mutable borrow on
        // `resource_mut` -- we need other resources by id to
        // resolve the material → texture link.
        let crumbs = self.resource_breadcrumb(id);

        let Some((resource_raw_id, current_name, resource_data)) =
            self.project.resource(id).map(|resource| {
                (
                    resource.id.raw(),
                    resource.name.clone(),
                    resource.data.clone(),
                )
            })
        else {
            ui.weak("Resource missing");
            return;
        };

        if !matches!(self.resource_renaming, Some((editing_id, _)) if editing_id == id) {
            self.resource_renaming = Some((id, current_name.clone()));
        }

        let mut rename_commit: Option<String> = None;
        let mut rename_cancelled = false;
        let mut transparency_key_action: Option<(ResourceId, PickedPsxtTexel)> = None;
        let mut changed = false;
        inspector_identity_header(
            ui,
            resource_lucide_icon(&resource_data),
            resource_lucide_color(&resource_data, true),
            &current_name,
            resource_data.label(),
            resource_raw_id,
        );
        draw_breadcrumb(ui, &crumbs, &mut nav_target);
        inspector_property_row(ui, "Name", |ui| {
            if let Some((_, buffer)) = &mut self.resource_renaming {
                let response =
                    ui.add(egui::TextEdit::singleline(buffer).desired_width(f32::INFINITY));
                let enter = response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let escape = response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape));
                if escape {
                    *buffer = current_name.clone();
                    rename_cancelled = true;
                } else if response.lost_focus() || enter {
                    rename_commit = Some(buffer.clone());
                }
            }
        });
        if rename_cancelled {
            self.status = "Resource rename cancelled".to_string();
        }
        if let Some(name) = rename_commit {
            self.commit_resource_rename(id, name);
        }

        if self.draw_resource_delete_controls(ui, id) {
            return;
        }

        if resource_can_open_in_animation_viewer(&resource_data)
            && ui
                .button(icons::label(icons::PLAY, "Open in Animation Viewer"))
                .on_hover_text("Preview this asset in the central animation workspace.")
                .clicked()
        {
            self.open_animation_viewer_for_resource(id);
        }

        let Some(resource) = self.project.resource_mut(id) else {
            ui.weak("Resource missing");
            return;
        };

        ui.separator();

        match &mut resource.data {
            ResourceData::Texture { psxt_path } => {
                if let Some(pick) = draw_psxt_preview_block_pickable(ui, preview_snapshot.as_ref())
                {
                    transparency_key_action = Some((id, pick));
                }
                egui::CollapsingHeader::new(icons::label(icons::FILE, "PSXT"))
                    .default_open(true)
                    .show(ui, |ui| {
                        ui.label("Path");
                        changed |= ui.text_edit_singleline(psxt_path).changed();
                        ui.label(
                            RichText::new("Cooked .psxt blob; same artifact the runtime embeds.")
                                .color(STUDIO_TEXT_WEAK)
                                .small(),
                        );
                    });
                if let Some((_, stats)) = preview_thumb {
                    egui::CollapsingHeader::new(icons::label(icons::SCAN, "Info"))
                        .default_open(true)
                        .show(ui, |ui| {
                            draw_psxt_stats(ui, stats);
                        });
                }
            }
            ResourceData::Material(material) => {
                if let Some(pick) = draw_psxt_preview_block_pickable(ui, preview_snapshot.as_ref())
                {
                    if material.psxt_path.is_some() {
                        transparency_key_action = Some((id, pick));
                    }
                }
                egui::CollapsingHeader::new(icons::label(icons::BLEND, "Material"))
                    .default_open(true)
                    .show(ui, |ui| {
                        changed |= crate::material_lab::draw_material_settings(
                            ui,
                            "resource_material",
                            material,
                            &material_options,
                            Some(id),
                        );
                    });
                if let Some((_, stats)) = preview_thumb {
                    egui::CollapsingHeader::new(icons::label(icons::SCAN, "Linked Texture"))
                        .default_open(false)
                        .show(ui, |ui| {
                            draw_psxt_stats(ui, stats);
                        });
                }
            }
            ResourceData::Model(model) => {
                changed |= draw_model_resource_editor(
                    ui,
                    model,
                    &project_root,
                    preview_thumb,
                    &skeleton_options,
                    &mut self.model_resource_preview_texture,
                );
            }
            ResourceData::Skeleton(skeleton) => {
                changed |= draw_skeleton_resource_editor(ui, skeleton);
            }
            ResourceData::AnimationSource(source) => {
                changed |= draw_animation_source_resource_editor(
                    ui,
                    source,
                    &project_root,
                    &skeleton_options,
                    &model_resource_options,
                );
            }
            ResourceData::AnimationClip(clip) => {
                changed |= draw_animation_clip_resource_editor(
                    ui,
                    clip,
                    &project_root,
                    &skeleton_options,
                    &model_resource_options,
                    &animation_source_options,
                );
            }
            ResourceData::AnimationSet(set) => {
                changed |= draw_animation_set_resource_editor(
                    ui,
                    set,
                    &skeleton_options,
                    &animation_clip_options,
                );
            }
            ResourceData::Character(character) => {
                changed |= draw_character_resource_editor(ui, character, &character_ctx);
            }
            ResourceData::Weapon(weapon) => {
                changed |= draw_weapon_resource_editor(
                    ui,
                    weapon,
                    &model_options,
                    &attachment_socket_names,
                );
            }
            ResourceData::BoostModule(module) => {
                egui::CollapsingHeader::new(icons::label(icons::FOCUS, "Boost Module"))
                    .default_open(true)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Protocol").color(STUDIO_TEXT_WEAK));
                            egui::ComboBox::from_id_salt("boost-module-kind")
                                .selected_text(module.kind.label())
                                .show_ui(ui, |ui| {
                                    for kind in psxed_project::BoostModuleKind::ALL {
                                        changed |= ui
                                            .selectable_value(
                                                &mut module.kind,
                                                kind,
                                                kind.label(),
                                            )
                                            .changed();
                                    }
                                });
                        });
                        ui.label(
                            RichText::new(
                                "Collectable module granted by a Point of Interest and assigned in the Player menu.",
                            )
                            .small()
                            .color(STUDIO_TEXT_WEAK),
                        );
                    });
            }
            ResourceData::Mesh { source_path }
            | ResourceData::Scene { source_path }
            | ResourceData::Script { source_path }
            | ResourceData::Audio { source_path } => {
                egui::CollapsingHeader::new(icons::label(icons::FILE, "Import"))
                    .default_open(true)
                    .show(ui, |ui| {
                        ui.label("Source");
                        changed |= ui.text_edit_singleline(source_path).changed();
                    });
            }
        }

        if changed {
            self.mark_dirty();
            if resource_can_open_in_animation_viewer(&resource_data) {
                self.animation_viewer.focus_resource(&self.project, id);
                self.animation_viewer_preview_texture = None;
            }
            // Resource edits land after the central preview was drawn. Force
            // the follow-up frame that reloads the edited clip/model/source.
            ui.ctx().request_repaint();
        }

        if let Some((texture_id, pick)) = transparency_key_action {
            self.apply_texture_transparency_key(ui.ctx(), texture_id, pick);
        }

        // Apply deferred nav so the user can drill straight
        // into the linked texture.
        if let Some(target) = nav_target {
            self.replace_resource_selection(target);
            self.clear_node_selection_state();
            self.clear_primitive_selection_state();
            self.clear_sector_selection();
        }
    }

    pub(crate) fn draw_content_browser(&mut self, ctx: &egui::Context) {
        if !self.resources_open {
            return;
        }
        // Refresh PSXT thumbnail handles up-front so the resource
        // cards rendered below have something to blit instead of the
        // name-keyword procedural fallback. Cheap when nothing's
        // changed -- the signature cache short-circuits per-resource.
        if self.content_browser_view == ContentBrowserView::Resources {
            self.refresh_texture_thumbs(ctx);
        }
        let max_height = max_resizable_bottom_dock_height(ctx);
        egui::TopBottomPanel::bottom("psxed_content_browser")
            .resizable(true)
            .default_height(240.0)
            .min_height(CONTENT_BROWSER_MIN_HEIGHT)
            .max_height(max_height)
            .frame(dock_frame())
            .show(ctx, |ui| {
                fixed_panel_content(ui, "psxed_content_browser_fixed_content", |ui| {
                    let content_width = ui.available_width().max(1.0);
                    ui.set_width(content_width);
                    tool_panel_frame().show(ui, |ui| {
                        self.draw_resource_panel_header(ui);
                        tool_panel_body(ui, |ui| {
                            let content_width = ui.available_width().max(1.0);
                            ui.set_width(content_width);
                            match self.content_browser_view {
                                ContentBrowserView::Resources => self.draw_resources_tab(ui),
                                ContentBrowserView::Debug => self.draw_debug_terminal_tab(ui),
                            }
                        });
                    });
                });
            });
    }

    pub(crate) fn draw_resource_panel_header(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .fill(STUDIO_PANEL_HEADER)
            .inner_margin(panel_header_margin())
            .show(ui, |ui| {
                ui.set_min_height(PANEL_HEADER_MIN_HEIGHT);
                ui.horizontal(|ui| {
                    ui.label(
                        icons::text(
                            match self.content_browser_view {
                                ContentBrowserView::Resources => icons::LAYERS,
                                ContentBrowserView::Debug => icons::TERMINAL,
                            },
                            15.0,
                        )
                        .color(STUDIO_ACCENT),
                    );
                    ui.selectable_value(
                        &mut self.content_browser_view,
                        ContentBrowserView::Resources,
                        "Resources",
                    );
                    ui.selectable_value(
                        &mut self.content_browser_view,
                        ContentBrowserView::Debug,
                        "Console",
                    );
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| match self.content_browser_view {
                            ContentBrowserView::Resources => self.draw_resource_panel_actions(ui),
                            ContentBrowserView::Debug => self.draw_debug_terminal_actions(ui),
                        },
                    );
                });
            });
    }

    pub(crate) fn draw_debug_terminal_actions(&mut self, ui: &mut egui::Ui) {
        if ui
            .add(egui::Button::new(icons::text(icons::COPY, 14.0)).min_size(Vec2::new(28.0, 24.0)))
            .on_hover_text("Copy all build and guest console lines")
            .clicked()
        {
            ui.ctx().copy_text(self.play_debug_terminal_text());
            self.status = "Copied console output".to_string();
        }
        if ui
            .add(egui::Button::new(icons::text(icons::TRASH, 14.0)).min_size(Vec2::new(28.0, 24.0)))
            .on_hover_text("Clear the build and guest console")
            .clicked()
        {
            self.play_debug_terminal_lines.clear();
            self.status = "Cleared console output".to_string();
        }
    }

    pub(crate) fn draw_resource_panel_actions(&mut self, ui: &mut egui::Ui) {
        ui.menu_button(icons::text(icons::PLUS, 14.0), |ui| {
            ui.set_min_width(220.0);

            if ui
                .button(icons::label(icons::FOCUS, "Boost Module"))
                .on_hover_text("Add a collectable vitality boost module.")
                .clicked()
            {
                let id = self.project.add_resource(
                    "New Boost Module",
                    ResourceData::BoostModule(psxed_project::BoostModuleResource::default()),
                );
                self.replace_resource_selection(id);
                self.clear_node_selection_state();
                self.clear_primitive_selection_state();
                self.clear_sector_selection();
                self.status = "Added boost module".to_string();
                self.mark_dirty();
                ui.close_menu();
            }
            if ui
                .button(icons::label(icons::WAYPOINT, "Weapon"))
                .on_hover_text("Add a Weapon resource with a grip and hitbox.")
                .clicked()
            {
                let id = self.project.add_resource(
                    "New Weapon",
                    ResourceData::Weapon(psxed_project::WeaponResource::default()),
                );
                self.replace_resource_selection(id);
                self.clear_node_selection_state();
                self.clear_primitive_selection_state();
                self.clear_sector_selection();
                self.status = "Added weapon".to_string();
                self.mark_dirty();
                ui.close_menu();
            }
            if ui
                .button(icons::label(icons::MAP_PIN, "Character Profile"))
                .on_hover_text(
                    "Add reusable movement, animation-role, capsule, and camera defaults for character entities.",
                )
                .clicked()
            {
                let id = self.project.add_resource(
                    "New Character Profile",
                    ResourceData::Character(psxed_project::CharacterResource::default()),
                );
                self.replace_resource_selection(id);
                self.clear_node_selection_state();
                self.clear_primitive_selection_state();
                self.clear_sector_selection();
                self.status = "Added character profile".to_string();
                self.mark_dirty();
                ui.close_menu();
            }
            if ui
                .button(icons::label(icons::PLAY, "Animation Source"))
                .on_hover_text(
                    "Add an authoring-time animation library entry. Sources are previewed or baked before runtime.",
                )
                .clicked()
            {
                let id = self.project.add_resource(
                    "New Animation Source",
                    ResourceData::AnimationSource(psxed_project::AnimationSourceResource::from_path(
                        "",
                        "",
                    )),
                );
                self.replace_resource_selection(id);
                self.clear_node_selection_state();
                self.clear_primitive_selection_state();
                self.clear_sector_selection();
                self.status = "Added animation source".to_string();
                self.mark_dirty();
                ui.close_menu();
            }
            if ui
                .button(icons::label(icons::PLAY, "Clip Role Map"))
                .on_hover_text(
                    "Add a reusable idle/walk/run/turn mapping for compatible animation clips.",
                )
                .clicked()
            {
                let id = self.project.add_resource(
                    "New Clip Role Map",
                    ResourceData::AnimationSet(psxed_project::AnimationSetResource::default()),
                );
                self.replace_resource_selection(id);
                self.clear_node_selection_state();
                self.clear_primitive_selection_state();
                self.clear_sector_selection();
                self.status = "Added clip role map".to_string();
                self.mark_dirty();
                ui.close_menu();
            }
            if ui
                .button(icons::label(icons::BLEND, "Material"))
                .on_hover_text("Add a new Material resource.")
                .clicked()
            {
                let id = self.project.add_resource(
                    "New Material",
                    ResourceData::Material(MaterialResource::opaque(None)),
                );
                self.replace_resource_selection(id);
                self.clear_node_selection_state();
                self.clear_primitive_selection_state();
                self.clear_sector_selection();
                self.status = "Added material".to_string();
                self.mark_dirty();
                ui.close_menu();
            }
            if ui
                .button(icons::label(icons::BLEND, "Transition Material"))
                .on_hover_text("Add a host-baked transition between two Material images.")
                .clicked()
            {
                let mut material = MaterialResource::opaque(None);
                material.texture_mode = MaterialTextureMode::Transition;
                material.transition.source_a = self.selection.selected_resource.filter(|id| {
                    self.project
                        .resource(*id)
                        .is_some_and(|resource| matches!(resource.data, ResourceData::Material(_)))
                });
                let id = self.project.add_resource(
                    "New Transition",
                    ResourceData::Material(material),
                );
                self.replace_resource_selection(id);
                self.material_lab.focused_material = Some(id);
                self.clear_node_selection_state();
                self.clear_primitive_selection_state();
                self.clear_sector_selection();
                self.status = "Added transition material".to_string();
                self.mark_dirty();
                ui.close_menu();
            }

            ui.separator();

            if ui
                .button(icons::label(icons::FILE_PLUS, "Import Model"))
                .on_hover_text(
                    "Open the GLB/glTF/FBX model import preview with atlas, clip, and root-centering controls.",
                )
                .clicked()
            {
                self.open_model_import_dialog();
                ui.close_menu();
            }
            if ui
                .button(icons::label(icons::FILE_PLUS, "Import Texture"))
                .on_hover_text("Open the PNG/JPG/BMP texture import preview with PSXT cook settings.")
                .clicked()
            {
                self.open_texture_import_dialog();
                ui.close_menu();
            }

            ui.separator();

            if ui
                .button(icons::label(icons::SCAN, "Catalogue Animation Source Folder"))
                .on_hover_text(
                    "Catalogue raw FBX/GLB animation source files without copying them into the project.",
                )
                .clicked()
            {
                self.catalogue_animation_source_folder();
                ui.close_menu();
            }
            if ui
                .button(icons::label(icons::SCAN, "Catalogue Animation Source Zip"))
                .on_hover_text(
                    "Catalogue raw FBX/GLB animation sources inside a zip without extracting them.",
                )
                .clicked()
            {
                self.catalogue_animation_source_zip();
                ui.close_menu();
            }
            if ui
                .button(icons::label(icons::PLAY, "Animation Viewer"))
                .on_hover_text("Open the central model and animation playback workspace.")
                .clicked()
            {
                self.open_animation_viewer_for_current_selection();
                ui.close_menu();
            }
            if ui
                .button(icons::label(icons::MAP_PIN, "Starter Content"))
                .on_hover_text(
                    "Sync the built-in player/enemy models, animations, profiles and saved material library into this project.",
                )
                .clicked()
            {
                self.push_undo();
                match sync_starter_character_catalogue(&mut self.project, &self.project_dir) {
                    Ok(report) => {
                        self.status = format!(
                            "Synced starter content: {} added, {} updated, {} removed, {} file(s) copied, {} file(s) removed",
                            report.resources_added,
                            report.resources_updated,
                            report.resources_removed,
                            report.files_copied,
                            report.files_removed
                        );
                        if report.changed() {
                            self.mark_dirty();
                        }
                    }
                    Err(error) => {
                        self.status = format!("Starter content sync failed: {error}");
                    }
                }
                ui.close_menu();
            }
        })
        .response
        .on_hover_text("Add, import, or sync resources");
    }

    pub(crate) fn play_debug_terminal_text(&self) -> String {
        let mut out = String::new();
        for line in &self.play_debug_terminal_lines {
            let _ = writeln!(out, "{line}");
        }
        out
    }

    pub(crate) fn draw_debug_terminal_tab(&mut self, ui: &mut egui::Ui) {
        let terminal_width = ui.available_width().max(1.0);
        let terminal_height = ui.available_height().max(1.0);
        egui::Frame::new()
            .fill(Color32::from_black_alpha(170))
            .stroke(Stroke::new(1.0, STUDIO_BORDER))
            .corner_radius(egui::CornerRadius::same(4))
            .inner_margin(egui::Margin::symmetric(8, 7))
            .show(ui, |ui| {
                ui.set_min_width((terminal_width - 16.0).max(1.0));
                ui.set_min_height((terminal_height - 14.0).max(1.0));
                if self.play_debug_terminal_lines.is_empty() {
                    ui.weak("Build and guest output will appear here.");
                    return;
                }
                egui::ScrollArea::vertical()
                    .id_salt("psxed_play_debug_terminal")
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .max_height(ui.available_height().max(1.0))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width().max(1.0));
                        let mut terminal_text = self.play_debug_terminal_text();
                        let rows = self.play_debug_terminal_lines.len().max(1);
                        ui.add(
                            egui::TextEdit::multiline(&mut terminal_text)
                                .font(egui::TextStyle::Monospace)
                                .desired_width(f32::INFINITY)
                                .desired_rows(rows)
                                .frame(false)
                                .margin(egui::Margin::same(0))
                                .interactive(true),
                        )
                        .on_hover_text("Select log text and press Ctrl+C or Cmd+C to copy it.");
                    });
            });
    }

    /// Walk every Texture resource and ensure its `.psxt` blob has
    /// been decoded into an egui texture handle the resource cards
    /// can blit. Skips entries whose `psxt_path` matches the cached
    /// signature; rebuilds when the path moves or the file is newly
    /// readable.
    pub(crate) fn refresh_texture_thumbs(&mut self, ctx: &egui::Context) {
        // Snapshot resource id + path first so cache mutation below
        // cannot fight the immutable project-resource walk.
        let project_root = self.project_dir.clone();
        let sources: Vec<(ResourceId, String, Option<String>, Option<[u8; 3]>)> = self
            .project
            .resources
            .iter()
            .filter_map(|resource| {
                // Texture resources point straight at a `.psxt`;
                // Model resources have a `texture_path` field -- both
                // share the same on-disk format and decoder, so the
                // thumbnail cache treats them uniformly.
                match &resource.data {
                    ResourceData::Texture { psxt_path } => Some((
                        resource.id,
                        psxt_path.clone(),
                        Some(psxt_path.clone()),
                        None,
                    )),
                    ResourceData::Material(material) => Some((
                        resource.id,
                        material_thumbnail_signature(&self.project, resource.id),
                        None,
                        Some(material.tint),
                    )),
                    ResourceData::Model(model) => {
                        let psxt_path = model.texture_path.as_ref()?;
                        Some((
                            resource.id,
                            psxt_path.clone(),
                            Some(psxt_path.clone()),
                            None,
                        ))
                    }
                    _ => None,
                }
            })
            .collect();
        let alive: HashSet<ResourceId> = sources.iter().map(|(id, _, _, _)| *id).collect();
        for (id, signature, psxt_path, material_tint) in sources {
            if let Some(entry) = self.texture_thumbs.get(&id) {
                if entry.signature == signature {
                    continue;
                }
            }
            if psxt_path.as_deref().is_some_and(str::is_empty) {
                self.remove_texture_thumb(id);
                continue;
            }
            let bytes = if let Some(psxt_path) = psxt_path {
                let abs = if Path::new(psxt_path.as_str()).is_absolute() {
                    PathBuf::from(psxt_path.as_str())
                } else {
                    project_root.join(psxt_path.as_str())
                };
                std::fs::read(&abs).ok()
            } else {
                psxed_project::resolve_material_texture_psxt(&self.project, id, &project_root)
                    .ok()
                    .flatten()
                    .map(|(_, bytes)| bytes)
            };
            let Some((mut image, stats)) = bytes.as_deref().and_then(decode_psxt_thumbnail) else {
                self.remove_texture_thumb(id);
                continue;
            };
            if let Some(tint) = material_tint {
                modulate_ps1_thumbnail(&mut image, tint);
            }
            self.set_texture_thumb(ctx, id, signature, image, stats);
        }
        // Drop entries for Texture / Model resources that no longer
        // exist -- keeps the cache from growing across delete + re-add.
        let stale: Vec<ResourceId> = self
            .texture_thumbs
            .keys()
            .copied()
            .filter(|id| !alive.contains(id))
            .collect();
        for id in stale {
            self.remove_texture_thumb(id);
        }
    }

    pub(crate) fn set_texture_thumb(
        &mut self,
        ctx: &egui::Context,
        id: ResourceId,
        signature: String,
        image: ColorImage,
        stats: PsxtStats,
    ) {
        if let Some(entry) = self.texture_thumbs.get_mut(&id) {
            entry
                .handle
                .set(image.clone(), egui::TextureOptions::NEAREST);
            entry.signature = signature;
            entry.image = image;
            entry.stats = stats;
        } else {
            let handle = ctx.load_texture(
                format!("psxt-thumb-{}", id.raw()),
                image.clone(),
                egui::TextureOptions::NEAREST,
            );
            self.texture_thumbs.insert(
                id,
                ThumbnailEntry {
                    signature,
                    handle,
                    image,
                    stats,
                },
            );
        }
    }

    pub(crate) fn apply_texture_transparency_key(
        &mut self,
        ctx: &egui::Context,
        texture_id: ResourceId,
        pick: PickedPsxtTexel,
    ) {
        let Some(resource) = self.project.resource(texture_id) else {
            self.status = "Texture resource missing".to_string();
            return;
        };
        let psxt_path = match &resource.data {
            ResourceData::Texture { psxt_path } => psxt_path.clone(),
            _ => {
                self.status = "Selected resource is not a Texture".to_string();
                return;
            }
        };
        if psxt_path.is_empty() {
            self.status = "Texture path is empty".to_string();
            return;
        }
        let abs = psxed_project::model_import::resolve_path(&psxt_path, Some(&self.project_dir));
        let mut bytes = match std::fs::read(&abs) {
            Ok(bytes) => bytes,
            Err(err) => {
                self.status = format!("Could not read {}: {err}", abs.display());
                return;
            }
        };
        let report = match psxed_project::texture_import::apply_texel_color_key_transparency(
            &mut bytes, pick.x, pick.y,
        ) {
            Ok(report) => report,
            Err(err) => {
                self.status = format!("Transparency key failed: {err}");
                return;
            }
        };
        if let Err(err) = std::fs::write(&abs, &bytes) {
            self.status = format!("Could not write {}: {err}", abs.display());
            return;
        }
        if let Some((image, stats)) = decode_psxt_thumbnail(&bytes) {
            self.set_texture_thumb(ctx, texture_id, psxt_path, image, stats);
        } else {
            self.remove_texture_thumb(texture_id);
        }
        self.status = format!(
            "Transparent key #{:02X} RGB({},{},{}) rewrote {} texel(s) from click RGB({},{},{})",
            report.picked_index,
            report.picked_rgb[0],
            report.picked_rgb[1],
            report.picked_rgb[2],
            report.rewritten_texels,
            pick.color.r(),
            pick.color.g(),
            pick.color.b()
        );
    }

    pub(crate) fn remove_texture_thumb(&mut self, id: ResourceId) {
        if let Some(entry) = self.texture_thumbs.remove(&id) {
            self.retire_egui_texture(entry.handle);
        }
    }

    /// Resolve the underlying Texture id for a Material, or the
    /// Texture's own id if `resource` is one. `None` for everything
    /// else.
    pub(crate) fn texture_thumb_id(&self, resource: &Resource) -> Option<egui::TextureId> {
        self.texture_thumb_entry(resource).map(|e| e.handle.id())
    }

    /// Look up the cached thumbnail entry (handle + stats) for a
    /// Texture resource directly, or for a Material via its texture
    /// link. `None` when the link is unset, the file isn't readable,
    /// or the PSXT blob cannot be decoded.
    pub(crate) fn texture_thumb_entry(&self, resource: &Resource) -> Option<&ThumbnailEntry> {
        let key = match &resource.data {
            ResourceData::Texture { .. } | ResourceData::Material(_) => Some(resource.id),
            _ => None,
        }?;
        self.texture_thumbs.get(&key)
    }

    pub(crate) fn draw_resources_tab(&mut self, ui: &mut egui::Ui) {
        let tab_height = ui.available_height().max(1.0);
        ui.horizontal(|ui| {
            section_frame().show(ui, |ui| {
                // Frame inherits the outer `ui.horizontal` layout, so
                // every child widget would otherwise flow on a single
                // row. Force vertical so the filter buttons stack as
                // intended.
                ui.vertical(|ui| {
                    ui.set_width(180.0);
                    ui.set_min_height((tab_height - 14.0).max(1.0));
                    panel_heading(ui, icons::SCAN, "Filter");
                    ui.add_space(2.0);
                    // Keep the heading pinned while categories scroll on a
                    // short editor window.
                    egui::ScrollArea::vertical()
                        .auto_shrink([false; 2])
                        .show(ui, |ui| {
                            ui.selectable_value(
                                &mut self.resource_filter,
                                ResourceFilter::All,
                                icons::label(ResourceFilter::All.icon(), "All"),
                            );
                            for (filter, count) in resource_filter_counts(&self.project) {
                                ui.selectable_value(
                                    &mut self.resource_filter,
                                    filter,
                                    format!(
                                        "{} ({count})",
                                        icons::label(filter.icon(), filter.label())
                                    ),
                                );
                            }
                        });
                });
            });

            ui.add_space(4.0);

            ui.vertical(|ui| {
                let pane_width = ui.available_width().max(1.0);
                ui.set_width(pane_width);
                let search_width = pane_width;
                ui.add(
                    egui::TextEdit::singleline(&mut self.resource_search)
                        .hint_text("Filter resources")
                        .desired_width(search_width),
                );
                let mut clicked = None;
                let search = self.resource_search.to_ascii_lowercase();
                let visible_items: Vec<ResourceId> = self
                    .project
                    .resources
                    .iter()
                    .filter(|resource| {
                        resource_matches_filter(resource, self.resource_filter, search.as_str())
                    })
                    .map(|resource| resource.id)
                    .collect();
                let cards_height = ui.available_height().max(1.0);
                let card_spacing = ui.spacing().item_spacing;
                let columns = ((pane_width + card_spacing.x)
                    / (RESOURCE_CARD_WIDTH + card_spacing.x))
                    .floor()
                    .max(1.0) as usize;
                egui::ScrollArea::vertical()
                    .id_salt("psxed_resource_card_grid")
                    .max_width(pane_width)
                    .max_height(cards_height)
                    .min_scrolled_height(cards_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_min_width(pane_width);
                        if visible_items.is_empty() {
                            section_frame().show(ui, |ui| {
                                ui.set_width((pane_width - 20.0).max(1.0));
                                ui.label(
                                    RichText::new("No matching resources")
                                        .strong()
                                        .color(STUDIO_TEXT),
                                );
                                ui.label(
                                    RichText::new(
                                        "Adjust the resource type or clear the search filter.",
                                    )
                                    .small()
                                    .color(STUDIO_TEXT_WEAK),
                                );
                            });
                            return;
                        }
                        egui::Grid::new("psxed_resource_card_grid_layout")
                            .num_columns(columns)
                            .spacing(card_spacing)
                            .show(ui, |ui| {
                                let mut column = 0usize;
                                for &id in &visible_items {
                                    let Some(resource) = self.project.resource(id) else {
                                        continue;
                                    };
                                    let thumb = self.texture_thumb_id(resource);
                                    let response = draw_resource_card(
                                        ui,
                                        &self.project,
                                        resource,
                                        self.resource_is_selected(resource.id),
                                        thumb,
                                    );
                                    if response.clicked() {
                                        clicked = Some(ResourceClick {
                                            id: resource.id,
                                            modifiers: ui.input(|input| input.modifiers),
                                        });
                                    }
                                    column += 1;
                                    if column == columns {
                                        ui.end_row();
                                        column = 0;
                                    }
                                }
                            });
                    });
                if let Some(click) = clicked {
                    // Sims-style: with a face selected, clicking a
                    // Material card retargets the selected face set's
                    // material rather than swapping the inspector.
                    // Box props also accept plain Material / Texture
                    // clicks; other clicks still navigate normally.
                    let id = click.id;
                    let is_material = matches!(
                        self.project.resource(id).map(|r| &r.data),
                        Some(ResourceData::Material(_))
                    );
                    let plain_click =
                        !click.modifiers.shift && !click.modifiers.ctrl && !click.modifiers.command;
                    let selected_targets = if is_material && plain_click {
                        self.selected_material_targets()
                    } else {
                        Vec::new()
                    };
                    if !selected_targets.is_empty() {
                        let updated = self.assign_selected_faces_material(Some(id));
                        match (selected_targets.as_slice(), updated) {
                            (_, 0) => {
                                self.status =
                                    "Material already assigned to selected surfaces".to_string();
                            }
                            ([target], 1) => {
                                self.status = format!(
                                    "Assigned material to {}",
                                    describe_material_target(*target)
                                );
                            }
                            (_, n) if n == selected_targets.len() => {
                                self.status = format!("Assigned material to {n} selected surfaces");
                            }
                            (_, n) => {
                                self.status = format!(
                                    "Assigned material to {n}/{} selected surfaces",
                                    selected_targets.len()
                                );
                            }
                        }
                        self.replace_resource_selection(id);
                    } else if !self.apply_selected_box_prop_resource_click(click) {
                        self.apply_resource_selection_modifiers(
                            id,
                            click.modifiers,
                            &visible_items,
                        );
                    }
                }
            });
        });
    }

    pub(crate) fn open_animation_viewer_for_current_selection(&mut self) {
        if let Some(resource_id) = self.selection.selected_resource {
            if self.open_animation_viewer_for_resource(resource_id) {
                return;
            }
        }
        let character_resource = self
            .project
            .active_scene()
            .node(self.selection.selected_node)
            .and_then(|node| entity_character_resource_id(self, node));
        if let Some(resource_id) = character_resource {
            if self.open_animation_viewer_for_resource(resource_id) {
                return;
            }
        }
        self.active_workspace = WorkspaceView::Animation;
        self.status = "Opened Animation Viewer".to_string();
    }

    /// Open Animation Studio focused on a model, character, or animation
    /// resource. Returns `false` when the resource cannot be previewed there.
    pub fn open_animation_viewer_for_resource(&mut self, resource_id: ResourceId) -> bool {
        let can_open = self
            .project
            .resource(resource_id)
            .is_some_and(|resource| resource_can_open_in_animation_viewer(&resource.data));
        if !can_open {
            return false;
        }
        self.animation_viewer
            .focus_resource(&self.project, resource_id);
        self.active_workspace = WorkspaceView::Animation;
        self.status = self
            .project
            .resource_name(resource_id)
            .map(|name| format!("Opened {name} in Animation Viewer"))
            .unwrap_or_else(|| "Opened Animation Viewer".to_string());
        true
    }
}

fn material_thumbnail_signature(project: &ProjectDocument, id: ResourceId) -> String {
    fn append(
        project: &ProjectDocument,
        id: ResourceId,
        stack: &mut Vec<ResourceId>,
        out: &mut String,
    ) {
        if stack.contains(&id) {
            out.push_str(&format!("cycle#{}", id.raw()));
            return;
        }
        let Some(resource) = project.resource(id) else {
            out.push_str(&format!("missing#{}", id.raw()));
            return;
        };
        out.push_str(&format!("{}:{:?}", resource.id.raw(), resource.data));
        let ResourceData::Material(material) = &resource.data else {
            return;
        };
        if material.texture_mode != MaterialTextureMode::Transition {
            return;
        }
        stack.push(id);
        for source in [material.transition.source_a, material.transition.source_b]
            .into_iter()
            .flatten()
        {
            append(project, source, stack, out);
        }
        stack.pop();
    }

    let mut signature = String::new();
    append(project, id, &mut Vec::new(), &mut signature);
    signature
}
