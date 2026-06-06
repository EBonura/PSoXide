use super::vram_upload::{upload_clut, upload_model_clut};
use super::vram_upload_queue::*;
use super::*;
use psx_font::{upload_fonts, FontAtlas, FontSetVram};
use psx_vram::{
    upload_bytes, Clut, TexDepth, Tpage, VramAllocator, VramHandle, VramRect, VramRegionSource,
};

/// Scratch for packing every UI font into one combined atlas before a single
/// `GP0(A0h)` upload. Sized for `MAX_RUNTIME_UI_FONTS` pages × 256-row atlases;
/// lives in BSS, not on the stack.
const FONT_PACK_SCRATCH_LEN: usize = MAX_RUNTIME_UI_FONTS * 64 * 256;
static mut FONT_PACK_SCRATCH: [u16; FONT_PACK_SCRATCH_LEN] = [0; FONT_PACK_SCRATCH_LEN];

/// Staging buffer for one CD-streamed UI image. Reused image-by-image
/// on menu entry: a UI image is read off UI.PAK into this buffer, then
/// uploaded to VRAM (a synchronous DMA), then the buffer is overwritten
/// by the next image. Lives in BSS, not on the stack. Sized to the
/// largest streamed UI chunk so any one image fits.
#[cfg(feature = "cd-stream-bench")]
const UI_STAGE_WORDS: usize = (UI_PACK_MAX_CHUNK_BYTES + 3) / 4;
#[cfg(feature = "cd-stream-bench")]
static mut UI_IMAGE_STAGE: [u32; UI_STAGE_WORDS] = [0; UI_STAGE_WORDS];

/// Streamed UI image VRAM slots created on menu entry, tracked so they
/// can be released on gameplay entry. One entry per streamed Texture
/// asset.
#[cfg(feature = "cd-stream-bench")]
const MAX_UI_IMAGE_SLOTS: usize = 16;
#[cfg(feature = "cd-stream-bench")]
static mut UI_IMAGE_SLOTS: [Option<AssetId>; MAX_UI_IMAGE_SLOTS] =
    [None; MAX_UI_IMAGE_SLOTS];

/// Capacity of the residency manager's RAM table. Holds room
/// world + model meshes + animation clips.
const MAX_RESIDENT_RAM_ASSETS: usize = 128;
/// Capacity of the residency manager's VRAM table. Holds room
/// material atlases + model atlases.
const MAX_RESIDENT_VRAM_ASSETS: usize = 64;

/// Residency manager -- tracks which AssetIds are RAM/VRAM
/// resident across frames. Static so it survives across the
/// `Scene::init` → `Scene::render` boundary.
pub(super) static mut RESIDENCY: ResidencyManager<
    MAX_RESIDENT_RAM_ASSETS,
    MAX_RESIDENT_VRAM_ASSETS,
> = ResidencyManager::new();

/// Per-asset upload bookkeeping. When a texture asset becomes
/// VRAM-resident we record its CLUT word, tpage word, and texture
/// window so the per-frame material build can reconstruct its
/// `TextureMaterial` without re-walking the upload code.
#[derive(Copy, Clone)]
pub(super) struct VramSlot {
    pub(super) asset: AssetId,
    pub(super) clut_mode: VramSlotClutMode,
    pub(super) ready: bool,
    pub(super) clut_word: u16,
    pub(super) tpage_word: u16,
    pub(super) texture_window: TextureWindow,
    pub(super) texture_width: u16,
    pub(super) texture_height: u16,
    /// Allocator handle for the texture window/page this slot owns. `Empty` when
    /// the slot shares another slot's pixels (a clut-only variant) or is a
    /// session-persistent resource (model/sky) freed elsewhere.
    pub(super) region: VramHandle,
    /// Allocator handle for this slot's CLUT. `Empty` if not separately owned.
    pub(super) clut_region: VramHandle,
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub(crate) enum VramSlotClutMode {
    OpaqueZero,
    TransparentZero,
    ModelAtlas,
    SkyPanorama,
}

const VRAM_SLOT_EMPTY: Option<VramSlot> = None;
static mut VRAM_SLOTS: [Option<VramSlot>; MAX_RESIDENT_VRAM_ASSETS] =
    [VRAM_SLOT_EMPTY; MAX_RESIDENT_VRAM_ASSETS];
/// Number of VRAM slots used so far across room textures and model atlases.
static mut VRAM_SLOT_COUNT: usize = 0;
/// Find a free VRAM slot index, reusing holes left by eviction before growing
/// into fresh entries. Returns `None` when the slot table is full.
fn next_vram_slot() -> Option<usize> {
    unsafe { (0..MAX_RESIDENT_VRAM_ASSETS).find(|&i| VRAM_SLOTS[i].is_none()) }
}

/// Release slot `i`'s VRAM (texture window/page + CLUT) to the allocator, drop
/// its residency mark, and clear the slot for reuse. Caller must ensure the slot
/// is `ready`: a slot with a pending upload job must not be freed, or the async
/// writeback would land in a slot that has since been reused.
fn free_vram_slot(i: usize) {
    unsafe {
        if let Some(slot) = VRAM_SLOTS[i].take() {
            VRAM_ALLOCATOR.free(slot.region);
            VRAM_ALLOCATOR.free(slot.clut_region);
            let _ = RESIDENCY.mark_vram_evicted(slot.asset);
            VRAM_SLOT_COUNT = VRAM_SLOT_COUNT.saturating_sub(1);
            telemetry::counter(telemetry::counter::VRAM_SLOTS_FREED, 1);
        }
    }
}

/// Free the room-texture VRAM slot a streamed UI image occupies, if any.
/// Tries both 4bpp clut modes since a UI image's transparency flag decides
/// which mode `ensure_ui_texture_uploaded` picked.
fn free_room_texture_vram_slot(asset_id: AssetId) {
    unsafe {
        for i in 0..MAX_RESIDENT_VRAM_ASSETS {
            if let Some(slot) = VRAM_SLOTS[i] {
                if slot.asset == asset_id
                    && matches!(
                        slot.clut_mode,
                        VramSlotClutMode::OpaqueZero | VramSlotClutMode::TransparentZero
                    )
                {
                    free_vram_slot(i);
                }
            }
        }
    }
}

/// Load every CD-streamed UI image into VRAM, one image at a time.
///
/// Each streamed image (a `Texture` asset with empty baked bytes) is read
/// off UI.PAK into the shared `UI_IMAGE_STAGE` buffer, uploaded to VRAM,
/// and the upload drained to completion BEFORE the next image overwrites
/// the buffer. The `&'static` slice handed to the uploader is sound only
/// because the buffer is `static mut` and fully consumed before the next
/// read mutates it. Tracks each created slot so `release_ui_images` can
/// free it on gameplay entry.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn load_ui_images_from_cd() {
    unsafe {
        let mut slot_count = 0usize;
        for asset in ASSETS {
            if asset.kind != AssetKind::Texture || !asset.bytes.is_empty() {
                continue;
            }
            if slot_count >= MAX_UI_IMAGE_SLOTS {
                break;
            }
            // Skip if already uploaded (idempotent re-entry).
            if find_room_texture_vram_slot(asset.id).is_some() {
                continue;
            }

            let res = cd_stream::read_ui_chunk_blocking(
                UI_PACK_START_LBA,
                UI_PACK_TOC,
                asset.id.0 as u32,
                &mut *core::ptr::addr_of_mut!(UI_IMAGE_STAGE),
            );
            if res.status != cd_stream::ROOM_CHUNK_STATUS_OK || res.bytes == 0 {
                continue;
            }

            // The staged bytes are valid until the next read overwrites the
            // buffer; the upload below consumes them synchronously first.
            let bytes: &'static [u8] = core::slice::from_raw_parts(
                core::ptr::addr_of!(UI_IMAGE_STAGE) as *const u8,
                res.bytes,
            );
            if ensure_ui_texture_uploaded(asset.id, bytes).is_none() {
                // Drain in case a job was queued but not yet completed, then
                // retry the lookup before giving up on this image.
                drain_ui_upload_queue();
            }

            // Make sure this asset's upload has fully drained before the next
            // iteration reuses the staging buffer.
            drain_ui_upload_queue();

            if find_room_texture_vram_slot(asset.id).is_some() {
                UI_IMAGE_SLOTS[slot_count] = Some(asset.id);
                slot_count += 1;
            }
        }
    }
}

/// Run the VRAM upload queue to idle so the shared UI staging buffer can be
/// safely overwritten. Bounded so a stuck job can't hang the loader.
#[cfg(feature = "cd-stream-bench")]
fn drain_ui_upload_queue() {
    unsafe {
        let mut steps = 0u32;
        while !VRAM_UPLOAD_QUEUE.is_idle() && steps < 4096 {
            VRAM_UPLOAD_QUEUE.step(ROOM_TILE_TEXELS, mark_vram_slot_ready);
            steps += 1;
        }
    }
}

/// Free every streamed UI image VRAM slot created by `load_ui_images_from_cd`.
/// Called on gameplay entry so the room textures reclaim that VRAM. Fonts are
/// shared and are NOT released here.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn release_ui_images() {
    unsafe {
        for entry in UI_IMAGE_SLOTS.iter_mut() {
            if let Some(asset_id) = entry.take() {
                free_room_texture_vram_slot(asset_id);
            }
        }
    }
}

/// Current room at the last eviction pass. Eviction only runs when the streamed
/// residency set shifts (the player crosses into a new room), keeping it off the
/// per-frame path.
pub(super) static mut LAST_EVICT_ROOM: RoomIndex = INVALID_ROOM_INDEX;

/// True if any of the `count` desired rooms lists `asset` in its required VRAM set.
fn vram_asset_required(
    asset: AssetId,
    desired: &[RoomIndex; STREAMED_ROOM_SLOT_COUNT],
    count: usize,
) -> bool {
    for &room in desired.iter().take(count) {
        if room == INVALID_ROOM_INDEX {
            continue;
        }
        if let Some(res) = ROOM_RESIDENCY.iter().find(|r| r.room == room) {
            if res.required_vram.iter().any(|&a| a == asset) {
                return true;
            }
        }
    }
    false
}

/// Free room-texture VRAM slots that no desired room still requires, returning
/// their window/CLUT to the allocator. Model atlases and the sky persist for the
/// session; only `ready` slots are freed so a pending upload's async writeback
/// cannot land in a slot that has since been reused.
pub(super) fn evict_unreferenced_vram(
    desired: &[RoomIndex; STREAMED_ROOM_SLOT_COUNT],
    count: usize,
) {
    for i in 0..MAX_RESIDENT_VRAM_ASSETS {
        let slot = match unsafe { VRAM_SLOTS[i] } {
            Some(s) if s.ready => s,
            _ => continue,
        };
        if !matches!(
            slot.clut_mode,
            VramSlotClutMode::OpaqueZero | VramSlotClutMode::TransparentZero
        ) {
            continue;
        }
        if !vram_asset_required(slot.asset, desired, count) {
            free_vram_slot(i);
        }
    }
}

/// CLUT-band rows the unified VRAM allocator manages, just past the back
/// buffer (Stage 1: only the shared font CLUT lands here).
const VRAM_CLUT_ROWS: usize = 16;
/// First VRAM row of the managed CLUT band.
const VRAM_CLUT_BASE_Y: u16 = 480;
/// The single owner of VRAM. Stage 1 routes fonts through it; later stages
/// fold in room textures, models, sky, shadow and particle.
static mut VRAM_ALLOCATOR: VramAllocator<ROOM_TPAGE_COUNT, VRAM_CLUT_ROWS> =
    VramAllocator::new(VRAM_CLUT_BASE_Y);
/// Set once the still-hardcoded VRAM regions are reserved in `VRAM_ALLOCATOR`.
static mut VRAM_REGIONS_RESERVED: bool = false;
/// Handles for the combined UI-font upload, kept for teardown.
static mut VRAM_FONT_SET: Option<FontSetVram> = None;

/// Reserve the framebuffer and every region still owned by legacy hardcoded
/// uploads, so the allocator places fonts in the remaining free VRAM without
/// collision. Each reserved region migrates into managed allocation in a later
/// stage.
fn reserve_static_vram_regions(alloc: &mut VramAllocator<ROOM_TPAGE_COUNT, VRAM_CLUT_ROWS>) {
    // Double-buffered framebuffer.
    alloc.reserve_rect(VramRect::new(0, 0, 320, 480));
    // Room-material window band (allocated via the unified allocator).
    alloc.reserve_room_band(ROOM_TPAGE_BASE_X, 0);
    // Column between the framebuffer and the model-atlas region. Model atlases,
    // the sky panorama, and shadow/particle decals are all allocated dynamically
    // (rows 256 and 0); reserving this gap keeps model atlases at their historical
    // x=384 base.
    alloc.reserve_rect(VramRect::new(
        320,
        MODEL_TPAGE.y(),
        MODEL_TPAGE.x() - 320,
        256,
    ));
}

pub(super) fn acquire_shared_ui_fonts(ui_fonts: &mut [Option<FontAtlas>; MAX_RUNTIME_UI_FONTS]) {
    unsafe {
        if !VRAM_REGIONS_RESERVED {
            reserve_static_vram_regions(&mut VRAM_ALLOCATOR);
            VRAM_REGIONS_RESERVED = true;
        }
        if ui_fonts[0].is_none() && !UI_FONTS.is_empty() {
            VRAM_FONT_SET = upload_fonts(
                UI_FONTS,
                &mut VRAM_ALLOCATOR,
                &mut FONT_PACK_SCRATCH,
                ui_fonts,
            );
        }
    }
}

/// Kept for completeness even though the current flow never releases the shared
/// UI fonts (they serve both menu and gameplay HUD): a future teardown path can
/// call this to free the font VRAM and clear the atlases for a clean re-acquire.
#[allow(dead_code)]
pub(super) fn release_shared_ui_fonts(ui_fonts: &mut [Option<FontAtlas>; MAX_RUNTIME_UI_FONTS]) {
    unsafe {
        if let Some(set) = VRAM_FONT_SET.take() {
            VRAM_ALLOCATOR.free(set.pages);
            VRAM_ALLOCATOR.free(set.clut);
            *ui_fonts = [const { None }; MAX_RUNTIME_UI_FONTS];
        }
    }
}

const VRAM_UPLOAD_ROWS_PER_BACKGROUND_TICK: u16 = 8;
const UI_TEXTURE_UPLOAD_ROW_BUDGET: u16 = ROOM_TILE_TEXELS;
const UI_TEXTURE_UPLOAD_MAX_STEPS: u8 = 8;
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
        unsafe { VRAM_UPLOAD_QUEUE.step(self.vram_rows_per_tick, mark_vram_slot_ready) }
    }

    pub(super) fn vram_uploads_idle(self) -> bool {
        unsafe { VRAM_UPLOAD_QUEUE.is_idle() }
    }
}

/// Sky panorama placement, filled by `ensure_sky_panorama_uploaded` from the
/// unified allocator: two contiguous 4bpp page words + one CLUT word per band.
static mut SKY_PAGE_TPAGE_WORDS: [u16; 2] = [0; 2];
static mut SKY_CLUT_WORDS: [u16; SKY_PANORAMA_PALETTE_BANDS] = [0; SKY_PANORAMA_PALETTE_BANDS];

pub(super) fn sky_panorama_tpage_word(page: usize) -> u16 {
    unsafe { SKY_PAGE_TPAGE_WORDS[page.min(1)] }
}

pub(super) fn sky_panorama_clut_word(band: usize) -> u16 {
    unsafe { SKY_CLUT_WORDS[band.min(SKY_PANORAMA_PALETTE_BANDS - 1)] }
}

pub(super) fn vram_slot_texture_size_u8(size: u16) -> u8 {
    size.min(u16::from(u8::MAX)) as u8
}

/// Shadow and particle decals share one 4bpp page (shadow at U=64, particle at
/// U=0). Allocated once from the unified allocator on first decal upload.
static mut SHADOW_PARTICLE_PAGE: Option<Tpage> = None;

fn shadow_particle_page() -> Option<Tpage> {
    unsafe {
        if SHADOW_PARTICLE_PAGE.is_none() {
            let (tpage, _region) = VRAM_ALLOCATOR.alloc_page_run(1, TexDepth::Bit4, 0)?;
            SHADOW_PARTICLE_PAGE = Some(tpage);
        }
        SHADOW_PARTICLE_PAGE
    }
}

pub(super) fn upload_shadow_texture() -> Option<TextureMaterial> {
    let texture = Texture::from_bytes(SHADOW_CIRCLE_BLOB).ok()?;
    if texture.width() != 64 || texture.height() != 64 || texture.clut_entries() != 16 {
        return None;
    }

    let page = shadow_particle_page()?;
    let (clut, _clut_region) = unsafe { VRAM_ALLOCATOR.alloc_clut(texture.clut_entries())? };
    upload_bytes(
        VramRect::new(
            page.x() + u16::from(SHADOW_TEXEL_U) / 4,
            page.y(),
            texture.halfwords_per_row(),
            texture.height(),
        ),
        texture.pixel_bytes(),
    );
    upload_clut(
        VramRect::new(clut.x(), clut.y(), texture.clut_entries(), 1),
        texture.clut_bytes(),
    );

    Some(
        TextureMaterial::blended(
            clut.uv_clut_word(),
            page.uv_tpage_word(0),
            (0x80, 0x80, 0x80),
            BlendMode::Average,
        )
        .with_raw_texture(true),
    )
}

pub(super) fn upload_particle_texture() -> Option<TextureMaterial> {
    let mut pixels =
        [0u8; (PARTICLE_TEXTURE_HALFWORDS_PER_ROW as usize) * (PARTICLE_TEXTURE_SIZE as usize) * 2];
    let mut row = 0usize;
    while row < PARTICLE_TEXTURE_SIZE as usize {
        let mut col = 0usize;
        while col < PARTICLE_TEXTURE_SIZE as usize {
            let dx = (col as i32 * 2 + 1) - PARTICLE_TEXTURE_SIZE as i32;
            let dy = (row as i32 * 2 + 1) - PARTICLE_TEXTURE_SIZE as i32;
            let inside = dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy)) <= 225;
            if inside {
                let halfword = row * PARTICLE_TEXTURE_HALFWORDS_PER_ROW as usize + (col / 4);
                let shift = (col & 3) * 4;
                let raw = u16::from_le_bytes([pixels[halfword * 2], pixels[halfword * 2 + 1]])
                    | (1u16 << shift);
                let packed = raw.to_le_bytes();
                pixels[halfword * 2] = packed[0];
                pixels[halfword * 2 + 1] = packed[1];
            }
            col += 1;
        }
        row += 1;
    }

    let mut clut = [0u8; 32];
    let white = 0x7FFFu16.to_le_bytes();
    clut[2] = white[0];
    clut[3] = white[1];

    let page = shadow_particle_page()?;
    let (clut_pos, _clut_region) = unsafe { VRAM_ALLOCATOR.alloc_clut(16)? };
    upload_bytes(
        VramRect::new(
            page.x() + u16::from(PARTICLE_TEXEL_U) / 4,
            page.y(),
            PARTICLE_TEXTURE_HALFWORDS_PER_ROW,
            PARTICLE_TEXTURE_SIZE,
        ),
        &pixels,
    );
    upload_clut(VramRect::new(clut_pos.x(), clut_pos.y(), 16, 1), &clut);

    Some(TextureMaterial::blended(
        clut_pos.uv_clut_word(),
        page.uv_tpage_word(0),
        (0x80, 0x80, 0x80),
        BlendMode::Average,
    ))
}

/// Upload `asset_bytes` to VRAM if not already resident; return
/// the slot record so the caller can build a TextureMaterial.
/// Returns `None` if the texture parse fails or the VRAM table
/// is full.
/// Look up the VRAM slot a previously-uploaded asset occupies.
/// VRAM_SLOTS is the source of truth -- `RESIDENCY` only tracks
/// the *contract*, which is pre-marked by `ensure_room_resident`
/// before any actual upload runs.
fn find_vram_slot(asset_id: AssetId, clut_mode: VramSlotClutMode) -> Option<VramSlot> {
    unsafe {
        VRAM_SLOTS
            .iter()
            .filter_map(|s| *s)
            .find(|s| s.ready && s.asset == asset_id && s.clut_mode == clut_mode)
    }
}

pub(super) fn find_room_texture_vram_slot(asset_id: AssetId) -> Option<VramSlot> {
    unsafe {
        VRAM_SLOTS.iter().filter_map(|s| *s).find(|s| {
            s.ready
                && s.asset == asset_id
                && matches!(
                    s.clut_mode,
                    VramSlotClutMode::OpaqueZero | VramSlotClutMode::TransparentZero
                )
        })
    }
}

fn pending_vram_upload(asset_id: AssetId, clut_mode: VramSlotClutMode) -> bool {
    unsafe {
        VRAM_SLOTS
            .iter()
            .filter_map(|s| *s)
            .any(|s| !s.ready && s.asset == asset_id && s.clut_mode == clut_mode)
            || VRAM_UPLOAD_QUEUE.contains(asset_id, clut_mode)
    }
}

fn pending_room_texture_upload(asset_id: AssetId) -> bool {
    unsafe {
        VRAM_SLOTS.iter().filter_map(|s| *s).any(|s| {
            !s.ready
                && s.asset == asset_id
                && matches!(
                    s.clut_mode,
                    VramSlotClutMode::OpaqueZero | VramSlotClutMode::TransparentZero
                )
        })
    }
}

fn mark_vram_slot_ready(index: usize) {
    unsafe {
        let Some(mut slot) = VRAM_SLOTS.get(index).copied().flatten() else {
            return;
        };
        slot.ready = true;
        VRAM_SLOTS[index] = Some(slot);
        let _ = RESIDENCY.mark_vram_resident(slot.asset);
    }
}

pub(super) fn ensure_texture_uploaded(
    asset_id: AssetId,
    asset_bytes: &'static [u8],
) -> Option<VramSlot> {
    let texture = Texture::from_bytes(asset_bytes).ok()?;
    let clut_mode = if texture.index_zero_transparent() {
        VramSlotClutMode::TransparentZero
    } else {
        VramSlotClutMode::OpaqueZero
    };
    ensure_texture_uploaded_with_clut_mode(asset_id, asset_bytes, clut_mode)
}

pub(super) fn ensure_ui_texture_uploaded(
    asset_id: AssetId,
    asset_bytes: &'static [u8],
) -> Option<VramSlot> {
    let texture = Texture::from_bytes(asset_bytes).ok()?;
    let clut_mode = if texture.index_zero_transparent() {
        VramSlotClutMode::TransparentZero
    } else {
        VramSlotClutMode::OpaqueZero
    };
    if let Some(slot) = find_vram_slot(asset_id, clut_mode) {
        return Some(slot);
    }

    let use_large_ui_upload = texture.width() > ROOM_TILE_TEXELS
        || texture.height() > ROOM_TILE_TEXELS
        || room_texture_window_size(texture.width()).is_none()
        || room_texture_window_size(texture.height()).is_none();
    let _ = if use_large_ui_upload {
        ensure_large_ui_texture_uploaded_with_clut_mode(asset_id, asset_bytes, clut_mode)
    } else {
        ensure_texture_uploaded_with_clut_mode(asset_id, asset_bytes, clut_mode)
    };
    let mut steps = 0u8;
    while pending_vram_upload(asset_id, clut_mode) && steps < UI_TEXTURE_UPLOAD_MAX_STEPS {
        unsafe {
            VRAM_UPLOAD_QUEUE.step(UI_TEXTURE_UPLOAD_ROW_BUDGET, mark_vram_slot_ready);
        }
        steps = steps.saturating_add(1);
    }

    find_vram_slot(asset_id, clut_mode)
}

fn ensure_large_ui_texture_uploaded_with_clut_mode(
    asset_id: AssetId,
    asset_bytes: &'static [u8],
    clut_mode: VramSlotClutMode,
) -> Option<VramSlot> {
    if let Some(slot) = find_vram_slot(asset_id, clut_mode) {
        return Some(slot);
    }
    if pending_vram_upload(asset_id, clut_mode) {
        return None;
    }
    if pending_room_texture_upload(asset_id) {
        return None;
    }
    unsafe {
        if !VRAM_UPLOAD_QUEUE.has_free_slot() {
            return None;
        }
    }

    let texture = Texture::from_bytes(asset_bytes).ok()?;
    if texture.clut_entries() != 16
        || texture.width() == 0
        || texture.width() > 256
        || texture.height() == 0
        || texture.height() > 256
    {
        return None;
    }
    let texture_width_halfwords = texture.halfwords_per_row();
    if texture_width_halfwords > ROOM_TPAGE_STRIDE_HW {
        return None;
    }
    let expected_pixel_bytes = usize::from(texture_width_halfwords)
        .saturating_mul(usize::from(texture.height()))
        .saturating_mul(2);
    if texture.pixel_bytes().len() != expected_pixel_bytes {
        return None;
    }

    let idx = next_vram_slot()?;
    let (tpage, region) = unsafe { VRAM_ALLOCATOR.alloc_room_page()? };
    let tpage_x = tpage.x();
    let (clut, clut_region) = unsafe { VRAM_ALLOCATOR.alloc_clut(texture.clut_entries())? };
    let slot = VramSlot {
        asset: asset_id,
        clut_mode,
        ready: false,
        clut_word: clut.uv_clut_word(),
        tpage_word: tpage.uv_tpage_word(0),
        texture_window: TextureWindow::NONE,
        texture_width: texture.width(),
        texture_height: texture.height(),
        region,
        clut_region,
    };

    unsafe {
        VRAM_SLOTS[idx] = Some(slot);
        VRAM_SLOT_COUNT += 1;
        if !VRAM_UPLOAD_QUEUE.push(VramUploadJob {
            active: true,
            slot_index: idx as u16,
            asset: asset_id,
            clut_mode,
            kind: VramUploadKind::TextureAndClut,
            bytes: Some(asset_bytes),
            texture_x: tpage_x,
            texture_y: SHARED_TPAGE.y(),
            texture_width_halfwords,
            texture_height_rows: texture.height(),
            next_texture_row: 0,
            clut_x: clut.x(),
            clut_y: clut.y(),
            clut_entries: texture.clut_entries(),
            clut_uploaded: false,
        }) {
            VRAM_SLOTS[idx] = None;
            VRAM_SLOT_COUNT -= 1;
            return None;
        }
    }

    None
}

pub(super) fn ensure_texture_uploaded_with_clut_mode(
    asset_id: AssetId,
    asset_bytes: &'static [u8],
    clut_mode: VramSlotClutMode,
) -> Option<VramSlot> {
    // VRAM_SLOTS is the source of truth for "have we actually
    // uploaded this asset". `RESIDENCY` is the *contract* -- it's
    // pre-marked by `ensure_room_resident` before any upload runs,
    // so reading it here would falsely report assets as uploaded
    // and skip the upload entirely.
    if let Some(slot) = find_vram_slot(asset_id, clut_mode) {
        return Some(slot);
    }
    if pending_vram_upload(asset_id, clut_mode) {
        return None;
    }
    if pending_room_texture_upload(asset_id) {
        return None;
    }
    unsafe {
        if !VRAM_UPLOAD_QUEUE.has_free_slot() {
            return None;
        }
    }

    let texture = Texture::from_bytes(asset_bytes).ok()?;
    if texture.clut_entries() != 16 {
        return None;
    }

    // Capacity check before we touch any VRAM state.
    let idx = next_vram_slot()?;

    if texture.width() > ROOM_TILE_TEXELS || texture.height() > ROOM_TILE_TEXELS {
        return None;
    }

    let texture_width = room_texture_window_size(texture.width())?;
    let texture_height = room_texture_window_size(texture.height())?;
    let texture_width_halfwords = u16::from(texture_width) / 4;
    let texture_height_rows = u16::from(texture_height);
    if texture.halfwords_per_row() > texture_width_halfwords
        || texture.height() > texture_height_rows
    {
        return None;
    }
    let src_bytes = texture.pixel_bytes();
    let src_len = (texture.halfwords_per_row() as usize)
        .saturating_mul(texture.height() as usize)
        .saturating_mul(2);
    if src_bytes.len() != src_len {
        return None;
    }

    if let Some(shared_texture) = find_room_texture_vram_slot(asset_id) {
        let (clut, clut_region) = unsafe { VRAM_ALLOCATOR.alloc_clut(texture.clut_entries())? };
        let slot = VramSlot {
            asset: asset_id,
            clut_mode,
            ready: false,
            clut_word: clut.uv_clut_word(),
            tpage_word: shared_texture.tpage_word,
            texture_window: shared_texture.texture_window,
            texture_width: shared_texture.texture_width,
            texture_height: shared_texture.texture_height,
            // Shared pixels are owned by the original slot; this variant only
            // owns its own CLUT.
            region: VramHandle::Empty,
            clut_region,
        };
        unsafe {
            VRAM_SLOTS[idx] = Some(slot);
            VRAM_SLOT_COUNT += 1;
            if !VRAM_UPLOAD_QUEUE.push(VramUploadJob {
                active: true,
                slot_index: idx as u16,
                asset: asset_id,
                clut_mode,
                kind: VramUploadKind::ClutOnly,
                bytes: Some(asset_bytes),
                texture_x: 0,
                texture_y: 0,
                texture_width_halfwords: 0,
                texture_height_rows: 0,
                next_texture_row: 0,
                clut_x: clut.x(),
                clut_y: clut.y(),
                clut_entries: texture.clut_entries(),
                clut_uploaded: false,
            }) {
                VRAM_SLOTS[idx] = None;
                VRAM_SLOT_COUNT -= 1;
                return None;
            }
        }
        return None;
    }

    // Pack room materials on the GP0(E2) 8-texel grid inside 4bpp
    // tpages. A 32x32 texture now consumes a 32x32 window instead of
    // burning a whole old 64x64 cell.
    let (tpage, placement, region) = unsafe {
        VRAM_ALLOCATOR.alloc_window(u16::from(texture_width), u16::from(texture_height))?
    };
    let tpage_x = tpage.x();
    let texture_x = tpage_x.checked_add(u16::from(placement.origin_u()) / 4)?;
    let texture_y = SHARED_TPAGE
        .y()
        .checked_add(u16::from(placement.origin_v()))?;

    let (clut, clut_region) = unsafe { VRAM_ALLOCATOR.alloc_clut(texture.clut_entries())? };
    let slot = VramSlot {
        asset: asset_id,
        clut_mode,
        ready: false,
        clut_word: clut.uv_clut_word(),
        tpage_word: tpage.uv_tpage_word(0),
        texture_window: TextureWindow::power_of_two_tile(
            placement.origin_u(),
            placement.origin_v(),
            texture_width,
            texture_height,
        ),
        texture_width: u16::from(texture_width),
        texture_height: u16::from(texture_height),
        region,
        clut_region,
    };

    unsafe {
        VRAM_SLOTS[idx] = Some(slot);
        VRAM_SLOT_COUNT += 1;
        if !VRAM_UPLOAD_QUEUE.push(VramUploadJob {
            active: true,
            slot_index: idx as u16,
            asset: asset_id,
            clut_mode,
            kind: VramUploadKind::TextureAndClut,
            bytes: Some(asset_bytes),
            texture_x,
            texture_y,
            texture_width_halfwords,
            texture_height_rows,
            next_texture_row: 0,
            clut_x: clut.x(),
            clut_y: clut.y(),
            clut_entries: texture.clut_entries(),
            clut_uploaded: false,
        }) {
            VRAM_SLOTS[idx] = None;
            VRAM_SLOT_COUNT -= 1;
            return None;
        }
    }

    None
}

pub(super) fn prop_texture_slot(texture_asset: AssetId) -> Option<VramSlot> {
    let clut_mode = VramSlotClutMode::TransparentZero;
    if let Some(slot) = find_vram_slot(texture_asset, clut_mode) {
        return Some(slot);
    }
    let asset = find_asset_of_kind(ASSETS, texture_asset, AssetKind::Texture)?;
    ensure_texture_uploaded_with_clut_mode(asset.id, asset.bytes, clut_mode)
}

#[cfg(feature = "cd-stream-bench")]
pub(super) fn room_prop_textures_ready(room: RoomIndex) -> bool {
    let mut ready = true;

    for prop in IMAGE_PROPS {
        if prop.room == room && prop_texture_slot(prop.texture_asset).is_none() {
            ready = false;
        }
    }

    for prop in BOX_PROPS {
        if prop.room != room {
            continue;
        }
        let mut face = 0usize;
        while face < psx_level::BOX_PROP_FACE_COUNT {
            if let Some(texture_asset) = prop.texture_assets[face] {
                if prop_texture_slot(texture_asset).is_none() {
                    ready = false;
                }
            }
            face += 1;
        }
    }

    ready
}

fn room_texture_window_size(size: u16) -> Option<u8> {
    if size < 8 || size > ROOM_TILE_TEXELS || !size.is_power_of_two() || size % 8 != 0 {
        return None;
    }
    u8::try_from(size).ok()
}

pub(super) fn ensure_sky_panorama_uploaded(
    asset_id: AssetId,
    asset_bytes: &[u8],
) -> Option<VramSlot> {
    if let Some(slot) = find_vram_slot(asset_id, VramSlotClutMode::SkyPanorama) {
        return Some(slot);
    }
    let texture = Texture::from_bytes(asset_bytes).ok()?;
    if texture.clut_entries() != SKY_PANORAMA_CLUT_ENTRIES * SKY_PANORAMA_PALETTE_BANDS as u16
        || texture.width() != SKY_PANORAMA_WIDTH
        || texture.height() != SKY_PANORAMA_HEIGHT
        || texture.halfwords_per_row() != SKY_PANORAMA_WIDTH / 4
    {
        return None;
    }
    let idx = next_vram_slot()?;
    let expected_pixel_bytes = (texture.halfwords_per_row() as usize)
        .saturating_mul(texture.height() as usize)
        .saturating_mul(2);
    if texture.pixel_bytes().len() != expected_pixel_bytes {
        return None;
    }

    let clut_row_bytes = usize::from(SKY_PANORAMA_CLUT_ENTRIES) * 2;
    if texture.clut_bytes().len() != clut_row_bytes * SKY_PANORAMA_PALETTE_BANDS {
        return None;
    }
    // Two contiguous 4bpp pages (the 512-texel panorama) + one CLUT per band,
    // all from the unified allocator.
    let (left_tpage, _page_region) =
        unsafe { VRAM_ALLOCATOR.alloc_page_run(2, TexDepth::Bit4, 256)? };
    let right_tpage = Tpage::new(left_tpage.x() + 64, left_tpage.y(), TexDepth::Bit4);
    let mut sky_cluts = [Clut::new(0, 0); SKY_PANORAMA_PALETTE_BANDS];
    for dst in sky_cluts.iter_mut() {
        let (clut, _clut_region) = unsafe { VRAM_ALLOCATOR.alloc_clut(SKY_PANORAMA_CLUT_ENTRIES)? };
        *dst = clut;
    }
    unsafe {
        SKY_PAGE_TPAGE_WORDS = [left_tpage.uv_tpage_word(0), right_tpage.uv_tpage_word(0)];
        for (band, clut) in sky_cluts.iter().enumerate() {
            SKY_CLUT_WORDS[band] = clut.uv_clut_word();
        }
    }

    telemetry::stage_begin(telemetry::stage::VRAM_UPLOAD);
    telemetry::counter(telemetry::counter::ROOM_TEXTURE_UPLOADS, 1);
    upload_bytes(
        VramRect::new(
            left_tpage.x(),
            left_tpage.y(),
            texture.halfwords_per_row(),
            texture.height(),
        ),
        texture.pixel_bytes(),
    );
    for (band, clut) in sky_cluts.iter().enumerate() {
        let offset = band * clut_row_bytes;
        upload_model_clut(
            VramRect::new(clut.x(), clut.y(), SKY_PANORAMA_CLUT_ENTRIES, 1),
            &texture.clut_bytes()[offset..offset + clut_row_bytes],
            texture.index_zero_transparent(),
        );
    }
    telemetry::stage_end(telemetry::stage::VRAM_UPLOAD);

    let slot = VramSlot {
        asset: asset_id,
        clut_mode: VramSlotClutMode::SkyPanorama,
        ready: true,
        clut_word: sky_panorama_clut_word(0),
        tpage_word: sky_panorama_tpage_word(0),
        texture_window: TextureWindow::NONE,
        texture_width: texture.width(),
        texture_height: texture.height(),
        // Sky is session-persistent (2 pages + 8 CLUTs); not freed via slot eviction.
        region: VramHandle::Empty,
        clut_region: VramHandle::Empty,
    };
    unsafe {
        VRAM_SLOTS[idx] = Some(slot);
        VRAM_SLOT_COUNT += 1;
        let _ = RESIDENCY.mark_vram_resident(asset_id);
    }
    Some(slot)
}

/// Upload an 8bpp model atlas to the dedicated model VRAM
/// region. Returns a `VramSlot` carrying the 8bpp tpage word
/// and the atlas's CLUT word. Reuses an existing slot when the
/// asset's already resident.
///
/// Caller is responsible for confirming `asset_bytes` parses as
/// a `Texture` whose CLUT carries 256 entries (8bpp). Anything
/// else returns `None`.
pub(super) fn ensure_model_atlas_uploaded(
    asset_id: AssetId,
    asset_bytes: &[u8],
) -> Option<VramSlot> {
    // Same caveat as `ensure_texture_uploaded`: VRAM_SLOTS is
    // the source of truth, not the residency tracker.
    if let Some(slot) = find_vram_slot(asset_id, VramSlotClutMode::ModelAtlas) {
        return Some(slot);
    }
    let texture = Texture::from_bytes(asset_bytes).ok()?;
    if texture.clut_entries() != 256 {
        // Only 8bpp atlases supported -- 4bpp model atlases
        // would round-trip through `ensure_texture_uploaded`.
        return None;
    }

    let idx = next_vram_slot()?;
    let texture_width = texture.width();
    let texture_height = texture.height();
    let texture_halfwords_per_row = texture.halfwords_per_row();
    if texture_width == 0
        || texture_width > 256
        || texture_height == 0
        || texture_height > 256
        || texture_halfwords_per_row > MODEL_TPAGE_MAX_HALFWORDS
    {
        return None;
    }
    let expected_pixel_bytes = (texture_halfwords_per_row as usize)
        .saturating_mul(texture_height as usize)
        .saturating_mul(2);
    if texture.pixel_bytes().len() != expected_pixel_bytes {
        return None;
    }

    // Placement comes only from the unified allocator: an 8bpp page run at row 256
    // plus a 256-entry CLUT (16 contiguous 16-px slots in the managed band).
    let (tpage, region) = unsafe { VRAM_ALLOCATOR.alloc_model_slot(texture_halfwords_per_row)? };
    let (clut, clut_region) = unsafe { VRAM_ALLOCATOR.alloc_clut(texture.clut_entries())? };
    telemetry::stage_begin(telemetry::stage::VRAM_UPLOAD);
    telemetry::counter(telemetry::counter::MODEL_ATLAS_UPLOADS, 1);
    let pix_rect = VramRect::new(
        tpage.x(),
        tpage.y(),
        texture_halfwords_per_row,
        texture_height,
    );
    upload_bytes(pix_rect, texture.pixel_bytes());

    let clut_rect = VramRect::new(clut.x(), clut.y(), texture.clut_entries(), 1);
    upload_model_clut(
        clut_rect,
        texture.clut_bytes(),
        texture.index_zero_transparent(),
    );
    telemetry::stage_end(telemetry::stage::VRAM_UPLOAD);

    let slot = VramSlot {
        asset: asset_id,
        clut_mode: VramSlotClutMode::ModelAtlas,
        ready: true,
        clut_word: clut.uv_clut_word(),
        tpage_word: tpage.uv_tpage_word(0),
        texture_window: TextureWindow::NONE,
        texture_width,
        texture_height,
        // Model atlases are session-persistent; handles stored but not evicted.
        region,
        clut_region,
    };

    unsafe {
        VRAM_SLOTS[idx] = Some(slot);
        VRAM_SLOT_COUNT += 1;
        let _ = RESIDENCY.mark_vram_resident(asset_id);
    }
    Some(slot)
}
