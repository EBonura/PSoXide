//! Lightweight guest-runtime telemetry for PSoXide host tooling.
//!
//! The runtime emits compact stage/counter events through an emulator-observed
//! Expansion 2 port. On non-MIPS host builds these functions compile to no-ops,
//! so editor-side preview code can depend on `psx-engine` without touching host
//! memory.

/// Runtime stage ids. Keep in sync with `emulator_core::telemetry::stage`.
pub mod stage {
    /// Per-frame gameplay/update work.
    pub const UPDATE: u16 = 1;
    /// Framebuffer clear before scene rendering.
    pub const FRAME_CLEAR: u16 = 2;
    /// Whole `Scene::render` call.
    pub const RENDER: u16 = 3;
    /// Present/vblank wait and framebuffer swap.
    pub const PRESENT: u16 = 4;
    /// Editor-playtest camera update.
    pub const CAMERA: u16 = 5;
    /// Grid-room surface rendering.
    pub const ROOM: u16 = 6;
    /// Legacy entity debug marker rendering.
    pub const ENTITY_MARKERS: u16 = 7;
    /// Placed model-instance rendering.
    pub const MODEL_INSTANCES: u16 = 8;
    /// Player model rendering.
    pub const PLAYER: u16 = 9;
    /// Whole-model bounds tests for placed model instances.
    pub const MODEL_BOUNDS: u16 = 13;
    /// Placed model draw calls after bounds culling.
    pub const MODEL_DRAW: u16 = 14;
    /// Whole-player bounds test.
    pub const PLAYER_BOUNDS: u16 = 15;
    /// Player model draw call after bounds culling.
    pub const PLAYER_DRAW: u16 = 16;
    /// Textured model joint pose sampling and transform setup.
    pub const TEXTURED_MODEL_JOINTS: u16 = 17;
    /// Textured model vertex projection.
    pub const TEXTURED_MODEL_PROJECT: u16 = 18;
    /// Textured model face culling, packet build, and command enqueue.
    pub const TEXTURED_MODEL_FACES: u16 = 19;
    /// Active room/chunk window rebuilds, including residency and cache setup.
    pub const ACTIVE_ROOM_WINDOW: u16 = 20;
    /// Runtime room surface-cache construction.
    pub const ROOM_SURFACE_CACHE: u16 = 21;
    /// Texture/atlas upload work.
    pub const VRAM_UPLOAD: u16 = 22;
    /// Editor-playtest CD streaming benchmark.
    pub const CD_STREAM_BENCH: u16 = 23;
    /// Steady-state portion of the editor-playtest CD streaming benchmark.
    pub const CD_STREAM_STEADY: u16 = 24;
    /// Sequential read of the real cooked world package.
    pub const CD_WORLD_PACK_STREAM: u16 = 25;
    /// Synchronous read of one streamed room chunk from WORLD.PAK.
    pub const CD_ROOM_CHUNK_LOAD: u16 = 26;
    /// Player-attached equipment / weapon rendering and hit-volume evaluation.
    pub const EQUIPMENT: u16 = 12;
    /// Deferred world-command sort and OT insertion.
    pub const WORLD_FLUSH: u16 = 10;
    /// Ordering-table DMA submission.
    pub const OT_SUBMIT: u16 = 11;
}

/// Runtime counter ids. Keep in sync with `emulator_core::telemetry::counter`.
pub mod counter {
    /// Textured primitive packets allocated this frame.
    pub const TRI_PRIMITIVES: u16 = 1;
    /// World render commands queued before flush.
    pub const WORLD_COMMANDS: u16 = 2;
    /// Placed model instances drawn.
    pub const MODEL_INSTANCE_DRAWS: u16 = 3;
    /// Vertices projected for placed model instances.
    pub const MODEL_INSTANCE_PROJECTED_VERTICES: u16 = 4;
    /// Triangles submitted for placed model instances.
    pub const MODEL_INSTANCE_SUBMITTED_TRIS: u16 = 5;
    /// Triangles culled for placed model instances.
    pub const MODEL_INSTANCE_CULLED_TRIS: u16 = 6;
    /// Triangles dropped for placed model instances.
    pub const MODEL_INSTANCE_DROPPED_TRIS: u16 = 7;
    /// Vertices projected for the player model.
    pub const PLAYER_PROJECTED_VERTICES: u16 = 8;
    /// Triangles submitted for the player model.
    pub const PLAYER_SUBMITTED_TRIS: u16 = 9;
    /// Triangles culled for the player model.
    pub const PLAYER_CULLED_TRIS: u16 = 10;
    /// Triangles dropped for the player model.
    pub const PLAYER_DROPPED_TRIS: u16 = 11;
    /// Bitfield of model-render overflow flags observed this frame.
    pub const MODEL_OVERFLOW_FLAGS: u16 = 12;
    /// Non-empty room grid cells considered by the visibility pass.
    pub const ROOM_CELLS_CONSIDERED: u16 = 13;
    /// Room grid cells drawn after visibility culling.
    pub const ROOM_CELLS_DRAWN: u16 = 14;
    /// Room grid cells rejected by the coarse frustum test.
    pub const ROOM_CELLS_CULLED: u16 = 15;
    /// Room floor/ceiling/wall surfaces considered for projection.
    pub const ROOM_SURFACES_CONSIDERED: u16 = 16;
    /// Player-attached equipment visuals drawn.
    pub const EQUIPMENT_DRAWS: u16 = 17;
    /// Active weapon hitboxes this frame.
    pub const EQUIPMENT_ACTIVE_HITBOXES: u16 = 18;
    /// Entity marker hits found by active weapon hitboxes.
    pub const EQUIPMENT_TARGET_HITS: u16 = 19;
    /// Vertices projected for equipment models.
    pub const EQUIPMENT_PROJECTED_VERTICES: u16 = 20;
    /// Triangles submitted for equipment models.
    pub const EQUIPMENT_SUBMITTED_TRIS: u16 = 21;
    /// Triangles culled for equipment models.
    pub const EQUIPMENT_CULLED_TRIS: u16 = 22;
    /// Triangles dropped for equipment models.
    pub const EQUIPMENT_DROPPED_TRIS: u16 = 23;
    /// Placed model instance bounds tests.
    pub const MODEL_INSTANCE_BOUNDS_TESTS: u16 = 24;
    /// Placed model instances rejected by whole-model bounds.
    pub const MODEL_INSTANCE_BOUNDS_CULLED: u16 = 25;
    /// Player bounds tests.
    pub const PLAYER_BOUNDS_TESTS: u16 = 26;
    /// Player draws rejected by whole-model bounds.
    pub const PLAYER_BOUNDS_CULLED: u16 = 27;
    /// Joints sampled for textured model submits.
    pub const TEXTURED_MODEL_JOINTS: u16 = 28;
    /// Parts walked for textured model submits.
    pub const TEXTURED_MODEL_PARTS: u16 = 29;
    /// Vertices projected for textured model submits.
    pub const TEXTURED_MODEL_VERTICES: u16 = 30;
    /// Face records considered by textured model submits.
    pub const TEXTURED_MODEL_FACES: u16 = 31;
    /// Active runtime room/chunk records walked this frame.
    pub const ROOM_ACTIVE_CHUNKS: u16 = 32;
    /// Precomputed/grid-visible cells supplied to the room renderer.
    pub const ROOM_VISIBLE_CELLS: u16 = 33;
    /// Active room/chunk draws that used the cached surface path.
    pub const ROOM_CACHED_DRAWS: u16 = 34;
    /// Active room/chunk draws that used the direct uncached path.
    pub const ROOM_UNCACHED_DRAWS: u16 = 35;
    /// Remaining primitive packet slots at the end of scene emission.
    pub const TRI_PRIMITIVE_REMAINING: u16 = 36;
    /// Cached room cell headers resident in the active chunk window.
    pub const ROOM_CACHE_CELLS: u16 = 37;
    /// Cached room vertices resident in the active chunk window.
    pub const ROOM_CACHE_VERTICES: u16 = 38;
    /// Cached room surfaces resident in the active chunk window.
    pub const ROOM_CACHE_SURFACES: u16 = 39;
    /// Active room/chunk draws that fell back because surface caching failed.
    pub const ROOM_CACHE_FALLBACK_DRAWS: u16 = 40;
    /// Active room/chunk draws that fell back because visibility cells were unavailable.
    pub const ROOM_VISIBILITY_FALLBACK_DRAWS: u16 = 41;
    /// Room cells rejected by the global player/camera range gate.
    pub const ROOM_CELLS_RANGE_CULLED: u16 = 42;
    /// Candidate chunks that were within activation range this frame.
    pub const ROOM_CHUNKS_CONSIDERED: u16 = 43;
    /// Candidate chunks skipped because the active cache budget was full.
    pub const ROOM_CHUNK_CACHE_SKIPS: u16 = 44;
    /// Active room/chunk windows rebuilt.
    pub const ROOM_WINDOW_REBUILDS: u16 = 45;
    /// Active chunks successfully built during room-window rebuilds.
    pub const ROOM_WINDOW_BUILT_CHUNKS: u16 = 46;
    /// Runtime room surface caches built.
    pub const ROOM_SURFACE_CACHE_BUILDS: u16 = 47;
    /// Cells emitted while building runtime room surface caches.
    pub const ROOM_SURFACE_CACHE_BUILD_CELLS: u16 = 48;
    /// Vertices emitted while building runtime room surface caches.
    pub const ROOM_SURFACE_CACHE_BUILD_VERTICES: u16 = 49;
    /// Surfaces emitted while building runtime room surface caches.
    pub const ROOM_SURFACE_CACHE_BUILD_SURFACES: u16 = 50;
    /// Room texture uploads performed.
    pub const ROOM_TEXTURE_UPLOADS: u16 = 51;
    /// Model atlas uploads performed.
    pub const MODEL_ATLAS_UPLOADS: u16 = 52;
    /// Fixed simulation/control ticks run by the cadence layer.
    pub const SIM_TICKS: u16 = 53;
    /// Rendered visual frames produced by the cadence layer.
    pub const VISUAL_FRAMES: u16 = 54;
    /// Visual VBlank slots intentionally held/skipped instead of rendered.
    pub const VISUAL_SKIPPED_VBLANKS: u16 = 55;
    /// Visual frames that missed their target cadence slot.
    pub const VISUAL_DEADLINE_MISSES: u16 = 56;
    /// Configured visual cadence interval in VBlanks.
    pub const VISUAL_INTERVAL_VBLANKS: u16 = 57;
    /// Worst observed lateness for a visual frame in VBlanks.
    pub const VISUAL_MAX_LATENESS_VBLANKS: u16 = 58;
    /// Bytes read by the editor-playtest CD streaming benchmark.
    pub const CD_STREAM_BENCH_BYTES: u16 = 59;
    /// Sectors read by the editor-playtest CD streaming benchmark.
    pub const CD_STREAM_BENCH_SECTORS: u16 = 60;
    /// Poll-loop iterations spent waiting on CD/DMA readiness.
    pub const CD_STREAM_BENCH_POLLS: u16 = 61;
    /// FNV checksum observed over the streamed benchmark payload.
    pub const CD_STREAM_BENCH_CHECKSUM: u16 = 62;
    /// Expected FNV checksum for the streamed benchmark payload.
    pub const CD_STREAM_BENCH_EXPECTED_CHECKSUM: u16 = 63;
    /// Status code for the editor-playtest CD streaming benchmark.
    pub const CD_STREAM_BENCH_STATUS: u16 = 64;
    /// Bytes read during the steady-state CD streaming benchmark window.
    pub const CD_STREAM_STEADY_BYTES: u16 = 65;
    /// Sectors read during the steady-state CD streaming benchmark window.
    pub const CD_STREAM_STEADY_SECTORS: u16 = 66;
    /// Bytes read from WORLD.PAK during the CD streaming benchmark.
    pub const CD_WORLD_PACK_BYTES: u16 = 67;
    /// Sectors read from WORLD.PAK during the CD streaming benchmark.
    pub const CD_WORLD_PACK_SECTORS: u16 = 68;
    /// Chunk entries reported by the streamed WORLD.PAK header.
    pub const CD_WORLD_PACK_CHUNKS: u16 = 69;
    /// FNV checksum observed over streamed WORLD.PAK sectors.
    pub const CD_WORLD_PACK_CHECKSUM: u16 = 70;
    /// Status code for streamed WORLD.PAK validation.
    pub const CD_WORLD_PACK_STATUS: u16 = 71;
    /// Room chunk bytes loaded from WORLD.PAK resident slots.
    pub const CD_ROOM_CHUNK_BYTES: u16 = 72;
    /// Room chunk sectors read from WORLD.PAK resident slots.
    pub const CD_ROOM_CHUNK_SECTORS: u16 = 73;
    /// Room chunk slot loads issued against WORLD.PAK.
    pub const CD_ROOM_CHUNK_LOADS: u16 = 74;
    /// Room chunk slot loads served from an already-resident slot.
    pub const CD_ROOM_CHUNK_HITS: u16 = 75;
    /// Status code for streamed room chunk loading.
    pub const CD_ROOM_CHUNK_STATUS: u16 = 76;
    /// Stream scheduler requests considered for the active window.
    pub const ROOM_STREAM_REQUESTS: u16 = 77;
    /// Stream scheduler requests that were not resident yet.
    pub const ROOM_STREAM_MISSES: u16 = 78;
    /// Stream scheduler requests issued only as prefetch/lookahead.
    pub const ROOM_STREAM_PREFETCH_REQUESTS: u16 = 79;
    /// Resident room stream slots after scheduler processing.
    pub const ROOM_STREAM_RESIDENT_SLOTS: u16 = 80;
    /// Resident stream slots evicted to satisfy requests.
    pub const ROOM_STREAM_EVICTIONS: u16 = 81;
    /// Stream slot loads that failed validation or CD reads.
    pub const ROOM_STREAM_FAILED_LOADS: u16 = 82;
    /// Stream slot loads scheduled by the current window refresh.
    pub const ROOM_STREAM_PENDING_LOADS: u16 = 83;
}

const EVENT_KIND_FRAME_BEGIN: u8 = 1;
const EVENT_KIND_STAGE_BEGIN: u8 = 2;
const EVENT_KIND_STAGE_END: u8 = 3;
const EVENT_KIND_COUNTER: u8 = 4;

#[cfg(target_arch = "mips")]
const EVENT_ADDR: *mut u32 = 0xBF80_2F00 as *mut u32;
#[cfg(target_arch = "mips")]
const VALUE_ADDR: *mut u32 = 0xBF80_2F04 as *mut u32;

/// Mark the start of a guest frame.
#[inline(always)]
pub fn frame_begin(frame: u32) {
    emit_value(frame);
    emit_event(EVENT_KIND_FRAME_BEGIN, 0);
}

/// Mark the start of a named stage.
#[inline(always)]
pub fn stage_begin(stage_id: u16) {
    emit_event(EVENT_KIND_STAGE_BEGIN, stage_id);
}

/// Mark the end of a named stage.
#[inline(always)]
pub fn stage_end(stage_id: u16) {
    emit_event(EVENT_KIND_STAGE_END, stage_id);
}

/// Emit a numeric counter value.
#[inline(always)]
pub fn counter(counter_id: u16, value: u32) {
    emit_value(value);
    emit_event(EVENT_KIND_COUNTER, counter_id);
}

#[cfg(target_arch = "mips")]
#[inline(always)]
fn encode_event(kind: u8, id: u16) -> u32 {
    ((kind as u32) << 24) | id as u32
}

#[cfg(target_arch = "mips")]
#[inline(always)]
fn emit_value(value: u32) {
    unsafe {
        core::ptr::write_volatile(VALUE_ADDR, value);
    }
}

#[cfg(not(target_arch = "mips"))]
#[inline(always)]
fn emit_value(_value: u32) {}

#[cfg(target_arch = "mips")]
#[inline(always)]
fn emit_event(kind: u8, id: u16) {
    unsafe {
        core::ptr::write_volatile(EVENT_ADDR, encode_event(kind, id));
    }
}

#[cfg(not(target_arch = "mips"))]
#[inline(always)]
fn emit_event(_kind: u8, _id: u16) {}

#[cfg(test)]
mod tests {
    use super::counter;

    #[test]
    fn frame_pacing_counter_ids_extend_existing_room_counters() {
        assert_eq!(counter::SIM_TICKS, counter::MODEL_ATLAS_UPLOADS + 1);
        assert_eq!(counter::VISUAL_FRAMES, counter::SIM_TICKS + 1);
        assert_eq!(
            counter::VISUAL_MAX_LATENESS_VBLANKS,
            counter::VISUAL_INTERVAL_VBLANKS + 1
        );
    }
}
