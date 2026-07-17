use super::*;
use psxed_project::{ModelSecondaryLayer, ModelSecondaryTexture, ProceduralNoiseTexture};

/// Transient Material Lab view state. The authored recipe itself lives in the
/// selected [`MaterialResource`], so switching projects or workspaces never
/// creates a second copy of material data.
#[derive(Default)]
pub(crate) struct MaterialLabState {
    pub(crate) focused_material: Option<ResourceId>,
    preview_signature: String,
    overlay_preview_size: [usize; 2],
}

impl EditorWorkspace {
    pub(crate) fn draw_material_lab(&mut self, ui: &mut egui::Ui) {
        let mut material_options: Vec<(ResourceId, String)> = self
            .project
            .resources
            .iter()
            .filter_map(|resource| match &resource.data {
                ResourceData::Material(_) => Some((resource.id, resource.name.clone())),
                _ => None,
            })
            .collect();

        if let Some(selected) = self.selection.selected_resource {
            if self
                .project
                .resource(selected)
                .is_some_and(|resource| matches!(&resource.data, ResourceData::Material(_)))
            {
                self.material_lab.focused_material = Some(selected);
            }
        }
        if !self.material_lab.focused_material.is_some_and(|id| {
            material_options
                .iter()
                .any(|(candidate, _)| *candidate == id)
        }) {
            self.material_lab.focused_material = material_options.first().map(|(id, _)| *id);
        }

        let mut create_material = false;
        let mut save_project = false;
        ui.horizontal_wrapped(|ui| {
            ui.heading(icons::label(icons::PALETTE, "Material Lab"));
            ui.add_space(8.0);
            ui.label(
                RichText::new("Reusable PS1 materials · 4bpp-first authoring")
                    .color(STUDIO_TEXT_WEAK),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                save_project = ui
                    .button(icons::label(icons::SAVE, "Save"))
                    .on_hover_text("Save every material and the project to project.ron")
                    .clicked();
                create_material = ui
                    .button(icons::label(icons::PLUS, "New Material"))
                    .on_hover_text("Create a reusable Material resource")
                    .clicked();
            });
        });
        ui.add_space(8.0);

        if create_material {
            let name = unique_material_name(&material_options);
            let id = self.project.add_resource(
                name.clone(),
                ResourceData::Material(MaterialResource::opaque(None)),
            );
            material_options.push((id, name.clone()));
            self.material_lab.focused_material = Some(id);
            self.replace_resource_selection(id);
            self.material_lab.preview_signature.clear();
            self.dirty = true;
            self.status = format!("Created material: {name}");
        }
        if save_project {
            if let Err(error) = self.save() {
                self.status = format!("Could not save materials: {error}");
            }
        }

        let Some(mut material_id) = self.material_lab.focused_material else {
            material_lab_empty_state(ui);
            return;
        };

        ui.horizontal_wrapped(|ui| {
            ui.label("Editing");
            let selected_name = material_options
                .iter()
                .find(|(id, _)| *id == material_id)
                .map(|(_, name)| name.as_str())
                .unwrap_or("Missing Material");
            egui::ComboBox::from_id_salt("material_lab_resource")
                .selected_text(selected_name)
                .width(260.0)
                .show_ui(ui, |ui| {
                    for (id, name) in &material_options {
                        ui.selectable_value(&mut material_id, *id, name);
                    }
                });
            if material_id != self.material_lab.focused_material.unwrap_or(material_id) {
                self.material_lab.focused_material = Some(material_id);
                self.replace_resource_selection(material_id);
                self.material_lab.preview_signature.clear();
            }
            ui.separator();
            ui.label(
                RichText::new(
                    "One Material resource can be assigned to models, props, rooms, or UI",
                )
                .small()
                .color(STUDIO_TEXT_WEAK),
            );
        });
        ui.add_space(10.0);

        let Some(resource) = self.project.resource(material_id) else {
            material_lab_empty_state(ui);
            return;
        };
        let ResourceData::Material(original) = &resource.data else {
            material_lab_empty_state(ui);
            return;
        };
        let original = original.clone();
        let mut edited = original.clone();
        let resource_name = resource.name.clone();
        let mut edited_name = resource_name.clone();

        ui.horizontal(|ui| {
            ui.label("Name");
            ui.add(
                egui::TextEdit::singleline(&mut edited_name)
                    .desired_width(320.0)
                    .hint_text("Material name"),
            );
        });
        ui.add_space(8.0);

        let available = ui.available_size();
        let settings_width = (available.x * 0.46).clamp(330.0, 520.0);
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(settings_width, available.y),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("material_lab_settings")
                        .show(ui, |ui| {
                            draw_material_source_presets(ui, &mut edited);
                            ui.add_space(10.0);
                            match edited.texture_mode {
                                MaterialTextureMode::SimpleImage => {
                                    draw_simple_image_settings(ui, &mut edited)
                                }
                                MaterialTextureMode::Generated => {
                                    draw_generated_settings(ui, &mut edited.generated)
                                }
                                MaterialTextureMode::ReflectiveProbe => {
                                    draw_reflection_settings(ui, &mut edited.reflection)
                                }
                            }
                            ui.add_space(12.0);
                            draw_world_animation_settings(ui, &mut edited);
                            ui.add_space(12.0);
                            draw_secondary_layer_settings(ui, &mut edited);
                            ui.add_space(12.0);
                            ui.separator();
                            ui.heading("Surface");
                            blend_mode_editor(ui, &mut edited.blend_mode);
                            color_editor(ui, "Modulation tint", &mut edited.tint);
                            draw_material_sidedness(ui, &mut edited);
                        });
                },
            );
            ui.separator();
            ui.vertical(|ui| {
                draw_material_lab_preview(self, ui, material_id, &resource_name, &edited);
            });
        });

        if edited != original {
            if let Some(resource) = self.project.resource_mut(material_id) {
                resource.data = ResourceData::Material(edited);
                self.dirty = true;
                self.status = format!("Updated material: {resource_name}");
                self.material_lab.preview_signature.clear();
            }
        }
        if edited_name != resource_name && !edited_name.trim().is_empty() {
            let edited_name = edited_name.trim().to_string();
            if let Some(resource) = self.project.resource_mut(material_id) {
                resource.name = edited_name.clone();
                self.dirty = true;
                self.status = format!("Renamed material: {edited_name}");
            }
        }
    }
}

fn material_lab_empty_state(ui: &mut egui::Ui) {
    ui.add_space(48.0);
    ui.vertical_centered(|ui| {
        ui.heading("No Material resources yet");
        ui.label(
            RichText::new(
                "Use New Material above to create a reusable material, then edit and save it here.",
            )
            .color(STUDIO_TEXT_WEAK),
        );
    });
}

fn unique_material_name(materials: &[(ResourceId, String)]) -> String {
    let available = |candidate: &str| materials.iter().all(|(_, name)| name != candidate);
    if available("New Material") {
        return "New Material".to_string();
    }
    for suffix in 2..=u16::MAX {
        let candidate = format!("New Material {suffix}");
        if available(&candidate) {
            return candidate;
        }
    }
    "New Material Copy".to_string()
}

fn draw_material_source_presets(ui: &mut egui::Ui, material: &mut MaterialResource) {
    ui.heading("Source");
    ui.label(
        RichText::new("Choose a preset; the underlying image and generator settings are preserved when switching.")
            .small()
            .color(STUDIO_TEXT_WEAK),
    );
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        for (mode, icon, description) in [
            (
                MaterialTextureMode::SimpleImage,
                icons::PALETTE,
                "Imported PSXT or model atlas",
            ),
            (
                MaterialTextureMode::ReflectiveProbe,
                icons::EYE,
                "Baked room environment",
            ),
            (
                MaterialTextureMode::Generated,
                icons::LAYERS,
                "Base colour plus noise",
            ),
        ] {
            let selected = material.texture_mode == mode;
            let response = ui
                .add(
                    egui::Button::new(icons::label(icon, mode.label()))
                        .selected(selected)
                        .min_size(Vec2::new(146.0, 30.0)),
                )
                .on_hover_text(description);
            if response.clicked() {
                material.texture_mode = mode;
            }
        }
    });
}

fn draw_simple_image_settings(ui: &mut egui::Ui, material: &mut MaterialResource) {
    section_frame().show(ui, |ui| {
        ui.heading("Image");
        let mut path = material.psxt_path.clone().unwrap_or_default();
        ui.label("4bpp PSXT path");
        if ui
            .add(
                egui::TextEdit::singleline(&mut path)
                    .hint_text("Empty uses the model atlas or flat tint"),
            )
            .changed()
        {
            material.psxt_path = (!path.trim().is_empty()).then_some(path);
        }
        ui.label(
            RichText::new("The original path remains available after trying another preset.")
                .small()
                .color(STUDIO_TEXT_WEAK),
        );
    });
}

fn draw_generated_settings(ui: &mut egui::Ui, generated: &mut GeneratedMaterialTexture) {
    section_frame().show(ui, |ui| {
        ui.heading("Generated 4bpp texture");
        ui.horizontal(|ui| {
            ui.label("Output");
            for size in [8u16, 16, 32, 64] {
                ui.selectable_value(&mut generated.size, size, format!("{size}×{size}"));
            }
        });
        color_editor(ui, "Base colour", &mut generated.base_color);
        color_editor(ui, "Noise colour", &mut generated.noise_color);

        ui.separator();
        ui.strong("Noise");
        egui::Grid::new("material_lab_noise")
            .num_columns(2)
            .spacing(Vec2::new(12.0, 5.0))
            .show(ui, |ui| {
                ui.label("Seed");
                ui.add(egui::DragValue::new(&mut generated.noise.seed));
                ui.end_row();
                ui.label("Feature size");
                ui.add(egui::DragValue::new(&mut generated.noise.feature_size).range(2..=64));
                ui.end_row();
                ui.label("Octaves");
                ui.add(egui::DragValue::new(&mut generated.noise.octaves).range(1..=5));
                ui.end_row();
                ui.label("Contrast");
                ui.add(egui::Slider::new(&mut generated.noise.contrast, 1..=255));
                ui.end_row();
            });

        ui.separator();
        ui.strong("Noise UV");
        ui.label(
            RichText::new("Baked into the texture: changing these costs nothing at runtime.")
                .small()
                .color(STUDIO_TEXT_WEAK),
        );
        egui::Grid::new("material_lab_noise_uv")
            .num_columns(2)
            .spacing(Vec2::new(12.0, 5.0))
            .show(ui, |ui| {
                ui.label("U scale");
                draw_q8_scale(ui, &mut generated.noise_uv.scale_u_q8);
                ui.end_row();
                ui.label("V scale");
                draw_q8_scale(ui, &mut generated.noise_uv.scale_v_q8);
                ui.end_row();
                ui.label("U offset");
                ui.add(egui::DragValue::new(&mut generated.noise_uv.offset_u));
                ui.end_row();
                ui.label("V offset");
                ui.add(egui::DragValue::new(&mut generated.noise_uv.offset_v));
                ui.end_row();
                ui.label("Rotation");
                egui::ComboBox::from_id_salt("material_lab_noise_rotation")
                    .selected_text(format!(
                        "{}°",
                        (generated.noise_uv.rotation_quarters & 3) * 90
                    ))
                    .show_ui(ui, |ui| {
                        for quarter in 0..4u8 {
                            ui.selectable_value(
                                &mut generated.noise_uv.rotation_quarters,
                                quarter,
                                format!("{}°", quarter * 90),
                            );
                        }
                    });
                ui.end_row();
            });
    });
}

fn draw_q8_scale(ui: &mut egui::Ui, value: &mut u16) {
    let mut scale = f32::from(*value) / 256.0;
    if ui
        .add(
            egui::DragValue::new(&mut scale)
                .speed(0.05)
                .range(0.0625..=8.0),
        )
        .changed()
    {
        *value = (scale * 256.0).round().clamp(16.0, 2048.0) as u16;
    }
}

fn draw_reflection_settings(ui: &mut egui::Ui, reflection: &mut ReflectionProbeMaterial) {
    section_frame().show(ui, |ui| {
        ui.heading("Room reflection probe");
        ui.label(
            RichText::new("Mirror-like environment mapping from the active room's baked 4bpp probe.")
                .color(STUDIO_TEXT_WEAK),
        );
        ui.add(egui::Slider::new(&mut reflection.strength, 0..=255).text("Strength"));
        ui.add(egui::Slider::new(&mut reflection.roughness, 0..=255).text("Roughness"));
        ui.add_space(6.0);
        ui.colored_label(
            STUDIO_WARNING,
            "Probe capture and reflected UV rendering are the next backend step. Until a room probe is baked, the existing image source remains the runtime fallback.",
        );
    });
}

fn draw_world_animation_settings(ui: &mut egui::Ui, material: &mut MaterialResource) {
    section_frame().show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.heading("Tile animation");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("Water preset")
                    .on_hover_text("Average-blended 4bpp texture with a slow diagonal UV current")
                    .clicked()
                {
                    material.blend_mode = PsxBlendMode::Average;
                    material.animation.mode = MaterialAnimationMode::UvScroll;
                    material.animation.uv_scroll.enabled = true;
                    material.animation.uv_scroll.speed_u_q8 = 2 * 256;
                    material.animation.uv_scroll.speed_v_q8 = 1 * 256;
                }
            });
        });
        ui.label(
            RichText::new(
                "One-pass animation for room tiles and other surfaces using this material.",
            )
            .small()
            .color(STUDIO_TEXT_WEAK),
        );

        egui::ComboBox::from_label("Mode")
            .selected_text(material.animation.mode.label())
            .show_ui(ui, |ui| {
                for mode in [
                    MaterialAnimationMode::Static,
                    MaterialAnimationMode::UvScroll,
                    MaterialAnimationMode::Flipbook,
                ] {
                    ui.selectable_value(&mut material.animation.mode, mode, mode.label());
                }
            });

        match material.animation.mode {
            MaterialAnimationMode::Static => {
                ui.label(
                    RichText::new("No per-frame UV work; eligible for prebuilt room packets.")
                        .small()
                        .color(STUDIO_TEXT_WEAK),
                );
            }
            MaterialAnimationMode::UvScroll => {
                material.animation.uv_scroll.enabled = true;
                egui::Grid::new("material_lab_world_uv_scroll")
                    .num_columns(2)
                    .spacing(Vec2::new(12.0, 5.0))
                    .show(ui, |ui| {
                        ui.label("U speed");
                        draw_q8_speed(ui, &mut material.animation.uv_scroll.speed_u_q8);
                        ui.end_row();
                        ui.label("V speed");
                        draw_q8_speed(ui, &mut material.animation.uv_scroll.speed_v_q8);
                        ui.end_row();
                        ui.label("U phase");
                        ui.add(egui::DragValue::new(
                            &mut material.animation.uv_scroll.phase_u,
                        ));
                        ui.end_row();
                        ui.label("V phase");
                        ui.add(egui::DragValue::new(
                            &mut material.animation.uv_scroll.phase_v,
                        ));
                        ui.end_row();
                    });
            }
            MaterialAnimationMode::Flipbook => {
                let flipbook = &mut material.animation.flipbook;
                egui::Grid::new("material_lab_world_flipbook")
                    .num_columns(2)
                    .spacing(Vec2::new(12.0, 5.0))
                    .show(ui, |ui| {
                        ui.label("Columns");
                        ui.add(egui::DragValue::new(&mut flipbook.columns).range(1..=8));
                        ui.end_row();
                        ui.label("Rows");
                        ui.add(egui::DragValue::new(&mut flipbook.rows).range(1..=8));
                        ui.end_row();
                        let cells = flipbook.columns.max(1).saturating_mul(flipbook.rows.max(1));
                        ui.label("Frames");
                        ui.add(
                            egui::DragValue::new(&mut flipbook.frame_count)
                                .range(1..=cells.max(1)),
                        );
                        ui.end_row();
                        ui.label("Ticks / frame");
                        ui.add(
                            egui::DragValue::new(&mut flipbook.ticks_per_frame).range(1..=255),
                        );
                        ui.end_row();
                        ui.label("Start frame");
                        ui.add(egui::DragValue::new(&mut flipbook.phase).range(0..=cells - 1));
                        ui.end_row();
                    });
                *flipbook = flipbook.normalized();
                ui.label(
                    RichText::new(
                        "Frames are a row-major grid inside one resident 4bpp texture; no runtime uploads.",
                    )
                    .small()
                    .color(STUDIO_TEXT_WEAK),
                );
            }
        }
    });
}

fn draw_secondary_layer_settings(ui: &mut egui::Ui, material: &mut MaterialResource) {
    section_frame().show(ui, |ui| {
        ui.heading("Model texture layer");
        ui.label(
            RichText::new(
                "A second 4bpp texture drawn over a model's base. It can remain static or scroll at runtime.",
            )
            .small()
            .color(STUDIO_TEXT_WEAK),
        );
        let mut enabled = material.secondary_layer.is_some();
        if ui.checkbox(&mut enabled, "Enable overlay").changed() {
            material.secondary_layer = enabled.then(ModelSecondaryLayer::default);
        }
        let Some(layer) = material.secondary_layer.as_mut() else {
            return;
        };

        ui.add_space(4.0);
        let mut generated = matches!(layer.texture, ModelSecondaryTexture::ProceduralNoise(_));
        egui::ComboBox::from_label("Overlay source")
            .selected_text(if generated {
                "Generated noise"
            } else {
                "4bpp texture"
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut generated, true, "Generated noise");
                ui.selectable_value(&mut generated, false, "4bpp texture");
            });
        if generated != matches!(layer.texture, ModelSecondaryTexture::ProceduralNoise(_)) {
            layer.texture = if generated {
                ModelSecondaryTexture::ProceduralNoise(ProceduralNoiseTexture::default())
            } else {
                ModelSecondaryTexture::Texture(String::new())
            };
        }
        match &mut layer.texture {
            ModelSecondaryTexture::Texture(path) => {
                ui.label("4bpp PSXT path");
                ui.add(egui::TextEdit::singleline(path).hint_text("textures/overlay.psxt"));
            }
            ModelSecondaryTexture::ProceduralNoise(noise) => {
                egui::Grid::new("material_lab_overlay_noise")
                    .num_columns(2)
                    .spacing(Vec2::new(12.0, 5.0))
                    .show(ui, |ui| {
                        ui.label("Seed");
                        ui.add(egui::DragValue::new(&mut noise.seed));
                        ui.end_row();
                        ui.label("Feature size");
                        ui.add(egui::DragValue::new(&mut noise.feature_size).range(2..=64));
                        ui.end_row();
                        ui.label("Octaves");
                        ui.add(egui::DragValue::new(&mut noise.octaves).range(1..=5));
                        ui.end_row();
                        ui.label("Contrast");
                        ui.add(egui::Slider::new(&mut noise.contrast, 1..=255));
                        ui.end_row();
                    });
            }
        }
        blend_mode_editor(ui, &mut layer.blend_mode);
        color_editor(ui, "Overlay tint / strength", &mut layer.tint);

        ui.separator();
        ui.checkbox(&mut layer.motion.enabled, "Dynamic UV motion");
        ui.add_enabled_ui(layer.motion.enabled, |ui| {
            ui.label(
                RichText::new(
                    "Signed texels per second; motion wraps in the PS1's 0–255 UV space.",
                )
                .small()
                .color(STUDIO_TEXT_WEAK),
            );
            egui::Grid::new("material_lab_overlay_motion")
                .num_columns(2)
                .spacing(Vec2::new(12.0, 5.0))
                .show(ui, |ui| {
                    ui.label("U speed");
                    draw_q8_speed(ui, &mut layer.motion.speed_u_q8);
                    ui.end_row();
                    ui.label("V speed");
                    draw_q8_speed(ui, &mut layer.motion.speed_v_q8);
                    ui.end_row();
                    ui.label("U phase");
                    ui.add(egui::DragValue::new(&mut layer.motion.phase_u));
                    ui.end_row();
                    ui.label("V phase");
                    ui.add(egui::DragValue::new(&mut layer.motion.phase_v));
                    ui.end_row();
                });
        });
        ui.label(
            RichText::new(
                "Runtime cost: integer UV offsets only. The overlay texture stays resident in VRAM.",
            )
            .small()
            .color(STUDIO_TEXT_WEAK),
        );
    });
}

fn draw_q8_speed(ui: &mut egui::Ui, value: &mut i16) {
    let mut speed = f32::from(*value) / 256.0;
    if ui
        .add(
            egui::DragValue::new(&mut speed)
                .speed(0.25)
                .range(-127.0..=127.0)
                .suffix(" tex/s"),
        )
        .changed()
    {
        *value = (speed * 256.0).round().clamp(-32_512.0, 32_512.0) as i16;
    }
}

fn draw_material_sidedness(ui: &mut egui::Ui, material: &mut MaterialResource) {
    let resolved = material.sidedness();
    if material.face_sidedness != resolved {
        material.face_sidedness = resolved;
    }
    egui::ComboBox::from_label("Rendered sides")
        .selected_text(material.face_sidedness.label())
        .show_ui(ui, |ui| {
            for side in [
                MaterialFaceSidedness::Front,
                MaterialFaceSidedness::Back,
                MaterialFaceSidedness::Both,
            ] {
                ui.selectable_value(&mut material.face_sidedness, side, side.label());
            }
        });
    material.sync_legacy_sidedness();
}

fn draw_material_lab_preview(
    workspace: &mut EditorWorkspace,
    ui: &mut egui::Ui,
    material_id: ResourceId,
    resource_name: &str,
    material: &MaterialResource,
) {
    ui.heading("Preview");
    ui.label(
        RichText::new(format!(
            "{} · {}",
            resource_name,
            material.texture_mode.label()
        ))
        .color(STUDIO_TEXT_WEAK),
    );
    ui.add_space(8.0);

    let image = match material.texture_mode {
        MaterialTextureMode::Generated => {
            let bytes = psxed_project::generate_material_texture_psxt(material.generated);
            decode_psxt_thumbnail(&bytes).map(|(image, _)| image)
        }
        MaterialTextureMode::SimpleImage => workspace
            .project
            .resource(material_id)
            .and_then(|resource| workspace.texture_thumb_entry(resource))
            .map(|entry| entry.image.clone()),
        MaterialTextureMode::ReflectiveProbe => None,
    };
    let overlay_image = material
        .secondary_layer
        .as_ref()
        .and_then(|layer| match &layer.texture {
            ModelSecondaryTexture::ProceduralNoise(settings) => {
                let bytes = psxed_project::generate_model_noise_psxt(*settings);
                decode_psxt_thumbnail(&bytes).map(|(image, _)| image)
            }
            ModelSecondaryTexture::Texture(path) if !path.trim().is_empty() => {
                let path = workspace.project_root().join(path);
                std::fs::read(path)
                    .ok()
                    .and_then(|bytes| decode_psxt_thumbnail(&bytes).map(|(image, _)| image))
            }
            ModelSecondaryTexture::Texture(_) => None,
        });

    let signature = format!("{material:?}");
    if workspace.material_lab.preview_signature != signature {
        workspace.material_lab.preview_signature = signature;
        if let Some(image) = image {
            if let Some(texture) = workspace.material_lab_preview_texture.as_mut() {
                texture.set(image, egui::TextureOptions::NEAREST);
            } else {
                workspace.material_lab_preview_texture = Some(ui.ctx().load_texture(
                    "material-lab-preview",
                    image,
                    egui::TextureOptions::NEAREST,
                ));
            }
        } else {
            workspace.material_lab_preview_texture = None;
        }
        if let Some(image) = overlay_image {
            workspace.material_lab.overlay_preview_size = image.size;
            if let Some(texture) = workspace.material_lab_overlay_preview_texture.as_mut() {
                texture.set(image, egui::TextureOptions::NEAREST);
            } else {
                workspace.material_lab_overlay_preview_texture = Some(ui.ctx().load_texture(
                    "material-lab-overlay-preview",
                    image,
                    egui::TextureOptions::NEAREST,
                ));
            }
        } else {
            workspace.material_lab_overlay_preview_texture = None;
            workspace.material_lab.overlay_preview_size = [0, 0];
        }
    }

    let preview_size = ui.available_width().min(420.0).max(240.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(preview_size), Sense::hover());
    let painter = ui.painter_at(rect);
    draw_preview_checker(&painter, rect);
    let tick = (ui.input(|input| input.time) * 60.0).max(0.0) as u32;
    if material.texture_mode == MaterialTextureMode::ReflectiveProbe {
        painter.rect_filled(rect.shrink(18.0), 8.0, STUDIO_VIEWPORT);
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "ROOM PROBE\nNOT BAKED",
            FontId::proportional(18.0),
            STUDIO_WARNING,
        );
    } else if let Some(texture) = workspace.material_lab_preview_texture.as_ref() {
        match material.animation.mode {
            MaterialAnimationMode::Static => {
                painter.image(
                    texture.id(),
                    rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            MaterialAnimationMode::UvScroll => {
                let [u, v] = material.animation.uv_scroll.offset_at_tick(tick, 60);
                let texture_size = texture.size_vec2();
                let offset = Vec2::new(
                    f32::from(u) / texture_size.x.max(1.0) * rect.width(),
                    f32::from(v) / texture_size.y.max(1.0) * rect.height(),
                );
                let clipped = painter.with_clip_rect(rect);
                for x in [-rect.width(), 0.0] {
                    for y in [-rect.height(), 0.0] {
                        clipped.image(
                            texture.id(),
                            rect.translate(offset + Vec2::new(x, y)),
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                }
                ui.ctx().request_repaint();
            }
            MaterialAnimationMode::Flipbook => {
                let flipbook = material.animation.flipbook.normalized();
                let frame = ((tick / u32::from(flipbook.ticks_per_frame))
                    + u32::from(flipbook.phase))
                    % u32::from(flipbook.frame_count);
                let column = frame % u32::from(flipbook.columns);
                let row = frame / u32::from(flipbook.columns);
                let cell = Vec2::new(
                    1.0 / f32::from(flipbook.columns),
                    1.0 / f32::from(flipbook.rows),
                );
                let uv_min = Pos2::new(column as f32 * cell.x, row as f32 * cell.y);
                painter.image(
                    texture.id(),
                    rect,
                    Rect::from_min_size(uv_min, cell),
                    Color32::WHITE,
                );
                ui.ctx().request_repaint();
            }
        }
    } else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "NO IMAGE",
            FontId::proportional(18.0),
            STUDIO_TEXT_WEAK,
        );
    }
    if let (Some(layer), Some(texture)) = (
        material.secondary_layer.as_ref(),
        workspace.material_lab_overlay_preview_texture.as_ref(),
    ) {
        let [u, v] = layer.motion.offset_at_tick(tick, 60);
        let [width, height] = workspace.material_lab.overlay_preview_size;
        if width > 0 && height > 0 {
            let offset = Vec2::new(
                f32::from(u) / width as f32 * rect.width(),
                f32::from(v) / height as f32 * rect.height(),
            );
            let tint = Color32::from_rgba_unmultiplied(
                layer.tint[0].saturating_mul(2),
                layer.tint[1].saturating_mul(2),
                layer.tint[2].saturating_mul(2),
                128,
            );
            let overlay_painter = painter.with_clip_rect(rect);
            for x in [-rect.width(), 0.0] {
                for y in [-rect.height(), 0.0] {
                    overlay_painter.image(
                        texture.id(),
                        rect.translate(offset + Vec2::new(x, y)),
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        tint,
                    );
                }
            }
            ui.ctx().request_repaint();
        }
    }

    ui.add_space(8.0);
    section_frame().show(ui, |ui| {
        ui.strong("PS1 output");
        ui.label("4bpp · 16-colour CLUT · nearest sampling");
        let pass_note = if material
            .secondary_layer
            .as_ref()
            .is_some_and(|layer| layer.motion.enabled)
        {
            "2 model passes · dynamic overlay (UV-only animation)"
        } else if material.secondary_layer.is_some() {
            "2 model passes · static overlay"
        } else if material.animation.mode == MaterialAnimationMode::UvScroll {
            "1 texture pass · dynamic tile UV scroll"
        } else if material.animation.mode == MaterialAnimationMode::Flipbook {
            "1 texture pass · resident flipbook atlas"
        } else {
            "1 texture pass"
        };
        ui.label(RichText::new(pass_note).color(STUDIO_TEXT_WEAK));
    });
}

fn draw_preview_checker(painter: &egui::Painter, rect: Rect) {
    const CELLS: usize = 8;
    let cell = rect.width() / CELLS as f32;
    for y in 0..CELLS {
        for x in 0..CELLS {
            let min = rect.min + Vec2::new(x as f32 * cell, y as f32 * cell);
            let tile = Rect::from_min_size(min, Vec2::splat(cell + 0.5));
            let color = if (x + y) & 1 == 0 {
                STUDIO_PANEL_HEADER
            } else {
                STUDIO_HOVER
            };
            painter.rect_filled(tile, 0.0, color);
        }
    }
}
