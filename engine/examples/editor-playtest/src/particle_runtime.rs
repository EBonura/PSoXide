//! Glue over `psx_game_runtime::particles`: threads this example's
//! screen extents into the crate emitter/atmosphere draws, keeping the
//! old call-site signatures.

use super::*;

/// Draw one authored particle emitter through the crate policy.
pub(super) fn draw_particle_emitter(
    emitter: ParticleEmitterRecord,
    camera: WorldCamera,
    projector: Option<LoadedWorldCameraGte>,
    depth_range: DepthRange,
    particle_material: TextureMaterial,
    elapsed_tick: SimTick,
    ot: &mut OtFrame<'_, OT_DEPTH>,
    primitive_packets: &mut PrimitivePacketArena<'_>,
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

pub(super) fn draw_water_wade_splash(
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
    psx_game_runtime::particles::draw_water_wade_splash(
        x,
        surface_y,
        z,
        camera,
        projector,
        depth_range,
        particle_material,
        elapsed_tick,
        ot,
        primitive_packets,
    )
}

/// Draw the room's screen-space atmosphere motes over the frame.
pub(super) fn draw_room_atmosphere_overlay(room: &LevelRoomRecord, elapsed_tick: SimTick) {
    psx_game_runtime::particles::draw_room_atmosphere_overlay(
        room,
        elapsed_tick,
        SCREEN_W,
        SCREEN_H,
    );
}
