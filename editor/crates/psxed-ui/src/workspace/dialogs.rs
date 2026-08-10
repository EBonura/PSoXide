use super::*;

impl EditorWorkspace {
    /// Draw the full editor workspace.
    ///
    /// `viewport_3d` describes what texture the central 3D viewport
    /// should paint this frame: editable authoring preview or live
    /// embedded playtest output.
    pub fn draw(
        &mut self,
        ctx: &egui::Context,
        viewport_3d: EditorViewport3dPresentation,
        playtest_status: EditorPlaytestStatus,
    ) {
        apply_studio_visuals(ctx);
        if self.character_motion_preview().is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        self.import_retired_textures.retain_mut(|(frames, _)| {
            if *frames == 0 {
                false
            } else {
                *frames -= 1;
                true
            }
        });
        let playtest_captured = matches!(
            playtest_status,
            EditorPlaytestStatus::Running {
                input_captured: true
            }
        );
        if !playtest_captured {
            self.handle_global_shortcuts(ctx, playtest_status);
        }
        let play_metrics = viewport_3d.play_metrics;
        let camera_preview = viewport_3d.camera_preview;
        self.draw_action_bar(ctx, playtest_status, play_metrics);
        self.draw_left_dock(ctx);
        self.draw_inspector(ctx, camera_preview);
        self.draw_content_browser(ctx);
        self.draw_viewport(ctx, viewport_3d, playtest_status);
        self.draw_new_project_dialog(ctx);
        self.draw_delete_project_dialog(ctx);
        self.draw_texture_import_dialog(ctx);
        self.draw_model_import_dialog(ctx);
    }

    /// Modal for the File → New Project flow. Pops over the editor
    /// when the active [`Modal`] is `NewProject`; submit calls
    /// [`Self::create_and_open_project`] and re-targets the
    /// workspace at the new directory.
    pub(crate) fn draw_new_project_dialog(&mut self, ctx: &egui::Context) {
        let Modal::NewProject {
            name,
            cook_mode,
            error,
        } = &mut self.modal
        else {
            return;
        };
        let mut close = false;
        let mut submit = false;
        egui::Window::new(icons::label(icons::FILE_PLUS, "New Project"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_min_width(360.0);
                ui.label("Project name");
                let response = ui.add(egui::TextEdit::singleline(name).hint_text("e.g. Test Room"));
                let preview_stem = if name.trim().is_empty() {
                    "<name>".to_string()
                } else {
                    psxed_project::project_file_stem(name.trim())
                };
                ui.label(
                    RichText::new(format!("→ editor/projects/{}/", preview_stem))
                        .color(STUDIO_TEXT_WEAK)
                        .small(),
                );
                ui.add_space(8.0);
                ui.label("BSP cook quality");
                for mode in psxed_project::brush_world::BrushWorldCookMode::ALL {
                    ui.radio_value(cook_mode, mode, mode.label())
                        .on_hover_text(mode.description());
                }
                ui.label(
                    RichText::new(cook_mode.description())
                        .color(STUDIO_TEXT_WEAK)
                        .small(),
                );
                if let Some(error) = error.as_ref() {
                    ui.label(RichText::new(error).color(Color32::from_rgb(0xE0, 0x60, 0x60)));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    if ui.button("Create").clicked() {
                        submit = true;
                    }
                });
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close = true;
                }
            });
        if submit {
            if let Modal::NewProject {
                name, cook_mode, ..
            } = &self.modal
            {
                let name = name.clone();
                let cook_mode = *cook_mode;
                match self.create_and_open_project_with_mode(&name, cook_mode) {
                    Ok(()) => self.modal = Modal::None,
                    Err(error) => {
                        if let Modal::NewProject { error: slot, .. } = &mut self.modal {
                            *slot = Some(error);
                        }
                    }
                }
            }
        }
        if close {
            self.modal = Modal::None;
        }
    }

    pub(crate) fn draw_delete_project_dialog(&mut self, ctx: &egui::Context) {
        let error_text = match &self.modal {
            Modal::DeleteProject { error } => error.clone(),
            _ => return,
        };
        let mut close = false;
        let mut confirm = false;
        let project_name = self.project.name.clone();
        let project_dir = self.project_dir.display().to_string();
        egui::Window::new("Delete Project")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_min_width(420.0);
                ui.label(RichText::new(format!("Delete \"{project_name}\"?")).strong());
                ui.label(
                    RichText::new(format!("This removes {project_dir}"))
                        .color(STUDIO_TEXT_WEAK)
                        .small(),
                );
                ui.label(RichText::new("This cannot be undone.").color(STUDIO_TEXT_WEAK));
                if let Some(error) = &error_text {
                    ui.add_space(6.0);
                    ui.label(RichText::new(error).color(Color32::from_rgb(0xE0, 0x60, 0x60)));
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    if ui
                        .add(egui::Button::new("Delete").fill(Color32::from_rgb(0x65, 0x1F, 0x1F)))
                        .clicked()
                    {
                        confirm = true;
                    }
                });
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close = true;
                }
            });
        if confirm {
            match self.delete_current_project() {
                Ok(()) => self.modal = Modal::None,
                Err(error) => self.modal = Modal::DeleteProject { error: Some(error) },
            }
        }
        if close {
            self.modal = Modal::None;
        }
    }

    pub(crate) fn draw_texture_import_dialog(&mut self, ctx: &egui::Context) {
        if !self.texture_import_dialog.open {
            return;
        }

        enum Action {
            BrowseSource,
            AutoPreview,
            Import,
            Close,
        }

        let before_preview_key = self.texture_import_preview_key();
        let mut action: Option<Action> = None;
        let dialog = &mut self.texture_import_dialog;
        egui::Window::new(icons::label(icons::FILE_PLUS, "Import Texture"))
            .collapsible(false)
            .resizable(true)
            .default_width(920.0)
            .default_height(620.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_min_size(Vec2::new(780.0, 520.0));
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(300.0);
                        ui.label(RichText::new("Source").strong());
                        ui.label(
                            RichText::new("PNG/JPG/BMP path")
                                .color(STUDIO_TEXT_WEAK)
                                .small(),
                        );
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut dialog.source_path)
                                    .desired_width(210.0),
                            );
                            if ui
                                .button(icons::label(icons::FOLDER, "Browse"))
                                .on_hover_text("Choose a PNG, JPG, or BMP image")
                                .clicked()
                            {
                                action = Some(Action::BrowseSource);
                            }
                        });
                        ui.label(RichText::new("Resource name").color(STUDIO_TEXT_WEAK).small());
                        ui.text_edit_singleline(&mut dialog.output_name);
                        if dialog.output_name.trim().is_empty() {
                            ui.label(
                                RichText::new("Uses the source file name when blank.")
                                    .color(STUDIO_TEXT_WEAK)
                                    .small(),
                            );
                        }

                        ui.separator();
                        ui.label(RichText::new("Cook Settings").strong());
                        egui::ComboBox::from_label("Target resolution")
                            .selected_text(texture_import_resolution_label(
                                dialog.width,
                                dialog.height,
                            ))
                            .show_ui(ui, |ui| {
                                for size in TEXTURE_IMPORT_RESOLUTION_PRESETS {
                                    let selected = dialog.width == size && dialog.height == size;
                                    if ui
                                        .selectable_label(selected, format!("{size} x {size}"))
                                        .clicked()
                                    {
                                        dialog.width = size;
                                        dialog.height = size;
                                    }
                                }
                                ui.separator();
                                ui.label(
                                    RichText::new("Use the fields below for custom dimensions.")
                                        .color(STUDIO_TEXT_WEAK)
                                        .small(),
                                );
                            });
                        ui.horizontal(|ui| {
                            ui.label("Width");
                            ui.add(
                                egui::DragValue::new(&mut dialog.width)
                                    .range(1..=256)
                                    .speed(4.0),
                            );
                            ui.label("Height");
                            ui.add(
                                egui::DragValue::new(&mut dialog.height)
                                    .range(1..=256)
                                    .speed(4.0),
                            );
                        });
                        egui::ComboBox::from_label("Depth")
                            .selected_text(format!("{}bpp", dialog.depth_bits))
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut dialog.depth_bits, 4, "4bpp indexed");
                                ui.selectable_value(&mut dialog.depth_bits, 8, "8bpp indexed");
                                ui.selectable_value(&mut dialog.depth_bits, 15, "15bpp direct");
                            });
                        egui::ComboBox::from_label("Resample")
                            .selected_text(dialog.resampler.label())
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut dialog.resampler,
                                    TextureImportResamplerChoice::Lanczos3,
                                    TextureImportResamplerChoice::Lanczos3.label(),
                                );
                                ui.selectable_value(
                                    &mut dialog.resampler,
                                    TextureImportResamplerChoice::Triangle,
                                    TextureImportResamplerChoice::Triangle.label(),
                                );
                                ui.selectable_value(
                                    &mut dialog.resampler,
                                    TextureImportResamplerChoice::Nearest,
                                    TextureImportResamplerChoice::Nearest.label(),
                                );
                            });
                        ui.checkbox(&mut dialog.centre_crop, "Centre crop");
                        ui.label(
                            RichText::new(
                                "Crop keeps arbitrary source aspect ratios from stretching.",
                            )
                            .color(STUDIO_TEXT_WEAK)
                            .small(),
                        );
                        ui.checkbox(
                            &mut dialog.transparent_index_zero,
                            "Source alpha is transparent",
                        );
                        ui.label(
                            RichText::new(
                                "For cutout props and sprites: transparent source pixels become PSX texel 0.",
                            )
                            .color(STUDIO_TEXT_WEAK)
                            .small(),
                        );
                        ui.add_space(4.0);
                        color_editor(ui, "Tint", &mut dialog.tint);
                        ui.horizontal(|ui| {
                            if ui.small_button("White").clicked() {
                                dialog.tint = [255, 255, 255];
                            }
                            ui.label(
                                RichText::new("Baked into the cooked PSXT.")
                                    .color(STUDIO_TEXT_WEAK)
                                    .small(),
                            );
                        });

                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui.button(icons::label(icons::PLUS, "Import")).clicked() {
                                action = Some(Action::Import);
                            }
                            if ui.button("Cancel").clicked() {
                                action = Some(Action::Close);
                            }
                        });

                        if let Some(status) = &dialog.status {
                            ui.add_space(6.0);
                            match status {
                                TextureImportStatus::Info(text) => {
                                    ui.label(RichText::new(text).color(STUDIO_TEXT_WEAK).small());
                                }
                                TextureImportStatus::Error(text) => {
                                    ui.label(
                                        RichText::new(text)
                                            .color(Color32::from_rgb(220, 120, 100))
                                            .small(),
                                    );
                                }
                            }
                        }
                    });

                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_min_width(400.0);
                        if let Some(preview) = &dialog.preview {
                            draw_psxt_preview_block_sized(
                                ui,
                                Some((preview.handle.id(), preview.stats)),
                                Vec2::splat(288.0),
                            );
                            egui::CollapsingHeader::new(icons::label(icons::SCAN, "Cooked PSXT"))
                                .default_open(true)
                                .show(ui, |ui| {
                                    draw_psxt_stats(ui, preview.stats);
                                });
                        } else {
                            ui.vertical_centered(|ui| {
                                ui.add_space(120.0);
                                ui.label(RichText::new("Choose a source image").strong());
                                ui.label(
                                    RichText::new(
                                        "The preview updates automatically as import settings change.",
                                    )
                                    .color(STUDIO_TEXT_WEAK)
                                    .small(),
                                );
                            });
                        }
                    });
                });

                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    action = Some(Action::Close);
                }
            });

        if action.is_none()
            && before_preview_key != self.texture_import_preview_key()
            && !self.texture_import_dialog.source_path.trim().is_empty()
        {
            action = Some(Action::AutoPreview);
        }

        match action {
            Some(Action::BrowseSource) if self.choose_texture_import_source() => {
                self.run_texture_import_preview(ctx);
            }
            Some(Action::AutoPreview) => self.run_texture_import_preview(ctx),
            Some(Action::Import) => self.commit_texture_import(),
            Some(Action::Close) => self.close_texture_import_dialog(),
            _ => {}
        }
    }

    pub(crate) fn close_texture_import_dialog(&mut self) {
        self.texture_import_dialog.open = false;
        self.retire_texture_import_preview();
    }

    pub(crate) fn retire_texture_import_preview(&mut self) {
        if let Some(preview) = self.texture_import_dialog.preview.take() {
            self.retire_egui_texture(preview.handle);
        }
    }

    pub(crate) fn retire_egui_texture(&mut self, handle: egui::TextureHandle) {
        self.import_retired_textures
            .push((EGUI_TEXTURE_RETIRE_FRAMES, handle));
    }

    pub(crate) fn retire_egui_textures(
        &mut self,
        handles: impl IntoIterator<Item = egui::TextureHandle>,
    ) {
        self.import_retired_textures.extend(
            handles
                .into_iter()
                .map(|handle| (EGUI_TEXTURE_RETIRE_FRAMES, handle)),
        );
    }

    pub(crate) fn drain_live_egui_textures(&mut self) -> Vec<egui::TextureHandle> {
        // `ui_font_textures` is intentionally NOT drained here: the bitmap UI
        // fonts are project-independent, so they persist across project
        // switches and are rebuilt only if they were never loaded.
        let mut handles = Vec::new();
        if let Some(handle) = self.psoxide_logo_texture.take() {
            handles.push(handle);
        }
        if let Some(handle) = self.model_resource_preview_texture.take() {
            handles.push(handle);
        }
        if let Some(handle) = self.animation_viewer_preview_texture.take() {
            handles.push(handle);
        }
        handles.extend(self.texture_thumbs.drain().map(|(_, entry)| entry.handle));
        if let Some(preview) = self.texture_import_dialog.preview.take() {
            handles.push(preview.handle);
        }
        if let Some(preview) = self.model_import_dialog.preview.take() {
            if let Some((handle, _)) = preview.atlas {
                handles.push(handle);
            }
            if let Some(handle) = preview.animated_texture {
                handles.push(handle);
            }
        }
        handles.extend(
            self.import_retired_textures
                .drain(..)
                .map(|(_, handle)| handle),
        );
        handles
    }

    pub(crate) fn set_texture_import_preview(&mut self, preview: TextureImportPreview) {
        self.retire_texture_import_preview();
        self.texture_import_dialog.preview = Some(preview);
    }

    pub(crate) fn run_texture_import_preview(&mut self, ctx: &egui::Context) {
        let source = self.texture_import_source_path();
        if source.as_os_str().is_empty() {
            self.texture_import_dialog.status = Some(TextureImportStatus::Error(
                "Choose a PNG/JPG/BMP source path.".to_string(),
            ));
            return;
        }

        let config = self.texture_import_config();
        match psxed_project::texture_import::preview_texture_import(&source, &config) {
            Ok(preview) => {
                let Some((image, stats)) = decode_psxt_thumbnail(&preview.texture) else {
                    self.retire_texture_import_preview();
                    self.texture_import_dialog.status = Some(TextureImportStatus::Error(
                        "Preview cooked but could not decode the PSXT thumbnail.".to_string(),
                    ));
                    return;
                };
                let handle = ctx.load_texture(
                    "texture-import-preview",
                    image,
                    egui::TextureOptions::NEAREST,
                );
                self.set_texture_import_preview(TextureImportPreview { handle, stats });
                self.texture_import_dialog.status = Some(TextureImportStatus::Info(format!(
                    "Preview updated: {}",
                    human_bytes(preview.stats.bytes as u32)
                )));
            }
            Err(error) => {
                self.retire_texture_import_preview();
                self.texture_import_dialog.status = Some(TextureImportStatus::Error(format!(
                    "Preview failed: {error}"
                )));
            }
        }
    }

    pub(crate) fn choose_texture_import_source(&mut self) -> bool {
        let mut dialog = rfd::FileDialog::new()
            .set_title("Choose source image")
            .add_filter("Image", &["png", "jpg", "jpeg", "bmp"]);
        let current = self.texture_import_source_path();
        if let Some(dir) = Self::path_parent_or_self(&current) {
            dialog = dialog.set_directory(dir);
        } else if self.project_dir.is_dir() {
            dialog = dialog.set_directory(&self.project_dir);
        }

        let Some(path) = dialog.pick_file() else {
            return false;
        };

        self.texture_import_dialog.source_path =
            Self::display_project_path(&path, &self.project_dir);
        if self.texture_import_dialog.output_name.trim().is_empty() {
            if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
                self.texture_import_dialog.output_name = stem.to_string();
            }
        }
        self.retire_texture_import_preview();
        self.texture_import_dialog.status = Some(TextureImportStatus::Info(format!(
            "Selected source: {}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("texture")
        )));
        true
    }

    pub(crate) fn commit_texture_import(&mut self) {
        let source = self.texture_import_source_path();
        if source.as_os_str().is_empty() {
            self.texture_import_dialog.status = Some(TextureImportStatus::Error(
                "Choose a PNG/JPG/BMP source path.".to_string(),
            ));
            return;
        }
        let output_name = self.texture_import_output_name(&source);
        let config = self.texture_import_config();
        match psxed_project::texture_import::import_texture(
            &mut self.project,
            &source,
            &output_name,
            &self.project_dir,
            &config,
        ) {
            Ok(id) => {
                self.replace_resource_selection(id);
                self.clear_node_selection_state();
                self.clear_primitive_selection_state();
                self.clear_sector_selection();
                self.close_texture_import_dialog();
                self.status = format!("Imported texture {output_name}");
                self.mark_dirty();
            }
            Err(error) => {
                self.texture_import_dialog.status = Some(TextureImportStatus::Error(format!(
                    "Import failed: {error}"
                )));
            }
        }
    }

    pub(crate) fn texture_import_config(
        &self,
    ) -> psxed_project::texture_import::TextureImportConfig {
        let depth = match self.texture_import_dialog.depth_bits {
            8 => psxed_project::texture_import::TextureDepth::Bit8,
            15 => psxed_project::texture_import::TextureDepth::Bit15,
            _ => psxed_project::texture_import::TextureDepth::Bit4,
        };
        let crop = if self.texture_import_dialog.centre_crop {
            psxed_project::texture_import::CropMode::CentreSquare
        } else {
            psxed_project::texture_import::CropMode::None
        };
        psxed_project::texture_import::TextureImportConfig {
            width: self.texture_import_dialog.width.clamp(1, 256) as u16,
            height: self.texture_import_dialog.height.clamp(1, 256) as u16,
            depth,
            crop,
            resampler: self.texture_import_dialog.resampler.to_import(),
            tint: self.texture_import_dialog.tint,
            transparent_index_zero: self.texture_import_dialog.transparent_index_zero,
        }
    }

    pub(crate) fn texture_import_preview_key(&self) -> TextureImportPreviewKey {
        TextureImportPreviewKey {
            source_path: self.texture_import_dialog.source_path.trim().to_string(),
            width: self.texture_import_dialog.width.clamp(1, 256),
            height: self.texture_import_dialog.height.clamp(1, 256),
            depth_bits: self.texture_import_dialog.depth_bits,
            centre_crop: self.texture_import_dialog.centre_crop,
            transparent_index_zero: self.texture_import_dialog.transparent_index_zero,
            resampler: self.texture_import_dialog.resampler,
            tint: self.texture_import_dialog.tint,
        }
    }

    pub(crate) fn texture_import_source_path(&self) -> PathBuf {
        let trimmed = self.texture_import_dialog.source_path.trim();
        if trimmed.is_empty() {
            return PathBuf::new();
        }
        let path = Path::new(trimmed);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.project_dir.join(path)
        }
    }

    pub(crate) fn texture_import_output_name(&self, source: &Path) -> String {
        let trimmed = self.texture_import_dialog.output_name.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
        source
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| "texture".to_string())
    }

    pub(crate) fn draw_model_import_dialog(&mut self, ctx: &egui::Context) {
        if !self.model_import_dialog.open {
            return;
        }

        enum Action {
            BrowseSource,
            BrowseAnimations,
            ClearAnimations,
            Preview,
            Import,
            Close,
        }

        let mut action: Option<Action> = None;
        let dialog = &mut self.model_import_dialog;
        egui::Window::new(icons::label(icons::FILE_PLUS, "Import Model"))
            .collapsible(false)
            .resizable(true)
            .default_width(1300.0)
            .default_height(760.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_min_size(Vec2::new(980.0, 620.0));
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(300.0);
                        ui.label(RichText::new("Source").strong());
                        ui.label(RichText::new("Model path").color(STUDIO_TEXT_WEAK).small());
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut dialog.source_path)
                                    .desired_width(210.0),
                            );
                            if ui
                                .button(icons::label(icons::FOLDER, "Browse"))
                                .on_hover_text("Choose a .glb, .gltf, or .fbx file")
                                .clicked()
                            {
                                action = Some(Action::BrowseSource);
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Extra animations")
                                    .color(STUDIO_TEXT_WEAK)
                                    .small(),
                            );
                            if ui
                                .button(icons::label(icons::FILE_PLUS, "Add"))
                                .on_hover_text(
                                    "Add standalone .fbx, .glb, or .gltf animation takes to bake with this model.",
                                )
                                .clicked()
                            {
                                action = Some(Action::BrowseAnimations);
                            }
                            if !dialog.animation_paths.is_empty()
                                && ui.button("Clear").clicked()
                            {
                                action = Some(Action::ClearAnimations);
                            }
                        });
                        for path in dialog.animation_paths.iter().take(5) {
                            ui.label(RichText::new(path).color(STUDIO_TEXT_WEAK).small());
                        }
                        if dialog.animation_paths.len() > 5 {
                            ui.label(
                                RichText::new(format!(
                                    "+{} more",
                                    dialog.animation_paths.len() - 5
                                ))
                                .color(STUDIO_TEXT_WEAK)
                                .small(),
                            );
                        }
                        ui.label(RichText::new("Resource name").color(STUDIO_TEXT_WEAK).small());
                        ui.text_edit_singleline(&mut dialog.output_name);
                        if dialog.output_name.trim().is_empty() {
                            ui.label(
                                RichText::new("Uses the source file name when blank.")
                                    .color(STUDIO_TEXT_WEAK)
                                    .small(),
                            );
                        }

                        ui.separator();
                        ui.label(RichText::new("Bake Settings").strong());
                        ui.horizontal(|ui| {
                            ui.label("Atlas");
                            ui.add(
                                egui::DragValue::new(&mut dialog.texture_width)
                                    .range(16..=512)
                                    .speed(8.0),
                            );
                            ui.label("×");
                            ui.add(
                                egui::DragValue::new(&mut dialog.texture_height)
                                    .range(16..=512)
                                    .speed(8.0),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label("Atlas depth");
                            egui::ComboBox::from_id_salt("model_import_texture_depth")
                                .selected_text(format!("{}bpp indexed", dialog.texture_depth_bits))
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut dialog.texture_depth_bits,
                                        4,
                                        "4bpp indexed (16 colours)",
                                    );
                                    ui.selectable_value(
                                        &mut dialog.texture_depth_bits,
                                        8,
                                        "8bpp indexed (256 colours)",
                                    );
                                });
                        });
                        ui.horizontal(|ui| {
                            ui.label("Anim Hz");
                            ui.add(
                                egui::DragValue::new(&mut dialog.animation_fps)
                                    .range(1..=60)
                                    .speed(1.0),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label("World height")
                                .on_hover_text("Target rendered height in engine units; recook preview after changing this bake setting.");
                            let previous_height = dialog.world_height.clamp(128, 8192) as u16;
                            let previous_default_radius =
                                default_model_collision_radius_for_height(previous_height) as i32;
                            let height_response = ui.add(
                                egui::DragValue::new(&mut dialog.world_height)
                                    .range(128..=8192)
                                    .speed(16.0),
                            );
                            if height_response.changed()
                                && dialog.collision_radius == previous_default_radius
                            {
                                dialog.collision_radius = default_model_collision_radius_for_height(
                                    dialog.world_height.clamp(128, 8192) as u16,
                                ) as i32;
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Actor radius").on_hover_text(
                                "Runtime actor-cylinder radius for model/character records. Use an explicit Collider node for authored prop collision.",
                            );
                            ui.add(
                                egui::DragValue::new(&mut dialog.collision_radius)
                                    .range(1..=4096)
                                    .speed(8.0),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label("Scale");
                            ui.add(
                                egui::DragValue::new(&mut dialog.visual_scale_q8)
                                    .range(1..=4096)
                                    .speed(16.0),
                            );
                            ui.label(
                                RichText::new(format!(
                                    "{:.3}x",
                                    dialog.visual_scale_q8.max(1) as f32
                                        / MODEL_SCALE_ONE_Q8 as f32
                                ))
                                .color(STUDIO_TEXT_WEAK)
                                .monospace(),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label("Default yaw").on_hover_text(
                                "Model-facing rotation used by preview and newly placed renderer nodes.",
                            );
                            ui.add(
                                egui::DragValue::new(&mut dialog.default_visual_yaw_q12)
                                    .range(0..=4095)
                                    .speed(16.0),
                            );
                            ui.label(
                                RichText::new(format!(
                                    "{:.1} deg",
                                    q12_turns_to_degrees(dialog.default_visual_yaw_q12)
                                ))
                                .color(STUDIO_TEXT_WEAK)
                                .monospace(),
                            );
                        });
                        ui.checkbox(
                            &mut dialog.normalize_root_translation,
                            "Bake clips in place",
                        )
                        .on_hover_text(
                            "Removes root-joint translation while baking clips so gameplay code owns character movement.",
                        );
                        ui.checkbox(
                            &mut dialog.force_single_bind,
                            "Pure rigid (1 bone/vertex)",
                        )
                        .on_hover_text(
                            "Collapse every vertex to its dominant bone, dropping secondary skin weights. Keeps the model on the GTE single-bone fast path with no CPU-blend vertices -- the PS1-preferred rigid skinning.",
                        );
                        ui.checkbox(
                            &mut dialog.collapse_detail_bones,
                            "Collapse detail bones",
                        )
                        .on_hover_text(
                            "Reweights finger chains and Mixamo terminal bones into their nearest retained joints so humanoid rigs share the smaller PS1 skeleton contract.",
                        );
                        ui.label(
                            RichText::new(format!(
                                "Texture depth: {}bpp indexed",
                                dialog.texture_depth_bits
                            ))
                            .color(STUDIO_TEXT_WEAK)
                            .small(),
                        );

                        ui.separator();
                        ui.label(RichText::new("Preview").strong());
                        ui.horizontal(|ui| {
                            ui.label("Yaw");
                            ui.add(
                                egui::DragValue::new(&mut dialog.preview_yaw_q12)
                                    .range(0..=4095)
                                    .speed(8.0),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label("Pitch");
                            ui.add(
                                egui::DragValue::new(&mut dialog.preview_pitch_q12)
                                    .range(64..=960)
                                    .speed(6.0),
                            );
                        });
                        ui.add(
                            egui::Slider::new(&mut dialog.preview_radius, 640..=4096)
                                .text("Distance"),
                        );
                        ui.checkbox(&mut dialog.show_animation_root, "Anchor marker");
                        ui.checkbox(&mut dialog.preview_in_place, "Preview in-place")
                            .on_hover_text("Show the selected clip with root-motion translation removed.");
                        if ui.button(icons::label(icons::ROTATE_CCW, "Reset View")).clicked() {
                            dialog.preview_yaw_q12 = 340;
                            dialog.preview_pitch_q12 = 350;
                            dialog.preview_radius = 1536;
                            dialog.show_animation_root = true;
                            dialog.preview_in_place = true;
                        }

                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui.button(icons::label(icons::SCAN, "Cook Preview")).clicked() {
                                action = Some(Action::Preview);
                            }
                            if ui.button(icons::label(icons::PLUS, "Import")).clicked() {
                                action = Some(Action::Import);
                            }
                            if ui.button("Cancel").clicked() {
                                action = Some(Action::Close);
                            }
                        });

                        if let Some(status) = &dialog.status {
                            ui.add_space(6.0);
                            match status {
                                ModelImportStatus::Info(text) => {
                                    ui.label(RichText::new(text).color(STUDIO_TEXT_WEAK).small());
                                }
                                ModelImportStatus::Error(text) => {
                                    ui.label(
                                        RichText::new(text)
                                            .color(Color32::from_rgb(220, 120, 100))
                                            .small(),
                                    );
                                }
                            }
                        }
                    });

                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_min_width(640.0);
                        if let Some(preview) = &mut dialog.preview {
                            draw_model_import_preview(
                                ui,
                                preview,
                                &mut dialog.selected_clip,
                                ModelImportPreviewContext {
                                    preview_yaw_q12: &mut dialog.preview_yaw_q12,
                                    preview_pitch_q12: &mut dialog.preview_pitch_q12,
                                    preview_radius: &mut dialog.preview_radius,
                                    collision_radius: dialog.collision_radius,
                                    visual_scale_q8: dialog.visual_scale_q8,
                                    default_visual_yaw_q12: dialog.default_visual_yaw_q12,
                                    show_animation_root: dialog.show_animation_root,
                                    preview_in_place: dialog.preview_in_place,
                                },
                            );
                            ui.add_space(4.0);
                        } else {
                            ui.vertical_centered(|ui| {
                                ui.add_space(160.0);
                                ui.label(RichText::new("Cook a preview").strong());
                                ui.label(
                                    RichText::new(
                                        "The preview shows the cooked model, atlas, clips, and root-motion stats before files are written.",
                                    )
                                    .color(STUDIO_TEXT_WEAK)
                                    .small(),
                                );
                            });
                        }
                    });
                    if dialog.preview.is_some() {
                        ui.separator();
                        ui.vertical(|ui| {
                            ui.set_width(300.0);
                            egui::ScrollArea::vertical()
                                .id_salt("model-import-details")
                                .show(ui, |ui| {
                                    if let Some(preview) = &dialog.preview {
                                        draw_model_import_details(
                                            ui,
                                            preview,
                                            &mut dialog.selected_clip,
                                            dialog.collision_radius,
                                            dialog.visual_scale_q8,
                                            dialog.default_visual_yaw_q12,
                                        );
                                    }
                                });
                        });
                    }
                });

                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    action = Some(Action::Close);
                }
            });

        match action {
            Some(Action::BrowseSource) if self.choose_model_import_source() => {
                self.run_model_import_preview(ctx);
            }
            Some(Action::BrowseAnimations) if self.choose_model_import_animation_sources() => {
                self.run_model_import_preview(ctx);
            }
            Some(Action::ClearAnimations) => {
                self.model_import_dialog.animation_paths.clear();
                self.retire_model_import_preview();
                self.model_import_dialog.status = Some(ModelImportStatus::Info(
                    "Cleared extra animations".to_string(),
                ));
            }
            Some(Action::Preview) => self.run_model_import_preview(ctx),
            Some(Action::Import) => self.commit_model_import(),
            Some(Action::Close) => self.close_model_import_dialog(),
            _ => {}
        }
    }

    pub(crate) fn close_model_import_dialog(&mut self) {
        self.model_import_dialog.open = false;
        self.retire_model_import_preview();
    }

    pub(crate) fn retire_model_import_preview(&mut self) {
        if let Some(preview) = self.model_import_dialog.preview.take() {
            if let Some((handle, _)) = preview.atlas {
                self.retire_egui_texture(handle);
            }
            if let Some(handle) = preview.animated_texture {
                self.retire_egui_texture(handle);
            }
        }
    }

    pub(crate) fn set_model_import_preview(&mut self, preview: ModelImportPreview) {
        self.retire_model_import_preview();
        self.model_import_dialog.preview = Some(preview);
    }

    pub(crate) fn run_model_import_preview(&mut self, ctx: &egui::Context) {
        let source = self.model_import_source_path();
        if source.as_os_str().is_empty() {
            self.model_import_dialog.status = Some(ModelImportStatus::Error(
                "Choose a GLB/glTF/FBX source path.".to_string(),
            ));
            return;
        }
        let config = self.model_import_config();
        let animation_sources = self.model_import_animation_source_paths();
        let world_height = config.world_height as i32;
        match psxed_project::model_import::preview_model_with_animation_sources(
            &source,
            &animation_sources,
            config,
        ) {
            Ok(package) => {
                let decoded_atlas = package
                    .texture
                    .as_ref()
                    .and_then(|bytes| decode_psxt_thumbnail(bytes));
                let atlas_image = decoded_atlas.as_ref().map(|(image, _)| image.clone());
                let atlas = decoded_atlas.map(|(image, stats)| {
                    let handle = ctx.load_texture(
                        "model-import-atlas-preview",
                        image,
                        egui::TextureOptions::NEAREST,
                    );
                    (handle, stats)
                });
                let clips = package
                    .clips
                    .iter()
                    .map(|clip| ModelImportClipPreview {
                        name: clip
                            .source_name
                            .as_deref()
                            .unwrap_or(&clip.sanitized_name)
                            .to_string(),
                        frames: clip.frames,
                        byte_len: clip.bytes.len(),
                        bytes: clip.bytes.clone(),
                        root_motion: root_motion_stats(&clip.bytes, 0),
                    })
                    .collect();
                let clip_count = package.clips.len();
                self.set_model_import_preview(ModelImportPreview {
                    model_bytes: package.model,
                    report: package.report,
                    atlas,
                    atlas_image,
                    animated_texture: None,
                    world_height,
                    clips,
                });
                self.model_import_dialog.selected_clip = self
                    .model_import_dialog
                    .selected_clip
                    .min(clip_count.saturating_sub(1));
                self.model_import_dialog.status = Some(ModelImportStatus::Info(format!(
                    "Preview cooked: {clip_count} clip(s){}",
                    if self.model_import_dialog.normalize_root_translation {
                        ", in-place baked"
                    } else {
                        ""
                    }
                )));
            }
            Err(error) => {
                self.retire_model_import_preview();
                self.model_import_dialog.status =
                    Some(ModelImportStatus::Error(format!("Preview failed: {error}")));
            }
        }
    }

    pub(crate) fn choose_model_import_source(&mut self) -> bool {
        let mut dialog = rfd::FileDialog::new()
            .set_title("Choose model source")
            .add_filter("Model source", &["glb", "gltf", "fbx"]);
        let current = self.model_import_source_path();
        if let Some(dir) = Self::path_parent_or_self(&current) {
            dialog = dialog.set_directory(dir);
        } else if self.project_dir.is_dir() {
            dialog = dialog.set_directory(&self.project_dir);
        }

        let Some(path) = dialog.pick_file() else {
            return false;
        };

        self.model_import_dialog.source_path = Self::display_project_path(&path, &self.project_dir);
        if self.model_import_dialog.output_name.trim().is_empty() {
            if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
                self.model_import_dialog.output_name = stem.to_string();
            }
        }
        self.retire_model_import_preview();
        self.model_import_dialog.selected_clip = 0;
        self.model_import_dialog.status = Some(ModelImportStatus::Info(format!(
            "Selected source: {}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("model")
        )));
        true
    }

    pub(crate) fn choose_model_import_animation_sources(&mut self) -> bool {
        let mut dialog = rfd::FileDialog::new()
            .set_title("Choose animation sources")
            .add_filter("Animation source", &["fbx", "glb", "gltf"]);
        let current = self.model_import_source_path();
        if let Some(dir) = Self::path_parent_or_self(&current) {
            dialog = dialog.set_directory(dir);
        } else if self.project_dir.is_dir() {
            dialog = dialog.set_directory(&self.project_dir);
        }

        let Some(paths) = dialog.pick_files() else {
            return false;
        };
        for path in paths {
            let stored = Self::display_project_path(&path, &self.project_dir);
            if !self.model_import_dialog.animation_paths.contains(&stored) {
                self.model_import_dialog.animation_paths.push(stored);
            }
        }
        self.retire_model_import_preview();
        self.model_import_dialog.selected_clip = 0;
        self.model_import_dialog.status = Some(ModelImportStatus::Info(format!(
            "{} extra animation source(s)",
            self.model_import_dialog.animation_paths.len()
        )));
        true
    }

    pub(crate) fn commit_model_import(&mut self) {
        let source = self.model_import_source_path();
        if source.as_os_str().is_empty() {
            self.model_import_dialog.status = Some(ModelImportStatus::Error(
                "Choose a GLB/glTF/FBX source path.".to_string(),
            ));
            return;
        }
        let output_name = self.model_import_output_name(&source);
        let config = self.model_import_config();
        let animation_sources = self.model_import_animation_source_paths();
        match psxed_project::model_import::import_model_with_animation_sources(
            &mut self.project,
            &source,
            &animation_sources,
            &output_name,
            &self.project_dir,
            config,
        ) {
            Ok(id) => {
                let collision_radius = self
                    .model_import_dialog
                    .collision_radius
                    .clamp(1, u16::MAX as i32) as u16;
                let visual_scale_q8 = self
                    .model_import_dialog
                    .visual_scale_q8
                    .clamp(1, u16::MAX as i32) as u16;
                let default_visual_yaw_q12 =
                    q12_turns_to_i16(self.model_import_dialog.default_visual_yaw_q12);
                if let Some(resource) = self.project.resource_mut(id) {
                    if let ResourceData::Model(model) = &mut resource.data {
                        model.collision_radius = collision_radius;
                        model.scale_q8 = [visual_scale_q8; 3];
                        model.default_visual_yaw_q12 = default_visual_yaw_q12;
                    }
                }
                self.replace_resource_selection(id);
                self.clear_node_selection_state();
                self.clear_primitive_selection_state();
                self.clear_sector_selection();
                self.close_model_import_dialog();
                self.status = format!("Imported model {output_name}");
                self.mark_dirty();
            }
            Err(error) => {
                self.model_import_dialog.status =
                    Some(ModelImportStatus::Error(format!("Import failed: {error}")));
            }
        }
    }

    pub(crate) fn model_import_config(&self) -> psxed_project::model_import::RigidModelConfig {
        psxed_project::model_import::RigidModelConfig {
            texture_width: self.model_import_dialog.texture_width.clamp(16, 512) as u16,
            texture_height: self.model_import_dialog.texture_height.clamp(16, 512) as u16,
            texture_depth: if self.model_import_dialog.texture_depth_bits == 8 {
                psxed_project::model_import::TextureDepth::Bit8
            } else {
                psxed_project::model_import::TextureDepth::Bit4
            },
            animation_fps: self.model_import_dialog.animation_fps.clamp(1, 60) as u16,
            world_height: self.model_import_dialog.world_height.clamp(128, 8192) as u16,
            normalize_root_translation: self.model_import_dialog.normalize_root_translation,
            strip_animation_scale: true,
            // Keep hand-authored separate pieces (armor plates, detail bits).
            prune_detached_face_islands: 0,
            extra_animations_affect_bounds: true,
            force_single_bind: self.model_import_dialog.force_single_bind,
            // Per-model opt-in; the in-editor dialog defaults single-sided.
            // Double-sided models are imported via the dedicated tooling.
            double_sided: false,
            ignore_embedded_animations: false,
            collapse_bone_patterns: if self.model_import_dialog.collapse_detail_bones {
                psxed_project::model_import::default_collapse_bone_patterns()
            } else {
                Vec::new()
            },
        }
    }

    pub(crate) fn model_import_source_path(&self) -> PathBuf {
        let trimmed = self.model_import_dialog.source_path.trim();
        if trimmed.is_empty() {
            return PathBuf::new();
        }
        let path = Path::new(trimmed);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.project_dir.join(path)
        }
    }

    pub(crate) fn model_import_animation_source_paths(&self) -> Vec<PathBuf> {
        self.model_import_dialog
            .animation_paths
            .iter()
            .filter_map(|path| {
                let trimmed = path.trim();
                if trimmed.is_empty() {
                    None
                } else if Path::new(trimmed).is_absolute() {
                    Some(PathBuf::from(trimmed))
                } else {
                    Some(self.project_dir.join(trimmed))
                }
            })
            .collect()
    }

    pub(crate) fn model_import_output_name(&self, source: &Path) -> String {
        let trimmed = self.model_import_dialog.output_name.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
        source
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| "model".to_string())
    }

    pub(crate) fn path_parent_or_self(path: &Path) -> Option<PathBuf> {
        if path.as_os_str().is_empty() {
            return None;
        }
        if path.is_dir() {
            Some(path.to_path_buf())
        } else {
            path.parent().map(Path::to_path_buf)
        }
    }

    pub(crate) fn display_project_path(path: &Path, project_dir: &Path) -> String {
        if let Ok(relative) = path.strip_prefix(project_dir) {
            if !relative.as_os_str().is_empty() {
                return relative.to_string_lossy().into_owned();
            }
        }
        path.to_string_lossy().into_owned()
    }
}
