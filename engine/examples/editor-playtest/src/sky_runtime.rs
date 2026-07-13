//! Glue over `psx_game_runtime::sky`: threads the cooked asset table,
//! this example's screen/projection knobs, and the arena-backed VRAM
//! slot resolvers into the crate cyclorama/far-vista draws, keeping
//! the old call-site signatures.

use super::*;

/// Draw the room's cooked sky panorama through the arena-owned
/// [`psx_game_runtime::sky::SkyCyclorama`] cache.
pub(super) fn draw_sky_panorama(
    sky: LevelSkyRecord,
    camera: WorldCamera,
    ot: &mut OtFrame<'_, OT_DEPTH>,
) {
    sky_arena().draw_panorama(
        sky,
        camera,
        ASSETS,
        SCREEN_W,
        SCREEN_H,
        FOCAL,
        find_sky_panorama_vram_slot,
        ensure_sky_panorama_uploaded,
        sky_panorama_tpage_word,
        sky_panorama_clut_word,
        ot,
    );
}

/// Draw the authored far-vista panel ring over this example's cooked
/// tables and texture uploader.
pub(super) fn draw_far_vista_ring(
    camera: WorldCamera,
    vista: LevelFarVistaRecord,
    options: WorldSurfaceOptions,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) {
    psx_game_runtime::sky::draw_far_vista_ring(
        camera,
        vista,
        options,
        ASSETS,
        ensure_texture_uploaded_with_clut_mode,
        triangles,
        world,
    );
}

/// True once the room's sky + far-vista textures are VRAM-resident.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn room_backdrop_textures_ready(record: &LevelRoomRecord) -> bool {
    psx_game_runtime::sky::room_backdrop_textures_ready(
        record,
        ASSETS,
        find_sky_panorama_vram_slot,
        ensure_sky_panorama_uploaded,
        ensure_texture_uploaded_with_clut_mode,
    )
}
