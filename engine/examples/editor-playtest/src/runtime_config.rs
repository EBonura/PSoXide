use super::*;
#[cfg(feature = "cd-stream-bench")]
use psx_game_runtime::asset_streaming::PersistentAssetStreamer;
#[cfg(feature = "cd-stream-bench")]
use psx_game_runtime::room_streaming::{RoomStreamScheduler, StreamedRoomPages};
#[cfg(feature = "cd-stream-bench")]
use psx_game_runtime::vram::UiImageCache;
use psx_game_runtime::vram::{FontPackScratch, VramRuntime, FONT_ATLAS_MAX_ROWS};

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
pub(super) const ROOM_TILE_TEXELS: u16 = 128;

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
pub(super) const SCREEN_W: i16 = 320;
pub(super) const SCREEN_H: i16 = 240;
pub(super) const SCREEN_CX: i16 = 160;
pub(super) const SCREEN_CY: i16 = 120;
pub(super) const FOCAL: i32 = 320;
pub(super) const NEAR_Z: i32 = 4;
pub(super) const FAR_Z: i32 = 1024;
pub(super) const PROJECTION: WorldProjection =
    WorldProjection::new(SCREEN_CX, SCREEN_CY, FOCAL, NEAR_Z);
pub(super) const SHADOW_DEPTH_BIAS: i32 = FAR_Z;
pub(super) const SHADOW_FLOOR_LIFT: i32 = 1;
pub(super) const SHADOW_RADIUS_SCALE_NUM: i32 = 5;
pub(super) const SHADOW_RADIUS_SCALE_DEN: i32 = 4;
pub(super) const SHADOW_RADIUS_MIN: i32 = 10;
pub(super) const SHADOW_RADIUS_MAX: i32 = 20;
pub(super) const COLLISION_DEBUG_BUTTON: u16 = button::L3;
pub(super) const FLOOR_LINK_CROSS_EPSILON: i32 = 2;
/// Dead-band (engine units) below a floor boundary before a downward room
/// switch fires. Climbing up lands the player AT the boundary; without a
/// margin the down-switch would immediately fire and the player would
/// thrash between floors. Must exceed `FLOOR_LINK_CROSS_EPSILON` (the
/// up-switch slack) so the up and down conditions can't both hold at the
/// seam; well under a floor's height so a real fall still registers.
pub(super) const FLOOR_LINK_SWITCH_HYSTERESIS: i32 = 16;
pub(super) const DEBUG_MAP_POSITION_BIAS: i32 = 1_000_000;

pub(super) const CAMERA_Y_OFFSET: i32 = 69;
pub(super) const CAMERA_START_RADIUS: i32 = 150;
pub(super) const CAMERA_RADIUS_MIN: i32 = 50;
pub(super) const CAMERA_RADIUS_MAX: i32 = 325;
pub(super) const CAMERA_RADIUS_STEP: i32 = 4;
pub(super) const CAMERA_START_YAW: Angle = Angle::from_q12(220);
pub(super) const CAMERA_YAW_STEP: Angle = Angle::from_q12(12);
pub(super) const CAMERA_SWEEP_ENABLED: bool = option_env!("PSXO_CAMERA_SWEEP").is_some();
/// Run the follow camera's spring-arm collision sweep every Nth tick
/// (1 = every tick, the old behavior). The sweep is ~40k of the camera's
/// ~44k per-tick cost; at 2 the collision reaction latency worst-cases
/// at 33ms while easing/pull-in still run per tick. Feel-gated by the
/// user; set back to 1 to revert.
pub(super) const CAMERA_COLLISION_SOLVE_INTERVAL: u8 = 2;
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
pub(super) const LOCK_RANGE: i32 = 256;
pub(super) const LOCK_BREAK_RANGE: i32 = 320;
/// Horizontal acquisition cone in signed Q8 screen-space (`256 = 45°`).
pub(super) const LOCK_ACQUIRE_HALF_CONE_Q8: i32 = 288;
/// Frames a still-live target may remain beyond break range before unlock.
pub(super) const LOCK_BREAK_GRACE_VBLANKS: u8 = 8;
pub(super) const SOFT_LOCK_RANGE: i32 = 192;
pub(super) const SOFT_LOCK_BREAK_RANGE: i32 = 240;
pub(super) const CAMERA_COLLISION_ENABLED: bool = true;
pub(super) const SOFT_LOCK_ENABLED: bool = false;

/// Quanta-per-frame turn rate when the runtime can't resolve a
/// Character (no PLAYER_CONTROLLER). Mirrors the pre-character
/// debug value.
pub(super) const FALLBACK_PLAYER_YAW_STEP: Angle = Angle::from_q12(32);
pub(super) const FALLBACK_PLAYER_SPEED: i32 = 2;
pub(super) const PLAYER_SPEED_SCALE_NUM: i32 = 3;
pub(super) const PLAYER_SPEED_SCALE_DEN: i32 = 4;
pub(super) const EVADE_RUN_BUTTON: u16 = button::CIRCLE;
pub(super) const EVADE_RUN_HOLD_VBLANKS: u8 = 8;
pub(super) const INTERACT_BUTTON: u16 = button::CROSS;
pub(super) const LIGHT_ATTACK_BUTTON: u16 = button::R1;
pub(super) const HEAVY_ATTACK_BUTTON: u16 = button::R2;
pub(super) const COMBO_ATTACK_BUTTON: u16 = button::L2;
/// Player health pool at gameplay init (the phase-3 combat slice's
/// sane cooked default -- the Character record carries no health
/// field yet; authoring it is a future editor slice). Death/respawn
/// handling is phase 4 (checkpoint loops): health floors at 0.
pub(super) const PLAYER_MAX_HEALTH: u16 = 100;
/// Delay between fatal BSP hazard contact and checkpoint/spawn reset.
pub(super) const BSP_HAZARD_DEATH_TICKS: u8 = 30;

#[cfg(feature = "ot-2048")]
pub(super) const OT_DEPTH: usize = 2048;
#[cfg(all(not(feature = "ot-2048"), feature = "ot-1024"))]
pub(super) const OT_DEPTH: usize = 1024;
#[cfg(all(not(feature = "ot-2048"), not(feature = "ot-1024")))]
pub(super) const OT_DEPTH: usize = 512;
/// Room geometry, actors, and shadows share one depth band so walls can
/// correctly overpaint the hidden parts of characters in the PS1
/// painter's algorithm.
// The ordinary world pass spans 0..=OT_DEPTH-2 so it stays in front of the
// sky. PXBSP's classic packet stream may also use the farthest slot; the scene
// therefore inserts the sky after that stream so same-slot OT prepend order
// still executes the sky first.
pub(super) const WORLD_BAND: DepthBand = DepthBand::new(0, OT_DEPTH - 2);
pub(super) const WORLD_DEPTH_RANGE: DepthRange = DepthRange::new(NEAR_Z, FAR_Z);
#[cfg(feature = "world-grid-visible")]
pub(super) const ROOM_VISIBLE_CELL_SCREEN_MARGIN: i32 = 0;
#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
pub(super) const ROOM_VISIBLE_CELL_CAMERA_MARGIN: i32 = 6;
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
    psx_game_runtime::world_cells::room_draw_distance(record, NEAR_Z)
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

/// Cortex tiles are smaller than the reference engine room sectors, so one four-way split gives
/// the desired affine correction without paying for the 16-leaf near band.
pub(super) const ROOM_ADAPTIVE_SUBDIVISION_LEVELS: u8 = 1;
pub(super) const ROOM_ADAPTIVE_SUBDIVISION_KINDS: AdaptiveSubdivisionKindMask =
    if cfg!(feature = "tr-subdivision-ceilings") {
        AdaptiveSubdivisionKindMask::ALL
    } else {
        AdaptiveSubdivisionKindMask::FLOOR_WALL
    };

pub(super) fn room_surface_options(record: &LevelRoomRecord) -> WorldSurfaceOptions {
    let subdivision_sector_size = if cfg!(feature = "tr-subdivision-wide-band") {
        record.sector_size.saturating_mul(4)
    } else {
        record.sector_size
    };
    WorldSurfaceOptions::new(WORLD_BAND, room_depth_range(record))
        .with_adaptive_subdivision_sector_size(subdivision_sector_size)
        .with_adaptive_subdivision_max_levels(ROOM_ADAPTIVE_SUBDIVISION_LEVELS)
        .with_adaptive_subdivision_kinds(ROOM_ADAPTIVE_SUBDIVISION_KINDS)
        .with_adaptive_subdivision_debug_levels(cfg!(feature = "tessellation-debug"))
        .with_textured_triangle_max_edge(CACHED_ROOM_TEXTURE_SPLIT_MAX_EDGE)
}

pub(super) fn fallback_surface_options() -> WorldSurfaceOptions {
    WorldSurfaceOptions::new(WORLD_BAND, WORLD_DEPTH_RANGE)
        .with_adaptive_subdivision(true)
        .with_adaptive_subdivision_max_levels(ROOM_ADAPTIVE_SUBDIVISION_LEVELS)
        .with_adaptive_subdivision_kinds(ROOM_ADAPTIVE_SUBDIVISION_KINDS)
        .with_textured_triangle_max_edge(CACHED_ROOM_TEXTURE_SPLIT_MAX_EDGE)
}

/// Room surface options for an ACTOR standing in that room.
///
/// Actors and the floor sort into the same ordering table by depth, and a
/// painter's algorithm cannot resolve a character standing ON a surface: with
/// a low camera the tile in front of her feet is genuinely nearer than her
/// torso, so it correctly wins the depth test and slices her on screen. No
/// sort-key refinement fixes that; splitting each tile into two per-triangle
/// leaves was measured to change nothing.
///
/// Tomb Raider avoids it structurally, drawing a room's geometry and then the
/// objects in that room, so an actor never competes with the floor it stands
/// on. This is that priority expressed as a depth offset: pull the actor
/// toward the camera by half a sector, which is the most a tile's centre key
/// can sit in front of a character standing on it. Derived from the room's own
/// sector size so it scales with the geometry instead of being tuned.
pub(super) fn actor_surface_options(record: &LevelRoomRecord) -> WorldSurfaceOptions {
    let clearance = i32::from(record.sector_size) / 2;
    room_surface_options(record).with_depth_bias(-clearance)
}

/// [`actor_surface_options`] for the room an actor currently occupies.
pub(super) fn current_actor_surface_options(room_index: RoomIndex) -> WorldSurfaceOptions {
    ROOMS
        .get(room_index.to_usize())
        .map(actor_surface_options)
        .unwrap_or_else(fallback_surface_options)
}

pub(super) fn current_room_surface_options(room_index: RoomIndex) -> WorldSurfaceOptions {
    ROOMS
        .get(room_index.to_usize())
        .map(room_surface_options)
        .unwrap_or_else(fallback_surface_options)
}

#[cfg(feature = "cd-stream-bench")]
pub(super) fn room_resident_chunk_limit(record: &LevelRoomRecord) -> usize {
    usize::from(record.resident_chunk_limit.max(1)).min(MAX_RUNTIME_RESIDENT_CHUNKS)
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

/// Prebuilt room-quad pool sizing: slots for recently drawn rooms and
/// the per-room quad capacity. 8 slots cover the at-most-6 rooms a
/// frame draws (visible_chunk_limit) with reuse headroom, so a slot
/// claimed this frame can never be stolen within the same frame.
/// Surfaces beyond the cap fall back to the per-frame arena path.
pub(super) const PREBUILT_ROOM_QUAD_SLOTS: usize = 8;
pub(super) const PREBUILT_ROOM_QUAD_CAP: usize = 256;

/// Per-frame packet budget sizing the primitive arena and world command list.
/// The cooked manifest derives this per project from its conservative packet
/// envelope (floor 1,536, ceiling 4,096), so heavy content sizes its own
/// arena instead of silently degrading against a fixed cap. The earlier
/// 1,024 cap covered the room benchmark but not a 529-face player drawn in
/// two material passes; overflow beyond the derived capacity still degrades
/// safely and is reported by telemetry.
pub(super) const MAX_TEXTURED_TRIS: usize = PLAYTEST_PACKET_CAPACITY;

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
/// Logical room handles and physical 2 KiB sector pages are budgeted
/// independently by the cooker. The page count covers the worst possible
/// combination of `STREAMED_ROOM_SLOT_COUNT` chunks, so no runtime selection
/// can overcommit RAM and small rooms do not pay for the largest room.
#[cfg(feature = "cd-stream-bench")]
pub(super) const MAX_STREAMED_ROOM_SLOT_COUNT: usize = 256;
#[cfg(feature = "cd-stream-bench")]
pub(super) const MAX_STREAMED_ROOM_INDEX_COUNT: usize = 256;
/// CD-backed room residency cache. The cooked manifest selects the byte
/// budget, and the runtime converts that budget into slots sized for this
/// particular chunk layout. This preserves the authored worst-case RAM cost
/// while allowing smaller chunks to keep more neighbors resident.
#[cfg(feature = "cd-stream-bench")]
pub(super) const STREAMED_ROOM_SLOT_COUNT: usize =
    clamp_streamed_room_slot_count(WORLD_STREAM_SLOT_COUNT);
#[cfg(feature = "cd-stream-bench")]
pub(super) const STREAMED_ROOM_PAGE_COUNT: usize = WORLD_RESIDENT_PAGE_COUNT;
#[cfg(feature = "cd-stream-bench")]
const _: () = assert!(
    STREAMED_ROOM_PAGE_COUNT * psx_game_runtime::cd_stream::SECTOR_BYTES
        >= WORLD_PACK_MAX_CHUNK_BYTES,
    "streaming page pool cannot hold the largest cooked room"
);
#[cfg(feature = "cd-stream-bench")]
const _: () = assert!(
    STREAMED_ROOM_SLOT_COUNT
        >= if WORLD_RESIDENT_CHUNK_LIMIT < WORLD_PACK_TOC.len() {
            WORLD_RESIDENT_CHUNK_LIMIT
        } else {
            WORLD_PACK_TOC.len()
        },
    "streaming slot count is smaller than the authored resident window"
);
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

pub(super) use psx_game_runtime::room_cache::INVALID_ROOM_INDEX;

/// Per-frame projected-vertex scratch for the model renderer.
/// Sized to the largest part vertex count we expect; instances
/// over this cap drop their over-budget triangles graceful.
pub(super) const MODEL_VERTEX_CAP: usize = 1024;
/// Predecoded face records shared by runtime model assets. The pool must hold
/// every simultaneously loaded model, not merely the largest one: Cortex's
/// 530-face enemy plus 529-face player already require 1,059 records. Keep
/// enough headroom for equipment or another compact NPC without silently
/// dropping a later model during boot-time decoding.
pub(super) const MAX_RUNTIME_MODEL_FACES: usize = 1536;
/// Predecoded part records shared by runtime model assets.
pub(super) const MAX_RUNTIME_MODEL_PARTS: usize = 128;
/// Predecoded vertex records shared by runtime model assets.
pub(super) const MAX_RUNTIME_MODEL_DECODED_VERTICES: usize = 1024;
/// Projected edge threshold used to subdivide close model triangles.
pub(super) const MODEL_TEXTURE_SPLIT_MAX_EDGE: u16 = 0;
/// Joint-transform scratch -- all biped rigs we currently cook
/// fit comfortably in 32.
pub(super) const JOINT_CAP: usize = 32;
/// Cap on placed model instances rendered per frame.
pub(super) const MAX_MODEL_INSTANCES: usize = 16;
/// Cap on cooked CylinderProps contributing one radial blocker apiece.
pub(super) const MAX_CYLINDER_PROP_BLOCKERS: usize = 32;
/// Shared fixed collision buffer for actor/model and CylinderProp blockers.
pub(super) const MAX_COLLISION_CYLINDERS: usize = MAX_MODEL_INSTANCES + MAX_CYLINDER_PROP_BLOCKERS;
/// Shared cap for BoxProp blockers and per-segment ArchProp blockers.
///
/// A maximum-detail full arch contributes 14 entries, while the default
/// contributes 8. The fixed cap keeps stack/RAM cost explicit on PS1.
pub(super) const MAX_STATIC_PROP_AABB_BLOCKERS: usize = psx_level::MAX_STATIC_PROP_AABB_BLOCKERS;
/// Authored box-prop state budget, cooked from the project's actual prop
/// count. Each slot costs about 1.4 KB, so a fixed worst-case budget spent
/// a large share of the PS1's 2 MB on props that do not exist. Props beyond
/// this still render as static props, but cannot be toggled broken in this
/// no-heap runtime.
pub(super) const MAX_BOX_PROP_STATE: usize = BOX_PROP_STATE_COUNT;
pub(super) const BOX_PROP_BROKEN_WORDS: usize = (MAX_BOX_PROP_STATE + 31) / 32;
/// Active baked break bursts retained after a prop is marked broken.
pub(super) const MAX_BOX_PROP_BREAK_EVENTS: usize = 16;
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

// ---------------------------------------------------------------------------
// RuntimeBudgets: every `psx_game_runtime` const-generic instantiation, in one
// place (phase 1.5 of docs/game-runtime-plan.md). Each alias below names the
// budget consts a crate type is instantiated with, so this section is the
// single place to read what this game grants the runtime. The consts feeding
// the aliases carry their provenance; the `build.rs`-generated `RuntimeBudgets`
// value struct replaces the hand-written numbers in a later pass.
// ---------------------------------------------------------------------------

/// Capacity of the residency manager's RAM table. Holds room
/// world + model meshes + animation clips.
pub(super) const MAX_RESIDENT_RAM_ASSETS: usize = 128;
/// Capacity of the residency manager's VRAM table. Holds room
/// material atlases + model atlases.
pub(super) const MAX_RESIDENT_VRAM_ASSETS: usize = 64;
/// CLUT-band rows the unified VRAM allocator manages, just past the back
/// buffer (Stage 1: only the shared font CLUT lands here).
pub(super) const VRAM_CLUT_ROWS: usize = 16;
/// The crate VRAM runtime (slot table, unified allocator, residency
/// tracker, upload queue) instantiated with this example's budget consts.
pub(super) type RuntimeVram = VramRuntime<
    MAX_RESIDENT_RAM_ASSETS,
    MAX_RESIDENT_VRAM_ASSETS,
    ROOM_TPAGE_COUNT,
    VRAM_CLUT_ROWS,
>;

/// Font-pack staging length in u16s, sized to the larger of the two scratch
/// uses (font atlas packing vs the streamed sky chunk); see the doc on
/// `psx_game_runtime::vram::FontPackScratch`.
const FONT_PACK_U16: usize = MAX_RUNTIME_UI_FONTS * 64 * FONT_ATLAS_MAX_ROWS;
#[cfg(feature = "cd-stream-bench")]
pub(super) const FONT_PACK_SCRATCH_LEN: usize = {
    let sky_u16 = (GAMEPLAY_PACK_MAX_CHUNK_BYTES + 1) / 2;
    if FONT_PACK_U16 > sky_u16 {
        FONT_PACK_U16
    } else {
        sky_u16
    }
};
#[cfg(not(feature = "cd-stream-bench"))]
pub(super) const FONT_PACK_SCRATCH_LEN: usize = FONT_PACK_U16;
/// The crate font/sky staging scratch instantiated with this example's length.
pub(super) type RuntimeFontPackScratch = FontPackScratch<FONT_PACK_SCRATCH_LEN>;

#[cfg(feature = "cd-stream-bench")]
const SKY_STAGE_WORDS: usize = (GAMEPLAY_PACK_MAX_CHUNK_BYTES + 3) / 4;
#[cfg(feature = "cd-stream-bench")]
const _: () = assert!(
    SKY_STAGE_WORDS * 4 <= FONT_PACK_SCRATCH_LEN * 2,
    "streamed sky chunk does not fit the font-pack staging buffer"
);

/// RAM cache slot width for one streamed menu UI image chunk.
#[cfg(feature = "cd-stream-bench")]
pub(super) const UI_STAGE_WORDS: usize = (UI_PACK_MAX_CHUNK_BYTES + 3) / 4;
/// The crate streamed-menu-UI image cache instantiated with this example's
/// chunk width and the cooked manifest's slot count.
#[cfg(feature = "cd-stream-bench")]
pub(super) type RuntimeUiImageCache = UiImageCache<UI_STAGE_WORDS, UI_PACK_IMAGE_CACHE_SLOTS>;

/// Sector-page room cache instantiated with the cook-time proven page budget.
#[cfg(feature = "cd-stream-bench")]
pub(super) type RuntimeStreamedRoomSlots =
    StreamedRoomPages<STREAMED_ROOM_PAGE_COUNT, STREAMED_ROOM_SLOT_COUNT>;

/// Persistent model/animation cache sized exactly by the cooked asset table.
#[cfg(feature = "cd-stream-bench")]
pub(super) type RuntimePersistentAssetStreamer =
    PersistentAssetStreamer<PERSISTENT_ASSET_PAGE_COUNT, PERSISTENT_ASSET_SLOT_COUNT>;

/// The crate streamed-room scheduler instantiated with this example's slot
/// count and room-index capacity.
#[cfg(feature = "cd-stream-bench")]
pub(super) type RuntimeRoomStreamScheduler =
    RoomStreamScheduler<STREAMED_ROOM_SLOT_COUNT, MAX_STREAMED_ROOM_INDEX_COUNT>;

/// The crate per-stream-slot room-material pool instantiated with this
/// example's slot count.
#[cfg(feature = "cd-stream-bench")]
pub(super) type RuntimeRoomMaterialPool =
    psx_game_runtime::room_cache::RoomMaterialPool<STREAMED_ROOM_SLOT_COUNT>;

/// The crate prebuilt room-quad pool instantiated with this example's slot
/// and per-room quad budgets (see `PREBUILT_ROOM_QUAD_*` above).
pub(super) type RuntimePrebuiltRoomQuads = psx_game_runtime::room_cache::PrebuiltRoomQuads<
    PREBUILT_ROOM_QUAD_SLOTS,
    PREBUILT_ROOM_QUAD_CAP,
>;

/// Crate-owned portal-visibility state instantiated with this example's
/// budget consts (its `result` field is a [`RuntimePortalVisibility`]).
pub(super) type RuntimeRoomVisibility = psx_game_runtime::room_visibility::RoomVisibility<
    MAX_ACTIVE_ROOMS,
    MAX_PORTAL_FRUSTUMS,
    MAX_PORTAL_FRONTIER_ROOMS,
    MAX_PORTAL_ROOM_BOUNDS,
>;
/// Crate-owned active-room window state instantiated with this
/// example's window capacity.
pub(super) type RuntimeRoomWindow = psx_game_runtime::room_window::RoomWindow<MAX_ACTIVE_ROOMS>;

/// Crate-owned box-prop state instantiated with this example's
/// state/word/event budgets (see `MAX_BOX_PROP_*` above).
pub(super) type RuntimeBoxProps = psx_game_runtime::box_props::BoxProps<
    MAX_BOX_PROP_STATE,
    BOX_PROP_BROKEN_WORDS,
    MAX_BOX_PROP_BREAK_EVENTS,
>;

/// Crate-owned visible-cell selection state instantiated with this
/// example's window/pool/candidate capacities.
#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
pub(super) type RuntimeVisibleCellSelector = psx_game_runtime::world_cells::VisibleCellSelector<
    MAX_ACTIVE_ROOMS,
    MAX_ACTIVE_VISIBLE_CELLS,
    MAX_PRECOMPUTED_VISIBLE_CELLS,
>;

/// The crate accepted-cell draw scratch instantiated with this
/// example's candidate capacity.
#[cfg(feature = "world-grid-visible")]
pub(super) type RuntimeCellDrawScratch =
    psx_game_runtime::world_cells::CellDrawScratch<MAX_PRECOMPUTED_VISIBLE_CELLS>;

/// The crate model projected-vertex + joint scratch instantiated with
/// this example's caps (see `MODEL_VERTEX_CAP`/`JOINT_CAP` above).
pub(super) type RuntimeModelDrawScratch =
    psx_game_runtime::model_rendering::ModelDrawScratch<MODEL_VERTEX_CAP, JOINT_CAP>;

/// The crate cached-room projection scratch instantiated with this
/// example's per-room vertex budget (see `MAX_CACHED_ROOM_VERTICES`).
pub(super) type RuntimeCachedRoomProjection =
    psx_game_runtime::room_cache::CachedRoomProjection<MAX_CACHED_ROOM_VERTICES>;

/// Phase-3 gameplay capacities (docs/game-runtime-plan.md, "Phase 3
/// budget"). The record caps are the psx-level cook<->runtime
/// contract (the cook rejects over-cap content), so the SoA arrays
/// here can never silently drop a cooked record.
pub(super) const MAX_GAME_ENTITIES: usize = psx_level::MAX_GAME_ENTITY_RECORDS;
/// See [`MAX_GAME_ENTITIES`].
pub(super) const MAX_LOGIC_RECORDS: usize = psx_level::MAX_LOGIC_RECORDS;
/// Fired-bitset words for the logic runtime (the BoxProps
/// broken-words pattern).
pub(super) const LOGIC_FIRED_WORDS: usize = MAX_LOGIC_RECORDS.div_ceil(32);
/// In-flight delayed logic events (budget line: hl-psx ships 64 for
/// full HL campaign maps; one 8-room cortex level gets 32 plus an
/// overflow counter).
pub(super) const MAX_LOGIC_EVENTS: usize = 32;

/// The crate souls-like entity state instantiated with this example's
/// entity cap.
pub(super) type RuntimeGameEntities = psx_game_runtime::entities::GameEntities<MAX_GAME_ENTITIES>;

/// Caller-owned one-tick handoff between entity state advancement and the
/// retained-pose combat pass. At most every live entity can attack in one
/// tick, though the combat director normally grants a single slot.
pub(super) type RuntimeDeferredEnemyAttacks =
    psx_game_runtime::entities::DeferredGameEntityAttacks<MAX_GAME_ENTITIES>;

/// The crate logic-entity runtime instantiated with this example's
/// record/word/event caps.
pub(super) type RuntimeLogic =
    psx_game_runtime::logic::LogicRuntime<MAX_LOGIC_RECORDS, LOGIC_FIRED_WORDS, MAX_LOGIC_EVENTS>;

/// This example's visible-cell selection tuning (the
/// `ROOM_VISIBLE_CELL_*` consts above, as the crate value struct).
#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
pub(super) const VISIBLE_CELL_TUNING: psx_game_runtime::world_cells::VisibleCellTuning =
    psx_game_runtime::world_cells::VisibleCellTuning {
        screen_margin: ROOM_VISIBLE_CELL_SCREEN_MARGIN,
        camera_margin: ROOM_VISIBLE_CELL_CAMERA_MARGIN,
        safety_ring: ROOM_VISIBLE_CELL_SAFETY_RING,
        near_ring: ROOM_VISIBLE_CELL_NEAR_RING,
        rear_ring: ROOM_VISIBLE_CELL_REAR_RING,
        wedge_margin_sectors: ROOM_VISIBLE_CELL_WEDGE_MARGIN_SECTORS,
        wedge_num: ROOM_VISIBLE_CELL_WEDGE_NUM,
        wedge_den: ROOM_VISIBLE_CELL_WEDGE_DEN,
        near_z: NEAR_Z,
    };
