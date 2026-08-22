//! The example's mutable runtime state, folded into ONE arena struct
//! behind ONE static (phase 1.5 of docs/game-runtime-plan.md). The
//! former `static mut` instances (`VRAM_RUNTIME`, `FONT_PACK_SCRATCH`,
//! `PRIMITIVE_PACKETS`, `WORLD_COMMANDS`, `UI_IMAGE_CACHE`, `PERSISTENT_ASSETS`,
//! `STREAMED_ROOM_SLOTS`, `ROOM_STREAM_SCHEDULER`, `ROOM_MATERIAL_POOL`,
//! `PREBUILT_ROOM_QUADS`) are now [`RuntimeArenas`] fields.
//!
//! Flat-binary discipline: the arena's project-sized buffers must stay
//! link-time-zero (`.bss` is NOLOAD in the PSX-EXE), so the static is
//! built from the crate types' all-zero `zeroed()`/`new()` constructors
//! and [`init_runtime_arenas`] stamps the non-zero initial state (VRAM
//! layout, sentinels) at boot, before `App::run_with_flow`.
//!
//! Borrow discipline: each accessor below mints a fresh borrow of ONE
//! field through a raw projection of the arena static. Borrows of
//! DISTINCT fields never overlap in memory, so holding one while
//! minting another mirrors the old disjoint-statics aliasing exactly;
//! the pre-existing rule is unchanged: never hold two borrows of the
//! SAME field at once, and treat streamed-slot views as stale after the
//! next streaming step (the [`StreamedRoomPages`] staleness contract).
//!
//! [`StreamedRoomPages`]: psx_game_runtime::room_streaming::StreamedRoomPages

use super::*;

/// Every mutable runtime arena, owned as one struct so the whole
/// mutable-state budget reads in one place (the scene's other state
/// lives on [`Playtest`]; this is the part its `const fn new` cannot
/// zero-initialize cheaply or that outlives scene borrows).
pub(super) struct RuntimeArenas {
    /// VRAM slot table, unified allocator, residency tracker, and
    /// upload queue (see `psx_game_runtime::vram::VramRuntime`).
    pub(super) vram: RuntimeVram,
    /// Scene-load staging and per-frame renderer scratch have disjoint
    /// lifetimes, so they share one RAM region.
    pub(super) load_render: LoadRenderOverlay,
    /// Front-end/gameplay RAM overlay (see [`FrontEndGameplayOverlay`]).
    #[cfg(feature = "cd-stream-bench")]
    pub(super) overlay: FrontEndGameplayOverlay,
    /// Streamed-room sector pages the CD loads land in directly.
    #[cfg(feature = "cd-stream-bench")]
    pub(super) streamed_slots: RuntimeStreamedRoomSlots,
    /// Streamed-room residency scheduler over `streamed_slots`.
    #[cfg(feature = "cd-stream-bench")]
    pub(super) room_streams: RuntimeRoomStreamScheduler,
    /// Room-surface materials pooled by resident stream slot.
    #[cfg(feature = "cd-stream-bench")]
    pub(super) room_materials: RuntimeRoomMaterialPool,
    /// Prebuilt GP0(3Ch) room-quad packets (docs/perf-30fps.md).
    /// With `cd-stream-bench` this lives inside [`FrontEndGameplayOverlay`].
    #[cfg(not(feature = "cd-stream-bench"))]
    pub(super) prebuilt_quads: RuntimePrebuiltRoomQuads,
    /// Rotation-keyed sky-cyclorama packet cache (phase-2 sky carve).
    pub(super) sky: psx_game_runtime::sky::SkyCyclorama,
    /// Accepted-cell draw scratch for the cached-room draw paths
    /// (phase-2 visible-cell carve).
    #[cfg(feature = "world-grid-visible")]
    pub(super) cell_scratch: RuntimeCellDrawScratch,
    /// Projected-vertex + joint scratch for the model submit paths
    /// (phase-2 model-rendering carve).
    pub(super) model_scratch: RuntimeModelDrawScratch,
    /// Break-time box-prop floor-debris cache (phase-2 box-prop carve).
    pub(super) debris_cache: psx_game_runtime::box_props::DebrisCache,
    /// Owned CD controller driver state (phase-2 retirement of the
    /// crate's carried `cd_stream` statics).
    #[cfg(feature = "cd-stream-bench")]
    pub(super) cd: psx_game_runtime::cd_stream::CdController,
    /// Per-frame projected-vertex scratch for the cached-room draws
    /// (the room_cache module's scratch, folded in with phase 2).
    /// With `cd-stream-bench` this lives inside [`FrontEndGameplayOverlay`].
    #[cfg(not(feature = "cd-stream-bench"))]
    pub(super) room_projection: RuntimeCachedRoomProjection,
}

/// Scratch used only while rendering a frame.
pub(super) struct FrameRenderScratch {
    pub(super) primitive_packets: PrimitivePacketScratch<MAX_TEXTURED_TRIS>,
    pub(super) world_commands: [WorldTriCommand; MAX_WORLD_COMMANDS],
}

impl FrameRenderScratch {
    const ZEROED: Self = Self {
        primitive_packets: PrimitivePacketScratch::ZERO,
        world_commands: [WorldTriCommand::EMPTY; MAX_WORLD_COMMANDS],
    };
}

/// RAM overlay for mutually exclusive load-time and render-time scratch.
///
/// Fonts are packed on first menu entry and the sky is staged on gameplay
/// entry. Both operations finish before the next frame starts using packet
/// and world-command storage. Once uploaded, their bytes live in VRAM and no
/// longer need this staging region.
pub(super) union LoadRenderOverlay {
    pub(super) load: core::mem::ManuallyDrop<RuntimeFontPackScratch>,
    pub(super) render: core::mem::ManuallyDrop<FrameRenderScratch>,
}

/// Gameplay-only arenas that overlay the front-end UI-image cache.
pub(super) struct GameplayAssetArenas {
    pub(super) persistent_assets: RuntimePersistentAssetStreamer,
    pub(super) prebuilt_quads: RuntimePrebuiltRoomQuads,
    pub(super) room_projection: RuntimeCachedRoomProjection,
}

/// RAM union of the streamed front-end UI-image cache and every gameplay-only
/// asset arena. The larger gameplay side determines the allocation, so adding
/// menus does not add their image cache to the PS1's resident RAM total.
///
/// Safety contract (why the two sides are never live together):
/// - `ui_images` is written by the contiguous menu preload and read only
///   until the loading scene's images are uploaded to VRAM
///   (`prepare_loading_assets`), which uploads the loading art, invalidates
///   the UI cache, resets the gameplay asset streamer in place, and hands the
///   bytes over.
/// - `gameplay` is written only by the persistent-asset loader, room builds,
///   and world draws, all after that handoff point.
/// - Returning to a menu re-preloads the cache from CD because
///   gameplay exit drops every parsed view and reinitializes the UI cache's
///   metadata before `service_menu_ui_images` sees the union again.
pub(super) union FrontEndGameplayOverlay {
    pub(super) ui_images: core::mem::ManuallyDrop<RuntimeUiImageCache>,
    pub(super) gameplay: core::mem::ManuallyDrop<GameplayAssetArenas>,
}

impl RuntimeArenas {
    /// Link-time-zero image of the arena; every field is all-zero bytes
    /// so the static lands in `.bss` instead of storing ~430 KB in the
    /// flat PSX-EXE. NOT ready for use until [`Self::init`] runs.
    const ZEROED: Self = Self {
        vram: RuntimeVram::zeroed(),
        // These three are zero-init types: their `new()` state IS the
        // all-zero pattern, so they need no later stamping.
        load_render: LoadRenderOverlay {
            render: core::mem::ManuallyDrop::new(FrameRenderScratch::ZEROED),
        },
        // Both union variants are all-zero images, so initializing via
        // the gameplay side leaves the ui_images side valid-zeroed too.
        #[cfg(feature = "cd-stream-bench")]
        overlay: FrontEndGameplayOverlay {
            gameplay: core::mem::ManuallyDrop::new(GameplayAssetArenas {
                persistent_assets: RuntimePersistentAssetStreamer::zeroed(),
                prebuilt_quads: RuntimePrebuiltRoomQuads::zeroed(),
                room_projection: RuntimeCachedRoomProjection::zeroed(),
            }),
        },
        #[cfg(feature = "cd-stream-bench")]
        streamed_slots: RuntimeStreamedRoomSlots::new(),
        #[cfg(feature = "cd-stream-bench")]
        room_streams: RuntimeRoomStreamScheduler::zeroed(),
        #[cfg(feature = "cd-stream-bench")]
        room_materials: RuntimeRoomMaterialPool::zeroed(),
        #[cfg(not(feature = "cd-stream-bench"))]
        prebuilt_quads: RuntimePrebuiltRoomQuads::zeroed(),
        // Zero state = invalid cache key, so the first draw rebuilds;
        // no init stamping needed.
        sky: psx_game_runtime::sky::SkyCyclorama::zeroed(),
        #[cfg(feature = "world-grid-visible")]
        cell_scratch: RuntimeCellDrawScratch::zeroed(),
        model_scratch: RuntimeModelDrawScratch::zeroed(),
        debris_cache: psx_game_runtime::box_props::DebrisCache::zeroed(),
        #[cfg(not(feature = "cd-stream-bench"))]
        room_projection: RuntimeCachedRoomProjection::zeroed(),
        #[cfg(feature = "cd-stream-bench")]
        cd: psx_game_runtime::cd_stream::CdController::zeroed(),
    };

    /// Stamp the non-zero initial state (what the old per-instance
    /// static initializers stored in `.data`) onto the zeroed storage.
    fn init(&mut self) {
        self.vram = RuntimeVram::new(VRAM_LAYOUT);
        #[cfg(feature = "cd-stream-bench")]
        {
            self.room_streams = RuntimeRoomStreamScheduler::new();
            self.room_materials.init(room_material_fallback());
        }
        // SAFETY: boot-time init; the overlay's gameplay side is the
        // all-zero image here and reset_claims only stamps claim keys.
        #[cfg(feature = "cd-stream-bench")]
        unsafe {
            let gameplay =
                core::ptr::addr_of_mut!(self.overlay.gameplay).cast::<GameplayAssetArenas>();
            (*gameplay).prebuilt_quads.reset_claims();
        }
        #[cfg(not(feature = "cd-stream-bench"))]
        self.prebuilt_quads.reset_claims();
        self.debris_cache.init();
    }
}

/// The one arena static. Zero-initialized at link time (`.bss`);
/// [`init_runtime_arenas`] stamps the real initial state at boot.
static mut RUNTIME_ARENAS: RuntimeArenas = RuntimeArenas::ZEROED;

/// Raw projection base for the field accessors below.
fn arenas_ptr() -> *mut RuntimeArenas {
    core::ptr::addr_of_mut!(RUNTIME_ARENAS)
}

/// Initialize the arena state. Must run once, before
/// `App::run_with_flow` hands control to the engine (nothing may touch
/// the arenas before this).
pub(super) fn init_runtime_arenas() {
    // SAFETY: single-threaded boot path; no other arena borrow exists yet.
    unsafe { (*arenas_ptr()).init() }
}

/// One short-lived exclusive borrow of the VRAM runtime per glue call,
/// same discipline as the old `VRAM_RUNTIME` static.
pub(super) fn vram_arena() -> &'static mut RuntimeVram {
    // SAFETY: field projection of the arena static; distinct-field
    // borrows are disjoint, and same-field borrows follow the one-
    // short-lived-borrow-per-call rule (see the module doc).
    unsafe { &mut (*arenas_ptr()).vram }
}

/// Exclusive borrow of the shared font/sky staging scratch.
pub(super) fn font_scratch_arena() -> &'static mut RuntimeFontPackScratch {
    // SAFETY: font packing and sky staging complete before frame rendering,
    // so the `load` and `render` union views never have live overlapping
    // borrows. Both variants are all-zero-compatible at boot.
    unsafe {
        &mut *core::ptr::addr_of_mut!((*arenas_ptr()).load_render.load)
            .cast::<RuntimeFontPackScratch>()
    }
}

/// Exclusive borrow of the packet and world-command scratch for one frame.
pub(super) fn frame_render_scratch() -> &'static mut FrameRenderScratch {
    // SAFETY: see `font_scratch_arena`; load-time users have returned before
    // the render pass borrows this union view.
    unsafe {
        &mut *core::ptr::addr_of_mut!((*arenas_ptr()).load_render.render)
            .cast::<FrameRenderScratch>()
    }
}

/// Shared borrow of the streamed-UI image RAM cache.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn ui_images_arena() -> &'static RuntimeUiImageCache {
    // SAFETY: see `vram_arena` + the `FrontEndGameplayOverlay` contract.
    unsafe {
        &*core::ptr::addr_of!((*arenas_ptr()).overlay.ui_images).cast::<RuntimeUiImageCache>()
    }
}

/// Exclusive borrow of the streamed-UI image RAM cache.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn ui_images_arena_mut() -> &'static mut RuntimeUiImageCache {
    // SAFETY: see `vram_arena` + the `FrontEndGameplayOverlay` contract.
    unsafe {
        &mut *core::ptr::addr_of_mut!((*arenas_ptr()).overlay.ui_images)
            .cast::<RuntimeUiImageCache>()
    }
}

/// Shared borrow of the streamed-room page pool. Views resolved out
/// of the slots inherit the type's staleness contract: re-resolve after
/// every streaming step.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn streamed_slots_arena() -> &'static RuntimeStreamedRoomSlots {
    // SAFETY: see `vram_arena`.
    unsafe { &(*arenas_ptr()).streamed_slots }
}

/// Exclusive borrow of the streamed-room page pool (the CD pump's
/// write destination).
#[cfg(feature = "cd-stream-bench")]
pub(super) fn streamed_slots_arena_mut() -> &'static mut RuntimeStreamedRoomSlots {
    // SAFETY: see `vram_arena`.
    unsafe { &mut (*arenas_ptr()).streamed_slots }
}

/// Shared borrow of stable persistent gameplay asset storage.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn persistent_assets_arena() -> &'static RuntimePersistentAssetStreamer {
    // SAFETY: see `vram_arena` + the `FrontEndGameplayOverlay` contract.
    unsafe {
        let gameplay =
            core::ptr::addr_of!((*arenas_ptr()).overlay.gameplay).cast::<GameplayAssetArenas>();
        &(*gameplay).persistent_assets
    }
}

/// Exclusive borrow of the persistent gameplay asset loader and storage.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn persistent_assets_arena_mut() -> &'static mut RuntimePersistentAssetStreamer {
    // SAFETY: see `vram_arena` + the `FrontEndGameplayOverlay` contract.
    unsafe {
        let gameplay =
            core::ptr::addr_of_mut!((*arenas_ptr()).overlay.gameplay).cast::<GameplayAssetArenas>();
        &mut (*gameplay).persistent_assets
    }
}

/// Exclusive borrow of the streamed-room scheduler, same discipline as
/// the old `ROOM_STREAM_SCHEDULER` static.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn room_streams_arena() -> &'static mut RuntimeRoomStreamScheduler {
    // SAFETY: see `vram_arena`.
    unsafe { &mut (*arenas_ptr()).room_streams }
}

/// Shared borrow of the per-stream-slot room-material pool.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn room_materials_arena() -> &'static RuntimeRoomMaterialPool {
    // SAFETY: see `vram_arena`.
    unsafe { &(*arenas_ptr()).room_materials }
}

/// Exclusive borrow of the per-stream-slot room-material pool.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn room_materials_arena_mut() -> &'static mut RuntimeRoomMaterialPool {
    // SAFETY: see `vram_arena`.
    unsafe { &mut (*arenas_ptr()).room_materials }
}

/// Exclusive borrow of the prebuilt room-quad pool. The returned
/// `'static` claim slices stay writable across frames by design (the
/// present flip's DMA drain makes in-place patching safe).
pub(super) fn prebuilt_quads_arena() -> &'static mut RuntimePrebuiltRoomQuads {
    // SAFETY: see `vram_arena` + the `FrontEndGameplayOverlay` contract.
    #[cfg(feature = "cd-stream-bench")]
    unsafe {
        let gameplay =
            core::ptr::addr_of_mut!((*arenas_ptr()).overlay.gameplay).cast::<GameplayAssetArenas>();
        &mut (*gameplay).prebuilt_quads
    }
    #[cfg(not(feature = "cd-stream-bench"))]
    unsafe {
        &mut (*arenas_ptr()).prebuilt_quads
    }
}

/// Exclusive borrow of the sky-cyclorama packet cache.
pub(super) fn sky_arena() -> &'static mut psx_game_runtime::sky::SkyCyclorama {
    // SAFETY: see `vram_arena`.
    unsafe { &mut (*arenas_ptr()).sky }
}

/// Exclusive borrow of the accepted-cell draw scratch.
#[cfg(feature = "world-grid-visible")]
pub(super) fn cell_scratch_arena() -> &'static mut RuntimeCellDrawScratch {
    // SAFETY: see `vram_arena`.
    unsafe { &mut (*arenas_ptr()).cell_scratch }
}

/// Exclusive borrow of the model draw scratch.
pub(super) fn model_scratch_arena() -> &'static mut RuntimeModelDrawScratch {
    // SAFETY: see `vram_arena`.
    unsafe { &mut (*arenas_ptr()).model_scratch }
}

/// Exclusive borrow of the box-prop floor-debris cache.
pub(super) fn debris_cache_arena() -> &'static mut psx_game_runtime::box_props::DebrisCache {
    // SAFETY: see `vram_arena`.
    unsafe { &mut (*arenas_ptr()).debris_cache }
}

/// Exclusive borrow of the cached-room projection scratch.
pub(super) fn room_projection_arena() -> &'static mut RuntimeCachedRoomProjection {
    // SAFETY: see `vram_arena` + the `FrontEndGameplayOverlay` contract.
    #[cfg(feature = "cd-stream-bench")]
    unsafe {
        let gameplay =
            core::ptr::addr_of_mut!((*arenas_ptr()).overlay.gameplay).cast::<GameplayAssetArenas>();
        &mut (*gameplay).room_projection
    }
    #[cfg(not(feature = "cd-stream-bench"))]
    unsafe {
        &mut (*arenas_ptr()).room_projection
    }
}

/// Exclusive borrow of the CD controller driver state.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn cd_arena() -> &'static mut psx_game_runtime::cd_stream::CdController {
    // SAFETY: see `vram_arena`.
    unsafe { &mut (*arenas_ptr()).cd }
}
