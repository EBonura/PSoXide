//! Particle-emitter and room-atmosphere overlay policy, carved out of
//! `editor-playtest`'s `particle_runtime` module (phase 2 of
//! docs/game-runtime-plan.md). Cooked emitter records arrive as
//! psx-level values, the OT depth arrives as a `const N` generic, and
//! the screen extents arrive as plain values.

use psx_engine::{
    DepthRange, LoadedWorldCameraGte, OtFrame, PrimitivePacketArena, PrimitiveSink,
    ProjectedVertex, SimTick, WorldCamera, WorldVertex,
};
use psx_gpu::{
    draw_tri_flat_blended,
    material::{BlendMode, TextureMaterial},
    prim::QuadTexturedMaterial,
};
use psx_level::{particle_emitter_flags, room_flags, LevelRoomRecord, ParticleEmitterRecord};
use psx_math::int32::clamp_i16;

/// Particle decals use the U=0 half of the shared shadow/particle 4bpp page;
/// the placement and generated-texture sizing are the crate vram module's
/// contract.
use crate::vram::{PARTICLE_TEXEL_U, PARTICLE_TEXTURE_SIZE};
const PARTICLE_TEXEL_V: u8 = 0;
const PARTICLE_UV_MAX: u8 = PARTICLE_TEXEL_U + PARTICLE_TEXTURE_SIZE as u8 - 1;

const ATMOSPHERE_PARTICLE_MAX: u32 = 96;
const ATMOSPHERE_SCREEN_MARGIN: i32 = 24;
const PARTICLE_EMITTER_DRAW_CAP: u16 = 64;
const PARTICLE_MIN_SCREEN_SIZE: i16 = 2;
const PARTICLE_MAX_SCREEN_SIZE: i16 = 18;
const WATER_SPLASH_PARTICLES: u32 = 3;
const WATER_SPLASH_LIFETIME: u32 = 16;

/// Draw one authored particle emitter's steady-state population as
/// camera-facing textured quads. Returns the submitted quad count.
pub fn draw_particle_emitter<const OT_DEPTH: usize>(
    emitter: ParticleEmitterRecord,
    camera: WorldCamera,
    projector: Option<LoadedWorldCameraGte>,
    depth_range: DepthRange,
    particle_material: TextureMaterial,
    elapsed_tick: SimTick,
    ot: &mut OtFrame<'_, OT_DEPTH>,
    primitive_packets: &mut PrimitivePacketArena<'_>,
) -> usize {
    if emitter.flags & particle_emitter_flags::ENABLED == 0
        || emitter.max_particles == 0
        || emitter.lifetime_frames == 0
        || emitter.spawn_rate_q8 == 0
    {
        return 0;
    }

    let lifetime = emitter.lifetime_frames as u32;
    let steady_count = ((emitter.spawn_rate_q8 as u32)
        .saturating_mul(lifetime)
        .saturating_add(60 * 256 - 1))
        / (60 * 256);
    let count = (emitter.max_particles as u32)
        .min(PARTICLE_EMITTER_DRAW_CAP as u32)
        .min(steady_count.max(1));
    if count == 0 {
        return 0;
    }

    let mut submitted = 0usize;
    let mut i = 0u32;
    while i < count {
        let seed = particle_seed(
            emitter.room.to_usize() as u32,
            emitter.x as u32,
            emitter.z as u32,
            i,
        );
        let age = (elapsed_tick.as_u32() + (i * lifetime / count)) % lifetime;
        submitted += draw_particle_sample(
            emitter,
            camera,
            projector,
            depth_range,
            particle_material,
            seed,
            age as i32,
            lifetime as i32,
            ot,
            primitive_packets,
        );
        i += 1;
    }
    submitted
}

/// Draw a tiny fixed-budget splash around a moving actor's feet. This is a
/// purely visual three-sprite effect: it owns no emitter state, performs no
/// collision queries, and derives its phase from the gameplay tick.
pub fn draw_water_wade_splash<const OT_DEPTH: usize>(
    x: i32,
    surface_y: i32,
    z: i32,
    camera: WorldCamera,
    projector: Option<LoadedWorldCameraGte>,
    depth_range: DepthRange,
    particle_material: TextureMaterial,
    elapsed_tick: SimTick,
    ot: &mut OtFrame<'_, OT_DEPTH>,
    primitive_packets: &mut PrimitivePacketArena<'_>,
) -> usize {
    let mut submitted = 0usize;
    let mut index = 0u32;
    while index < WATER_SPLASH_PARTICLES {
        let age = (elapsed_tick
            .as_u32()
            .wrapping_add(index.saturating_mul(WATER_SPLASH_LIFETIME / WATER_SPLASH_PARTICLES))
            % WATER_SPLASH_LIFETIME) as i32;
        let spread = 10i32.saturating_add(age.saturating_mul(3));
        let (dx, dz) = match index {
            0 => (-spread, spread / 2),
            1 => (spread, spread / 3),
            _ => (-(spread / 3), -spread),
        };
        // A short parabola reads as droplets kicked away from the feet.
        let rise = age.saturating_mul(16 - age) / 2;
        let position = WorldVertex::new(
            x.saturating_add(dx),
            surface_y.saturating_add(6).saturating_add(rise),
            z.saturating_add(dz),
        );
        let center = if let Some(projector) = projector {
            projector.project_world(position)
        } else {
            camera.project_world(position)
        };
        if let Some(center) = center {
            let world_size = 18i32.saturating_sub(age / 2).max(8);
            let half = ((world_size.saturating_mul(camera.projection.focal_length))
                / center.sz.max(1))
            .clamp(2, 10) as i16;
            let fade = (112i32.saturating_sub(age.saturating_mul(4))).clamp(48, 112) as u8;
            let material = particle_material
                .with_tint((fade / 2, fade, fade.saturating_add(24)))
                .with_blend_mode(BlendMode::AddQuarter);
            submitted += draw_particle_quad(
                center,
                half,
                material,
                depth_range.slot::<OT_DEPTH>(center.sz),
                ot,
                primitive_packets,
            );
        }
        index += 1;
    }
    submitted
}

fn draw_particle_sample<const OT_DEPTH: usize>(
    emitter: ParticleEmitterRecord,
    camera: WorldCamera,
    projector: Option<LoadedWorldCameraGte>,
    depth_range: DepthRange,
    particle_material: TextureMaterial,
    seed: u32,
    age: i32,
    lifetime: i32,
    ot: &mut OtFrame<'_, OT_DEPTH>,
    primitive_packets: &mut PrimitivePacketArena<'_>,
) -> usize {
    let spawn_radius = emitter.spawn_radius as i32;
    let origin_x = emitter
        .x
        .saturating_add(particle_signed_spread(seed, spawn_radius));
    let origin_y = emitter.y.saturating_add(particle_signed_spread(
        seed.rotate_left(9),
        spawn_radius >> 1,
    ));
    let origin_z = emitter
        .z
        .saturating_add(particle_signed_spread(seed.rotate_left(17), spawn_radius));
    let x = particle_axis_position(
        origin_x,
        emitter.base_velocity_q4[0],
        emitter.random_velocity_q4[0],
        emitter.acceleration_q4[0],
        age,
        seed.rotate_left(3),
    );
    let y = particle_axis_position(
        origin_y,
        emitter.base_velocity_q4[1],
        emitter.random_velocity_q4[1],
        emitter.acceleration_q4[1],
        age,
        seed.rotate_left(11),
    );
    let z = particle_axis_position(
        origin_z,
        emitter.base_velocity_q4[2],
        emitter.random_velocity_q4[2],
        emitter.acceleration_q4[2],
        age,
        seed.rotate_left(21),
    );
    let position = WorldVertex::new(x, y, z);
    let center = if let Some(projector) = projector {
        projector.project_world(position)
    } else {
        camera.project_world(position)
    };
    let Some(center) = center else {
        return 0;
    };

    let t_q8 = if lifetime <= 1 {
        255
    } else {
        ((age * 255) / (lifetime - 1)).clamp(0, 255)
    };
    let size = particle_lerp_u16(emitter.start_size, emitter.end_size, t_q8);
    let half = ((i32::from(size) * camera.projection.focal_length) / center.sz.max(1)).clamp(
        i32::from(PARTICLE_MIN_SCREEN_SIZE),
        i32::from(PARTICLE_MAX_SCREEN_SIZE),
    ) as i16;
    let tint = particle_lerp_rgb(emitter.start_color, emitter.end_color, t_q8);
    let blend = particle_blend_mode(emitter.blend_mode);
    let slot = depth_range.slot::<OT_DEPTH>(center.sz);
    draw_particle_quad(
        center,
        half,
        particle_material.with_tint(tint).with_blend_mode(blend),
        slot,
        ot,
        primitive_packets,
    )
}

fn draw_particle_quad<const OT_DEPTH: usize>(
    center: ProjectedVertex,
    half: i16,
    material: TextureMaterial,
    slot: psx_engine::DepthSlot,
    ot: &mut OtFrame<'_, OT_DEPTH>,
    primitive_packets: &mut PrimitivePacketArena<'_>,
) -> usize {
    let left = clamp_i16(i32::from(center.sx).saturating_sub(i32::from(half)));
    let right = clamp_i16(i32::from(center.sx).saturating_add(i32::from(half)));
    let top = clamp_i16(i32::from(center.sy).saturating_sub(i32::from(half)));
    let bottom = clamp_i16(i32::from(center.sy).saturating_add(i32::from(half)));
    if left == right || top == bottom {
        return 0;
    }
    let quad = QuadTexturedMaterial::with_material(
        [(left, top), (right, top), (left, bottom), (right, bottom)],
        [
            (PARTICLE_TEXEL_U, PARTICLE_TEXEL_V),
            (PARTICLE_UV_MAX, PARTICLE_TEXEL_V),
            (PARTICLE_TEXEL_U, PARTICLE_UV_MAX),
            (PARTICLE_UV_MAX, PARTICLE_UV_MAX),
        ],
        material,
    );
    let Some(packet) = primitive_packets.push(quad) else {
        return 0;
    };
    ot.add_packet_slot(slot, packet);
    1
}

fn particle_axis_position(
    origin: i32,
    base_velocity_q4: i16,
    random_velocity_q4: u16,
    acceleration_q4: i16,
    age: i32,
    seed: u32,
) -> i32 {
    let random_velocity = particle_signed_spread(seed, random_velocity_q4 as i32);
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

fn particle_seed(room: u32, x: u32, z: u32, index: u32) -> u32 {
    let mut value = room
        .wrapping_mul(0x9E37_79B9)
        .wrapping_add(x.rotate_left(7))
        .wrapping_add(z.rotate_left(17))
        .wrapping_add(index.wrapping_mul(0x85EB_CA6B));
    value ^= value >> 16;
    value = value.wrapping_mul(0x7FEB_352D);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846C_A68B);
    value ^ (value >> 16)
}

fn particle_signed_spread(seed: u32, spread: i32) -> i32 {
    if spread <= 0 {
        return 0;
    }
    let span = spread.saturating_mul(2).saturating_add(1) as u32;
    (seed % span) as i32 - spread
}

fn particle_lerp_u16(a: u16, b: u16, t_q8: i32) -> u16 {
    let inv = 255 - t_q8;
    (((i32::from(a) * inv) + (i32::from(b) * t_q8)) / 255).clamp(0, u16::MAX as i32) as u16
}

fn particle_lerp_rgb(a: [u8; 3], b: [u8; 3], t_q8: i32) -> (u8, u8, u8) {
    (
        particle_lerp_u8(a[0], b[0], t_q8),
        particle_lerp_u8(a[1], b[1], t_q8),
        particle_lerp_u8(a[2], b[2], t_q8),
    )
}

fn particle_lerp_u8(a: u8, b: u8, t_q8: i32) -> u8 {
    let inv = 255 - t_q8;
    (((i32::from(a) * inv) + (i32::from(b) * t_q8)) / 255).clamp(0, 255) as u8
}

const fn particle_blend_mode(mode: u8) -> BlendMode {
    match mode & 3 {
        1 => BlendMode::Add,
        2 => BlendMode::Subtract,
        3 => BlendMode::AddQuarter,
        _ => BlendMode::Average,
    }
}

/// Draw the room's screen-space atmosphere particles (drifting motes)
/// as immediate-mode flat triangles over the presented frame.
pub fn draw_room_atmosphere_overlay(
    room: &LevelRoomRecord,
    elapsed_tick: SimTick,
    screen_w: i16,
    screen_h: i16,
) {
    if room.flags & room_flags::ATMOSPHERE_ENABLED == 0 {
        return;
    }
    let count = (room.atmosphere_density as u32).min(ATMOSPHERE_PARTICLE_MAX);
    if count == 0 {
        return;
    }
    let wrap_w = screen_w as i32 + ATMOSPHERE_SCREEN_MARGIN * 2;
    let wrap_h = screen_h as i32 + ATMOSPHERE_SCREEN_MARGIN * 2;
    let base_fall_q4 = room.atmosphere_fall_speed_q4.max(0) as i32;
    let base_wind_q4 = room.atmosphere_wind_speed_q4 as i32;
    let elapsed_vblanks = elapsed_tick.as_u32();
    let elapsed = elapsed_vblanks as i32;
    let mut i = 0u32;
    while i < count {
        let seed = atmosphere_seed(i);
        let layer = ((seed >> 4) & 3) as u32;
        let fall_q4 = base_fall_q4 + (layer as i32) * 3;
        let wind_q4 = base_wind_q4 + layer as i32;
        let base_x = (seed & 0x1ff) as i32;
        let base_y = ((seed >> 9) & 0x1ff) as i32;
        let drift_phase = ((elapsed_vblanks >> (2 + layer)) as i32 + ((seed >> 18) as i32)) & 31;
        let drift = drift_phase - 16;
        let x = wrap_atmosphere_axis(
            base_x + (elapsed.wrapping_mul(wind_q4) >> 4) + drift,
            wrap_w,
        );
        let y = wrap_atmosphere_axis(base_y + (elapsed.wrapping_mul(fall_q4) >> 4), wrap_h);
        let size = 1 + ((layer as i16) >> 1);
        draw_atmosphere_particle(
            x,
            y,
            size,
            atmosphere_particle_tint(room.atmosphere_rgb, layer, seed),
        );
        i += 1;
    }
}

fn draw_atmosphere_particle(x: i16, y: i16, size: i16, tint: (u8, u8, u8)) {
    let lean = size + 1;
    draw_tri_flat_blended(
        [(x, y), (x + lean, y + 1), (x, y + size + 1)],
        tint.0,
        tint.1,
        tint.2,
        BlendMode::Average,
    );
}

fn atmosphere_particle_tint(base: [u8; 3], layer: u32, seed: u32) -> (u8, u8, u8) {
    let lift = ((layer * 10) + ((seed >> 22) & 7)) as i16;
    (
        tint_channel(base[0], lift),
        tint_channel(base[1], lift),
        tint_channel(base[2], lift),
    )
}

fn tint_channel(value: u8, delta: i16) -> u8 {
    (value as i16 + delta).clamp(0, 255) as u8
}

fn wrap_atmosphere_axis(value: i32, span: i32) -> i16 {
    (value.rem_euclid(span) - ATMOSPHERE_SCREEN_MARGIN) as i16
}

fn atmosphere_seed(index: u32) -> u32 {
    let mut x = index.wrapping_mul(0x9e37_79b9).wrapping_add(0x7f4a_7c15);
    x ^= x >> 16;
    x = x.wrapping_mul(0x85eb_ca6b);
    x ^ (x >> 13)
}
