//! The example's mutable runtime state, folded into ONE arena struct
//! behind ONE static (phase 1.5 of docs/game-runtime-plan.md). The
//! former `static mut` instances (`VRAM_RUNTIME`, `FONT_PACK_SCRATCH`,
//! `PRIMITIVE_PACKETS`, `WORLD_COMMANDS`, `UI_IMAGE_CACHE`, `PERSISTENT_ASSETS`)
//! are now [`RuntimeArenas`] fields.
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
//! SAME field at once. The frame-backend overlay below is the deliberate
//! exception: PXBSP frame-face collection finishes before model projection
//! starts, and its accessors document that shorter per-frame handoff.

use super::*;
use crate::generated::PXBSP_FACE_CHAIN_CAPACITY;

// A cube-only project never draws a panorama. Keep its unused packet cache
// out of PS1 RAM; mixed-sky projects still retain the ordinary panorama path.
const PANORAMA_CACHE_COUNT: usize = {
    let mut index = 0;
    let mut needed = 0;
    while index < ROOMS.len() {
        let flags = ROOMS[index].sky.flags;
        let projection = flags & psx_level::sky_flags::PROJECTION_MASK;
        if flags & psx_level::sky_flags::ENABLED != 0
            && (projection == 0 || projection == psx_level::sky_flags::PANORAMA)
        {
            needed = 1;
        }
        index += 1;
    }
    needed
};

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
    /// The resident PXBSP renderer's session-lifetime visibility face chain.
    pub(super) pxbsp_visible_faces: [u16; PXBSP_FACE_CHAIN_CAPACITY],
    /// Rotation-keyed sky-cyclorama packet cache (phase-2 sky carve).
    pub(super) sky: [psx_game_runtime::sky::SkyCyclorama; PANORAMA_CACHE_COUNT],
    /// Per-frame scratch shared by the PXBSP face filter and model submits.
    /// BSP static-world/mover drawing completes before any model path runs.
    pub(super) frame_backend: FrameWorldBackendOverlay,
    /// Break-time box-prop floor-debris cache (phase-2 box-prop carve).
    pub(super) debris_cache: RuntimeDebrisCache,
    /// Owned CD controller driver state (phase-2 retirement of the
    /// crate's carried `cd_stream` statics).
    #[cfg(feature = "cd-stream-bench")]
    pub(super) cd: psx_game_runtime::cd_stream::CdController,
}

/// Frame-lifetime overlay between BSP face filtering and model projection.
pub(super) union FrameWorldBackendOverlay {
    pub(super) model: core::mem::ManuallyDrop<RuntimeModelDrawScratch>,
    pub(super) pxbsp_frame_faces: core::mem::ManuallyDrop<[u16; PXBSP_FACE_CHAIN_CAPACITY]>,
}

// Keep the overlay honest as generated project capacities change. Rust
// unions use the largest variant (rounded to their common alignment); the
// model variant is 32-bit aligned and larger than the u16 chain, so the
// assertion failing means this carve no longer has the promised
// zero-static-RAM cost and must be reviewed at cook/runtime together.
const PXBSP_FACE_CHAIN_BYTES: usize = core::mem::size_of::<[u16; PXBSP_FACE_CHAIN_CAPACITY]>();
const FRAME_WORLD_OVERLAY_PAYLOAD_BYTES: usize =
    if core::mem::size_of::<RuntimeModelDrawScratch>() > PXBSP_FACE_CHAIN_BYTES {
        core::mem::size_of::<RuntimeModelDrawScratch>()
    } else {
        PXBSP_FACE_CHAIN_BYTES
    };
const fn aligned_union_bytes(payload: usize, alignment: usize) -> usize {
    ((payload + alignment - 1) / alignment) * alignment
}
const FRAME_WORLD_OVERLAY_BYTES: usize = aligned_union_bytes(
    FRAME_WORLD_OVERLAY_PAYLOAD_BYTES,
    core::mem::align_of::<FrameWorldBackendOverlay>(),
);
const _: () =
    assert!(core::mem::size_of::<FrameWorldBackendOverlay>() == FRAME_WORLD_OVERLAY_BYTES);

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
/// - `gameplay` is written only by the persistent-asset loader, after that
///   handoff point.
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
            }),
        },
        pxbsp_visible_faces: [0; PXBSP_FACE_CHAIN_CAPACITY],
        // Zero state = invalid cache key, so the first draw rebuilds;
        // no init stamping needed.
        sky: [const { psx_game_runtime::sky::SkyCyclorama::zeroed() }; PANORAMA_CACHE_COUNT],
        frame_backend: FrameWorldBackendOverlay {
            pxbsp_frame_faces: core::mem::ManuallyDrop::new([0; PXBSP_FACE_CHAIN_CAPACITY]),
        },
        debris_cache: RuntimeDebrisCache::zeroed(),
        #[cfg(feature = "cd-stream-bench")]
        cd: psx_game_runtime::cd_stream::CdController::zeroed(),
    };

    /// Stamp the non-zero initial state (what the old per-instance
    /// static initializers stored in `.data`) onto the zeroed storage.
    fn init(&mut self) {
        self.vram = RuntimeVram::new(VRAM_LAYOUT);
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

/// Exclusive borrow of the sky-cyclorama packet cache.
pub(super) fn sky_arena() -> Option<&'static mut psx_game_runtime::sky::SkyCyclorama> {
    // SAFETY: see `vram_arena`.
    unsafe { (*arenas_ptr()).sky.get_mut(0) }
}

/// Exclusive borrow of the model draw scratch.
pub(super) fn model_scratch_arena() -> &'static mut RuntimeModelDrawScratch {
    // SAFETY: PXBSP face filtering/mover drawing happens before all model
    // paths in `Playtest::render`; `draw_pxbsp_world` clears the frame-chain
    // length and marks before the model side may overwrite these bytes.
    unsafe {
        &mut *core::ptr::addr_of_mut!((*arenas_ptr()).frame_backend.model)
            .cast::<RuntimeModelDrawScratch>()
    }
}

/// Session-lifetime PXBSP visible-face chain.
pub(super) fn pxbsp_visible_face_chain_arena() -> &'static mut [u16; PXBSP_FACE_CHAIN_CAPACITY] {
    // SAFETY: see `vram_arena`.
    unsafe { &mut (*arenas_ptr()).pxbsp_visible_faces }
}

/// Per-frame PXBSP face-filter chain, live only until BSP drawing returns.
pub(super) fn pxbsp_frame_face_chain_arena() -> &'static mut [u16; PXBSP_FACE_CHAIN_CAPACITY] {
    // SAFETY: the BSP pass precedes every `model_scratch_arena` borrow and
    // retires this chain before returning. The model pass may then overwrite
    // the backing bytes while the retained Vec facade has length zero.
    unsafe {
        &mut *core::ptr::addr_of_mut!((*arenas_ptr()).frame_backend.pxbsp_frame_faces)
            .cast::<[u16; PXBSP_FACE_CHAIN_CAPACITY]>()
    }
}

/// Exclusive borrow of the box-prop floor-debris cache.
pub(super) fn debris_cache_arena() -> &'static mut RuntimeDebrisCache {
    // SAFETY: see `vram_arena`.
    unsafe { &mut (*arenas_ptr()).debris_cache }
}

/// Exclusive borrow of the CD controller driver state.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn cd_arena() -> &'static mut psx_game_runtime::cd_stream::CdController {
    // SAFETY: see `vram_arena`.
    unsafe { &mut (*arenas_ptr()).cd }
}
