//! Guest-runtime telemetry captured out-of-band by the emulator.
//!
//! Instrumented homebrew writes compact event words into a reserved slice of
//! Expansion Region 2. Retail software sees the normal expansion-port
//! behaviour, while PSoXide can timestamp those writes with the emulator's bus
//! cycle counter and surface the result in host-side tooling.

use std::collections::VecDeque;

use psx_hw::memory;

/// Physical base of PSoXide's emulator-only telemetry port.
pub const BASE_PHYS: u32 = memory::expansion2::BASE + 0x0F00;
/// Event command register. A write appends one telemetry event.
pub const EVENT_PHYS: u32 = BASE_PHYS;
/// Event value register. The next command write snapshots this value.
pub const VALUE_PHYS: u32 = BASE_PHYS + 4;
/// Read-only low 32 bits of the emulator-observed guest cycle counter.
pub const CYCLE_PHYS: u32 = BASE_PHYS + 8;

const EVENT_CAP: usize = 65_536;
const KIND_SHIFT: u32 = 24;
const KIND_MASK: u32 = 0xFF;
const ID_MASK: u32 = 0xFFFF;

/// Runtime stage id constants shared with `psx-engine::telemetry`.
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
    /// Cached-room visible-cell/PVS list lookup.
    pub const ROOM_VISIBLE_LIST: u16 = 27;
    /// Cached-room visible-cell lookup and vertex-index gathering.
    pub const ROOM_CELL_SELECT: u16 = 28;
    /// Cached-room GTE/CPU vertex projection.
    pub const ROOM_PROJECT: u16 = 29;
    /// Cached-room per-vertex depth/fog preparation.
    pub const ROOM_DEPTH_PREP: u16 = 30;
    /// Cached-room surface culling, lighting, packet build, and command enqueue.
    pub const ROOM_SURFACE_DRAW: u16 = 31;
    /// Cooked sky/cyclorama backdrop rendering.
    pub const SKY: u16 = 32;
    /// Distant far-vista ring rendering.
    pub const FAR_VISTA: u16 = 33;
    /// Editor-authored image/card prop rendering.
    pub const IMAGE_PROPS: u16 = 34;
    /// Player-attached equipment / weapon rendering and hit-volume evaluation.
    pub const EQUIPMENT: u16 = 12;
    /// Deferred world-command sort and OT insertion.
    pub const WORLD_FLUSH: u16 = 10;
    /// Ordering-table DMA submission.
    pub const OT_SUBMIT: u16 = 11;
}

/// Number of stage slots, including index zero for unknown/reserved ids.
pub const STAGE_COUNT: usize = 35;

/// Runtime counter id constants shared with `psx-engine::telemetry`.
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
    /// Unique cached room vertices projected by visible cells.
    pub const ROOM_PROJECTED_VERTICES: u16 = 84;
    /// Cycles spent on room-surface material lookup/setup.
    pub const ROOM_SURF_MATERIAL_CYCLES: u16 = 85;
    /// Cycles spent fetching/validating projected room-surface quads.
    pub const ROOM_SURF_PROJECTED_CYCLES: u16 = 86;
    /// Cycles spent on room-surface screen culling.
    pub const ROOM_SURF_SCREEN_CYCLES: u16 = 87;
    /// Cycles spent classifying room-surface kind.
    pub const ROOM_SURF_KIND_CYCLES: u16 = 88;
    /// Cycles spent on room-surface backface culling.
    pub const ROOM_SURF_BACKFACE_CYCLES: u16 = 89;
    /// Cycles spent selecting baked/lit room-surface vertex colors.
    pub const ROOM_SURF_LIGHTING_CYCLES: u16 = 90;
    /// Cycles spent submitting room-surface packets/commands.
    pub const ROOM_SURF_SUBMIT_CYCLES: u16 = 91;
    /// Room surfaces sampled by the micro-profiler.
    pub const ROOM_SURF_PROFILED: u16 = 92;
    /// Room surfaces with missing material records.
    pub const ROOM_SURF_MATERIAL_MISSES: u16 = 93;
    /// Room surfaces rejected by projected-quad validity checks.
    pub const ROOM_SURF_PROJECTED_REJECTS: u16 = 94;
    /// Room surfaces culled by screen bounds.
    pub const ROOM_SURF_SCREEN_CULLED: u16 = 95;
    /// Room surfaces culled by backface tests.
    pub const ROOM_SURF_BACKFACE_CULLED: u16 = 96;
    /// Room floor surfaces sampled by the micro-profiler.
    pub const ROOM_SURF_FLOORS: u16 = 97;
    /// Room ceiling surfaces sampled by the micro-profiler.
    pub const ROOM_SURF_CEILINGS: u16 = 98;
    /// Room wall surfaces sampled by the micro-profiler.
    pub const ROOM_SURF_WALLS: u16 = 99;
    /// Whole-quad room surfaces sampled by the micro-profiler.
    pub const ROOM_SURF_WHOLE_QUADS: u16 = 100;
    /// Split-triangle room surfaces sampled by the micro-profiler.
    pub const ROOM_SURF_SPLIT_TRIS: u16 = 101;
    /// Room surfaces where color selection returned no drawable colors.
    pub const ROOM_SURF_LIGHTING_REJECTS: u16 = 102;
    /// Cycles spent checking cached room triangle hardware safety.
    pub const ROOM_SUBMIT_HW_SAFE_TEST_CYCLES: u16 = 103;
    /// Cycles spent building cached room triangle packet values.
    pub const ROOM_SUBMIT_PACKET_FILL_CYCLES: u16 = 104;
    /// Cycles spent pushing cached room triangle packets into primitive storage.
    pub const ROOM_SUBMIT_PRIMITIVE_PUSH_CYCLES: u16 = 105;
    /// Cycles spent calculating cached room triangle depth/order keys.
    pub const ROOM_SUBMIT_DEPTH_CYCLES: u16 = 106;
    /// Cycles spent pushing cached room triangle world commands.
    pub const ROOM_SUBMIT_COMMAND_CYCLES: u16 = 107;
    /// Cycles spent in cached room triangle fallback split/general path.
    pub const ROOM_SUBMIT_FALLBACK_CYCLES: u16 = 108;
    /// Cached room triangle submits that used the hardware-safe fast path.
    pub const ROOM_SUBMIT_HW_SAFE_CALLS: u16 = 109;
    /// Cached room triangle submits that used the split/general fallback path.
    pub const ROOM_SUBMIT_FALLBACK_CALLS: u16 = 110;
    /// Cached room triangle submits rejected by command-buffer capacity.
    pub const ROOM_SUBMIT_COMMAND_OVERFLOWS: u16 = 111;
    /// Cached room triangle submits rejected by primitive-buffer capacity.
    pub const ROOM_SUBMIT_PRIMITIVE_OVERFLOWS: u16 = 112;
    /// Guest cycles spent rendering runtime model slot 0.
    pub const MODEL_PROFILE_CYCLES_0: u16 = 113;
    /// Guest cycles spent rendering runtime model slot 1.
    pub const MODEL_PROFILE_CYCLES_1: u16 = 114;
    /// Guest cycles spent rendering runtime model slot 2.
    pub const MODEL_PROFILE_CYCLES_2: u16 = 115;
    /// Guest cycles spent rendering runtime model slot 3.
    pub const MODEL_PROFILE_CYCLES_3: u16 = 116;
    /// Guest cycles spent rendering runtime model slot 4.
    pub const MODEL_PROFILE_CYCLES_4: u16 = 117;
    /// Guest cycles spent rendering runtime model slot 5.
    pub const MODEL_PROFILE_CYCLES_5: u16 = 118;
    /// Guest cycles spent rendering runtime model slot 6.
    pub const MODEL_PROFILE_CYCLES_6: u16 = 119;
    /// Guest cycles spent rendering runtime model slot 7.
    pub const MODEL_PROFILE_CYCLES_7: u16 = 120;
    /// Runtime model slot 0 draw submits.
    pub const MODEL_PROFILE_DRAWS_0: u16 = 121;
    /// Runtime model slot 1 draw submits.
    pub const MODEL_PROFILE_DRAWS_1: u16 = 122;
    /// Runtime model slot 2 draw submits.
    pub const MODEL_PROFILE_DRAWS_2: u16 = 123;
    /// Runtime model slot 3 draw submits.
    pub const MODEL_PROFILE_DRAWS_3: u16 = 124;
    /// Runtime model slot 4 draw submits.
    pub const MODEL_PROFILE_DRAWS_4: u16 = 125;
    /// Runtime model slot 5 draw submits.
    pub const MODEL_PROFILE_DRAWS_5: u16 = 126;
    /// Runtime model slot 6 draw submits.
    pub const MODEL_PROFILE_DRAWS_6: u16 = 127;
    /// Runtime model slot 7 draw submits.
    pub const MODEL_PROFILE_DRAWS_7: u16 = 128;
    /// Low 32 bits of the resident streamed room/chunk bitset.
    pub const ROOM_STREAM_RESIDENT_MASK_LO: u16 = 129;
    /// High 32 bits of the resident streamed room/chunk bitset.
    pub const ROOM_STREAM_RESIDENT_MASK_HI: u16 = 130;
    /// Low 32 bits of the active drawable room/chunk bitset.
    pub const ROOM_ACTIVE_CHUNK_MASK_LO: u16 = 131;
    /// High 32 bits of the active drawable room/chunk bitset.
    pub const ROOM_ACTIVE_CHUNK_MASK_HI: u16 = 132;
    /// Low 32 bits of the room/chunk bitset that submitted room geometry.
    pub const ROOM_DRAWN_CHUNK_MASK_LO: u16 = 133;
    /// High 32 bits of the room/chunk bitset that submitted room geometry.
    pub const ROOM_DRAWN_CHUNK_MASK_HI: u16 = 134;
    /// Runtime room/chunk index containing the player.
    pub const ROOM_PLAYER_ROOM_INDEX: u16 = 135;
    /// Player room-local X, biased for unsigned telemetry transport.
    pub const ROOM_PLAYER_LOCAL_X_BIASED: u16 = 136;
    /// Player room-local Z, biased for unsigned telemetry transport.
    pub const ROOM_PLAYER_LOCAL_Z_BIASED: u16 = 137;
    /// Camera/view yaw used by player-centred chunk diagnostics, in Q12 angle units.
    pub const ROOM_PLAYER_VIEW_YAW_Q12: u16 = 138;
    /// Current room used as the root of portal traversal.
    pub const PORTAL_VIS_CURRENT_ROOM: u16 = 139;
    /// Portal-visible rooms accepted by the runtime traversal.
    pub const PORTAL_VIS_VISIBLE_ROOMS: u16 = 140;
    /// Rooms one portal beyond the visible set.
    pub const PORTAL_VIS_FRONTIER_ROOMS: u16 = 141;
    /// Portal frustums accepted by the runtime traversal.
    pub const PORTAL_VIS_FRUSTUMS: u16 = 142;
    /// Directed portals tested by the runtime traversal.
    pub const PORTAL_VIS_PORTALS_TESTED: u16 = 143;
    /// Directed portals accepted by the runtime traversal.
    pub const PORTAL_VIS_PORTALS_ACCEPTED: u16 = 144;
    /// Portals rejected by source-facing backface tests.
    pub const PORTAL_VIS_REJECT_BACKFACE: u16 = 145;
    /// Portals rejected by camera/window clipping.
    pub const PORTAL_VIS_REJECT_FRUSTUM: u16 = 146;
    /// Portals rejected because the clipped cone was tiny.
    pub const PORTAL_VIS_REJECT_TINY: u16 = 147;
    /// Visible-room pool capacity hits.
    pub const PORTAL_VIS_CAP_ROOM: u16 = 148;
    /// Frustum pool capacity hits.
    pub const PORTAL_VIS_CAP_FRUSTUM: u16 = 149;
    /// Portal traversal max-depth hits.
    pub const PORTAL_VIS_CAP_DEPTH: u16 = 150;
    /// Portal-visible rooms neither resident nor loading when the active window was built.
    pub const PORTAL_VIS_VISIBLE_MISSING_RESIDENT: u16 = 151;
    /// Stream priority requests for the current room.
    pub const ROOM_STREAM_PRIORITY_CURRENT: u16 = 152;
    /// Stream priority requests for portal-visible rooms.
    pub const ROOM_STREAM_PRIORITY_VISIBLE: u16 = 153;
    /// Stream priority requests for portal-frontier rooms.
    pub const ROOM_STREAM_PRIORITY_FRONTIER: u16 = 154;
    /// Stream loads blocked because resident/requested rooms filled the pool.
    pub const ROOM_STREAM_PROTECTED_FULL: u16 = 155;
    /// Low 32 bits of the portal-visible room bitset.
    pub const PORTAL_VIS_VISIBLE_MASK_LO: u16 = 156;
    /// High 32 bits of the portal-visible room bitset.
    pub const PORTAL_VIS_VISIBLE_MASK_HI: u16 = 157;
    /// Low 32 bits of the portal-frontier room bitset.
    pub const PORTAL_VIS_FRONTIER_MASK_LO: u16 = 158;
    /// High 32 bits of the portal-frontier room bitset.
    pub const PORTAL_VIS_FRONTIER_MASK_HI: u16 = 159;
    /// Low 32 bits of the visible-but-missing-residency room bitset.
    pub const PORTAL_VIS_MISSING_MASK_LO: u16 = 160;
    /// High 32 bits of the visible-but-missing-residency room bitset.
    pub const PORTAL_VIS_MISSING_MASK_HI: u16 = 161;
    /// Render camera room-local X, biased for unsigned telemetry transport.
    pub const ROOM_CAMERA_LOCAL_X_BIASED: u16 = 162;
    /// Render camera room-local Z, biased for unsigned telemetry transport.
    pub const ROOM_CAMERA_LOCAL_Z_BIASED: u16 = 163;
    /// Low 32 bits of destination rooms for portals tested this frame.
    pub const PORTAL_VIS_TESTED_MASK_LO: u16 = 164;
    /// High 32 bits of destination rooms for portals tested this frame.
    pub const PORTAL_VIS_TESTED_MASK_HI: u16 = 165;
    /// Low 32 bits of destination rooms for accepted portals this frame.
    pub const PORTAL_VIS_ACCEPTED_MASK_LO: u16 = 166;
    /// High 32 bits of destination rooms for accepted portals this frame.
    pub const PORTAL_VIS_ACCEPTED_MASK_HI: u16 = 167;
    /// Low 32 bits of destination rooms rejected by portal window clipping.
    pub const PORTAL_VIS_REJECT_FRUSTUM_MASK_LO: u16 = 168;
    /// High 32 bits of destination rooms rejected by portal window clipping.
    pub const PORTAL_VIS_REJECT_FRUSTUM_MASK_HI: u16 = 169;
    /// Portals recovered by occupied-room-bounds fallback.
    pub const PORTAL_VIS_BOUNDS_FALLBACKS: u16 = 170;
    /// Low 32 bits of destination rooms recovered by occupied-room-bounds fallback.
    pub const PORTAL_VIS_BOUNDS_FALLBACK_MASK_LO: u16 = 171;
    /// High 32 bits of destination rooms recovered by occupied-room-bounds fallback.
    pub const PORTAL_VIS_BOUNDS_FALLBACK_MASK_HI: u16 = 172;
    /// Effective resident streamed room slot limit for the current window.
    pub const ROOM_STREAM_SLOT_LIMIT: u16 = 173;
    /// Low 32 bits of rooms with in-flight streamed loads.
    pub const ROOM_STREAM_LOADING_MASK_LO: u16 = 174;
    /// High 32 bits of rooms with in-flight streamed loads.
    pub const ROOM_STREAM_LOADING_MASK_HI: u16 = 175;
    /// Portal-visible rooms resident in the stream cache but not buildable.
    pub const PORTAL_VIS_VISIBLE_BUILD_FAILED: u16 = 176;
    /// Low 32 bits of visible resident rooms that failed active-room build.
    pub const PORTAL_VIS_BUILD_FAILED_MASK_LO: u16 = 177;
    /// High 32 bits of visible resident rooms that failed active-room build.
    pub const PORTAL_VIS_BUILD_FAILED_MASK_HI: u16 = 178;
    /// Low 32 bits of directed portal records tested this frame.
    pub const PORTAL_VIS_TESTED_PORTAL_MASK_LO: u16 = 179;
    /// High 32 bits of directed portal records tested this frame.
    pub const PORTAL_VIS_TESTED_PORTAL_MASK_HI: u16 = 180;
    /// Low 32 bits of directed portal records accepted this frame.
    pub const PORTAL_VIS_ACCEPTED_PORTAL_MASK_LO: u16 = 181;
    /// High 32 bits of directed portal records accepted this frame.
    pub const PORTAL_VIS_ACCEPTED_PORTAL_MASK_HI: u16 = 182;
    /// Low 32 bits of directed portal records rejected by camera/window clipping.
    pub const PORTAL_VIS_REJECT_FRUSTUM_PORTAL_MASK_LO: u16 = 183;
    /// High 32 bits of directed portal records rejected by camera/window clipping.
    pub const PORTAL_VIS_REJECT_FRUSTUM_PORTAL_MASK_HI: u16 = 184;
    /// Low 32 bits of directed portal records accepted by occupied-bounds fallback.
    pub const PORTAL_VIS_BOUNDS_FALLBACK_PORTAL_MASK_LO: u16 = 185;
    /// High 32 bits of directed portal records accepted by occupied-bounds fallback.
    pub const PORTAL_VIS_BOUNDS_FALLBACK_PORTAL_MASK_HI: u16 = 186;
    /// Render camera yaw sine in Q12, biased by 4096 for unsigned transport.
    pub const ROOM_CAMERA_VIEW_SIN_YAW_Q12_BIASED: u16 = 187;
    /// Render camera yaw cosine in Q12, biased by 4096 for unsigned transport.
    pub const ROOM_CAMERA_VIEW_COS_YAW_Q12_BIASED: u16 = 188;
}

/// Number of counter slots, including index zero for unknown/reserved ids.
pub const COUNTER_COUNT: usize = 189;

/// Telemetry event kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuestTelemetryKind {
    /// A new guest frame began; `value` is the guest frame index.
    FrameBegin,
    /// A named runtime stage began.
    StageBegin,
    /// A named runtime stage ended.
    StageEnd,
    /// A numeric counter was emitted.
    Counter,
    /// Unknown event kind preserved for diagnostics.
    Unknown(u8),
}

impl GuestTelemetryKind {
    fn from_raw(raw: u8) -> Self {
        match raw {
            1 => Self::FrameBegin,
            2 => Self::StageBegin,
            3 => Self::StageEnd,
            4 => Self::Counter,
            other => Self::Unknown(other),
        }
    }
}

/// One telemetry event timestamped by the emulator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuestTelemetryEvent {
    /// Bus cycles elapsed when the guest wrote the event command.
    pub cycles: u64,
    /// Event kind.
    pub kind: GuestTelemetryKind,
    /// Stage or counter id, depending on [`kind`](Self::kind).
    pub id: u16,
    /// Latched value from [`VALUE_PHYS`].
    pub value: u32,
}

/// Rolling capture buffer for guest telemetry events.
pub struct GuestTelemetry {
    pending_value: u32,
    events: VecDeque<GuestTelemetryEvent>,
    frames_seen: u64,
    counter_totals: [u64; COUNTER_COUNT],
    counter_max_values: [u32; COUNTER_COUNT],
    counter_latest_values: [u32; COUNTER_COUNT],
}

impl Default for GuestTelemetry {
    fn default() -> Self {
        Self {
            pending_value: 0,
            events: VecDeque::with_capacity(EVENT_CAP),
            frames_seen: 0,
            counter_totals: [0; COUNTER_COUNT],
            counter_max_values: [0; COUNTER_COUNT],
            counter_latest_values: [0; COUNTER_COUNT],
        }
    }
}

impl GuestTelemetry {
    /// Create an empty telemetry buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// True if `phys` lands inside the telemetry port.
    pub const fn contains(phys: u32) -> bool {
        phys == EVENT_PHYS || phys == VALUE_PHYS || phys == CYCLE_PHYS
    }

    /// Observe a 32-bit read. Returns a value if the telemetry port consumed it.
    pub const fn observe_read32(&self, phys: u32, cycles: u64) -> Option<u32> {
        match phys {
            CYCLE_PHYS => Some(cycles as u32),
            _ => None,
        }
    }

    /// Observe a 32-bit write. Returns true if the telemetry port consumed it.
    pub fn observe_write32(&mut self, phys: u32, value: u32, cycles: u64) -> bool {
        match phys {
            VALUE_PHYS => {
                self.pending_value = value;
                true
            }
            EVENT_PHYS => {
                let raw_kind = ((value >> KIND_SHIFT) & KIND_MASK) as u8;
                let id = (value & ID_MASK) as u16;
                self.push(GuestTelemetryEvent {
                    cycles,
                    kind: GuestTelemetryKind::from_raw(raw_kind),
                    id,
                    value: self.pending_value,
                });
                true
            }
            _ => false,
        }
    }

    /// Drain all captured events in chronological order.
    pub fn drain_events(&mut self) -> Vec<GuestTelemetryEvent> {
        self.events.drain(..).collect()
    }

    /// Number of guest frame-begin markers observed since reset.
    pub const fn frames_seen(&self) -> u64 {
        self.frames_seen
    }

    /// Summed value observed for a known counter since reset.
    pub fn counter_total(&self, id: u16) -> u64 {
        self.counter_totals
            .get(id as usize)
            .copied()
            .unwrap_or_default()
    }

    /// Largest single value observed for a known counter since reset.
    pub fn counter_max_value(&self, id: u16) -> u32 {
        self.counter_max_values
            .get(id as usize)
            .copied()
            .unwrap_or_default()
    }

    /// Most recent single value observed for a known counter since reset.
    pub fn counter_latest_value(&self, id: u16) -> u32 {
        self.counter_latest_values
            .get(id as usize)
            .copied()
            .unwrap_or_default()
    }

    /// Snapshot of all summed counter values observed since reset.
    pub const fn counter_totals(&self) -> [u64; COUNTER_COUNT] {
        self.counter_totals
    }

    /// Snapshot of all largest counter values observed since reset.
    pub const fn counter_max_values(&self) -> [u32; COUNTER_COUNT] {
        self.counter_max_values
    }

    /// Snapshot of the most recent counter values observed since reset.
    pub const fn counter_latest_values(&self) -> [u32; COUNTER_COUNT] {
        self.counter_latest_values
    }

    fn push(&mut self, event: GuestTelemetryEvent) {
        if matches!(event.kind, GuestTelemetryKind::FrameBegin) {
            self.frames_seen = self.frames_seen.saturating_add(1);
        }
        if matches!(event.kind, GuestTelemetryKind::Counter) {
            if let Some(total) = self.counter_totals.get_mut(event.id as usize) {
                *total = total.saturating_add(event.value as u64);
            }
            if let Some(max_value) = self.counter_max_values.get_mut(event.id as usize) {
                *max_value = (*max_value).max(event.value);
            }
            if let Some(latest_value) = self.counter_latest_values.get_mut(event.id as usize) {
                *latest_value = event.value;
            }
        }
        if self.events.len() >= EVENT_CAP {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }
}

/// Aggregated guest telemetry over a span of events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuestTelemetrySummary {
    /// Number of guest frame-begin markers observed.
    pub frames: u64,
    /// Total cycles spent in each known stage id.
    pub stage_cycles: [u64; STAGE_COUNT],
    /// Number of completed spans per known stage id.
    pub stage_hits: [u64; STAGE_COUNT],
    /// Largest single completed span per known stage id.
    pub stage_max_cycles: [u64; STAGE_COUNT],
    /// Summed counter values per known counter id.
    pub counters: [u64; COUNTER_COUNT],
    /// Largest single value observed per known counter id.
    pub counter_max_values: [u32; COUNTER_COUNT],
    /// Most recent value observed per known counter id.
    pub counter_latest_values: [u32; COUNTER_COUNT],
}

impl Default for GuestTelemetrySummary {
    fn default() -> Self {
        Self {
            frames: 0,
            stage_cycles: [0; STAGE_COUNT],
            stage_hits: [0; STAGE_COUNT],
            stage_max_cycles: [0; STAGE_COUNT],
            counters: [0; COUNTER_COUNT],
            counter_max_values: [0; COUNTER_COUNT],
            counter_latest_values: [0; COUNTER_COUNT],
        }
    }
}

impl GuestTelemetrySummary {
    /// Build a summary from raw telemetry events.
    pub fn from_events(events: &[GuestTelemetryEvent]) -> Self {
        let mut out = Self::default();
        out.add_events(events);
        out
    }

    /// Add raw events to this summary.
    pub fn add_events(&mut self, events: &[GuestTelemetryEvent]) {
        let mut stage_start: [Option<u64>; STAGE_COUNT] = [None; STAGE_COUNT];
        for event in events {
            match event.kind {
                GuestTelemetryKind::FrameBegin => {
                    self.frames = self.frames.saturating_add(1);
                }
                GuestTelemetryKind::StageBegin => {
                    if let Some(slot) = stage_start.get_mut(event.id as usize) {
                        *slot = Some(event.cycles);
                    }
                }
                GuestTelemetryKind::StageEnd => {
                    let Some(slot) = stage_start.get_mut(event.id as usize) else {
                        continue;
                    };
                    let Some(start) = slot.take() else {
                        continue;
                    };
                    let idx = event.id as usize;
                    let elapsed = event.cycles.saturating_sub(start);
                    self.stage_cycles[idx] = self.stage_cycles[idx].saturating_add(elapsed);
                    self.stage_hits[idx] = self.stage_hits[idx].saturating_add(1);
                    self.stage_max_cycles[idx] = self.stage_max_cycles[idx].max(elapsed);
                }
                GuestTelemetryKind::Counter => {
                    let idx = event.id as usize;
                    if let Some(counter) = self.counters.get_mut(idx) {
                        *counter = counter.saturating_add(event.value as u64);
                    }
                    if let Some(max_value) = self.counter_max_values.get_mut(idx) {
                        *max_value = (*max_value).max(event.value);
                    }
                    if let Some(latest_value) = self.counter_latest_values.get_mut(idx) {
                        *latest_value = event.value;
                    }
                }
                GuestTelemetryKind::Unknown(_) => {}
            }
        }
    }

    /// True when at least one event contributed useful data.
    pub fn has_data(&self) -> bool {
        self.frames > 0
            || self.stage_cycles.iter().any(|&cycles| cycles > 0)
            || self.counters.iter().any(|&value| value > 0)
    }
}

/// Human-readable stage name for host tooling.
pub fn stage_name(id: u16) -> &'static str {
    match id {
        stage::UPDATE => "update",
        stage::FRAME_CLEAR => "frame clear",
        stage::RENDER => "render total",
        stage::PRESENT => "present/wait",
        stage::CAMERA => "camera",
        stage::ROOM => "room",
        stage::ENTITY_MARKERS => "entity markers",
        stage::MODEL_INSTANCES => "model instances",
        stage::PLAYER => "player",
        stage::MODEL_BOUNDS => "model bounds",
        stage::MODEL_DRAW => "model draw",
        stage::PLAYER_BOUNDS => "player bounds",
        stage::PLAYER_DRAW => "player draw",
        stage::TEXTURED_MODEL_JOINTS => "mdl joints",
        stage::TEXTURED_MODEL_PROJECT => "mdl project",
        stage::TEXTURED_MODEL_FACES => "mdl faces",
        stage::ACTIVE_ROOM_WINDOW => "room window",
        stage::ROOM_SURFACE_CACHE => "room cache build",
        stage::VRAM_UPLOAD => "vram upload",
        stage::CD_STREAM_BENCH => "cd stream bench",
        stage::CD_STREAM_STEADY => "cd stream steady",
        stage::CD_WORLD_PACK_STREAM => "cd world pack",
        stage::CD_ROOM_CHUNK_LOAD => "cd room chunk load",
        stage::ROOM_VISIBLE_LIST => "room visible list",
        stage::ROOM_CELL_SELECT => "room cell select",
        stage::ROOM_PROJECT => "room project",
        stage::ROOM_DEPTH_PREP => "room depth prep",
        stage::ROOM_SURFACE_DRAW => "room surface draw",
        stage::SKY => "sky",
        stage::FAR_VISTA => "far vista",
        stage::IMAGE_PROPS => "image props",
        stage::EQUIPMENT => "equipment",
        stage::WORLD_FLUSH => "world flush/sort",
        stage::OT_SUBMIT => "ot submit",
        _ => "unknown",
    }
}

/// Human-readable counter name for host tooling.
pub fn counter_name(id: u16) -> &'static str {
    match id {
        counter::TRI_PRIMITIVES => "tri prims",
        counter::WORLD_COMMANDS => "world commands",
        counter::MODEL_INSTANCE_DRAWS => "model draws",
        counter::MODEL_INSTANCE_PROJECTED_VERTICES => "model verts",
        counter::MODEL_INSTANCE_SUBMITTED_TRIS => "model tris",
        counter::MODEL_INSTANCE_CULLED_TRIS => "model culled",
        counter::MODEL_INSTANCE_DROPPED_TRIS => "model dropped",
        counter::PLAYER_PROJECTED_VERTICES => "player verts",
        counter::PLAYER_SUBMITTED_TRIS => "player tris",
        counter::PLAYER_CULLED_TRIS => "player culled",
        counter::PLAYER_DROPPED_TRIS => "player dropped",
        counter::MODEL_OVERFLOW_FLAGS => "overflow flags",
        counter::ROOM_CELLS_CONSIDERED => "room cells",
        counter::ROOM_CELLS_DRAWN => "room cells drawn",
        counter::ROOM_CELLS_CULLED => "room cells culled",
        counter::ROOM_SURFACES_CONSIDERED => "room surfaces",
        counter::EQUIPMENT_DRAWS => "equipment draws",
        counter::EQUIPMENT_ACTIVE_HITBOXES => "weapon hitboxes",
        counter::EQUIPMENT_TARGET_HITS => "weapon hits",
        counter::EQUIPMENT_PROJECTED_VERTICES => "equipment verts",
        counter::EQUIPMENT_SUBMITTED_TRIS => "equipment tris",
        counter::EQUIPMENT_CULLED_TRIS => "equipment culled",
        counter::EQUIPMENT_DROPPED_TRIS => "equipment dropped",
        counter::MODEL_INSTANCE_BOUNDS_TESTS => "model bound tests",
        counter::MODEL_INSTANCE_BOUNDS_CULLED => "model bound culled",
        counter::PLAYER_BOUNDS_TESTS => "player bound tests",
        counter::PLAYER_BOUNDS_CULLED => "player bound culled",
        counter::TEXTURED_MODEL_JOINTS => "mdl joints",
        counter::TEXTURED_MODEL_PARTS => "mdl parts",
        counter::TEXTURED_MODEL_VERTICES => "mdl verts",
        counter::TEXTURED_MODEL_FACES => "mdl faces",
        counter::ROOM_ACTIVE_CHUNKS => "room chunks",
        counter::ROOM_VISIBLE_CELLS => "room visible cells",
        counter::ROOM_CACHED_DRAWS => "room cached draws",
        counter::ROOM_UNCACHED_DRAWS => "room uncached draws",
        counter::TRI_PRIMITIVE_REMAINING => "tri slots free",
        counter::ROOM_CACHE_CELLS => "room cache cells",
        counter::ROOM_CACHE_VERTICES => "room cache verts",
        counter::ROOM_CACHE_SURFACES => "room cache surfaces",
        counter::ROOM_CACHE_FALLBACK_DRAWS => "room cache fallbacks",
        counter::ROOM_VISIBILITY_FALLBACK_DRAWS => "room visibility fallbacks",
        counter::ROOM_CELLS_RANGE_CULLED => "room range culled",
        counter::ROOM_CHUNKS_CONSIDERED => "room chunks considered",
        counter::ROOM_CHUNK_CACHE_SKIPS => "room chunk cache skips",
        counter::ROOM_WINDOW_REBUILDS => "room window rebuilds",
        counter::ROOM_WINDOW_BUILT_CHUNKS => "room window chunks",
        counter::ROOM_SURFACE_CACHE_BUILDS => "room cache builds",
        counter::ROOM_SURFACE_CACHE_BUILD_CELLS => "cache build cells",
        counter::ROOM_SURFACE_CACHE_BUILD_VERTICES => "cache build verts",
        counter::ROOM_SURFACE_CACHE_BUILD_SURFACES => "cache build surfaces",
        counter::ROOM_TEXTURE_UPLOADS => "room texture uploads",
        counter::MODEL_ATLAS_UPLOADS => "model atlas uploads",
        counter::SIM_TICKS => "sim ticks",
        counter::VISUAL_FRAMES => "visual frames",
        counter::VISUAL_SKIPPED_VBLANKS => "visual skipped vblanks",
        counter::VISUAL_DEADLINE_MISSES => "visual deadline misses",
        counter::VISUAL_INTERVAL_VBLANKS => "visual interval vblanks",
        counter::VISUAL_MAX_LATENESS_VBLANKS => "visual max lateness vblanks",
        counter::CD_STREAM_BENCH_BYTES => "cd stream bytes",
        counter::CD_STREAM_BENCH_SECTORS => "cd stream sectors",
        counter::CD_STREAM_BENCH_POLLS => "cd stream polls",
        counter::CD_STREAM_BENCH_CHECKSUM => "cd stream checksum",
        counter::CD_STREAM_BENCH_EXPECTED_CHECKSUM => "cd stream expected checksum",
        counter::CD_STREAM_BENCH_STATUS => "cd stream status",
        counter::CD_STREAM_STEADY_BYTES => "cd steady bytes",
        counter::CD_STREAM_STEADY_SECTORS => "cd steady sectors",
        counter::CD_WORLD_PACK_BYTES => "cd world bytes",
        counter::CD_WORLD_PACK_SECTORS => "cd world sectors",
        counter::CD_WORLD_PACK_CHUNKS => "cd world chunks",
        counter::CD_WORLD_PACK_CHECKSUM => "cd world checksum",
        counter::CD_WORLD_PACK_STATUS => "cd world status",
        counter::CD_ROOM_CHUNK_BYTES => "cd room chunk bytes",
        counter::CD_ROOM_CHUNK_SECTORS => "cd room chunk sectors",
        counter::CD_ROOM_CHUNK_LOADS => "cd room chunk loads",
        counter::CD_ROOM_CHUNK_HITS => "cd room chunk hits",
        counter::CD_ROOM_CHUNK_STATUS => "cd room chunk status",
        counter::ROOM_STREAM_REQUESTS => "room stream requests",
        counter::ROOM_STREAM_MISSES => "room stream misses",
        counter::ROOM_STREAM_PREFETCH_REQUESTS => "room stream prefetches",
        counter::ROOM_STREAM_RESIDENT_SLOTS => "room stream resident slots",
        counter::ROOM_STREAM_EVICTIONS => "room stream evictions",
        counter::ROOM_STREAM_FAILED_LOADS => "room stream failed loads",
        counter::ROOM_STREAM_PENDING_LOADS => "room stream pending loads",
        counter::ROOM_PROJECTED_VERTICES => "room projected verts",
        counter::ROOM_SURF_MATERIAL_CYCLES => "room surf material cyc",
        counter::ROOM_SURF_PROJECTED_CYCLES => "room surf projected cyc",
        counter::ROOM_SURF_SCREEN_CYCLES => "room surf screen cyc",
        counter::ROOM_SURF_KIND_CYCLES => "room surf kind cyc",
        counter::ROOM_SURF_BACKFACE_CYCLES => "room surf backface cyc",
        counter::ROOM_SURF_LIGHTING_CYCLES => "room surf lighting cyc",
        counter::ROOM_SURF_SUBMIT_CYCLES => "room surf submit cyc",
        counter::ROOM_SURF_PROFILED => "room surf profiled",
        counter::ROOM_SURF_MATERIAL_MISSES => "room surf material misses",
        counter::ROOM_SURF_PROJECTED_REJECTS => "room surf projected rejects",
        counter::ROOM_SURF_SCREEN_CULLED => "room surf screen culled",
        counter::ROOM_SURF_BACKFACE_CULLED => "room surf backface culled",
        counter::ROOM_SURF_FLOORS => "room surf floors",
        counter::ROOM_SURF_CEILINGS => "room surf ceilings",
        counter::ROOM_SURF_WALLS => "room surf walls",
        counter::ROOM_SURF_WHOLE_QUADS => "room surf whole quads",
        counter::ROOM_SURF_SPLIT_TRIS => "room surf split tris",
        counter::ROOM_SURF_LIGHTING_REJECTS => "room surf lighting rejects",
        counter::ROOM_SUBMIT_HW_SAFE_TEST_CYCLES => "room submit hw-safe cyc",
        counter::ROOM_SUBMIT_PACKET_FILL_CYCLES => "room submit packet cyc",
        counter::ROOM_SUBMIT_PRIMITIVE_PUSH_CYCLES => "room submit prim push cyc",
        counter::ROOM_SUBMIT_DEPTH_CYCLES => "room submit depth cyc",
        counter::ROOM_SUBMIT_COMMAND_CYCLES => "room submit command cyc",
        counter::ROOM_SUBMIT_FALLBACK_CYCLES => "room submit fallback cyc",
        counter::ROOM_SUBMIT_HW_SAFE_CALLS => "room submit hw-safe calls",
        counter::ROOM_SUBMIT_FALLBACK_CALLS => "room submit fallback calls",
        counter::ROOM_SUBMIT_COMMAND_OVERFLOWS => "room submit command overflows",
        counter::ROOM_SUBMIT_PRIMITIVE_OVERFLOWS => "room submit prim overflows",
        counter::MODEL_PROFILE_CYCLES_0 => "model0 cycles",
        counter::MODEL_PROFILE_CYCLES_1 => "model1 cycles",
        counter::MODEL_PROFILE_CYCLES_2 => "model2 cycles",
        counter::MODEL_PROFILE_CYCLES_3 => "model3 cycles",
        counter::MODEL_PROFILE_CYCLES_4 => "model4 cycles",
        counter::MODEL_PROFILE_CYCLES_5 => "model5 cycles",
        counter::MODEL_PROFILE_CYCLES_6 => "model6 cycles",
        counter::MODEL_PROFILE_CYCLES_7 => "model7 cycles",
        counter::MODEL_PROFILE_DRAWS_0 => "model0 draws",
        counter::MODEL_PROFILE_DRAWS_1 => "model1 draws",
        counter::MODEL_PROFILE_DRAWS_2 => "model2 draws",
        counter::MODEL_PROFILE_DRAWS_3 => "model3 draws",
        counter::MODEL_PROFILE_DRAWS_4 => "model4 draws",
        counter::MODEL_PROFILE_DRAWS_5 => "model5 draws",
        counter::MODEL_PROFILE_DRAWS_6 => "model6 draws",
        counter::MODEL_PROFILE_DRAWS_7 => "model7 draws",
        counter::ROOM_STREAM_RESIDENT_MASK_LO => "resident chunk mask lo",
        counter::ROOM_STREAM_RESIDENT_MASK_HI => "resident chunk mask hi",
        counter::ROOM_ACTIVE_CHUNK_MASK_LO => "active chunk mask lo",
        counter::ROOM_ACTIVE_CHUNK_MASK_HI => "active chunk mask hi",
        counter::ROOM_DRAWN_CHUNK_MASK_LO => "drawn chunk mask lo",
        counter::ROOM_DRAWN_CHUNK_MASK_HI => "drawn chunk mask hi",
        counter::ROOM_PLAYER_ROOM_INDEX => "player room index",
        counter::ROOM_PLAYER_LOCAL_X_BIASED => "player local x",
        counter::ROOM_PLAYER_LOCAL_Z_BIASED => "player local z",
        counter::ROOM_PLAYER_VIEW_YAW_Q12 => "player view yaw q12",
        counter::ROOM_CAMERA_LOCAL_X_BIASED => "camera local x",
        counter::ROOM_CAMERA_LOCAL_Z_BIASED => "camera local z",
        counter::PORTAL_VIS_CURRENT_ROOM => "portal current room",
        counter::PORTAL_VIS_VISIBLE_ROOMS => "portal visible rooms",
        counter::PORTAL_VIS_FRONTIER_ROOMS => "portal frontier rooms",
        counter::PORTAL_VIS_FRUSTUMS => "portal frustums",
        counter::PORTAL_VIS_PORTALS_TESTED => "portal tests",
        counter::PORTAL_VIS_PORTALS_ACCEPTED => "portal accepts",
        counter::PORTAL_VIS_REJECT_BACKFACE => "portal reject backface",
        counter::PORTAL_VIS_REJECT_FRUSTUM => "portal reject frustum",
        counter::PORTAL_VIS_REJECT_TINY => "portal reject tiny",
        counter::PORTAL_VIS_CAP_ROOM => "portal room cap",
        counter::PORTAL_VIS_CAP_FRUSTUM => "portal frustum cap",
        counter::PORTAL_VIS_CAP_DEPTH => "portal depth cap",
        counter::PORTAL_VIS_VISIBLE_MISSING_RESIDENT => "portal visible missing resident",
        counter::ROOM_STREAM_PRIORITY_CURRENT => "stream priority current",
        counter::ROOM_STREAM_PRIORITY_VISIBLE => "stream priority visible",
        counter::ROOM_STREAM_PRIORITY_FRONTIER => "stream priority frontier",
        counter::ROOM_STREAM_PROTECTED_FULL => "stream protected full",
        counter::PORTAL_VIS_VISIBLE_MASK_LO => "portal visible mask lo",
        counter::PORTAL_VIS_VISIBLE_MASK_HI => "portal visible mask hi",
        counter::PORTAL_VIS_FRONTIER_MASK_LO => "portal frontier mask lo",
        counter::PORTAL_VIS_FRONTIER_MASK_HI => "portal frontier mask hi",
        counter::PORTAL_VIS_MISSING_MASK_LO => "portal missing mask lo",
        counter::PORTAL_VIS_MISSING_MASK_HI => "portal missing mask hi",
        counter::PORTAL_VIS_TESTED_MASK_LO => "portal tested mask lo",
        counter::PORTAL_VIS_TESTED_MASK_HI => "portal tested mask hi",
        counter::PORTAL_VIS_ACCEPTED_MASK_LO => "portal accepted mask lo",
        counter::PORTAL_VIS_ACCEPTED_MASK_HI => "portal accepted mask hi",
        counter::PORTAL_VIS_REJECT_FRUSTUM_MASK_LO => "portal frustum reject mask lo",
        counter::PORTAL_VIS_REJECT_FRUSTUM_MASK_HI => "portal frustum reject mask hi",
        counter::PORTAL_VIS_BOUNDS_FALLBACKS => "portal bounds fallback",
        counter::PORTAL_VIS_BOUNDS_FALLBACK_MASK_LO => "portal bounds fallback mask lo",
        counter::PORTAL_VIS_BOUNDS_FALLBACK_MASK_HI => "portal bounds fallback mask hi",
        counter::ROOM_STREAM_SLOT_LIMIT => "room stream slot limit",
        counter::ROOM_STREAM_LOADING_MASK_LO => "loading chunk mask lo",
        counter::ROOM_STREAM_LOADING_MASK_HI => "loading chunk mask hi",
        counter::PORTAL_VIS_VISIBLE_BUILD_FAILED => "portal visible build failed",
        counter::PORTAL_VIS_BUILD_FAILED_MASK_LO => "portal build failed mask lo",
        counter::PORTAL_VIS_BUILD_FAILED_MASK_HI => "portal build failed mask hi",
        counter::PORTAL_VIS_TESTED_PORTAL_MASK_LO => "portal tested portal mask lo",
        counter::PORTAL_VIS_TESTED_PORTAL_MASK_HI => "portal tested portal mask hi",
        counter::PORTAL_VIS_ACCEPTED_PORTAL_MASK_LO => "portal accepted portal mask lo",
        counter::PORTAL_VIS_ACCEPTED_PORTAL_MASK_HI => "portal accepted portal mask hi",
        counter::PORTAL_VIS_REJECT_FRUSTUM_PORTAL_MASK_LO => "portal frustum reject portal mask lo",
        counter::PORTAL_VIS_REJECT_FRUSTUM_PORTAL_MASK_HI => "portal frustum reject portal mask hi",
        counter::PORTAL_VIS_BOUNDS_FALLBACK_PORTAL_MASK_LO => {
            "portal bounds fallback portal mask lo"
        }
        counter::PORTAL_VIS_BOUNDS_FALLBACK_PORTAL_MASK_HI => {
            "portal bounds fallback portal mask hi"
        }
        counter::ROOM_CAMERA_VIEW_SIN_YAW_Q12_BIASED => "camera view sin yaw q12 biased",
        counter::ROOM_CAMERA_VIEW_COS_YAW_Q12_BIASED => "camera view cos yaw q12 biased",
        _ => "unknown",
    }
}

/// Encode a guest event command word.
pub const fn encode_event(kind: u8, id: u16) -> u32 {
    ((kind as u32) << KIND_SHIFT) | (id as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telemetry_port_latches_value_then_event() {
        let mut telemetry = GuestTelemetry::new();
        assert!(telemetry.observe_write32(VALUE_PHYS, 42, 100));
        assert!(telemetry.observe_write32(
            EVENT_PHYS,
            encode_event(4, counter::WORLD_COMMANDS),
            110
        ));

        let events = telemetry.drain_events();
        assert_eq!(telemetry.frames_seen(), 0);
        assert_eq!(telemetry.counter_total(counter::WORLD_COMMANDS), 42);
        assert_eq!(telemetry.counter_max_value(counter::WORLD_COMMANDS), 42);
        assert_eq!(telemetry.counter_latest_value(counter::WORLD_COMMANDS), 42);
        assert_eq!(telemetry.observe_read32(CYCLE_PHYS, 1234), Some(1234));
        assert_eq!(
            events,
            [GuestTelemetryEvent {
                cycles: 110,
                kind: GuestTelemetryKind::Counter,
                id: counter::WORLD_COMMANDS,
                value: 42,
            }]
        );
    }

    #[test]
    fn summary_accumulates_stage_spans_and_counters() {
        let events = [
            GuestTelemetryEvent {
                cycles: 10,
                kind: GuestTelemetryKind::FrameBegin,
                id: 0,
                value: 7,
            },
            GuestTelemetryEvent {
                cycles: 20,
                kind: GuestTelemetryKind::StageBegin,
                id: stage::RENDER,
                value: 0,
            },
            GuestTelemetryEvent {
                cycles: 70,
                kind: GuestTelemetryKind::StageEnd,
                id: stage::RENDER,
                value: 0,
            },
            GuestTelemetryEvent {
                cycles: 80,
                kind: GuestTelemetryKind::Counter,
                id: counter::TRI_PRIMITIVES,
                value: 12,
            },
            GuestTelemetryEvent {
                cycles: 90,
                kind: GuestTelemetryKind::Counter,
                id: counter::VISUAL_MAX_LATENESS_VBLANKS,
                value: 2,
            },
            GuestTelemetryEvent {
                cycles: 100,
                kind: GuestTelemetryKind::Counter,
                id: counter::VISUAL_MAX_LATENESS_VBLANKS,
                value: 1,
            },
        ];
        let summary = GuestTelemetrySummary::from_events(&events);
        assert_eq!(summary.frames, 1);
        assert_eq!(summary.stage_cycles[stage::RENDER as usize], 50);
        assert_eq!(summary.stage_hits[stage::RENDER as usize], 1);
        assert_eq!(summary.counters[counter::TRI_PRIMITIVES as usize], 12);
        assert_eq!(
            summary.counters[counter::VISUAL_MAX_LATENESS_VBLANKS as usize],
            3
        );
        assert_eq!(
            summary.counter_max_values[counter::VISUAL_MAX_LATENESS_VBLANKS as usize],
            2
        );
        assert_eq!(
            summary.counter_latest_values[counter::VISUAL_MAX_LATENESS_VBLANKS as usize],
            1
        );
    }

    #[test]
    fn frame_pacing_counter_names_are_known() {
        assert_eq!(counter_name(counter::SIM_TICKS), "sim ticks");
        assert_eq!(counter_name(counter::VISUAL_FRAMES), "visual frames");
        assert_eq!(
            counter_name(counter::VISUAL_MAX_LATENESS_VBLANKS),
            "visual max lateness vblanks"
        );
        assert_eq!(stage_name(stage::CD_STREAM_BENCH), "cd stream bench");
        assert_eq!(stage_name(stage::CD_STREAM_STEADY), "cd stream steady");
        assert_eq!(stage_name(stage::CD_WORLD_PACK_STREAM), "cd world pack");
        assert_eq!(stage_name(stage::CD_ROOM_CHUNK_LOAD), "cd room chunk load");
        assert_eq!(stage_name(stage::ROOM_VISIBLE_LIST), "room visible list");
        assert_eq!(stage_name(stage::ROOM_CELL_SELECT), "room cell select");
        assert_eq!(stage_name(stage::ROOM_SURFACE_DRAW), "room surface draw");
        assert_eq!(stage_name(stage::SKY), "sky");
        assert_eq!(stage_name(stage::FAR_VISTA), "far vista");
        assert_eq!(stage_name(stage::IMAGE_PROPS), "image props");
        assert_eq!(
            counter_name(counter::CD_STREAM_BENCH_STATUS),
            "cd stream status"
        );
        assert_eq!(
            counter_name(counter::CD_ROOM_CHUNK_BYTES),
            "cd room chunk bytes"
        );
        assert_eq!(
            counter_name(counter::ROOM_STREAM_RESIDENT_SLOTS),
            "room stream resident slots"
        );
        assert_eq!(
            counter_name(counter::ROOM_PROJECTED_VERTICES),
            "room projected verts"
        );
        assert_eq!(
            counter_name(counter::ROOM_SURF_SUBMIT_CYCLES),
            "room surf submit cyc"
        );
        assert_eq!(
            counter_name(counter::ROOM_SURF_BACKFACE_CULLED),
            "room surf backface culled"
        );
        assert_eq!(
            counter_name(counter::ROOM_SUBMIT_PACKET_FILL_CYCLES),
            "room submit packet cyc"
        );
    }
}
