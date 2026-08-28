//! Glue over `psx_game_runtime::image_props`: threads this example's
//! GTE-projection toggle and arena-backed prop-texture resolver into
//! the crate image-prop draw.

use super::*;

/// Draw the authored image props of `current_room` through the crate
/// policy.
pub(super) fn draw_image_props<T>(
    props: &[LevelImagePropRecord],
    current_room: RoomIndex,
    object_visible: impl FnMut(usize) -> bool,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<TriTextured> + PrimitiveSink<TriTexturedGouraud>,
{
    psx_game_runtime::image_props::draw_image_props::<T, PROP_PARTICLE_GTE_PROJECT_ENABLED, OT_DEPTH>(
        props,
        current_room,
        object_visible,
        camera,
        options,
        lighting,
        prop_texture_slot,
        triangles,
        world,
    );
}
