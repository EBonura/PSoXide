// SPDX-License-Identifier: GPL-2.0-or-later
//! Shared guest/host telemetry id tables.
//!
//! The guest runtime (`psx-engine`) emits stage/task/counter events
//! through an emulator-observed Expansion 2 port, and the emulator
//! (`emulator-core`) decodes and aggregates them. Both sides used to
//! carry hand-synced copies of the id tables; this crate owns the ids,
//! the slot counts, and the compile-time guards once. It is `no_std`
//! and const-only, so the MIPS guest and host tooling share it freely.

#![no_std]

pub mod emit;

/// Declares one id module plus a `*_desc(id)` lookup returning each id's own
/// rustdoc as a runtime string. Host tooling (the frame profiler's tooltips)
/// reads descriptions through the lookup, so the docs here are the single
/// source and cannot drift from what the UI shows. The const declarations
/// inside the braces are ordinary Rust, passed through verbatim.
macro_rules! id_table {
    (
        $(#[$mod_attr:meta])*
        pub mod $module:ident;
        pub fn $desc_fn:ident;
        {
            $(
                $(#[doc = $doc:literal])+
                pub const $name:ident: u16 = $value:expr;
            )+
        }
    ) => {
        $(#[$mod_attr])*
        pub mod $module {
            $(
                $(#[doc = $doc])+
                pub const $name: u16 = $value;
            )+
        }

        /// Host-tooling description for an id: the id's doc comment from this
        /// crate, verbatim. Returns `""` for unknown ids.
        pub fn $desc_fn(id: u16) -> &'static str {
            match id {
                $( $module::$name => concat!($($doc),+), )+
                _ => "",
            }
        }
    };
}

id_table! {
    /// Runtime stage ids.
    pub mod stage;
    pub fn stage_desc;
    {
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
    /// Ordering-table DMA kick (CPU-side setup; excludes the GPU-draw wait).
    pub const OT_SUBMIT: u16 = 11;
    /// Blocking wait for the ordering-table DMA walk to finish: the
    /// CPU-blocked-on-GPU portion of a submit, i.e. GPU draw + DMA cost.
    pub const OT_WAIT: u16 = 45;
    /// Player collision gather + motor solve (sim).
    pub const SIM_COLLISION: u16 = 36;
    /// Current-room tracking + active-window refresh (sim).
    pub const SIM_ROOM_TRACK: u16 = 37;
    /// Streamed-room residency reconcile (sim).
    pub const SIM_RESIDENCY: u16 = 38;
    /// Streamed-room sector pump (sim).
    pub const SIM_PUMP: u16 = 39;
    /// Character motor solve (floor snap + wall sweep), excludes the gather.
    pub const SIM_SOLVE: u16 = 40;
    /// Solid unbroken editor box props.
    pub const BOX_PROPS: u16 = 41;
    /// Persistent floor debris from broken editor box props.
    pub const BOX_PROP_DEBRIS: u16 = 42;
    /// Transient break shards from editor box props.
    pub const BOX_PROP_SHARDS: u16 = 43;
    /// Editor-authored image/card props, excluding box props.
    pub const IMAGE_CARDS: u16 = 44;
    /// Pre-collision actor work in the fixed update: prop advances,
    /// interactables, lock-on, evade/anim input, attack break checks.
    pub const UPDATE_ACTOR: u16 = 46;
    /// Always-on active-room window refresh at the fixed-update tail.
    pub const UPDATE_WINDOW: u16 = 47;
    /// Inside ROOM_CELL_SELECT: candidate -> cached cell index lookup.
    pub const CELL_LOOKUP: u16 = 48;
    /// Inside ROOM_CELL_SELECT: per-cell view transform + frustum accept.
    pub const CELL_DEPTH: u16 = 49;
    /// Inside ROOM_CELL_SELECT: unique vertex-index collection.
    pub const CELL_COLLECT: u16 = 50;
    /// Phase-3 gameplay layer: entity state machines + logic event
    /// graph + effect dispatch, ticked at the top of the update band.
    /// Budgeted at 60k cycles per 30 fps frame in
    /// docs/game-runtime-plan.md ("Phase 3 budget").
    pub const GAME_LOGIC: u16 = 51;
    }
}

/// Number of stage slots, including index zero for unknown/reserved ids.
/// Sized to the highest stage id (`GAME_LOGIC = 51`) plus one.
pub const STAGE_COUNT: usize = 52;

// Enforce `STAGE_COUNT = highest stage id + 1` at compile time. The host's
// stage arrays are indexed by id and out-of-range ids are dropped silently,
// so a new higher id without a matching STAGE_COUNT bump would quietly
// vanish from every summary. Adding a higher id trips this and must
// update both the count and this guard.
const _: () = assert!(stage::GAME_LOGIC as usize == STAGE_COUNT - 1);

id_table! {
    /// Runtime task ids.
    pub mod task;
    pub fn task_desc;
    {
    /// Built-in fixed simulation/update task.
    pub const FIXED_UPDATE: u16 = 0;
    /// Built-in visual render/present task.
    pub const VISUAL_RENDER: u16 = 1;
    }
}

/// Number of task slots, including reserved future scheduler jobs.
pub const TASK_COUNT: usize = 16;

id_table! {
    /// Runtime counter ids.
    pub mod counter;
    pub fn counter_desc;
    {
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
    // 18 and 19 are retired (the render-path weapon-hitbox counters,
    // removed with that dead system). Do not reuse: old counter-log
    // captures still carry them under the old meaning.
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
    /// Render camera room-local Y, biased for unsigned telemetry transport.
    pub const ROOM_CAMERA_LOCAL_Y_BIASED: u16 = 189;
    /// Render camera pitch sine in Q12, biased by 4096 for unsigned transport.
    pub const ROOM_CAMERA_VIEW_SIN_PITCH_Q12_BIASED: u16 = 190;
    /// Render camera pitch cosine in Q12, biased by 4096 for unsigned transport.
    pub const ROOM_CAMERA_VIEW_COS_PITCH_Q12_BIASED: u16 = 191;
    /// Render camera absolute level X used by portal traversal, biased for unsigned transport.
    pub const ROOM_CAMERA_GLOBAL_X_BIASED: u16 = 192;
    /// Render camera absolute level Y used by portal traversal, biased for unsigned transport.
    pub const ROOM_CAMERA_GLOBAL_Y_BIASED: u16 = 193;
    /// Render camera absolute level Z used by portal traversal, biased for unsigned transport.
    pub const ROOM_CAMERA_GLOBAL_Z_BIASED: u16 = 194;
    /// Model vertices projected through CPU blend skinning.
    pub const TEXTURED_MODEL_CPU_BLEND_VERTICES: u16 = 195;
    /// Model faces handled by any packed fast-path helper.
    pub const TEXTURED_MODEL_PACKED_FACE_CALLS: u16 = 196;
    /// Model faces handled by the packed all-front/all-HW-bounds helper.
    pub const TEXTURED_MODEL_PACKED_UNCLAMPED_CALLS: u16 = 197;
    /// Model faces handled by packed all-front helpers that still clamp screen coordinates.
    pub const TEXTURED_MODEL_PACKED_CLAMPED_CALLS: u16 = 198;
    /// Model faces handled by the generic packed helper.
    pub const TEXTURED_MODEL_PACKED_GENERAL_CALLS: u16 = 199;
    /// Model faces handled by the fully general face path.
    pub const TEXTURED_MODEL_FALLBACK_FACE_CALLS: u16 = 200;
    /// Packed model faces that fell back to split/general submission due hardware extents.
    pub const TEXTURED_MODEL_HW_EXTENT_FALLBACKS: u16 = 201;
    /// Model faces dropped because they crossed or sat behind the near plane.
    pub const TEXTURED_MODEL_NEAR_DROPS: u16 = 202;
    /// Model faces dropped because the projected triangle was not hardware-safe.
    pub const TEXTURED_MODEL_HW_UNSAFE_DROPS: u16 = 203;
    /// Model triangles submitted through packed fast paths.
    pub const TEXTURED_MODEL_FAST_SUBMITTED_TRIS: u16 = 204;
    /// Model submits that required CPU blended vertices.
    pub const TEXTURED_MODEL_CPU_BLEND_SUBMITS: u16 = 205;
    /// Model submits that used primary-joint-only projection.
    pub const TEXTURED_MODEL_PRIMARY_JOINT_SUBMITS: u16 = 206;
    /// Model submits where all projected vertices were in front of the near plane.
    pub const TEXTURED_MODEL_ALL_FRONT_SUBMITS: u16 = 207;
    /// Model submits where all projected vertices were inside PS1 hardware bounds.
    pub const TEXTURED_MODEL_ALL_HW_BOUNDS_SUBMITS: u16 = 208;
    /// Model submits eligible for the fastest packed-unclamped face path.
    pub const TEXTURED_MODEL_PACKED_UNCLAMPED_ELIGIBLE_SUBMITS: u16 = 209;
    /// Model submits eligible for packed all-front helpers.
    pub const TEXTURED_MODEL_PACKED_CLAMPED_ELIGIBLE_SUBMITS: u16 = 210;
    /// Model submits eligible for the generic packed helper only.
    pub const TEXTURED_MODEL_PACKED_GENERAL_ELIGIBLE_SUBMITS: u16 = 211;
    /// Model triangles emitted by split/general fallback submission.
    pub const TEXTURED_MODEL_SPLIT_TRIS: u16 = 212;
    /// Model triangles skipped because face indices exceeded projected vertex ranges.
    pub const TEXTURED_MODEL_SKIPPED_TRIS: u16 = 213;
    /// Model submits that exceeded the projected vertex scratch buffer.
    pub const TEXTURED_MODEL_VERTEX_OVERFLOW_SUBMITS: u16 = 214;
    /// Model submits that exceeded primitive packet storage.
    pub const TEXTURED_MODEL_PRIMITIVE_OVERFLOW_SUBMITS: u16 = 215;
    /// Model submits that exceeded world-command storage.
    pub const TEXTURED_MODEL_COMMAND_OVERFLOW_SUBMITS: u16 = 216;
    /// Room-texture VRAM slots freed by residency eviction (Stage 4 teardown).
    pub const VRAM_SLOTS_FREED: u16 = 217;
    /// Texture uploads that found no free `VRAM_SLOTS` entry: the 64-slot
    /// residency table cap, the binding VRAM budget for distinct resident
    /// textures. A non-zero value during traversal is the silent missing-texture
    /// root cause.
    pub const VRAM_SLOT_TABLE_FULL: u16 = 218;
    /// Room-texture uploads where `alloc_window` found no free space in the
    /// room-material page band (the secondary 4bpp window cap).
    pub const VRAM_WINDOW_FULL: u16 = 219;
    /// Texture uploads where `alloc_clut` found no free CLUT slot (the CLUT band cap).
    pub const VRAM_CLUT_FULL: u16 = 220;
    /// Room-texture uploads skipped because the in-flight VRAM upload queue had
    /// no free slot (the upload-queue depth throttle, another silent skip).
    pub const VRAM_UPLOAD_QUEUE_FULL: u16 = 221;
    /// Room materials left untextured because their texture failed to become
    /// VRAM-resident (a real drop, not merely a pending upload): the silent
    /// missing-texture fallback at material-build time.
    pub const ROOM_MATERIAL_TEXTURE_DROPS: u16 = 222;
    /// Room materials dropped because their `local_slot` is >= MAX_ROOM_MATERIALS,
    /// i.e. the room references more distinct materials than the per-room material
    /// table can hold. Every surface using a dropped slot renders untextured or
    /// not at all. A non-zero value means the per-room material cap is too small
    /// for the cooked room (raise MAX_ROOM_MATERIALS or reduce the room's
    /// material count). This was the silent root cause of the demo10 invisible
    /// frieze/stairs.
    pub const ROOM_MATERIAL_SLOT_OVERFLOW: u16 = 223;
    /// Game entities that ran their state machine this tick (the
    /// phase-3 per-tick "thinker" count; budget target <= 8).
    pub const GAME_ENTITIES_THOUGHT: u16 = 224;
    /// Game-entity transitions INTO Patrol since spawn.
    pub const GAME_ENTITY_PATROL_ENTERS: u16 = 225;
    /// Game-entity transitions INTO Aggro since spawn.
    pub const GAME_ENTITY_AGGRO_ENTERS: u16 = 226;
    /// Game-entity transitions INTO Windup since spawn.
    pub const GAME_ENTITY_WINDUP_ENTERS: u16 = 227;
    /// Game-entity transitions INTO Attack since spawn (the souls
    /// commit; contact resolves through the combat counters below).
    pub const GAME_ENTITY_ATTACK_ENTERS: u16 = 228;
    /// Logic records fired since init (LogicRuntime rolling total).
    pub const LOGIC_RECORDS_FIRED: u16 = 229;
    /// Game-entity poise breaks (transitions INTO Staggered) from
    /// player hits (phase-3 combat slice).
    pub const GAME_ENTITY_STAGGER_ENTERS: u16 = 230;
    /// Game-entity deaths from player hits.
    pub const GAME_ENTITY_DEATHS: u16 = 231;
    /// Player melee-arc swings that connected with an entity.
    pub const PLAYER_MELEE_HITS: u16 = 232;
    /// Entity attacks that connected with the player (i-framed and
    /// out-of-arc swings whiff and do not count).
    pub const PLAYER_HITS_TAKEN: u16 = 233;

    /// Live bytes in the persistent gameplay asset pool. Makes the difference
    /// between whole-level and neighbourhood-scoped residency observable, which
    /// is the only way to tell that asset paging is doing anything.
    pub const PERSISTENT_ASSET_RESIDENT_BYTES: u16 = 234;

    /// Times the persistent asset pool refused an allocation. Non-zero means the
    /// cooked budget is too small for the neighbourhood, or the pool fragmented
    /// past the point first-fit can serve. The visible symptom is otherwise a
    /// texture that silently never loads, so this must stay at zero.
    pub const PERSISTENT_ASSET_LOAD_FAILURES: u16 = 235;

    /// Asset id of the FIRST persistent asset that failed to load, or
    /// `u16::MAX` if the failure could not be attributed to one asset. Without
    /// it `PERSISTENT_ASSET_LOAD_FAILURES` says only that something broke, and
    /// the search space is every streamed asset in the level.
    pub const PERSISTENT_ASSET_FAILED_ID: u16 = 236;

    /// Why that asset failed: a `cd_stream` chunk status (0..11) when the
    /// read reached the drive, or one of the `asset_streaming` reason codes
    /// (100+) when it failed before any read was armed.
    pub const PERSISTENT_ASSET_FAILED_REASON: u16 = 237;

    /// Room surfaces whose exact TR subdivision predicate passed.
    pub const ROOM_SURF_TR_SUBDIVISION_CANDIDATES: u16 = 238;

    /// TR subdivision candidates that successfully emitted geometry.
    pub const ROOM_SURF_TR_SUBDIVISION_SUBMITTED: u16 = 239;

    /// Primitive packets emitted by room surface draws, excluding water,
    /// props, actors, and other scene passes.
    pub const ROOM_SURFACE_PACKETS: u16 = 240;

    /// World commands emitted by room surface draws, excluding water, props,
    /// actors, and other scene passes.
    pub const ROOM_SURFACE_COMMANDS: u16 = 241;

    /// Warp probe (read-only diagnostic, `room-surface-profile` only).
    ///
    /// Room surfaces bucketed by the closed-form predicted affine texture
    /// error from `docs/texture-warping-2026-07-27.md`, cross-tabbed against
    /// what the depth-band subdivision rule actually decided. Buckets are
    /// `<1`, `1..2`, `2..4` and `>=4` calibrated texels.
    ///
    /// Error is reported as count / sum / max in 1/16 texel units rather than
    /// as buckets: a first run bucketed it and put 99% of surfaces in the
    /// top bin, which answers nothing. Sum and count give the mean; max gives
    /// the worst case. `..._UNDER_1TX` is the one bucket worth keeping, since
    /// "subdivided a surface that could not warp" is wasted primitives.
    pub const ROOM_SURF_WARP_SUBDIVIDED_COUNT: u16 = 242;
    /// Sum of predicted error over subdivided surfaces, 1/16 texel units.
    pub const ROOM_SURF_WARP_SUBDIVIDED_SUM: u16 = 243;
    /// Worst predicted error among subdivided surfaces, 1/16 texel units.
    pub const ROOM_SURF_WARP_SUBDIVIDED_MAX: u16 = 244;
    /// Subdivided surfaces predicted to warp under 1 texel: wasted work.
    pub const ROOM_SURF_WARP_SUBDIVIDED_UNDER_1TX: u16 = 245;
    /// Surfaces the depth-band rule left as authored polygons.
    pub const ROOM_SURF_WARP_UNTOUCHED_COUNT: u16 = 246;
    /// Sum of predicted error over untouched surfaces, 1/16 texel units.
    pub const ROOM_SURF_WARP_UNTOUCHED_SUM: u16 = 247;
    /// Worst predicted error among untouched surfaces, 1/16 texel units.
    pub const ROOM_SURF_WARP_UNTOUCHED_MAX: u16 = 248;
    /// Untouched surfaces predicted to warp under 1 texel: correctly skipped.
    pub const ROOM_SURF_WARP_UNTOUCHED_UNDER_1TX: u16 = 249;

    /// E2 (docs/engine-30fps-architecture-2026-07-26.md): cycles rebuilding the
    /// per-surface `WorldSurfaceOptions` variants. This sits between the timed
    /// lighting and submit sections, i.e. inside the ~48% of room_surface_draw
    /// that no existing counter attributed.
    pub const ROOM_SURF_OPTIONS_CYCLES: u16 = 250;

    /// E2: per-cell setup ahead of the surface loop (tile depth options plus
    /// the submit-depth table), charged once per accepted cell.
    pub const ROOM_SURF_CELL_SETUP_CYCLES: u16 = 251;

    /// E2: the whole per-surface draw call. `call - sum(inner sections)` is the
    /// surface body no counter reaches; `stage - cell_setup - call` is the
    /// loop's own overhead. Together these close the attribution.
    pub const ROOM_SURF_CALL_CYCLES: u16 = 252;

    /// Player attack actions that actually started (an accepted light,
    /// heavy, or combo press; presses swallowed by locks or hit-stun do
    /// not count). Lets headless combat gates assert attempt counts
    /// independently of whether the swings later connect.
    pub const PLAYER_ATTACK_STARTS: u16 = 253;
    }
}

/// Number of counter slots, including index zero for unknown/reserved ids.
/// Must stay larger than the highest counter id emitted by the guest; a
/// counter id >= this is silently dropped.
pub const COUNTER_COUNT: usize = 254;

const _: () = assert!(counter::PLAYER_ATTACK_STARTS as usize == COUNTER_COUNT - 1);
