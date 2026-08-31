use super::*;

pub(crate) fn draw_transform_policy_editor(
    ui: &mut egui::Ui,
    node: &mut psxed_project::SceneNode,
    inherited_sector_size: i32,
    texture_options: &[(ResourceId, String)],
    nav_target: &mut Option<ResourceId>,
    snap_to_floor_requested: &mut bool,
) -> bool {
    let show_snap_to_floor = matches!(node.kind, NodeKind::Entity);
    match &mut node.kind {
        NodeKind::World {
            sector_size: _,
            sky,
            far_vista,
            camera: _,
            culling,
            streaming: _,
            physics,
            world_message,
        } => draw_world_settings(
            ui,
            sky,
            far_vista,
            culling,
            physics,
            world_message,
            texture_options,
            nav_target,
        ),
        NodeKind::ArchProp { geometry, .. } => {
            arch_prop_transform_editor(ui, &mut node.transform, inherited_sector_size, *geometry)
        }
        _ => match node_transform_inspector(&node.kind) {
            NodeTransformInspector::Hidden => false,
            NodeTransformInspector::PositionOnly => {
                light_transform_editor(ui, &mut node.transform, inherited_sector_size)
            }
            NodeTransformInspector::PositionYaw => entity_transform_editor(
                ui,
                &mut node.transform,
                inherited_sector_size,
                false,
                show_snap_to_floor,
                snap_to_floor_requested,
            ),
            NodeTransformInspector::PositionFullRotation => entity_transform_editor(
                ui,
                &mut node.transform,
                inherited_sector_size,
                true,
                show_snap_to_floor,
                snap_to_floor_requested,
            ),
            NodeTransformInspector::FullTransform => {
                let mut changed = false;
                changed |= transform_editor(ui, "Position", &mut node.transform.translation, 1.0);
                changed |=
                    transform_editor(ui, "Rotation", &mut node.transform.rotation_degrees, 1.0);
                changed |= transform_editor(ui, "Scale", &mut node.transform.scale, 0.05);
                changed
            }
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeTransformInspector {
    Hidden,
    PositionOnly,
    PositionYaw,
    PositionFullRotation,
    FullTransform,
}

pub(crate) fn node_transform_inspector(kind: &NodeKind) -> NodeTransformInspector {
    match kind {
        NodeKind::World { .. }
        | NodeKind::Node
        | NodeKind::Group
        | NodeKind::Section { .. }
        | NodeKind::WaterVolume { .. }
        | NodeKind::Portal { .. } => NodeTransformInspector::Hidden,
        NodeKind::PointLight { .. }
        | NodeKind::ParticleEmitter { .. }
        | NodeKind::Logic { .. }
        | NodeKind::Destructible { .. } => NodeTransformInspector::PositionOnly,
        NodeKind::ModelRenderer { .. }
        | NodeKind::Animator { .. }
        | NodeKind::Collider { .. }
        | NodeKind::CharacterController { .. }
        | NodeKind::Camera { .. }
        | NodeKind::Equipment { .. }
        | NodeKind::PhysicsBody { .. }
        | NodeKind::Interactable { .. }
        | NodeKind::PointOfInterest { .. } => NodeTransformInspector::Hidden,
        NodeKind::MeshInstance { .. } | NodeKind::SpawnPoint { .. } => {
            NodeTransformInspector::PositionYaw
        }
        // Entities allow pitch/roll so placed model props can face any
        // direction; the cook forwards all three axes to the runtime
        // model instance (character gameplay still drives yaw only).
        NodeKind::Entity
        | NodeKind::ImageProp { .. }
        | NodeKind::BoxProp { .. }
        | NodeKind::CylinderProp { .. } => NodeTransformInspector::PositionFullRotation,
        NodeKind::ArchProp { .. } => NodeTransformInspector::PositionFullRotation,
        NodeKind::Node3D => NodeTransformInspector::FullTransform,
    }
}

pub(crate) fn draw_world_settings(
    ui: &mut egui::Ui,
    sky: &mut SkySettings,
    far_vista: &mut FarVistaSettings,
    culling: &mut WorldCullingSettings,
    physics: &mut WorldPhysicsSettings,
    world_message: &mut Option<psxed_project::WorldMessage>,
    texture_options: &[(ResourceId, String)],
    nav_target: &mut Option<ResourceId>,
) -> bool {
    let mut changed = false;
    egui::CollapsingHeader::new(icons::label(icons::FILE, "World Message"))
        .default_open(world_message.is_some())
        .show(ui, |ui| {
            let mut enabled = world_message.is_some();
            if ui.checkbox(&mut enabled, "Show on scene start").changed() {
                *world_message = enabled.then(psxed_project::WorldMessage::default);
                changed = true;
            }
            if let Some(message) = world_message {
                ui.label(
                    RichText::new("Shown once per game launch. CROSS advances pages.")
                        .small()
                        .color(STUDIO_TEXT_WEAK),
                );
                changed |= draw_message_pages_editor(ui, "world-message", &mut message.pages, 3);
            }
        });
    egui::CollapsingHeader::new(icons::label(icons::WAYPOINT, "Physics"))
        .default_open(true)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Gravity").color(STUDIO_TEXT_WEAK));
                if ui
                    .add(
                        egui::DragValue::new(&mut physics.gravity_per_tick)
                            .speed(8.0)
                            .range(MIN_WORLD_GRAVITY_PER_TICK..=MAX_WORLD_GRAVITY_PER_TICK),
                    )
                    .on_hover_text("Downward acceleration in engine units per 60 Hz tick squared.")
                    .changed()
                {
                    *physics = physics.normalized();
                    changed = true;
                }
                ui.label(RichText::new("units/tick^2").color(STUDIO_TEXT_WEAK));
            });
        });
    egui::CollapsingHeader::new(icons::label(icons::SUN, "Sky"))
        .default_open(true)
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Test sky").color(STUDIO_TEXT_WEAK));
                if ui.small_button("Procedural").clicked() {
                    sky.mode = SkyMode::Panorama;
                    sky.visibility = SkyVisibility::Always;
                    sky.texture = None;
                    changed = true;
                }
                let quake = texture_options
                    .iter()
                    .find(|(_, name)| name == BUILTIN_QUAKE_SKY_NAME)
                    .map(|(id, _)| *id);
                if ui
                    .add_enabled(quake.is_some(), egui::Button::new("Quake layered").small())
                    .on_hover_text("Animated two-layer 4bpp sky. Test mode makes it visible everywhere.")
                    .clicked()
                {
                    sky.mode = SkyMode::QuakeLayered;
                    sky.visibility = SkyVisibility::Always;
                    sky.texture = quake;
                    changed = true;
                }
                let cube = texture_options
                    .iter()
                    .find(|(_, name)| name == BUILTIN_CUBE_SKY_NAME)
                    .map(|(id, _)| *id);
                if ui
                    .add_enabled(cube.is_some(), egui::Button::new("Sunset cube").small())
                    .on_hover_text("Six-face 4bpp directional sunset. Test mode makes it visible everywhere.")
                    .clicked()
                {
                    sky.mode = SkyMode::Cube;
                    sky.visibility = SkyVisibility::Always;
                    sky.texture = cube;
                    changed = true;
                }
            });
            ui.label(
                RichText::new(
                    "Test presets use Always visible so every project can preview them before authoring sky-aperture faces.",
                )
                .small()
                .color(STUDIO_TEXT_WEAK),
            );
            ui.horizontal(|ui| {
                ui.label(RichText::new("Mode").color(STUDIO_TEXT_WEAK));
                egui::ComboBox::from_id_salt("world-sky-mode")
                    .selected_text(sky.mode.label())
                    .show_ui(ui, |ui| {
                        for mode in SkyMode::ALL {
                            changed |= ui
                                .selectable_value(&mut sky.mode, mode, mode.label())
                                .changed();
                        }
                    });
            });
            if sky.mode != SkyMode::Off {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Visible").color(STUDIO_TEXT_WEAK));
                    egui::ComboBox::from_id_salt("world-sky-visibility")
                        .selected_text(sky.visibility.label())
                        .show_ui(ui, |ui| {
                            for visibility in SkyVisibility::ALL {
                                changed |= ui
                                    .selectable_value(
                                        &mut sky.visibility,
                                        visibility,
                                        visibility.label(),
                                    )
                                    .changed();
                            }
                        });
                });
            }
            if sky.mode.uses_authored_texture() {
                changed |= texture_resource_picker(
                    ui,
                    "Sky Texture",
                    &mut sky.texture,
                    texture_options,
                    nav_target,
                );
                let help = match sky.mode {
                    SkyMode::QuakeLayered => {
                        "4bpp atlas with two equal square layers side by side, for example 256×128. Palette index 0 masks the foreground."
                    }
                    SkyMode::Cube => {
                        "1536×256 4bpp atlas containing six adjacent 256×256 faces and six 16-colour palettes."
                    }
                    _ => "",
                };
                ui.label(RichText::new(help).small().color(STUDIO_TEXT_WEAK));
                if sky.texture.is_none() {
                    ui.colored_label(
                        STUDIO_WARNING,
                        "Choose a textured Material before building this sky.",
                    );
                }
            }
            if sky.mode == SkyMode::Panorama {
                changed |= color_editor(ui, "Top", &mut sky.top_color);
                changed |= color_editor(ui, "Horizon", &mut sky.horizon_color);
                changed |= color_editor(ui, "Lower", &mut sky.lower_color);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Horizon").color(STUDIO_TEXT_WEAK));
                    let mut horizon = sky.horizon_percent.clamp(5, 95);
                    if ui
                        .add(egui::Slider::new(&mut horizon, 5..=95).suffix("%"))
                        .changed()
                    {
                        sky.horizon_percent = horizon;
                        changed = true;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Horizon Thickness").color(STUDIO_TEXT_WEAK));
                    let mut thickness = sky.horizon_thickness_percent.clamp(0, 80);
                    if ui
                        .add(egui::Slider::new(&mut thickness, 0..=80).suffix("%"))
                        .changed()
                    {
                        sky.horizon_thickness_percent = thickness;
                        changed = true;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Horizon Glow").color(STUDIO_TEXT_WEAK));
                    let mut glow = sky.horizon_glow_percent.clamp(0, 100);
                    if ui
                        .add(egui::Slider::new(&mut glow, 0..=100).suffix("%"))
                        .changed()
                    {
                        sky.horizon_glow_percent = glow;
                        changed = true;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Glow Direction").color(STUDIO_TEXT_WEAK));
                    let mut yaw = sky.horizon_glow_yaw_degrees.clamp(-180, 180);
                    if ui
                        .add(egui::Slider::new(&mut yaw, -180..=180).suffix("deg"))
                        .changed()
                    {
                        sky.horizon_glow_yaw_degrees = yaw;
                        changed = true;
                    }
                });
                ui.separator();
                changed |= ui.checkbox(&mut sky.sun_enabled, "Sun").changed();
                ui.add_enabled_ui(sky.sun_enabled, |ui| {
                    changed |= color_editor(ui, "Sun Inner", &mut sky.sun_color);
                    changed |= color_editor(ui, "Sun Border", &mut sky.sun_border_color);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Sun Direction").color(STUDIO_TEXT_WEAK));
                        let mut yaw = sky.sun_yaw_degrees.clamp(-180, 180);
                        if ui
                            .add(egui::Slider::new(&mut yaw, -180..=180).suffix("deg"))
                            .changed()
                        {
                            sky.sun_yaw_degrees = yaw;
                            changed = true;
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Sun Height").color(STUDIO_TEXT_WEAK));
                        let mut pitch = sky.sun_pitch_degrees.clamp(-30, 75);
                        if ui
                            .add(egui::Slider::new(&mut pitch, -30..=75).suffix("deg"))
                            .changed()
                        {
                            sky.sun_pitch_degrees = pitch;
                            changed = true;
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Sun Size").color(STUDIO_TEXT_WEAK));
                        let mut size = sky.sun_size_percent.clamp(1, 100);
                        if ui
                            .add(egui::Slider::new(&mut size, 1..=100).suffix("%"))
                            .changed()
                        {
                            sky.sun_size_percent = size;
                            changed = true;
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Sun Glow").color(STUDIO_TEXT_WEAK));
                        let mut glow = sky.sun_glow_percent.clamp(0, 100);
                        if ui
                            .add(egui::Slider::new(&mut glow, 0..=100).suffix("%"))
                            .changed()
                        {
                            sky.sun_glow_percent = glow;
                            changed = true;
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Glow Size").color(STUDIO_TEXT_WEAK));
                        let mut spread = sky.sun_glow_size_percent.clamp(0, 100);
                        if ui
                            .add(egui::Slider::new(&mut spread, 0..=100).suffix("%"))
                            .changed()
                        {
                            sky.sun_glow_size_percent = spread;
                            changed = true;
                        }
                    });
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Mountains").color(STUDIO_TEXT_WEAK));
                    let mut mountains = sky
                        .mountain_height_percent
                        .clamp(0, SKY_MOUNTAIN_HEIGHT_PERCENT_MAX);
                    if ui
                        .add(
                            egui::Slider::new(&mut mountains, 0..=SKY_MOUNTAIN_HEIGHT_PERCENT_MAX)
                                .suffix("%"),
                        )
                        .changed()
                    {
                        sky.mountain_height_percent = mountains;
                        changed = true;
                    }
                });
                changed |= color_editor(ui, "Mountain Peak", &mut sky.mountain_top_color);
                changed |= color_editor(ui, "Mountain Base", &mut sky.mountain_base_color);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Mountain Gap").color(STUDIO_TEXT_WEAK));
                    let mut gap = sky.mountain_gap_percent.clamp(0, 100);
                    if ui
                        .add(egui::Slider::new(&mut gap, 0..=100).suffix("%"))
                        .changed()
                    {
                        sky.mountain_gap_percent = gap;
                        changed = true;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Mountain Shape").color(STUDIO_TEXT_WEAK));
                    let mut roughness = sky.mountain_roughness_percent.clamp(0, 100);
                    if ui
                        .add(egui::Slider::new(&mut roughness, 0..=100).suffix("%"))
                        .changed()
                    {
                        sky.mountain_roughness_percent = roughness;
                        changed = true;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Mountain Layers").color(STUDIO_TEXT_WEAK));
                    let mut layers = sky.mountain_layer_count.clamp(1, 3);
                    if ui.add(egui::Slider::new(&mut layers, 1..=3)).changed() {
                        sky.mountain_layer_count = layers;
                        changed = true;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Cyclorama Columns").color(STUDIO_TEXT_WEAK));
                    let mut columns = sky
                        .skybox_columns
                        .clamp(SKYBOX_COLUMNS_MIN, SKYBOX_COLUMNS_MAX);
                    if ui
                        .add(egui::Slider::new(
                            &mut columns,
                            SKYBOX_COLUMNS_MIN..=SKYBOX_COLUMNS_MAX,
                        ))
                        .changed()
                    {
                        sky.skybox_columns = columns;
                        changed = true;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Cyclorama Rows").color(STUDIO_TEXT_WEAK));
                    let mut rows = sky.skybox_rows.clamp(SKYBOX_ROWS_MIN, SKYBOX_ROWS_MAX);
                    if ui
                        .add(egui::Slider::new(
                            &mut rows,
                            SKYBOX_ROWS_MIN..=SKYBOX_ROWS_MAX,
                        ))
                        .changed()
                    {
                        sky.skybox_rows = rows;
                        changed = true;
                    }
                });
                changed |= ui
                    .checkbox(&mut sky.match_room_fog, "Match world fog")
                    .changed();
                ui.separator();
                let cloud = &mut sky.cloud_layer;
                changed |= ui.checkbox(&mut cloud.enabled, "Cloud Layer").changed();
                ui.add_enabled_ui(cloud.enabled, |ui| {
                    changed |= color_editor(ui, "Cloud Color", &mut cloud.color);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Density").color(STUDIO_TEXT_WEAK));
                        changed |= ui
                            .add(egui::Slider::new(&mut cloud.density, 0..=255))
                            .changed();
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Altitude").color(STUDIO_TEXT_WEAK));
                        changed |= ui
                            .add(
                                egui::DragValue::new(&mut cloud.altitude)
                                    .speed(64.0)
                                    .range(64..=u16::MAX as i32),
                            )
                            .changed();
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Extent").color(STUDIO_TEXT_WEAK));
                        changed |= ui
                            .add(
                                egui::DragValue::new(&mut cloud.extent)
                                    .speed(256.0)
                                    .range(1024..=u16::MAX as i32),
                            )
                            .changed();
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Detail").color(STUDIO_TEXT_WEAK));
                        changed |= ui
                            .add(egui::Slider::new(&mut cloud.tile_count, 1..=16))
                            .changed();
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Scroll").color(STUDIO_TEXT_WEAK));
                        let mut sx = cloud.scroll_speed[0];
                        let mut sz = cloud.scroll_speed[1];
                        let sx_changed = ui
                            .add(
                                egui::DragValue::new(&mut sx)
                                    .speed(1.0)
                                    .range(-256..=256)
                                    .prefix("X "),
                            )
                            .changed();
                        let sz_changed = ui
                            .add(
                                egui::DragValue::new(&mut sz)
                                    .speed(1.0)
                                    .range(-256..=256)
                                    .prefix("Z "),
                            )
                            .changed();
                        if sx_changed || sz_changed {
                            cloud.scroll_speed = [sx, sz];
                            changed = true;
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Noise Seed").color(STUDIO_TEXT_WEAK));
                        let mut seed = cloud.noise_seed;
                        if ui
                            .add(
                                egui::DragValue::new(&mut seed)
                                    .speed(1.0)
                                    .hexadecimal(8, false, true),
                            )
                            .changed()
                        {
                            cloud.noise_seed = seed;
                            changed = true;
                        }
                    });
                });
            }
        });
    egui::CollapsingHeader::new(icons::label(icons::FOCUS, "Culling"))
        .default_open(false)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Draw Distance").color(STUDIO_TEXT_WEAK));
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut culling.draw_distance)
                            .speed(512.0)
                            .range(MIN_WORLD_DRAW_DISTANCE..=MAX_WORLD_DRAW_DISTANCE),
                    )
                    .changed();
                ui.label(RichText::new("units").color(STUDIO_TEXT_WEAK));
            });
        });
    egui::CollapsingHeader::new(icons::label(icons::WAYPOINT, "Far Vista"))
        .default_open(true)
        .show(ui, |ui| {
            changed |= ui.checkbox(&mut far_vista.enabled, "Enabled").changed();
            changed |= texture_resource_picker(
                ui,
                "Texture",
                &mut far_vista.texture,
                texture_options,
                nav_target,
            );
            egui::CollapsingHeader::new("Panel Textures")
                .default_open(false)
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("Filled panels override the repeated texture.")
                            .color(STUDIO_TEXT_WEAK),
                    );
                    for index in 0..psxed_project::FAR_VISTA_TEXTURE_PANEL_COUNT {
                        let label = format!("Panel {:02}", index + 1);
                        changed |= texture_resource_picker(
                            ui,
                            &label,
                            &mut far_vista.texture_panels[index],
                            texture_options,
                            nav_target,
                        );
                    }
                });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Radius").color(STUDIO_TEXT_WEAK));
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut far_vista.radius)
                            .speed(128.0)
                            .range(1_024..=65_535),
                    )
                    .changed();
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Height").color(STUDIO_TEXT_WEAK));
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut far_vista.height)
                            .speed(64.0)
                            .range(128..=32_768),
                    )
                    .changed();
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Y Offset").color(STUDIO_TEXT_WEAK));
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut far_vista.vertical_offset)
                            .speed(64.0)
                            .range(-32_768..=32_768),
                    )
                    .changed();
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Segments").color(STUDIO_TEXT_WEAK));
                let mut segments = far_vista.segments.clamp(3, 16);
                if ui.add(egui::Slider::new(&mut segments, 3..=16)).changed() {
                    far_vista.segments = segments;
                    changed = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Rotation").color(STUDIO_TEXT_WEAK));
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut far_vista.rotation_degrees)
                            .speed(1.0)
                            .suffix(" deg"),
                    )
                    .changed();
            });
            changed |= color_editor(ui, "Tint", &mut far_vista.tint);
            changed |= ui
                .checkbox(&mut far_vista.match_room_fog, "Match world fog")
                .changed();
        });
    changed
}

fn draw_message_pages_editor(
    ui: &mut egui::Ui,
    id_salt: &'static str,
    pages: &mut Vec<String>,
    desired_rows: usize,
) -> bool {
    let mut changed = false;
    if pages.is_empty() {
        pages.push(String::new());
        changed = true;
    }
    let mut remove = None;
    let can_remove = pages.len() > 1;
    for (index, page) in pages.iter_mut().enumerate() {
        ui.push_id((id_salt, index), |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("Page {}", index + 1))
                        .small()
                        .color(STUDIO_TEXT_WEAK),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(
                            can_remove,
                            egui::Button::new(icons::text(icons::TRASH, 12.0)).small(),
                        )
                        .on_hover_text("Remove this page")
                        .clicked()
                    {
                        remove = Some(index);
                    }
                });
            });
            changed |= ui
                .add(
                    egui::TextEdit::multiline(page)
                        .desired_rows(desired_rows)
                        .desired_width(f32::INFINITY),
                )
                .changed();
            if page.trim().is_empty() {
                ui.label(
                    RichText::new(format!(
                        "Page {} is blank. Enter message text before Play.",
                        index + 1
                    ))
                    .small()
                    .color(STUDIO_ERROR),
                );
            }
        });
    }
    if let Some(index) = remove {
        pages.remove(index);
        changed = true;
    }
    if ui.button(icons::label(icons::PLUS, "Add Page")).clicked() {
        pages.push(String::new());
        changed = true;
    }
    changed
}

pub(crate) fn draw_gameplay_camera_settings(
    ui: &mut egui::Ui,
    camera: &mut WorldCameraSettings,
) -> bool {
    let mut changed = false;
    egui::CollapsingHeader::new(icons::label(icons::FOCUS, "Follow Rig"))
        .default_open(true)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Distance").color(STUDIO_TEXT_WEAK));
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut camera.distance)
                            .speed(128.0)
                            .range(MIN_WORLD_CAMERA_DISTANCE..=MAX_WORLD_CAMERA_DISTANCE),
                    )
                    .on_hover_text("Preferred trailing distance from the player focus point.")
                    .changed();
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Height").color(STUDIO_TEXT_WEAK));
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut camera.height)
                            .speed(64.0)
                            .range(0..=MAX_WORLD_CAMERA_HEIGHT),
                    )
                    .on_hover_text("Camera origin height above the player root.")
                    .changed();
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Target Height").color(STUDIO_TEXT_WEAK));
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut camera.target_height)
                            .speed(64.0)
                            .range(0..=MAX_WORLD_CAMERA_HEIGHT),
                    )
                    .on_hover_text("Look-at focus height above the player root.")
                    .changed();
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Lock Rise").color(STUDIO_TEXT_WEAK));
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut camera.lock_rise_percent)
                            .speed(0.25)
                            .range(0..=psxed_project::MAX_WORLD_CAMERA_LOCK_RISE_PERCENT)
                            .suffix("%"),
                    )
                    .on_hover_text(
                        "Fixed additional camera height while locked, as a percentage of Height. The transition is smoothed without allowing collision or manual pitch to reduce the authored lift.",
                    )
                    .changed();
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Floor Clearance").color(STUDIO_TEXT_WEAK));
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut camera.min_floor_clearance)
                            .speed(16.0)
                            .range(0..=MAX_WORLD_CAMERA_MIN_FLOOR_CLEARANCE),
                    )
                    .on_hover_text("Minimum camera origin height above the sampled floor.")
                    .changed();
            });
            ui.separator();
            ui.weak("Input speed");
            changed |= draw_camera_orbit_speed_control(
                ui,
                &mut camera.orbit_speed_level,
                "Right-stick/manual camera orbit turn speed. Higher values orbit faster.",
            );
            ui.separator();
            ui.weak("Follow smoothing");
            changed |= draw_camera_speed_control(
                ui,
                "Position",
                &mut camera.position_lag_shift,
                "How quickly the camera origin catches up to its desired position.",
            );
            changed |= draw_camera_speed_control(
                ui,
                "Focus",
                &mut camera.focus_lag_shift,
                "How quickly the look-at point follows the player.",
            );
            changed |= draw_camera_speed_control(
                ui,
                "Boom Return",
                &mut camera.distance_lag_shift,
                "How quickly the camera returns to full distance after collision pulls it in.",
            );
            if ui.button("Reset").clicked() {
                *camera = WorldCameraSettings::default();
                changed = true;
            }
            if changed {
                *camera = camera.normalized();
            }
        });
    changed
}

fn draw_camera_orbit_speed_control(
    ui: &mut egui::Ui,
    orbit_speed_level: &mut u8,
    hover: &'static str,
) -> bool {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Orbit").color(STUDIO_TEXT_WEAK));
        ui.add(egui::DragValue::new(orbit_speed_level).speed(0.1).range(
            psxed_project::MIN_WORLD_CAMERA_ORBIT_SPEED_LEVEL
                ..=psxed_project::MAX_WORLD_CAMERA_ORBIT_SPEED_LEVEL,
        ))
        .on_hover_text(hover)
        .changed()
    })
    .inner
}

fn draw_camera_speed_control(
    ui: &mut egui::Ui,
    label: &'static str,
    lag_shift: &mut u8,
    hover: &'static str,
) -> bool {
    const MAX_SPEED_LEVEL: u8 = psxed_project::MAX_WORLD_CAMERA_LAG_SHIFT + 1;
    let mut speed = camera_speed_level_for_lag_shift(*lag_shift);
    let response = ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(STUDIO_TEXT_WEAK));
        ui.add(
            egui::DragValue::new(&mut speed)
                .speed(0.1)
                .range(1..=MAX_SPEED_LEVEL),
        )
        .on_hover_text(hover)
        .changed()
    });
    if response.inner {
        *lag_shift = camera_lag_shift_for_speed_level(speed);
    }
    response.inner
}

fn camera_speed_level_for_lag_shift(lag_shift: u8) -> u8 {
    const MAX_SPEED_LEVEL: u8 = psxed_project::MAX_WORLD_CAMERA_LAG_SHIFT + 1;
    MAX_SPEED_LEVEL - lag_shift.min(psxed_project::MAX_WORLD_CAMERA_LAG_SHIFT)
}

fn camera_lag_shift_for_speed_level(speed: u8) -> u8 {
    const MAX_SPEED_LEVEL: u8 = psxed_project::MAX_WORLD_CAMERA_LAG_SHIFT + 1;
    MAX_SPEED_LEVEL - speed.clamp(1, MAX_SPEED_LEVEL)
}

pub(crate) fn draw_gameplay_camera_render_preview(
    ui: &mut egui::Ui,
    preview: Option<EditorCameraPreviewPresentation>,
) {
    let width = ui.available_width().clamp(1.0, 360.0);
    let height = width * 0.75;
    let size = Vec2::new(width, height);
    if let Some(preview) = preview {
        egui::Frame::new()
            .fill(STUDIO_PANEL_HEADER)
            .stroke(egui::Stroke::new(1.0, Color32::from_rgb(44, 55, 70)))
            .corner_radius(6.0)
            .show(ui, |ui| {
                ui.add(egui::Image::new((preview.texture, size)).uv(preview.uv));
            });
        ui.weak("Rendered from this Camera component's starting gameplay rig.");
    } else {
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 6.0, STUDIO_PANEL_HEADER);
        painter.rect_stroke(
            rect,
            6.0,
            egui::Stroke::new(1.0, Color32::from_rgb(44, 55, 70)),
            egui::StrokeKind::Inside,
        );
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Select a Camera component to render preview",
            egui::TextStyle::Small.resolve(ui.style()),
            STUDIO_TEXT_WEAK,
        );
    }
}

pub(crate) fn draw_gameplay_camera_start_preview(ui: &mut egui::Ui, camera: WorldCameraSettings) {
    let width = ui.available_width().clamp(1.0, 360.0);
    let height = 150.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 6.0, STUDIO_PANEL_HEADER);
    painter.rect_stroke(
        rect,
        6.0,
        egui::Stroke::new(1.0, Color32::from_rgb(44, 55, 70)),
        egui::StrokeKind::Inside,
    );

    let left = rect.left() + 24.0;
    let right = rect.right() - 28.0;
    let top = rect.top() + 22.0;
    let floor_y = rect.bottom() - 26.0;
    let player_x = right;
    let effective_camera_height = camera.height.max(camera.min_floor_clearance);
    let lock_height_boost = camera
        .height
        .saturating_mul(i32::from(camera.lock_rise_percent))
        / 100;
    let locked_camera_height = camera.height.saturating_add(lock_height_boost);
    let effective_locked_camera_height = locked_camera_height.max(camera.min_floor_clearance);
    let max_vertical = camera
        .height
        .max(effective_camera_height)
        .max(effective_locked_camera_height)
        .max(camera.target_height)
        .max(camera.min_floor_clearance)
        .max(512) as f32;
    let x_scale = (player_x - left) / camera.distance.max(1) as f32;
    let y_scale = (floor_y - top) / max_vertical;
    let camera_x = player_x - camera.distance as f32 * x_scale;
    let desired_camera_y = floor_y - camera.height as f32 * y_scale;
    let camera_y = floor_y - effective_camera_height as f32 * y_scale;
    let locked_camera_y = floor_y - effective_locked_camera_height as f32 * y_scale;
    let target_y = floor_y - camera.target_height as f32 * y_scale;
    let clearance_y = floor_y - camera.min_floor_clearance as f32 * y_scale;

    let floor_a = egui::pos2(rect.left() + 12.0, floor_y);
    let floor_b = egui::pos2(rect.right() - 12.0, floor_y);
    let player_root = egui::pos2(player_x, floor_y);
    let target = egui::pos2(player_x, target_y);
    let camera_eye = egui::pos2(camera_x, camera_y);
    let locked_camera_eye = egui::pos2(camera_x, locked_camera_y);
    let desired_camera_eye = egui::pos2(camera_x, desired_camera_y);
    let clearance_a = egui::pos2(rect.left() + 12.0, clearance_y);
    let clearance_b = egui::pos2(rect.right() - 12.0, clearance_y);
    if camera.min_floor_clearance > 0 {
        painter.rect_filled(
            egui::Rect::from_min_max(clearance_a, floor_b),
            0.0,
            Color32::from_rgba_unmultiplied(230, 160, 90, 18),
        );
        painter.line_segment(
            [clearance_a, clearance_b],
            egui::Stroke::new(1.0, Color32::from_rgb(190, 130, 80)),
        );
        painter.text(
            clearance_b + Vec2::new(-4.0, -3.0),
            egui::Align2::RIGHT_BOTTOM,
            "floor clearance",
            egui::TextStyle::Small.resolve(ui.style()),
            Color32::from_rgb(210, 160, 100),
        );
    }
    painter.line_segment(
        [floor_a, floor_b],
        egui::Stroke::new(1.0, Color32::from_rgb(64, 75, 88)),
    );
    painter.line_segment(
        [player_root, target],
        egui::Stroke::new(2.0, Color32::from_rgb(94, 126, 160)),
    );
    painter.line_segment(
        [camera_eye, target],
        egui::Stroke::new(1.0, Color32::from_rgb(125, 145, 170)),
    );
    if lock_height_boost > 0 {
        painter.line_segment(
            [camera_eye, locked_camera_eye],
            egui::Stroke::new(2.0, Color32::from_rgb(80, 145, 225)),
        );
        painter.line_segment(
            [locked_camera_eye, target],
            egui::Stroke::new(1.5, Color32::from_rgb(105, 165, 235)),
        );
        painter.circle_filled(locked_camera_eye, 5.0, Color32::from_rgb(105, 165, 235));
        painter.text(
            locked_camera_eye + Vec2::new(8.0, 0.0),
            egui::Align2::LEFT_CENTER,
            "Locked",
            egui::TextStyle::Small.resolve(ui.style()),
            Color32::from_rgb(135, 185, 245),
        );
    }
    if effective_camera_height != camera.height {
        painter.circle_stroke(
            desired_camera_eye,
            4.0,
            egui::Stroke::new(1.0, Color32::from_rgb(155, 120, 80)),
        );
        painter.line_segment(
            [desired_camera_eye, camera_eye],
            egui::Stroke::new(1.0, Color32::from_rgb(190, 130, 80)),
        );
    }
    painter.circle_filled(target, 4.0, Color32::from_rgb(120, 170, 230));
    painter.circle_filled(camera_eye, 5.0, Color32::from_rgb(230, 190, 120));
    painter.circle_filled(player_root, 5.0, Color32::from_rgb(180, 220, 170));
    painter.text(
        camera_eye + Vec2::new(8.0, 0.0),
        egui::Align2::LEFT_CENTER,
        "Free",
        egui::TextStyle::Small.resolve(ui.style()),
        STUDIO_TEXT,
    );
    painter.text(
        target + Vec2::new(-7.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        "Look target",
        egui::TextStyle::Small.resolve(ui.style()),
        STUDIO_TEXT_WEAK,
    );
    painter.text(
        player_root + Vec2::new(0.0, 12.0),
        egui::Align2::CENTER_TOP,
        "Player root",
        egui::TextStyle::Small.resolve(ui.style()),
        STUDIO_TEXT_WEAK,
    );
    painter.text(
        rect.left_top() + Vec2::new(10.0, 8.0),
        egui::Align2::LEFT_TOP,
        format!(
            "{}u back  •  free {}u  •  locked {}u (+{}u){}",
            camera.distance,
            effective_camera_height,
            effective_locked_camera_height,
            lock_height_boost,
            if effective_camera_height != camera.height {
                " (clearance clamped)"
            } else {
                ""
            }
        ),
        egui::TextStyle::Small.resolve(ui.style()),
        STUDIO_TEXT_WEAK,
    );
}

pub(crate) fn entity_transform_editor(
    ui: &mut egui::Ui,
    transform: &mut psxed_project::Transform3,
    sector_size: i32,
    allow_full_rotation: bool,
    show_snap_to_floor: bool,
    snap_to_floor_requested: &mut bool,
) -> bool {
    let mut changed = false;
    let sector_size = sector_size.max(1);
    inspector_property_row(ui, icons::label(icons::MOVE, "Position"), |ui| {
        ui.horizontal_wrapped(|ui| {
            let mut x =
                node_transform_component_to_world_units(transform.translation[0], sector_size);
            let mut y =
                node_transform_component_to_world_units(transform.translation[1], sector_size);
            let mut z =
                node_transform_component_to_world_units(transform.translation[2], sector_size);
            let pos_changed = ui
                .add(
                    egui::DragValue::new(&mut x)
                        .prefix("X ")
                        .speed(HEIGHT_QUANTUM as f64),
                )
                .changed()
                | ui.add(
                    egui::DragValue::new(&mut y)
                        .prefix("Y ")
                        .speed(HEIGHT_QUANTUM as f64),
                )
                .changed()
                | ui.add(
                    egui::DragValue::new(&mut z)
                        .prefix("Z ")
                        .speed(HEIGHT_QUANTUM as f64),
                )
                .changed();
            if pos_changed {
                // World-unit nodes (BSP scenes) keep typed coordinates exact;
                // grid nodes snap to the height quantum as before.
                transform.translation = if sector_size == 1 {
                    [x as f32, y as f32, z as f32]
                } else {
                    [
                        node_transform_component_from_world_units(snap_height(x), sector_size),
                        node_transform_component_from_world_units(snap_height(y), sector_size),
                        node_transform_component_from_world_units(snap_height(z), sector_size),
                    ]
                };
                changed = true;
            }
            if show_snap_to_floor
                && ui
                    .button(icons::label(icons::CHEVRON_DOWN, "Snap to Floor"))
                    .on_hover_text(
                        "Move the complete Entity to the exact brush/BSP floor beneath it (End)",
                    )
                    .clicked()
            {
                *snap_to_floor_requested = true;
            }
        });
    });

    inspector_property_row(ui, icons::label(icons::ROTATE_3D, "Rotation"), |ui| {
        ui.horizontal_wrapped(|ui| {
            let mut x_rot = transform.rotation_degrees[0].rem_euclid(360.0);
            let mut y_rot = transform.rotation_degrees[1].rem_euclid(360.0);
            let mut z_rot = transform.rotation_degrees[2].rem_euclid(360.0);
            let mut rot_changed = false;
            if allow_full_rotation {
                rot_changed |= ui
                    .add(
                        egui::DragValue::new(&mut x_rot)
                            .prefix("X ")
                            .speed(1.0)
                            .range(0.0..=359.0),
                    )
                    .changed();
            }
            rot_changed |= ui
                .add(
                    egui::DragValue::new(&mut y_rot)
                        .prefix("Y ")
                        .speed(1.0)
                        .range(0.0..=359.0),
                )
                .changed();
            if allow_full_rotation {
                rot_changed |= ui
                    .add(
                        egui::DragValue::new(&mut z_rot)
                            .prefix("Z ")
                            .speed(1.0)
                            .range(0.0..=359.0),
                    )
                    .changed();
            }
            if rot_changed {
                transform.rotation_degrees = [
                    if allow_full_rotation {
                        x_rot.round().rem_euclid(360.0)
                    } else {
                        0.0
                    },
                    y_rot.round().rem_euclid(360.0),
                    if allow_full_rotation {
                        z_rot.round().rem_euclid(360.0)
                    } else {
                        0.0
                    },
                ];
                changed = true;
            }
        });
    });

    if !allow_full_rotation
        && (transform.rotation_degrees[0] != 0.0 || transform.rotation_degrees[2] != 0.0)
    {
        transform.rotation_degrees[0] = 0.0;
        transform.rotation_degrees[2] = 0.0;
        changed = true;
    }
    if transform.scale != [1.0, 1.0, 1.0] {
        transform.scale = [1.0, 1.0, 1.0];
        changed = true;
    }
    changed
}

pub(crate) fn arch_prop_transform_editor(
    ui: &mut egui::Ui,
    transform: &mut psxed_project::Transform3,
    sector_size: i32,
    geometry: psxed_project::ArchPropGeometry,
) -> bool {
    let mut changed = false;
    let sector_size = sector_size.max(1);
    inspector_property_row(ui, icons::label(icons::MOVE, "Grid anchor"), |ui| {
        ui.horizontal_wrapped(|ui| {
            let mut x =
                node_transform_component_to_world_units(transform.translation[0], sector_size);
            let mut y =
                node_transform_component_to_world_units(transform.translation[1], sector_size);
            let mut z =
                node_transform_component_to_world_units(transform.translation[2], sector_size);
            let position_changed = ui
                .add(
                    egui::DragValue::new(&mut x)
                        .prefix("X ")
                        .speed(sector_size as f64 * 0.5),
                )
                .changed()
                | ui.add(
                    egui::DragValue::new(&mut y)
                        .prefix("Y ")
                        .speed(HEIGHT_QUANTUM as f64),
                )
                .changed()
                | ui.add(
                    egui::DragValue::new(&mut z)
                        .prefix("Z ")
                        .speed(sector_size as f64 * 0.5),
                )
                .changed();
            if position_changed {
                transform.translation = [
                    node_transform_component_from_world_units(x, sector_size),
                    node_transform_component_from_world_units(snap_height(y), sector_size),
                    node_transform_component_from_world_units(z, sector_size),
                ];
                changed = true;
            }
        });
    });

    inspector_property_row(ui, icons::label(icons::ROTATE_3D, "Direction"), |ui| {
        ui.horizontal_wrapped(|ui| {
            let mut yaw = cardinal_yaw(transform.rotation_degrees[1]);
            for candidate in [0, 90, 180, 270] {
                if ui
                    .selectable_value(&mut yaw, candidate, format!("{candidate}°"))
                    .changed()
                {
                    transform.rotation_degrees = [0.0, yaw as f32, 0.0];
                    changed = true;
                }
            }
        });
    });

    changed |= snap_arch_prop_transform(transform, geometry, sector_size);
    changed
}

/// Enforce the ArchProp grid contract after placement, inspector edits, load,
/// or gizmo movement. Odd tile counts centre on a cell; even counts centre on
/// a grid line, so every outer footprint edge lands exactly on room geometry.
pub(crate) fn snap_arch_prop_transform(
    transform: &mut psxed_project::Transform3,
    geometry: psxed_project::ArchPropGeometry,
    sector_size: i32,
) -> bool {
    let sector_size = sector_size.max(1);
    let yaw = cardinal_yaw(transform.rotation_degrees[1]);
    let swapped = yaw == 90 || yaw == 270;
    let (tiles_x, tiles_z) = if swapped {
        (geometry.depth_tiles, geometry.span_tiles)
    } else {
        (geometry.span_tiles, geometry.depth_tiles)
    };
    let snap_axis = |value: f32, tiles: u8| {
        let offset = if tiles.max(1) & 1 == 0 { 0.0 } else { 0.5 };
        (value - offset).round() + offset
    };
    let next = [
        snap_axis(transform.translation[0], tiles_x),
        node_transform_component_from_world_units(
            snap_height(node_transform_component_to_world_units(
                transform.translation[1],
                sector_size,
            )),
            sector_size,
        ),
        snap_axis(transform.translation[2], tiles_z),
    ];
    let next_rotation = [0.0, yaw as f32, 0.0];
    let mut changed = false;
    if transform.translation != next {
        transform.translation = next;
        changed = true;
    }
    if transform.rotation_degrees != next_rotation {
        transform.rotation_degrees = next_rotation;
        changed = true;
    }
    if transform.scale != [1.0, 1.0, 1.0] {
        transform.scale = [1.0, 1.0, 1.0];
        changed = true;
    }
    changed
}

pub(crate) fn light_transform_editor(
    ui: &mut egui::Ui,
    transform: &mut psxed_project::Transform3,
    sector_size: i32,
) -> bool {
    let mut changed = normalise_light_transform(transform, sector_size);
    let sector_size = sector_size.max(1);
    inspector_property_row(ui, icons::label(icons::MOVE, "Position"), |ui| {
        ui.horizontal_wrapped(|ui| {
            let mut x =
                node_transform_component_to_world_units(transform.translation[0], sector_size);
            let mut y =
                node_transform_component_to_world_units(transform.translation[1], sector_size);
            let mut z =
                node_transform_component_to_world_units(transform.translation[2], sector_size);
            let pos_changed = ui
                .add(
                    egui::DragValue::new(&mut x)
                        .prefix("X ")
                        .speed(HEIGHT_QUANTUM as f64),
                )
                .changed()
                | ui.add(
                    egui::DragValue::new(&mut y)
                        .prefix("Y ")
                        .speed(HEIGHT_QUANTUM as f64),
                )
                .changed()
                | ui.add(
                    egui::DragValue::new(&mut z)
                        .prefix("Z ")
                        .speed(HEIGHT_QUANTUM as f64),
                )
                .changed();
            if pos_changed {
                // Same exact-typing rule as the entity editor for world-unit
                // (BSP) lights.
                transform.translation = if sector_size == 1 {
                    [x as f32, y as f32, z as f32]
                } else {
                    [
                        node_transform_component_from_world_units(snap_height(x), sector_size),
                        node_transform_component_from_world_units(snap_height(y), sector_size),
                        node_transform_component_from_world_units(snap_height(z), sector_size),
                    ]
                };
                changed = true;
            }
        });
    });
    changed
}

/// Translate a node by `steps` gizmo increments along `direction`, a
/// unit world-space vector (a gizmo basis column). Global space passes
/// a world axis here, which keeps the old single-component stepping
/// and snapping; Local space passes the node's rotated axis, where the
/// quantum applies along the direction instead of per component so a
/// diagonal slide doesn't zig.
pub(crate) fn node_gizmo_translation(
    node: &psxed_project::SceneNode,
    start: [f32; 3],
    direction: [f32; 3],
    steps: i32,
    sector_size: i32,
    world_quantum: i32,
) -> [f32; 3] {
    let mut translation = start;
    let sector_size = sector_size.max(1);
    // World-unit nodes (BSP scenes, sector_size == 1) step and snap on the
    // caller's quantum (the brush grid, or 1 when dragging free); grid nodes
    // keep the legacy HEIGHT_QUANTUM step in sector units.
    let world_units = sector_size == 1;
    let entity_step = if world_units {
        world_quantum.max(1) as f32
    } else {
        node_transform_component_from_world_units(HEIGHT_QUANTUM, sector_size)
    };
    let step = match &node.kind {
        NodeKind::Entity
        | NodeKind::PointLight { .. }
        | NodeKind::ParticleEmitter { .. }
        | NodeKind::ImageProp { .. }
        | NodeKind::BoxProp { .. }
        | NodeKind::CylinderProp { .. } => entity_step,
        NodeKind::ArchProp { .. } if direction[1].abs() > 0.5 => entity_step,
        _ => 1.0,
    };
    let axis_aligned = direction.iter().filter(|c| c.abs() > 1e-4).count() <= 1;
    for index in 0..3 {
        if direction[index].abs() <= 1e-4 {
            continue;
        }
        translation[index] = start[index] + direction[index] * steps as f32 * step;
        // World-axis drags keep the legacy per-component snap; rotated
        // directions own their quantum along the drag axis instead.
        if axis_aligned && steps != 0 {
            match &node.kind {
                NodeKind::Entity
                | NodeKind::PointLight { .. }
                | NodeKind::ParticleEmitter { .. }
                | NodeKind::ImageProp { .. }
                | NodeKind::BoxProp { .. }
                | NodeKind::CylinderProp { .. }
                | NodeKind::ArchProp { .. } => {
                    translation[index] = if world_units {
                        snap_world_units_component(translation[index], world_quantum)
                    } else {
                        snap_node_transform_component_to_world_step(translation[index], sector_size)
                    };
                }
                _ => {}
            }
        }
    }
    translation
}

pub(crate) fn node_gizmo_plane_translation(
    node: &psxed_project::SceneNode,
    start: [f32; 3],
    plane: NodeGizmoPlane,
    delta_world: [f32; 3],
    sector_size: i32,
    world_quantum: i32,
) -> [f32; 3] {
    let mut translation = start;
    let sector_size = sector_size.max(1);
    let world_units = sector_size == 1;
    for axis in plane.axes() {
        let index = axis.index();
        translation[index] = start[index] + delta_world[index] / sector_size as f32;
    }

    match &node.kind {
        NodeKind::Entity
        | NodeKind::PointLight { .. }
        | NodeKind::ParticleEmitter { .. }
        | NodeKind::ImageProp { .. }
        | NodeKind::BoxProp { .. }
        | NodeKind::ArchProp { .. } => {
            for axis in plane.axes() {
                let index = axis.index();
                if delta_world[index].abs() > f32::EPSILON {
                    translation[index] = if world_units {
                        snap_world_units_component(translation[index], world_quantum)
                    } else {
                        snap_node_transform_component_to_world_step(translation[index], sector_size)
                    };
                }
            }
            translation
        }
        _ => translation,
    }
}

pub(crate) fn node_gizmo_drag_has_motion(drag: &NodeGizmoDrag) -> bool {
    match drag.handle {
        NodeGizmoHandle::Axis(_) | NodeGizmoHandle::BoxFace(_) => drag.current_steps != 0,
        NodeGizmoHandle::Plane(plane) => {
            let [a, b] = plane.axes();
            drag.current_plane_delta_world[a.index()].abs() > f32::EPSILON
                || drag.current_plane_delta_world[b.index()].abs() > f32::EPSILON
        }
    }
}

pub(crate) fn apply_box_prop_face_gizmo_resize(
    node: &mut psxed_project::SceneNode,
    start_translation: [f32; 3],
    start_box_prop_vertices: Option<[[i16; 3]; psxed_project::BOX_PROP_VERTEX_COUNT]>,
    face: u8,
    steps: i32,
    sector_size: i32,
) {
    let NodeKind::BoxProp { vertices, .. } = &mut node.kind else {
        return;
    };
    let Some(start_vertices) = start_box_prop_vertices else {
        return;
    };
    let (axis, positive) = box_prop_face_axis_and_sign(face);
    let index = axis.index();
    let (mut min, mut max) = (i32::MAX, i32::MIN);
    for vertex in start_vertices {
        min = min.min(i32::from(vertex[index]));
        max = max.max(i32::from(vertex[index]));
    }
    if min >= max {
        return;
    }

    let delta = steps.saturating_mul(HEIGHT_QUANTUM);
    let (new_min, new_max) = if positive {
        (
            min,
            max.saturating_add(delta).clamp(
                min.saturating_add(1),
                min.saturating_add(i32::from(MAX_IMAGE_PROP_SIZE)),
            ),
        )
    } else {
        (
            min.saturating_sub(delta).clamp(
                max.saturating_sub(i32::from(MAX_IMAGE_PROP_SIZE)),
                max.saturating_sub(1),
            ),
            max,
        )
    };
    let old_size = (max - min) as f32;
    let new_size = (new_max - new_min) as f32;
    let old_anchor = if index == PrimitiveGizmoAxis::Y.index() {
        min as f32
    } else {
        (min + max) as f32 * 0.5
    };
    let new_anchor = if index == PrimitiveGizmoAxis::Y.index() {
        new_min as f32
    } else {
        (new_min + new_max) as f32 * 0.5
    };
    let anchor_shift = new_anchor - old_anchor;

    *vertices = start_vertices;
    for vertex in vertices.iter_mut() {
        let t = (f32::from(vertex[index]) - min as f32) / old_size;
        let remapped = new_min as f32 + t * new_size - anchor_shift;
        vertex[index] = remapped.round().clamp(
            -f32::from(MAX_IMAGE_PROP_SIZE),
            f32::from(MAX_IMAGE_PROP_SIZE),
        ) as i16;
    }

    let mut local_shift = [0.0; 3];
    local_shift[index] = anchor_shift;
    let rotation = euler_degrees_to_matrix(node.transform.rotation_degrees);
    let world_shift = rotate_vector_by_matrix(&rotation, local_shift);
    let sector_size = sector_size.max(1) as f32;
    node.transform.translation = [
        start_translation[0] + world_shift[0] / sector_size,
        start_translation[1] + world_shift[1] / sector_size,
        start_translation[2] + world_shift[2] / sector_size,
    ];
}

pub(crate) fn node_gizmo_rotation(
    node: &psxed_project::SceneNode,
    start: [f32; 3],
    axis: PrimitiveGizmoAxis,
    steps: i32,
    space: RotationSpace,
) -> [f32; 3] {
    if !node_kind_supports_transform_gizmo(&node.kind, TransformGizmoMode::Rotate) {
        return start;
    }
    let supported = node_rotation_axes(&node.kind);
    if !supported.contains(&axis) {
        return start;
    }
    // Compose the delta as a true rotation about the chosen world or
    // node axis. Editing one Euler component directly is only correct
    // while the other two are zero; `rotate_euler_degrees` keeps that
    // exact-degree fast path and handles the general case by matrix.
    let mut result = rotate_euler_degrees(start, axis.index(), steps as f32, space);
    // Force any unsupported axes back to zero so node kinds with
    // legacy yaw-only semantics don't accumulate stale roll/pitch.
    for (i, _) in [
        PrimitiveGizmoAxis::X,
        PrimitiveGizmoAxis::Y,
        PrimitiveGizmoAxis::Z,
    ]
    .iter()
    .enumerate()
    .filter(|(_, a)| !supported.contains(a))
    {
        result[i] = 0.0;
    }
    result
}

/// Rotation axes supported by `kind`'s transform gizmo. Groups, entities,
/// ImageProps, and BoxProps rotate freely around all three world
/// axes; every other gizmo target keeps the legacy yaw-only behavior
/// so spawn / trigger transforms stay flat without stray pitch / roll.
pub(crate) fn node_rotation_axes(kind: &NodeKind) -> &'static [PrimitiveGizmoAxis] {
    match kind {
        NodeKind::Group
        | NodeKind::Entity
        | NodeKind::ImageProp { .. }
        | NodeKind::BoxProp { .. }
        | NodeKind::CylinderProp { .. } => &[
            PrimitiveGizmoAxis::X,
            PrimitiveGizmoAxis::Y,
            PrimitiveGizmoAxis::Z,
        ],
        NodeKind::ArchProp { .. } => &[PrimitiveGizmoAxis::Y],
        _ => &[PrimitiveGizmoAxis::Y],
    }
}

pub(crate) fn apply_node_gizmo_scale(
    node: &mut psxed_project::SceneNode,
    start_image_prop_size: Option<[u16; 2]>,
    start_box_prop_vertices: Option<[[i16; 3]; psxed_project::BOX_PROP_VERTEX_COUNT]>,
    start_cylinder_prop_geometry: Option<psxed_project::CylinderPropGeometry>,
    start_arch_prop_geometry: Option<psxed_project::ArchPropGeometry>,
    axis: PrimitiveGizmoAxis,
    steps: i32,
) {
    match &mut node.kind {
        NodeKind::ImageProp { width, height, .. } => {
            let Some([start_width, start_height]) = start_image_prop_size else {
                return;
            };
            let delta = steps.saturating_mul(HEIGHT_QUANTUM);
            let resize_axis = |start: u16| -> u16 {
                (i32::from(start) + delta).clamp(1, i32::from(MAX_IMAGE_PROP_SIZE)) as u16
            };
            match axis {
                PrimitiveGizmoAxis::X => *width = resize_axis(start_width),
                PrimitiveGizmoAxis::Y => *height = resize_axis(start_height),
                PrimitiveGizmoAxis::Z => {
                    *width = resize_axis(start_width);
                    *height = resize_axis(start_height);
                }
            }
        }
        NodeKind::BoxProp { vertices, .. } => {
            let Some(start_vertices) = start_box_prop_vertices else {
                return;
            };
            apply_box_prop_gizmo_scale(vertices, start_vertices, axis, steps);
        }
        NodeKind::CylinderProp { geometry, .. } => {
            let Some(start) = start_cylinder_prop_geometry else {
                return;
            };
            let delta = steps.saturating_mul(HEIGHT_QUANTUM);
            let resize = |value: u16| {
                (i32::from(value) + delta).clamp(1, i32::from(MAX_IMAGE_PROP_SIZE)) as u16
            };
            *geometry = start;
            match axis {
                PrimitiveGizmoAxis::X => geometry.radius[0] = resize(start.radius[0]),
                PrimitiveGizmoAxis::Y => geometry.height = resize(start.height),
                PrimitiveGizmoAxis::Z => geometry.radius[1] = resize(start.radius[1]),
            }
        }
        NodeKind::ArchProp { geometry, .. } => {
            let Some(start) = start_arch_prop_geometry else {
                return;
            };
            *geometry = start;
            match axis {
                PrimitiveGizmoAxis::X => {
                    geometry.span_tiles = (i32::from(start.span_tiles) + steps).clamp(
                        i32::from(psxed_project::ARCH_PROP_MIN_TILES),
                        i32::from(psxed_project::ARCH_PROP_MAX_TILES),
                    ) as u8;
                }
                PrimitiveGizmoAxis::Y => {
                    geometry.rise_quanta = (i32::from(start.rise_quanta) + steps)
                        .clamp(1, i32::from(psxed_project::ARCH_PROP_MAX_HEIGHT_QUANTA))
                        as u16;
                }
                PrimitiveGizmoAxis::Z => {
                    geometry.depth_tiles = (i32::from(start.depth_tiles) + steps).clamp(
                        i32::from(psxed_project::ARCH_PROP_MIN_TILES),
                        i32::from(psxed_project::ARCH_PROP_MAX_TILES),
                    ) as u8;
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn apply_box_prop_gizmo_scale(
    vertices: &mut [[i16; 3]; psxed_project::BOX_PROP_VERTEX_COUNT],
    start_vertices: [[i16; 3]; psxed_project::BOX_PROP_VERTEX_COUNT],
    axis: PrimitiveGizmoAxis,
    steps: i32,
) {
    let idx = axis.index();
    let mut min = i32::MAX;
    let mut max = i32::MIN;
    for vertex in start_vertices {
        let value = i32::from(vertex[idx]);
        min = min.min(value);
        max = max.max(value);
    }
    if min >= max {
        *vertices = start_vertices;
        return;
    }

    let delta = steps.saturating_mul(HEIGHT_QUANTUM);
    let old_size = max - min;
    let new_size = (old_size + delta).clamp(1, i32::from(MAX_IMAGE_PROP_SIZE));
    let pivot = if axis == PrimitiveGizmoAxis::Y {
        min as f32
    } else {
        (min + max) as f32 * 0.5
    };
    let scale = new_size as f32 / old_size as f32;
    *vertices = start_vertices;
    for vertex in vertices.iter_mut() {
        let value = pivot + (f32::from(vertex[idx]) - pivot) * scale;
        vertex[idx] = value.round().clamp(
            -f32::from(MAX_IMAGE_PROP_SIZE),
            f32::from(MAX_IMAGE_PROP_SIZE),
        ) as i16;
    }
}

/// Return the UV span that preserves the material's native room-surface
/// texel density on one Box Prop face.
///
/// Room surfaces map one complete source texture across one sector. Box
/// faces can be arbitrary quadrilaterals, so use the average length of each
/// pair of opposing edges and repeat the source texture proportionally.
/// `GridUvTransform` stores inclusive u8 spans, hence the `- 1`.
pub(crate) fn box_prop_face_native_texel_span(
    vertices: [[i16; 3]; psxed_project::BOX_PROP_VERTEX_COUNT],
    face: usize,
    sector_size: i32,
    texture_size: [u16; 2],
) -> [u16; 2] {
    let [top_left, top_right, bottom_right, bottom_left] =
        psxed_project::BOX_PROP_FACE_VERTEX_INDICES
            [face.min(psxed_project::BOX_PROP_FACE_COUNT.saturating_sub(1))];
    let edge_length = |a: usize, b: usize| {
        let delta = [
            f32::from(vertices[b][0] - vertices[a][0]),
            f32::from(vertices[b][1] - vertices[a][1]),
            f32::from(vertices[b][2] - vertices[a][2]),
        ];
        (delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2]).sqrt()
    };
    let face_width =
        (edge_length(top_left, top_right) + edge_length(bottom_left, bottom_right)) * 0.5;
    let face_height =
        (edge_length(top_left, bottom_left) + edge_length(top_right, bottom_right)) * 0.5;
    let sector_size = sector_size.max(1) as f32;
    let native_span = |length: f32, texels: u16| {
        ((length / sector_size * f32::from(texels.max(1))).round() as i32 - 1)
            .clamp(1, u8::MAX as i32) as u16
    };
    [
        native_span(face_width, texture_size[0]),
        native_span(face_height, texture_size[1]),
    ]
}

pub(crate) fn node_kind_supports_transform_gizmo(
    kind: &NodeKind,
    mode: TransformGizmoMode,
) -> bool {
    match mode {
        TransformGizmoMode::Move => matches!(
            kind,
            NodeKind::Group
                | NodeKind::Entity
                | NodeKind::PointLight { .. }
                | NodeKind::ParticleEmitter { .. }
                | NodeKind::ImageProp { .. }
                | NodeKind::BoxProp { .. }
                | NodeKind::CylinderProp { .. }
                | NodeKind::ArchProp { .. }
                | NodeKind::MeshInstance { .. }
                | NodeKind::SpawnPoint { .. }
                | NodeKind::Portal { .. }
        ),
        TransformGizmoMode::Rotate => matches!(
            kind,
            NodeKind::Group
                | NodeKind::Entity
                | NodeKind::ImageProp { .. }
                | NodeKind::BoxProp { .. }
                | NodeKind::CylinderProp { .. }
                | NodeKind::ArchProp { .. }
                | NodeKind::MeshInstance { .. }
                | NodeKind::SpawnPoint { .. }
                | NodeKind::Portal { .. }
        ),
        TransformGizmoMode::Scale => {
            matches!(
                kind,
                NodeKind::Group
                    | NodeKind::ImageProp { .. }
                    | NodeKind::BoxProp { .. }
                    | NodeKind::CylinderProp { .. }
                    | NodeKind::ArchProp { .. }
            )
        }
    }
}

pub(crate) fn normalise_light_transform(
    transform: &mut psxed_project::Transform3,
    sector_size: i32,
) -> bool {
    let mut changed = false;
    // Grid lights snap their Y to the height quantum; world-unit lights
    // (BSP scenes, sector_size == 1) keep authored Y exact.
    if sector_size > 1 {
        let snapped_y = snap_light_transform_y(transform.translation[1], sector_size);
        if transform.translation[1] != snapped_y {
            transform.translation[1] = snapped_y;
            changed = true;
        }
    }
    if transform.rotation_degrees != [0.0, 0.0, 0.0] {
        transform.rotation_degrees = [0.0, 0.0, 0.0];
        changed = true;
    }
    if transform.scale != [1.0, 1.0, 1.0] {
        transform.scale = [1.0, 1.0, 1.0];
        changed = true;
    }
    changed
}

pub(crate) fn node_transform_component_to_world_units(value: f32, sector_size: i32) -> i32 {
    (value * sector_size.max(1) as f32).round() as i32
}

pub(crate) fn node_transform_component_from_world_units(value: i32, sector_size: i32) -> f32 {
    value as f32 / sector_size.max(1) as f32
}

pub(crate) fn snap_node_transform_component_to_world_step(value: f32, sector_size: i32) -> f32 {
    let world = node_transform_component_to_world_units(value, sector_size);
    node_transform_component_from_world_units(snap_height(world), sector_size)
}

/// Snap a raw world-unit component to a caller-chosen grid (the brush
/// `snap_units` in BSP scenes). Quantum 1 rounds to whole units.
pub(crate) fn snap_world_units_component(value: f32, quantum: i32) -> f32 {
    let q = quantum.max(1) as f32;
    (value / q).round() * q
}

pub(crate) fn snap_light_transform_y(value: f32, sector_size: i32) -> f32 {
    snap_node_transform_component_to_world_step(value, sector_size)
}

pub(crate) fn image_prop_default_size_for_sector(sector_size: i32) -> u16 {
    sector_size.clamp(1, MAX_IMAGE_PROP_SIZE as i32) as u16
}

pub(crate) fn cardinal_yaw(degrees: f32) -> i32 {
    let normalized = degrees.rem_euclid(360.0);
    ((normalized / 90.0).round() as i32 * 90).rem_euclid(360)
}

pub(crate) fn transform_editor(
    ui: &mut egui::Ui,
    label: &str,
    values: &mut [f32; 3],
    speed: f64,
) -> bool {
    inspector_property_row(ui, icons::label(transform_icon(label), label), |ui| {
        ui.horizontal_wrapped(|ui| {
            let mut changed = false;
            changed |= ui
                .add(
                    egui::DragValue::new(&mut values[0])
                        .prefix("X ")
                        .speed(speed),
                )
                .changed();
            changed |= ui
                .add(
                    egui::DragValue::new(&mut values[1])
                        .prefix("Y ")
                        .speed(speed),
                )
                .changed();
            changed |= ui
                .add(
                    egui::DragValue::new(&mut values[2])
                        .prefix("Z ")
                        .speed(speed),
                )
                .changed();
            changed
        })
        .inner
    })
}

pub(crate) fn transform_icon(label: &str) -> char {
    match label {
        "Position" => icons::MOVE,
        "Rotation" => icons::ROTATE_3D,
        "Scale" => icons::SCALE_3D,
        _ => icons::WAYPOINT,
    }
}

pub(crate) struct AnimatorClipContext {
    pub(crate) model_name: String,
    pub(crate) clips: Vec<String>,
    pub(crate) clip_frame_counts: Vec<Option<u16>>,
    pub(crate) clip_in_place_defaults: Vec<bool>,
    pub(crate) profile_name: Option<String>,
    pub(crate) profile_action_clips: [Option<u16>; psxed_project::CHARACTER_ANIMATION_ACTION_COUNT],
}

pub(crate) fn draw_box_prop_nudge_buttons(
    ui: &mut egui::Ui,
    vertices: &mut [[i16; 3]; psxed_project::BOX_PROP_VERTEX_COUNT],
    indices: &[usize],
) -> bool {
    let mut changed = false;
    let step = HEIGHT_QUANTUM as i16;
    for (axis, label) in [(0usize, "X"), (1, "Y"), (2, "Z")] {
        if ui.small_button(format!("{label}-")).clicked() {
            nudge_box_prop_vertices(vertices, indices, axis, -step);
            changed = true;
        }
        if ui.small_button(format!("{label}+")).clicked() {
            nudge_box_prop_vertices(vertices, indices, axis, step);
            changed = true;
        }
    }
    changed
}

pub(crate) fn draw_box_prop_break_flag_checkbox(
    ui: &mut egui::Ui,
    flags: &mut u16,
    flag: u16,
    label: &str,
) -> bool {
    let mut checked = *flags & flag != 0;
    if !ui.checkbox(&mut checked, label).changed() {
        return false;
    }
    if checked {
        *flags |= flag;
    } else {
        *flags &= !flag;
    }
    true
}

pub(crate) fn nudge_box_prop_vertices(
    vertices: &mut [[i16; 3]; psxed_project::BOX_PROP_VERTEX_COUNT],
    indices: &[usize],
    axis: usize,
    delta: i16,
) {
    for index in indices {
        let value = i32::from(vertices[*index][axis]) + i32::from(delta);
        vertices[*index][axis] = value.clamp(
            -i32::from(MAX_IMAGE_PROP_SIZE),
            i32::from(MAX_IMAGE_PROP_SIZE),
        ) as i16;
    }
}

pub(crate) fn selected_animator_clip_context(
    project: &ProjectDocument,
    selected: NodeId,
    project_root: &std::path::Path,
) -> Option<AnimatorClipContext> {
    let scene = project.active_scene();
    let node = scene.node(selected)?;
    if !matches!(node.kind, NodeKind::Animator { .. }) {
        return None;
    }
    let host = scene.node(node.parent?)?;
    let model_id = host.children.iter().find_map(|child_id| {
        scene.node(*child_id).and_then(|child| match child.kind {
            NodeKind::ModelRenderer {
                model: Some(model), ..
            } => Some(model),
            _ => None,
        })
    })?;
    let profile = host.children.iter().find_map(|child_id| {
        scene.node(*child_id).and_then(|child| match child.kind {
            NodeKind::CharacterController {
                character: Some(character),
                ..
            } => project
                .resource(character)
                .and_then(|resource| match &resource.data {
                    ResourceData::Character(character) => Some((resource.name.clone(), character)),
                    _ => None,
                }),
            _ => None,
        })
    });
    let mut profile_action_clips = [None; psxed_project::CHARACTER_ANIMATION_ACTION_COUNT];
    let profile_name = profile.as_ref().map(|(name, _)| name.clone());
    if let Some((_, character)) = profile {
        for action in psxed_project::CharacterAnimationAction::ALL {
            profile_action_clips[action.to_index()] =
                character_profile_action_clip(project, model_id, character, action);
        }
    }
    let authoring_labels = collect_animation_clip_authoring_labels(project);
    project
        .resources
        .iter()
        .find(|resource| resource.id == model_id)
        .and_then(|resource| match &resource.data {
            ResourceData::Model(_) => {
                let clips = project.resolved_model_animation_clips(model_id);
                Some(AnimatorClipContext {
                    model_name: resource.name.clone(),
                    clip_frame_counts: clips
                        .iter()
                        .map(|clip| animation_clip_frame_count(&clip.psxanim_path, project_root))
                        .collect(),
                    clip_in_place_defaults: clips
                        .iter()
                        .map(|clip| clip.calibration.in_place)
                        .collect(),
                    clips: clips
                        .iter()
                        .map(|clip| {
                            clip.animation_resource.map_or_else(
                                || clip.name.clone(),
                                |clip_id| {
                                    authoring_labels
                                        .get(&clip_id)
                                        .cloned()
                                        .unwrap_or_else(|| clip.name.clone())
                                },
                            )
                        })
                        .collect(),
                    profile_name,
                    profile_action_clips,
                })
            }
            _ => None,
        })
}

fn animation_clip_frame_count(psxanim_path: &str, project_root: &std::path::Path) -> Option<u16> {
    let path = psxed_project::model_import::resolve_path(psxanim_path, Some(project_root));
    let mut file = std::fs::File::open(path).ok()?;
    let mut header = [0u8; 20];
    std::io::Read::read_exact(&mut file, &mut header).ok()?;
    if &header[0..4] != b"PSXA" {
        return None;
    }
    let version = u16::from_le_bytes([header[4], header[5]]);
    if version != 1 && version != 2 {
        return None;
    }
    let payload_len = u32::from_le_bytes([header[8], header[9], header[10], header[11]]);
    if payload_len < 8 {
        return None;
    }
    let frame_count = u16::from_le_bytes([header[14], header[15]]);
    (frame_count > 0).then_some(frame_count)
}

pub(crate) fn character_profile_action_clip(
    project: &ProjectDocument,
    model_id: ResourceId,
    character: &psxed_project::CharacterResource,
    action: psxed_project::CharacterAnimationAction,
) -> Option<u16> {
    let set = character.animation_set.and_then(|id| {
        project
            .resource(id)
            .and_then(|resource| match &resource.data {
                ResourceData::AnimationSet(set) => Some(set),
                _ => None,
            })
    })?;
    let clip = set.action_clip(action)?;
    project.resolved_model_animation_index(model_id, clip)
}

/// Everything the node-kind editor needs beyond the node itself: resource
/// option lists, animator clip info, and the resize/navigation outputs.
pub(crate) struct NodeKindEditorContext<'a> {
    pub(crate) material_options: &'a [(ResourceId, String)],
    pub(crate) material_texture_dimensions: &'a [(ResourceId, [u16; 2])],
    pub(crate) texture_options: &'a [(ResourceId, String)],
    pub(crate) room_options: &'a [(NodeId, String)],
    pub(crate) destructible_options: &'a [(NodeId, String)],
    pub(crate) model_options: &'a [(ResourceId, String, Vec<String>)],
    pub(crate) character_options: &'a [(ResourceId, String)],
    /// Each Character's own tuning, used as the shown value and the seed for a
    /// per-placement override.
    pub(crate) character_defaults: &'a [(ResourceId, psxed_project::CharacterControllerSettings)],
    /// Each Character's named loadouts, in declaration order, so a placement
    /// can pick one by index without reaching back into the project.
    pub(crate) character_loadouts: &'a [(ResourceId, Vec<String>)],
    pub(crate) weapon_options: &'a [(ResourceId, String)],
    pub(crate) boost_module_options: &'a [(ResourceId, String)],
    pub(crate) animator_clip_context: Option<&'a AnimatorClipContext>,
    pub(crate) inherited_sector_size: i32,
    pub(crate) room_grid_resize: &'a mut Option<(u16, u16)>,
    pub(crate) nav_target: &'a mut Option<ResourceId>,
    pub(crate) character_preview_action: &'a mut Option<psxed_project::CharacterAnimationAction>,
    pub(crate) camera_preview: Option<EditorCameraPreviewPresentation>,
}

/// What the placement's loadout picker shows, or `None` when it should not be
/// drawn at all.
///
/// A Character with one way to be equipped should not grow a control that only
/// ever offers "Default". The exception is a placement that still holds a
/// selection after the loadouts were deleted: hiding the control there would
/// leave a value on the node that cannot be seen or cleared, even though the
/// cook quietly treats it as the default.
pub(crate) fn loadout_picker_label(loadout: Option<u16>, names: &[String]) -> Option<&str> {
    if names.is_empty() && loadout.is_none() {
        return None;
    }
    // A stale index cooks as the default, so name it as such rather than
    // showing an index the Character no longer has.
    Some(
        loadout
            .and_then(|index| names.get(index as usize))
            .map_or("Default", String::as_str),
    )
}

/// Loadout picker for a placed Character Controller.
///
/// Hidden entirely when the Character declares no loadouts, which is the
/// common case: a character with one way to be equipped should not grow a
/// control that only ever offers "Default".
fn draw_character_loadout_picker(
    ui: &mut egui::Ui,
    character: Option<ResourceId>,
    loadout: &mut Option<u16>,
    character_loadouts: &[(ResourceId, Vec<String>)],
) -> bool {
    let names = character
        .and_then(|id| {
            character_loadouts
                .iter()
                .find_map(|(candidate, names)| (*candidate == id).then_some(names.as_slice()))
        })
        .unwrap_or_default();
    let Some(selected_label) = loadout_picker_label(*loadout, names) else {
        return false;
    };

    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label("Loadout");
        egui::ComboBox::from_id_salt("character-controller-loadout")
            .selected_text(selected_label)
            .show_ui(ui, |ui| {
                changed |= ui
                    .selectable_value(loadout, None, "Default")
                    .changed();
                for (index, name) in names.iter().enumerate() {
                    changed |= ui
                        .selectable_value(loadout, Some(index as u16), name)
                        .changed();
                }
            });
    })
    .response
    .on_hover_text("Which of the Character's named loadouts this placement carries.");
    changed
}

pub(crate) fn draw_node_kind_editor(
    ui: &mut egui::Ui,
    kind: &mut NodeKind,
    ctx: NodeKindEditorContext<'_>,
) -> bool {
    if matches!(
        kind,
        NodeKind::Section { .. } | NodeKind::WaterVolume { .. } | NodeKind::Portal { .. }
    ) {
        ui.colored_label(
            Color32::from_rgb(220, 160, 80),
            "This retired grid-world node is read-only. BSP brushes and ordinary entities are the supported authoring path.",
        );
        return false;
    }
    let NodeKindEditorContext {
        material_options,
        material_texture_dimensions,
        texture_options,
        room_options,
        destructible_options,
        model_options,
        character_options,
        character_defaults,
        character_loadouts,
        weapon_options,
        boost_module_options,
        animator_clip_context,
        inherited_sector_size,
        room_grid_resize,
        nav_target,
        character_preview_action,
        camera_preview,
    } = ctx;
    let mut changed = false;
    match kind {
        NodeKind::Node | NodeKind::Node3D => {
            ui.weak("Organisational transform node");
        }
        NodeKind::Group => {
            ui.weak("Authoring group. Closed groups edit as one object; double-click to edit their contents.");
        }
        NodeKind::Entity => {
            ui.weak("Entity host. Add component children for rendering, collision, interaction, lighting, or logic.");
        }
        NodeKind::World { .. } => {
            ui.weak("BSP world root; holds global physics, camera, sky, and far vista settings.");
        }
        NodeKind::Section { grid } => {
            ui.horizontal(|ui| {
                ui.label(icons::text(icons::GRID, 12.0).color(STUDIO_TEXT_WEAK));
                ui.label("Grid");
                let mut new_w = grid.width;
                let mut new_d = grid.depth;
                let w_changed = ui
                    .add(
                        egui::DragValue::new(&mut new_w)
                            .speed(0.1)
                            .range(1..=64)
                            .prefix("W "),
                    )
                    .changed();
                let d_changed = ui
                    .add(
                        egui::DragValue::new(&mut new_d)
                            .speed(0.1)
                            .range(1..=64)
                            .prefix("D "),
                    )
                    .changed();
                if w_changed || d_changed {
                    *room_grid_resize = Some((new_w, new_d));
                    changed = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label(icons::text(icons::WAYPOINT, 12.0).color(STUDIO_TEXT_WEAK));
                ui.label("World Grid");
                ui.label(
                    RichText::new(format!("{inherited_sector_size} units")).color(STUDIO_TEXT_WEAK),
                );
            });
            ui.horizontal(|ui| {
                ui.label(icons::text(icons::BOX, 12.0).color(STUDIO_TEXT_WEAK));
                ui.label(format!(
                    "{} populated sectors",
                    grid.populated_sector_count()
                ));
            });
            changed |= color_editor(ui, "Ambient Light", &mut grid.ambient_color);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Preset").color(STUDIO_TEXT_WEAK));
                if ui.small_button("Low").clicked() {
                    grid.ambient_color = [32, 32, 32];
                    changed = true;
                }
                if ui.small_button("Neutral").clicked() {
                    grid.ambient_color = [128, 128, 128];
                    changed = true;
                }
                if ui.small_button("Warm").clicked() {
                    grid.ambient_color = [96, 80, 64];
                    changed = true;
                }
            });
            changed |= ui
                .checkbox(&mut grid.fog_enabled, icons::label(icons::SCAN, "Fog"))
                .changed();
            if grid.fog_enabled {
                changed |= color_editor(ui, "Fog Color", &mut grid.fog_color);
                ui.horizontal(|ui| {
                    ui.label(icons::text(icons::SCAN, 12.0).color(STUDIO_TEXT_WEAK));
                    ui.label("Fog Range");
                    let near_changed = ui
                        .add(
                            egui::DragValue::new(&mut grid.fog_near)
                                .prefix("Near ")
                                .speed(128.0)
                                .range(0..=262_144),
                        )
                        .changed();
                    let far_changed = ui
                        .add(
                            egui::DragValue::new(&mut grid.fog_far)
                                .prefix("Far ")
                                .speed(128.0)
                                .range(128..=262_144),
                        )
                        .changed();
                    if near_changed || far_changed {
                        grid.fog_near = grid.fog_near.max(0);
                        grid.fog_far = grid.fog_far.max(grid.fog_near + 128);
                        changed = true;
                    }
                });
            }
            ui.separator();
            changed |= ui
                .checkbox(
                    &mut grid.atmosphere_enabled,
                    icons::label(icons::SCAN, "Atmosphere"),
                )
                .changed();
            if grid.atmosphere_enabled {
                changed |= color_editor(ui, "Particle Color", &mut grid.atmosphere_color);
                changed |= drag_i32(ui, "Density", &mut grid.atmosphere_density, 0, 96);
                changed |= drag_i32(ui, "Fall Speed", &mut grid.atmosphere_fall_speed_q4, 0, 64);
                changed |= drag_i32(
                    ui,
                    "Wind Speed",
                    &mut grid.atmosphere_wind_speed_q4,
                    -64,
                    64,
                );
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Preset").color(STUDIO_TEXT_WEAK));
                    if ui.small_button("Ash").clicked() {
                        grid.atmosphere_color = [58, 52, 44];
                        grid.atmosphere_density = 44;
                        grid.atmosphere_fall_speed_q4 = 7;
                        grid.atmosphere_wind_speed_q4 = 2;
                        changed = true;
                    }
                    if ui.small_button("Snow").clicked() {
                        grid.atmosphere_color = [198, 205, 214];
                        grid.atmosphere_density = 36;
                        grid.atmosphere_fall_speed_q4 = 10;
                        grid.atmosphere_wind_speed_q4 = 1;
                        changed = true;
                    }
                    if ui.small_button("Sparse").clicked() {
                        grid.atmosphere_color = [74, 66, 56];
                        grid.atmosphere_density = 18;
                        grid.atmosphere_fall_speed_q4 = 5;
                        grid.atmosphere_wind_speed_q4 = 1;
                        changed = true;
                    }
                });
            }
        }
        NodeKind::WaterVolume {
            material,
            cells,
            settings,
        } => {
            ui.weak("Painted, floor-bound water. Every cell extends from its terrain tile up to the authored height; there is no separate volume bottom.");
            changed |= material_picker(
                ui,
                "Surface material",
                material,
                material_options,
                nav_target,
            );
            ui.horizontal(|ui| {
                ui.label("Painted cells");
                ui.monospace(cells.len().to_string());
            });
            ui.separator();
            ui.label(RichText::new("Water behaviour").strong());
            changed |= ui
                .add(
                    egui::DragValue::new(&mut settings.height_above_floor)
                        .range(1..=8192)
                        .speed(8.0)
                        .prefix("Water height "),
                )
                .on_hover_text(
                    "Distance from the lowest point of the terrain tile to the water surface. Each painted cell calculates its surface from its own floor geometry.",
                )
                .changed();
            changed |= ui
                .add(
                    egui::DragValue::new(&mut settings.lethal_depth)
                        .range(1..=8192)
                        .prefix("Death threshold "),
                )
                .on_hover_text(
                    "Gameplay threshold only: water at least this tall is lethal. It does not change the volume geometry.",
                )
                .changed();
            changed |= ui
                .add(
                    egui::Slider::new(&mut settings.movement_percent, 10..=100)
                        .text("Movement speed"),
                )
                .on_hover_text(
                    "Percentage of normal walk and run speed retained in non-lethal water. 70% means movement is exactly 70% of normal.",
                )
                .changed();
            let classification = if settings.height_above_floor >= settings.lethal_depth {
                "Lethal water"
            } else {
                "Wading water"
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new("Result").color(STUDIO_TEXT_WEAK));
                ui.label(classification);
            });
            changed |= ui
                .add(
                    egui::DragValue::new(&mut settings.death_submerge_depth)
                        .range(0..=2048)
                        .prefix("Submerge "),
                )
                .on_hover_text(
                    "How far below the surface the actor must fall before deep-water death begins.",
                )
                .changed();
            changed |= ui
                .add(
                    egui::DragValue::new(&mut settings.death_delay_ticks)
                        .range(1..=240)
                        .prefix("Death delay ")
                        .suffix(" ticks"),
                )
                .changed();
            if changed {
                *settings = settings.normalized();
            }
        }
        NodeKind::MeshInstance {
            mesh,
            material,
            animation_clip,
        } => {
            // Look up the bound model (if any) so we can show
            // a real clip-name combo for animation_clip.
            let bound_model: Option<&(ResourceId, String, Vec<String>)> =
                mesh.and_then(|id| model_options.iter().find(|(rid, _, _)| *rid == id));

            ui.horizontal(|ui| {
                ui.label(icons::text(icons::BOX, 12.0).color(STUDIO_TEXT_WEAK));
                ui.label(match (mesh, bound_model) {
                    (Some(_), Some((_, name, _))) => format!("Model: {name}"),
                    (Some(id), None) => format!("Mesh resource #{}", id.raw()),
                    (None, _) => "No mesh resource assigned".to_string(),
                });
            });
            ui.separator();
            // Same `material_picker` the face inspector uses, so
            // the `→` jump button is available here too. (Models
            // ignore this field -- material is baked into .psxmdl.)
            changed |= material_picker(ui, "Material", material, material_options, nav_target);

            // Animation clip override. When the bound mesh is a
            // Model, render a clip-name combo so the user picks
            // by name; otherwise fall back to a numeric override
            // for legacy mesh instances.
            ui.horizontal(|ui| {
                ui.label(RichText::new("Animation clip").color(STUDIO_TEXT_WEAK));
                if let Some((_, _, clips)) = bound_model {
                    let preview = match *animation_clip {
                        Some(idx) => clips
                            .get(idx as usize)
                            .map(|n| n.as_str())
                            .unwrap_or("(invalid)")
                            .to_string(),
                        None => "(inherit default)".to_string(),
                    };
                    egui::ComboBox::from_id_salt("mesh_instance_clip")
                        .selected_text(preview)
                        .height(360.0)
                        .show_ui(ui, |ui| {
                            ui.set_min_width(380.0);
                            let filter = animation_picker_filter(
                                ui,
                                ui.id().with(("mesh_instance_clip", "filter")),
                            );
                            let matching = clips
                                .iter()
                                .filter(|name| animation_name_matches_filter(name, &filter))
                                .count();
                            ui.label(
                                RichText::new(format!(
                                    "{matching} of {} compatible clips",
                                    clips.len()
                                ))
                                .small()
                                .color(STUDIO_TEXT_WEAK),
                            );
                            ui.separator();
                            if ui
                                .selectable_label(animation_clip.is_none(), "(inherit default)")
                                .clicked()
                            {
                                *animation_clip = None;
                                changed = true;
                            }
                            for (i, name) in clips
                                .iter()
                                .enumerate()
                                .filter(|(_, name)| animation_name_matches_filter(name, &filter))
                            {
                                let label = format!("{i}: {name}");
                                if ui
                                    .selectable_label(*animation_clip == Some(i as u16), label)
                                    .clicked()
                                {
                                    *animation_clip = Some(i as u16);
                                    changed = true;
                                }
                            }
                        });
                    if let Some(idx) = *animation_clip {
                        if (idx as usize) >= clips.len() {
                            ui.colored_label(
                                Color32::from_rgb(220, 160, 80),
                                format!("clip {idx} out of range ({} clips)", clips.len()),
                            );
                        }
                    }
                } else {
                    let mut current = animation_clip.map(|i| i as i32).unwrap_or(-1);
                    let response = ui.add(
                        egui::DragValue::new(&mut current)
                            .speed(0.1)
                            .range(-1..=255)
                            .custom_formatter(|n, _| {
                                if n < 0.0 {
                                    "default".to_string()
                                } else {
                                    format!("{}", n as i32)
                                }
                            }),
                    );
                    if response.changed() {
                        *animation_clip = if current < 0 {
                            None
                        } else {
                            Some(current as u16)
                        };
                        changed = true;
                    }
                }
            });
        }
        NodeKind::ImageProp {
            material,
            width,
            height,
            cylindrical_billboard,
            collision_enabled,
            collision_size,
            destructible,
        } => {
            ui.weak(
                "Flat material-backed image plane. Transform position is the bottom-center anchor.",
            );
            changed |= material_picker(ui, "Material", material, material_options, nav_target);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Size").color(STUDIO_TEXT_WEAK));
                let mut w = i32::from(*width);
                let mut h = i32::from(*height);
                let w_changed = ui
                    .add(
                        egui::DragValue::new(&mut w)
                            .speed(1.0)
                            .range(1..=i32::from(MAX_IMAGE_PROP_SIZE))
                            .prefix("W "),
                    )
                    .changed();
                let h_changed = ui
                    .add(
                        egui::DragValue::new(&mut h)
                            .speed(1.0)
                            .range(1..=i32::from(MAX_IMAGE_PROP_SIZE))
                            .prefix("H "),
                    )
                    .changed();
                if w_changed || h_changed {
                    *width = w.clamp(1, i32::from(MAX_IMAGE_PROP_SIZE)) as u16;
                    *height = h.clamp(1, i32::from(MAX_IMAGE_PROP_SIZE)) as u16;
                    changed = true;
                }
            });
            changed |= ui
                .checkbox(cylindrical_billboard, "Face camera cylindrically")
                .changed();
            ui.separator();
            changed |= ui.checkbox(collision_enabled, "Collision").changed();
            ui.add_enabled_ui(*collision_enabled, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Box Size").color(STUDIO_TEXT_WEAK));
                    let mut box_x = i32::from(collision_size[0]);
                    let mut box_y = i32::from(collision_size[1]);
                    let mut box_z = i32::from(collision_size[2]);
                    let box_changed = ui
                        .add(
                            egui::DragValue::new(&mut box_x)
                                .speed(1.0)
                                .range(1..=i32::from(MAX_IMAGE_PROP_SIZE))
                                .prefix("X "),
                        )
                        .changed()
                        | ui.add(
                            egui::DragValue::new(&mut box_y)
                                .speed(1.0)
                                .range(1..=i32::from(MAX_IMAGE_PROP_SIZE))
                                .prefix("Y "),
                        )
                        .changed()
                        | ui.add(
                            egui::DragValue::new(&mut box_z)
                                .speed(1.0)
                                .range(1..=i32::from(MAX_IMAGE_PROP_SIZE))
                                .prefix("Z "),
                        )
                        .changed();
                    if box_changed {
                        *collision_size = [
                            box_x.clamp(1, i32::from(MAX_IMAGE_PROP_SIZE)) as u16,
                            box_y.clamp(1, i32::from(MAX_IMAGE_PROP_SIZE)) as u16,
                            box_z.clamp(1, i32::from(MAX_IMAGE_PROP_SIZE)) as u16,
                        ];
                        changed = true;
                    }
                });
            });
            ui.separator();
            let selected = destructible
                .and_then(|id| {
                    destructible_options
                        .iter()
                        .find(|(candidate, _)| *candidate == id)
                        .map(|(_, name)| name.as_str())
                })
                .unwrap_or("None");
            egui::ComboBox::from_id_salt("image-prop-destructible")
                .selected_text(selected)
                .show_ui(ui, |ui| {
                    changed |= ui.selectable_value(destructible, None, "None").changed();
                    for (id, name) in destructible_options {
                        changed |= ui.selectable_value(destructible, Some(*id), name).changed();
                    }
                });
            ui.weak(
                "Optional shared Destructible state. Breaking it removes this card from rendering and collision.",
            );
        }
        NodeKind::BoxProp {
            materials,
            uvs,
            vertices,
            collision_enabled,
            break_flags,
            erosion,
        } => {
            ui.weak(
                "Editable material-backed box. Transform position is the bottom-center anchor.",
            );
            ui.horizontal(|ui| {
                ui.label(RichText::new("Faces").color(STUDIO_TEXT_WEAK));
                if ui.small_button("Fill from Front").clicked() {
                    if let Some(material) = materials[0] {
                        for slot in materials.iter_mut() {
                            *slot = Some(material);
                        }
                        changed = true;
                    }
                }
                if ui.small_button("Copy Front UV").clicked() {
                    let front = uvs[0];
                    uvs.fill(front);
                    changed = true;
                }
            });
            ui.weak(
                "Each face has independent offset, span, rotation, and mirroring. 1:1 keeps the project's native world-space texel density instead of stretching one tile across the face.",
            );
            for (face, name) in psxed_project::BOX_PROP_FACE_NAMES.iter().enumerate() {
                egui::CollapsingHeader::new(*name)
                    .id_salt(("box-prop-face", face))
                    .default_open(face == 0)
                    .show(ui, |ui| {
                        changed |= material_picker(
                            ui,
                            "Material",
                            &mut materials[face],
                            material_options,
                            nav_target,
                        );
                        let texture_size = materials[face].and_then(|material| {
                            material_texture_dimensions
                                .iter()
                                .find_map(|(id, size)| (*id == material).then_some(*size))
                        });
                        ui.horizontal(|ui| {
                            let one_to_one = ui
                                .add_enabled(texture_size.is_some(), egui::Button::new("1:1 Texels"))
                                .on_hover_text(
                                    format!(
                                        "Use the material's native texel density: one texture tile per {inherited_sector_size} world units, repeated across larger faces."
                                    ),
                                );
                            if one_to_one.clicked() {
                                uvs[face].span = box_prop_face_native_texel_span(
                                    *vertices,
                                    face,
                                    inherited_sector_size,
                                    texture_size.unwrap_or([1, 1]),
                                );
                                changed = true;
                            }
                            if ui
                                .small_button("Fit once")
                                .on_hover_text("Stretch one complete material tile across this face.")
                                .clicked()
                            {
                                uvs[face].span = [0, 0];
                                changed = true;
                            }
                        });
                        changed |= uv_transform_controls(&mut uvs[face], ui).changed();
                    });
            }
            ui.separator();
            changed |= ui.checkbox(collision_enabled, "Collision").changed();
            ui.separator();
            ui.label(RichText::new("Break On").color(STUDIO_TEXT_WEAK));
            ui.horizontal(|ui| {
                changed |= draw_box_prop_break_flag_checkbox(
                    ui,
                    break_flags,
                    psx_level::box_prop_flags::BREAK_ON_WALK,
                    "Walk",
                );
                changed |= draw_box_prop_break_flag_checkbox(
                    ui,
                    break_flags,
                    psx_level::box_prop_flags::BREAK_ON_RUN,
                    "Run",
                );
                changed |= draw_box_prop_break_flag_checkbox(
                    ui,
                    break_flags,
                    psx_level::box_prop_flags::BREAK_ON_ATTACK,
                    "Attack",
                );
            });
            ui.separator();
            egui::CollapsingHeader::new("Procedural Erosion")
                .default_open(erosion.is_enabled())
                .show(ui, |ui| {
                    ui.weak(
                        "One shared seeded field erodes the editable box cage from any enabled direction.",
                    );
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Templates").color(STUDIO_TEXT_WEAK));
                        if ui.small_button("Broken top").clicked() {
                            erosion.apply_broken_top_template();
                            changed = true;
                        }
                        if ui.small_button("Boulder").clicked() {
                            erosion.apply_boulder_template();
                            changed = true;
                        }
                        if ui.small_button("Clear").clicked() {
                            erosion.clear();
                            changed = true;
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Seed").color(STUDIO_TEXT_WEAK));
                        changed |= ui
                            .add(egui::DragValue::new(&mut erosion.seed).speed(1.0))
                            .changed();
                        if ui.small_button("New variation").clicked() {
                            erosion.seed = erosion.seed.wrapping_add(1);
                            changed = true;
                        }
                        ui.label(RichText::new("Detail").color(STUDIO_TEXT_WEAK));
                        changed |= ui
                            .add(
                                egui::DragValue::new(&mut erosion.detail)
                                    .range(1..=psxed_project::BOX_PROP_EROSION_MAX_DETAIL),
                            )
                            .changed();
                    });
                    ui.separator();
                    for (face, name) in psxed_project::BOX_PROP_FACE_NAMES.iter().enumerate() {
                        let direction = &mut erosion.directions[face];
                        ui.horizontal(|ui| {
                            changed |= ui.checkbox(&mut direction.enabled, *name).changed();
                            if direction.enabled {
                                ui.label(RichText::new("Depth").color(STUDIO_TEXT_WEAK));
                                changed |= ui
                                    .add(
                                        egui::DragValue::new(&mut direction.amount)
                                            .range(0..=45)
                                            .suffix("%"),
                                    )
                                    .changed();
                                ui.label(RichText::new("Cover").color(STUDIO_TEXT_WEAK));
                                changed |= ui
                                    .add(
                                        egui::DragValue::new(&mut direction.coverage)
                                            .range(0..=100)
                                            .suffix("%"),
                                    )
                                    .changed();
                            }
                        });
                        if direction.enabled {
                            ui.horizontal(|ui| {
                                ui.add_space(24.0);
                                ui.label(RichText::new("Rough").color(STUDIO_TEXT_WEAK));
                                changed |= ui
                                    .add(
                                        egui::DragValue::new(&mut direction.roughness)
                                            .range(0..=100)
                                            .suffix("%"),
                                    )
                                    .changed();
                                ui.label(RichText::new("Feature").color(STUDIO_TEXT_WEAK));
                                changed |= ui
                                    .add(
                                        egui::DragValue::new(&mut direction.feature_size)
                                            .range(1..=psxed_project::BOX_PROP_EROSION_MAX_DETAIL),
                                    )
                                    .changed();
                                ui.label(RichText::new("Protect edge").color(STUDIO_TEXT_WEAK));
                                changed |= ui
                                    .add(
                                        egui::DragValue::new(&mut direction.edge_protection)
                                            .range(0..=100)
                                            .suffix("%"),
                                    )
                                    .changed();
                            });
                        }
                    }
                });
            ui.separator();
            egui::CollapsingHeader::new("Move Faces")
                .default_open(false)
                .show(ui, |ui| {
                    for (name, indices) in psxed_project::BOX_PROP_FACE_NAMES
                        .iter()
                        .zip(BOX_PROP_FACE_VERTEX_INDICES.iter())
                    {
                        ui.horizontal(|ui| {
                            ui.label(*name);
                            changed |= draw_box_prop_nudge_buttons(ui, vertices, indices);
                        });
                    }
                });
            egui::CollapsingHeader::new("Move Edges")
                .default_open(false)
                .show(ui, |ui| {
                    for (edge, indices) in BOX_PROP_EDGE_VERTEX_INDICES.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(format!("E{edge}"));
                            changed |= draw_box_prop_nudge_buttons(ui, vertices, indices);
                        });
                    }
                });
            ui.separator();
            egui::CollapsingHeader::new("Vertices")
                .default_open(true)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Preset").color(STUDIO_TEXT_WEAK));
                        if ui.small_button("Reset Cube").clicked() {
                            *vertices = psxed_project::box_prop_vertices_for_size(
                                psxed_project::DEFAULT_BOX_PROP_SIZE,
                            );
                            changed = true;
                        }
                    });
                    for (index, vertex) in vertices.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(format!("V{index}"));
                            let mut x = i32::from(vertex[0]);
                            let mut y = i32::from(vertex[1]);
                            let mut z = i32::from(vertex[2]);
                            let vertex_changed = ui
                                .add(
                                    egui::DragValue::new(&mut x)
                                        .speed(1.0)
                                        .range(
                                            -i32::from(MAX_IMAGE_PROP_SIZE)
                                                ..=i32::from(MAX_IMAGE_PROP_SIZE),
                                        )
                                        .prefix("X "),
                                )
                                .changed()
                                | ui.add(
                                    egui::DragValue::new(&mut y)
                                        .speed(1.0)
                                        .range(
                                            -i32::from(MAX_IMAGE_PROP_SIZE)
                                                ..=i32::from(MAX_IMAGE_PROP_SIZE),
                                        )
                                        .prefix("Y "),
                                )
                                .changed()
                                | ui.add(
                                    egui::DragValue::new(&mut z)
                                        .speed(1.0)
                                        .range(
                                            -i32::from(MAX_IMAGE_PROP_SIZE)
                                                ..=i32::from(MAX_IMAGE_PROP_SIZE),
                                        )
                                        .prefix("Z "),
                                )
                                .changed();
                            if vertex_changed {
                                *vertex = [
                                    x.clamp(
                                        -i32::from(MAX_IMAGE_PROP_SIZE),
                                        i32::from(MAX_IMAGE_PROP_SIZE),
                                    ) as i16,
                                    y.clamp(
                                        -i32::from(MAX_IMAGE_PROP_SIZE),
                                        i32::from(MAX_IMAGE_PROP_SIZE),
                                    ) as i16,
                                    z.clamp(
                                        -i32::from(MAX_IMAGE_PROP_SIZE),
                                        i32::from(MAX_IMAGE_PROP_SIZE),
                                    ) as i16,
                                ];
                                changed = true;
                            }
                        });
                    }
                });
        }
        NodeKind::CylinderProp {
            materials,
            uvs,
            geometry,
            collision_enabled,
        } => {
            ui.weak(
                "Low-poly radial prop. Transform position is the bottom-center anchor; geometry is generated only for preview and cooking.",
            );
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Templates").color(STUDIO_TEXT_WEAK));
                if ui.small_button("Column").clicked() {
                    *geometry = psxed_project::CylinderPropGeometry::default();
                    changed = true;
                }
                if ui.small_button("Broken column").clicked() {
                    *geometry = psxed_project::CylinderPropGeometry::default();
                    geometry.broken_ends = psxed_project::CylinderBrokenEnds::Top;
                    geometry.top_bulge.enabled = true;
                    changed = true;
                }
                if ui.small_button("Pedestal").clicked() {
                    *geometry = psxed_project::CylinderPropGeometry::default();
                    geometry.base_bulge.enabled = true;
                    geometry.top_bulge.enabled = true;
                    geometry.base_bulge.radius_percent = 135;
                    geometry.top_bulge.radius_percent = 135;
                    changed = true;
                }
            });

            ui.separator();
            ui.label(RichText::new("Shape").color(STUDIO_TEXT_WEAK));
            ui.horizontal(|ui| {
                ui.label("Radius");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut geometry.radius[0])
                            .range(1..=MAX_IMAGE_PROP_SIZE)
                            .prefix("X "),
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut geometry.radius[1])
                            .range(1..=MAX_IMAGE_PROP_SIZE)
                            .prefix("Z "),
                    )
                    .changed();
            });
            ui.horizontal(|ui| {
                ui.label("Height");
                changed |= ui
                    .add(egui::DragValue::new(&mut geometry.height).range(1..=MAX_IMAGE_PROP_SIZE))
                    .changed();
                ui.label("Sides");
                changed |= ui
                    .add(egui::DragValue::new(&mut geometry.sides).range(
                        psxed_project::CYLINDER_PROP_MIN_SIDES
                            ..=psxed_project::CYLINDER_PROP_MAX_SIDES,
                    ))
                    .changed();
            });
            ui.horizontal(|ui| {
                ui.label("Top radius");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut geometry.top_radius_percent)
                            .range(10..=300)
                            .suffix("%"),
                    )
                    .changed();
                changed |= ui.checkbox(collision_enabled, "Collision").changed();
            });

            ui.separator();
            egui::CollapsingHeader::new("Base / top profile")
                .default_open(geometry.base_bulge.enabled || geometry.top_bulge.enabled)
                .show(ui, |ui| {
                    for (label, bulge) in [
                        ("Base collar", &mut geometry.base_bulge),
                        ("Top collar", &mut geometry.top_bulge),
                    ] {
                        ui.horizontal(|ui| {
                            changed |= ui.checkbox(&mut bulge.enabled, label).changed();
                            if bulge.enabled {
                                changed |= ui
                                    .add(
                                        egui::DragValue::new(&mut bulge.radius_percent)
                                            .range(100..=250)
                                            .prefix("Radius ")
                                            .suffix("%"),
                                    )
                                    .changed();
                                changed |= ui
                                    .add(
                                        egui::DragValue::new(&mut bulge.height_percent)
                                            .range(2..=45)
                                            .prefix("Height ")
                                            .suffix("%"),
                                    )
                                    .changed();
                            }
                        });
                    }
                });

            ui.separator();
            egui::CollapsingHeader::new("Broken ends")
                .default_open(!matches!(
                    geometry.broken_ends,
                    psxed_project::CylinderBrokenEnds::None
                ))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Break");
                        egui::ComboBox::from_id_salt("cylinder-prop-broken-ends")
                            .selected_text(geometry.broken_ends.label())
                            .show_ui(ui, |ui| {
                                for candidate in [
                                    psxed_project::CylinderBrokenEnds::None,
                                    psxed_project::CylinderBrokenEnds::Top,
                                    psxed_project::CylinderBrokenEnds::Bottom,
                                    psxed_project::CylinderBrokenEnds::Both,
                                ] {
                                    changed |= ui
                                        .selectable_value(
                                            &mut geometry.broken_ends,
                                            candidate,
                                            candidate.label(),
                                        )
                                        .changed();
                                }
                            });
                        if ui.small_button("New variation").clicked() {
                            geometry.seed = geometry.seed.wrapping_add(1);
                            changed = true;
                        }
                    });
                    if !matches!(
                        geometry.broken_ends,
                        psxed_project::CylinderBrokenEnds::None
                    ) {
                        ui.horizontal(|ui| {
                            ui.label("Depth");
                            changed |= ui
                                .add(
                                    egui::DragValue::new(&mut geometry.fracture_depth_percent)
                                        .range(2..=80)
                                        .suffix("%"),
                                )
                                .changed();
                            ui.label("Roughness");
                            changed |= ui
                                .add(
                                    egui::DragValue::new(&mut geometry.fracture_roughness)
                                        .range(0..=100)
                                        .suffix("%"),
                                )
                                .changed();
                            ui.label("Seed");
                            changed |= ui
                                .add(egui::DragValue::new(&mut geometry.seed).speed(1.0))
                                .changed();
                        });
                    }
                });

            ui.separator();
            ui.label(RichText::new("Materials / UV").color(STUDIO_TEXT_WEAK));
            for (slot, name) in psxed_project::CYLINDER_PROP_MATERIAL_NAMES
                .iter()
                .enumerate()
            {
                egui::CollapsingHeader::new(*name)
                    .id_salt(("cylinder-prop-material", slot))
                    .default_open(slot == 0)
                    .show(ui, |ui| {
                        changed |= material_picker(
                            ui,
                            "Material",
                            &mut materials[slot],
                            material_options,
                            nav_target,
                        );
                        let texture_size = materials[slot].and_then(|material| {
                            material_texture_dimensions
                                .iter()
                                .find_map(|(id, size)| (*id == material).then_some(*size))
                        });
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    texture_size.is_some(),
                                    egui::Button::new("1:1 Texels"),
                                )
                                .clicked()
                            {
                                let texture = texture_size.unwrap_or([1, 1]);
                                let sector = inherited_sector_size.max(1) as f32;
                                let (world_u, world_v) = if slot
                                    == usize::from(psxed_project::CYLINDER_PROP_MATERIAL_SIDE)
                                {
                                    let rx = f32::from(geometry.radius[0]);
                                    let rz = f32::from(geometry.radius[1]);
                                    (
                                        core::f32::consts::TAU * ((rx * rx + rz * rz) * 0.5).sqrt(),
                                        f32::from(geometry.height),
                                    )
                                } else {
                                    (
                                        f32::from(geometry.radius[0]) * 2.0,
                                        f32::from(geometry.radius[1]) * 2.0,
                                    )
                                };
                                uvs[slot].span = [
                                    ((world_u / sector) * f32::from(texture[0]))
                                        .round()
                                        .clamp(1.0, 255.0)
                                        as u16,
                                    ((world_v / sector) * f32::from(texture[1]))
                                        .round()
                                        .clamp(1.0, 255.0)
                                        as u16,
                                ];
                                changed = true;
                            }
                            if ui.small_button("Fit once").clicked() {
                                uvs[slot].span = [0, 0];
                                changed = true;
                            }
                        });
                        changed |= uv_transform_controls(&mut uvs[slot], ui).changed();
                    });
            }
            let surfaces = psxed_project::generate_cylinder_prop_surfaces(*geometry);
            let triangles = surfaces
                .iter()
                .map(|surface| if surface.vertex_count == 4 { 2 } else { 1 })
                .sum::<usize>();
            ui.weak(format!(
                "{} generated surfaces · {} render triangles",
                surfaces.len(),
                triangles
            ));
        }
        NodeKind::ArchProp {
            materials,
            uvs,
            geometry,
            collision_enabled,
        } => {
            ui.weak(
                "Procedural extruded arch. Its footprint uses the project level scale; vertical controls use a 64-unit quantum.",
            );
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Portion").color(STUDIO_TEXT_WEAK));
                for candidate in [
                    psxed_project::ArchPortion::Full,
                    psxed_project::ArchPortion::LeftHalf,
                    psxed_project::ArchPortion::RightHalf,
                ] {
                    changed |= ui
                        .selectable_value(&mut geometry.portion, candidate, candidate.label())
                        .changed();
                }
            });
            ui.horizontal(|ui| {
                ui.label("Span");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut geometry.span_tiles)
                            .range(
                                psxed_project::ARCH_PROP_MIN_TILES
                                    ..=psxed_project::ARCH_PROP_MAX_TILES,
                            )
                            .suffix(" steps"),
                    )
                    .changed();
                ui.label("Depth");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut geometry.depth_tiles)
                            .range(
                                psxed_project::ARCH_PROP_MIN_TILES
                                    ..=psxed_project::ARCH_PROP_MAX_TILES,
                            )
                            .suffix(" steps"),
                    )
                    .changed();
            });
            ui.horizontal(|ui| {
                ui.label("Rise");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut geometry.rise_quanta)
                            .range(1..=psxed_project::ARCH_PROP_MAX_HEIGHT_QUANTA)
                            .suffix(" ×64"),
                    )
                    .changed();
                ui.label("Legs");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut geometry.leg_height_quanta)
                            .range(0..=psxed_project::ARCH_PROP_MAX_HEIGHT_QUANTA)
                            .suffix(" ×64"),
                    )
                    .changed();
            });
            ui.horizontal(|ui| {
                ui.label("Band");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut geometry.band_thickness_quanta)
                            .range(1..=psxed_project::ARCH_PROP_MAX_HEIGHT_QUANTA)
                            .suffix(" ×64"),
                    )
                    .changed();
                ui.label("Detail");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut geometry.segments_per_quadrant).range(
                            psxed_project::ARCH_PROP_MIN_SEGMENTS_PER_QUADRANT
                                ..=psxed_project::ARCH_PROP_MAX_SEGMENTS_PER_QUADRANT,
                        ),
                    )
                    .changed();
            });
            ui.horizontal(|ui| {
                ui.label("Curve");
                ui.label(
                    RichText::new(geometry.curve.label())
                        .color(STUDIO_TEXT_WEAK)
                        .italics(),
                );
                changed |= ui
                    .checkbox(&mut geometry.filled_top, "Filled top")
                    .changed();
                changed |= ui.checkbox(collision_enabled, "Collision").changed();
            });
            if geometry.filled_top {
                ui.weak(
                    "Filled top closes the spandrel up to a flat crown-height surface, ready to meet flat tiles.",
                );
            }
            if *collision_enabled {
                ui.weak(
                    "Collision is approximated by bounded segment boxes; the opening remains passable.",
                );
            }
            ui.weak(format!(
                "{}×{} level steps · {} units total height",
                geometry.span_tiles,
                geometry.depth_tiles,
                u32::from(
                    geometry
                        .rise_quanta
                        .saturating_add(geometry.leg_height_quanta)
                ) * psxed_project::HEIGHT_QUANTUM as u32
            ));
            ui.colored_label(
                Color32::from_rgb(180, 150, 90),
                "Arch props do not carve BSP; place them in an authored open span.",
            );

            ui.separator();
            ui.label(RichText::new("Materials / UV").color(STUDIO_TEXT_WEAK));
            for (slot, name) in psxed_project::ARCH_PROP_MATERIAL_NAMES.iter().enumerate() {
                egui::CollapsingHeader::new(*name)
                    .id_salt(("arch-prop-material", slot))
                    .default_open(slot == 0)
                    .show(ui, |ui| {
                        changed |= material_picker(
                            ui,
                            "Material",
                            &mut materials[slot],
                            material_options,
                            nav_target,
                        );
                        let texture_size = materials[slot].and_then(|material| {
                            material_texture_dimensions
                                .iter()
                                .find_map(|(id, size)| (*id == material).then_some(*size))
                        });
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    texture_size.is_some(),
                                    egui::Button::new("1:1 Texels"),
                                )
                                .clicked()
                            {
                                let texture = texture_size.unwrap_or([1, 1]);
                                let sector = inherited_sector_size.max(1) as f32;
                                let total_height = f32::from(
                                    geometry
                                        .rise_quanta
                                        .saturating_add(geometry.leg_height_quanta),
                                ) * psxed_project::HEIGHT_QUANTUM as f32;
                                let span = f32::from(geometry.span_tiles.max(1)) * sector;
                                let depth = f32::from(geometry.depth_tiles.max(1)) * sector;
                                let band = f32::from(geometry.band_thickness_quanta.max(1))
                                    * psxed_project::HEIGHT_QUANTUM as f32;
                                let (world_u, world_v) = match slot as u8 {
                                    psxed_project::ARCH_PROP_MATERIAL_FASCIA => {
                                        (span, total_height)
                                    }
                                    psxed_project::ARCH_PROP_MATERIAL_SOFFIT
                                    | psxed_project::ARCH_PROP_MATERIAL_EXTRADOS => (span, depth),
                                    _ => (band, depth),
                                };
                                uvs[slot].span = [
                                    ((world_u / sector) * f32::from(texture[0]))
                                        .round()
                                        .clamp(1.0, 255.0)
                                        as u16,
                                    ((world_v / sector) * f32::from(texture[1]))
                                        .round()
                                        .clamp(1.0, 255.0)
                                        as u16,
                                ];
                                changed = true;
                            }
                            if ui.small_button("Fit once").clicked() {
                                uvs[slot].span = [0, 0];
                                changed = true;
                            }
                        });
                        changed |= uv_transform_controls(&mut uvs[slot], ui).changed();
                    });
            }
            let surfaces =
                psxed_project::generate_arch_prop_surfaces(*geometry, inherited_sector_size);
            ui.weak(format!(
                "{} generated quads · {} render triangles",
                surfaces.len(),
                surfaces.len() * 2
            ));
        }
        NodeKind::ModelRenderer {
            model,
            material,
            visual_offset,
            visual_scale_q8,
        } => {
            ui.weak("Component: renders a Model from the parent Entity transform.");
            let bound_model =
                model.and_then(|id| model_options.iter().find(|(rid, _, _)| *rid == id));
            let searchable_model_options = model_options
                .iter()
                .map(|(id, name, _)| (*id, name.clone()))
                .collect::<Vec<_>>();
            ui.horizontal(|ui| {
                ui.label("Model");
                let preview = bound_model
                    .map(|(_, name, _)| name.as_str())
                    .unwrap_or("(none)");
                changed |= searchable_picker(
                    ui,
                    "model-renderer-model-picker",
                    model,
                    preview,
                    &searchable_model_options,
                    SearchablePickerConfig::optional("(none)").with_search_hint("Search models…"),
                );
            });
            if model.is_some() && bound_model.is_none() {
                ui.colored_label(
                    Color32::from_rgb(220, 120, 100),
                    "Model resource is missing.",
                );
            }
            ui.separator();
            // Appearance switch: `None` renders the baked model
            // material unchanged; `Some` applies the selected
            // Material's blend/tint/sidedness and only replaces the
            // atlas when that Material has a texture path.
            let mut custom_material = material.is_some();
            ui.horizontal(|ui| {
                ui.label("Appearance");
                egui::ComboBox::from_id_salt("model-renderer-material-mode")
                    .selected_text(if custom_material {
                        "Material override"
                    } else {
                        "Model material"
                    })
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(!custom_material, "Model material")
                            .clicked()
                        {
                            custom_material = false;
                        }
                        if ui
                            .selectable_label(custom_material, "Material override")
                            .clicked()
                        {
                            custom_material = true;
                        }
                    });
            });
            if custom_material != material.is_some() {
                *material = if custom_material {
                    material_options.first().map(|(id, _)| *id)
                } else {
                    None
                };
                changed = true;
                if custom_material && material.is_none() {
                    ui.colored_label(
                        Color32::from_rgb(220, 160, 80),
                        "No Material resources exist yet.",
                    );
                }
            }
            if material.is_some() {
                changed |= material_picker(ui, "Material", material, material_options, nav_target);
                ui.weak("Applies blend mode, tint, and sidedness. Leave the Material texture empty to retain the model atlas.");
            }
            ui.separator();
            ui.weak("Visual calibration only. Collision, camera, and movement still use Entity and Character Controller data.");
            ui.label("Visual Offset");
            for (axis, label) in [(0usize, "X"), (1, "Y"), (2, "Z")] {
                ui.horizontal(|ui| {
                    ui.label(label);
                    let mut v = visual_offset[axis] as i32;
                    if ui
                        .add(egui::DragValue::new(&mut v).range(-8192..=8192).speed(8.0))
                        .changed()
                    {
                        visual_offset[axis] = v.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                        changed = true;
                    }
                });
            }
            ui.horizontal(|ui| {
                ui.label("Visual Scale");
                let mut q8 = (*visual_scale_q8).max(1) as i32;
                if ui
                    .add(egui::DragValue::new(&mut q8).range(1..=4096).speed(16.0))
                    .changed()
                {
                    *visual_scale_q8 = q8.clamp(1, u16::MAX as i32) as u16;
                    changed = true;
                }
                ui.label(
                    RichText::new(format!(
                        "{:.3}x",
                        *visual_scale_q8 as f32 / MODEL_SCALE_ONE_Q8 as f32
                    ))
                    .color(STUDIO_TEXT_WEAK)
                    .monospace(),
                );
                if ui.button("Reset").clicked() {
                    *visual_offset = [0; 3];
                    *visual_scale_q8 = MODEL_SCALE_ONE_Q8;
                    changed = true;
                }
            });
        }
        NodeKind::Animator {
            clip,
            action_clips,
            autoplay,
            pose_frame,
        } => {
            ui.weak("Component: maps gameplay actions to model animation clips.");
            let autoplay_response = ui.checkbox(autoplay, icons::label(icons::PLAY, "Autoplay"));
            if autoplay_response.changed() {
                changed = true;
            }
            autoplay_response.on_hover_text(
                "Advance the editor preview clip in the 3D viewport. Off freezes the model on the Pose Frame below.",
            );
            if !*autoplay {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Pose Frame").color(STUDIO_TEXT_WEAK));
                    let mut frame = i32::from(*pose_frame);
                    if ui
                        .add(egui::DragValue::new(&mut frame).speed(1.0).range(0..=4095))
                        .changed()
                    {
                        *pose_frame = frame.clamp(0, u16::MAX as i32 - 1) as u16;
                        changed = true;
                    }
                })
                .response
                .on_hover_text(
                    "Frame of the selected clip to hold while autoplay is off. Place a model frozen on a chosen pose, e.g. a corpse on its death frame.",
                );
            }
            if let Some(context) = animator_clip_context {
                ui.label(
                    RichText::new(format!("Model: {}", context.model_name))
                        .color(STUDIO_TEXT_WEAK)
                        .small(),
                );
                changed |= clip_role_picker(
                    ui,
                    "Editor Clip",
                    "animator-preview-clip",
                    clip,
                    &context.clips,
                );
                egui::CollapsingHeader::new(icons::label(icons::PLAY, "Gameplay Actions"))
                    .default_open(true)
                    .show(ui, |ui| {
                        if let Some(profile_name) = &context.profile_name {
                            ui.label(
                                RichText::new(format!(
                                    "Empty slots inherit from profile: {profile_name}"
                                ))
                                .color(STUDIO_TEXT_WEAK)
                                .small(),
                            );
                        }
                        changed |= draw_animator_action_clip_table(ui, action_clips, context);
                    });
            } else {
                ui.colored_label(
                    STUDIO_TEXT_WEAK,
                    "Add a Model Renderer sibling to select animation clips.",
                );
                let mut current = clip.map(|i| i as i32).unwrap_or(-1);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Editor Clip").color(STUDIO_TEXT_WEAK));
                    let response = ui.add(
                        egui::DragValue::new(&mut current)
                            .speed(0.1)
                            .range(-1..=255)
                            .custom_formatter(|n, _| {
                                if n < 0.0 {
                                    "inherit".to_string()
                                } else {
                                    format!("{}", n as i32)
                                }
                            }),
                    );
                    if response.changed() {
                        *clip = if current < 0 {
                            None
                        } else {
                            Some(current as u16)
                        };
                        changed = true;
                    }
                });
            }
        }
        NodeKind::Collider { shape, solid } => {
            ui.weak("Component: collision authored on an Entity. Runtime entity collision is not cooked yet.");
            changed |= ui.checkbox(solid, "Solid").changed();
            changed |= collider_shape_editor(ui, shape);
        }
        NodeKind::CharacterController {
            loadout,
            character,
            settings,
            player,
        } => {
            ui.weak("Component: owns this character's per-instance movement and gameplay behavior. Model Renderer owns visuals; Animator owns action clips.");
            // No override means this placement follows its Character. Show the
            // type's values, and only write an override once something here
            // actually changes.
            let inherited = character
                .and_then(|id| {
                    character_defaults
                        .iter()
                        .find(|(candidate, _)| *candidate == id)
                        .map(|(_, settings)| *settings)
                })
                .unwrap_or_default();
            if settings.is_none() {
                ui.weak("Inherited from the Character. Editing anything here overrides it for this placement only.");
            }
            let mut working = settings.unwrap_or(inherited);
            let edited = draw_character_controller_editor(
                ui,
                character,
                &mut working,
                player,
                character_options,
                nav_target,
                character_preview_action,
            );
            if edited {
                *settings = Some(working);
            }
            changed |= edited;
            changed |= draw_character_loadout_picker(ui, *character, loadout, character_loadouts);
        }
        NodeKind::Camera { settings } => {
            ui.weak("Component: third-person gameplay camera for the player Entity. The Entity transform supplies the start position and yaw.");
            changed |= draw_gameplay_camera_settings(ui, settings);
            egui::CollapsingHeader::new(icons::label(icons::EYE, "Camera Preview"))
                .default_open(true)
                .show(ui, |ui| {
                    draw_gameplay_camera_render_preview(ui, camera_preview);
                });
            egui::CollapsingHeader::new(icons::label(icons::EYE, "Starting Position Preview"))
                .default_open(true)
                .show(ui, |ui| {
                    draw_gameplay_camera_start_preview(ui, settings.normalized());
                });
        }
        NodeKind::Equipment {
            weapon,
            character_socket,
            weapon_grip,
        } => {
            ui.weak("Component: attaches a Weapon resource to a named model socket.");
            changed |= draw_weapon_selector(ui, weapon_options, weapon);
            ui.horizontal(|ui| {
                ui.label("Character Socket");
                changed |= ui.text_edit_singleline(character_socket).changed();
            });
            ui.horizontal(|ui| {
                ui.label("Weapon Grip");
                changed |= ui.text_edit_singleline(weapon_grip).changed();
            });
        }
        NodeKind::PhysicsBody { settings } => {
            ui.weak("Component: per-entity physics tuning. Weight is a Q8 gravity multiplier.");
            ui.horizontal(|ui| {
                ui.label("Weight");
                let mut q8 = settings.weight_q8 as i32;
                if ui
                    .add(
                        egui::DragValue::new(&mut q8)
                            .speed(16.0)
                            .range(MIN_PHYSICS_WEIGHT_Q8 as i32..=MAX_PHYSICS_WEIGHT_Q8 as i32),
                    )
                    .on_hover_text("Q8 multiplier applied to world gravity: 256 = 1.0x.")
                    .changed()
                {
                    settings.weight_q8 =
                        q8.clamp(MIN_PHYSICS_WEIGHT_Q8 as i32, MAX_PHYSICS_WEIGHT_Q8 as i32) as u16;
                    *settings = settings.normalized();
                    changed = true;
                }
                ui.label(
                    RichText::new(format!("{:.3}x", settings.weight_q8 as f32 / 256.0))
                        .color(STUDIO_TEXT_WEAK)
                        .monospace(),
                );
                if ui.button("Reset").clicked() {
                    settings.weight_q8 = PHYSICS_WEIGHT_ONE_Q8;
                    changed = true;
                }
            });
        }
        NodeKind::Interactable {
            kind: interactable_kind,
            prompt,
            radius,
            enabled,
        } => {
            ui.weak("Component: lets the player press CROSS near this Entity to read a message or synchronize a checkpoint.");
            changed |= ui.checkbox(enabled, "Enabled").changed();
            ui.horizontal(|ui| {
                ui.label(RichText::new("Kind").color(STUDIO_TEXT_WEAK));
                let current = match interactable_kind {
                    InteractableKind::Message { .. } => "Message",
                    InteractableKind::Checkpoint { .. } => "Checkpoint",
                };
                egui::ComboBox::from_id_salt("interactable-kind")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        let is_message =
                            matches!(interactable_kind, InteractableKind::Message { .. });
                        if ui.selectable_label(is_message, "Message").clicked() && !is_message {
                            *interactable_kind = InteractableKind::Message {
                                title: "ECHO REMNANT".to_string(),
                                body: String::new(),
                            };
                            if prompt.trim().is_empty() || prompt == "SYNCHRONIZE" {
                                *prompt = "READ ECHO".to_string();
                            }
                            changed = true;
                        }
                        let is_checkpoint =
                            matches!(interactable_kind, InteractableKind::Checkpoint { .. });
                        if ui.selectable_label(is_checkpoint, "Checkpoint").clicked()
                            && !is_checkpoint
                        {
                            *interactable_kind = InteractableKind::Checkpoint {
                                checkpoint_id: String::new(),
                                title: "SYNC RELAY".to_string(),
                                body: "Relay synchronized.".to_string(),
                            };
                            if prompt.trim().is_empty() || prompt == "READ ECHO" {
                                *prompt = "SYNCHRONIZE".to_string();
                            }
                            changed = true;
                        }
                    });
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Prompt").color(STUDIO_TEXT_WEAK));
                changed |= ui.text_edit_singleline(prompt).changed();
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Radius").color(STUDIO_TEXT_WEAK));
                let mut r = i32::from(*radius);
                if ui
                    .add(egui::DragValue::new(&mut r).speed(4.0).range(1..=4096))
                    .changed()
                {
                    *radius = r.clamp(1, u16::MAX as i32) as u16;
                    changed = true;
                }
            });
            ui.separator();
            match interactable_kind {
                InteractableKind::Message { title, body } => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Title").color(STUDIO_TEXT_WEAK));
                        changed |= ui.text_edit_singleline(title).changed();
                    });
                    ui.label(RichText::new("Body").color(STUDIO_TEXT_WEAK));
                    changed |= ui
                        .add(
                            egui::TextEdit::multiline(body)
                                .desired_rows(4)
                                .desired_width(f32::INFINITY),
                        )
                        .changed();
                }
                InteractableKind::Checkpoint {
                    checkpoint_id,
                    title,
                    body,
                } => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Checkpoint ID").color(STUDIO_TEXT_WEAK));
                        changed |= ui.text_edit_singleline(checkpoint_id).changed();
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Title").color(STUDIO_TEXT_WEAK));
                        changed |= ui.text_edit_singleline(title).changed();
                    });
                    ui.label(RichText::new("Body").color(STUDIO_TEXT_WEAK));
                    changed |= ui
                        .add(
                            egui::TextEdit::multiline(body)
                                .desired_rows(3)
                                .desired_width(f32::INFINITY),
                        )
                        .changed();
                }
            }
        }
        NodeKind::PointOfInterest {
            pages,
            prompt,
            radius,
            marker_height,
            repeatable,
            persistence_id,
            reward,
            enabled,
        } => {
            ui.weak("Component: procedural readable beacon. The parent Entity supplies its world position.");
            changed |= ui.checkbox(enabled, "Enabled").changed();
            changed |= ui.checkbox(repeatable, "Repeatable message").changed();
            ui.horizontal(|ui| {
                ui.label(RichText::new("Action verb").color(STUDIO_TEXT_WEAK));
                changed |= ui.text_edit_singleline(prompt).changed();
            });
            ui.label(
                RichText::new("The runtime adds the CROSS control prefix (for example: X - READ).")
                    .small()
                    .color(STUDIO_TEXT_WEAK),
            );
            ui.horizontal(|ui| {
                ui.label(RichText::new("Persistence ID").color(STUDIO_TEXT_WEAK));
                changed |= ui.text_edit_singleline(persistence_id).changed();
            });
            ui.label(
                RichText::new("Leave empty to derive a stable id from the authored node.")
                    .small()
                    .color(STUDIO_TEXT_WEAK),
            );
            ui.horizontal(|ui| {
                ui.label(RichText::new("Radius").color(STUDIO_TEXT_WEAK));
                let mut value = i32::from(*radius);
                if ui
                    .add(egui::DragValue::new(&mut value).speed(8.0).range(1..=8192))
                    .changed()
                {
                    *radius = value.clamp(1, u16::MAX as i32) as u16;
                    changed = true;
                }
                ui.label(RichText::new("units").small().color(STUDIO_TEXT_WEAK));
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Beacon Scale").color(STUDIO_TEXT_WEAK));
                let mut value = i32::from(*marker_height);
                if ui
                    .add(egui::DragValue::new(&mut value).speed(4.0).range(1..=4096))
                    .changed()
                {
                    *marker_height = value.clamp(1, u16::MAX as i32) as u16;
                    changed = true;
                }
                ui.label(RichText::new("units").small().color(STUDIO_TEXT_WEAK));
            });
            ui.separator();
            ui.label(RichText::new("Message").color(STUDIO_TEXT_WEAK));
            changed |= draw_message_pages_editor(ui, "point-of-interest", pages, 2);
            ui.separator();
            let mut has_reward = reward.is_some();
            if ui.checkbox(&mut has_reward, "Grant item").changed() {
                *reward = has_reward.then(psxed_project::PointOfInterestReward::default);
                changed = true;
            }
            if let Some(reward) = reward {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Item name").color(STUDIO_TEXT_WEAK));
                    changed |= ui.text_edit_singleline(&mut reward.item_name).changed();
                });
                ui.label(RichText::new("Description").color(STUDIO_TEXT_WEAK));
                changed |= ui
                    .add(
                        egui::TextEdit::multiline(&mut reward.description)
                            .desired_rows(2)
                            .desired_width(f32::INFINITY),
                    )
                    .changed();
                ui.label(RichText::new("Percentage effects").color(STUDIO_TEXT_WEAK));
                let mut remove_effect = None;
                for (index, modifier) in reward.modifiers.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt(("poi-reward-stat", index))
                            .selected_text(modifier.stat.label())
                            .show_ui(ui, |ui| {
                                for stat in psxed_project::BoostStatKind::ALL {
                                    changed |= ui
                                        .selectable_value(&mut modifier.stat, stat, stat.label())
                                        .changed();
                                }
                            });
                        changed |= ui
                            .add(
                                egui::DragValue::new(&mut modifier.percent)
                                    .suffix("%")
                                    .range(-100..=500),
                            )
                            .changed();
                        if ui
                            .small_button(icons::text(icons::TRASH, 13.0))
                            .on_hover_text("Remove effect")
                            .clicked()
                        {
                            remove_effect = Some(index);
                        }
                    });
                }
                if let Some(index) = remove_effect {
                    reward.modifiers.remove(index);
                    changed = true;
                }
                if reward.modifiers.len() < psx_level::boost_stat::COUNT
                    && ui.button(icons::label(icons::PLUS, "Add effect")).clicked()
                {
                    reward
                        .modifiers
                        .push(psxed_project::BoostStatModifier::default());
                    changed = true;
                }
                if reward.item_name.trim().is_empty() {
                    if let Some(module_id) = reward.module {
                        let legacy_name = boost_module_options
                            .iter()
                            .find(|(candidate, _)| *candidate == module_id)
                            .map(|(_, name)| name.as_str())
                            .unwrap_or("missing resource");
                        ui.label(
                            RichText::new(format!("Legacy module: {legacy_name}"))
                                .small()
                                .color(STUDIO_TEXT_WEAK),
                        );
                    }
                }
                ui.label(
                    RichText::new("Unique item: granted once even when the message is repeatable.")
                        .small()
                        .color(STUDIO_TEXT_WEAK),
                );
            }
        }
        NodeKind::Destructible {
            max_health,
            damage_affinity,
            enabled,
        } => {
            ui.weak(
                "Brush module. Assign one or more solid BSP brushes through their Model owner; all assigned brushes break as one object.",
            );
            changed |= ui.checkbox(enabled, "Enabled").changed();
            ui.horizontal(|ui| {
                ui.label(RichText::new("Health").color(STUDIO_TEXT_WEAK));
                let mut value = i32::from(*max_health);
                if ui
                    .add(
                        egui::DragValue::new(&mut value)
                            .speed(1.0)
                            .range(1..=u16::MAX as i32),
                    )
                    .changed()
                {
                    *max_health = value.clamp(1, u16::MAX as i32) as u16;
                    changed = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Breakable by").color(STUDIO_TEXT_WEAK));
                egui::ComboBox::from_id_salt("destructible-damage-affinity")
                    .selected_text(damage_affinity.label())
                    .show_ui(ui, |ui| {
                        for option in psxed_project::DestructibleDamageAffinity::ALL {
                            if ui
                                .selectable_value(damage_affinity, option, option.label())
                                .changed()
                            {
                                changed = true;
                            }
                        }
                    });
            });
            ui.label(
                RichText::new(match damage_affinity {
                    psxed_project::DestructibleDamageAffinity::Horizon => {
                        "Only R1/R2 Horizon attacks can damage this object."
                    }
                    psxed_project::DestructibleDamageAffinity::Zenith => {
                        "Only L1/L2 Zenith attacks can damage this object."
                    }
                    psxed_project::DestructibleDamageAffinity::Both => {
                        "Horizon and Zenith attacks can both damage this object."
                    }
                })
                .small()
                .color(STUDIO_TEXT_WEAK),
            );
        }
        NodeKind::Logic {
            kind: logic_kind,
            target,
            killtarget,
            master,
            delay_ticks,
            wait_ticks,
            enabled,
        } => {
            ui.weak(
                "Event-graph node. The NODE NAME is its targetname; \
                 target/killtarget/master name other nodes (Logic, \
                 Interactable, or enemy entities).",
            );
            changed |= ui.checkbox(enabled, "Enabled").changed();
            ui.horizontal(|ui| {
                ui.label(RichText::new("Kind").color(STUDIO_TEXT_WEAK));
                let current = match logic_kind {
                    psxed_project::LogicNodeKind::TriggerVolume { .. } => "Trigger Volume",
                    psxed_project::LogicNodeKind::Relay => "Relay",
                    psxed_project::LogicNodeKind::Multisource { .. } => "Multisource",
                    psxed_project::LogicNodeKind::Door { .. } => "Door",
                };
                egui::ComboBox::from_id_salt("logic-node-kind")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        let is_trigger = matches!(
                            logic_kind,
                            psxed_project::LogicNodeKind::TriggerVolume { .. }
                        );
                        if ui.selectable_label(is_trigger, "Trigger Volume").clicked()
                            && !is_trigger
                        {
                            *logic_kind = psxed_project::LogicNodeKind::default();
                            changed = true;
                        }
                        let is_relay = matches!(logic_kind, psxed_project::LogicNodeKind::Relay);
                        if ui.selectable_label(is_relay, "Relay").clicked() && !is_relay {
                            *logic_kind = psxed_project::LogicNodeKind::Relay;
                            changed = true;
                        }
                        let is_multisource =
                            matches!(logic_kind, psxed_project::LogicNodeKind::Multisource { .. });
                        if ui.selectable_label(is_multisource, "Multisource").clicked()
                            && !is_multisource
                        {
                            *logic_kind = psxed_project::LogicNodeKind::Multisource { required: 1 };
                            changed = true;
                        }
                        let is_door =
                            matches!(logic_kind, psxed_project::LogicNodeKind::Door { .. });
                        if ui.selectable_label(is_door, "Door").clicked() && !is_door {
                            *logic_kind = psxed_project::LogicNodeKind::Door {
                                box_prop: String::new(),
                                start_open: false,
                                open_offset: psxed_project::default_brush_door_open_offset(),
                                travel_ticks: psxed_project::default_brush_door_travel_ticks(),
                            };
                            changed = true;
                        }
                    });
            });
            match logic_kind {
                psxed_project::LogicNodeKind::TriggerVolume { size } => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Extent").color(STUDIO_TEXT_WEAK));
                        for (axis, label) in ["X", "Y", "Z"].iter().enumerate() {
                            let mut value = i32::from(size[axis]);
                            if ui
                                .add(
                                    egui::DragValue::new(&mut value)
                                        .speed(16.0)
                                        .range(1..=16384)
                                        .prefix(format!("{label} ")),
                                )
                                .changed()
                            {
                                size[axis] = value.clamp(1, u16::MAX as i32) as u16;
                                changed = true;
                            }
                        }
                    });
                    if size.contains(&0) {
                        ui.colored_label(
                            Color32::from_rgb(220, 120, 100),
                            "Extent must be > 0 on every axis (cook will fail)",
                        );
                    }
                }
                psxed_project::LogicNodeKind::Relay => {}
                psxed_project::LogicNodeKind::Multisource { required } => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Required inputs").color(STUDIO_TEXT_WEAK));
                        let mut value = i32::from(*required);
                        if ui
                            .add(egui::DragValue::new(&mut value).speed(0.1).range(1..=32))
                            .changed()
                        {
                            *required = value.clamp(1, u16::MAX as i32) as u16;
                            changed = true;
                        }
                    });
                }
                psxed_project::LogicNodeKind::Door {
                    box_prop: _,
                    start_open,
                    open_offset,
                    travel_ticks,
                } => {
                    ui.weak("Bind this door from the BSP brush inspector.");
                    changed |= ui.checkbox(start_open, "Start open").changed();
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Open offset").color(STUDIO_TEXT_WEAK));
                        for (axis, label) in ["X", "Y", "Z"].iter().enumerate() {
                            let mut value = i32::from(open_offset[axis]);
                            if ui
                                .add(
                                    egui::DragValue::new(&mut value)
                                        .speed(8.0)
                                        .range(i16::MIN as i32..=i16::MAX as i32)
                                        .prefix(format!("{label} ")),
                                )
                                .changed()
                            {
                                open_offset[axis] =
                                    value.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                                changed = true;
                            }
                        }
                    });
                    if *open_offset == [0; 3] {
                        ui.colored_label(
                            Color32::from_rgb(220, 120, 100),
                            "Open offset must move on at least one axis",
                        );
                    }
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Travel (ticks)").color(STUDIO_TEXT_WEAK));
                        let mut value = i32::from(*travel_ticks);
                        if ui
                            .add(egui::DragValue::new(&mut value).speed(0.2).range(1..=3600))
                            .changed()
                        {
                            *travel_ticks = value.clamp(1, u16::MAX as i32) as u16;
                            changed = true;
                        }
                    });
                }
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(RichText::new("Target").color(STUDIO_TEXT_WEAK));
                changed |= ui.text_edit_singleline(target).changed();
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Killtarget").color(STUDIO_TEXT_WEAK));
                changed |= ui.text_edit_singleline(killtarget).changed();
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Master").color(STUDIO_TEXT_WEAK));
                changed |= ui.text_edit_singleline(master).changed();
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Delay (ticks)").color(STUDIO_TEXT_WEAK));
                let mut value = i32::from(*delay_ticks);
                if ui
                    .add(egui::DragValue::new(&mut value).speed(1.0).range(0..=3600))
                    .changed()
                {
                    *delay_ticks = value.clamp(0, u16::MAX as i32) as u16;
                    changed = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Wait (ticks)").color(STUDIO_TEXT_WEAK));
                let mut value = i32::from(*wait_ticks);
                if ui
                    .add(egui::DragValue::new(&mut value).speed(1.0).range(-1..=3600))
                    .changed()
                {
                    *wait_ticks = value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
                    changed = true;
                }
            });
            ui.weak("Wait -1 fires once, then retires.");
        }
        NodeKind::PointLight {
            color,
            intensity,
            radius,
        } => {
            ui.weak("Static point light emitted from this node transform.");
            changed |= color_editor(ui, "Color", color);
            changed |= ui
                .add(
                    egui::Slider::new(intensity, 0.0..=4.0)
                        .text(icons::label(icons::SUN, "Intensity (× 1.0)")),
                )
                .changed();
            let radius_scale = inherited_sector_size.max(1) as f32;
            let raw_radius_units = *radius * radius_scale;
            let mut radius_units = if raw_radius_units.is_finite() {
                raw_radius_units.clamp(1.0, psxed_project::POINT_LIGHT_RADIUS_MAX_WORLD_UNITS)
            } else {
                1.0
            };
            if raw_radius_units != radius_units {
                *radius = radius_units / radius_scale;
                changed = true;
            }
            if ui
                .add(
                    egui::DragValue::new(&mut radius_units)
                        .speed(1.0)
                        .range(1.0..=psxed_project::POINT_LIGHT_RADIUS_MAX_WORLD_UNITS)
                        .prefix("Radius ")
                        .suffix(" units"),
                )
                .changed()
            {
                *radius = radius_units / radius_scale;
                changed = true;
            }
            // Validation warnings -- match what the playtest cooker
            // refuses, so authors see the issue before they cook.
            if *radius <= 0.0 {
                ui.colored_label(
                    Color32::from_rgb(220, 120, 100),
                    "Radius must be > 0 (cook will fail)",
                );
            }
            if !intensity.is_finite() || *intensity < 0.0 {
                ui.colored_label(
                    Color32::from_rgb(220, 120, 100),
                    "Intensity must be finite and ≥ 0 (cook will fail)",
                );
            }
            if *intensity > 4.0 {
                ui.colored_label(
                    Color32::from_rgb(220, 160, 80),
                    "Intensity above 4.0 saturates almost every surface",
                );
            }
        }
        NodeKind::ParticleEmitter { settings } => {
            ui.weak(
                "Fixed-budget world particle emitter. Runtime projects each particle center and draws one tinted sprite.",
            );
            changed |= draw_particle_emitter_settings(ui, settings, texture_options, nav_target);
        }
        NodeKind::SpawnPoint { player, character } => {
            changed |= ui
                .checkbox(player, icons::label(icons::MAP_PIN, "Player spawn"))
                .changed();
            if *player {
                changed |= draw_character_selector(ui, character_options, character, nav_target);
            }
        }
        NodeKind::Portal {
            target_room,
            target_entry,
            entry_name,
            geometry,
        } => {
            ui.horizontal(|ui| {
                ui.label(icons::text(icons::WAYPOINT, 12.0).color(STUDIO_TEXT_WEAK));
                ui.label("Entry name");
                changed |= ui.text_edit_singleline(entry_name).changed();
            });
            ui.horizontal(|ui| {
                ui.label(icons::text(icons::HOUSE, 12.0).color(STUDIO_TEXT_WEAK));
                ui.label("Target room");
                let preview = target_room
                    .and_then(|id| {
                        room_options
                            .iter()
                            .find(|(rid, _)| *rid == id)
                            .map(|(_, name)| name.as_str())
                    })
                    .unwrap_or("(none)");
                changed |= searchable_picker(
                    ui,
                    "portal_target_room",
                    target_room,
                    preview,
                    room_options,
                    SearchablePickerConfig::optional("(none)").with_search_hint("Search rooms…"),
                );
            });
            ui.horizontal(|ui| {
                ui.label(icons::text(icons::MAP_PIN, 12.0).color(STUDIO_TEXT_WEAK));
                ui.label("Target entry");
                changed |= ui.text_edit_singleline(target_entry).changed();
            });
            if let Some(geometry) = geometry {
                ui.horizontal(|ui| {
                    ui.label(icons::text(icons::BOX, 12.0).color(STUDIO_TEXT_WEAK));
                    ui.label(format!(
                        "Imported plane n=({}, {}, {})",
                        geometry.normal[0], geometry.normal[1], geometry.normal[2]
                    ));
                });
            }
        }
    }
    changed
}

pub(crate) fn blend_mode_editor(ui: &mut egui::Ui, mode: &mut PsxBlendMode) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(icons::text(icons::BLEND, 12.0).color(STUDIO_TEXT_WEAK));
        ui.label("Blend mode");
    });
    for candidate in [
        PsxBlendMode::Opaque,
        PsxBlendMode::Average,
        PsxBlendMode::Add,
        PsxBlendMode::Subtract,
        PsxBlendMode::AddQuarter,
    ] {
        if ui
            .selectable_label(*mode == candidate, candidate.label())
            .clicked()
            && *mode != candidate
        {
            *mode = candidate;
            changed = true;
        }
    }
    changed
}

/// Compact Material editor embedded below a ModelRenderer. The
/// Material resource remains canonical; this is an inspector view
/// onto the same data, not a second per-node copy.
pub(crate) fn draw_model_material_override_editor(
    ui: &mut egui::Ui,
    material: &mut MaterialResource,
    material_options: &[(ResourceId, String)],
    owner: ResourceId,
) -> bool {
    crate::material_lab::draw_material_settings(
        ui,
        "model_renderer_material",
        material,
        material_options,
        Some(owner),
    )
}

pub(crate) fn draw_particle_emitter_settings(
    ui: &mut egui::Ui,
    settings: &mut ParticleEmitterSettings,
    texture_options: &[(ResourceId, String)],
    nav_target: &mut Option<ResourceId>,
) -> bool {
    let mut changed = false;
    changed |= ui.checkbox(&mut settings.enabled, "Enabled").changed();
    changed |= texture_resource_picker(
        ui,
        "Mask Texture",
        &mut settings.texture,
        texture_options,
        nav_target,
    );
    ui.label(
        RichText::new("Use 16x16 greyscale/white masks; particle tint comes from the curve below.")
            .color(STUDIO_TEXT_WEAK)
            .small(),
    );
    changed |= blend_mode_editor(ui, &mut settings.blend_mode);
    ui.separator();
    ui.label(RichText::new("Budget").color(STUDIO_TEXT_WEAK));
    changed |= drag_u16(ui, "Max Particles", &mut settings.max_particles, 1, 256);
    ui.horizontal(|ui| {
        ui.label("Spawn Rate");
        let mut per_second = settings.spawn_rate_q8 as f32 / 256.0;
        if ui
            .add(
                egui::DragValue::new(&mut per_second)
                    .speed(0.25)
                    .range(0.0..=120.0)
                    .suffix(" /s"),
            )
            .changed()
        {
            settings.spawn_rate_q8 = (per_second.clamp(0.0, 120.0) * 256.0).round() as u16;
            changed = true;
        }
    });
    ui.horizontal(|ui| {
        ui.label("Lifetime");
        let mut lifetime = i32::from(settings.lifetime_frames);
        if ui
            .add(
                egui::DragValue::new(&mut lifetime)
                    .speed(1.0)
                    .range(1..=255)
                    .suffix(" frames"),
            )
            .changed()
        {
            settings.lifetime_frames = lifetime.clamp(1, 255) as u8;
            changed = true;
        }
    });
    changed |= drag_u16(ui, "Spawn Radius", &mut settings.spawn_radius, 0, 8192);
    ui.separator();
    ui.label(RichText::new("Size Curve").color(STUDIO_TEXT_WEAK));
    changed |= drag_u16(ui, "Start Size", &mut settings.start_size, 1, 8192);
    changed |= drag_u16(ui, "End Size", &mut settings.end_size, 1, 8192);
    ui.separator();
    ui.label(RichText::new("Tint Curve").color(STUDIO_TEXT_WEAK));
    changed |= color_editor(ui, "Start", &mut settings.start_color);
    changed |= color_editor(ui, "End", &mut settings.end_color);
    ui.separator();
    ui.label(RichText::new("Velocity Q4.4").color(STUDIO_TEXT_WEAK));
    changed |= draw_particle_i16_vec3(ui, "Base", &mut settings.base_velocity_q4, -4096, 4096);
    changed |= draw_particle_u16_vec3(ui, "Random", &mut settings.random_velocity_q4, 0, 4096);
    changed |= draw_particle_i16_vec3(
        ui,
        "Acceleration",
        &mut settings.acceleration_q4,
        -4096,
        4096,
    );
    changed
}

pub(crate) fn draw_particle_i16_vec3(
    ui: &mut egui::Ui,
    label: &str,
    values: &mut [i16; 3],
    min: i16,
    max: i16,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        for (axis, value) in ["X", "Y", "Z"].into_iter().zip(values.iter_mut()) {
            let mut next = i32::from(*value);
            if ui
                .add(
                    egui::DragValue::new(&mut next)
                        .speed(1.0)
                        .range(i32::from(min)..=i32::from(max))
                        .prefix(format!("{axis} ")),
                )
                .changed()
            {
                *value = next.clamp(i32::from(min), i32::from(max)) as i16;
                changed = true;
            }
        }
    });
    changed
}

pub(crate) fn draw_particle_u16_vec3(
    ui: &mut egui::Ui,
    label: &str,
    values: &mut [u16; 3],
    min: u16,
    max: u16,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        for (axis, value) in ["X", "Y", "Z"].into_iter().zip(values.iter_mut()) {
            let mut next = i32::from(*value);
            if ui
                .add(
                    egui::DragValue::new(&mut next)
                        .speed(1.0)
                        .range(i32::from(min)..=i32::from(max))
                        .prefix(format!("{axis} ")),
                )
                .changed()
            {
                *value = next.clamp(i32::from(min), i32::from(max)) as u16;
                changed = true;
            }
        }
    });
    changed
}

pub(crate) fn color_editor(ui: &mut egui::Ui, label: &str, color: &mut [u8; 3]) -> bool {
    ui.horizontal(|ui| {
        let mut changed = false;
        ui.label(icons::text(icons::PALETTE, 12.0).color(STUDIO_TEXT_WEAK));
        ui.label(label);
        changed |= ui.color_edit_button_srgb(color).changed();
        changed |= ui
            .add(
                egui::DragValue::new(&mut color[0])
                    .prefix("R ")
                    .range(0..=255),
            )
            .changed();
        changed |= ui
            .add(
                egui::DragValue::new(&mut color[1])
                    .prefix("G ")
                    .range(0..=255),
            )
            .changed();
        changed |= ui
            .add(
                egui::DragValue::new(&mut color[2])
                    .prefix("B ")
                    .range(0..=255),
            )
            .changed();
        changed
    })
    .inner
}

pub(crate) fn draw_ui_gradient_editor(
    ui: &mut egui::Ui,
    label: &str,
    from: &[u8; 3],
    gradient: &mut Option<UiGradient>,
) -> bool {
    ui.push_id(("ui-gradient", label), |ui| {
        let mut changed = false;
        let mut enabled = gradient.is_some();
        ui.horizontal(|ui| {
            changed |= ui.checkbox(&mut enabled, label).changed();
            if enabled {
                ui.label(
                    RichText::new("start = color")
                        .color(STUDIO_TEXT_WEAK)
                        .small(),
                );
            }
        });

        if enabled {
            if gradient.is_none() {
                *gradient = Some(UiGradient::new(
                    default_ui_gradient_end(*from),
                    UiGradientDirection::Vertical,
                ));
                changed = true;
            }
            if let Some(gradient) = gradient {
                changed |= color_editor(ui, "To", &mut gradient.to);
                egui::ComboBox::from_label("Direction")
                    .selected_text(gradient.direction.label())
                    .show_ui(ui, |ui| {
                        for candidate in UiGradientDirection::ALL {
                            changed |= ui
                                .selectable_value(
                                    &mut gradient.direction,
                                    candidate,
                                    candidate.label(),
                                )
                                .changed();
                        }
                    });
            }
        } else if gradient.take().is_some() {
            changed = true;
        }
        changed
    })
    .inner
}

pub(crate) fn default_ui_gradient_end(from: [u8; 3]) -> [u8; 3] {
    [
        from[0].saturating_add(48),
        from[1].saturating_add(48),
        from[2].saturating_add(48),
    ]
}

pub(crate) fn short_path(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| path.display().to_string())
}

pub(crate) fn project_menu_label(path: &Path) -> String {
    let folder = short_path(path);
    let project_file = path.join("project.ron");
    let Ok(project) = ProjectDocument::load_from_path(&project_file) else {
        return folder;
    };
    let name = project.name.trim();
    if name.is_empty() {
        folder
    } else if psxed_project::project_file_stem(name) == folder {
        name.to_string()
    } else {
        format!("{name} ({folder})")
    }
}

pub(crate) fn paths_equivalent(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

pub(crate) fn bundled_project_is_protected(path: &Path) -> bool {
    paths_equivalent(path, &psxed_project::default_project_dir())
        || paths_equivalent(path, &psxed_project::new_project_template_dir())
}

pub(crate) fn node_kind_is_player_source(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::SpawnPoint { player: true, .. }
            | NodeKind::CharacterController { player: true, .. }
    )
}

pub(crate) fn translations_match(a: [f32; 3], b: [f32; 3]) -> bool {
    a.iter()
        .zip(b)
        .all(|(lhs, rhs)| (*lhs - rhs).abs() <= PLACEMENT_DUPLICATE_EPSILON)
}

pub(crate) fn entity_model_resource_id(scene: &Scene, entity: &SceneNode) -> Option<ResourceId> {
    entity.children.iter().find_map(|id| {
        scene.node(*id).and_then(|child| match child.kind {
            NodeKind::ModelRenderer {
                model: Some(model), ..
            } => Some(model),
            _ => None,
        })
    })
}

pub(crate) fn entity_character_component_resource_id(
    scene: &Scene,
    entity: &SceneNode,
) -> Option<ResourceId> {
    entity.children.iter().find_map(|id| {
        scene.node(*id).and_then(|child| match child.kind {
            NodeKind::CharacterController {
                character: Some(character),
                ..
            } => Some(character),
            _ => None,
        })
    })
}

pub(crate) fn entity_weapon_resource_id(scene: &Scene, entity: &SceneNode) -> Option<ResourceId> {
    entity.children.iter().find_map(|id| {
        scene.node(*id).and_then(|child| match child.kind {
            NodeKind::Equipment {
                weapon: Some(weapon),
                ..
            } => Some(weapon),
            _ => None,
        })
    })
}

pub(crate) fn node_lucide_icon(kind: &str, root: bool) -> char {
    if root {
        return icons::HOUSE;
    }

    match kind {
        "Node3D" => icons::CIRCLE_DOT,
        "Entity" => icons::BOX,
        "World" => icons::HOUSE,
        "Section" | "Room" | "Map" => icons::GRID,
        "Mesh Instance" | "MeshInstance" => icons::BOX,
        "Image Prop" | "ImageProp" => icons::PALETTE,
        "Box Prop" | "BoxProp" => icons::BOX,
        "Model Renderer" | "ModelRenderer" => icons::BOX,
        "Animator" => icons::PLAY,
        "Collider" => icons::SCALE_3D,
        "Character Controller" | "CharacterController" => icons::MAP_PIN,
        "Equipment" => icons::WAYPOINT,
        "Light" => icons::SUN,
        "Point Light" | "PointLight" => icons::SUN,
        "Particle Emitter" | "ParticleEmitter" => icons::FOCUS,
        "Point of Interest" | "PointOfInterest" => icons::FOCUS,
        "Spawn Point" | "SpawnPoint" => icons::MAP_PIN,
        "Portal" => icons::WAYPOINT,
        _ => icons::CIRCLE_DOT,
    }
}

pub(crate) fn node_lucide_color(kind: &str, root: bool, selected: bool) -> Color32 {
    if selected {
        return Color32::WHITE;
    }
    if root {
        return STUDIO_ACCENT;
    }

    match kind {
        "Entity" => Color32::from_rgb(156, 174, 190),
        "World" => Color32::from_rgb(232, 152, 96),
        "Section" | "Room" | "Map" => Color32::from_rgb(209, 118, 71),
        "Mesh Instance" | "MeshInstance" => Color32::from_rgb(156, 174, 190),
        "Image Prop" | "ImageProp" => Color32::from_rgb(210, 170, 120),
        "Box Prop" | "BoxProp" => Color32::from_rgb(135, 180, 220),
        "Model Renderer" | "ModelRenderer" => Color32::from_rgb(134, 168, 196),
        "Animator" => Color32::from_rgb(126, 164, 220),
        "Collider" => Color32::from_rgb(180, 170, 112),
        "Character Controller" | "CharacterController" => Color32::from_rgb(104, 194, 142),
        "Equipment" => Color32::from_rgb(210, 190, 104),
        "Light" => Color32::from_rgb(238, 203, 116),
        "Point Light" | "PointLight" => Color32::from_rgb(238, 203, 116),
        "Particle Emitter" | "ParticleEmitter" => Color32::from_rgb(152, 214, 230),
        "Point of Interest" | "PointOfInterest" => Color32::from_rgb(224, 72, 56),
        "Spawn Point" | "SpawnPoint" => Color32::from_rgb(236, 188, 104),
        "Portal" => PORTAL_PINK,
        _ => Color32::from_rgb(141, 160, 180),
    }
}

pub(crate) fn draw_inline_icon(ui: &mut egui::Ui, icon: char, color: Color32) {
    ui.label(icons::text(icon, 16.0).color(color));
}

/// Toolbar group button showing the group's icon plus its current
/// value (e.g. rotate icon + "Rotate"), so the active mode is readable
/// without hovering.
pub(crate) fn toolbar_group_menu<R>(
    ui: &mut egui::Ui,
    number: u8,
    glow: f32,
    icon: char,
    label: &str,
    current: &str,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) {
    toolbar_group_menu_impl(ui, number, glow, icon, label, current, true, add_contents)
}

/// Icon-only toolbar group button for groups whose state is a set of
/// toggles rather than a single mode (e.g. Visibility), where one
/// word can't summarise the current value.
pub(crate) fn toolbar_group_menu_icon_only<R>(
    ui: &mut egui::Ui,
    number: u8,
    glow: f32,
    icon: char,
    label: &str,
    current: &str,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) {
    toolbar_group_menu_impl(ui, number, glow, icon, label, current, false, add_contents)
}

fn toolbar_group_menu_impl<R>(
    ui: &mut egui::Ui,
    number: u8,
    glow: f32,
    icon: char,
    label: &str,
    current: &str,
    show_value: bool,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) {
    let number_text = number.to_string();
    let shortcut = command_shortcut_text(&number_text);
    let reverse_shortcut = command_shift_shortcut_text(&number_text);
    let shortcut_summary = format!("Shortcut: {shortcut} / Reverse: {reverse_shortcut}");
    let glow = glow.clamp(0.0, 1.0);
    let mut button = if show_value {
        egui::Button::new(icons::label(icon, current)).min_size(Vec2::new(30.0, 23.0))
    } else {
        egui::Button::new(icons::text(icon, 15.0)).min_size(Vec2::new(30.0, 23.0))
    };
    if glow > 0.0 {
        let fill_alpha = (34.0 + 58.0 * glow).round() as u8;
        let stroke_alpha = (120.0 + 120.0 * glow).round() as u8;
        button = button
            .fill(Color32::from_rgba_unmultiplied(
                STUDIO_ACCENT.r(),
                STUDIO_ACCENT.g(),
                STUDIO_ACCENT.b(),
                fill_alpha,
            ))
            .stroke(Stroke::new(
                1.0 + glow * 0.75,
                Color32::from_rgba_unmultiplied(
                    STUDIO_ACCENT_HOVER.r(),
                    STUDIO_ACCENT_HOVER.g(),
                    STUDIO_ACCENT_HOVER.b(),
                    stroke_alpha,
                ),
            ));
    }
    let footer = shortcut_summary.clone();
    let response = egui::menu::menu_custom_button(ui, button, |ui| {
        let result = add_contents(ui);
        ui.separator();
        ui.label(RichText::new(footer).small().color(STUDIO_TEXT_WEAK));
        result
    })
    .response;
    response.on_hover_text(format!(
        "{label}: {current}\n{shortcut} cycles forward\n{reverse_shortcut} cycles backward"
    ));
}

/// Toolbar option button showing the option's icon plus a short
/// `button_text`; the full `label: current` state lives in the hover
/// tooltip and the active fill marks enabled toggles.
pub(crate) fn toolbar_option_menu<R>(
    ui: &mut egui::Ui,
    icon: char,
    button_text: &str,
    label: &str,
    current: impl Into<String>,
    active: bool,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) {
    let mut button =
        egui::Button::new(icons::label(icon, button_text)).min_size(Vec2::new(30.0, 23.0));
    if active {
        button = button
            .fill(Color32::from_rgba_unmultiplied(
                STUDIO_ACCENT.r(),
                STUDIO_ACCENT.g(),
                STUDIO_ACCENT.b(),
                44,
            ))
            .stroke(Stroke::new(
                1.0,
                Color32::from_rgba_unmultiplied(
                    STUDIO_ACCENT_HOVER.r(),
                    STUDIO_ACCENT_HOVER.g(),
                    STUDIO_ACCENT_HOVER.b(),
                    180,
                ),
            ));
    }
    let current = current.into();
    let response = egui::menu::menu_custom_button(ui, button, add_contents).response;
    response.on_hover_text(format!("{label}: {current}"));
}

pub(crate) fn toolbar_menu_choice(
    ui: &mut egui::Ui,
    label: impl Into<egui::WidgetText>,
    selected: bool,
) -> bool {
    let clicked = ui.selectable_label(selected, label).clicked();
    if clicked {
        ui.close_menu();
    }
    clicked
}

pub(crate) fn visibility_menu_row(
    ui: &mut egui::Ui,
    id_salt: &'static str,
    label: &str,
    visible: &mut bool,
) -> bool {
    ui.label(label);
    let icon = if *visible { icons::EYE } else { icons::EYE_OFF };
    let icon_color = if *visible {
        STUDIO_TEXT
    } else {
        Color32::from_rgb(82, 92, 102)
    };
    let response = ui
        .push_id(id_salt, |ui| {
            ui.add_sized(
                [32.0, 22.0],
                egui::Button::new(icons::text(icon, 14.0).color(icon_color)),
            )
            .on_hover_text(if *visible { "Hide" } else { "Show" })
        })
        .inner;
    let changed = response.clicked();
    if changed {
        *visible = !*visible;
    }
    ui.end_row();
    changed
}

pub(crate) fn viewport_camera_mode_label(mode: ViewportCameraMode) -> &'static str {
    match mode {
        ViewportCameraMode::Orbit => "Orbit",
        ViewportCameraMode::Free => "Free",
    }
}

#[cfg(test)]
mod sky_ux_tests {
    use super::*;

    fn collect_text(shape: &egui::epaint::Shape, out: &mut String) {
        match shape {
            egui::epaint::Shape::Text(text) => {
                out.push_str(&text.galley.job.text);
                out.push(' ');
            }
            egui::epaint::Shape::Vec(shapes) => {
                for shape in shapes {
                    collect_text(shape, out);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn world_sky_editor_exposes_one_projection_source_and_visibility_policy() {
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
        let mut sky = SkySettings {
            mode: SkyMode::Cube,
            visibility: SkyVisibility::ThroughSkySurfaces,
            texture: None,
            ..SkySettings::default()
        };
        let mut far_vista = FarVistaSettings::default();
        let mut culling = WorldCullingSettings::default();
        let mut physics = WorldPhysicsSettings::default();
        let mut world_message = None;
        let mut nav_target = None;
        let mut project = ProjectDocument::new("sky ux");
        let atlas = project.add_resource(
            "Sunset Atlas",
            ResourceData::Material(MaterialResource::opaque(Some("sky.psxt".to_string()))),
        );
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(520.0, 2200.0))),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    draw_world_settings(
                        ui,
                        &mut sky,
                        &mut far_vista,
                        &mut culling,
                        &mut physics,
                        &mut world_message,
                        &[(atlas, "Sunset Atlas".to_string())],
                        &mut nav_target,
                    );
                });
            },
        );
        let mut text = String::new();
        for clipped in &output.shapes {
            collect_text(&clipped.shape, &mut text);
        }
        assert!(text.contains("Directional cube"), "{text}");
        assert!(text.contains("Test sky"), "{text}");
        assert!(text.contains("Procedural"), "{text}");
        assert!(text.contains("Quake layered"), "{text}");
        assert!(text.contains("Sunset cube"), "{text}");
        assert!(text.contains("Through sky surfaces"), "{text}");
        assert!(text.contains("Sky Texture"), "{text}");
        assert!(text.contains("1536×256"), "{text}");
        assert!(
            text.contains("Choose a textured Material before building this sky"),
            "{text}"
        );
    }
}
