use super::*;

pub(super) const fn cached_room_depth_mode() -> CachedRoomDepthMode {
    match CACHED_ROOM_DEPTH_MODE {
        0 => CachedRoomDepthMode::FixedCell,
        2 => CachedRoomDepthMode::HybridWalls,
        3 => CachedRoomDepthMode::PerTriangle,
        _ => CachedRoomDepthMode::Hybrid,
    }
}

pub(super) const fn cached_room_subdivision_mode() -> CachedRoomSubdivisionMode {
    match CACHED_ROOM_TEXTURE_SPLIT_MODE {
        1 => CachedRoomSubdivisionMode::DepthSorted,
        2 => CachedRoomSubdivisionMode::Risky,
        _ => CachedRoomSubdivisionMode::All,
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum CachedRoomDrawOrderMode {
    Distance,
    Portal,
    Slot,
}

pub(super) const fn cached_room_draw_order_mode() -> CachedRoomDrawOrderMode {
    match CACHED_ROOM_DRAW_ORDER_MODE {
        1 => CachedRoomDrawOrderMode::Portal,
        2 => CachedRoomDrawOrderMode::Slot,
        _ => CachedRoomDrawOrderMode::Distance,
    }
}

// VRAM layout. Room materials and model atlases live in
// disjoint regions so a model atlas upload never overwrites a
// room texture (and vice versa).
//
// Room materials: 4bpp pages starting at (640, 0), packed on an
// 8-texel grid inside each tpage. Each material carries GP0(E2)
// texture-window state so authored UV repetition samples only its
// allocation instead of requiring physically repeated texels.
//
// Model atlases: 8bpp pages starting at (384, 256), packed
// left-to-right on 64-halfword boundaries. Each atlas gets a
// tpage word matching its own VRAM origin, so mesh UVs stay local
// to the atlas. One CLUT row per atlas starts at y=484 (below the
// material CLUT band so the two never collide).
pub(super) const ROOM_TPAGE_BASE_X: u16 = 640;
pub(super) const SHARED_TPAGE: Tpage = Tpage::new(ROOM_TPAGE_BASE_X, 0, TexDepth::Bit4);
pub(super) const TPAGE_WORD: u16 = SHARED_TPAGE.uv_tpage_word(0);
pub(super) const ROOM_TPAGE_STRIDE_HW: u16 = 64;
pub(super) const ROOM_TPAGE_LIMIT_X: u16 = 1024;
pub(super) const ROOM_TPAGE_COUNT: usize =
    ((ROOM_TPAGE_LIMIT_X - ROOM_TPAGE_BASE_X) / ROOM_TPAGE_STRIDE_HW) as usize;
pub(super) const ROOM_TILE_TEXELS: u16 = 64;

pub(super) const MODEL_TPAGE: Tpage = Tpage::new(384, 256, TexDepth::Bit8);
/// Maximum halfword width addressable by one 8bpp texture page.
pub(super) const MODEL_TPAGE_MAX_HALFWORDS: u16 = 128;

/// Runtime UI font slots. The cooked manifest compacts authored font choices
/// into these slots, so only fonts actually used by cooked UI text are uploaded.
pub(super) const MAX_RUNTIME_UI_FONTS: usize = 4;
/// Resource-set key for menu (non-gameplay) states. Distinct from the gameplay
/// key so the flow driver fires `on_exit_state`/`on_enter_state` when crossing
/// the menu->gameplay boundary, letting the runtime load streamed UI images on
/// menu entry and free them on gameplay entry.
pub(super) const MENU_RESOURCE_KEY: u32 = 2;
/// Resource-set key for gameplay states (see `MENU_RESOURCE_KEY`).
pub(super) const GAMEPLAY_RESOURCE_KEY: u32 = 3;
pub(super) static SHADOW_CIRCLE_BLOB: &[u8] = include_bytes!("../assets/shadow_circle_64.psxt");
/// Shadow decals share the shadow/particle 4bpp page allocated by the unified
/// VRAM allocator. UVs are page-relative, so only the page base moves.
pub(super) const SHADOW_TEXEL_U: u8 = 64;
pub(super) const SHADOW_UV_MAX: u8 = SHADOW_TEXEL_U + 63;
pub(super) const SCREEN_W: i16 = 320;
pub(super) const SCREEN_H: i16 = 240;
pub(super) const SCREEN_CX: i16 = 160;
pub(super) const SCREEN_CY: i16 = 120;
pub(super) const FOCAL: i32 = 320;
pub(super) const NEAR_Z: i32 = 64;
pub(super) const FAR_Z: i32 = 16384;
pub(super) const PROJECTION: WorldProjection =
    WorldProjection::new(SCREEN_CX, SCREEN_CY, FOCAL, NEAR_Z);
pub(super) const SHADOW_DEPTH_BIAS: i32 = FAR_Z;
pub(super) const SHADOW_FLOOR_LIFT: i32 = 4;
pub(super) const SHADOW_RADIUS_SCALE_NUM: i32 = 5;
pub(super) const SHADOW_RADIUS_SCALE_DEN: i32 = 4;
pub(super) const SHADOW_RADIUS_MIN: i32 = 160;
pub(super) const SHADOW_RADIUS_MAX: i32 = 320;
pub(super) const COLLISION_DEBUG_BUTTON: u16 = button::L3;
pub(super) const COLLISION_DEBUG_SEGMENTS: usize = 8;
pub(super) const COLLISION_DEBUG_FLOOR_LIFT: i32 = 8;
pub(super) const FLOOR_LINK_CROSS_EPSILON: i32 = 32;
/// Dead-band (engine units) below a floor boundary before a downward room
/// switch fires. Climbing up lands the player AT the boundary; without a
/// margin the down-switch would immediately fire and the player would
/// thrash between floors. Must exceed `FLOOR_LINK_CROSS_EPSILON` (the
/// up-switch slack) so the up and down conditions can't both hold at the
/// seam; well under a floor's height so a real fall still registers.
pub(super) const FLOOR_LINK_SWITCH_HYSTERESIS: i32 = 256;
pub(super) const DEBUG_MAP_POSITION_BIAS: i32 = 1_000_000;

pub(super) const CAMERA_Y_OFFSET: i32 = 1100;
pub(super) const CAMERA_START_RADIUS: i32 = 2400;
pub(super) const CAMERA_RADIUS_MIN: i32 = 800;
pub(super) const CAMERA_RADIUS_MAX: i32 = 5200;
pub(super) const CAMERA_RADIUS_STEP: i32 = 64;
pub(super) const CAMERA_START_YAW: Angle = Angle::from_q12(220);
pub(super) const CAMERA_YAW_STEP: Angle = Angle::from_q12(12);
pub(super) const CAMERA_SWEEP_ENABLED: bool = option_env!("PSXO_CAMERA_SWEEP").is_some();
pub(super) const CAMERA_SWEEP_FAST_ENABLED: bool = option_env!("PSXO_CAMERA_SWEEP_FAST").is_some();
pub(super) const CAMERA_SWEEP_WIDE_ENABLED: bool = option_env!("PSXO_CAMERA_SWEEP_WIDE").is_some();
pub(super) const CAMERA_SWEEP_FORCE_VISIBILITY: bool =
    option_env!("PSXO_CAMERA_SWEEP_FORCE_VIS").is_some();
pub(super) const CAMERA_SWEEP_YAW_STEP_Q12: i16 = if CAMERA_SWEEP_FAST_ENABLED { 96 } else { 4 };
pub(super) const CAMERA_SWEEP_RADIUS: i32 = if CAMERA_SWEEP_WIDE_ENABLED {
    CAMERA_RADIUS_MAX
} else {
    CAMERA_START_RADIUS
};
pub(super) const MOVE_STICK_DEADZONE: i16 = 18;
pub(super) const STICK_MAX: i16 = 127;
pub(super) const CAMERA_STICK_DEADZONE: i16 = 18;
pub(super) const CAMERA_STICK_YAW_STEP: i16 = 64;
pub(super) const CAMERA_STICK_PITCH_STEP: i16 = 48;
pub(super) const MIN_CAMERA_ORBIT_SPEED_LEVEL: u8 = 1;
pub(super) const DEFAULT_CAMERA_ORBIT_SPEED_LEVEL: u8 = 5;
pub(super) const MAX_CAMERA_ORBIT_SPEED_LEVEL: u8 = 7;
pub(super) const CAMERA_SOFT_LOCK_BREAK_STICK: i16 = 72;
pub(super) const LOCK_SWITCH_STICK_THRESHOLD: i16 = 72;
pub(super) const LOCK_SWITCH_STICK_RELEASE: i16 = 36;
pub(super) const LOCK_RANGE: i32 = 4096;
pub(super) const LOCK_BREAK_RANGE: i32 = 5120;
pub(super) const SOFT_LOCK_RANGE: i32 = 3072;
pub(super) const SOFT_LOCK_BREAK_RANGE: i32 = 3840;
pub(super) const CAMERA_COLLISION_ENABLED: bool = true;
pub(super) const SOFT_LOCK_ENABLED: bool = false;

/// Quanta-per-frame turn rate when the runtime can't resolve a
/// Character (no PLAYER_CONTROLLER). Mirrors the pre-character
/// debug value.
pub(super) const FALLBACK_PLAYER_YAW_STEP: Angle = Angle::from_q12(32);
pub(super) const FALLBACK_PLAYER_SPEED: i32 = 32;
pub(super) const PLAYER_SPEED_SCALE_NUM: i32 = 3;
pub(super) const PLAYER_SPEED_SCALE_DEN: i32 = 4;
pub(super) const EVADE_RUN_BUTTON: u16 = button::CIRCLE;
pub(super) const EVADE_RUN_HOLD_VBLANKS: u8 = 8;
pub(super) const INTERACT_BUTTON: u16 = button::CROSS;
pub(super) const LIGHT_ATTACK_BUTTON: u16 = button::R1;
pub(super) const HEAVY_ATTACK_BUTTON: u16 = button::R2;

#[cfg(feature = "ot-2048")]
pub(super) const OT_DEPTH: usize = 2048;
#[cfg(all(not(feature = "ot-2048"), feature = "ot-1024"))]
pub(super) const OT_DEPTH: usize = 1024;
#[cfg(all(not(feature = "ot-2048"), not(feature = "ot-1024")))]
pub(super) const OT_DEPTH: usize = 512;
/// Room geometry, actors, and shadows share one depth band so walls can
/// correctly overpaint the hidden parts of characters in the PS1
/// painter's algorithm.
// Farthest slot (OT_DEPTH - 1) is reserved for the sky cyclorama (see
// SKY_OT_SLOT), so world geometry spans 0..=OT_DEPTH-2 and always draws in
// front of the sky.
pub(super) const WORLD_BAND: DepthBand = DepthBand::new(0, OT_DEPTH - 2);
pub(super) const WORLD_DEPTH_RANGE: DepthRange = DepthRange::new(NEAR_Z, FAR_Z);
#[cfg(feature = "world-grid-visible")]
pub(super) const ROOM_VISIBLE_CELL_SCREEN_MARGIN: i32 = 0;
#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
pub(super) const ROOM_VISIBLE_CELL_CAMERA_MARGIN: i32 = 96;
#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
pub(super) const ROOM_VISIBLE_CELL_SAFETY_RING: i32 = 1;
#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
pub(super) const ROOM_VISIBLE_CELL_NEAR_RING: i32 = 4;
#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
pub(super) const ROOM_VISIBLE_CELL_REAR_RING: i32 = 6;
#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
pub(super) const ROOM_VISIBLE_CELL_WEDGE_MARGIN_SECTORS: i32 = 3;
#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
pub(super) const ROOM_VISIBLE_CELL_WEDGE_NUM: i32 = 3;
#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
pub(super) const ROOM_VISIBLE_CELL_WEDGE_DEN: i32 = 4;
#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
pub(super) const ROOM_VISIBLE_CELL_STATIONARY_CANDIDATES: bool = true;
#[cfg(feature = "world-grid-visible")]
// Right-sized 2026-06-11 (perf-30fps RAM map): 1024-cell pools cost
// ~26KB of .bss the 2MB budget cannot spare, while gameplay telemetry
// peaks at 77 visible cells; the runtime degrades gracefully past the
// cap (overflow guards fall back to uncached selection).
pub(super) const MAX_PRECOMPUTED_VISIBLE_CELLS: usize = 192;
#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
pub(super) const MAX_ACTIVE_VISIBLE_CELLS: usize = 192;

pub(super) fn room_draw_distance(record: &LevelRoomRecord) -> i32 {
    record.draw_distance.max(NEAR_Z + 128)
}

pub(super) fn room_depth_range(record: &LevelRoomRecord) -> DepthRange {
    DepthRange::new(NEAR_Z, room_draw_distance(record))
}

/// Project-option ids cooked from demo10's screen-position settings. Applied
/// through [`Scene::apply_options`] when front-end menus publish new values and
/// again on gameplay entry, using the authentic GP1 display-window registers:
/// the classic CRT screen-position setting that slides the active window within
/// overscan without clipping.
pub(super) const SCREEN_OFFSET_X_OPTION_ID: u16 = 1;
pub(super) const SCREEN_OFFSET_Y_OPTION_ID: u16 = 2;

pub(super) fn room_surface_options(record: &LevelRoomRecord) -> WorldSurfaceOptions {
    WorldSurfaceOptions::new(WORLD_BAND, room_depth_range(record))
        .with_textured_triangle_max_edge(CACHED_ROOM_TEXTURE_SPLIT_MAX_EDGE)
}

pub(super) fn fallback_surface_options() -> WorldSurfaceOptions {
    WorldSurfaceOptions::new(WORLD_BAND, WORLD_DEPTH_RANGE)
        .with_textured_triangle_max_edge(CACHED_ROOM_TEXTURE_SPLIT_MAX_EDGE)
}

pub(super) fn current_room_surface_options(room_index: RoomIndex) -> WorldSurfaceOptions {
    ROOMS
        .get(room_index.to_usize())
        .map(room_surface_options)
        .unwrap_or_else(fallback_surface_options)
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
pub(super) fn room_chunk_activation_radius_sectors(record: &LevelRoomRecord) -> i32 {
    record.chunk_activation_radius_sectors.max(1)
}

#[cfg(feature = "cd-stream-bench")]
pub(super) fn room_resident_chunk_limit(record: &LevelRoomRecord) -> usize {
    streamed_room_slot_count_for_budget_units(record.resident_chunk_limit as usize)
        .min(MAX_RUNTIME_RESIDENT_CHUNKS)
}

#[cfg(feature = "cd-stream-bench")]
pub(super) fn room_visible_chunk_limit(record: &LevelRoomRecord) -> usize {
    usize::from(record.visible_chunk_limit.max(1)).min(MAX_ACTIVE_ROOMS)
}

pub(super) fn room_active_chunk_limit(record: &LevelRoomRecord) -> usize {
    #[cfg(feature = "cd-stream-bench")]
    {
        room_visible_chunk_limit(record).min(room_resident_chunk_limit(record))
    }
    #[cfg(not(feature = "cd-stream-bench"))]
    {
        room_visible_chunk_limit(record)
    }
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
pub(super) fn room_visibility_radius(record: &LevelRoomRecord) -> u16 {
    record.visibility_radius.max(1)
}
/// Per-frame projected scratch for one generated room surface cache.
/// Rooms that exceed this vertex budget fall back to the uncached draw.
pub(super) const MAX_CACHED_ROOM_VERTICES: usize = 4096;

/// Per-frame triangle budget sizing the primitive packet arena and the
/// world command list. Right-sized from measured data (2026-06-11): the
/// real-gameplay benchmark tape peaks at 511 tri primitives per vblank
/// (avg ~340), so 1024 is 2x the observed worst case. The old 3328
/// figure cost ~166 KB of RAM for headroom no scene used; overflow
/// degrades gracefully (commands stop accepting, counters flag it).
pub(super) const MAX_TEXTURED_TRIS: usize = 1024;

/// Cap on the per-room material slot count. Single source of truth is
/// `psx_level::MAX_ROOM_MATERIALS` (the cook<->runtime contract): the cook now
/// rejects any room that exceeds it, so an over-cap room fails loudly at cook
/// time instead of silently dropping the over-cap material at runtime. Sized to
/// comfortably exceed the cooker's emitted material count (observed max 12 in
/// demo10) without over-reserving VRAM or RAM.
pub(super) const MAX_ROOM_MATERIALS: usize = psx_level::MAX_ROOM_MATERIALS;
/// Current manual portal room plus the best cache-budgeted nearby rooms.
///
/// Upper bound for rooms that can be active, drawable, and collidable in one
/// runtime window. The world-level resident room limit picks the effective
/// count per cooked build; this cap only prevents the fixed arrays from
/// growing past the editor-exposed maximum.
pub(super) const MAX_ACTIVE_ROOMS: usize = 16;
/// Reachability draw model: the camera's room plus this many portal hops are the
/// ACTIVE/DRAWN set, with no frustum or far-plane room cull (per-polygon
/// backface + screen culling still applies). Side and behind rooms stay drawn.
pub(super) const RESIDENT_DRAW_DEPTH: u16 = 3;
/// Extra portal hops kept RESIDENT beyond the draw set (the load-ahead margin).
/// Resident radius = RESIDENT_DRAW_DEPTH + RESIDENT_PREFETCH_HOPS; since it
/// covers the draw depth, resident is a superset of drawn by construction.
pub(super) const RESIDENT_PREFETCH_HOPS: u16 = 2;
pub(super) const MAX_PORTAL_FRUSTUMS: usize = 64;
pub(super) const MAX_PORTAL_FRONTIER_ROOMS: usize = 32;
pub(super) const MAX_PORTAL_ROOM_BOUNDS: usize = 256;
pub(super) const PORTAL_ROOM_BOUNDS_MIN_Y: i32 = -4096;
pub(super) const PORTAL_ROOM_BOUNDS_MAX_Y: i32 = 8192;
pub(super) type RuntimePortalVisibility =
    PortalVisibilityResult<MAX_ACTIVE_ROOMS, MAX_PORTAL_FRUSTUMS, MAX_PORTAL_FRONTIER_ROOMS>;
/// Streamed room slot budget. A slot stores one runtime room payload:
/// the room `.psxw` plus the room-local render cache records carried by
/// the `.psxc` payload. Slots are sized to the largest payload in the cooked
/// WORLD.PAK, while the slot count is derived from a fixed byte budget so
/// smaller rooms can stay resident in larger numbers.
#[cfg(feature = "cd-stream-bench")]
pub(super) const MIN_STREAMED_ROOM_SLOT_BYTES: usize = 2048;
#[cfg(feature = "cd-stream-bench")]
pub(super) const MAX_STREAMED_ROOM_SLOT_BYTES: usize = 32 * 1024;
#[cfg(feature = "cd-stream-bench")]
pub(super) const STREAMED_ROOM_RESIDENT_BUDGET_UNIT_BYTES: usize = MAX_STREAMED_ROOM_SLOT_BYTES;
#[cfg(feature = "cd-stream-bench")]
pub(super) const STREAMED_ROOM_SLOT_BYTES: usize =
    clamp_streamed_room_slot_bytes(WORLD_PACK_MAX_CHUNK_BYTES);
#[cfg(feature = "cd-stream-bench")]
pub(super) const STREAMED_ROOM_SLOT_WORDS: usize = STREAMED_ROOM_SLOT_BYTES / 4;
#[cfg(feature = "cd-stream-bench")]
pub(super) const MAX_STREAMED_ROOM_SLOT_COUNT: usize = 256;
pub(super) const STREAMED_ROOM_SLOT_NONE: u16 = u16::MAX;
#[cfg(feature = "cd-stream-bench")]
pub(super) const MAX_STREAMED_ROOM_INDEX_COUNT: usize = 256;
/// CD-backed room residency cache. The cooked manifest selects the byte
/// budget, and the runtime converts that budget into slots sized for this
/// particular chunk layout. This preserves the authored worst-case RAM cost
/// while allowing smaller chunks to keep more neighbors resident.
#[cfg(feature = "cd-stream-bench")]
pub(super) const STREAMED_ROOM_SLOT_COUNT: usize =
    streamed_room_slot_count_for_budget_units(WORLD_RESIDENT_CHUNK_LIMIT);
#[cfg(feature = "cd-stream-bench")]
pub(super) const MAX_RUNTIME_RESIDENT_CHUNKS: usize = STREAMED_ROOM_SLOT_COUNT;
#[cfg(feature = "cd-stream-bench")]
pub(super) const MAX_COLLISION_ROOMS: usize = STREAMED_ROOM_SLOT_COUNT;
#[cfg(not(feature = "cd-stream-bench"))]
pub(super) const MAX_COLLISION_ROOMS: usize = MAX_ACTIVE_ROOMS;

#[cfg(feature = "cd-stream-bench")]
pub(super) const fn clamp_streamed_room_slot_count(raw: usize) -> usize {
    if raw < 1 {
        1
    } else if raw > MAX_STREAMED_ROOM_SLOT_COUNT {
        MAX_STREAMED_ROOM_SLOT_COUNT
    } else {
        raw
    }
}

#[cfg(feature = "cd-stream-bench")]
pub(super) const fn streamed_room_slot_count_for_budget_units(raw_units: usize) -> usize {
    let units = if raw_units < 1 { 1 } else { raw_units };
    let budget_bytes = if units > usize::MAX / STREAMED_ROOM_RESIDENT_BUDGET_UNIT_BYTES {
        usize::MAX
    } else {
        units * STREAMED_ROOM_RESIDENT_BUDGET_UNIT_BYTES
    };
    clamp_streamed_room_slot_count(budget_bytes / STREAMED_ROOM_SLOT_BYTES)
}

#[cfg(feature = "cd-stream-bench")]
pub(super) const fn clamp_streamed_room_slot_bytes(raw: usize) -> usize {
    let clamped = if raw < MIN_STREAMED_ROOM_SLOT_BYTES {
        MIN_STREAMED_ROOM_SLOT_BYTES
    } else if raw > MAX_STREAMED_ROOM_SLOT_BYTES {
        MAX_STREAMED_ROOM_SLOT_BYTES
    } else {
        raw
    };
    (clamped + 3) & !3
}
pub(super) const INVALID_ROOM_INDEX: RoomIndex = RoomIndex(u16::MAX);

/// Per-frame projected-vertex scratch for the model renderer.
/// Sized to the largest part vertex count we expect; instances
/// over this cap drop their over-budget triangles graceful.
pub(super) const MODEL_VERTEX_CAP: usize = 1024;
/// Predecoded face records shared by runtime model assets. Right-sized from
/// 4096: a single PS1 character mesh is on the order of ~100-200 faces, so the
/// shared decode pool only needs to hold the loaded models' faces (the decode
/// returns `None` and skips a model that would overflow it -- no corruption).
pub(super) const MAX_RUNTIME_MODEL_FACES: usize = 1024;
/// Predecoded part records shared by runtime model assets.
pub(super) const MAX_RUNTIME_MODEL_PARTS: usize = 128;
/// Predecoded vertex records shared by runtime model assets.
pub(super) const MAX_RUNTIME_MODEL_DECODED_VERTICES: usize = 1024;
/// Projected edge threshold used to subdivide close model triangles.
pub(super) const MODEL_TEXTURE_SPLIT_MAX_EDGE: u16 = 0;
/// Q8 fixed-point identity for per-instance visual model scale.
pub(super) const MODEL_VISUAL_SCALE_ONE_Q8: u16 = 256;
/// Joint-transform scratch -- all biped rigs we currently cook
/// fit comfortably in 32.
pub(super) const JOINT_CAP: usize = 32;
/// Cap on placed model instances rendered per frame.
pub(super) const MAX_MODEL_INSTANCES: usize = 16;
/// Cap on static boxed prop collision blockers per frame.
pub(super) const MAX_BOX_PROP_BLOCKERS: usize = 32;
/// Fixed authored box-prop state budget. Props beyond this still render
/// as static props, but cannot be toggled broken in this no-heap runtime.
pub(super) const MAX_BOX_PROP_STATE: usize = 128;
pub(super) const BOX_PROP_BROKEN_WORDS: usize = (MAX_BOX_PROP_STATE + 31) / 32;
/// Active baked break bursts retained after a prop is marked broken.
pub(super) const MAX_BOX_PROP_BREAK_EVENTS: usize = 16;
pub(super) const BOX_PROP_BREAK_FRAMES: u8 = 24;
pub(super) const BOX_PROP_BREAK_MOTION_FRAMES: u8 = 20;
pub(super) const BOX_PROP_BREAK_SHARD_COUNT: usize = 8;
/// Gravity applied to an unsupported, falling box (room units per vblank,
/// per vblank). Tuned so a stacked box drops over a handful of frames.
pub(super) const BOX_PROP_FALL_GRAVITY: i32 = 28;
/// Per-vblank fall-speed cap so a tall drop cannot tunnel past its
/// landing in one step (the landing check snaps any overshoot anyway).
pub(super) const BOX_PROP_FALL_MAX_VEL: i32 = 384;
/// Slack for "rests on the floor / on the box below" support tests, in
/// room units. Boxes are ~900+ units tall, so this only absorbs rounding
/// and small authored gaps.
pub(super) const BOX_PROP_SUPPORT_TOLERANCE: i32 = 64;
pub(super) const BOX_PROP_BREAK_ATTACK_REACH: i32 = 768;
pub(super) const BOX_PROP_BREAK_ATTACK_WIDTH: i32 = 320;
pub(super) const BOX_PROP_FACE_NORMAL_SHIFT: u32 = 10;
/// Cap on attached weapon/equipment visuals rendered per frame.
pub(super) const MAX_EQUIPMENT_DRAWS: usize = 8;
/// Runtime model cache capacity. The current playtest package only
/// needs one player model, but this keeps a little headroom for
/// lightweight NPC experiments without introducing heap allocation.
pub(super) const MAX_RUNTIME_MODELS: usize = 8;
/// Runtime animation cache capacity. Demo-scale character sets can
/// easily carry player + several enemy clip banks; keep this aligned
/// with the residency table rather than the old single-character cap.
pub(super) const MAX_RUNTIME_MODEL_CLIPS: usize = 128;
pub(super) const MODEL_PROFILE_ENABLED: bool = option_env!("PSXO_PROFILE_MODELS").is_some();
pub(super) const MODEL_BOUNDS_CULLING_ENABLED: bool =
    option_env!("PSXO_BENCH_DISABLE_MODEL_BOUNDS_CULL").is_none();
pub(super) const PROP_PARTICLE_GTE_PROJECT_ENABLED: bool =
    option_env!("PSXO_GTE_PROP_PARTICLE_PROJECT").is_some();
pub(super) const BOX_PROP_GTE_PROJECT_ENABLED: bool = true;
pub(super) const BOX_PROP_PROFILE_ENABLED: bool = option_env!("PSXO_PROFILE_BOX_PROPS").is_some();
