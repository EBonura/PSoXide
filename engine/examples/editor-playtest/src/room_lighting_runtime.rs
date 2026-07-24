//! Glue over `psx_game_runtime::room_lighting`: re-exports the shading
//! vocabulary and threads the cooked manifest tables plus this
//! example's VRAM upload resolvers into the crate material builder.

use super::*;

pub(super) use psx_game_runtime::room_lighting::{room_light_slice, RuntimeRoomLighting};

/// The crate room-material builder over this example's cooked
/// `MATERIALS`/`ASSETS` tables and arena-backed texture uploads.
pub(super) fn build_room_materials(
    room: &LevelRoomRecord,
    out: &mut [Option<WorldRenderMaterial>; MAX_ROOM_MATERIALS],
) -> (usize, bool) {
    psx_game_runtime::room_lighting::build_room_materials(
        room,
        MATERIALS,
        ASSETS,
        out,
        ensure_room_texture_uploaded,
        pending_room_texture_upload,
    )
}
