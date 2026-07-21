use super::*;

pub(super) fn draw_water<T>(
    room: RoomIndex,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<TriTextured> + PrimitiveSink<TriTexturedGouraud>,
{
    let sector_size = ROOMS
        .get(room.to_usize())
        .map(|record| i32::from(record.sector_size))
        .unwrap_or(0);
    psx_game_runtime::water::draw_water_cells::<T, PROP_PARTICLE_GTE_PROJECT_ENABLED, OT_DEPTH>(
        WATER_CELLS,
        room,
        sector_size,
        camera,
        options,
        lighting,
        prop_texture_slot,
        triangles,
        world,
    );
}
