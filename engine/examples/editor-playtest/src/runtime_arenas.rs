//! The example's mutable runtime state, folded into ONE arena struct
//! behind ONE static (phase 1.5 of docs/game-runtime-plan.md). The
//! seven former `static mut` instances (`VRAM_RUNTIME`,
//! `FONT_PACK_SCRATCH`, `UI_IMAGE_CACHE`, `STREAMED_ROOM_SLOTS`,
//! `ROOM_STREAM_SCHEDULER`, `ROOM_MATERIAL_POOL`,
//! `PREBUILT_ROOM_QUADS`) are now [`RuntimeArenas`] fields.
//!
//! Flat-binary discipline: the arena's ~430 KB of buffers must stay
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
//! next streaming step (the [`StreamedRoomSlots`] staleness contract).
//!
//! [`StreamedRoomSlots`]: psx_game_runtime::room_streaming::StreamedRoomSlots

use super::*;

/// Every mutable runtime arena, owned as one struct so the whole
/// mutable-state budget reads in one place (the scene's other state
/// lives on [`Playtest`]; this is the part its `const fn new` cannot
/// zero-initialize cheaply or that outlives scene borrows).
pub(super) struct RuntimeArenas {
    /// VRAM slot table, unified allocator, residency tracker, and
    /// upload queue (see `psx_game_runtime::vram::VramRuntime`).
    pub(super) vram: RuntimeVram,
    /// Shared font-pack / streamed-sky staging scratch.
    pub(super) font_scratch: RuntimeFontPackScratch,
    /// RAM cache for streamed front-end UI images.
    #[cfg(feature = "cd-stream-bench")]
    pub(super) ui_images: RuntimeUiImageCache,
    /// Streamed-room slot word buffers the CD loads land in.
    #[cfg(feature = "cd-stream-bench")]
    pub(super) streamed_slots: RuntimeStreamedRoomSlots,
    /// Streamed-room residency scheduler over `streamed_slots`.
    #[cfg(feature = "cd-stream-bench")]
    pub(super) room_streams: RuntimeRoomStreamScheduler,
    /// Room-surface materials pooled by resident stream slot.
    #[cfg(feature = "cd-stream-bench")]
    pub(super) room_materials: RuntimeRoomMaterialPool,
    /// Prebuilt GP0(3Ch) room-quad packets (docs/perf-30fps.md).
    pub(super) prebuilt_quads: RuntimePrebuiltRoomQuads,
}

impl RuntimeArenas {
    /// Link-time-zero image of the arena; every field is all-zero bytes
    /// so the static lands in `.bss` instead of storing ~430 KB in the
    /// flat PSX-EXE. NOT ready for use until [`Self::init`] runs.
    const ZEROED: Self = Self {
        vram: RuntimeVram::zeroed(),
        // These three are zero-init types: their `new()` state IS the
        // all-zero pattern, so they need no later stamping.
        font_scratch: RuntimeFontPackScratch::new(),
        #[cfg(feature = "cd-stream-bench")]
        ui_images: RuntimeUiImageCache::new(),
        #[cfg(feature = "cd-stream-bench")]
        streamed_slots: RuntimeStreamedRoomSlots::new(),
        #[cfg(feature = "cd-stream-bench")]
        room_streams: RuntimeRoomStreamScheduler::zeroed(),
        #[cfg(feature = "cd-stream-bench")]
        room_materials: RuntimeRoomMaterialPool::zeroed(),
        prebuilt_quads: RuntimePrebuiltRoomQuads::zeroed(),
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
        self.prebuilt_quads.reset_claims();
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
    // SAFETY: see `vram_arena`.
    unsafe { &mut (*arenas_ptr()).font_scratch }
}

/// Shared borrow of the streamed-UI image RAM cache.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn ui_images_arena() -> &'static RuntimeUiImageCache {
    // SAFETY: see `vram_arena`.
    unsafe { &(*arenas_ptr()).ui_images }
}

/// Exclusive borrow of the streamed-UI image RAM cache.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn ui_images_arena_mut() -> &'static mut RuntimeUiImageCache {
    // SAFETY: see `vram_arena`.
    unsafe { &mut (*arenas_ptr()).ui_images }
}

/// Shared borrow of the streamed-room slot buffers. Views resolved out
/// of the slots inherit the type's staleness contract: re-resolve after
/// every streaming step.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn streamed_slots_arena() -> &'static RuntimeStreamedRoomSlots {
    // SAFETY: see `vram_arena`.
    unsafe { &(*arenas_ptr()).streamed_slots }
}

/// Exclusive borrow of the streamed-room slot buffers (the CD pump's
/// write destination).
#[cfg(feature = "cd-stream-bench")]
pub(super) fn streamed_slots_arena_mut() -> &'static mut RuntimeStreamedRoomSlots {
    // SAFETY: see `vram_arena`.
    unsafe { &mut (*arenas_ptr()).streamed_slots }
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
    // SAFETY: see `vram_arena`.
    unsafe { &mut (*arenas_ptr()).prebuilt_quads }
}
