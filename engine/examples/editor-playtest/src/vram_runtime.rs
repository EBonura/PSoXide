//! Glue over `psx_game_runtime::vram`: threads the cooked manifest
//! tables and the [`VRAM_LAYOUT`] value into the crate methods over the
//! arena-owned [`RuntimeVram`], [`RuntimeFontPackScratch`], and
//! [`RuntimeUiImageCache`] instances (see `runtime_arenas`), and
//! re-exports the vocabulary under the old names so call sites keep
//! their signatures.

use super::*;
use psx_game_runtime::vram::VramLayout;

pub(super) use psx_game_runtime::vram::{SkyTextureKind, VramSlot};

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
    // The CLUT band: the 320 x 32 strip under the back buffer, the only VRAM
    // no texture page covers. A page at page row 256 runs to VRAM row 511, so
    // the band used to share rows (and pixels) with every lower page and a
    // texel read past the bottom of a window returned palette words.
    clut_band: psx_vram::VramRect::new(0, 480, 320, VRAM_CLUT_ROWS as u16),
};

// Pin the layout: the framebuffer, the CLUT band and the two page rows the
// allocator can place pages on (everything right of the framebuffer) share no
// pixel, and the band is as tall as the allocator's CLUT row count. This is
// the same for every project, so every cook runs against a disjoint layout.
const _: () = assert!(
    psx_vram::vram_layout_is_disjoint(&[
        VRAM_LAYOUT.framebuffer,
        VRAM_LAYOUT.clut_band,
        psx_vram::VramRect::new(320, 0, 704, 256),
        psx_vram::VramRect::new(320, 256, 704, 256),
    ]),
    "VRAM layout: framebuffer, CLUT band and texture pages overlap",
);

/// The upload queue's byte source by AssetId (the crate's
/// `resolve_upload_bytes` rule over this example's cooked tables and
/// arena-owned UI cache): queued jobs re-resolve per step instead of
/// retaining `&'static` slices, so the crate contract is plain borrows.
fn upload_bytes_for(asset_id: AssetId) -> Option<&'static [u8]> {
    #[cfg(feature = "cd-stream-bench")]
    {
        // A level texture streamed off the disc lives in the session-resident
        // asset arena, which shares its RAM with the front end's image cache:
        // ask the arena that owns the asset, never the other one. A queued
        // upload exists only once those assets are ready (see
        // `grid_materials`).
        if let Some(asset) = psx_level::find_asset_of_kind(ASSETS, asset_id, AssetKind::Texture) {
            if asset.flags & psx_level::asset_flags::STREAMED_GAMEPLAY_PERSISTENT != 0 {
                return persistent_assets_arena().bytes_for(asset);
            }
        }
        psx_game_runtime::vram::resolve_upload_bytes(ASSETS, ui_images_arena(), asset_id)
    }
    #[cfg(not(feature = "cd-stream-bench"))]
    {
        psx_game_runtime::vram::baked_texture_bytes(ASSETS, asset_id)
    }
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
        ROOMS.as_slice(),
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
        ROOMS.as_slice(),
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

/// Upload a UI texture, stepping the upload queue so menu images resolve
/// within the calling frame.
pub(super) fn ensure_ui_texture_uploaded(
    asset_id: AssetId,
    asset_bytes: &[u8],
) -> Option<VramSlot> {
    vram_arena().ensure_ui_texture_uploaded(VRAM_LAYOUT, asset_id, asset_bytes, &upload_bytes_for)
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
        ROOMS.as_slice(),
        UI_PACK_START_LBA,
        UI_PACK_TOC,
    );
}

/// Return every gameplay-owned texture page and CLUT before a front-end scene
/// becomes active.
pub(super) fn release_gameplay_vram() {
    #[cfg(feature = "cd-stream-bench")]
    // SAFETY: single-threaded guest; the stream is touched only here and in
    // `step_chunk_sky`.
    unsafe {
        (*core::ptr::addr_of_mut!(CHUNK_SKY))
            .release(&mut psx_game_runtime::grid_stream::DiscTransport);
    }
    vram_arena().release_gameplay_vram();
}

/// The chunk sky being streamed in, and the RAM its reads land in.
#[cfg(feature = "cd-stream-bench")]
static mut CHUNK_SKY: psx_game_runtime::cube_sky_stream::CubeSkyStream<
    <psx_game_runtime::grid_stream::DiscTransport as psx_game_runtime::grid_stream::RoomTransport>::Ticket,
> = psx_game_runtime::cube_sky_stream::CubeSkyStream::new();
#[cfg(feature = "cd-stream-bench")]
static mut CHUNK_SKY_WINDOW: psx_game_runtime::cube_sky_stream::CubeSkyWindow =
    psx_game_runtime::cube_sky_stream::CubeSkyWindow::new();

/// Keep the baked sky of the chunk the player is in on screen, for a world
/// that bakes one per chunk (`GRID_SKY_SECTORS` non-zero). Call once per tick.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn step_chunk_sky(room: Option<usize>) {
    use crate::generated::{GRID_ROOM_SKY_SECTORS, GRID_SKY_SECTORS, WORLD_PACK_START_LBA};
    if GRID_SKY_SECTORS == 0 {
        return;
    }
    let Some(key) = ROOMS
        .as_slice()
        .iter()
        .find(|room| {
            room.sky.flags & psx_level::sky_flags::PROJECTION_MASK == psx_level::sky_flags::CUBE
        })
        .and_then(|room| {
            psx_level::find_asset_of_kind(ASSETS, room.sky.texture_asset, AssetKind::Texture)
        })
        .map(|asset| asset.id)
    else {
        return;
    };
    let offset = room
        .and_then(|room| GRID_ROOM_SKY_SECTORS.get(room))
        .copied()
        .unwrap_or(0);
    let lba = if offset == 0 {
        0
    } else {
        WORLD_PACK_START_LBA + offset
    };
    // SAFETY: single-threaded guest; see `release_gameplay_vram`.
    unsafe {
        (*core::ptr::addr_of_mut!(CHUNK_SKY)).step(
            &mut psx_game_runtime::grid_stream::DiscTransport,
            vram_arena(),
            &mut *core::ptr::addr_of_mut!(CHUNK_SKY_WINDOW),
            key,
            lba,
            GRID_SKY_SECTORS,
        );
        CHUNK_SKY_COUNTERS = (*core::ptr::addr_of!(CHUNK_SKY)).counters;
    }
}

/// The chunk sky stream's counters, at a fixed address (like the fault
/// registers) so a RAM dump of a run shows what the stream did.
#[cfg(feature = "cd-stream-bench")]
#[no_mangle]
pub static mut CHUNK_SKY_COUNTERS: psx_game_runtime::cube_sky_stream::SkyCounters =
    psx_game_runtime::cube_sky_stream::SkyCounters {
        submitted: 0,
        landed: 0,
        failed: 0,
        rows: 0,
        shown: 0,
        refused: 0,
    };

/// Upload (once) a recoloured palette copy of a resident texture.
pub(super) fn ensure_clut_variant(
    asset_id: AssetId,
    asset_bytes: &[u8],
    variant: u8,
    recolor: impl FnOnce(&mut [u16]),
) -> Option<u16> {
    vram_arena().ensure_clut_variant(asset_id, asset_bytes, variant, recolor)
}

/// [`ensure_clut_variant`] for a texture whose bytes stay resolvable after
/// load (static or persistent-arena textures, e.g. material overrides).
pub(super) fn ensure_resident_clut_variant(
    asset_id: AssetId,
    variant: u8,
    recolor: impl FnOnce(&mut [u16]),
) -> Option<u16> {
    let bytes = upload_bytes_for(asset_id)?;
    ensure_clut_variant(asset_id, bytes, variant, recolor)
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
