//! Glue over `psx_game_runtime::sky`: threads the cooked asset table,
//! this example's screen/projection knobs, and the arena-backed VRAM
//! slot resolvers into the crate cyclorama/far-vista draws, keeping
//! the old call-site signatures.

use super::*;
use psx_engine::classic_affine::ClassicAffineSubmit;
use psx_bsp::sky::{
    submit_view_ray_cube_sky, submit_view_ray_layered_sky, VIEW_RAY_CUBE_SKY_PACKET_WORDS,
    VIEW_RAY_SKY_PACKET_WORDS,
};

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

/// Rotation-keyed copy of the last cube-sky packet stream.
///
/// The cube sky depends on the view rotation and the sky's VRAM slot alone:
/// the lattice is fixed on screen and its packets carry staged OT slot tags,
/// not addresses. A frame that keeps the previous rotation therefore replays
/// the stream with one copy instead of walking the lattice and clipping the
/// mixed cells against six cube faces, which was 7% of the whole-level tape.
struct CubeSkyPacketCache {
    rotation: [[i16; 3]; 3],
    tpage_word: u16,
    clut_word: u16,
    valid: bool,
    words: usize,
    packets: u32,
    stream: [u32; VIEW_RAY_CUBE_SKY_PACKET_WORDS],
}

static mut CUBE_SKY_PACKET_CACHE: CubeSkyPacketCache = CubeSkyPacketCache {
    rotation: [[0; 3]; 3],
    tpage_word: 0,
    clut_word: 0,
    valid: false,
    words: 0,
    packets: 0,
    stream: [0; VIEW_RAY_CUBE_SKY_PACKET_WORDS],
};

/// Draw the World node's one sky definition. Projection-specific packet
/// kernels remain specialized, while selection, visibility and residency are
/// resolved once here.
pub(super) fn draw_scene_sky(
    sky: LevelSkyRecord,
    camera: WorldCamera,
    material_tick: u32,
    visible_sky_aperture: bool,
    primitive_packets: &mut PrimitivePacketArena<'_>,
    ot: &mut OtFrame<'_, OT_DEPTH>,
) {
    if sky.flags & psx_level::sky_flags::ENABLED == 0
        || (sky.flags & psx_level::sky_flags::THROUGH_SKY_SURFACES != 0 && !visible_sky_aperture)
    {
        return;
    }

    let projection = sky.flags & psx_level::sky_flags::PROJECTION_MASK;
    if projection == 0 || projection == psx_level::sky_flags::PANORAMA {
        draw_sky_panorama(sky, camera, ot);
        return;
    }

    let kind = if projection == psx_level::sky_flags::QUAKE_LAYERED {
        SkyTextureKind::QuakeLayered
    } else if projection == psx_level::sky_flags::CUBE {
        SkyTextureKind::Cube
    } else {
        return;
    };
    let Some(asset) = find_asset_of_kind(ASSETS, sky.texture_asset, AssetKind::Texture) else {
        return;
    };
    let slot = find_sky_texture_vram_slot(kind, asset.id)
        .or_else(|| ensure_sky_texture_uploaded(kind, asset.id, asset.bytes));
    let Some(slot) = slot.filter(|slot| slot.ready) else {
        return;
    };
    let word_capacity = match kind {
        SkyTextureKind::QuakeLayered => VIEW_RAY_SKY_PACKET_WORDS,
        SkyTextureKind::Cube => VIEW_RAY_CUBE_SKY_PACKET_WORDS,
        SkyTextureKind::Panorama => unreachable!(),
    };
    let Some(mut reservation) = primitive_packets.reserve_packet_words(word_capacity) else {
        return;
    };
    let view = psx_bsp::render::load_pxbsp_view_rotation(
        crate::bsp_runtime::pxbsp_camera(camera).origin,
        crate::bsp_runtime::pxbsp_view_rotation(camera),
    );
    let submitted = unsafe {
        let output = reservation.words_mut().as_mut_ptr();
        match kind {
            SkyTextureKind::QuakeLayered => {
                let layer_width = slot.texture_width / 2;
                submit_view_ray_layered_sky(
                    slot.tpage_word,
                    slot.clut_word,
                    [0, 0],
                    [
                        u8::try_from(layer_width).unwrap_or(u8::MAX),
                        u8::try_from(slot.texture_height).unwrap_or(u8::MAX),
                    ],
                    view.rotation,
                    [SCREEN_W as i16, SCREEN_H as i16],
                    [SCREEN_W as i16 / 2, SCREEN_H as i16 / 2],
                    FOCAL as i16,
                    material_tick,
                    output,
                )
            }
            SkyTextureKind::Cube => {
                // Single-threaded guest; the cache is touched only here.
                let cache = &mut *core::ptr::addr_of_mut!(CUBE_SKY_PACKET_CACHE);
                if cache.valid
                    && cache.rotation == view.rotation.m
                    && cache.tpage_word == slot.tpage_word
                    && cache.clut_word == slot.clut_word
                {
                    core::ptr::copy_nonoverlapping(cache.stream.as_ptr(), output, cache.words);
                    ClassicAffineSubmit {
                        next_packet: output.add(cache.words),
                        packets: cache.packets,
                        hardware_triangles: 0,
                    }
                } else {
                    let submitted = submit_view_ray_cube_sky(
                        slot.tpage_word,
                        slot.clut_word,
                        view.rotation,
                        [SCREEN_W as i16, SCREEN_H as i16],
                        [SCREEN_W as i16 / 2, SCREEN_H as i16 / 2],
                        FOCAL as i16,
                        output,
                    );
                    let words = submitted.next_packet.offset_from(output).max(0) as usize;
                    core::ptr::copy_nonoverlapping(output, cache.stream.as_mut_ptr(), words);
                    cache.rotation = view.rotation.m;
                    cache.tpage_word = slot.tpage_word;
                    cache.clut_word = slot.clut_word;
                    cache.words = words;
                    cache.packets = submitted.packets;
                    cache.valid = true;
                    submitted
                }
            }
            SkyTextureKind::Panorama => unreachable!(),
        }
    };
    let words = unsafe {
        submitted
            .next_packet
            .offset_from(reservation.words_mut().as_mut_ptr())
    }
    .max(0) as usize;
    let stream = reservation
        .commit(words, submitted.packets as usize)
        .expect("scene sky kernel exceeded its reserved packet range");
    unsafe {
        ot.add_committed_tagged_packet_stream_unchecked(stream);
    }
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
        find_sky_texture_vram_slot,
        ensure_sky_texture_uploaded,
        ensure_texture_uploaded_with_clut_mode,
    )
}
