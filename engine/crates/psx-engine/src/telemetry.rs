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
    /// Portal traversal and visible-room selection.
    pub const PORTAL_VISIBILITY: u16 = 35;
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
    /// Runtime rooms accepted by portal traversal.
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
    /// Portal-accepted rooms neither resident nor loading when the active window was built.
    pub const PORTAL_VIS_VISIBLE_MISSING_RESIDENT: u16 = 151;
    /// Stream priority requests for the current room.
    pub const ROOM_STREAM_PRIORITY_CURRENT: u16 = 152;
    /// Stream priority requests for portal-accepted rooms.
    pub const ROOM_STREAM_PRIORITY_VISIBLE: u16 = 153;
    /// Stream priority requests for portal-frontier rooms.
    pub const ROOM_STREAM_PRIORITY_FRONTIER: u16 = 154;
    /// Stream loads blocked because resident/requested rooms filled the pool.
    pub const ROOM_STREAM_PROTECTED_FULL: u16 = 155;
    /// Low 32 bits of the portal-accepted room bitset.
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
    /// Portal-accepted rooms resident in the stream cache but not buildable.
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

const EVENT_KIND_FRAME_BEGIN: u8 = 1;
const EVENT_KIND_STAGE_BEGIN: u8 = 2;
const EVENT_KIND_STAGE_END: u8 = 3;
const EVENT_KIND_COUNTER: u8 = 4;

#[cfg(target_arch = "mips")]
const EVENT_ADDR: *mut u32 = 0xBF80_2F00 as *mut u32;
#[cfg(target_arch = "mips")]
const VALUE_ADDR: *mut u32 = 0xBF80_2F04 as *mut u32;
#[cfg(target_arch = "mips")]
const CYCLE_ADDR: *const u32 = 0xBF80_2F08 as *const u32;

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

/// Read the emulator-observed guest cycle counter.
///
/// This is only meaningful under PSoXide's emulator telemetry port. On
/// hardware, and on host builds, it is a profiling-only helper and returns
/// zero unless the emulator provides the Expansion 2 cycle register.
#[inline(always)]
pub fn cycle_counter() -> u32 {
    read_cycle_counter()
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
fn read_cycle_counter() -> u32 {
    unsafe { core::ptr::read_volatile(CYCLE_ADDR) }
}

#[cfg(not(target_arch = "mips"))]
#[inline(always)]
fn read_cycle_counter() -> u32 {
    0
}

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
