//! Glue over `psx_game_runtime::vram`: keeps this example's static
//! instances of the crate-owned [`VramRuntime`], the shared
//! [`FontPackScratch`] staging buffer, and the streamed-UI
//! [`UiImageCache`], threads the cooked manifest tables and the
//! [`VRAM_LAYOUT`] value into the crate methods, and re-exports the
//! vocabulary under the old names so call sites keep their signatures.
//! The scratch and cache stay separate zero-init statics (not
//! `VramRuntime` fields) so they remain in `.bss` instead of storing
//! ~220 KB of zeros in the flat PSX-EXE image.

use super::*;
#[cfg(feature = "cd-stream-bench")]
use psx_game_runtime::vram::UiImageCache;
use psx_game_runtime::vram::{FontPackScratch, VramLayout, VramRuntime, FONT_ATLAS_MAX_ROWS};

pub(super) use psx_game_runtime::vram::{vram_slot_texture_size_u8, VramSlot, VramSlotClutMode};

/// Capacity of the residency manager's RAM table. Holds room
/// world + model meshes + animation clips.
const MAX_RESIDENT_RAM_ASSETS: usize = 128;
/// Capacity of the residency manager's VRAM table. Holds room
/// material atlases + model atlases.
const MAX_RESIDENT_VRAM_ASSETS: usize = 64;
/// CLUT-band rows the unified VRAM allocator manages, just past the back
/// buffer (Stage 1: only the shared font CLUT lands here).
const VRAM_CLUT_ROWS: usize = 16;

/// This example's VRAM placement, threaded into every crate
/// `VramRuntime` method as one value (the PROJECTION pattern).
const VRAM_LAYOUT: VramLayout = VramLayout {
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

/// This example's one crate VRAM runtime instance (slot table, unified
/// allocator, residency tracker, upload queue), in static storage per
/// the carve pattern.
static mut VRAM_RUNTIME: PlaytestVramRuntime = PlaytestVramRuntime::new(VRAM_LAYOUT);

/// The crate VRAM runtime instantiated with this example's budget consts.
type PlaytestVramRuntime = VramRuntime<
    MAX_RESIDENT_RAM_ASSETS,
    MAX_RESIDENT_VRAM_ASSETS,
    ROOM_TPAGE_COUNT,
    VRAM_CLUT_ROWS,
>;

/// One short-lived exclusive borrow of the static runtime per glue
/// call, same discipline as the example's other static mut instances.
fn vram() -> &'static mut PlaytestVramRuntime {
    unsafe { &mut *core::ptr::addr_of_mut!(VRAM_RUNTIME) }
}

// Sized to the larger of the two scratch uses (font atlas packing vs the
// streamed sky chunk); see the doc on `psx_game_runtime::vram::FontPackScratch`.
const FONT_PACK_U16: usize = MAX_RUNTIME_UI_FONTS * 64 * FONT_ATLAS_MAX_ROWS;
#[cfg(feature = "cd-stream-bench")]
const FONT_PACK_SCRATCH_LEN: usize = {
    let sky_u16 = (GAMEPLAY_PACK_MAX_CHUNK_BYTES + 1) / 2;
    if FONT_PACK_U16 > sky_u16 {
        FONT_PACK_U16
    } else {
        sky_u16
    }
};
#[cfg(not(feature = "cd-stream-bench"))]
const FONT_PACK_SCRATCH_LEN: usize = FONT_PACK_U16;
static mut FONT_PACK_SCRATCH: FontPackScratch<FONT_PACK_SCRATCH_LEN> = FontPackScratch::new();

#[cfg(feature = "cd-stream-bench")]
const SKY_STAGE_WORDS: usize = (GAMEPLAY_PACK_MAX_CHUNK_BYTES + 3) / 4;
#[cfg(feature = "cd-stream-bench")]
const _: () = assert!(
    SKY_STAGE_WORDS * 4 <= FONT_PACK_SCRATCH_LEN * 2,
    "streamed sky chunk does not fit FONT_PACK_SCRATCH staging buffer"
);

/// RAM cache slot width for one streamed menu UI image chunk.
#[cfg(feature = "cd-stream-bench")]
const UI_STAGE_WORDS: usize = (UI_PACK_MAX_CHUNK_BYTES + 3) / 4;
/// This example's streamed menu UI image cache instance (see
/// `psx_game_runtime::vram::UiImageCache`).
#[cfg(feature = "cd-stream-bench")]
static mut UI_IMAGE_CACHE: UiImageCache<UI_STAGE_WORDS, UI_PACK_IMAGE_CACHE_SLOTS> =
    UiImageCache::new();

#[cfg(feature = "cd-stream-bench")]
fn ui_cache() -> &'static UiImageCache<UI_STAGE_WORDS, UI_PACK_IMAGE_CACHE_SLOTS> {
    unsafe { &*core::ptr::addr_of!(UI_IMAGE_CACHE) }
}

#[cfg(feature = "cd-stream-bench")]
fn ui_cache_mut() -> &'static mut UiImageCache<UI_STAGE_WORDS, UI_PACK_IMAGE_CACHE_SLOTS> {
    unsafe { &mut *core::ptr::addr_of_mut!(UI_IMAGE_CACHE) }
}

/// Pre-mark a room's required asset set on the residency contract
/// tracker (the crate runtime owns the `ResidencyManager`).
pub(super) fn ensure_room_resident(residency: &psx_level::RoomResidencyRecord) {
    let _ = vram().ensure_room_resident(residency);
}

/// Debounced room-texture eviction against the desired resident set;
/// policy and the last-evict-room debounce live on
/// `VramRuntime::evict_unreferenced_vram`.
pub(super) fn evict_unreferenced_vram(
    current_room: RoomIndex,
    desired: &[RoomIndex],
    count: usize,
) {
    vram().evict_unreferenced_vram(current_room, desired, count, ROOM_RESIDENCY);
}

/// Start a short grace period after entering a menu scene. This lets the boot
/// frame (or transition cover) present before the next UI.PAK read starts.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn note_menu_ui_scene_entered() {
    ui_cache_mut().note_menu_scene_entered();
}

/// Advance menu UI streaming by at most one CD chunk, then upload any active
/// scene image whose bytes are already cached. Prioritising the active scene
/// keeps the visible menu complete, while the fallback preloads the other menu
/// states so later transitions avoid CD reads.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn service_menu_ui_images(scene_id: u16) {
    if scene_id == psx_level::UI_SCENE_NONE {
        return;
    }

    if ui_cache_mut().defer_tick() {
        load_ui_images_for_scene(scene_id);
        return;
    }

    // Stream the WHOLE front-end UI image run from UI.PAK in ONE contiguous CD
    // read (one seek, sequential sectors, one pause) instead of N separate
    // SetLoc+ReadN+Pause cycles. Each per-image cycle forces a real CD-R drive
    // to stop/seek/re-acquire -- a HUGE boot stall on hardware, cheap only in
    // the emulator. The front-end CD-DA is held until this completes
    // (Scene::front_end_assets_ready), so no music contends with the read.
    if !ui_cache().ready() {
        ui_cache_mut().preload_all_contiguous(ASSETS, ROOMS, UI_PACK_START_LBA, UI_PACK_TOC);
    }
    load_ui_images_for_scene(scene_id);
}

/// True once every streamed front-end UI image is resident in the RAM cache.
/// The engine holds the menu CD-DA until this is true (see
/// `Scene::front_end_assets_ready`) so the front-end never reads the CD while
/// music plays.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn menu_ui_cache_ready() -> bool {
    ui_cache().ready()
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
pub(super) fn load_ui_images_for_scene(scene_id: u16) {
    vram().load_ui_images_for_scene(
        VRAM_LAYOUT,
        ui_cache(),
        scene_id,
        UI_SCENES,
        UI_NODES,
        ASSETS,
        ROOMS,
    );
}

/// Free every streamed UI image VRAM slot created by `load_ui_images_for_scene`.
/// Called on gameplay entry so the room textures reclaim that VRAM. Fonts are
/// shared and are NOT released here.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn release_ui_images() {
    vram().release_ui_images();
}

/// Acquire the shared UI fonts (reserving the static VRAM regions on
/// first call); the packing/upload policy lives on
/// `VramRuntime::acquire_shared_ui_fonts`.
pub(super) fn acquire_shared_ui_fonts(ui_fonts: &mut [Option<FontAtlas>; MAX_RUNTIME_UI_FONTS]) {
    let scratch = unsafe { &mut *core::ptr::addr_of_mut!(FONT_PACK_SCRATCH) };
    vram().acquire_shared_ui_fonts(VRAM_LAYOUT, scratch, UI_FONTS, ui_fonts);
}

const VRAM_UPLOAD_ROWS_PER_BACKGROUND_TICK: u16 = 512;
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
        vram().step_uploads(self.vram_rows_per_tick)
    }

    pub(super) fn vram_uploads_idle(self) -> bool {
        vram().uploads_idle()
    }
}

/// Sky panorama page tpage word (`page` 0 or 1), from the runtime's
/// cached placement.
pub(super) fn sky_panorama_tpage_word(page: usize) -> u16 {
    vram().sky_panorama_tpage_word(page)
}

/// Sky panorama CLUT word for `band`, from the runtime's cached placement.
pub(super) fn sky_panorama_clut_word(band: usize) -> u16 {
    vram().sky_panorama_clut_word(band)
}

/// Upload the subtract-blended circular floor shadow decal.
pub(super) fn upload_shadow_texture() -> Option<TextureMaterial> {
    vram().upload_shadow_texture(SHADOW_CIRCLE_BLOB)
}

/// Generate and upload the 16x16 white circular particle sprite.
pub(super) fn upload_particle_texture() -> Option<TextureMaterial> {
    vram().upload_particle_texture()
}

/// Look up the sky panorama's VRAM slot, if the sky is uploaded.
pub(super) fn find_sky_panorama_vram_slot(asset_id: AssetId) -> Option<VramSlot> {
    vram().find_sky_panorama_vram_slot(asset_id)
}

/// Ready room-texture slot (either 4bpp CLUT mode) for `asset_id`.
pub(super) fn find_room_texture_vram_slot(asset_id: AssetId) -> Option<VramSlot> {
    vram().find_room_texture_vram_slot(asset_id)
}

/// True while `asset_id` has a room-texture upload still in flight.
pub(super) fn pending_room_texture_upload(asset_id: AssetId) -> bool {
    vram().pending_room_texture_upload(asset_id)
}

/// Upload `asset_bytes` to VRAM if not already resident (CLUT mode from
/// the texture's transparency flag).
pub(super) fn ensure_texture_uploaded(
    asset_id: AssetId,
    asset_bytes: &'static [u8],
) -> Option<VramSlot> {
    vram().ensure_texture_uploaded(VRAM_LAYOUT, asset_id, asset_bytes)
}

/// Upload a room material texture (palette entry 0 forced opaque).
pub(super) fn ensure_room_texture_uploaded(
    asset_id: AssetId,
    asset_bytes: &'static [u8],
) -> Option<VramSlot> {
    vram().ensure_room_texture_uploaded(VRAM_LAYOUT, asset_id, asset_bytes)
}

/// Upload a UI texture, stepping the upload queue so menu images resolve
/// within the calling frame.
pub(super) fn ensure_ui_texture_uploaded(
    asset_id: AssetId,
    asset_bytes: &'static [u8],
) -> Option<VramSlot> {
    vram().ensure_ui_texture_uploaded(VRAM_LAYOUT, asset_id, asset_bytes)
}

/// Queue `asset_bytes` for upload with an explicit CLUT stamping mode.
pub(super) fn ensure_texture_uploaded_with_clut_mode(
    asset_id: AssetId,
    asset_bytes: &'static [u8],
    clut_mode: VramSlotClutMode,
) -> Option<VramSlot> {
    vram().ensure_texture_uploaded_with_clut_mode(VRAM_LAYOUT, asset_id, asset_bytes, clut_mode)
}

/// Resolve (or begin uploading) a prop texture's transparent-zero slot.
pub(super) fn prop_texture_slot(texture_asset: AssetId) -> Option<VramSlot> {
    vram().prop_texture_slot(VRAM_LAYOUT, ASSETS, texture_asset)
}

/// True once every image/box prop texture of `room` is VRAM-resident.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn room_prop_textures_ready(room: RoomIndex) -> bool {
    vram().room_prop_textures_ready(VRAM_LAYOUT, ASSETS, IMAGE_PROPS, BOX_PROPS, room)
}

/// Upload the streamed sky panorama synchronously from `asset_bytes`.
pub(super) fn ensure_sky_panorama_uploaded(
    asset_id: AssetId,
    asset_bytes: &[u8],
) -> Option<VramSlot> {
    vram().ensure_sky_panorama_uploaded(asset_id, asset_bytes)
}

/// Load the CD-streamed sky panorama into VRAM on gameplay entry, staged
/// through the shared `FONT_PACK_SCRATCH` (free during gameplay; see the
/// crate method's doc and the `SKY_STAGE_WORDS` const assert above).
#[cfg(feature = "cd-stream-bench")]
pub(super) fn load_streamed_sky_from_cd() {
    let scratch = unsafe { &mut *core::ptr::addr_of_mut!(FONT_PACK_SCRATCH) };
    vram().load_streamed_sky_from_cd(
        scratch,
        GAMEPLAY_PACK_MAX_CHUNK_BYTES,
        ASSETS,
        ROOMS,
        UI_PACK_START_LBA,
        UI_PACK_TOC,
    );
}

/// Free the streamed sky panorama's VRAM on gameplay exit.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn release_streamed_sky() {
    vram().release_streamed_sky();
}

/// Upload an 8bpp model atlas to the dedicated model VRAM region.
pub(super) fn ensure_model_atlas_uploaded(
    asset_id: AssetId,
    asset_bytes: &[u8],
) -> Option<VramSlot> {
    vram().ensure_model_atlas_uploaded(VRAM_LAYOUT, asset_id, asset_bytes)
}
