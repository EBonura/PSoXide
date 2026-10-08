//! Glue over `psx_game_runtime::particles`: threads this example's
//! screen extents into the crate emitter/atmosphere draws, keeping the
//! old call-site signatures.

use super::*;

/// Draw one authored particle emitter through the crate policy.
pub(super) fn draw_particle_emitter<'a>(
    emitter: ParticleEmitterRecord,
    camera: WorldCamera,
    projector: Option<LoadedWorldCameraGte>,
    depth_range: DepthRange,
    particle_material: TextureMaterial,
    elapsed_tick: SimTick,
    ot: &mut OtFrame<'a, OT_DEPTH>,
    primitive_packets: &mut PrimitivePacketArena<'a>,
) -> usize {
    psx_game_runtime::particles::draw_particle_emitter(
        emitter,
        camera,
        projector,
        depth_range,
        particle_material,
        elapsed_tick,
        ot,
        primitive_packets,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_projectile_bolt<'a>(
    projectile: psx_game_runtime::projectiles::ProjectileSnapshot,
    camera: WorldCamera,
    projector: Option<LoadedWorldCameraGte>,
    depth_range: DepthRange,
    particle_material: TextureMaterial,
    ot: &mut OtFrame<'a, OT_DEPTH>,
    primitive_packets: &mut PrimitivePacketArena<'a>,
) -> usize {
    psx_game_runtime::particles::draw_projectile_bolt(
        projectile,
        camera,
        projector,
        depth_range,
        particle_material,
        ot,
        primitive_packets,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_projectile_charge<'a>(
    charge: psx_game_runtime::combat::AuthoredProjectileCharge,
    camera: WorldCamera,
    projector: Option<LoadedWorldCameraGte>,
    depth_range: DepthRange,
    particle_material: TextureMaterial,
    ot: &mut OtFrame<'a, OT_DEPTH>,
    primitive_packets: &mut PrimitivePacketArena<'a>,
) -> usize {
    psx_game_runtime::particles::draw_projectile_charge(
        charge,
        camera,
        projector,
        depth_range,
        particle_material,
        ot,
        primitive_packets,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_projectile_impact<'a>(
    impact: psx_game_runtime::projectiles::ProjectileImpactEffect,
    camera: WorldCamera,
    projector: Option<LoadedWorldCameraGte>,
    depth_range: DepthRange,
    particle_material: TextureMaterial,
    ot: &mut OtFrame<'a, OT_DEPTH>,
    primitive_packets: &mut PrimitivePacketArena<'a>,
) -> usize {
    psx_game_runtime::particles::draw_projectile_impact(
        impact,
        camera,
        projector,
        depth_range,
        particle_material,
        ot,
        primitive_packets,
    )
}

/// Draw the room's screen-space atmosphere motes over the frame.
pub(super) fn draw_room_atmosphere_overlay(
    gpu: &mut Gpu,
    room: &LevelRoomRecord,
    elapsed_tick: SimTick,
) {
    psx_game_runtime::particles::draw_room_atmosphere_overlay(
        gpu,
        room,
        elapsed_tick,
        SCREEN_W,
        SCREEN_H,
    );
}
