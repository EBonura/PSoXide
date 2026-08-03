//! VRAM residency and upload runtime, carved out of `editor-playtest`'s
//! `vram_runtime`/`vram_upload*` modules (phase 1, vram_runtime slice of
//! docs/game-runtime-plan.md). [`VramRuntime`] owns the VRAM slot table,
//! the unified allocator, the residency contract tracker, the async
//! upload queue, and the sky/decal placements as one struct with
//! `&mut self` methods; capacities arrive as `const N` generic
//! parameters, cooked tables as psx-level record parameters, and the
//! game's VRAM placement as one [`VramLayout`] value parameter (the
//! PROJECTION-into-`RoomVisibility::rebuild` pattern).
//!
//! The big zero-initialized staging buffers ([`FontPackScratch`] and the
//! streamed-UI [`UiImageCache`]) are separate zero-init types rather
//! than [`VramRuntime`] fields: the PSX-EXE is a flat binary whose
//! `.bss` is NOLOAD, so folding ~220 KB of zeroed buffers into a struct
//! with non-zero initializers would move them into `.data` and store
//! the zeros in the image. The game keeps one static instance of each.

mod upload_queue;

#[cfg(feature = "cd-stream-bench")]
use crate::cd_stream;
use psx_asset::Texture;
use psx_engine::telemetry;
use psx_font::{upload_fonts, BitmapFont, FontAtlas};
use psx_gpu::material::{BlendMode, TextureMaterial, TextureWindow};
#[cfg(feature = "cd-stream-bench")]
use psx_level::{
    asset_flags, LevelArchPropRecord, LevelBoxPropRecord, LevelCylinderPropRecord,
    LevelImagePropRecord, LevelUiNodeKind, LevelUiNodeRecord, LevelUiScene,
    LevelWorldPackEntryRecord, ARCH_PROP_MATERIAL_COUNT, BOX_PROP_FACE_COUNT, UI_SCENE_NONE,
};
use psx_level::{
    find_asset_of_kind, sky_flags, AssetId, AssetKind, LevelAssetRecord, LevelRoomRecord,
    ResidencyChangeSet, ResidencyManager, RoomIndex, RoomResidencyRecord,
};
use psx_vram::{
    upload_bytes, Clut, TexDepth, Tpage, VramAllocator, VramHandle, VramRect, VramRegionSource,
};
use upload_queue::{VramUploadJob, VramUploadKind, VramUploadQueue};

use crate::room_cache::INVALID_ROOM_INDEX;

/// Pixel fonts are short (glyph_h <= 16), so a 128-row atlas cap is
/// generous; the game sizes its [`FontPackScratch`] from this and the
/// streamed sky chunk (the dominant consumer).
pub const FONT_ATLAS_MAX_ROWS: usize = 128;

/// Cooked sky panorama CLUT entries per palette band.
pub const SKY_PANORAMA_CLUT_ENTRIES: u16 = 16;
/// Cooked sky panorama palette band count (one CLUT row per band).
pub const SKY_PANORAMA_PALETTE_BANDS: usize = 8;
/// Cooked sky panorama width in texels (two side-by-side 4bpp pages).
pub const SKY_PANORAMA_WIDTH: u16 = 512;
/// Cooked sky panorama height in texels.
pub const SKY_PANORAMA_HEIGHT: u16 = 256;

/// Shadow decal's texel U origin inside the shared shadow/particle 4bpp
/// page. UVs are page-relative, so only the page base moves.
pub const SHADOW_TEXEL_U: u8 = 64;
/// Particle decals use the U=0 half of the shared shadow/particle 4bpp page.
pub const PARTICLE_TEXEL_U: u8 = 0;
/// Generated particle decal edge in texels.
pub const PARTICLE_TEXTURE_SIZE: u16 = 16;
/// Generated particle decal row width in halfwords (4bpp).
pub const PARTICLE_TEXTURE_HALFWORDS_PER_ROW: u16 = PARTICLE_TEXTURE_SIZE / 4;

/// Streamed UI image VRAM slots created on menu entry, tracked so they
/// can be released on gameplay entry. One entry per streamed Texture
/// asset.
#[cfg(feature = "cd-stream-bench")]
const MAX_UI_IMAGE_SLOTS: usize = 16;

const UI_TEXTURE_UPLOAD_MAX_STEPS: u8 = 8;

/// VRAM placement contract the game passes into every [`VramRuntime`]
/// method that reserves or allocates space (the PROJECTION-parameter
/// pattern): the framebuffer, the room-material page band, the
/// model-atlas region, and the managed CLUT band base row.
#[derive(Copy, Clone)]
pub struct VramLayout {
    /// Double-buffered framebuffer rect, reserved from the allocator.
    pub framebuffer: VramRect,
    /// First VRAM x of the room-material 4bpp page band.
    pub room_tpage_base_x: u16,
    /// Shared room-material tpage (the band's first page).
    pub shared_tpage: Tpage,
    /// Room-material band page stride in halfwords.
    pub room_tpage_stride_hw: u16,
    /// Largest square room texture edge in texels.
    pub room_tile_texels: u16,
    /// Model-atlas indexed-texture region origin page.
    pub model_tpage: Tpage,
    /// Maximum physical halfword width reserved for one model atlas.
    pub model_tpage_max_halfwords: u16,
    /// First VRAM row of the managed CLUT band.
    pub clut_base_y: u16,
}

/// Shared transient load scratch (BSS, not on the stack). At boot it packs
/// every UI font into one combined atlas for a single `GP0(A0h)` upload; during
/// gameplay the fonts are already resident, so the same buffer is reused to
/// stage the CD-streamed sky chunk. The game sizes `LEN` to the larger of the
/// two uses (see [`FONT_ATLAS_MAX_ROWS`]). `repr(align(4))` guarantees the
/// u32-aligned staging view the CD read requires.
#[repr(C, align(4))]
pub struct FontPackScratch<const LEN: usize> {
    words: [u16; LEN],
}

impl<const LEN: usize> FontPackScratch<LEN> {
    /// Zero-initialized scratch; `const` so the game's static instance
    /// stays in `.bss`.
    pub const fn new() -> Self {
        Self { words: [0; LEN] }
    }

    /// The u16 view the font packer fills.
    fn words_mut(&mut self) -> &mut [u16] {
        &mut self.words
    }

    /// u32-aligned staging view of the first `words` u32s, for whole-sector
    /// CD reads. `None` when the request does not fit the buffer.
    #[cfg(feature = "cd-stream-bench")]
    fn stage_words_mut(&mut self, words: usize) -> Option<&mut [u32]> {
        if words.checked_mul(4)? > LEN.saturating_mul(2) {
            return None;
        }
        // SAFETY: `repr(align(4))` guarantees u32 alignment, the range is
        // in-bounds, and both views are plain old data. Both views alias
        // the same dead-after-boot buffer; the read fully consumes it
        // before the upload, and nothing else touches it during gameplay.
        Some(unsafe {
            core::slice::from_raw_parts_mut(self.words.as_mut_ptr().cast::<u32>(), words)
        })
    }

    /// Byte view of the first `bytes` staged bytes. The staged bytes are
    /// valid until the next read overwrites the buffer; consume them
    /// synchronously.
    #[cfg(feature = "cd-stream-bench")]
    fn staged_bytes(&self, bytes: usize) -> Option<&[u8]> {
        if bytes > LEN.saturating_mul(2) {
            return None;
        }
        // SAFETY: in-bounds u16 -> u8 view of plain old data.
        Some(unsafe { core::slice::from_raw_parts(self.words.as_ptr().cast::<u8>(), bytes) })
    }
}

/// One cached CD-streamed menu UI image. The bytes are loaded once from UI.PAK
/// after menu boot, then scene transitions upload their active images from this
/// RAM cache instead of seeking the disc again.
#[cfg(feature = "cd-stream-bench")]
#[derive(Copy, Clone)]
struct UiImageCacheEntry {
    asset: AssetId,
    bytes: usize,
    loaded: bool,
}

#[cfg(feature = "cd-stream-bench")]
impl UiImageCacheEntry {
    // Zero-initialized empty entry so the game's cache static stays in
    // `.bss`: `loaded: false` gates every read of the other fields, so
    // the old `AssetId(u16::MAX)` sentinel value was never observable.
    const EMPTY: Self = Self {
        asset: AssetId(0),
        bytes: 0,
        loaded: false,
    };
}

/// RAM cache for every streamed menu UI image over the game's
/// `(STAGE_WORDS, SLOTS)` budget. Each slot is fixed-width so the CD
/// reader can write directly into it as a u32-aligned destination;
/// zero-init `const` construction keeps the game's static instance in
/// `.bss`.
#[cfg(feature = "cd-stream-bench")]
pub struct UiImageCache<const STAGE_WORDS: usize, const SLOTS: usize> {
    words: [[u32; STAGE_WORDS]; SLOTS],
    entries: [UiImageCacheEntry; SLOTS],
    count: usize,
    ready: bool,
    defer_frames: u8,
}

#[cfg(feature = "cd-stream-bench")]
impl<const STAGE_WORDS: usize, const SLOTS: usize> UiImageCache<STAGE_WORDS, SLOTS> {
    /// Drop every cached image and mark the cache not ready, so the
    /// next menu-scene service pass re-preloads from CD. Called when
    /// gameplay takes over the cache's RAM (the menu/gameplay arena
    /// overlay); the bytes are about to be overwritten by room draws.
    pub fn invalidate(&mut self) {
        self.entries = [UiImageCacheEntry::EMPTY; SLOTS];
        self.count = 0;
        self.ready = false;
        self.defer_frames = 0;
    }

    /// Zero-initialized cache; `const` so the game's static instance
    /// stays in `.bss`.
    pub const fn new() -> Self {
        Self {
            words: [[0; STAGE_WORDS]; SLOTS],
            entries: [UiImageCacheEntry::EMPTY; SLOTS],
            count: 0,
            ready: false,
            defer_frames: 0,
        }
    }

    /// True once every streamed front-end UI image is resident in the RAM
    /// cache. The engine holds the menu CD-DA until this is true (see
    /// `Scene::front_end_assets_ready`) so the front-end never reads the
    /// CD while music plays.
    pub fn ready(&self) -> bool {
        self.ready
    }

    /// Start a short grace period after entering a menu scene. This lets the
    /// boot frame (or transition cover) present before the next UI.PAK read
    /// starts.
    pub fn note_menu_scene_entered(&mut self) {
        if !self.ready {
            self.defer_frames = 2;
        }
    }

    /// Consume one deferred frame; true while the grace period holds.
    pub fn defer_tick(&mut self) -> bool {
        if self.defer_frames != 0 {
            self.defer_frames -= 1;
            return true;
        }
        false
    }

    /// Stream the ENTIRE front-end UI image group from UI.PAK into the RAM
    /// cache in a SINGLE contiguous CD read, then mark the cache ready. The
    /// whole intro -> menu -> settings group is resident before the menu CD-DA
    /// starts (the engine holds music on `Scene::front_end_assets_ready` ->
    /// the cache's `ready`), so the front-end issues ZERO CD reads while music
    /// plays and navigation between those scenes needs no CD.
    ///
    /// The chunks are read back-to-back in one `SetLoc + ReadN .. Pause`
    /// session rather than one cycle per image. On a real CD-R, each separate
    /// cycle stops and re-seeks the drive (the menu's HUGE boot stall); the
    /// emulator has no seek model, so the win shows there only as the
    /// relocated-read penalty collapsing from N reads to 1.
    pub fn preload_all_contiguous(
        &mut self,
        cd: &mut cd_stream::CdController,
        assets: &'static [LevelAssetRecord],
        rooms: &'static [LevelRoomRecord],
        ui_pack_start_lba: u32,
        ui_pack_toc: &'static [LevelWorldPackEntryRecord],
    ) {
        if self.ready {
            return;
        }

        // Build the read plan from UI.PAK's TOC: only assets explicitly cooked
        // as front-end UI images belong in this cache. The same pack also holds
        // persistent model atlases and transient gameplay textures. The TOC is in disc
        // (ascending sector) order, exactly what the contiguous read requires.
        // Slot k holds chunk k, so its bytes land at the cache word
        // `k * STAGE_WORDS` that `image_bytes(k, ..)` reads back.
        let mut plans = [cd_stream::UiChunkPlan::EMPTY; SLOTS];
        let mut plan_assets = [AssetId(u16::MAX); SLOTS];
        let mut n = 0usize;
        let mut i = 0usize;
        while i < ui_pack_toc.len() && n < SLOTS {
            let entry = ui_pack_toc[i];
            i += 1;
            let asset_id = AssetId(entry.room.raw() as u16);
            let Some(asset) = find_asset_of_kind(assets, asset_id, AssetKind::Texture) else {
                continue;
            };
            if asset.flags & asset_flags::STREAMED_UI == 0
                || !asset.bytes.is_empty()
                || is_sky_panorama_asset(rooms, asset_id)
            {
                continue;
            }
            plans[n] = cd_stream::UiChunkPlan {
                sector_offset: entry.sector_offset,
                sector_count: entry.sector_count,
                byte_size: entry.byte_size as usize,
                checksum: entry.checksum,
                cache_word_start: n.saturating_mul(STAGE_WORDS),
            };
            plan_assets[n] = asset_id;
            n += 1;
        }

        if n == 0 {
            self.ready = true;
            return;
        }

        let mut statuses = [cd_stream::ROOM_CHUNK_STATUS_OK; SLOTS];
        cd_stream::read_chunks_contiguous(
            cd,
            ui_pack_start_lba,
            &plans[..n],
            self.words.as_flattened_mut(),
            &mut statuses[..n],
        );

        // Record one cache entry per planned chunk at its slot. A chunk that
        // failed its read/checksum is left `loaded = false` so the uploader
        // (which checks `loaded`) skips it exactly like a cache miss; COUNT is
        // the plan count so the lookup scans every slot.
        let mut k = 0usize;
        while k < n {
            self.entries[k] = UiImageCacheEntry {
                asset: plan_assets[k],
                bytes: plans[k].byte_size,
                loaded: statuses[k] == cd_stream::ROOM_CHUNK_STATUS_OK,
            };
            k += 1;
        }
        self.count = n;
        self.ready = true;
    }

    /// Advance menu UI streaming by at most one CD chunk, then upload any
    /// active scene image whose bytes are already cached. Prioritising the
    /// active scene keeps the visible menu complete, while the fallback
    /// preloads the other menu states so later transitions avoid CD reads.
    /// Collapsed from the game's glue in phase 1.5: one owner drives the
    /// defer window, the contiguous preload, and the VRAM upload pass.
    pub fn service_menu_images<
        const RAM_ASSETS: usize,
        const VRAM_ASSETS: usize,
        const TPAGES: usize,
        const CLUT_ROWS: usize,
    >(
        &mut self,
        vram: &mut VramRuntime<RAM_ASSETS, VRAM_ASSETS, TPAGES, CLUT_ROWS>,
        layout: VramLayout,
        cd: &mut cd_stream::CdController,
        scene_id: u16,
        ui_scenes: &'static [LevelUiScene],
        ui_nodes: &'static [LevelUiNodeRecord],
        assets: &'static [LevelAssetRecord],
        rooms: &'static [LevelRoomRecord],
        ui_pack_start_lba: u32,
        ui_pack_toc: &'static [LevelWorldPackEntryRecord],
    ) {
        if scene_id == UI_SCENE_NONE {
            return;
        }

        if self.defer_tick() {
            vram.load_ui_images_for_scene(
                layout, self, scene_id, ui_scenes, ui_nodes, assets, rooms,
            );
            return;
        }

        // Stream the WHOLE front-end UI image run from UI.PAK in ONE
        // contiguous CD read (one seek, sequential sectors, one pause)
        // instead of N separate SetLoc+ReadN+Pause cycles. Each per-image
        // cycle forces a real CD-R drive to stop/seek/re-acquire -- a HUGE
        // boot stall on hardware, cheap only in the emulator. The front-end
        // CD-DA is held until this completes (the game gates its
        // front-end-assets-ready on `ready`), so no music contends with
        // the read.
        if !self.ready() {
            self.preload_all_contiguous(cd, assets, rooms, ui_pack_start_lba, ui_pack_toc);
        }
        vram.load_ui_images_for_scene(layout, self, scene_id, ui_scenes, ui_nodes, assets, rooms);
    }

    fn find_entry(&self, asset_id: AssetId) -> Option<(usize, UiImageCacheEntry)> {
        let mut i = 0usize;
        while i < self.count {
            let entry = self.entries[i];
            if entry.loaded && entry.asset == asset_id {
                return Some((i, entry));
            }
            i += 1;
        }
        None
    }

    /// Cached bytes for `asset_id`, when its chunk loaded. The upload
    /// resolver's streamed-UI source (see `VramRuntime::step_uploads`).
    pub fn bytes_for(&self, asset_id: AssetId) -> Option<&[u8]> {
        let (slot, entry) = self.find_entry(asset_id)?;
        self.image_bytes(slot, entry.bytes)
    }

    fn image_bytes(&self, slot: usize, bytes: usize) -> Option<&[u8]> {
        if slot >= SLOTS || bytes > STAGE_WORDS.saturating_mul(4) {
            return None;
        }
        // SAFETY: in-bounds u32 -> u8 view of one cache slot row (chunk
        // byte sizes never exceed a slot; plain old data).
        Some(unsafe { core::slice::from_raw_parts(self.words[slot].as_ptr().cast::<u8>(), bytes) })
    }
}

/// Per-asset upload bookkeeping. When a texture asset becomes
/// VRAM-resident we record its CLUT word, tpage word, and texture
/// window so the per-frame material build can reconstruct its
/// `TextureMaterial` without re-walking the upload code.
#[derive(Copy, Clone)]
pub struct VramSlot {
    /// Asset occupying this slot.
    pub asset: AssetId,
    /// CLUT stamping variant the slot was uploaded with.
    pub clut_mode: VramSlotClutMode,
    /// True once the async upload fully landed.
    pub ready: bool,
    /// GP0 CLUT word of the slot's palette.
    pub clut_word: u16,
    /// GP0 tpage word of the slot's texture page.
    pub tpage_word: u16,
    /// GP0(E2) texture window of the slot's packed placement.
    pub texture_window: TextureWindow,
    /// Uploaded texture width in texels.
    pub texture_width: u16,
    /// Uploaded texture height in texels.
    pub texture_height: u16,
    /// Allocator handle for the texture window/page this slot owns. `Empty` when
    /// the slot shares another slot's pixels (a clut-only variant) or is a
    /// session-persistent resource (model/sky) freed elsewhere.
    pub region: VramHandle,
    /// Allocator handle for this slot's CLUT. `Empty` if not separately owned.
    pub clut_region: VramHandle,
}

/// CLUT stamping variant a [`VramSlot`] was uploaded with.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum VramSlotClutMode {
    /// Palette entry 0 forced opaque (room/world materials).
    OpaqueZero,
    /// Palette entry 0 left transparent (props, UI with alpha).
    TransparentZero,
    /// Indexed model atlas palette (4bpp or 8bpp).
    ModelAtlas,
    /// Streamed sky panorama band palettes.
    SkyPanorama,
}

const VRAM_SLOT_EMPTY: Option<VramSlot> = None;

/// `true` when `asset_id` is referenced as a room's sky cyclorama panorama.
/// The runtime manifest does not carry the host-side streamed class, so the
/// menu UI-image loader and the gameplay sky loader tell their streamed
/// (empty-bytes) Texture assets apart by this room-table lookup.
pub fn is_sky_panorama_asset(rooms: &'static [LevelRoomRecord], asset_id: AssetId) -> bool {
    rooms.iter().any(|room| {
        room.sky.flags & sky_flags::ENABLED != 0 && room.sky.cloud_layer.texture_asset == asset_id
    })
}

/// Baked (non-streamed) texture bytes for `asset_id` out of the asset
/// table: the upload resolver's primary source. Streamed textures carry
/// empty baked bytes and resolve from their RAM cache instead.
pub fn baked_texture_bytes(
    assets: &'static [LevelAssetRecord],
    asset_id: AssetId,
) -> Option<&'static [u8]> {
    let asset = find_asset_of_kind(assets, asset_id, AssetKind::Texture)?;
    if asset.bytes.is_empty() {
        return None;
    }
    Some(asset.bytes)
}

/// The upload queue's byte-resolution rule (see `VramRuntime::step_uploads`):
/// a queued job's bytes come from the asset table when baked, else from
/// the streamed-UI RAM cache. One definition so the game's resolver and
/// the crate's internal UI passes can never disagree.
#[cfg(feature = "cd-stream-bench")]
pub fn resolve_upload_bytes<'a, const STAGE_WORDS: usize, const SLOTS: usize>(
    assets: &'static [LevelAssetRecord],
    ui_images: &'a UiImageCache<STAGE_WORDS, SLOTS>,
    asset_id: AssetId,
) -> Option<&'a [u8]> {
    baked_texture_bytes(assets, asset_id).or_else(|| ui_images.bytes_for(asset_id))
}

/// Clamp a texture edge into the u8 the render material carries.
pub fn vram_slot_texture_size_u8(size: u16) -> u8 {
    size.min(u16::from(u8::MAX)) as u8
}

/// Mark `slots[index]` ready and record the asset VRAM-resident. Free fn
/// (not a method) so the upload queue's step closure can borrow the slot
/// table and residency tracker while the queue itself is `&mut`.
fn mark_vram_slot_ready<const RAM_ASSETS: usize, const VRAM_ASSETS: usize>(
    slots: &mut [Option<VramSlot>; VRAM_ASSETS],
    residency: &mut ResidencyManager<RAM_ASSETS, VRAM_ASSETS>,
    index: usize,
) {
    let Some(mut slot) = slots.get(index).copied().flatten() else {
        return;
    };
    slot.ready = true;
    slots[index] = Some(slot);
    let _ = residency.mark_vram_resident(slot.asset);
}

/// True if any of the `count` desired rooms lists `asset` in its required VRAM set.
fn vram_asset_required(
    asset: AssetId,
    desired: &[RoomIndex],
    count: usize,
    room_residency: &'static [RoomResidencyRecord],
) -> bool {
    for &room in desired.iter().take(count) {
        if room == INVALID_ROOM_INDEX {
            continue;
        }
        if let Some(res) = room_residency.iter().find(|r| r.room == room) {
            if res.required_vram.iter().any(|&a| a == asset) {
                return true;
            }
        }
    }
    false
}

/// VRAM residency/upload runtime: the slot table, the unified allocator,
/// the residency contract tracker, the async upload queue, and the
/// sky/decal placements, owned as one struct. The game supplies its
/// budget consts as the generic parameters (RAM/VRAM residency table
/// capacities, room band page count, managed CLUT rows) and keeps ONE
/// instance in its own static storage (the carve pattern).
pub struct VramRuntime<
    const RAM_ASSETS: usize,
    const VRAM_ASSETS: usize,
    const TPAGES: usize,
    const CLUT_ROWS: usize,
> {
    /// Residency manager -- tracks which AssetIds are RAM/VRAM
    /// resident across frames.
    residency: ResidencyManager<RAM_ASSETS, VRAM_ASSETS>,
    slots: [Option<VramSlot>; VRAM_ASSETS],
    /// Number of VRAM slots used so far across room textures and model atlases.
    slot_count: usize,
    /// The single owner of VRAM. Stage 1 routes fonts through it; later stages
    /// fold in room textures, models, sky, shadow and particle.
    allocator: VramAllocator<TPAGES, CLUT_ROWS>,
    /// Set once the still-hardcoded VRAM regions are reserved in `allocator`.
    regions_reserved: bool,
    /// Current room at the last eviction pass. Eviction only runs when the
    /// streamed residency set shifts (the player crosses into a new room),
    /// keeping it off the per-frame path.
    last_evict_room: RoomIndex,
    /// Sky panorama placement, filled by `ensure_sky_panorama_uploaded` from
    /// the unified allocator: two contiguous 4bpp page words + one CLUT word
    /// per band.
    sky_page_tpage_words: [u16; 2],
    sky_clut_words: [u16; SKY_PANORAMA_PALETTE_BANDS],
    /// Allocator handles for the sky panorama's two-page run and per-band
    /// CLUTs. Captured at upload so `release_streamed_sky` can return them to
    /// the unified allocator on gameplay exit. `Empty` while the sky is not
    /// resident.
    sky_page_region: VramHandle,
    sky_clut_regions: [VramHandle; SKY_PANORAMA_PALETTE_BANDS],
    /// Shadow and particle decals share one 4bpp page (shadow at U=64,
    /// particle at U=0). Allocated once from the unified allocator on first
    /// decal upload.
    shadow_particle_page: Option<Tpage>,
    upload_queue: VramUploadQueue,
    /// Streamed UI image VRAM slots created on menu entry, tracked so they
    /// can be released on gameplay entry. One entry per streamed Texture
    /// asset.
    #[cfg(feature = "cd-stream-bench")]
    ui_image_slots: [Option<AssetId>; MAX_UI_IMAGE_SLOTS],
}

impl<
        const RAM_ASSETS: usize,
        const VRAM_ASSETS: usize,
        const TPAGES: usize,
        const CLUT_ROWS: usize,
    > VramRuntime<RAM_ASSETS, VRAM_ASSETS, TPAGES, CLUT_ROWS>
{
    /// All-zero-bytes placeholder so a game can hold this runtime inside a
    /// link-time-zero (`.bss`) arena static instead of storing `new`'s
    /// non-zero image (allocator CLUT base, eviction sentinel) in the flat
    /// PSX-EXE. The value is NOT ready for use: assign `Self::new(layout)`
    /// over it (once, before first use) to stamp the real initial state.
    ///
    /// Zero-bytes validity, field by field: integer/bool/array fields are
    /// plain old data; `VramHandle`/`VramSlotClutMode` and the upload
    /// queue's enums all have a valid variant at discriminant 0; the
    /// niche-optimized `Option`s (`Option<VramSlot>`, `Option<Tpage>`)
    /// read back as `Some(zeroed payload)` whose payload is itself a valid
    /// (if meaningless) value; `Option<&[u8]>` reads back as `None`.
    pub const fn zeroed() -> Self {
        // SAFETY: every field tolerates the all-zero bit pattern (see the
        // doc above); the placeholder is overwritten by `new` before use.
        unsafe { core::mem::zeroed() }
    }

    /// Empty runtime with the allocator's CLUT band rooted at
    /// `layout.clut_base_y`; `const` so the game can keep it in static
    /// storage.
    pub const fn new(layout: VramLayout) -> Self {
        Self {
            residency: ResidencyManager::new(),
            slots: [VRAM_SLOT_EMPTY; VRAM_ASSETS],
            slot_count: 0,
            allocator: VramAllocator::new(layout.clut_base_y),
            regions_reserved: false,
            last_evict_room: INVALID_ROOM_INDEX,
            sky_page_tpage_words: [0; 2],
            sky_clut_words: [0; SKY_PANORAMA_PALETTE_BANDS],
            sky_page_region: VramHandle::Empty,
            sky_clut_regions: [VramHandle::Empty; SKY_PANORAMA_PALETTE_BANDS],
            shadow_particle_page: None,
            upload_queue: VramUploadQueue::new(),
            #[cfg(feature = "cd-stream-bench")]
            ui_image_slots: [None; MAX_UI_IMAGE_SLOTS],
        }
    }

    /// Pre-mark a room's required asset set on the residency contract
    /// tracker (see `RoomResidencyRecord`).
    pub fn ensure_room_resident(&mut self, room: &RoomResidencyRecord) -> ResidencyChangeSet {
        self.residency.ensure_room_resident(room)
    }

    /// Find a free VRAM slot index, reusing holes left by eviction before growing
    /// into fresh entries. Returns `None` when the slot table is full.
    fn next_vram_slot(&self) -> Option<usize> {
        let slot = (0..VRAM_ASSETS).find(|&i| self.slots[i].is_none());
        if slot.is_none() {
            // The residency table is the binding VRAM budget for distinct
            // resident textures; record the overflow so the otherwise-silent drop is
            // observable (counter-log / overlay) instead of a flat untextured surface.
            telemetry::counter(telemetry::counter::VRAM_SLOT_TABLE_FULL, 1);
        }
        slot
    }

    /// Release slot `i`'s VRAM (texture window/page + CLUT) to the allocator, drop
    /// its residency mark, and clear the slot for reuse. Caller must ensure the slot
    /// is `ready`: a slot with a pending upload job must not be freed, or the async
    /// writeback would land in a slot that has since been reused.
    fn free_vram_slot(&mut self, i: usize) {
        if let Some(slot) = self.slots[i].take() {
            self.allocator.free(slot.region);
            self.allocator.free(slot.clut_region);
            let _ = self.residency.mark_vram_evicted(slot.asset);
            self.slot_count = self.slot_count.saturating_sub(1);
            telemetry::counter(telemetry::counter::VRAM_SLOTS_FREED, 1);
        }
    }

    /// Free the room-texture VRAM slot a streamed UI image occupies, if any.
    /// Tries both 4bpp clut modes since a UI image's transparency flag decides
    /// which mode `ensure_ui_texture_uploaded` picked.
    fn free_room_texture_vram_slot(&mut self, asset_id: AssetId) {
        for i in 0..VRAM_ASSETS {
            if let Some(slot) = self.slots[i] {
                if slot.asset == asset_id
                    && matches!(
                        slot.clut_mode,
                        VramSlotClutMode::OpaqueZero | VramSlotClutMode::TransparentZero
                    )
                {
                    self.free_vram_slot(i);
                }
            }
        }
    }

    /// Free room-texture VRAM slots that no desired room still requires,
    /// returning their window/CLUT to the allocator, debounced on the
    /// current room (the desired set only moves when the camera changes
    /// room, so eviction stays off the per-frame path). Model atlases and
    /// the sky persist for the session; only `ready` slots are freed so a
    /// pending upload's async writeback cannot land in a slot that has
    /// since been reused. Replaces the example's `LAST_EVICT_ROOM` /
    /// `evict_unreferenced_vram` statics.
    pub fn evict_unreferenced_vram(
        &mut self,
        current_room: RoomIndex,
        desired: &[RoomIndex],
        count: usize,
        room_residency: &'static [RoomResidencyRecord],
    ) {
        if self.last_evict_room == current_room {
            return;
        }
        for i in 0..VRAM_ASSETS {
            let slot = match self.slots[i] {
                Some(s) if s.ready => s,
                _ => continue,
            };
            if !matches!(
                slot.clut_mode,
                VramSlotClutMode::OpaqueZero | VramSlotClutMode::TransparentZero
            ) {
                continue;
            }
            if !vram_asset_required(slot.asset, desired, count, room_residency) {
                self.free_vram_slot(i);
            }
        }
        self.last_evict_room = current_room;
    }

    /// Reserve the framebuffer and every region still owned by legacy hardcoded
    /// uploads, so the allocator places fonts in the remaining free VRAM without
    /// collision. Each reserved region migrates into managed allocation in a later
    /// stage.
    fn reserve_static_vram_regions(&mut self, layout: VramLayout) {
        // Double-buffered framebuffer.
        self.allocator.reserve_rect(layout.framebuffer);
        // Room-material window band (allocated via the unified allocator).
        self.allocator
            .reserve_room_band(layout.room_tpage_base_x, 0);
        // Column between the framebuffer and the model-atlas region. Model atlases,
        // the sky panorama, and shadow/particle decals are all allocated dynamically
        // (rows 256 and 0); reserving this gap keeps model atlases at their historical
        // x=384 base.
        self.allocator.reserve_rect(VramRect::new(
            320,
            layout.model_tpage.y(),
            layout.model_tpage.x() - 320,
            256,
        ));
    }

    /// Reserve the static VRAM regions on first call, then pack every UI
    /// font into one combined atlas and upload it in a single `GP0(A0h)`
    /// transfer.
    pub fn acquire_shared_ui_fonts<const LEN: usize>(
        &mut self,
        layout: VramLayout,
        scratch: &mut FontPackScratch<LEN>,
        fonts: &[&'static BitmapFont],
        ui_fonts: &mut [Option<FontAtlas>],
    ) {
        if !self.regions_reserved {
            self.reserve_static_vram_regions(layout);
            self.regions_reserved = true;
        }
        if ui_fonts[0].is_none() && !fonts.is_empty() {
            // Fonts are uploaded once and never torn down (menu and gameplay HUD
            // share them), so the returned VRAM handle is not retained.
            let _ = upload_fonts(fonts, &mut self.allocator, scratch.words_mut(), ui_fonts);
        }
    }

    /// Advance the upload queue by up to `row_budget` texture rows,
    /// marking completed slots ready. Returns whether any job completed.
    /// `resolve` maps a queued job's [`AssetId`] back to its source
    /// bytes (the game's asset table, or its streamed-UI cache); the
    /// queue re-resolves per step instead of retaining slices, so the
    /// sources only need to outlive this call.
    pub fn step_uploads<'r>(
        &mut self,
        row_budget: u16,
        resolve: &impl Fn(AssetId) -> Option<&'r [u8]>,
    ) -> bool {
        let Self {
            upload_queue,
            slots,
            residency,
            ..
        } = self;
        upload_queue.step(row_budget, resolve, |index| {
            mark_vram_slot_ready(slots, residency, index)
        })
    }

    /// True while no upload job is in flight.
    pub fn uploads_idle(&self) -> bool {
        self.upload_queue.is_idle()
    }

    /// Run the VRAM upload queue to idle so the shared UI staging buffer can be
    /// safely overwritten. Bounded so a stuck job can't hang the loader.
    #[cfg(feature = "cd-stream-bench")]
    fn drain_ui_upload_queue<'r>(
        &mut self,
        layout: VramLayout,
        resolve: &impl Fn(AssetId) -> Option<&'r [u8]>,
    ) {
        let mut steps = 0u32;
        while !self.upload_queue.is_idle() && steps < 4096 {
            self.step_uploads(layout.room_tile_texels, resolve);
            steps += 1;
        }
    }

    /// Sky panorama page tpage word (`page` 0 or 1).
    pub fn sky_panorama_tpage_word(&self, page: usize) -> u16 {
        self.sky_page_tpage_words[page.min(1)]
    }

    /// Sky panorama CLUT word for `band`.
    pub fn sky_panorama_clut_word(&self, band: usize) -> u16 {
        self.sky_clut_words[band.min(SKY_PANORAMA_PALETTE_BANDS - 1)]
    }

    fn shadow_particle_page(&mut self) -> Option<Tpage> {
        if self.shadow_particle_page.is_none() {
            let (tpage, _region) = self.allocator.alloc_page_run(1, TexDepth::Bit4, 0)?;
            self.shadow_particle_page = Some(tpage);
        }
        self.shadow_particle_page
    }

    /// Upload the subtract-blended circular floor shadow decal (a 64x64
    /// 4bpp `Texture` blob) onto the shared shadow/particle page.
    pub fn upload_shadow_texture(&mut self, shadow_circle_blob: &[u8]) -> Option<TextureMaterial> {
        let texture = Texture::from_bytes(shadow_circle_blob).ok()?;
        if texture.width() != 64 || texture.height() != 64 || texture.clut_entries() != 16 {
            return None;
        }

        let page = self.shadow_particle_page()?;
        let (clut, _clut_region) = self.allocator.alloc_clut(texture.clut_entries())?;
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

    /// Generate and upload the 16x16 white circular particle sprite onto
    /// the shared shadow/particle page.
    pub fn upload_particle_texture(&mut self) -> Option<TextureMaterial> {
        let mut pixels = [0u8; (PARTICLE_TEXTURE_HALFWORDS_PER_ROW as usize)
            * (PARTICLE_TEXTURE_SIZE as usize)
            * 2];
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

        let page = self.shadow_particle_page()?;
        let (clut_pos, _clut_region) = self.allocator.alloc_clut(16)?;
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

    /// Look up the VRAM slot a previously-uploaded asset occupies.
    /// The slot table is the source of truth -- the residency tracker only
    /// tracks the *contract*, which is pre-marked by `ensure_room_resident`
    /// before any actual upload runs.
    fn find_vram_slot(&self, asset_id: AssetId, clut_mode: VramSlotClutMode) -> Option<VramSlot> {
        self.slots
            .iter()
            .filter_map(|s| *s)
            .find(|s| s.ready && s.asset == asset_id && s.clut_mode == clut_mode)
    }

    /// Look up the sky panorama's VRAM slot, if the sky is uploaded. Used by the
    /// render path so a streamed (empty-bytes) sky resolves its already-uploaded
    /// slot instead of re-parsing empty asset bytes.
    pub fn find_sky_panorama_vram_slot(&self, asset_id: AssetId) -> Option<VramSlot> {
        self.find_vram_slot(asset_id, VramSlotClutMode::SkyPanorama)
    }

    /// Ready room-texture slot (either 4bpp CLUT mode) for `asset_id`.
    pub fn find_room_texture_vram_slot(&self, asset_id: AssetId) -> Option<VramSlot> {
        self.slots.iter().filter_map(|s| *s).find(|s| {
            s.ready
                && s.asset == asset_id
                && matches!(
                    s.clut_mode,
                    VramSlotClutMode::OpaqueZero | VramSlotClutMode::TransparentZero
                )
        })
    }

    fn pending_vram_upload(&self, asset_id: AssetId, clut_mode: VramSlotClutMode) -> bool {
        self.slots
            .iter()
            .filter_map(|s| *s)
            .any(|s| !s.ready && s.asset == asset_id && s.clut_mode == clut_mode)
            || self.upload_queue.contains(asset_id, clut_mode)
    }

    /// True while `asset_id` has a room-texture upload still in flight.
    pub fn pending_room_texture_upload(&self, asset_id: AssetId) -> bool {
        self.slots.iter().filter_map(|s| *s).any(|s| {
            !s.ready
                && s.asset == asset_id
                && matches!(
                    s.clut_mode,
                    VramSlotClutMode::OpaqueZero | VramSlotClutMode::TransparentZero
                )
        })
    }

    /// Upload `asset_bytes` to VRAM if not already resident; return
    /// the slot record so the caller can build a TextureMaterial.
    /// Returns `None` if the texture parse fails or the VRAM table
    /// is full.
    pub fn ensure_texture_uploaded(
        &mut self,
        layout: VramLayout,
        asset_id: AssetId,
        asset_bytes: &[u8],
    ) -> Option<VramSlot> {
        let texture = Texture::from_bytes(asset_bytes).ok()?;
        let clut_mode = if texture.index_zero_transparent() {
            VramSlotClutMode::TransparentZero
        } else {
            VramSlotClutMode::OpaqueZero
        };
        self.ensure_texture_uploaded_with_clut_mode(layout, asset_id, asset_bytes, clut_mode)
    }

    /// Upload a room material texture (palette entry 0 forced opaque).
    pub fn ensure_room_texture_uploaded(
        &mut self,
        layout: VramLayout,
        asset_id: AssetId,
        asset_bytes: &[u8],
    ) -> Option<VramSlot> {
        self.ensure_texture_uploaded_with_clut_mode(
            layout,
            asset_id,
            asset_bytes,
            VramSlotClutMode::OpaqueZero,
        )
    }

    /// Upload a UI texture, stepping the upload queue a bounded number of
    /// times (through `resolve`, see [`Self::step_uploads`]) so menu
    /// images resolve within the calling frame.
    pub fn ensure_ui_texture_uploaded<'r>(
        &mut self,
        layout: VramLayout,
        asset_id: AssetId,
        asset_bytes: &[u8],
        resolve: &impl Fn(AssetId) -> Option<&'r [u8]>,
    ) -> Option<VramSlot> {
        let texture = Texture::from_bytes(asset_bytes).ok()?;
        let clut_mode = if texture.index_zero_transparent() {
            VramSlotClutMode::TransparentZero
        } else {
            VramSlotClutMode::OpaqueZero
        };
        if let Some(slot) = self.find_vram_slot(asset_id, clut_mode) {
            return Some(slot);
        }

        let use_large_ui_upload = texture.width() > layout.room_tile_texels
            || texture.height() > layout.room_tile_texels
            || room_texture_window_size(layout, texture.width()).is_none()
            || room_texture_window_size(layout, texture.height()).is_none();
        let _ = if use_large_ui_upload {
            self.ensure_large_ui_texture_uploaded_with_clut_mode(
                layout,
                asset_id,
                asset_bytes,
                clut_mode,
            )
        } else {
            self.ensure_texture_uploaded_with_clut_mode(layout, asset_id, asset_bytes, clut_mode)
        };
        let mut steps = 0u8;
        while self.pending_vram_upload(asset_id, clut_mode) && steps < UI_TEXTURE_UPLOAD_MAX_STEPS {
            self.step_uploads(layout.room_tile_texels, resolve);
            steps = steps.saturating_add(1);
        }

        self.find_vram_slot(asset_id, clut_mode)
    }

    fn ensure_large_ui_texture_uploaded_with_clut_mode(
        &mut self,
        layout: VramLayout,
        asset_id: AssetId,
        asset_bytes: &[u8],
        clut_mode: VramSlotClutMode,
    ) -> Option<VramSlot> {
        if let Some(slot) = self.find_vram_slot(asset_id, clut_mode) {
            return Some(slot);
        }
        if self.pending_vram_upload(asset_id, clut_mode) {
            return None;
        }
        if self.pending_room_texture_upload(asset_id) {
            return None;
        }
        if !self.upload_queue.has_free_slot() {
            telemetry::counter(telemetry::counter::VRAM_UPLOAD_QUEUE_FULL, 1);
            return None;
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
        if texture_width_halfwords > layout.room_tpage_stride_hw {
            return None;
        }
        let expected_pixel_bytes = usize::from(texture_width_halfwords)
            .saturating_mul(usize::from(texture.height()))
            .saturating_mul(2);
        if texture.pixel_bytes().len() != expected_pixel_bytes {
            return None;
        }

        let idx = self.next_vram_slot()?;
        let (tpage, region) = self.allocator.alloc_room_page()?;
        let tpage_x = tpage.x();
        let (clut, clut_region) = self.allocator.alloc_clut(texture.clut_entries())?;
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

        self.slots[idx] = Some(slot);
        self.slot_count += 1;
        if !self.upload_queue.push(VramUploadJob {
            active: true,
            slot_index: idx as u16,
            asset: asset_id,
            clut_mode,
            kind: VramUploadKind::TextureAndClut,
            texture_x: tpage_x,
            texture_y: layout.shared_tpage.y(),
            texture_width_halfwords,
            texture_height_rows: texture.height(),
            next_texture_row: 0,
            clut_x: clut.x(),
            clut_y: clut.y(),
            clut_entries: texture.clut_entries(),
            clut_uploaded: false,
        }) {
            self.slots[idx] = None;
            self.slot_count -= 1;
            return None;
        }

        None
    }

    /// Queue `asset_bytes` for upload with an explicit CLUT stamping mode.
    /// Returns the already-resident slot when the asset is up; `None` while
    /// the upload is pending (or dropped).
    pub fn ensure_texture_uploaded_with_clut_mode(
        &mut self,
        layout: VramLayout,
        asset_id: AssetId,
        asset_bytes: &[u8],
        clut_mode: VramSlotClutMode,
    ) -> Option<VramSlot> {
        // The slot table is the source of truth for "have we actually
        // uploaded this asset". The residency tracker is the *contract* --
        // it's pre-marked by `ensure_room_resident` before any upload runs,
        // so reading it here would falsely report assets as uploaded
        // and skip the upload entirely.
        if let Some(slot) = self.find_vram_slot(asset_id, clut_mode) {
            return Some(slot);
        }
        if self.pending_vram_upload(asset_id, clut_mode) {
            return None;
        }
        if self.pending_room_texture_upload(asset_id) {
            return None;
        }
        if !self.upload_queue.has_free_slot() {
            telemetry::counter(telemetry::counter::VRAM_UPLOAD_QUEUE_FULL, 1);
            return None;
        }

        let texture = Texture::from_bytes(asset_bytes).ok()?;
        if texture.clut_entries() != 16 {
            return None;
        }

        // Capacity check before we touch any VRAM state.
        let idx = self.next_vram_slot()?;

        if texture.width() > layout.room_tile_texels || texture.height() > layout.room_tile_texels {
            return None;
        }

        let texture_width = room_texture_window_size(layout, texture.width())?;
        let texture_height = room_texture_window_size(layout, texture.height())?;
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

        if let Some(shared_texture) = self.find_room_texture_vram_slot(asset_id) {
            let (clut, clut_region) = match self.allocator.alloc_clut(texture.clut_entries()) {
                Some(pair) => pair,
                None => {
                    telemetry::counter(telemetry::counter::VRAM_CLUT_FULL, 1);
                    return None;
                }
            };
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
            self.slots[idx] = Some(slot);
            self.slot_count += 1;
            if !self.upload_queue.push(VramUploadJob {
                active: true,
                slot_index: idx as u16,
                asset: asset_id,
                clut_mode,
                kind: VramUploadKind::ClutOnly,
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
                self.slots[idx] = None;
                self.slot_count -= 1;
                return None;
            }
            return None;
        }

        // Pack room materials on the GP0(E2) 8-texel grid inside 4bpp
        // tpages. A 32x32 texture now consumes a 32x32 window instead of
        // burning a whole old 64x64 cell.
        let (tpage, placement, region) = match self
            .allocator
            .alloc_window(u16::from(texture_width), u16::from(texture_height))
        {
            Some(window) => window,
            None => {
                telemetry::counter(telemetry::counter::VRAM_WINDOW_FULL, 1);
                return None;
            }
        };
        let tpage_x = tpage.x();
        let texture_x = tpage_x.checked_add(u16::from(placement.origin_u()) / 4)?;
        let texture_y = layout
            .shared_tpage
            .y()
            .checked_add(u16::from(placement.origin_v()))?;

        let (clut, clut_region) = match self.allocator.alloc_clut(texture.clut_entries()) {
            Some(pair) => pair,
            None => {
                telemetry::counter(telemetry::counter::VRAM_CLUT_FULL, 1);
                return None;
            }
        };
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

        self.slots[idx] = Some(slot);
        self.slot_count += 1;
        if !self.upload_queue.push(VramUploadJob {
            active: true,
            slot_index: idx as u16,
            asset: asset_id,
            clut_mode,
            kind: VramUploadKind::TextureAndClut,
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
            self.slots[idx] = None;
            self.slot_count -= 1;
            return None;
        }

        None
    }

    /// Resolve (or begin uploading) a prop texture's transparent-zero slot.
    pub fn prop_texture_slot(
        &mut self,
        layout: VramLayout,
        assets: &'static [LevelAssetRecord],
        texture_asset: AssetId,
    ) -> Option<VramSlot> {
        let clut_mode = VramSlotClutMode::TransparentZero;
        if let Some(slot) = self.find_vram_slot(texture_asset, clut_mode) {
            return Some(slot);
        }
        let asset = find_asset_of_kind(assets, texture_asset, AssetKind::Texture)?;
        self.ensure_texture_uploaded_with_clut_mode(layout, asset.id, asset.bytes, clut_mode)
    }

    /// True once every image, box, cylinder, and arch texture of `room` is
    /// VRAM-resident.
    #[cfg(feature = "cd-stream-bench")]
    pub fn room_prop_textures_ready(
        &mut self,
        layout: VramLayout,
        assets: &'static [LevelAssetRecord],
        image_props: &'static [LevelImagePropRecord],
        box_props: &'static [LevelBoxPropRecord],
        cylinder_props: &'static [LevelCylinderPropRecord],
        arch_props: &'static [LevelArchPropRecord],
        room: RoomIndex,
    ) -> bool {
        let mut ready = true;

        for prop in image_props {
            if prop.room == room
                && self
                    .prop_texture_slot(layout, assets, prop.texture_asset)
                    .is_none()
            {
                ready = false;
            }
        }

        for prop in box_props {
            if prop.room != room {
                continue;
            }
            let mut face = 0usize;
            while face < BOX_PROP_FACE_COUNT {
                if let Some(texture_asset) = prop.texture_assets[face] {
                    if self
                        .prop_texture_slot(layout, assets, texture_asset)
                        .is_none()
                    {
                        ready = false;
                    }
                }
                face += 1;
            }
        }

        for prop in cylinder_props {
            if prop.room != room {
                continue;
            }
            for texture_asset in prop.texture_assets.iter().flatten() {
                if self
                    .prop_texture_slot(layout, assets, *texture_asset)
                    .is_none()
                {
                    ready = false;
                }
            }
        }

        for prop in arch_props {
            if prop.room != room {
                continue;
            }
            let mut slot = 0;
            while slot < ARCH_PROP_MATERIAL_COUNT {
                if let Some(texture_asset) = prop.texture_assets[slot] {
                    if self
                        .prop_texture_slot(layout, assets, texture_asset)
                        .is_none()
                    {
                        ready = false;
                    }
                }
                slot += 1;
            }
        }

        ready
    }

    /// Upload the streamed sky panorama (two 4bpp pages + one CLUT per
    /// band) synchronously from `asset_bytes`. Reuses the resident slot on
    /// re-entry.
    pub fn ensure_sky_panorama_uploaded(
        &mut self,
        asset_id: AssetId,
        asset_bytes: &[u8],
    ) -> Option<VramSlot> {
        if let Some(slot) = self.find_vram_slot(asset_id, VramSlotClutMode::SkyPanorama) {
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
        let idx = self.next_vram_slot()?;
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
        // all from the unified allocator. The page-run and per-band CLUT handles are
        // retained so `release_streamed_sky` can free them on gameplay exit.
        let (left_tpage, page_region) = self.allocator.alloc_page_run(2, TexDepth::Bit4, 256)?;
        let right_tpage = Tpage::new(left_tpage.x() + 64, left_tpage.y(), TexDepth::Bit4);
        let mut sky_cluts = [Clut::new(0, 0); SKY_PANORAMA_PALETTE_BANDS];
        let mut sky_clut_regions = [VramHandle::Empty; SKY_PANORAMA_PALETTE_BANDS];
        for (dst, region_dst) in sky_cluts.iter_mut().zip(sky_clut_regions.iter_mut()) {
            let (clut, clut_region) = self.allocator.alloc_clut(SKY_PANORAMA_CLUT_ENTRIES)?;
            *dst = clut;
            *region_dst = clut_region;
        }
        self.sky_page_tpage_words = [left_tpage.uv_tpage_word(0), right_tpage.uv_tpage_word(0)];
        for (band, clut) in sky_cluts.iter().enumerate() {
            self.sky_clut_words[band] = clut.uv_clut_word();
        }
        self.sky_page_region = page_region;
        self.sky_clut_regions = sky_clut_regions;

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
            clut_word: self.sky_panorama_clut_word(0),
            tpage_word: self.sky_panorama_tpage_word(0),
            texture_window: TextureWindow::NONE,
            texture_width: texture.width(),
            texture_height: texture.height(),
            // The sky's allocator handles (2 pages + 8 CLUTs) live in the dedicated
            // sky_page_region / sky_clut_regions fields and are freed by
            // `release_streamed_sky` on gameplay exit, not via slot eviction.
            region: VramHandle::Empty,
            clut_region: VramHandle::Empty,
        };
        self.slots[idx] = Some(slot);
        self.slot_count += 1;
        let _ = self.residency.mark_vram_resident(asset_id);
        Some(slot)
    }

    /// Load the CD-streamed sky panorama into VRAM on gameplay entry.
    ///
    /// The sky is a gameplay-scoped streamed Texture (empty baked bytes under
    /// `cd-stream-bench`): its UI.PAK chunk is read into a transient staging
    /// buffer and handed to the normal sky upload path. The staging buffer is
    /// the shared [`FontPackScratch`], which is free during gameplay -- the UI
    /// fonts are packed once on first scene entry (`acquire_shared_ui_fonts`,
    /// guarded by `ui_fonts[0].is_none()`) and are already in VRAM, never
    /// re-packed and never released, so the scratch is unused after boot. The
    /// sky (~64 KB) fits inside its 128 KB (the game const-asserts the fit).
    ///
    /// A no-op for non-streamed (baked) builds: a non-empty sky asset is uploaded
    /// lazily by the render path's `ensure_sky_panorama_uploaded`.
    #[cfg(feature = "cd-stream-bench")]
    pub fn load_streamed_sky_from_cd<const LEN: usize>(
        &mut self,
        cd: &mut cd_stream::CdController,
        scratch: &mut FontPackScratch<LEN>,
        gameplay_pack_max_chunk_bytes: usize,
        assets: &'static [LevelAssetRecord],
        rooms: &'static [LevelRoomRecord],
        ui_pack_start_lba: u32,
        ui_pack_toc: &'static [LevelWorldPackEntryRecord],
    ) {
        for asset in assets {
            if asset.kind != AssetKind::Texture || !asset.bytes.is_empty() {
                continue;
            }
            if !is_sky_panorama_asset(rooms, asset.id) {
                continue;
            }
            // Already resident (idempotent re-entry).
            if self
                .find_vram_slot(asset.id, VramSlotClutMode::SkyPanorama)
                .is_some()
            {
                continue;
            }

            // The CD read writes whole sectors as bytes through the scratch's
            // u32-aligned staging view; the read consumes it before the
            // upload, and nothing else touches it during gameplay.
            let sky_stage_words = (gameplay_pack_max_chunk_bytes + 3) / 4;
            let Some(stage) = scratch.stage_words_mut(sky_stage_words) else {
                continue;
            };

            let res = cd_stream::read_chunk_blocking(
                cd,
                ui_pack_start_lba,
                ui_pack_toc,
                asset.id.0 as u32,
                stage,
            );
            if res.status != cd_stream::ROOM_CHUNK_STATUS_OK || res.bytes == 0 {
                continue;
            }

            // The staged bytes are valid until the next read overwrites the
            // buffer; the sky upload below consumes them synchronously.
            let Some(bytes) = scratch.staged_bytes(res.bytes) else {
                continue;
            };
            let _ = self.ensure_sky_panorama_uploaded(asset.id, bytes);
        }
    }

    /// Free the streamed sky panorama's VRAM on gameplay exit: return its two-page
    /// run and per-band CLUTs to the unified allocator, drop its VRAM slot, and
    /// clear the cached placement words so the next gameplay entry re-streams it.
    /// A no-op when the sky is not resident.
    #[cfg(feature = "cd-stream-bench")]
    pub fn release_streamed_sky(&mut self) {
        let mut freed = false;
        for i in 0..VRAM_ASSETS {
            if let Some(slot) = self.slots[i] {
                if slot.clut_mode == VramSlotClutMode::SkyPanorama {
                    let _ = self.residency.mark_vram_evicted(slot.asset);
                    self.slots[i] = None;
                    self.slot_count = self.slot_count.saturating_sub(1);
                    freed = true;
                }
            }
        }
        if !freed {
            return;
        }
        self.allocator.free(core::mem::replace(
            &mut self.sky_page_region,
            VramHandle::Empty,
        ));
        for region in self.sky_clut_regions.iter_mut() {
            self.allocator
                .free(core::mem::replace(region, VramHandle::Empty));
        }
        self.sky_page_tpage_words = [0; 2];
        self.sky_clut_words = [0; SKY_PANORAMA_PALETTE_BANDS];
        telemetry::counter(telemetry::counter::VRAM_SLOTS_FREED, 1);
    }

    /// Upload a 4bpp or 8bpp model atlas to the dedicated model VRAM
    /// region. Returns a `VramSlot` carrying the depth-correct tpage word
    /// and the atlas's CLUT word. Reuses an existing slot when the
    /// asset's already resident.
    ///
    /// Direct-colour atlases and malformed indexed palettes return `None`.
    pub fn ensure_model_atlas_uploaded(
        &mut self,
        layout: VramLayout,
        asset_id: AssetId,
        asset_bytes: &[u8],
    ) -> Option<VramSlot> {
        // Same caveat as `ensure_texture_uploaded`: the slot table is
        // the source of truth, not the residency tracker.
        if let Some(slot) = self.find_vram_slot(asset_id, VramSlotClutMode::ModelAtlas) {
            return Some(slot);
        }
        let texture = Texture::from_bytes(asset_bytes).ok()?;
        let texture_depth = match texture.clut_entries() {
            16 => TexDepth::Bit4,
            256 => TexDepth::Bit8,
            _ => return None,
        };

        let idx = self.next_vram_slot()?;
        let texture_width = texture.width();
        let texture_height = texture.height();
        let texture_halfwords_per_row = texture.halfwords_per_row();
        if texture_width == 0
            || texture_width > 256
            || texture_height == 0
            || texture_height > 256
            || texture_halfwords_per_row > layout.model_tpage_max_halfwords
        {
            return None;
        }
        let expected_pixel_bytes = (texture_halfwords_per_row as usize)
            .saturating_mul(texture_height as usize)
            .saturating_mul(2);
        if texture.pixel_bytes().len() != expected_pixel_bytes {
            return None;
        }

        // Placement comes only from the unified allocator: an indexed page run
        // at row 256 plus a depth-sized CLUT in the managed band.
        let (tpage, region) = self
            .allocator
            .alloc_model_slot(texture_halfwords_per_row, texture_depth)?;
        let (clut, clut_region) = self.allocator.alloc_clut(texture.clut_entries())?;
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

        self.slots[idx] = Some(slot);
        self.slot_count += 1;
        let _ = self.residency.mark_vram_resident(asset_id);
        Some(slot)
    }

    /// Upload the active UI scene's streamed images into VRAM from the RAM cache.
    /// Tracks each created slot so `release_ui_images` can free it when the scene
    /// changes or gameplay starts. The cache is an ordinary borrow since the
    /// upload queue re-resolves job bytes through `resolve` per step (phase
    /// 1.5) instead of retaining `&'static` slices across frames.
    #[cfg(feature = "cd-stream-bench")]
    pub fn load_ui_images_for_scene<'r, const STAGE_WORDS: usize, const SLOTS: usize>(
        &mut self,
        layout: VramLayout,
        cache: &'r UiImageCache<STAGE_WORDS, SLOTS>,
        scene_id: u16,
        ui_scenes: &'static [LevelUiScene],
        ui_nodes: &'static [LevelUiNodeRecord],
        assets: &'static [LevelAssetRecord],
        rooms: &'static [LevelRoomRecord],
    ) -> bool {
        if scene_id == UI_SCENE_NONE {
            return true;
        }
        // One byte-resolution rule for the whole pass; queued jobs
        // re-resolve through this same cache borrow per step.
        let resolve = |asset_id| resolve_upload_bytes(assets, cache, asset_id);

        let Some(scene) = ui_scenes.iter().find(|scene| scene.id == scene_id) else {
            return false;
        };
        let mut all_ready = true;
        let first = scene.node_first as usize;
        let end = first
            .saturating_add(scene.node_count as usize)
            .min(ui_nodes.len());
        for node in &ui_nodes[first..end] {
            if node.kind != LevelUiNodeKind::Image {
                continue;
            }
            let Some(asset) = find_asset_of_kind(assets, node.texture_asset, AssetKind::Texture)
            else {
                continue;
            };
            if !asset.bytes.is_empty() {
                continue;
            }
            // The sky panorama is also a streamed (empty-bytes) Texture but is
            // gameplay-scoped: it is loaded by `load_streamed_sky_from_cd` into
            // a larger staging buffer, never through this small per-image one.
            if is_sky_panorama_asset(rooms, asset.id) {
                continue;
            }
            // Skip if already uploaded (idempotent re-entry).
            if self.find_room_texture_vram_slot(asset.id).is_some() {
                self.track_ui_image_slot(asset.id);
                continue;
            }

            let Some((cache_slot, cache_entry)) = cache.find_entry(asset.id) else {
                all_ready = false;
                continue;
            };
            let Some(bytes) = cache.image_bytes(cache_slot, cache_entry.bytes) else {
                all_ready = false;
                continue;
            };
            if self
                .ensure_ui_texture_uploaded(layout, asset.id, bytes, &resolve)
                .is_none()
            {
                // Drain in case a job was queued but not yet completed, then
                // retry the lookup before giving up on this image.
                self.drain_ui_upload_queue(layout, &resolve);
            }

            // Make sure this asset's upload has fully drained before render
            // asks the UI texture resolver for its slot.
            self.drain_ui_upload_queue(layout, &resolve);

            if self.find_room_texture_vram_slot(asset.id).is_some() {
                self.track_ui_image_slot(asset.id);
            } else {
                all_ready = false;
            }
        }
        all_ready
    }

    #[cfg(feature = "cd-stream-bench")]
    fn track_ui_image_slot(&mut self, asset_id: AssetId) {
        for entry in self.ui_image_slots.iter() {
            if *entry == Some(asset_id) {
                return;
            }
        }
        for entry in self.ui_image_slots.iter_mut() {
            if entry.is_none() {
                *entry = Some(asset_id);
                return;
            }
        }
    }

    /// Free every streamed UI image VRAM slot created by `load_ui_images_for_scene`.
    /// Called on gameplay entry so the room textures reclaim that VRAM. Fonts are
    /// shared and are NOT released here.
    #[cfg(feature = "cd-stream-bench")]
    pub fn release_ui_images(&mut self) {
        for i in 0..MAX_UI_IMAGE_SLOTS {
            if let Some(asset_id) = self.ui_image_slots[i].take() {
                self.free_room_texture_vram_slot(asset_id);
            }
        }
    }
}

fn room_texture_window_size(layout: VramLayout, size: u16) -> Option<u8> {
    if size < 8 || size > layout.room_tile_texels || !size.is_power_of_two() || size % 8 != 0 {
        return None;
    }
    u8::try_from(size).ok()
}

/// Stamp the 0x8000 (semi-transparency-disable) bit on every
/// non-zero CLUT entry so opaque textures don't accidentally
/// trigger STP-bit blending.
pub(crate) fn upload_clut(rect: VramRect, bytes: &[u8]) {
    upload_clut_with_mode(rect, bytes, false);
}

/// Upload a CLUT for room/world materials. Imported room textures
/// are opaque until the material system grows an explicit alpha
/// control, so palette entry 0 must not punch holes in geometry.
pub(crate) fn upload_opaque_clut(rect: VramRect, bytes: &[u8]) {
    upload_clut_with_mode(rect, bytes, true);
}

/// Upload a CLUT for indexed model atlases. New alpha-aware atlases can
/// reserve palette index 0 for transparent gutter texels; legacy
/// atlases keep their old fully-opaque behaviour.
pub(crate) fn upload_model_clut(rect: VramRect, bytes: &[u8], transparent_index_zero: bool) {
    let mut marked = [0u8; 512];
    if bytes.len() > marked.len() || !bytes.len().is_multiple_of(2) {
        return;
    }

    let mut i = 0;
    while i < bytes.len() {
        let raw = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        let index = i / 2;
        let pair = model_clut_entry_for_upload(index, raw, transparent_index_zero).to_le_bytes();
        marked[i] = pair[0];
        marked[i + 1] = pair[1];
        i += 2;
    }

    upload_bytes(rect, &marked[..bytes.len()]);
}

/// CLUT entry stamping rule for indexed model atlases (see [`upload_model_clut`]).
pub const fn model_clut_entry_for_upload(
    index: usize,
    raw: u16,
    transparent_index_zero: bool,
) -> u16 {
    if transparent_index_zero && index == 0 && raw == 0 {
        0
    } else {
        raw | 0x8000
    }
}

fn upload_clut_with_mode(rect: VramRect, bytes: &[u8], force_zero_opaque: bool) {
    let mut marked = [0u8; 512];
    if bytes.len() > marked.len() || !bytes.len().is_multiple_of(2) {
        return;
    }

    let mut i = 0;
    while i < bytes.len() {
        let raw = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        let stamped = if raw == 0 && !force_zero_opaque {
            0
        } else {
            raw | 0x8000
        };
        let pair = stamped.to_le_bytes();
        marked[i] = pair[0];
        marked[i + 1] = pair[1];
        i += 2;
    }

    upload_bytes(rect, &marked[..bytes.len()]);
}
