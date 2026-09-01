//! Glue over `psx_game_runtime::vram`: threads the cooked manifest
//! tables and the [`VRAM_LAYOUT`] value into the crate methods over the
//! arena-owned [`RuntimeVram`], [`RuntimeFontPackScratch`], and
//! [`RuntimeUiImageCache`] instances (see `runtime_arenas`), and
//! re-exports the vocabulary under the old names so call sites keep
//! their signatures.

use super::*;
use psx_game_runtime::vram::VramLayout;

pub(super) use psx_game_runtime::vram::{SkyTextureKind, VramSlot, VramSlotClutMode};

/// This example's VRAM placement, threaded into every crate
/// `VramRuntime` method as one value (the PROJECTION pattern).
pub(super) const VRAM_LAYOUT: VramLayout = VramLayout {
    // Double-buffered framebuffer.
    framebuffer: psx_vram::VramRect::new(0, 0, 320, 480),
    room_tpage_base_x: ROOM_TPAGE_BASE_X,
    shared_tpage: SHARED_TPAGE,
    room_tpage_stride_hw: ROOM_TPAGE_STRIDE_HW,
    room_tile_texels: ROOM_TILE_TEXELS,
    model_tpage: MODEL_TPAGE,
    model_tpage_max_halfwords: MODEL_TPAGE_MAX_HALFWORDS,
    // First VRAM row of the managed CLUT band, just past the back buffer.
    clut_base_y: 480,
};

/// The upload queue's byte source by AssetId (the crate's
/// `resolve_upload_bytes` rule over this example's cooked tables and
/// arena-owned UI cache): queued jobs re-resolve per step instead of
/// retaining `&'static` slices, so the crate contract is plain borrows.
fn upload_bytes_for(asset_id: AssetId) -> Option<&'static [u8]> {
    #[cfg(feature = "cd-stream-bench")]
    {
        psx_game_runtime::vram::resolve_upload_bytes(ASSETS, ui_images_arena(), asset_id)
    }
    #[cfg(not(feature = "cd-stream-bench"))]
    {
        psx_game_runtime::vram::baked_texture_bytes(ASSETS, asset_id)
    }
}

/// Pre-mark a room's required asset set on the residency contract
/// tracker (the crate runtime owns the `ResidencyManager`).
pub(super) fn ensure_room_resident(residency: &psx_level::RoomResidencyRecord) {
    let _ = vram_arena().ensure_room_resident(residency);
}

/// Scope the persistent RAM asset set to `desired`'s residency needs.
///
/// Releases texture payloads no desired room requires or warms, and reads only
/// the ones that are missing, so crossing into a neighbour that shares textures
/// costs no CD traffic. Model meshes and clips stay pinned for the level; see
/// `PersistentAssetStreamer::evictable`.
pub(super) fn request_persistent_assets(desired: &[RoomIndex], count: usize) {
    let desired = &desired[..count.min(desired.len())];
    let arena = persistent_assets_arena_mut();
    // The streamer counts PERSISTENT_ASSET_LOAD_FAILURES on the failing edge
    // itself, so every path that gives up is counted, not just this one.
    arena.request_rooms(
        UI_PACK_START_LBA,
        UI_PACK_TOC,
        ASSETS,
        desired,
        ROOM_RESIDENCY,
    );
    telemetry::counter(
        telemetry::counter::PERSISTENT_ASSET_RESIDENT_BYTES,
        arena.resident_bytes().min(u32::MAX as usize) as u32,
    );
}

/// Debounced room-texture eviction against the desired resident set;
/// policy and the last-evict-room debounce live on
/// `VramRuntime::evict_unreferenced_vram`.
pub(super) fn evict_unreferenced_vram(
    current_room: RoomIndex,
    desired: &[RoomIndex],
    count: usize,
) {
    vram_arena().evict_unreferenced_vram(current_room, desired, count, ROOM_RESIDENCY);
}

/// Start a short grace period after entering a menu scene. This lets the boot
/// frame (or transition cover) present before the next UI.PAK read starts.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn note_menu_ui_scene_entered() {
    ui_images_arena_mut().note_menu_scene_entered();
}

/// Advance menu UI streaming by at most one CD chunk, then upload any active
/// scene image whose bytes are already cached; the policy lives on
/// `UiImageCache::service_menu_images` since phase 1.5.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn service_menu_ui_images(scene_id: u16) {
    ui_images_arena_mut().service_menu_images(
        vram_arena(),
        VRAM_LAYOUT,
        cd_arena(),
        scene_id,
        UI_SCENES,
        UI_NODES,
        ASSETS,
        ROOMS,
        UI_PACK_START_LBA,
        UI_PACK_TOC,
    );
}

/// True once every streamed front-end UI image is resident in the RAM cache.
/// The engine holds the menu CD-DA until this is true (see
/// `Scene::front_end_assets_ready`) so the front-end never reads the CD while
/// music plays.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn menu_ui_cache_ready() -> bool {
    ui_images_arena().ready()
}

/// Without the streaming feature there are no streamed UI images, so the
/// front-end is always resident.
#[cfg(not(feature = "cd-stream-bench"))]
pub(super) fn menu_ui_cache_ready() -> bool {
    true
}

/// Upload the active UI scene's streamed images into VRAM from the RAM cache.
/// Tracks each created slot so `release_ui_images` can free it when the scene
/// changes or gameplay starts.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn load_ui_images_for_scene(scene_id: u16) -> bool {
    vram_arena().load_ui_images_for_scene(
        VRAM_LAYOUT,
        ui_images_arena(),
        scene_id,
        UI_SCENES,
        UI_NODES,
        ASSETS,
        ROOMS,
    )
}

/// Hand the menu UI-image cache's RAM over to gameplay: drop the cache
/// contents so room draws may overwrite the bytes (`MenuGameplayOverlay`),
/// and so the next menu entry re-preloads from CD.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn retire_menu_ui_cache() {
    ui_images_arena_mut().invalidate();
}

/// Free every streamed UI image VRAM slot created by `load_ui_images_for_scene`.
/// Called on gameplay entry so the room textures reclaim that VRAM. Fonts are
/// shared and are NOT released here.
pub(super) fn release_ui_images() {
    vram_arena().release_ui_images();
}

/// Acquire the active scene's UI fonts (reserving static VRAM regions on the
/// first call). A different slice replaces and releases the previous pack.
#[must_use = "false means the complete UI font set is unavailable"]
pub(super) fn acquire_ui_fonts(
    fonts: &'static [&'static psx_font::BitmapFont],
    ui_fonts: &mut [Option<FontAtlas>; MAX_RUNTIME_UI_FONTS],
) -> bool {
    vram_arena().acquire_shared_ui_fonts(VRAM_LAYOUT, font_scratch_arena(), fonts, ui_fonts)
}

const VRAM_UPLOAD_ROWS_PER_BACKGROUND_TICK: u16 = 768;
const ROOM_WINDOW_BACKGROUND_TICK_MASK: u32 = 1;

#[derive(Copy, Clone)]
pub(super) struct RuntimeStreamingJobs {
    vram_rows_per_tick: u16,
}

impl RuntimeStreamingJobs {
    pub(super) const fn new() -> Self {
        Self {
            vram_rows_per_tick: VRAM_UPLOAD_ROWS_PER_BACKGROUND_TICK,
        }
    }

    pub(super) fn background_tick(self, ctx: &Ctx) -> bool {
        (ctx.sim_tick.as_u32() & ROOM_WINDOW_BACKGROUND_TICK_MASK) != 0
    }

    pub(super) fn step_vram_uploads(self) -> bool {
        vram_arena().step_uploads(self.vram_rows_per_tick, &upload_bytes_for)
    }

    pub(super) fn vram_uploads_idle(self) -> bool {
        vram_arena().uploads_idle()
    }
}

/// Sky panorama page tpage word (`page` 0 or 1), from the runtime's
/// cached placement.
pub(super) fn sky_panorama_tpage_word(page: usize) -> u16 {
    vram_arena().sky_panorama_tpage_word(page)
}

/// Sky panorama CLUT word for `band`, from the runtime's cached placement.
pub(super) fn sky_panorama_clut_word(band: usize) -> u16 {
    vram_arena().sky_panorama_clut_word(band)
}

/// Upload the subtract-blended circular floor shadow decal.
pub(super) fn upload_shadow_texture() -> Option<TextureMaterial> {
    vram_arena().upload_shadow_texture(SHADOW_CIRCLE_BLOB)
}

/// Upload the tintable vitality-circle machine glyph into the shared effects
/// page.
pub(super) fn upload_vitality_circle_texture() -> Option<TextureMaterial> {
    vram_arena().upload_vitality_circle_texture(VITALITY_CIRCLE_GLYPH_BLOB)
}

/// Upload the tintable 32x32 HUD swap-dial texels.  Its material and CLUT are
/// shared with the world vitality-circle glyph.
pub(super) fn upload_vitality_hud_dial_texels() -> bool {
    vram_arena().upload_vitality_hud_dial_texels(VITALITY_HUD_DIAL_BLOB)
}

/// Generate and upload the 16x16 white circular particle sprite.
pub(super) fn upload_particle_texture() -> Option<TextureMaterial> {
    vram_arena().upload_particle_texture()
}

/// Look up the sky panorama's VRAM slot, if the sky is uploaded.
pub(super) fn find_sky_panorama_vram_slot(asset_id: AssetId) -> Option<VramSlot> {
    vram_arena().find_sky_panorama_vram_slot(asset_id)
}

/// Look up the scene-level sky through the shared residency owner.
pub(super) fn find_sky_texture_vram_slot(
    kind: SkyTextureKind,
    asset_id: AssetId,
) -> Option<VramSlot> {
    vram_arena().find_sky_texture_vram_slot(kind, asset_id)
}

/// Ready room-texture slot (either 4bpp CLUT mode) for `asset_id`.
pub(super) fn find_room_texture_vram_slot(asset_id: AssetId) -> Option<VramSlot> {
    vram_arena().find_room_texture_vram_slot(asset_id)
}

/// True while `asset_id` has a room-texture upload still in flight.
pub(super) fn pending_room_texture_upload(asset_id: AssetId) -> bool {
    vram_arena().pending_room_texture_upload(asset_id)
}

/// Upload `asset_bytes` to VRAM if not already resident (CLUT mode from
/// the texture's transparency flag).
pub(super) fn ensure_texture_uploaded(asset_id: AssetId, asset_bytes: &[u8]) -> Option<VramSlot> {
    vram_arena().ensure_texture_uploaded(VRAM_LAYOUT, asset_id, asset_bytes)
}

/// Upload a room material texture (palette entry 0 forced opaque).
pub(super) fn ensure_room_texture_uploaded(
    asset_id: AssetId,
    asset_bytes: &[u8],
) -> Option<VramSlot> {
    vram_arena().ensure_room_texture_uploaded(VRAM_LAYOUT, asset_id, asset_bytes)
}

/// Upload whichever scene sky the World selected.
pub(super) fn ensure_sky_texture_uploaded(
    kind: SkyTextureKind,
    asset_id: AssetId,
    asset_bytes: &[u8],
) -> Option<VramSlot> {
    vram_arena().ensure_sky_texture_uploaded(VRAM_LAYOUT, kind, asset_id, asset_bytes)
}

/// Queue (or resolve) the room-owned reflection probe. The active-room window
/// calls this for the current and warm adjacent rooms, so a portal crossing
/// only switches the selected resident slot.
pub(super) fn room_reflection_probe_ready(room: RoomIndex) -> bool {
    let Some(asset_id) = ROOM_REFLECTION_PROBES
        .get(room.to_usize())
        .copied()
        .flatten()
    else {
        return true;
    };
    if find_room_texture_vram_slot(asset_id).is_some() {
        return true;
    }
    let Some(bytes) = upload_bytes_for(asset_id) else {
        return false;
    };
    ensure_room_texture_uploaded(asset_id, bytes).is_some_and(|slot| slot.ready)
}

/// Upload a UI texture, stepping the upload queue so menu images resolve
/// within the calling frame.
pub(super) fn ensure_ui_texture_uploaded(
    asset_id: AssetId,
    asset_bytes: &[u8],
) -> Option<VramSlot> {
    vram_arena().ensure_ui_texture_uploaded(VRAM_LAYOUT, asset_id, asset_bytes, &upload_bytes_for)
}

/// Queue `asset_bytes` for upload with an explicit CLUT stamping mode.
pub(super) fn ensure_texture_uploaded_with_clut_mode(
    asset_id: AssetId,
    asset_bytes: &[u8],
    clut_mode: VramSlotClutMode,
) -> Option<VramSlot> {
    vram_arena().ensure_texture_uploaded_with_clut_mode(
        VRAM_LAYOUT,
        asset_id,
        asset_bytes,
        clut_mode,
    )
}

/// Resolve (or begin uploading) a prop texture's transparent-zero slot.
pub(super) fn prop_texture_slot(texture_asset: AssetId) -> Option<VramSlot> {
    vram_arena().prop_texture_slot(VRAM_LAYOUT, ASSETS, texture_asset)
}

/// Resolve a model material texture using the transparency mode authored in
/// its PSXT header. Model base layers are commonly opaque while their overlays
/// use transparent index zero, so forcing both through the prop resolver can
/// miss an already-resident base slot and expose the model atlas for a frame.
pub(super) fn model_texture_slot(texture_asset: AssetId) -> Option<VramSlot> {
    let bytes = upload_bytes_for(texture_asset)?;
    ensure_texture_uploaded(texture_asset, bytes)
}

/// True once every image/box prop texture of `room` is VRAM-resident.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn room_prop_textures_ready(room: RoomIndex) -> bool {
    if !vram_arena().room_prop_textures_ready(
        VRAM_LAYOUT,
        ASSETS,
        IMAGE_PROPS,
        BOX_PROPS,
        CYLINDER_PROPS,
        ARCH_PROPS,
        room,
    ) {
        return false;
    }
    WATER_CELLS
        .iter()
        .filter(|cell| cell.room == room)
        .all(|cell| {
            cell.texture_asset
                .is_none_or(|asset| prop_texture_slot(asset).is_some())
        })
}

/// Upload the streamed sky panorama synchronously from `asset_bytes`.
pub(super) fn ensure_sky_panorama_uploaded(
    asset_id: AssetId,
    asset_bytes: &[u8],
) -> Option<VramSlot> {
    vram_arena().ensure_sky_panorama_uploaded(asset_id, asset_bytes)
}

/// Load the CD-streamed sky panorama into VRAM on gameplay entry, staged
/// through the shared font-pack scratch (free during gameplay; see the
/// crate method's doc and the `SKY_STAGE_WORDS` const assert in
/// `runtime_config`).
#[cfg(feature = "cd-stream-bench")]
pub(super) fn load_streamed_sky_from_cd() {
    vram_arena().load_streamed_sky_from_cd(
        VRAM_LAYOUT,
        cd_arena(),
        font_scratch_arena(),
        GAMEPLAY_PACK_MAX_CHUNK_BYTES,
        ASSETS,
        ROOMS,
        UI_PACK_START_LBA,
        UI_PACK_TOC,
    );
}

/// Return every gameplay-owned texture page and CLUT before a front-end scene
/// becomes active.
pub(super) fn release_gameplay_vram() {
    vram_arena().release_gameplay_vram();
}

/// Upload an 8bpp model atlas to the dedicated model VRAM region.
pub(super) fn ensure_model_atlas_uploaded(
    asset_id: AssetId,
    asset_bytes: &[u8],
) -> Option<VramSlot> {
    vram_arena().ensure_model_atlas_uploaded(VRAM_LAYOUT, asset_id, asset_bytes)
}

#[cfg(test)]
mod tests {
    use super::SHADOW_CIRCLE_BLOB;
    use psx_asset::Texture;

    /// The decal blends only where its palette says so.
    ///
    /// `BlendMode::Average` on the primitive selects *how* to blend; the CLUT
    /// entry's STP bit (15) selects *which texels* blend at all. The shipped
    /// decal had STP clear on every entry, so the GPU painted an opaque patch
    /// of near-black grey and the "shadow" only moved a dark floor by about
    /// 30 of 255. Nothing in the pipeline reports that, which is exactly why
    /// it needs a test rather than a comment.
    #[test]
    fn shadow_decal_palette_blends_and_reserves_index_zero() {
        let texture = Texture::from_bytes(SHADOW_CIRCLE_BLOB).expect("decal parses");
        assert_eq!(texture.width(), 64);
        assert_eq!(texture.height(), 64);
        assert_eq!(texture.clut_entries(), 16);

        let clut = texture.clut_bytes();
        assert_eq!(clut.len(), 32);
        let entry = |i: usize| u16::from_le_bytes([clut[i * 2], clut[i * 2 + 1]]);

        // Index 0 is the one value the GPU skips outright: the decal's
        // "outside the circle". Setting STP there would blend it instead.
        assert_eq!(entry(0), 0, "index 0 must stay fully transparent");
        for i in 1..16 {
            assert_ne!(
                entry(i) & 0x8000,
                0,
                "CLUT entry {i} has STP clear, so it would draw opaque"
            );
        }
    }

    /// The darkening comes from a BLACK texel under Average, not a grey one:
    /// `(background + 0) / 2` halves any floor, while a grey texel brightens
    /// every floor darker than itself.
    #[test]
    fn shadow_decal_core_is_black_and_the_rim_stays_dark() {
        let texture = Texture::from_bytes(SHADOW_CIRCLE_BLOB).expect("decal parses");
        let clut = texture.clut_bytes();
        let entry = |i: usize| u16::from_le_bytes([clut[i * 2], clut[i * 2 + 1]]);

        assert_eq!(entry(1), 0x8000, "the core must be STP-set pure black");
        // Rim greys stay under 4 of 31 per channel so they cannot lift a dark
        // Cortex floor. 0x8000 | (4 | 4<<5 | 4<<10) is the ceiling.
        for i in 1..16 {
            let e = entry(i);
            for shift in [0, 5, 10] {
                assert!(
                    (e >> shift) & 0x1f <= 4,
                    "CLUT entry {i} channel is too bright to stay a shadow"
                );
            }
        }

        // The centre texel is the darkest index, the corner is transparent.
        let px = texture.pixel_bytes();
        let centre = px[32 * 32 + 16] & 0x0f;
        assert_eq!(centre, 1, "decal centre must be the black core index");
        assert_eq!(px[0] & 0x0f, 0, "decal corner must be transparent");
    }
}
