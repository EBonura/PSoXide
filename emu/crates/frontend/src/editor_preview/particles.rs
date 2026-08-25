//! Particle-emitter preview submission for BSP editor scenes.

use super::*;

pub(crate) fn walk_bsp_particle_emitters(
    project: &ProjectDocument,
    hidden_scene_nodes: &HashSet<NodeId>,
    particle_slot: MaterialSlot,
    preview_tick: u32,
    scratch: &mut PreviewScratch,
) {
    let scene = project.active_scene();
    for node in scene.nodes() {
        if scene_node_hidden(scene, hidden_scene_nodes, node.id) {
            continue;
        }
        let NodeKind::ParticleEmitter { settings } = &node.kind else {
            continue;
        };
        let [x, y, z] = node.transform.translation;
        push_particle_emitter_preview(
            settings,
            psx_engine::WorldVertex::new(x.round() as i32, y.round() as i32, z.round() as i32),
            particle_slot,
            preview_tick,
            scratch,
        );
    }
}

pub(crate) fn push_particle_emitter_preview(
    settings: &psxed_project::ParticleEmitterSettings,
    origin: psx_engine::WorldVertex,
    particle_slot: MaterialSlot,
    preview_tick: u32,
    scratch: &mut PreviewScratch,
) {
    if !settings.enabled
        || settings.max_particles == 0
        || settings.lifetime_frames == 0
        || settings.spawn_rate_q8 == 0
    {
        return;
    }
    let lifetime = settings.lifetime_frames as u32;
    let steady_count = ((settings.spawn_rate_q8 as u32)
        .saturating_mul(lifetime)
        .saturating_add(60 * 256 - 1))
        / (60 * 256);
    let count = (settings.max_particles as u32)
        .min(PREVIEW_PARTICLE_DRAW_CAP as u32)
        .min(steady_count.max(1));
    for i in 0..count {
        let seed = preview_particle_seed(origin.x as u32, origin.z as u32, i);
        let age = (preview_tick + (i * lifetime / count)) % lifetime;
        push_particle_preview_sample(
            settings,
            origin,
            particle_slot,
            seed,
            age as i32,
            lifetime as i32,
            scratch,
        );
    }
}

fn push_particle_preview_sample(
    settings: &psxed_project::ParticleEmitterSettings,
    origin: psx_engine::WorldVertex,
    particle_slot: MaterialSlot,
    seed: u32,
    age: i32,
    lifetime: i32,
    scratch: &mut PreviewScratch,
) {
    let spawn_radius = settings.spawn_radius as i32;
    let origin_x = origin
        .x
        .saturating_add(preview_particle_signed_spread(seed, spawn_radius));
    let origin_y = origin.y.saturating_add(preview_particle_signed_spread(
        seed.rotate_left(9),
        spawn_radius / 2,
    ));
    let origin_z = origin.z.saturating_add(preview_particle_signed_spread(
        seed.rotate_left(17),
        spawn_radius,
    ));
    let x = preview_particle_axis_position(
        origin_x,
        settings.base_velocity_q4[0],
        settings.random_velocity_q4[0],
        settings.acceleration_q4[0],
        age,
        seed.rotate_left(3),
    );
    let y = preview_particle_axis_position(
        origin_y,
        settings.base_velocity_q4[1],
        settings.random_velocity_q4[1],
        settings.acceleration_q4[1],
        age,
        seed.rotate_left(11),
    );
    let z = preview_particle_axis_position(
        origin_z,
        settings.base_velocity_q4[2],
        settings.random_velocity_q4[2],
        settings.acceleration_q4[2],
        age,
        seed.rotate_left(21),
    );
    let projected = gte_scene::project_vertex(world_to_view([x, y, z]));
    if projected.sz == 0 {
        return;
    }
    let t_q8 = if lifetime <= 1 {
        255
    } else {
        ((age * 255) / (lifetime - 1)).clamp(0, 255)
    };
    let size = preview_particle_lerp_u16(settings.start_size, settings.end_size, t_q8);
    let half = ((i32::from(size) * PROJ_H) / i32::from(projected.sz.max(1))).clamp(
        i32::from(PREVIEW_PARTICLE_MIN_SCREEN_SIZE),
        i32::from(PREVIEW_PARTICLE_MAX_SCREEN_SIZE),
    ) as i16;
    let tint = preview_particle_lerp_rgb(settings.start_color, settings.end_color, t_q8);
    push_particle_preview_quad(
        projected,
        half,
        particle_slot,
        tint,
        psx_blend_mode(settings.blend_mode),
        scratch,
    );
}

fn push_particle_preview_quad(
    center: psx_gte::scene::Projected,
    half: i16,
    particle_slot: MaterialSlot,
    tint: (u8, u8, u8),
    blend_mode: BlendMode,
    scratch: &mut PreviewScratch,
) {
    if scratch.particle_used >= PREVIEW_PARTICLE_DRAW_CAP {
        return;
    }
    let left = (i32::from(center.sx).saturating_sub(i32::from(half)))
        .clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    let right = (i32::from(center.sx).saturating_add(i32::from(half)))
        .clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    let top = (i32::from(center.sy).saturating_sub(i32::from(half)))
        .clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    let bottom = (i32::from(center.sy).saturating_add(i32::from(half)))
        .clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    if left == right || top == bottom {
        return;
    }
    let u0 = PREVIEW_PARTICLE_TEXEL_U;
    let v0 = PREVIEW_PARTICLE_TEXEL_V;
    let u1 = u0 + particle_slot.texture_width.saturating_sub(1);
    let v1 = v0 + particle_slot.texture_height.saturating_sub(1);
    let material = preview_texture_material(particle_slot, tint, blend_mode);
    let idx = scratch.particle_used;
    scratch.particle_quads[idx] = QuadTexturedMaterial::with_material(
        [(left, top), (right, top), (left, bottom), (right, bottom)],
        [(u0, v0), (u1, v0), (u0, v1), (u1, v1)],
        material,
    );
    scratch.particle_used = idx + 1;
    let packet_ptr: *mut QuadTexturedMaterial = &mut scratch.particle_quads[idx];
    unsafe {
        scratch.ot.insert(
            room_depth_slot(center.sz as u32),
            packet_ptr.cast::<u32>(),
            QuadTexturedMaterial::WORDS,
        );
    }
}

fn preview_particle_axis_position(
    origin: i32,
    base_velocity_q4: i16,
    random_velocity_q4: u16,
    acceleration_q4: i16,
    age: i32,
    seed: u32,
) -> i32 {
    let random_velocity = preview_particle_signed_spread(seed, random_velocity_q4 as i32);
    let velocity = i32::from(base_velocity_q4).saturating_add(random_velocity);
    let velocity_term = velocity.saturating_mul(age) >> 4;
    let acceleration_term = i32::from(acceleration_q4)
        .saturating_mul(age)
        .saturating_mul(age)
        >> 5;
    origin
        .saturating_add(velocity_term)
        .saturating_add(acceleration_term)
}

fn preview_particle_seed(x: u32, z: u32, index: u32) -> u32 {
    let mut value = x
        .rotate_left(7)
        .wrapping_add(z.rotate_left(17))
        .wrapping_add(index.wrapping_mul(0x85EB_CA6B));
    value ^= value >> 16;
    value = value.wrapping_mul(0x7FEB_352D);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846C_A68B);
    value ^ (value >> 16)
}

fn preview_particle_signed_spread(seed: u32, spread: i32) -> i32 {
    if spread <= 0 {
        return 0;
    }
    let span = spread.saturating_mul(2).saturating_add(1) as u32;
    (seed % span) as i32 - spread
}

fn preview_particle_lerp_u16(a: u16, b: u16, t_q8: i32) -> u16 {
    let inv = 255 - t_q8;
    (((i32::from(a) * inv) + (i32::from(b) * t_q8)) / 255).clamp(0, u16::MAX as i32) as u16
}

fn preview_particle_lerp_rgb(a: [u8; 3], b: [u8; 3], t_q8: i32) -> (u8, u8, u8) {
    (
        preview_particle_lerp_u8(a[0], b[0], t_q8),
        preview_particle_lerp_u8(a[1], b[1], t_q8),
        preview_particle_lerp_u8(a[2], b[2], t_q8),
    )
}

fn preview_particle_lerp_u8(a: u8, b: u8, t_q8: i32) -> u8 {
    let inv = 255 - t_q8;
    (((i32::from(a) * inv) + (i32::from(b) * t_q8)) / 255).clamp(0, 255) as u8
}
