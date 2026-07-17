use super::*;

/// Transient Material Lab view state. The authored recipe itself lives in the
/// selected [`MaterialResource`], so switching projects or workspaces never
/// creates a second copy of material data.
#[derive(Default)]
pub(crate) struct MaterialLabState {
    pub(crate) focused_material: Option<ResourceId>,
    preview_signature: String,
}

impl EditorWorkspace {
    pub(crate) fn draw_material_lab(&mut self, ui: &mut egui::Ui) {
        let material_options: Vec<(ResourceId, String)> = self
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

        ui.horizontal(|ui| {
            ui.heading(icons::label(icons::PALETTE, "Material Lab"));
            ui.add_space(8.0);
            ui.label(
                RichText::new("Reusable PS1 materials · 4bpp-first authoring")
                    .color(STUDIO_TEXT_WEAK),
            );
        });
        ui.add_space(8.0);

        let Some(mut material_id) = self.material_lab.focused_material else {
            material_lab_empty_state(ui);
            return;
        };

        ui.horizontal(|ui| {
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
    }
}

fn material_lab_empty_state(ui: &mut egui::Ui) {
    ui.add_space(48.0);
    ui.vertical_centered(|ui| {
        ui.heading("No Material resources yet");
        ui.label(
            RichText::new(
                "Create or import a Material from the Resources panel, then edit it here.",
            )
            .color(STUDIO_TEXT_WEAK),
        );
    });
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
    ui.group(|ui| {
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
    ui.group(|ui| {
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
    ui.group(|ui| {
        ui.heading("Room reflection probe");
        ui.label(
            RichText::new("Mirror-like environment mapping from the active room's baked 4bpp probe.")
                .color(STUDIO_TEXT_WEAK),
        );
        ui.add(egui::Slider::new(&mut reflection.strength, 0..=255).text("Strength"));
        ui.add(egui::Slider::new(&mut reflection.roughness, 0..=255).text("Roughness"));
        ui.add_space(6.0);
        ui.colored_label(
            Color32::from_rgb(224, 174, 92),
            "Probe capture and reflected UV rendering are the next backend step. Until a room probe is baked, the existing image source remains the runtime fallback.",
        );
    });
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
    }

    let preview_size = ui.available_width().min(420.0).max(240.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(preview_size), Sense::hover());
    let painter = ui.painter_at(rect);
    draw_preview_checker(&painter, rect);
    if material.texture_mode == MaterialTextureMode::ReflectiveProbe {
        painter.rect_filled(rect.shrink(18.0), 8.0, Color32::from_rgb(18, 24, 32));
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "ROOM PROBE\nNOT BAKED",
            FontId::proportional(18.0),
            Color32::from_rgb(224, 174, 92),
        );
    } else if let Some(texture) = workspace.material_lab_preview_texture.as_ref() {
        painter.image(
            texture.id(),
            rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    } else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "NO IMAGE",
            FontId::proportional(18.0),
            STUDIO_TEXT_WEAK,
        );
    }

    ui.add_space(8.0);
    ui.group(|ui| {
        ui.strong("PS1 output");
        ui.label("4bpp · 16-colour CLUT · nearest sampling");
        let pass_note = if material.secondary_layer.is_some() {
            "2 model passes (additional overlay enabled)"
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
                Color32::from_rgb(34, 38, 44)
            } else {
                Color32::from_rgb(50, 55, 62)
            };
            painter.rect_filled(tile, 0.0, color);
        }
    }
}
