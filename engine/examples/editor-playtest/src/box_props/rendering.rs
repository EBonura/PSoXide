use super::*;

#[derive(Copy, Clone)]
struct BoxPropFaceTextureRuntime {
    material: TextureMaterial,
    u_max: u8,
    v_max: u8,
}

fn box_prop_face_textures(
    prop: &LevelBoxPropRecord,
) -> [Option<BoxPropFaceTextureRuntime>; psx_level::BOX_PROP_FACE_COUNT] {
    let mut textures = [None; psx_level::BOX_PROP_FACE_COUNT];
    let mut face = 0usize;
    while face < psx_level::BOX_PROP_FACE_COUNT {
        if let Some(texture_asset) = prop.texture_assets[face] {
            if let Some(slot) = prop_texture_slot(texture_asset) {
                textures[face] = Some(BoxPropFaceTextureRuntime {
                    material: TextureMaterial::opaque(
                        slot.clut_word,
                        slot.tpage_word,
                        (0x80, 0x80, 0x80),
                    )
                    .with_texture_window(slot.texture_window),
                    u_max: model_render_uv_max(slot.texture_width),
                    v_max: model_render_uv_max(slot.texture_height),
                });
            }
        }
        face += 1;
    }
    textures
}

pub(super) fn draw_box_props<T>(
    props: &[LevelBoxPropRecord],
    broken: &[u32; BOX_PROP_BROKEN_WORDS],
    runtime: &[BoxPropRuntime; MAX_BOX_PROP_STATE],
    fall: &[BoxPropFallState; MAX_BOX_PROP_STATE],
    current_room: RoomIndex,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<TriTextured>
        + PrimitiveSink<TriTexturedGouraud>
        + PrimitiveSink<QuadTexturedGouraud>,
{
    for (index, prop) in props.iter().enumerate() {
        if prop.room != current_room || box_prop_broken_in_words(broken, index) {
            continue;
        }
        let Some(box_runtime) = runtime.get(index) else {
            continue;
        };
        // A box mid-fall is drawn shifted down by its current fall offset.
        let fall_y = fall[index].fall_y;
        let cull_center = WorldVertex::new(
            box_runtime.cull_center.x,
            box_runtime.cull_center.y.saturating_add(fall_y),
            box_runtime.cull_center.z,
        );
        if !sphere_visible_to_camera(camera, options, cull_center, box_runtime.cull_radius, 96) {
            continue;
        }
        draw_box_prop_faces(
            prop,
            &box_runtime.faces,
            fall_y,
            camera,
            options,
            lighting,
            triangles,
            world,
        );
    }
}

pub(super) fn draw_box_prop_floor_debris<T>(
    props: &[LevelBoxPropRecord],
    broken: &[u32; BOX_PROP_BROKEN_WORDS],
    runtime: &[BoxPropRuntime; MAX_BOX_PROP_STATE],
    current_room: RoomIndex,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<QuadTexturedGouraud>
        + PrimitiveSink<TriTextured>
        + PrimitiveSink<TriTexturedGouraud>,
{
    let mut projector = None;
    for (index, prop) in props.iter().enumerate() {
        if prop.room != current_room || !box_prop_broken_in_words(broken, index) {
            continue;
        }
        let Some(box_runtime) = runtime.get(index) else {
            continue;
        };
        let debris_center = WorldVertex::new(
            box_runtime.cull_center.x,
            box_runtime.ground_y.saturating_add(16),
            box_runtime.cull_center.z,
        );
        if !sphere_visible_to_camera(
            camera,
            options,
            debris_center,
            box_runtime.cull_radius.saturating_mul(2),
            128,
        ) {
            continue;
        }
        let loaded_projector = match projector {
            Some(projector) => projector,
            None => {
                let loaded = LoadedWorldCameraGte::load(*camera);
                projector = Some(loaded);
                loaded
            }
        };
        let face_textures = box_prop_face_textures(prop);
        draw_box_prop_floor_debris_chips(
            prop,
            &face_textures,
            box_runtime.debris_bounds,
            box_runtime.ground_y,
            loaded_projector,
            camera,
            options,
            lighting,
            triangles,
            world,
        );
    }
}

fn draw_box_prop_floor_debris_chips<T>(
    prop: &LevelBoxPropRecord,
    face_textures: &[Option<BoxPropFaceTextureRuntime>; psx_level::BOX_PROP_FACE_COUNT],
    bounds: BoxPropDebrisBounds,
    floor_y: i32,
    projector: LoadedWorldCameraGte,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<QuadTexturedGouraud>
        + PrimitiveSink<TriTextured>
        + PrimitiveSink<TriTexturedGouraud>,
{
    for chip in BOX_PROP_FLOOR_DEBRIS_CHIPS {
        let face = chip.face as usize;
        if face >= psx_level::BOX_PROP_FACE_COUNT {
            continue;
        }
        draw_box_prop_floor_debris_chip(
            prop,
            face,
            face_textures[face],
            bounds,
            floor_y,
            chip,
            projector,
            camera,
            options,
            lighting,
            triangles,
            world,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_box_prop_floor_debris_chip<T>(
    prop: &LevelBoxPropRecord,
    face: usize,
    face_texture: Option<BoxPropFaceTextureRuntime>,
    bounds: BoxPropDebrisBounds,
    floor_y: i32,
    chip: BoxPropFloorDebrisChip,
    projector: LoadedWorldCameraGte,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<QuadTexturedGouraud>
        + PrimitiveSink<TriTextured>
        + PrimitiveSink<TriTexturedGouraud>,
{
    let Some(face_texture) = face_texture else {
        return;
    };

    let material = face_texture.material;
    let uvs = box_prop_floor_debris_uvs(face_texture.u_max, face_texture.v_max, chip);
    let quad = box_prop_floor_debris_quad(bounds, floor_y, chip);
    let opts = options
        .with_depth_policy(DepthPolicy::Average)
        .with_cull_mode(CullMode::None)
        .with_material_layer(material)
        .with_textured_triangle_splitting(true)
        .with_textured_triangle_max_edge(0);
    if BOX_PROP_GTE_PROJECT_ENABLED {
        if let Some(projected) = projector.project_world_quad(quad) {
            let colors = [
                lighting.apply_vertex_fog_weight(
                    box_prop_face_color_at(prop, face, chip.u0_q8, chip.v0_q8),
                    lighting.fog_weight_at_depth(projected[0].sz),
                ),
                lighting.apply_vertex_fog_weight(
                    box_prop_face_color_at(prop, face, chip.u1_q8, chip.v0_q8),
                    lighting.fog_weight_at_depth(projected[1].sz),
                ),
                lighting.apply_vertex_fog_weight(
                    box_prop_face_color_at(prop, face, chip.u1_q8, chip.v1_q8),
                    lighting.fog_weight_at_depth(projected[2].sz),
                ),
                lighting.apply_vertex_fog_weight(
                    box_prop_face_color_at(prop, face, chip.u0_q8, chip.v1_q8),
                    lighting.fog_weight_at_depth(projected[3].sz),
                ),
            ];
            submit_projected_textured_gouraud_quad_u8(
                world, triangles, projected, uvs, colors, material, opts,
            );
            return;
        }
    }
    let colors = [
        lighting.apply_vertex_fog(
            box_prop_face_color_at(prop, face, chip.u0_q8, chip.v0_q8),
            quad[0],
        ),
        lighting.apply_vertex_fog(
            box_prop_face_color_at(prop, face, chip.u1_q8, chip.v0_q8),
            quad[1],
        ),
        lighting.apply_vertex_fog(
            box_prop_face_color_at(prop, face, chip.u1_q8, chip.v1_q8),
            quad[2],
        ),
        lighting.apply_vertex_fog(
            box_prop_face_color_at(prop, face, chip.u0_q8, chip.v1_q8),
            quad[3],
        ),
    ];
    if let Some(projected) = camera.project_world_quad(quad) {
        submit_projected_textured_gouraud_quad_u8(
            world, triangles, projected, uvs, colors, material, opts,
        );
    } else {
        let tint = average_vertex_rgb(colors);
        let material = material.with_tint(tint);
        let opts = opts.with_material_layer(material);
        let _ = world.submit_textured_world_quad(triangles, *camera, quad, uvs, material, opts);
    }
}

pub(super) fn draw_box_prop_break_events<T>(
    events: &[BoxPropBreakEvent; MAX_BOX_PROP_BREAK_EVENTS],
    props: &[LevelBoxPropRecord],
    runtime: &[BoxPropRuntime; MAX_BOX_PROP_STATE],
    current_room: RoomIndex,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<QuadTexturedGouraud>
        + PrimitiveSink<TriTextured>
        + PrimitiveSink<TriTexturedGouraud>,
{
    let mut projector = None;
    for event in events {
        if !event.is_active() || event.age >= BOX_PROP_BREAK_FRAMES {
            continue;
        }
        let Some(prop) = props.get(event.prop_index as usize) else {
            continue;
        };
        if prop.room != current_room {
            continue;
        }
        let Some(box_runtime) = runtime.get(event.prop_index as usize) else {
            continue;
        };
        if !sphere_visible_to_camera(
            camera,
            options,
            box_runtime.cull_center,
            box_runtime.cull_radius.saturating_mul(3),
            128,
        ) {
            continue;
        }
        let loaded_projector = match projector {
            Some(projector) => projector,
            None => {
                let loaded = LoadedWorldCameraGte::load(*camera);
                projector = Some(loaded);
                loaded
            }
        };
        let face_textures = box_prop_face_textures(prop);
        draw_box_prop_break_shards(
            &face_textures,
            &box_runtime.break_shards,
            *event,
            loaded_projector,
            camera,
            options,
            lighting,
            triangles,
            world,
        );
    }
}

fn draw_box_prop_break_shards<T>(
    face_textures: &[Option<BoxPropFaceTextureRuntime>; psx_level::BOX_PROP_FACE_COUNT],
    shard_runtimes: &[BoxPropBreakShardRuntime; BOX_PROP_BREAK_SHARD_COUNT],
    event: BoxPropBreakEvent,
    projector: LoadedWorldCameraGte,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<QuadTexturedGouraud>
        + PrimitiveSink<TriTextured>
        + PrimitiveSink<TriTexturedGouraud>,
{
    for (shard_index, shard) in BOX_PROP_BREAK_SHARDS.iter().copied().enumerate() {
        if event.age < shard.delay {
            continue;
        }
        let shard_runtime = shard_runtimes[shard_index];
        let face = shard_runtime.face as usize;
        if face >= psx_level::BOX_PROP_FACE_COUNT {
            continue;
        }
        draw_box_prop_break_shard(
            face_textures[face],
            shard_runtime,
            event,
            shard,
            shard_index,
            projector,
            camera,
            options,
            lighting,
            triangles,
            world,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_box_prop_break_shard<T>(
    face_texture: Option<BoxPropFaceTextureRuntime>,
    shard_runtime: BoxPropBreakShardRuntime,
    event: BoxPropBreakEvent,
    shard: BoxPropBreakShard,
    shard_index: usize,
    projector: LoadedWorldCameraGte,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<QuadTexturedGouraud>
        + PrimitiveSink<TriTextured>
        + PrimitiveSink<TriTexturedGouraud>,
{
    let Some(face_texture) = face_texture else {
        return;
    };

    let material = face_texture.material;
    let uvs = box_prop_shard_uvs(face_texture.u_max, face_texture.v_max, shard);
    let quad = box_prop_break_shard_quad(shard_runtime, event, shard, shard_index);
    let opts = options
        .with_depth_policy(DepthPolicy::Average)
        .with_cull_mode(CullMode::None)
        .with_material_layer(material)
        .with_textured_triangle_splitting(true)
        .with_textured_triangle_max_edge(0);
    if BOX_PROP_GTE_PROJECT_ENABLED {
        if let Some(projected) = projector.project_world_quad(quad) {
            let fog_weight = lighting.fog_weight_at_depth(average4_i32(
                projected[0].sz,
                projected[1].sz,
                projected[2].sz,
                projected[3].sz,
            ));
            let colors = box_prop_apply_fog_weight(lighting, shard_runtime.colors, fog_weight);
            submit_projected_textured_gouraud_quad_u8(
                world, triangles, projected, uvs, colors, material, opts,
            );
            return;
        }
    }
    let center = box_prop_quad_center(quad);
    let fog_weight = lighting.fog_weight_at_depth(camera.view_vertex(center).z);
    let colors = box_prop_apply_fog_weight(lighting, shard_runtime.colors, fog_weight);
    if let Some(projected) = camera.project_world_quad(quad) {
        submit_projected_textured_gouraud_quad_u8(
            world, triangles, projected, uvs, colors, material, opts,
        );
    } else {
        let tint = average_vertex_rgb(colors);
        let material = material.with_tint(tint);
        let opts = opts.with_material_layer(material);
        let _ = world.submit_textured_world_quad(triangles, *camera, quad, uvs, material, opts);
    }
}

fn submit_projected_textured_gouraud_quad_u8<T>(
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
    triangles: &mut T,
    projected: [ProjectedVertex; 4],
    uvs: [(u8, u8); 4],
    colors: [(u8, u8, u8); 4],
    material: TextureMaterial,
    options: WorldSurfaceOptions,
) where
    T: PrimitiveSink<QuadTexturedGouraud> + PrimitiveSink<TriTexturedGouraud>,
{
    let _ = world.submit_textured_gouraud_quad_prescreened_u8(
        triangles, projected, uvs, colors, material, options,
    );
}

fn box_prop_apply_fog_weight(
    lighting: &RuntimeRoomLighting,
    colors: [(u8, u8, u8); 4],
    fog_weight: i32,
) -> [(u8, u8, u8); 4] {
    [
        lighting.apply_vertex_fog_weight(colors[0], fog_weight),
        lighting.apply_vertex_fog_weight(colors[1], fog_weight),
        lighting.apply_vertex_fog_weight(colors[2], fog_weight),
        lighting.apply_vertex_fog_weight(colors[3], fog_weight),
    ]
}

fn draw_box_prop_faces<T>(
    prop: &LevelBoxPropRecord,
    faces: &[BoxPropFaceRuntime; psx_level::BOX_PROP_FACE_COUNT],
    fall_y: i32,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<TriTextured>
        + PrimitiveSink<TriTexturedGouraud>
        + PrimitiveSink<QuadTexturedGouraud>,
{
    for face in 0..psx_level::BOX_PROP_FACE_COUNT {
        let face_runtime = faces[face];
        if !box_prop_face_front_facing(camera, face_runtime) {
            continue;
        }
        // A uniform Y shift while the box falls; facing is unchanged so the
        // front-facing test above still uses the resting normal/center.
        let face_vertices = box_prop_offset_quad_y(face_runtime.vertices, fall_y);
        let Some(texture_asset) = prop.texture_assets[face] else {
            continue;
        };
        let Some(slot) = prop_texture_slot(texture_asset) else {
            continue;
        };
        let material = TextureMaterial::opaque(slot.clut_word, slot.tpage_word, (0x80, 0x80, 0x80))
            .with_texture_window(slot.texture_window);
        let u_max = model_render_uv_max(slot.texture_width);
        let v_max = model_render_uv_max(slot.texture_height);
        let uvs = [(0, 0), (u_max, 0), (u_max, v_max), (0, v_max)];
        let colors = [
            lighting.apply_vertex_fog(prop.baked_vertex_rgb[face][0], face_vertices[0]),
            lighting.apply_vertex_fog(prop.baked_vertex_rgb[face][1], face_vertices[1]),
            lighting.apply_vertex_fog(prop.baked_vertex_rgb[face][2], face_vertices[2]),
            lighting.apply_vertex_fog(prop.baked_vertex_rgb[face][3], face_vertices[3]),
        ];
        let opts = options
            .with_depth_policy(DepthPolicy::Average)
            .with_cull_mode(CullMode::None)
            .with_material_layer(material)
            .with_textured_triangle_splitting(true)
            .with_textured_triangle_max_edge(0);
        if let Some(projected) = camera.project_world_quad(face_vertices) {
            submit_projected_textured_gouraud_quad_u8(
                world, triangles, projected, uvs, colors, material, opts,
            );
        } else {
            let tint = average_vertex_rgb(colors);
            let material = TextureMaterial::opaque(slot.clut_word, slot.tpage_word, tint)
                .with_texture_window(slot.texture_window);
            let opts = opts.with_material_layer(material);
            let _ = world.submit_textured_world_quad(
                triangles,
                *camera,
                face_vertices,
                uvs,
                material,
                opts,
            );
        }
    }
}

fn box_prop_face_front_facing(camera: &WorldCamera, face: BoxPropFaceRuntime) -> bool {
    let [nx, ny, nz] = face.normal;
    let center = face.center;
    let vx = camera.position.x.saturating_sub(center.x);
    let vy = camera.position.y.saturating_sub(center.y);
    let vz = camera.position.z.saturating_sub(center.z);
    nx.saturating_mul(vx)
        .saturating_add(ny.saturating_mul(vy))
        .saturating_add(nz.saturating_mul(vz))
        > 0
}

fn box_prop_break_shard_quad(
    shard_runtime: BoxPropBreakShardRuntime,
    event: BoxPropBreakEvent,
    shard: BoxPropBreakShard,
    shard_index: usize,
) -> [WorldVertex; 4] {
    let age = event
        .age
        .saturating_sub(shard.delay)
        .min(BOX_PROP_BREAK_MOTION_FRAMES) as i32;
    let mut quad = shard_runtime.base_quad;
    let shard_center = shard_runtime.center;
    let edge_u = shard_runtime.edge_u;
    let edge_v = shard_runtime.edge_v;
    let spin_q12 = box_prop_break_shard_spin_q12(event.prop_index, shard_index, age);
    let outward_q8 = age.saturating_mul(age);
    let drift_q8 = (shard.drift_q8_per_frame as i32)
        .saturating_mul(age)
        .clamp(-96, 96);
    let twist_q8 = (shard.twist_q8_per_frame as i32)
        .saturating_mul(age)
        .clamp(-96, 96);
    let shrink_q8 = (252 - age.saturating_mul(3)).max(176);
    let impulse_units = age.saturating_mul(shard.impulse_per_frame as i32);
    let fall = age.saturating_mul(age).saturating_mul(4);
    let drift = scale_world_delta_q8(edge_u, drift_q8);
    let offset = [
        scale_q8_i32_signed(shard_runtime.face_delta[0], outward_q8)
            .saturating_add((event.impulse_x_q8 as i32).saturating_mul(impulse_units) / 256)
            .saturating_add(drift[0]),
        scale_q8_i32_signed(shard_runtime.face_delta[1], outward_q8)
            .saturating_add((shard.lift_per_frame as i32).saturating_mul(age))
            .saturating_sub(fall)
            .saturating_add(drift[1]),
        scale_q8_i32_signed(shard_runtime.face_delta[2], outward_q8)
            .saturating_add((event.impulse_z_q8 as i32).saturating_mul(impulse_units) / 256)
            .saturating_add(drift[2]),
    ];

    for (corner, vertex) in quad.iter_mut().enumerate() {
        let mut p = shrink_world_vertex_around(*vertex, shard_center, shrink_q8);
        let sign_u = if corner == 0 || corner == 3 { -1 } else { 1 };
        let sign_v = if corner == 0 || corner == 1 { -1 } else { 1 };
        let tumble_u = scale_world_delta_q8(edge_u, sign_v * twist_q8 / 2);
        let tumble_v = scale_world_delta_q8(edge_v, -sign_u * twist_q8);
        p = add_world_vertex_offset(p, tumble_u);
        p = add_world_vertex_offset(p, tumble_v);
        p = rotate_world_vertex_y_around_q12(p, shard_center, spin_q12);
        let landed = add_world_vertex_offset(p, offset);
        // Shift by the box's landed fall offset (0 for an in-place break),
        // then keep the fragment from sinking below the room floor: for an
        // elevated or fallen box this settles it on the ground rather than
        // the box's own (elevated) bottom.
        let y = landed.y.saturating_add(event.y_offset).max(event.ground_y);
        *vertex = WorldVertex::new(landed.x, y, landed.z);
    }
    quad
}

fn box_prop_break_shard_spin_q12(prop_index: u16, shard_index: usize, age: i32) -> u16 {
    let seed = (prop_index as u32)
        .wrapping_mul(73)
        .wrapping_add((shard_index as u32).wrapping_mul(151))
        .wrapping_add(0x4d3);
    let speed = 4 + (seed & 0x0f) as i32;
    let wobble = (((seed >> 5) & 0x07) as i32).saturating_sub(3);
    let signed = age.saturating_mul(speed.saturating_add(wobble).max(2));
    let spin = if seed & 0x10 == 0 { signed } else { -signed };
    spin.rem_euclid(4096) as u16
}

fn rotate_world_vertex_y_around_q12(
    vertex: WorldVertex,
    center: WorldVertex,
    angle_q12: u16,
) -> WorldVertex {
    if angle_q12 == 0 {
        return vertex;
    }
    let relative = [
        vertex.x.saturating_sub(center.x),
        vertex.y.saturating_sub(center.y),
        vertex.z.saturating_sub(center.z),
    ];
    let rotated = rotate_y_q12(relative, angle_q12);
    WorldVertex::new(
        center.x.saturating_add(rotated[0]),
        center.y.saturating_add(rotated[1]),
        center.z.saturating_add(rotated[2]),
    )
}

fn box_prop_floor_debris_quad(
    bounds: BoxPropDebrisBounds,
    floor_y: i32,
    chip: BoxPropFloorDebrisChip,
) -> [WorldVertex; 4] {
    let base = bounds.span_x.max(bounds.span_z).max(128);
    let half_length = (base.saturating_mul(chip.half_length_q8 as i32) >> 8).clamp(32, base);
    let half_width = (base.saturating_mul(chip.half_width_q8 as i32) >> 8).clamp(16, base);
    let center_x = bounds
        .center_x
        .saturating_add(bounds.span_x.saturating_mul(chip.offset_x_q8 as i32) / 256);
    let center_z = bounds
        .center_z
        .saturating_add(bounds.span_z.saturating_mul(chip.offset_z_q8 as i32) / 256);
    let long = rotate_y_q12([half_length, 0, 0], chip.yaw_q12);
    let short = rotate_y_q12([0, 0, half_width], chip.yaw_q12);
    let y = floor_y.saturating_add(chip.lift as i32);
    [
        WorldVertex::new(
            center_x - long[0] - short[0],
            y,
            center_z - long[2] - short[2],
        ),
        WorldVertex::new(
            center_x + long[0] - short[0],
            y,
            center_z + long[2] - short[2],
        ),
        WorldVertex::new(
            center_x + long[0] + short[0],
            y,
            center_z + long[2] + short[2],
        ),
        WorldVertex::new(
            center_x - long[0] + short[0],
            y,
            center_z - long[2] + short[2],
        ),
    ]
}

fn box_prop_floor_debris_uvs(u_max: u8, v_max: u8, chip: BoxPropFloorDebrisChip) -> [(u8, u8); 4] {
    let u0 = uv_from_q8(u_max, chip.u0_q8);
    let u1 = uv_from_q8(u_max, chip.u1_q8);
    let v0 = uv_from_q8(v_max, chip.v0_q8);
    let v1 = uv_from_q8(v_max, chip.v1_q8);
    [(u0, v0), (u1, v0), (u1, v1), (u0, v1)]
}

fn box_prop_shard_uvs(u_max: u8, v_max: u8, shard: BoxPropBreakShard) -> [(u8, u8); 4] {
    let u0 = uv_from_q8(u_max, shard.u0_q8);
    let u1 = uv_from_q8(u_max, shard.u1_q8);
    let v0 = uv_from_q8(v_max, shard.v0_q8);
    let v1 = uv_from_q8(v_max, shard.v1_q8);
    [(u0, v0), (u1, v0), (u1, v1), (u0, v1)]
}
