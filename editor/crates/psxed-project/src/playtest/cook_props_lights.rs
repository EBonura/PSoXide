use super::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_material_texture_asset(
    project: &ProjectDocument,
    project_root: &Path,
    label: &str,
    material_id: ResourceId,
    texture_asset_for_path: &mut HashMap<String, usize>,
    assets: &mut Vec<PlaytestAsset>,
    report: &mut PlaytestValidationReport,
) -> Option<(usize, [u8; 3])> {
    let Some(material_resource) = project.resource(material_id) else {
        report.warn(format!(
            "{label} references missing Material #{} - skipped",
            material_id.raw()
        ));
        return None;
    };
    let ResourceData::Material(material) = &material_resource.data else {
        report.warn(format!(
            "{label} references '{}' but it is not a Material - skipped",
            material_resource.name
        ));
        return None;
    };
    let Some(psxt_path) = material.psxt_path.clone() else {
        report.warn(format!(
            "{label} material '{}' has no Texture - skipped",
            material_resource.name
        ));
        return None;
    };
    let texture_asset_index = if let Some(&existing) = texture_asset_for_path.get(&psxt_path) {
        existing
    } else {
        let bytes = match load_psxt_bytes(&material_resource.name, &psxt_path, project_root) {
            Ok(bytes) => bytes,
            Err(msg) => {
                report.warn(format!("{label}: {msg} - skipped"));
                return None;
            }
        };
        if let Err(msg) = expect_room_material_depth(&material_resource.name, &bytes) {
            report.warn(format!("{label}: {msg} - skipped"));
            return None;
        }
        let texture_index = texture_asset_for_path.len();
        let new_index = assets.len();
        assets.push(PlaytestAsset {
            kind: PlaytestAssetKind::Texture,
            bytes,
            filename: format!("texture_{texture_index:03}.psxt"),
            source_label: material_resource.name.clone(),
            streamed_class: StreamedClass::None,
        });
        texture_asset_for_path.insert(psxt_path, new_index);
        new_index
    };
    Some((texture_asset_index, material.tint))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn push_image_prop(
    project: &ProjectDocument,
    project_root: &Path,
    node_name: &str,
    room_index: u16,
    pos: [i32; 3],
    pitch: i16,
    yaw: i16,
    roll: i16,
    material: Option<ResourceId>,
    width: u16,
    height: u16,
    cylindrical_billboard: bool,
    texture_asset_for_path: &mut HashMap<String, usize>,
    assets: &mut Vec<PlaytestAsset>,
    image_props: &mut Vec<PlaytestImageProp>,
    report: &mut PlaytestValidationReport,
) -> bool {
    let Some(material_id) = material else {
        report.warn(format!(
            "Image Prop '{node_name}' has no Material - skipped"
        ));
        return true;
    };
    let label = format!("Image Prop '{node_name}'");
    let Some((texture_asset_index, tint_rgb)) = resolve_material_texture_asset(
        project,
        project_root,
        &label,
        material_id,
        texture_asset_for_path,
        assets,
        report,
    ) else {
        return true;
    };
    image_props.push(PlaytestImageProp {
        room: room_index,
        texture_asset_index,
        x: pos[0],
        y: pos[1],
        z: pos[2],
        pitch,
        yaw,
        roll,
        width: width.max(1),
        height: height.max(1),
        tint_rgb,
        baked_vertex_rgb: [rgb_tuple(tint_rgb); 4],
        flags: if cylindrical_billboard {
            image_prop_flags::CYLINDRICAL_BILLBOARD
        } else {
            0
        },
    });
    true
}

pub(crate) const BOX_PROP_FACE_VERTEX_INDICES: [[usize; 4]; psx_level::BOX_PROP_FACE_COUNT] = [
    [4, 5, 1, 0],
    [5, 6, 2, 1],
    [6, 7, 3, 2],
    [7, 4, 0, 3],
    [7, 6, 5, 4],
    [0, 1, 2, 3],
];

#[allow(clippy::too_many_arguments)]
pub(crate) fn push_box_prop(
    project: &ProjectDocument,
    project_root: &Path,
    node_name: &str,
    room_index: u16,
    pos: [i32; 3],
    ground_y: i32,
    pitch: i16,
    yaw: i16,
    roll: i16,
    materials: &[Option<ResourceId>; crate::BOX_PROP_FACE_COUNT],
    vertices: [[i16; 3]; crate::BOX_PROP_VERTEX_COUNT],
    collision_enabled: bool,
    break_flags: u16,
    texture_asset_for_path: &mut HashMap<String, usize>,
    assets: &mut Vec<PlaytestAsset>,
    box_props: &mut Vec<PlaytestBoxProp>,
    report: &mut PlaytestValidationReport,
) -> bool {
    let mut texture_asset_indices = [None; psx_level::BOX_PROP_FACE_COUNT];
    let mut tint_rgb = [[128, 128, 128]; psx_level::BOX_PROP_FACE_COUNT];
    let mut valid_faces = 0usize;
    for (face, material) in materials.iter().enumerate() {
        let Some(material_id) = *material else {
            continue;
        };
        let label = format!(
            "Box Prop '{node_name}' {} face",
            crate::BOX_PROP_FACE_NAMES[face]
        );
        let Some((texture_asset_index, tint)) = resolve_material_texture_asset(
            project,
            project_root,
            &label,
            material_id,
            texture_asset_for_path,
            assets,
            report,
        ) else {
            continue;
        };
        texture_asset_indices[face] = Some(texture_asset_index);
        tint_rgb[face] = tint;
        valid_faces += 1;
    }

    if valid_faces == 0 {
        report.warn(format!(
            "Box Prop '{node_name}' has no drawable Material faces - skipped"
        ));
        return true;
    }

    let mut baked_vertex_rgb = [[rgb_tuple([128, 128, 128]); 4]; psx_level::BOX_PROP_FACE_COUNT];
    for face in 0..psx_level::BOX_PROP_FACE_COUNT {
        baked_vertex_rgb[face] = [rgb_tuple(tint_rgb[face]); 4];
    }

    let mut flags = break_flags & box_prop_flags::BREAK_ON_MASK;
    if collision_enabled {
        flags |= box_prop_flags::COLLISION_ENABLED;
    }

    box_props.push(PlaytestBoxProp {
        room: room_index,
        texture_asset_indices,
        x: pos[0],
        y: pos[1],
        z: pos[2],
        ground_y,
        pitch,
        yaw,
        roll,
        vertices,
        tint_rgb,
        baked_vertex_rgb,
        flags,
    });
    true
}

pub(crate) fn push_point_light(
    node_name: &str,
    grid: &crate::WorldGrid,
    room_index: u16,
    pos: [i32; 3],
    color: [u8; 3],
    intensity: f32,
    radius: f32,
    lights: &mut Vec<PlaytestLight>,
    report: &mut PlaytestValidationReport,
) -> bool {
    // Reject obviously broken lights at cook time -- radius 0
    // contributes nothing, negative intensity is meaningless.
    // Clamp the rest into the wire format's u16 ranges.
    if radius <= 0.0 {
        report.error(format!(
            "Light '{node_name}' has radius {radius} (must be > 0)"
        ));
        return false;
    }
    if !intensity.is_finite() || intensity < 0.0 {
        report.error(format!(
            "Light '{node_name}' has invalid intensity {intensity}"
        ));
        return false;
    }
    // Editor radius is in *sector units* -- convert to world
    // units (engine units) at cook time so the runtime record
    // stays in one canonical unit regardless of room sector size.
    let radius_world = spatial::light_radius_record_units(grid, radius);
    let intensity_q8 = (intensity * 256.0).clamp(0.0, u16::MAX as f32) as u16;
    lights.push(PlaytestLight {
        room: room_index,
        x: pos[0],
        y: pos[1],
        z: pos[2],
        radius: radius_world,
        intensity_q8,
        color,
    });
    true
}

pub(crate) fn push_particle_emitter(
    node_name: &str,
    room_index: u16,
    pos: [i32; 3],
    settings: &ParticleEmitterSettings,
    particle_emitters: &mut Vec<PlaytestParticleEmitter>,
    report: &mut PlaytestValidationReport,
) -> bool {
    if !settings.enabled {
        return true;
    }
    if settings.max_particles == 0 {
        report.warn(format!(
            "Particle Emitter '{node_name}' has max_particles=0 -- skipped"
        ));
        return true;
    }
    if settings.lifetime_frames == 0 {
        report.warn(format!(
            "Particle Emitter '{node_name}' has lifetime_frames=0 -- skipped"
        ));
        return true;
    }
    particle_emitters.push(PlaytestParticleEmitter {
        room: room_index,
        x: pos[0],
        y: pos[1],
        z: pos[2],
        max_particles: settings.max_particles,
        spawn_rate_q8: settings.spawn_rate_q8,
        lifetime_frames: settings.lifetime_frames,
        start_size: settings.start_size,
        end_size: settings.end_size,
        start_color: settings.start_color,
        end_color: settings.end_color,
        blend_mode: particle_blend_mode_code(settings.blend_mode),
        base_velocity_q4: settings.base_velocity_q4,
        random_velocity_q4: settings.random_velocity_q4,
        acceleration_q4: settings.acceleration_q4,
        spawn_radius: settings.spawn_radius,
        flags: particle_emitter_flags::ENABLED,
    });
    true
}

pub(crate) fn push_interactable(
    node_name: &str,
    room_index: u16,
    pos: [i32; 3],
    yaw: i16,
    component: InteractableComponent<'_>,
    messages: &mut Vec<PlaytestInteractableMessage>,
    interactables: &mut Vec<PlaytestInteractable>,
    report: &mut PlaytestValidationReport,
) -> bool {
    if component.radius == 0 {
        report.error(format!(
            "Interactable on '{node_name}' has radius 0 (must be > 0)"
        ));
        return false;
    }

    let prompt = non_empty_or(
        component.prompt,
        default_prompt_for_interactable(component.kind),
    );
    let (kind, title, body, checkpoint_id) = match component.kind {
        crate::InteractableKind::Message { title, body } => (
            PlaytestInteractableKind::Message,
            non_empty_or(title, "ECHO REMNANT").to_string(),
            body.clone(),
            String::new(),
        ),
        crate::InteractableKind::Checkpoint {
            checkpoint_id,
            title,
            body,
        } => (
            PlaytestInteractableKind::Checkpoint,
            non_empty_or(title, "SYNC RELAY").to_string(),
            non_empty_or(body, "Relay synchronized.").to_string(),
            non_empty_or(checkpoint_id, node_name).to_string(),
        ),
    };
    let message = messages.len().min(u16::MAX as usize) as u16;
    messages.push(PlaytestInteractableMessage { title, body });
    interactables.push(PlaytestInteractable {
        room: room_index,
        kind,
        x: pos[0],
        y: pos[1],
        z: pos[2],
        yaw,
        radius: component.radius,
        prompt: prompt.to_string(),
        message,
        checkpoint_id,
        flags: if component.enabled {
            psx_level::interactable_flags::ENABLED
        } else {
            0
        },
    });
    true
}

pub(crate) fn non_empty_or<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.trim().is_empty() {
        fallback
    } else {
        value
    }
}

pub(crate) fn default_prompt_for_interactable(kind: &crate::InteractableKind) -> &'static str {
    match kind {
        crate::InteractableKind::Message { .. } => "READ ECHO",
        crate::InteractableKind::Checkpoint { .. } => "SYNCHRONIZE",
    }
}

pub(crate) const fn particle_blend_mode_code(mode: PsxBlendMode) -> u8 {
    match mode {
        PsxBlendMode::Opaque | PsxBlendMode::Average => 0,
        PsxBlendMode::Add => 1,
        PsxBlendMode::Subtract => 2,
        PsxBlendMode::AddQuarter => 3,
    }
}

pub(crate) fn expand_lights_across_chunks(
    rooms: &[CookedRoomBakeInput],
    lights: &[PlaytestLight],
) -> Vec<PlaytestLight> {
    let mut out = Vec::new();
    for light in lights {
        let Some(source_room) = rooms.iter().find(|room| room.room_index == light.room) else {
            out.push(*light);
            continue;
        };
        let source_origin = room_origin_units(source_room);
        let global_x = source_origin[0].saturating_add(light.x);
        let global_z = source_origin[1].saturating_add(light.z);
        // Absolute Y of the light (source floor elevation + local Y).
        // `light.y` already carries the entity's floor via its authored
        // transform, so a floor-1 light sits in floor 1's band.
        let global_y = source_room.origin_y.saturating_add(light.y);
        let mut emitted = false;
        for target_room in rooms {
            if !light_overlaps_room_chunk(global_x, global_z, light.radius, target_room) {
                continue;
            }
            // Keep light on its own level: only spill to a target floor
            // whose band contains the light's Y (within its radius). This
            // stops a floor-1 light from lighting floor 0's overlapping
            // chunk and vice versa. Same-floor chunks share `origin_y`, so
            // intra-floor spill is unaffected.
            let dy = global_y.saturating_sub(target_room.origin_y);
            if i64::from(dy).saturating_mul(i64::from(dy))
                > i64::from(light.radius).saturating_mul(i64::from(light.radius))
            {
                continue;
            }
            let target_origin = room_origin_units(target_room);
            out.push(PlaytestLight {
                room: target_room.room_index,
                x: global_x.saturating_sub(target_origin[0]),
                y: light.y,
                z: global_z.saturating_sub(target_origin[1]),
                radius: light.radius,
                intensity_q8: light.intensity_q8,
                color: light.color,
            });
            emitted = true;
        }
        if !emitted {
            out.push(*light);
        }
    }
    out
}

pub(crate) fn light_overlaps_room_chunk(
    global_x: i32,
    global_z: i32,
    radius: u16,
    room: &CookedRoomBakeInput,
) -> bool {
    let origin = room_origin_units(room);
    let min_x = origin[0] as i64;
    let min_z = origin[1] as i64;
    let max_x =
        origin[0].saturating_add((room.cooked.width as i32) * room.cooked.sector_size) as i64;
    let max_z =
        origin[1].saturating_add((room.cooked.depth as i32) * room.cooked.sector_size) as i64;
    let x = global_x as i64;
    let z = global_z as i64;
    let closest_x = x.clamp(min_x, max_x);
    let closest_z = z.clamp(min_z, max_z);
    let dx = x - closest_x;
    let dz = z - closest_z;
    let radius = radius as i64;
    dx.saturating_mul(dx).saturating_add(dz.saturating_mul(dz)) <= radius.saturating_mul(radius)
}

pub(crate) fn room_origin_units(room: &CookedRoomBakeInput) -> [i32; 2] {
    [
        room.world_origin[0].saturating_mul(room.cooked.sector_size),
        room.world_origin[1].saturating_mul(room.cooked.sector_size),
    ]
}

pub(crate) fn bake_static_surface_lights(
    rooms: &mut [CookedRoomBakeInput],
    lights: &[PlaytestLight],
) {
    for room in rooms {
        room.cooked.static_vertex_lighting = true;
        let room_lights: Vec<&PlaytestLight> = lights
            .iter()
            .filter(|light| light.room == room.room_index)
            .collect();
        let depth = room.cooked.depth as usize;
        let sector_size = room.cooked.sector_size;
        let ambient = room.cooked.ambient_color;
        let materials = room.cooked.materials.clone();
        for (idx, sector) in room.cooked.sectors.iter_mut().enumerate() {
            let Some(sector) = sector else {
                continue;
            };
            let sx = (idx / depth) as u16;
            let sz = (idx % depth) as u16;
            if let Some(face) = &mut sector.floor {
                let verts = horizontal_vertices(sx, sz, sector_size, face.heights);
                face.baked_vertex_rgb = bake_surface_vertex_rgb(
                    &materials,
                    ambient,
                    verts,
                    face.material,
                    &room_lights,
                );
            }
            if let Some(face) = &mut sector.ceiling {
                let verts =
                    reverse_quad_vertices(horizontal_vertices(sx, sz, sector_size, face.heights));
                face.baked_vertex_rgb = bake_surface_vertex_rgb(
                    &materials,
                    ambient,
                    verts,
                    face.material,
                    &room_lights,
                );
            }

            for (direction, walls) in [
                (psxw::direction::NORTH, sector.walls.north.as_mut_slice()),
                (psxw::direction::EAST, sector.walls.east.as_mut_slice()),
                (psxw::direction::SOUTH, sector.walls.south.as_mut_slice()),
                (psxw::direction::WEST, sector.walls.west.as_mut_slice()),
                (
                    psxw::direction::NORTH_WEST_SOUTH_EAST,
                    sector.walls.north_west_south_east.as_mut_slice(),
                ),
                (
                    psxw::direction::NORTH_EAST_SOUTH_WEST,
                    sector.walls.north_east_south_west.as_mut_slice(),
                ),
            ] {
                for wall in walls {
                    if let Some(verts) = wall_vertices(sx, sz, sector_size, direction, wall.heights)
                    {
                        wall.baked_vertex_rgb = bake_surface_vertex_rgb(
                            &materials,
                            ambient,
                            verts,
                            wall.material,
                            &room_lights,
                        );
                    }
                }
            }
        }
    }
}

pub(crate) fn bake_static_image_prop_lights(
    image_props: &mut [PlaytestImageProp],
    rooms: &[CookedRoomBakeInput],
    lights: &[PlaytestLight],
) {
    for prop in image_props {
        let Some(room) = rooms.iter().find(|room| room.room_index == prop.room) else {
            continue;
        };
        let room_lights: Vec<&PlaytestLight> = lights
            .iter()
            .filter(|light| light.room == prop.room)
            .collect();
        let ambient = room.cooked.ambient_color;
        let base = prop.tint_rgb;
        prop.baked_vertex_rgb = if prop.flags & image_prop_flags::CYLINDRICAL_BILLBOARD != 0 {
            let bottom = [prop.x, prop.y, prop.z];
            let top = [prop.x, prop.y.saturating_add(prop.height as i32), prop.z];
            let top_rgb = rgb_tuple(bake_static_vertex_rgb(top, base, ambient, &room_lights));
            let bottom_rgb = rgb_tuple(bake_static_vertex_rgb(bottom, base, ambient, &room_lights));
            [top_rgb, top_rgb, bottom_rgb, bottom_rgb]
        } else {
            let vertices = image_prop_static_vertices(prop);
            [
                rgb_tuple(bake_static_vertex_rgb(
                    vertices[0],
                    base,
                    ambient,
                    &room_lights,
                )),
                rgb_tuple(bake_static_vertex_rgb(
                    vertices[1],
                    base,
                    ambient,
                    &room_lights,
                )),
                rgb_tuple(bake_static_vertex_rgb(
                    vertices[2],
                    base,
                    ambient,
                    &room_lights,
                )),
                rgb_tuple(bake_static_vertex_rgb(
                    vertices[3],
                    base,
                    ambient,
                    &room_lights,
                )),
            ]
        };
    }
}

pub(crate) fn bake_static_box_prop_lights(
    box_props: &mut [PlaytestBoxProp],
    rooms: &[CookedRoomBakeInput],
    lights: &[PlaytestLight],
) {
    for prop in box_props {
        let Some(room) = rooms.iter().find(|room| room.room_index == prop.room) else {
            continue;
        };
        let room_lights: Vec<&PlaytestLight> = lights
            .iter()
            .filter(|light| light.room == prop.room)
            .collect();
        let ambient = room.cooked.ambient_color;
        let face_vertices = box_prop_static_face_vertices(prop);
        for face in 0..psx_level::BOX_PROP_FACE_COUNT {
            let base = prop.tint_rgb[face];
            prop.baked_vertex_rgb[face] = [
                rgb_tuple(bake_static_vertex_rgb(
                    face_vertices[face][0],
                    base,
                    ambient,
                    &room_lights,
                )),
                rgb_tuple(bake_static_vertex_rgb(
                    face_vertices[face][1],
                    base,
                    ambient,
                    &room_lights,
                )),
                rgb_tuple(bake_static_vertex_rgb(
                    face_vertices[face][2],
                    base,
                    ambient,
                    &room_lights,
                )),
                rgb_tuple(bake_static_vertex_rgb(
                    face_vertices[face][3],
                    base,
                    ambient,
                    &room_lights,
                )),
            ];
        }
    }
}

pub(crate) fn image_prop_static_vertices(prop: &PlaytestImageProp) -> [[i32; 3]; 4] {
    let half_width = (prop.width as i32) / 2;
    let height = prop.height as i32;
    let locals = [
        [-half_width, height, 0],
        [half_width, height, 0],
        [half_width, 0, 0],
        [-half_width, 0, 0],
    ];
    let mut out = [[0, 0, 0]; 4];
    for (idx, local) in locals.iter().enumerate() {
        let rotated = crate::spatial::rotate_euler_local_q12(
            *local,
            prop.pitch as u16,
            prop.yaw as u16,
            prop.roll as u16,
        );
        out[idx] = [
            prop.x.saturating_add(rotated[0]),
            prop.y.saturating_add(rotated[1]),
            prop.z.saturating_add(rotated[2]),
        ];
    }
    out
}

pub(crate) fn box_prop_static_face_vertices(
    prop: &PlaytestBoxProp,
) -> [[[i32; 3]; 4]; psx_level::BOX_PROP_FACE_COUNT] {
    let mut vertices = [[0, 0, 0]; psx_level::BOX_PROP_VERTEX_COUNT];
    for (idx, local) in prop.vertices.iter().enumerate() {
        let rotated = crate::spatial::rotate_euler_local_q12(
            [local[0] as i32, local[1] as i32, local[2] as i32],
            prop.pitch as u16,
            prop.yaw as u16,
            prop.roll as u16,
        );
        vertices[idx] = [
            prop.x.saturating_add(rotated[0]),
            prop.y.saturating_add(rotated[1]),
            prop.z.saturating_add(rotated[2]),
        ];
    }

    let mut faces = [[[0, 0, 0]; 4]; psx_level::BOX_PROP_FACE_COUNT];
    for face in 0..psx_level::BOX_PROP_FACE_COUNT {
        for corner in 0..4 {
            faces[face][corner] = vertices[BOX_PROP_FACE_VERTEX_INDICES[face][corner]];
        }
    }
    faces
}

pub(crate) const fn rgb_tuple(rgb: [u8; 3]) -> (u8, u8, u8) {
    (rgb[0], rgb[1], rgb[2])
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn bake_surface_vertex_rgb(
    materials: &[CookedWorldMaterial],
    ambient: [u8; 3],
    vertices: [[i32; 3]; 4],
    material_slot: u16,
    lights: &[&PlaytestLight],
) -> [[u8; 3]; 4] {
    let base = cooked_material_tint(materials, material_slot);
    [
        bake_static_vertex_rgb(vertices[0], base, ambient, lights),
        bake_static_vertex_rgb(vertices[1], base, ambient, lights),
        bake_static_vertex_rgb(vertices[2], base, ambient, lights),
        bake_static_vertex_rgb(vertices[3], base, ambient, lights),
    ]
}

pub(crate) fn cooked_material_tint(materials: &[CookedWorldMaterial], slot: u16) -> [u8; 3] {
    materials
        .iter()
        .find(|material| material.slot == slot)
        .map(|material| material.tint)
        .unwrap_or([128, 128, 128])
}

pub(crate) fn horizontal_vertices(
    sx: u16,
    sz: u16,
    sector_size: i32,
    heights: [i32; 4],
) -> [[i32; 3]; 4] {
    let x0 = (sx as i32) * sector_size;
    let x1 = ((sx as i32) + 1) * sector_size;
    let z0 = (sz as i32) * sector_size;
    let z1 = ((sz as i32) + 1) * sector_size;
    [
        [x0, heights[0], z0],
        [x1, heights[1], z0],
        [x1, heights[2], z1],
        [x0, heights[3], z1],
    ]
}

pub(crate) fn reverse_quad_vertices(vertices: [[i32; 3]; 4]) -> [[i32; 3]; 4] {
    [vertices[3], vertices[2], vertices[1], vertices[0]]
}

pub(crate) fn wall_vertices(
    sx: u16,
    sz: u16,
    sector_size: i32,
    direction: u8,
    heights: [i32; 4],
) -> Option<[[i32; 3]; 4]> {
    let x0 = (sx as i32) * sector_size;
    let x1 = ((sx as i32) + 1) * sector_size;
    let z0 = (sz as i32) * sector_size;
    let z1 = ((sz as i32) + 1) * sector_size;
    match direction {
        psxw::direction::NORTH => Some([
            [x0, heights[0], z0],
            [x1, heights[1], z0],
            [x1, heights[2], z0],
            [x0, heights[3], z0],
        ]),
        psxw::direction::EAST => Some([
            [x1, heights[0], z0],
            [x1, heights[1], z1],
            [x1, heights[2], z1],
            [x1, heights[3], z0],
        ]),
        psxw::direction::SOUTH => Some([
            [x1, heights[0], z1],
            [x0, heights[1], z1],
            [x0, heights[2], z1],
            [x1, heights[3], z1],
        ]),
        psxw::direction::WEST => Some([
            [x0, heights[0], z1],
            [x0, heights[1], z0],
            [x0, heights[2], z0],
            [x0, heights[3], z1],
        ]),
        psxw::direction::NORTH_WEST_SOUTH_EAST => Some([
            [x0, heights[0], z0],
            [x1, heights[1], z1],
            [x1, heights[2], z1],
            [x0, heights[3], z0],
        ]),
        psxw::direction::NORTH_EAST_SOUTH_WEST => Some([
            [x1, heights[0], z0],
            [x0, heights[1], z1],
            [x0, heights[2], z1],
            [x1, heights[3], z0],
        ]),
        _ => None,
    }
}

pub(crate) fn bake_static_vertex_rgb(
    point: [i32; 3],
    base: [u8; 3],
    ambient: [u8; 3],
    lights: &[&PlaytestLight],
) -> [u8; 3] {
    const LIGHTING_NEUTRAL: u32 = 128;
    const LIGHTING_MAX: u32 = 255;
    let mut accum = [ambient[0] as u32, ambient[1] as u32, ambient[2] as u32];
    for light in lights {
        let Some(weight_q8) =
            point_light_weight_q8(point, [light.x, light.y, light.z], light.radius)
        else {
            continue;
        };
        for (channel, color) in accum.iter_mut().zip(light.color) {
            let weighted = (color as u32).saturating_mul(light.intensity_q8 as u32);
            *channel = channel.saturating_add(weighted.saturating_mul(weight_q8) >> 16);
        }
    }
    [
        ((base[0] as u32 * accum[0].min(LIGHTING_MAX)) / LIGHTING_NEUTRAL).min(255) as u8,
        ((base[1] as u32 * accum[1].min(LIGHTING_MAX)) / LIGHTING_NEUTRAL).min(255) as u8,
        ((base[2] as u32 * accum[2].min(LIGHTING_MAX)) / LIGHTING_NEUTRAL).min(255) as u8,
    ]
}

pub(crate) fn point_light_weight_q8(
    point: [i32; 3],
    light_position: [i32; 3],
    radius: u16,
) -> Option<u32> {
    let radius = radius as u32;
    if radius == 0 {
        return None;
    }
    let dx = point[0].abs_diff(light_position[0]);
    let dy = point[1].abs_diff(light_position[1]);
    let dz = point[2].abs_diff(light_position[2]);
    if dx >= radius || dy >= radius || dz >= radius {
        return None;
    }
    let d2 = dx
        .checked_mul(dx)?
        .checked_add(dy.checked_mul(dy)?)?
        .checked_add(dz.checked_mul(dz)?)?;
    let r2 = radius.checked_mul(radius)?;
    if d2 >= r2 {
        return None;
    }
    Some((radius - isqrt_u32(d2)).saturating_mul(256) / radius)
}

pub(crate) fn isqrt_u32(value: u32) -> u32 {
    let mut x = value;
    let mut r = 0u32;
    let mut bit = 1u32 << 30;
    while bit > x {
        bit >>= 2;
    }
    while bit != 0 {
        if x >= r + bit {
            x -= r + bit;
            r = (r >> 1) + bit;
        } else {
            r >>= 1;
        }
        bit >>= 2;
    }
    r
}

pub(crate) const FULL_HEIGHT_BLOCKER_TOLERANCE: i32 = 32;
