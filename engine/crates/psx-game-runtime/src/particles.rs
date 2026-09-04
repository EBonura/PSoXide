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
use crate::{
    combat::AuthoredProjectileCharge,
    projectiles::{ProjectileImpactEffect, ProjectileSnapshot},
};
const PARTICLE_TEXEL_V: u8 = 0;
const PARTICLE_UV_MAX: u8 = PARTICLE_TEXEL_U + PARTICLE_TEXTURE_SIZE as u8 - 1;

const ATMOSPHERE_PARTICLE_MAX: u32 = 96;
const ATMOSPHERE_SCREEN_MARGIN: i32 = 24;
const PARTICLE_EMITTER_DRAW_CAP: u16 = 64;
const PARTICLE_MIN_SCREEN_SIZE: i16 = 2;
const PARTICLE_MAX_SCREEN_SIZE: i16 = 18;
const WATER_SPLASH_PARTICLES: u32 = 3;
const WATER_SPLASH_LIFETIME: u32 = 16;

/// Draw one retained dash sample: three body-height ion filaments or an
/// expanding ground pulse. World projection/depth keeps walls authoritative.
#[allow(clippy::too_many_arguments)]
pub fn draw_dash_sample<const OT_DEPTH: usize>(
    sample: crate::combat_feedback::DashSample,
    camera: WorldCamera,
    projector: Option<LoadedWorldCameraGte>,
    depth_range: DepthRange,
    particle_material: TextureMaterial,
    ot: &mut OtFrame<'_, OT_DEPTH>,
    primitive_packets: &mut PrimitivePacketArena<'_>,
) -> usize {
    let project = |p: [i32; 3]| {
        let vertex = WorldVertex::new(p[0], p[1], p[2]);
        if let Some(projector) = projector { projector.project_world(vertex) }
        else { camera.project_world(vertex) }
    };
    let fade = u16::from(12u8.saturating_sub(sample.age));
    let accent = if sample.zenith { [36, 150, 176] } else { [160, 72, 30] };
    let material = particle_material.with_blend_mode(BlendMode::Add)
        .with_tint(rgb_tuple(scale_rgb(accent, fade, 12)));
    let mut submitted = 0;
    if sample.pulse {
        // An eight-sided floor ring, rather than a camera-facing halo.
        const RING: [(i32, i32); 9] = [(4,0),(3,3),(0,4),(-3,3),(-4,0),(-3,-3),(0,-4),(3,-3),(4,0)];
        let radius = i32::from(sample.height) / 6 + i32::from(sample.age) * 2;
        for edge in RING.windows(2) {
            let p = |v: (i32,i32)| [sample.to[0] + v.0 * radius / 4,
                sample.to[1] + 3, sample.to[2] + v.1 * radius / 4];
            if let (Some(a), Some(b)) = (project(p(edge[0])), project(p(edge[1]))) {
                let slot = depth_range.slot::<OT_DEPTH>((a.sz + b.sz) / 2);
                submitted += draw_projectile_segment(a, b, 1, material, slot, ot, primitive_packets);
            }
        }
    } else {
        for band in [1, 2, 3] {
            let lift = i32::from(sample.height) * band / 5;
            let mut from = sample.from;
            let mut to = sample.to;
            from[1] += lift;
            to[1] += lift;
            if let (Some(a), Some(b)) = (project(from), project(to)) {
                let half = (camera.projection.focal_length / b.sz.max(1)).clamp(1, 2) as i16;
                submitted += draw_projectile_segment(a, b, half, material,
                    depth_range.slot::<OT_DEPTH>((a.sz + b.sz) / 2), ot, primitive_packets);
            }
        }
    }
    submitted
}

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

/// Draw one live combat projectile as a velocity-aligned needle with a wider
/// additive glow, a short tapered ghost trail, and a two-frame muzzle flash.
/// The effect uses the shared particle page and remains fully fixed-budget.
#[allow(clippy::too_many_arguments)]
pub fn draw_projectile_bolt<const OT_DEPTH: usize>(
    projectile: ProjectileSnapshot,
    camera: WorldCamera,
    projector: Option<LoadedWorldCameraGte>,
    depth_range: DepthRange,
    particle_material: TextureMaterial,
    ot: &mut OtFrame<'_, OT_DEPTH>,
    primitive_packets: &mut PrimitivePacketArena<'_>,
) -> usize {
    if projectile.radius == 0 {
        return 0;
    }
    let project = |position: [i32; 3]| {
        let position = WorldVertex::new(position[0], position[1], position[2]);
        if let Some(projector) = projector {
            projector.project_world(position)
        } else {
            camera.project_world(position)
        }
    };
    let Some(head) = project(projectile.position) else {
        return 0;
    };
    let half = ((i32::from(projectile.radius) * camera.projection.focal_length) / head.sz.max(1))
        .clamp(
            i32::from(PARTICLE_MIN_SCREEN_SIZE),
            i32::from(PARTICLE_MAX_SCREEN_SIZE),
        ) as i16;
    let tail_position = projectile_offset(
        projectile.position,
        projectile.velocity,
        -i32::from(projectile.visual.length_ticks.max(1)),
    );
    let Some(tail) = project(tail_position) else {
        return 0;
    };
    let slot = depth_range.slot::<OT_DEPTH>(head.sz.min(tail.sz));
    let glow_half = ((i32::from(half) * i32::from(projectile.visual.glow_scale_q8.max(256))) >> 8)
        .clamp(i32::from(half), i32::from(PARTICLE_MAX_SCREEN_SIZE)) as i16;
    let mut submitted = draw_projectile_segment(
        tail,
        head,
        glow_half,
        particle_material
            .with_tint(rgb_tuple(projectile.visual.glow_rgb))
            .with_blend_mode(BlendMode::Add),
        slot,
        ot,
        primitive_packets,
    );
    submitted += draw_projectile_segment(
        tail,
        head,
        (half / 2).max(1),
        particle_material
            .with_tint(rgb_tuple(projectile.visual.core_rgb))
            .with_blend_mode(BlendMode::Add),
        slot,
        ot,
        primitive_packets,
    );
    let trail_count = projectile.visual.trail_segments.min(6);
    let mut trail = 0u8;
    while trail < trail_count {
        let distance = i32::from(trail.saturating_add(1))
            .saturating_mul(i32::from(projectile.visual.trail_spacing_ticks.max(1)));
        let trail_head_position =
            projectile_offset(projectile.position, projectile.velocity, -distance);
        let trail_tail_position = projectile_offset(
            trail_head_position,
            projectile.velocity,
            -i32::from(projectile.visual.length_ticks.max(1)),
        );
        if let (Some(trail_head), Some(trail_tail)) =
            (project(trail_head_position), project(trail_tail_position))
        {
            let remaining = u16::from(trail_count.saturating_sub(trail));
            let divisor = u16::from(trail_count).saturating_add(1);
            let tint = scale_rgb(projectile.visual.glow_rgb, remaining, divisor);
            let trail_half =
                ((i32::from(half) * i32::from(remaining)) / i32::from(divisor)).max(1) as i16;
            submitted += draw_projectile_segment(
                trail_tail,
                trail_head,
                trail_half,
                particle_material
                    .with_tint(rgb_tuple(tint))
                    .with_blend_mode(BlendMode::Add),
                depth_range.slot::<OT_DEPTH>(trail_head.sz.min(trail_tail.sz)),
                ot,
                primitive_packets,
            );
        }
        trail += 1;
    }
    if projectile.age_ticks <= 2 {
        if let Some(origin) = project(projectile.origin) {
            let flash_half = glow_half.saturating_add(projectile.age_ticks as i16 * 2);
            submitted += draw_particle_diamond(
                origin,
                flash_half,
                particle_material
                    .with_tint(rgb_tuple(projectile.visual.core_rgb))
                    .with_blend_mode(BlendMode::Add),
                depth_range.slot::<OT_DEPTH>(origin.sz),
                ot,
                primitive_packets,
            );
        }
    }
    submitted
}

/// Draw a compact angular charge at an animated projectile muzzle.
#[allow(clippy::too_many_arguments)]
pub fn draw_projectile_charge<const OT_DEPTH: usize>(
    charge: AuthoredProjectileCharge,
    camera: WorldCamera,
    projector: Option<LoadedWorldCameraGte>,
    depth_range: DepthRange,
    particle_material: TextureMaterial,
    ot: &mut OtFrame<'_, OT_DEPTH>,
    primitive_packets: &mut PrimitivePacketArena<'_>,
) -> usize {
    let position = WorldVertex::new(charge.position[0], charge.position[1], charge.position[2]);
    let center = if let Some(projector) = projector {
        projector.project_world(position)
    } else {
        camera.project_world(position)
    };
    let Some(center) = center else {
        return 0;
    };
    let base = ((i32::from(charge.radius) * camera.projection.focal_length) / center.sz.max(1))
        .clamp(2, i32::from(PARTICLE_MAX_SCREEN_SIZE)) as i16;
    let pulse = ((u32::from(charge.progress_q8) * 5) >> 8) as i16;
    let material = particle_material
        .with_tint(rgb_tuple(charge.visual.glow_rgb))
        .with_blend_mode(BlendMode::Add);
    let slot = depth_range.slot::<OT_DEPTH>(center.sz);
    draw_particle_diamond(
        center,
        base.saturating_add(pulse),
        material,
        slot,
        ot,
        primitive_packets,
    ) + draw_particle_diamond(
        center,
        (base / 2).max(1),
        particle_material
            .with_tint(rgb_tuple(charge.visual.core_rgb))
            .with_blend_mode(BlendMode::Add),
        slot,
        ot,
        primitive_packets,
    )
}

/// Draw one expanding angular impact flare from the fixed presentation pool.
#[allow(clippy::too_many_arguments)]
pub fn draw_projectile_impact<const OT_DEPTH: usize>(
    impact: ProjectileImpactEffect,
    camera: WorldCamera,
    projector: Option<LoadedWorldCameraGte>,
    depth_range: DepthRange,
    particle_material: TextureMaterial,
    ot: &mut OtFrame<'_, OT_DEPTH>,
    primitive_packets: &mut PrimitivePacketArena<'_>,
) -> usize {
    let position = WorldVertex::new(impact.position[0], impact.position[1], impact.position[2]);
    let center = if let Some(projector) = projector {
        projector.project_world(position)
    } else {
        camera.project_world(position)
    };
    let Some(center) = center else {
        return 0;
    };
    let lifetime = impact.visual.impact_lifetime_ticks.max(1);
    let age = impact.age_ticks.min(lifetime);
    let base = ((i32::from(impact.radius) * camera.projection.focal_length) / center.sz.max(1))
        .clamp(2, i32::from(PARTICLE_MAX_SCREEN_SIZE)) as i16;
    let expansion = ((i32::from(age) * i32::from(base) * 2) / i32::from(lifetime)).max(1) as i16;
    let fade = u16::from(lifetime.saturating_sub(age)).max(1);
    let slot = depth_range.slot::<OT_DEPTH>(center.sz);
    if impact.visual.break_fragment_count != 0 {
        const FRAGMENT_DIRECTIONS: [(i32, i32); 12] = [
            (-8, -8),
            (-5, -10),
            (-2, -7),
            (2, -9),
            (5, -6),
            (8, -8),
            (-9, -3),
            (-4, -2),
            (4, -3),
            (9, -1),
            (-6, 1),
            (6, 2),
        ];
        let age_i32 = i32::from(age);
        let lifetime_i32 = i32::from(lifetime);
        let base_i32 = i32::from(base);
        let travel = age_i32.saturating_mul(base_i32).saturating_mul(4) / lifetime_i32;
        let gravity = age_i32
            .saturating_mul(age_i32)
            .saturating_mul(base_i32)
            .saturating_mul(4)
            / lifetime_i32.saturating_mul(lifetime_i32);
        let flash_ticks = (lifetime / 5).max(2);
        let mut submitted = 0usize;
        if age < flash_ticks {
            let flash_fade = u16::from(flash_ticks.saturating_sub(age)).max(1);
            let flash_tint = scale_rgb(impact.visual.glow_rgb, flash_fade, u16::from(flash_ticks));
            submitted += draw_particle_diamond(
                center,
                base.saturating_add(expansion),
                particle_material
                    .with_tint(rgb_tuple(flash_tint))
                    .with_blend_mode(BlendMode::Add),
                slot,
                ot,
                primitive_packets,
            );
        }
        let count = usize::from(impact.visual.break_fragment_count).min(FRAGMENT_DIRECTIONS.len());
        let shard_half = ((base_i32.saturating_mul(i32::from(fade)))
            / lifetime_i32.saturating_mul(3))
        .max(1) as i16;
        for (fragment, (velocity_x, velocity_y)) in
            FRAGMENT_DIRECTIONS[..count].iter().copied().enumerate()
        {
            let dx = velocity_x.saturating_mul(travel) / 8;
            let dy = velocity_y.saturating_mul(travel) / 8 + gravity;
            let shard = ProjectedVertex {
                sx: clamp_i16(i32::from(center.sx).saturating_add(dx)),
                sy: clamp_i16(i32::from(center.sy).saturating_add(dy)),
                ..center
            };
            let bright = fragment % 3 == 0;
            let rgb = if bright {
                impact.visual.core_rgb
            } else {
                impact.visual.impact_rgb
            };
            let tint = scale_rgb(rgb, fade, u16::from(lifetime));
            let material = particle_material
                .with_tint(rgb_tuple(tint))
                .with_blend_mode(if bright {
                    BlendMode::AddQuarter
                } else {
                    BlendMode::Average
                });
            submitted +=
                draw_particle_diamond(shard, shard_half, material, slot, ot, primitive_packets);
        }
        return submitted;
    }
    let tint = scale_rgb(impact.visual.impact_rgb, fade, u16::from(lifetime));
    let material = particle_material
        .with_tint(rgb_tuple(tint))
        .with_blend_mode(BlendMode::Add);
    let mut submitted = draw_particle_diamond(
        center,
        base.saturating_add(expansion),
        material,
        slot,
        ot,
        primitive_packets,
    );
    let shard_half = (base / 3).max(1);
    for (dx, dy) in [
        (-expansion, 0),
        (expansion, 0),
        (0, -expansion),
        (0, expansion),
    ] {
        let shard = ProjectedVertex {
            sx: clamp_i16(i32::from(center.sx).saturating_add(i32::from(dx))),
            sy: clamp_i16(i32::from(center.sy).saturating_add(i32::from(dy))),
            ..center
        };
        submitted +=
            draw_particle_diamond(shard, shard_half, material, slot, ot, primitive_packets);
    }
    submitted
}

fn projectile_offset(position: [i32; 3], velocity: [i32; 3], ticks: i32) -> [i32; 3] {
    [
        position[0].saturating_add(velocity[0].saturating_mul(ticks)),
        position[1].saturating_add(velocity[1].saturating_mul(ticks)),
        position[2].saturating_add(velocity[2].saturating_mul(ticks)),
    ]
}

fn rgb_tuple(rgb: [u8; 3]) -> (u8, u8, u8) {
    (rgb[0], rgb[1], rgb[2])
}

fn scale_rgb(rgb: [u8; 3], numerator: u16, denominator: u16) -> [u8; 3] {
    let denominator = denominator.max(1);
    [
        ((u16::from(rgb[0]) * numerator) / denominator).min(255) as u8,
        ((u16::from(rgb[1]) * numerator) / denominator).min(255) as u8,
        ((u16::from(rgb[2]) * numerator) / denominator).min(255) as u8,
    ]
}

fn draw_projectile_segment<const OT_DEPTH: usize>(
    tail: ProjectedVertex,
    head: ProjectedVertex,
    half: i16,
    material: TextureMaterial,
    slot: psx_engine::DepthSlot,
    ot: &mut OtFrame<'_, OT_DEPTH>,
    primitive_packets: &mut PrimitivePacketArena<'_>,
) -> usize {
    let dx = i32::from(head.sx) - i32::from(tail.sx);
    let dy = i32::from(head.sy) - i32::from(tail.sy);
    let length =
        psx_math::int32::isqrt_i32(dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy)))
            .max(1);
    let nx = (-dy).saturating_mul(i32::from(half)) / length;
    let ny = dx.saturating_mul(i32::from(half)) / length;
    draw_particle_oriented_quad(
        [
            (
                clamp_i16(i32::from(tail.sx) + nx),
                clamp_i16(i32::from(tail.sy) + ny),
            ),
            (
                clamp_i16(i32::from(head.sx) + nx),
                clamp_i16(i32::from(head.sy) + ny),
            ),
            (
                clamp_i16(i32::from(tail.sx) - nx),
                clamp_i16(i32::from(tail.sy) - ny),
            ),
            (
                clamp_i16(i32::from(head.sx) - nx),
                clamp_i16(i32::from(head.sy) - ny),
            ),
        ],
        material,
        slot,
        ot,
        primitive_packets,
    )
}

fn draw_particle_diamond<const OT_DEPTH: usize>(
    center: ProjectedVertex,
    half: i16,
    material: TextureMaterial,
    slot: psx_engine::DepthSlot,
    ot: &mut OtFrame<'_, OT_DEPTH>,
    primitive_packets: &mut PrimitivePacketArena<'_>,
) -> usize {
    draw_particle_oriented_quad(
        [
            (center.sx, center.sy.saturating_sub(half)),
            (center.sx.saturating_add(half), center.sy),
            (center.sx.saturating_sub(half), center.sy),
            (center.sx, center.sy.saturating_add(half)),
        ],
        material,
        slot,
        ot,
        primitive_packets,
    )
}

fn draw_particle_oriented_quad<const OT_DEPTH: usize>(
    screen: [(i16, i16); 4],
    material: TextureMaterial,
    slot: psx_engine::DepthSlot,
    ot: &mut OtFrame<'_, OT_DEPTH>,
    primitive_packets: &mut PrimitivePacketArena<'_>,
) -> usize {
    let quad = QuadTexturedMaterial::with_material(
        screen,
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
        let layer = (seed >> 4) & 3;
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
