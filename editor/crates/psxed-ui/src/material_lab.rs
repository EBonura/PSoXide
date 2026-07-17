use super::*;
use psxed_project::MaterialUvMotion;

/// Transient Material Lab view state. The authored recipe itself lives in the
/// selected [`MaterialResource`], so switching projects or workspaces never
/// creates a second copy of material data.
#[derive(Default)]
pub(crate) struct MaterialLabState {
    pub(crate) focused_material: Option<ResourceId>,
    preview_signature: String,
    base_preview_image: Option<ColorImage>,
    overlay_preview_image: Option<ColorImage>,
    rendered_preview_tick: Option<u32>,
}

impl EditorWorkspace {
    /// Focus a Material resource in both Material Lab and the Inspector.
    /// Used by deterministic/headless editor routes as well as future UI
    /// navigation that needs the two views to stay in sync.
    pub fn focus_material_resource(&mut self, material_id: ResourceId) -> bool {
        if !self
            .project
            .resource(material_id)
            .is_some_and(|resource| matches!(&resource.data, ResourceData::Material(_)))
        {
            return false;
        }
        self.material_lab.focused_material = Some(material_id);
        self.replace_resource_selection(material_id);
        self.material_lab.preview_signature.clear();
        true
    }

    fn material_lab_options(&self) -> Vec<(ResourceId, String)> {
        self.project
            .resources
            .iter()
            .filter_map(|resource| match &resource.data {
                ResourceData::Material(_) => Some((resource.id, resource.name.clone())),
                _ => None,
            })
            .collect()
    }

    fn sync_material_lab_focus(&mut self, material_options: &[(ResourceId, String)]) {
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
    }

    pub(crate) fn draw_material_lab_toolbar(&mut self, ui: &mut egui::Ui) {
        let material_options = self.material_lab_options();
        self.sync_material_lab_focus(&material_options);
        let mut material_id = self.material_lab.focused_material;
        ui.label(RichText::new("Material").color(STUDIO_TEXT_WEAK));
        let selected_name = material_id
            .and_then(|selected| material_options.iter().find(|(id, _)| *id == selected))
            .map(|(_, name)| name.as_str())
            .unwrap_or("(none)");
        egui::ComboBox::from_id_salt("material_lab_resource")
            .selected_text(selected_name)
            .width(190.0)
            .show_ui(ui, |ui| {
                for (id, name) in &material_options {
                    ui.selectable_value(&mut material_id, Some(*id), name);
                }
            });
        if material_id != self.material_lab.focused_material {
            if let Some(material_id) = material_id {
                self.focus_material_resource(material_id);
            } else {
                self.material_lab.focused_material = None;
            }
        }

        let create_material = ui
            .button(icons::label(icons::PLUS, "New"))
            .on_hover_text("Create a reusable Material resource")
            .clicked();
        let save_project = ui
            .button(icons::label(icons::SAVE, "Save"))
            .on_hover_text("Save every material and the project to project.ron")
            .clicked();

        if create_material {
            let name = unique_material_name(&material_options);
            let id = self.project.add_resource(
                name.clone(),
                ResourceData::Material(MaterialResource::opaque(None)),
            );
            self.focus_material_resource(id);
            self.dirty = true;
            self.status = format!("Created material: {name}");
        }
        if save_project {
            if let Err(error) = self.save() {
                self.status = format!("Could not save materials: {error}");
            }
        }
    }

    pub(crate) fn draw_material_lab(&mut self, ui: &mut egui::Ui) {
        let material_options = self.material_lab_options();
        self.sync_material_lab_focus(&material_options);
        let Some(material_id) = self.material_lab.focused_material else {
            material_lab_empty_state(ui);
            return;
        };

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
                            draw_material_settings(ui, "material_lab", &mut edited);
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

pub(crate) fn draw_primary_layer_settings(ui: &mut egui::Ui, material: &mut MaterialResource) {
    ui.push_id("material_layer_1", |ui| {
        draw_primary_layer_settings_inner(ui, material);
    });
}

/// Canonical material settings body shared by Material Lab and every
/// inspector route. The caller-provided salt allows more than one view of the
/// same material to be visible without egui widget-ID collisions.
pub(crate) fn draw_material_settings(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash,
    material: &mut MaterialResource,
) -> bool {
    let original = material.clone();
    ui.push_id(id_salt, |ui| {
        draw_primary_layer_settings(ui, material);
        ui.add_space(12.0);
        draw_secondary_layer_settings(ui, material);
        ui.add_space(12.0);
        ui.separator();
        ui.heading("Surface");
        draw_material_sidedness(ui, material);
    });
    *material != original
}

fn draw_primary_layer_settings_inner(ui: &mut egui::Ui, material: &mut MaterialResource) {
    ui.heading("Layer 1");
    draw_material_source_presets(ui, &mut material.texture_mode);
    ui.add_space(6.0);
    match material.texture_mode {
        MaterialTextureMode::SimpleImage => draw_simple_image_settings(ui, &mut material.psxt_path),
        MaterialTextureMode::Generated => draw_generated_settings(ui, &mut material.generated),
        MaterialTextureMode::ReflectiveProbe => {
            draw_reflection_settings(ui, &mut material.reflection)
        }
    }
    ui.add_space(6.0);
    section_frame().show(ui, |ui| {
        blend_mode_editor(ui, &mut material.blend_mode);
        draw_material_color_editor(ui, "Base colour / tint", &mut material.tint);
        let mut motion = material.animation.uv_scroll;
        motion.enabled = material.animation.mode == MaterialAnimationMode::UvScroll;
        let before = motion;
        draw_layer_motion(ui, "material_lab_layer_1_motion", &mut motion);
        if motion != before {
            material.animation.uv_scroll = motion;
            material.animation.mode = if motion.enabled {
                MaterialAnimationMode::UvScroll
            } else {
                MaterialAnimationMode::Static
            };
        }
    });
}

fn draw_material_source_presets(ui: &mut egui::Ui, selected_mode: &mut MaterialTextureMode) {
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
            let selected = *selected_mode == mode;
            let response = ui
                .add(
                    egui::Button::new(icons::label(icon, mode.label()))
                        .selected(selected)
                        .min_size(Vec2::new(146.0, 30.0)),
                )
                .on_hover_text(description);
            if response.clicked() {
                *selected_mode = mode;
            }
        }
    });
}

fn draw_simple_image_settings(ui: &mut egui::Ui, psxt_path: &mut Option<String>) {
    section_frame().show(ui, |ui| {
        ui.heading("Image");
        let mut path = psxt_path.clone().unwrap_or_default();
        ui.label("4bpp PSXT path");
        if ui
            .add(
                egui::TextEdit::singleline(&mut path)
                    .hint_text("Empty uses the model atlas or flat tint"),
            )
            .changed()
        {
            *psxt_path = (!path.trim().is_empty()).then_some(path);
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
        ui.label("Output");
        let output_columns = if ui.available_width() >= 390.0 {
            5
        } else if ui.available_width() >= 240.0 {
            3
        } else {
            2
        };
        egui::Grid::new("material_output_sizes")
            .num_columns(output_columns)
            .min_col_width(72.0)
            .spacing(Vec2::new(6.0, 4.0))
            .show(ui, |ui| {
                for (index, size) in [8u16, 16, 32, 64, 128].into_iter().enumerate() {
                    if ui
                        .add_sized(
                            Vec2::new(72.0, 24.0),
                            egui::Button::new(format!("{size}×{size}"))
                                .selected(generated.size == size),
                        )
                        .clicked()
                    {
                        generated.size = size;
                    }
                    if (index + 1) % output_columns == 0 {
                        ui.end_row();
                    }
                }
            });
        ui.label(
            RichText::new("128×128 uses about 8 KiB of PS1 VRAM at 4bpp.")
                .small()
                .color(STUDIO_TEXT_WEAK),
        );
        draw_material_color_editor(ui, "Base colour", &mut generated.base_color);

        ui.separator();
        ui.checkbox(&mut generated.noise_enabled, "Enable baked noise");
        ui.label(
            RichText::new("Noise is folded into the base image and costs no extra PS1 pass.")
                .small()
                .color(STUDIO_TEXT_WEAK),
        );
        ui.add_enabled_ui(generated.noise_enabled, |ui| {
            draw_material_color_editor(ui, "Noise colour", &mut generated.noise_color);
            ui.strong("Noise recipe");
            egui::Grid::new("material_lab_noise")
                .num_columns(2)
                .min_col_width(96.0)
                .spacing(Vec2::new(12.0, 5.0))
                .show(ui, |ui| {
                    material_grid_label(ui, "Seed");
                    ui.add(egui::DragValue::new(&mut generated.noise.seed));
                    ui.end_row();
                    material_grid_label(ui, "Feature size");
                    ui.add(egui::DragValue::new(&mut generated.noise.feature_size).range(2..=64));
                    ui.end_row();
                    material_grid_label(ui, "Octaves");
                    ui.add(egui::DragValue::new(&mut generated.noise.octaves).range(1..=5));
                    ui.end_row();
                    material_grid_label(ui, "Contrast");
                    ui.add(egui::Slider::new(&mut generated.noise.contrast, 1..=255));
                    ui.end_row();
                });

            ui.separator();
            ui.strong("Noise UV");
            egui::Grid::new("material_lab_noise_uv")
                .num_columns(2)
                .min_col_width(96.0)
                .spacing(Vec2::new(12.0, 5.0))
                .show(ui, |ui| {
                    material_grid_label(ui, "U scale");
                    draw_q8_scale(ui, &mut generated.noise_uv.scale_u_q8);
                    ui.end_row();
                    material_grid_label(ui, "V scale");
                    draw_q8_scale(ui, &mut generated.noise_uv.scale_v_q8);
                    ui.end_row();
                    material_grid_label(ui, "U offset");
                    ui.add(egui::DragValue::new(&mut generated.noise_uv.offset_u));
                    ui.end_row();
                    material_grid_label(ui, "V offset");
                    ui.add(egui::DragValue::new(&mut generated.noise_uv.offset_v));
                    ui.end_row();
                    material_grid_label(ui, "Rotation");
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
    });
}

fn material_grid_label(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(text).extend());
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
        ui.label(
            RichText::new(
                "The active room is baked automatically to a 64x64 4bpp probe. Runtime reflections switch probes as the actor crosses rooms.",
            )
            .color(STUDIO_TEXT_WEAK),
        );
    });
}

pub(crate) fn draw_secondary_layer_settings(ui: &mut egui::Ui, material: &mut MaterialResource) {
    ui.push_id("material_layer_2", |ui| {
        draw_secondary_layer_settings_inner(ui, material);
    });
}

fn draw_secondary_layer_settings_inner(ui: &mut egui::Ui, material: &mut MaterialResource) {
    ui.heading("Layer 2");
    let mut enabled = material.enabled_secondary_layer().is_some();
    if ui.checkbox(&mut enabled, "Enable layer 2").changed() {
        material.set_secondary_layer_enabled(enabled);
    }
    let Some(layer) = material.secondary_layer.as_mut() else {
        return;
    };
    if !layer.enabled {
        ui.label(
            RichText::new("Layer settings are preserved while disabled.")
                .small()
                .color(STUDIO_TEXT_WEAK),
        );
        return;
    }
    draw_material_source_presets(ui, &mut layer.texture_mode);
    ui.add_space(6.0);
    match layer.texture_mode {
        MaterialTextureMode::SimpleImage => draw_simple_image_settings(ui, &mut layer.psxt_path),
        MaterialTextureMode::Generated => draw_generated_settings(ui, &mut layer.generated),
        MaterialTextureMode::ReflectiveProbe => draw_reflection_settings(ui, &mut layer.reflection),
    }
    ui.add_space(6.0);
    section_frame().show(ui, |ui| {
        blend_mode_editor(ui, &mut layer.blend_mode);
        draw_material_color_editor(ui, "Base colour / tint", &mut layer.tint);
        draw_layer_motion(ui, "material_lab_layer_2_motion", &mut layer.motion);
    });
}

fn draw_material_color_editor(ui: &mut egui::Ui, label: &str, color: &mut [u8; 3]) -> bool {
    ui.horizontal(|ui| {
        ui.label(icons::text(icons::PALETTE, 12.0).color(STUDIO_TEXT_WEAK));
        ui.label(label);
    });
    ui.horizontal(|ui| {
        let mut changed = ui.color_edit_button_srgb(color).changed();
        for (channel, prefix) in color.iter_mut().zip(["R ", "G ", "B "]) {
            changed |= ui
                .add_sized(
                    Vec2::new(52.0, 22.0),
                    egui::DragValue::new(channel).prefix(prefix).range(0..=255),
                )
                .changed();
        }
        changed
    })
    .inner
}

fn draw_layer_motion(ui: &mut egui::Ui, grid_id: &'static str, motion: &mut MaterialUvMotion) {
    ui.separator();
    ui.checkbox(&mut motion.enabled, "Movement");
    ui.add_enabled_ui(motion.enabled, |ui| {
        ui.label(
            RichText::new("Signed texels per second; UVs wrap in the PS1's 0–255 space.")
                .small()
                .color(STUDIO_TEXT_WEAK),
        );
        egui::Grid::new(grid_id)
            .num_columns(2)
            .min_col_width(96.0)
            .spacing(Vec2::new(12.0, 5.0))
            .show(ui, |ui| {
                material_grid_label(ui, "U speed");
                draw_q8_speed(ui, &mut motion.speed_u_q8);
                ui.end_row();
                material_grid_label(ui, "V speed");
                draw_q8_speed(ui, &mut motion.speed_v_q8);
                ui.end_row();
                material_grid_label(ui, "U phase");
                ui.add(egui::DragValue::new(&mut motion.phase_u));
                ui.end_row();
                material_grid_label(ui, "V phase");
                ui.add(egui::DragValue::new(&mut motion.phase_v));
                ui.end_row();
            });
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

    let mut probe_signature = String::new();
    let needs_probe = material.texture_mode == MaterialTextureMode::ReflectiveProbe
        || material
            .enabled_secondary_layer()
            .is_some_and(|layer| layer.texture_mode == MaterialTextureMode::ReflectiveProbe);
    let probe_image = if needs_probe {
        let project_root = workspace.project_root().to_path_buf();
        workspace.active_room_id().and_then(|room| {
            let node = workspace.project.active_scene().node(room)?;
            let NodeKind::Room { grid } = &node.kind else {
                return None;
            };
            let bytes = psxed_project::generate_room_reflection_probe_psxt(
                &workspace.project,
                grid,
                &project_root,
            )
            .ok()?;
            let checksum = bytes
                .iter()
                .fold(0u32, |hash, byte| hash.rotate_left(5) ^ u32::from(*byte));
            probe_signature = format!("room={}:probe={checksum:08x}", room.raw());
            decode_psxt_thumbnail(&bytes).map(|(image, _)| image)
        })
    } else {
        None
    };
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
        MaterialTextureMode::ReflectiveProbe => probe_image.clone(),
    };
    let overlay_image =
        material
            .enabled_secondary_layer()
            .and_then(|layer| match layer.texture_mode {
                MaterialTextureMode::Generated => {
                    let bytes = psxed_project::generate_material_texture_psxt(layer.generated);
                    decode_psxt_thumbnail(&bytes).map(|(image, _)| image)
                }
                MaterialTextureMode::SimpleImage => layer.psxt_path.as_deref().and_then(|path| {
                    let path = workspace.project_root().join(path);
                    std::fs::read(path)
                        .ok()
                        .and_then(|bytes| decode_psxt_thumbnail(&bytes).map(|(image, _)| image))
                }),
                MaterialTextureMode::ReflectiveProbe => probe_image.clone(),
            });

    let signature = format!("{material:?}:{probe_signature}");
    if workspace.material_lab.preview_signature != signature {
        workspace.material_lab.preview_signature = signature;
        workspace.material_lab.base_preview_image = image;
        workspace.material_lab.overlay_preview_image = overlay_image;
        workspace.material_lab.rendered_preview_tick = None;
    }

    let preview_size = ui.available_width().min(420.0).max(240.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(preview_size), Sense::hover());
    let painter = ui.painter_at(rect);
    draw_preview_checker(&painter, rect);
    let tick = (ui.input(|input| input.time) * 60.0).max(0.0) as u32;
    if workspace.material_lab.base_preview_image.is_some()
        || workspace.material_lab.overlay_preview_image.is_some()
    {
        let animated = model_stack_base_preview_animation(material)
            != MaterialAnimationMode::Static
            || material
                .enabled_secondary_layer()
                .is_some_and(|layer| layer.motion.enabled);
        let rendered_tick = if animated { tick } else { 0 };
        if workspace.material_lab.rendered_preview_tick != Some(rendered_tick) {
            let composite = compose_material_preview(
                workspace.material_lab.base_preview_image.as_ref(),
                workspace.material_lab.overlay_preview_image.as_ref(),
                material,
                tick,
            );
            if let Some(texture) = workspace.material_lab_preview_texture.as_mut() {
                texture.set(composite, egui::TextureOptions::NEAREST);
            } else {
                workspace.material_lab_preview_texture = Some(ui.ctx().load_texture(
                    "material-lab-preview",
                    composite,
                    egui::TextureOptions::NEAREST,
                ));
            }
            workspace.material_lab.rendered_preview_tick = Some(rendered_tick);
        }
        if let Some(texture) = workspace.material_lab_preview_texture.as_ref() {
            painter.image(
                texture.id(),
                rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        if animated {
            ui.ctx().request_repaint();
        }
    } else {
        workspace.material_lab_preview_texture = None;
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "NO IMAGE",
            FontId::proportional(18.0),
            STUDIO_TEXT_WEAK,
        );
    }
    ui.add_space(8.0);
    section_frame().show(ui, |ui| {
        ui.strong("PS1 output");
        ui.label("4bpp · 16-colour CLUT · nearest sampling");
        let pass_note = if material.enabled_secondary_layer().is_some() {
            "2 model passes · independent source, blend and movement"
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

fn model_stack_base_preview_animation(material: &MaterialResource) -> MaterialAnimationMode {
    material.animation.mode
}

/// Composite the host preview with the same integer tint and semi-transparency
/// equations used by the PS1 GPU. Egui's painter only offers ordinary alpha
/// blending, which made every Material Lab overlay look like Average and hid
/// the actual Add/Subtract/Add Quarter result.
fn compose_material_preview(
    base: Option<&ColorImage>,
    overlay: Option<&ColorImage>,
    material: &MaterialResource,
    tick: u32,
) -> ColorImage {
    let width = base
        .map_or(0, |image| image.size[0])
        .max(overlay.map_or(0, |image| image.size[0]))
        .max(64);
    let height = base
        .map_or(0, |image| image.size[1])
        .max(overlay.map_or(0, |image| image.size[1]))
        .max(64);
    let mut output = ColorImage::new([width, height], Color32::TRANSPARENT);
    for y in 0..height {
        for x in 0..width {
            output.pixels[y * width + x] = preview_checker_pixel(x, y, width, height);
        }
    }

    if let Some(base) = base {
        let animation = model_stack_base_preview_animation(material);
        let offset = match animation {
            MaterialAnimationMode::UvScroll => {
                material.animation.uv_scroll.offset_at_tick(tick, 60)
            }
            _ => [0, 0],
        };
        let flipbook = (animation == MaterialAnimationMode::Flipbook)
            .then(|| material.animation.flipbook.normalized());
        for y in 0..height {
            for x in 0..width {
                let texel = sample_preview_image(base, x, y, width, height, offset, flipbook, tick);
                composite_preview_pixel(
                    &mut output.pixels[y * width + x],
                    texel,
                    material.tint,
                    material.blend_mode,
                );
            }
        }
    }

    if let (Some(overlay), Some(layer)) = (overlay, material.enabled_secondary_layer()) {
        let offset = layer.motion.offset_at_tick(tick, 60);
        for y in 0..height {
            for x in 0..width {
                let texel = sample_preview_image(overlay, x, y, width, height, offset, None, tick);
                composite_preview_pixel(
                    &mut output.pixels[y * width + x],
                    texel,
                    layer.tint,
                    layer.blend_mode,
                );
            }
        }
    }
    output
}

fn preview_checker_pixel(x: usize, y: usize, width: usize, height: usize) -> Color32 {
    let cell_width = width.div_ceil(8).max(1);
    let cell_height = height.div_ceil(8).max(1);
    if (x / cell_width + y / cell_height) & 1 == 0 {
        STUDIO_PANEL_HEADER
    } else {
        STUDIO_HOVER
    }
}

fn sample_preview_image(
    image: &ColorImage,
    x: usize,
    y: usize,
    output_width: usize,
    output_height: usize,
    offset: [u8; 2],
    flipbook: Option<psxed_project::MaterialFlipbook>,
    tick: u32,
) -> Color32 {
    let [image_width, image_height] = image.size;
    if image_width == 0 || image_height == 0 {
        return Color32::TRANSPARENT;
    }
    let (source_x, source_y) = if let Some(flipbook) = flipbook {
        let frame = ((tick / u32::from(flipbook.ticks_per_frame)) + u32::from(flipbook.phase))
            % u32::from(flipbook.frame_count);
        let column = frame % u32::from(flipbook.columns);
        let row = frame / u32::from(flipbook.columns);
        let cell_width = image_width / usize::from(flipbook.columns);
        let cell_height = image_height / usize::from(flipbook.rows);
        (
            column as usize * cell_width + x * cell_width / output_width.max(1),
            row as usize * cell_height + y * cell_height / output_height.max(1),
        )
    } else {
        (
            (x * image_width / output_width.max(1) + usize::from(offset[0])) % image_width,
            (y * image_height / output_height.max(1) + usize::from(offset[1])) % image_height,
        )
    };
    image.pixels[source_y.min(image_height - 1) * image_width + source_x.min(image_width - 1)]
}

fn composite_preview_pixel(
    background: &mut Color32,
    source: Color32,
    tint: [u8; 3],
    blend: PsxBlendMode,
) {
    if source.a() == 0 {
        return;
    }
    let [source_r, source_g, source_b, _] = source.to_array();
    let foreground = [
        ps1_modulate(source_r, tint[0]),
        ps1_modulate(source_g, tint[1]),
        ps1_modulate(source_b, tint[2]),
    ];
    let [background_r, background_g, background_b, _] = background.to_array();
    let result = [
        ps1_blend_channel(background_r, foreground[0], blend),
        ps1_blend_channel(background_g, foreground[1], blend),
        ps1_blend_channel(background_b, foreground[2], blend),
    ];
    *background = Color32::from_rgb(result[0], result[1], result[2]);
}

fn ps1_modulate(texel: u8, tint: u8) -> u8 {
    ((u16::from(texel) * u16::from(tint) + 64) / 128).min(255) as u8
}

fn ps1_blend_channel(background: u8, foreground: u8, blend: PsxBlendMode) -> u8 {
    match blend {
        PsxBlendMode::Opaque => foreground,
        PsxBlendMode::Average => ((u16::from(background) + u16::from(foreground)) / 2) as u8,
        PsxBlendMode::Add => background.saturating_add(foreground),
        PsxBlendMode::Subtract => background.saturating_sub(foreground),
        PsxBlendMode::AddQuarter => background.saturating_add(foreground / 4),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use psxed_project::ModelSecondaryLayer;

    fn collect_text_shapes<'a>(
        shape: &'a egui::epaint::Shape,
        output: &mut Vec<&'a egui::epaint::TextShape>,
    ) {
        match shape {
            egui::epaint::Shape::Text(text) => output.push(text),
            egui::epaint::Shape::Vec(shapes) => {
                for shape in shapes {
                    collect_text_shapes(shape, output);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn shared_material_editor_has_unique_ids_and_readable_narrow_layout() {
        let mut material = MaterialResource::opaque(None);
        material.texture_mode = MaterialTextureMode::Generated;
        material.generated.noise_enabled = true;
        material.secondary_layer = Some(ModelSecondaryLayer::moving_default());
        material
            .secondary_layer
            .as_mut()
            .expect("secondary layer")
            .generated
            .noise_enabled = true;

        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        let proportional = fonts
            .families
            .get(&egui::FontFamily::Proportional)
            .cloned()
            .expect("default proportional font family");
        fonts
            .families
            .insert(egui::FontFamily::Name("lucide".into()), proportional);
        ctx.set_fonts(fonts);
        ctx.options_mut(|options| options.warn_on_id_clash = true);
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(380.0, 4800.0))),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    draw_material_settings(ui, "material_lab_test", &mut material);
                    draw_material_settings(ui, "resource_inspector_test", &mut material);
                });
            },
        );

        let mut text_shapes = Vec::new();
        for clipped in &output.shapes {
            collect_text_shapes(&clipped.shape, &mut text_shapes);
        }
        assert!(
            text_shapes
                .iter()
                .all(|text| !text.galley.job.text.contains("use of Grid ID")
                    && !text.galley.job.text.contains("use of widget ID")),
            "material controls should not emit egui duplicate-ID warnings"
        );
        let compact_grid_labels: Vec<_> = text_shapes
            .iter()
            .filter(|text| {
                matches!(
                    text.galley.job.text.as_str(),
                    "Seed"
                        | "Feature size"
                        | "Octaves"
                        | "Contrast"
                        | "U scale"
                        | "V scale"
                        | "U offset"
                        | "V offset"
                        | "Rotation"
                )
            })
            .collect();
        assert_eq!(
            compact_grid_labels.len(),
            36,
            "both shared editors should render both layers' recipe and UV labels"
        );
        assert!(
            compact_grid_labels
                .iter()
                .all(|text| text.galley.rows.len() == 1),
            "recipe and UV labels should stay on one line in an inspector-width panel"
        );
    }

    #[test]
    fn layer_two_toggle_preserves_the_complete_recipe() {
        let mut material = MaterialResource::opaque(None);
        let mut layer = ModelSecondaryLayer::moving_default();
        layer.texture_mode = MaterialTextureMode::SimpleImage;
        layer.psxt_path = Some("materials/water.psxt".to_string());
        layer.generated.noise.seed = 93;
        layer.reflection.strength = 71;
        layer.blend_mode = PsxBlendMode::Subtract;
        layer.tint = [17, 83, 149];
        layer.motion.speed_u_q8 = -704;
        material.secondary_layer = Some(layer.clone());

        material.set_secondary_layer_enabled(false);
        assert!(material.enabled_secondary_layer().is_none());
        let disabled = material.secondary_layer.as_ref().expect("recipe retained");
        assert!(!disabled.enabled);
        assert_eq!(disabled.psxt_path, layer.psxt_path);
        assert_eq!(disabled.generated, layer.generated);
        assert_eq!(disabled.reflection, layer.reflection);
        assert_eq!(disabled.blend_mode, layer.blend_mode);
        assert_eq!(disabled.tint, layer.tint);
        assert_eq!(disabled.motion, layer.motion);

        material.set_secondary_layer_enabled(true);
        assert_eq!(material.enabled_secondary_layer(), Some(&layer));
    }

    #[test]
    fn model_stack_preview_moves_both_layers_independently() {
        let mut material = MaterialResource::opaque(None);
        material.animation.mode = MaterialAnimationMode::UvScroll;
        assert_eq!(
            model_stack_base_preview_animation(&material),
            MaterialAnimationMode::UvScroll
        );

        material.secondary_layer = Some(ModelSecondaryLayer::moving_default());
        assert_eq!(
            model_stack_base_preview_animation(&material),
            MaterialAnimationMode::UvScroll
        );
        assert!(material.secondary_layer.unwrap().motion.enabled);
    }

    #[test]
    fn preview_uses_the_five_ps1_blend_equations() {
        assert_eq!(ps1_blend_channel(100, 80, PsxBlendMode::Opaque), 80);
        assert_eq!(ps1_blend_channel(100, 80, PsxBlendMode::Average), 90);
        assert_eq!(ps1_blend_channel(200, 80, PsxBlendMode::Add), 255);
        assert_eq!(ps1_blend_channel(40, 80, PsxBlendMode::Subtract), 0);
        assert_eq!(ps1_blend_channel(100, 80, PsxBlendMode::AddQuarter), 120);
    }

    #[test]
    fn preview_tint_matches_ps1_neutral_and_saturation() {
        assert_eq!(ps1_modulate(96, 128), 96);
        assert_eq!(ps1_modulate(255, 255), 255);
        assert_eq!(ps1_modulate(64, 64), 32);
    }

    #[test]
    fn transparent_overlay_texels_leave_the_base_untouched() {
        let mut background = Color32::from_rgb(12, 34, 56);
        composite_preview_pixel(
            &mut background,
            Color32::TRANSPARENT,
            [255; 3],
            PsxBlendMode::Add,
        );
        assert_eq!(background, Color32::from_rgb(12, 34, 56));
    }
}
