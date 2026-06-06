//! `editor-playtest` -- render a level cooked from the editor.
//!
//! Loads a Rust manifest selected by `build.rs`: the ignored
//! `generated/level_manifest.cooked.rs` when the editor has
//! cooked a project, otherwise the tracked placeholder
//! `generated/level_manifest.rs`. The cooked manifest contains:
//!
//! * a master [`LevelAssetRecord`] table -- every cooked
//!   `.psxw` room blob and `.psxt` texture blob is a record;
//! * per-room [`LevelMaterialRecord`]s mapping each cooked
//!   local material slot to a texture asset id;
//! * per-room [`RoomResidencyRecord`]s declaring required
//!   RAM/VRAM assets;
//! * a [`PlayerSpawnRecord`] and [`EntityRecord`]s.
//!
//! The runtime resolves the active room by walking `ASSETS`,
//! uploads its texture assets through a tiny no-alloc
//! [`ResidencyManager`], builds a `TextureMaterial` table from
//! the room's material slice, and renders. No hardcoded starter
//! textures -- the asset table is the source of truth.
//!
//! Controls (free-orbit toggled with SELECT):
//! * Left stick / D-pad -- camera-relative movement.
//! * Right stick        -- camera yaw; vertical adjusts camera height.
//! * CIRCLE tap        -- roll / backstep.
//! * CIRCLE hold       -- run while moving.
//! * R1                -- light attack.
//! * R2                -- heavy attack.

#![no_std]
#![no_main]
#![allow(static_mut_refs)]

extern crate psx_rt;

use psx_asset::{Animation, Model, ModelPart, ModelVertex, Texture};
#[cfg(feature = "vis-full-active-chunks")]
use psx_engine::draw_indexed_cached_room_vertex_lit_all_cells;
use psx_engine::ui::UiTextureSlot;
#[cfg(feature = "cd-stream-bench")]
use psx_engine::CompactCollisionRoom;
#[cfg(feature = "world-grid-visible")]
use psx_engine::GridVisibilityStats;
#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
use psx_engine::GridVisibleCell;
use psx_engine::SkyDirectionProjector;
use psx_engine::{
    apply_model_pose_translation, button, compute_joint_world_transform, telemetry, Angle, App,
    CachedRoomCell, CachedRoomDepthMode, CachedRoomSubdivisionMode, CachedRoomSurface,
    CharacterCollision, CharacterCollisionAabb, CharacterCollisionCylinder, CharacterCollisionRoom,
    CharacterMotorAnim, CharacterMotorConfig, CharacterMotorInput, CharacterMotorState, Config,
    Ctx, CullMode, DepthBand, DepthPolicy, DepthRange, JointViewTransform, JointWorldTransform,
    LoadedWorldCameraGte, LocalToWorldScale, Mat3I16, MaterialTint, ModelPoseTranslation, OtFrame,
    PointLightSample, PrimitivePacketArena, PrimitivePacketScratch, PrimitiveSink, ProjectedVertex,
    Rgb8, RoomPoint, RoomRender, RuntimeCollisionRoom, RuntimeRoom, Scene, SceneStateRef,
    SchedulerConfig, SimTick, TexturedModelGeometry, TexturedModelRenderFace,
    TexturedModelRenderStats, ThirdPersonCameraConfig, ThirdPersonCameraInput,
    ThirdPersonCameraState, ThirdPersonCameraTarget, VideoHz, VisualPacing, WorldCamera,
    WorldProjection, WorldRenderMaterial, WorldRenderPass, WorldSurfaceLighting,
    WorldSurfaceOptions, WorldSurfaceSample, WorldTriCommand, WorldVertex, Q12, Q8,
};
use psx_engine::{
    cached_room_cells_from_level_records, cached_room_surfaces_from_level_records,
    cached_room_vertices_from_level_records, draw_room_vertex_lit,
};
#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
use psx_engine::{
    draw_indexed_cached_room_vertex_lit_visible_cells, draw_room_vertex_lit_visible_cells,
    GridVisibility,
};
use psx_font::FontAtlas;
use psx_gpu::{
    draw_line_mono, draw_tri_flat_blended,
    material::{BlendMode, TextureMaterial, TextureWindow},
    ot::OrderingTable,
    prim::{QuadTexturedGouraud, QuadTexturedMaterial, TriTextured, TriTexturedGouraud},
    VideoMode,
};
use psx_level::portal_visibility::{
    build_portal_visibility_with_room_bounds, debug_portal_clip, PortalClipDebug,
    PortalClipDebugDecision, PortalClipDebugPlane, PortalClipDebugRect, PortalFrustum,
    PortalRoomBounds, PortalVisibilityCamera, PortalVisibilityResult,
};
use psx_level::{
    box_prop_flags, character_action_flags, equipment_flags, far_vista_flags, find_asset_of_kind,
    image_prop_flags, model_clip_flags, particle_emitter_flags, room_flags, sky_flags,
    visibility_cell_flags, AssetId, AssetKind, CharacterAnimationAction, EntityRecord,
    InteractableKind, InteractableRecord, LevelBoxPropRecord, LevelCameraRecord,
    LevelCharacterRecord, LevelChunkRecord, LevelFarVistaRecord, LevelImagePropRecord,
    LevelMaterialRecord, LevelMaterialSidedness, LevelModelFrameBoundsRecord, LevelModelRecord,
    LevelModelSocketRecord, LevelRoomRecord, LevelSkyRecord, ModelClipIndex, ModelClipTableIndex,
    ModelIndex, ModelSocketIndex, OptionalModelClipIndex, ParticleEmitterRecord, ResidencyManager,
    RoomIndex, RuntimeDebugMask, WeaponHitShapeRecord, CHARACTER_ANIMATION_ACTION_COUNT,
};
#[cfg(feature = "cd-stream-bench")]
use psx_level::{
    streamed_room_chunk_header, LevelCachedRoomCellRecord, LevelCachedRoomSurfaceRecord,
    LevelCachedRoomVertexRecord, STREAMED_ROOM_CHUNK_FLAG_COLLISION_COMPACT,
    STREAMED_ROOM_CHUNK_HEADER_BYTES, STREAMED_ROOM_CHUNK_MAGIC, STREAMED_ROOM_CHUNK_VERSION,
};
use psx_vram::{TexDepth, Tpage};

mod active_rooms;
mod box_props;
#[cfg(feature = "cd-stream-bench")]
mod cd_stream;
mod debug_runtime;
mod image_props_runtime;
mod input;
mod marker_runtime;
mod model_rendering;
mod overlay;
mod particle_runtime;
mod runtime_schedule;
mod sky_runtime;
mod visibility_runtime;
mod vram_runtime;
mod vram_upload;

use active_rooms::*;
use box_props::*;
use debug_runtime::*;
use image_props_runtime::*;
use input::*;
use marker_runtime::*;
use model_rendering::*;
use overlay::*;
use particle_runtime::*;
use runtime_schedule::RUNTIME_SCHEDULE;
use sky_runtime::*;
use visibility_runtime::*;
use vram_runtime::*;

// Placeholder manifests reference unused statics; populated
// manifests reference all of them. Quiet either side here.
#[allow(dead_code, unused_imports)]
mod generated {
    include!(env!("PSXED_PLAYTEST_MANIFEST"));
}

use generated::{
    ASSETS, BOX_PROPS, CACHED_ROOM_DEPTH_MODE, CACHED_ROOM_DRAW_ORDER_MODE,
    CACHED_ROOM_TEXTURE_SPLIT_MAX_EDGE, CACHED_ROOM_TEXTURE_SPLIT_MODE, CHARACTERS, ENTITIES,
    EQUIPMENT, IMAGE_PROPS, INTERACTABLES, INTERACTABLE_MESSAGES, LIGHTS, MATERIALS, MODELS,
    MODEL_CLIPS, MODEL_CLIP_BOUNDS, MODEL_FRAME_BOUNDS, MODEL_INSTANCES, MODEL_SOCKETS,
    PARTICLE_EMITTERS, PLAYER_CONTROLLER, PLAYER_SPAWN, ROOMS, ROOM_CACHE_CELLS,
    ROOM_CACHE_CELL_VERTICES, ROOM_CACHE_SURFACES, ROOM_CACHE_VERTICES, ROOM_CHUNKS, ROOM_PORTALS,
    ROOM_RESIDENCY, ROOM_SURFACE_CACHES, ROOM_VISIBILITY, UI_FONTS, UI_NODES, UI_PAINTS,
    UI_SFX_CUES, UI_SFX_SAMPLES, VISIBILITY_CELLS, WEAPONS, WEAPON_HITBOXES,
};
use generated::{GAME_FLOW, OPTIONS, UI_SCENES};
#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
use generated::{VISIBILITY_PVS, VISIBILITY_PVS_BITS};
#[cfg(feature = "cd-stream-bench")]
use generated::{
    WORLD_PACK_MAX_CHUNK_BYTES, WORLD_PACK_START_LBA, WORLD_PACK_TOC, WORLD_RESIDENT_CHUNK_LIMIT,
};

const fn cached_room_depth_mode() -> CachedRoomDepthMode {
    match CACHED_ROOM_DEPTH_MODE {
        0 => CachedRoomDepthMode::FixedCell,
        2 => CachedRoomDepthMode::HybridWalls,
        3 => CachedRoomDepthMode::PerTriangle,
        _ => CachedRoomDepthMode::Hybrid,
    }
}

const fn cached_room_subdivision_mode() -> CachedRoomSubdivisionMode {
    match CACHED_ROOM_TEXTURE_SPLIT_MODE {
        1 => CachedRoomSubdivisionMode::DepthSorted,
        2 => CachedRoomSubdivisionMode::Risky,
        _ => CachedRoomSubdivisionMode::All,
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum CachedRoomDrawOrderMode {
    Distance,
    Portal,
    Slot,
}

const fn cached_room_draw_order_mode() -> CachedRoomDrawOrderMode {
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
const ROOM_TPAGE_BASE_X: u16 = 640;
const SHARED_TPAGE: Tpage = Tpage::new(ROOM_TPAGE_BASE_X, 0, TexDepth::Bit4);
const TPAGE_WORD: u16 = SHARED_TPAGE.uv_tpage_word(0);
const ROOM_TPAGE_STRIDE_HW: u16 = 64;
const ROOM_TPAGE_LIMIT_X: u16 = 1024;
const ROOM_TPAGE_COUNT: usize =
    ((ROOM_TPAGE_LIMIT_X - ROOM_TPAGE_BASE_X) / ROOM_TPAGE_STRIDE_HW) as usize;
const ROOM_TILE_TEXELS: u16 = 64;

const MODEL_TPAGE: Tpage = Tpage::new(384, 256, TexDepth::Bit8);
/// Maximum halfword width addressable by one 8bpp texture page.
const MODEL_TPAGE_MAX_HALFWORDS: u16 = 128;

/// Runtime UI font slots. The cooked manifest compacts authored font choices
/// into these slots, so only fonts actually used by cooked UI text are uploaded.
const MAX_RUNTIME_UI_FONTS: usize = 4;
/// Resource-set key shared by every flow state: the UI font atlas is used by
/// the menus and the gameplay HUD, so it is acquired once and never torn down.
const UI_FONT_RESOURCE_KEY: u32 = 1;
static SHADOW_CIRCLE_BLOB: &[u8] = include_bytes!("../assets/shadow_circle_64.psxt");
/// Shadow decals share the shadow/particle 4bpp page allocated by the unified
/// VRAM allocator. UVs are page-relative, so only the page base moves.
const SHADOW_TEXEL_U: u8 = 64;
const SHADOW_UV_MAX: u8 = SHADOW_TEXEL_U + 63;
const SCREEN_W: i16 = 320;
const SCREEN_H: i16 = 240;
const SCREEN_CX: i16 = 160;
const SCREEN_CY: i16 = 120;
const FOCAL: i32 = 320;
const NEAR_Z: i32 = 64;
const FAR_Z: i32 = 16384;
const PROJECTION: WorldProjection = WorldProjection::new(SCREEN_CX, SCREEN_CY, FOCAL, NEAR_Z);
const SHADOW_DEPTH_BIAS: i32 = FAR_Z;
const SHADOW_FLOOR_LIFT: i32 = 4;
const SHADOW_RADIUS_SCALE_NUM: i32 = 5;
const SHADOW_RADIUS_SCALE_DEN: i32 = 4;
const SHADOW_RADIUS_MIN: i32 = 160;
const SHADOW_RADIUS_MAX: i32 = 320;
const COLLISION_DEBUG_BUTTON: u16 = button::L3;
const COLLISION_DEBUG_SEGMENTS: usize = 8;
const COLLISION_DEBUG_FLOOR_LIFT: i32 = 8;
const FLOOR_LINK_CROSS_EPSILON: i32 = 32;
/// Dead-band (engine units) below a floor boundary before a downward room
/// switch fires. Climbing up lands the player AT the boundary; without a
/// margin the down-switch would immediately fire and the player would
/// thrash between floors. Must exceed `FLOOR_LINK_CROSS_EPSILON` (the
/// up-switch slack) so the up and down conditions can't both hold at the
/// seam; well under a floor's height so a real fall still registers.
const FLOOR_LINK_SWITCH_HYSTERESIS: i32 = 256;
const DEBUG_MAP_POSITION_BIAS: i32 = 1_000_000;

const CAMERA_Y_OFFSET: i32 = 1100;
const CAMERA_START_RADIUS: i32 = 2400;
const CAMERA_RADIUS_MIN: i32 = 800;
const CAMERA_RADIUS_MAX: i32 = 5200;
const CAMERA_RADIUS_STEP: i32 = 64;
const CAMERA_START_YAW: Angle = Angle::from_q12(220);
const CAMERA_YAW_STEP: Angle = Angle::from_q12(12);
const CAMERA_SWEEP_ENABLED: bool = option_env!("PSXO_CAMERA_SWEEP").is_some();
const CAMERA_SWEEP_FAST_ENABLED: bool = option_env!("PSXO_CAMERA_SWEEP_FAST").is_some();
const CAMERA_SWEEP_WIDE_ENABLED: bool = option_env!("PSXO_CAMERA_SWEEP_WIDE").is_some();
const CAMERA_SWEEP_FORCE_VISIBILITY: bool = option_env!("PSXO_CAMERA_SWEEP_FORCE_VIS").is_some();
const CAMERA_SWEEP_YAW_STEP_Q12: i16 = if CAMERA_SWEEP_FAST_ENABLED { 96 } else { 4 };
const CAMERA_SWEEP_RADIUS: i32 = if CAMERA_SWEEP_WIDE_ENABLED {
    CAMERA_RADIUS_MAX
} else {
    CAMERA_START_RADIUS
};
const MOVE_STICK_DEADZONE: i16 = 18;
const STICK_MAX: i16 = 127;
const CAMERA_STICK_DEADZONE: i16 = 18;
const CAMERA_STICK_YAW_STEP: i16 = 64;
const CAMERA_STICK_PITCH_STEP: i16 = 48;
const CAMERA_SOFT_LOCK_BREAK_STICK: i16 = 72;
const LOCK_SWITCH_STICK_THRESHOLD: i16 = 72;
const LOCK_SWITCH_STICK_RELEASE: i16 = 36;
const LOCK_RANGE: i32 = 4096;
const LOCK_BREAK_RANGE: i32 = 5120;
const SOFT_LOCK_RANGE: i32 = 3072;
const SOFT_LOCK_BREAK_RANGE: i32 = 3840;
const CAMERA_COLLISION_ENABLED: bool = true;
const SOFT_LOCK_ENABLED: bool = false;

/// Quanta-per-frame turn rate when the runtime can't resolve a
/// Character (no PLAYER_CONTROLLER). Mirrors the pre-character
/// debug value.
const FALLBACK_PLAYER_YAW_STEP: Angle = Angle::from_q12(32);
const FALLBACK_PLAYER_SPEED: i32 = 32;
const PLAYER_SPEED_SCALE_NUM: i32 = 3;
const PLAYER_SPEED_SCALE_DEN: i32 = 4;
const EVADE_RUN_BUTTON: u16 = button::CIRCLE;
const EVADE_RUN_HOLD_VBLANKS: u8 = 8;
const INTERACT_BUTTON: u16 = button::CROSS;
const LIGHT_ATTACK_BUTTON: u16 = button::R1;
const HEAVY_ATTACK_BUTTON: u16 = button::R2;

#[cfg(feature = "ot-2048")]
const OT_DEPTH: usize = 2048;
#[cfg(all(not(feature = "ot-2048"), feature = "ot-1024"))]
const OT_DEPTH: usize = 1024;
#[cfg(all(not(feature = "ot-2048"), not(feature = "ot-1024")))]
const OT_DEPTH: usize = 512;
/// Room geometry, actors, and shadows share one depth band so walls can
/// correctly overpaint the hidden parts of characters in the PS1
/// painter's algorithm.
// Farthest slot (OT_DEPTH - 1) is reserved for the sky cyclorama (see
// SKY_OT_SLOT), so world geometry spans 0..=OT_DEPTH-2 and always draws in
// front of the sky.
const WORLD_BAND: DepthBand = DepthBand::new(0, OT_DEPTH - 2);
const WORLD_DEPTH_RANGE: DepthRange = DepthRange::new(NEAR_Z, FAR_Z);
#[cfg(feature = "world-grid-visible")]
const ROOM_VISIBLE_CELL_SCREEN_MARGIN: i32 = 0;
#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
const ROOM_VISIBLE_CELL_CAMERA_MARGIN: i32 = 96;
#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
const ROOM_VISIBLE_CELL_SAFETY_RING: i32 = 1;
#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
const ROOM_VISIBLE_CELL_NEAR_RING: i32 = 4;
#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
const ROOM_VISIBLE_CELL_REAR_RING: i32 = 6;
#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
const ROOM_VISIBLE_CELL_WEDGE_MARGIN_SECTORS: i32 = 3;
#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
const ROOM_VISIBLE_CELL_WEDGE_NUM: i32 = 3;
#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
const ROOM_VISIBLE_CELL_WEDGE_DEN: i32 = 4;
#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
const ROOM_VISIBLE_CELL_STATIONARY_CANDIDATES: bool = true;
#[cfg(feature = "world-grid-visible")]
const MAX_PRECOMPUTED_VISIBLE_CELLS: usize = 1024;
#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
const MAX_ACTIVE_VISIBLE_CELLS: usize = 1024;

fn room_draw_distance(record: &LevelRoomRecord) -> i32 {
    record.draw_distance.max(NEAR_Z + 128)
}

fn room_depth_range(record: &LevelRoomRecord) -> DepthRange {
    DepthRange::new(NEAR_Z, room_draw_distance(record))
}

/// Project-option ids cooked from demo10's screen-position settings. Applied
/// through [`Scene::apply_options`] when front-end menus publish new values and
/// again on gameplay entry, using the authentic GP1 display-window registers:
/// the classic CRT screen-position setting that slides the active window within
/// overscan without clipping.
const SCREEN_OFFSET_X_OPTION_ID: u16 = 1;
const SCREEN_OFFSET_Y_OPTION_ID: u16 = 2;

fn room_surface_options(record: &LevelRoomRecord) -> WorldSurfaceOptions {
    WorldSurfaceOptions::new(WORLD_BAND, room_depth_range(record))
        .with_textured_triangle_max_edge(CACHED_ROOM_TEXTURE_SPLIT_MAX_EDGE)
}

fn fallback_surface_options() -> WorldSurfaceOptions {
    WorldSurfaceOptions::new(WORLD_BAND, WORLD_DEPTH_RANGE)
        .with_textured_triangle_max_edge(CACHED_ROOM_TEXTURE_SPLIT_MAX_EDGE)
}

fn current_room_surface_options(room_index: RoomIndex) -> WorldSurfaceOptions {
    ROOMS
        .get(room_index.to_usize())
        .map(room_surface_options)
        .unwrap_or_else(fallback_surface_options)
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn room_chunk_activation_radius_sectors(record: &LevelRoomRecord) -> i32 {
    record.chunk_activation_radius_sectors.max(1)
}

#[cfg(feature = "cd-stream-bench")]
fn room_resident_chunk_limit(record: &LevelRoomRecord) -> usize {
    streamed_room_slot_count_for_budget_units(record.resident_chunk_limit as usize)
        .min(MAX_RUNTIME_RESIDENT_CHUNKS)
}

#[cfg(feature = "cd-stream-bench")]
fn room_visible_chunk_limit(record: &LevelRoomRecord) -> usize {
    usize::from(record.visible_chunk_limit.max(1)).min(MAX_ACTIVE_ROOMS)
}

fn room_active_chunk_limit(record: &LevelRoomRecord) -> usize {
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
fn room_visibility_radius(record: &LevelRoomRecord) -> u16 {
    record.visibility_radius.max(1)
}
/// Per-frame projected scratch for one generated room surface cache.
/// Rooms that exceed this vertex budget fall back to the uncached draw.
const MAX_CACHED_ROOM_VERTICES: usize = 4096;

const MAX_TEXTURED_TRIS: usize = 3328;

/// Cap on the per-room material slot count. Picked to comfortably
/// exceed the cooker's currently-emitted material count without
/// over-reserving VRAM or RAM. If a future room exceeds this,
/// the runtime fails graceful (skips the over-cap material) and
/// the cook report should also flag.
const MAX_ROOM_MATERIALS: usize = 8;
/// Current manual portal room plus the best cache-budgeted nearby rooms.
///
/// Upper bound for rooms that can be active, drawable, and collidable in one
/// runtime window. The world-level resident room limit picks the effective
/// count per cooked build; this cap only prevents the fixed arrays from
/// growing past the editor-exposed maximum.
const MAX_ACTIVE_ROOMS: usize = 16;
/// Reachability draw model: the camera's room plus this many portal hops are the
/// ACTIVE/DRAWN set, with no frustum or far-plane room cull (per-polygon
/// backface + screen culling still applies). Side and behind rooms stay drawn.
const RESIDENT_DRAW_DEPTH: u16 = 3;
/// Extra portal hops kept RESIDENT beyond the draw set (the load-ahead margin).
/// Resident radius = RESIDENT_DRAW_DEPTH + RESIDENT_PREFETCH_HOPS; since it
/// covers the draw depth, resident is a superset of drawn by construction.
const RESIDENT_PREFETCH_HOPS: u16 = 2;
const MAX_PORTAL_FRUSTUMS: usize = 64;
const MAX_PORTAL_FRONTIER_ROOMS: usize = 32;
const MAX_PORTAL_ROOM_BOUNDS: usize = 256;
const PORTAL_ROOM_BOUNDS_MIN_Y: i32 = -4096;
const PORTAL_ROOM_BOUNDS_MAX_Y: i32 = 8192;
type RuntimePortalVisibility =
    PortalVisibilityResult<MAX_ACTIVE_ROOMS, MAX_PORTAL_FRUSTUMS, MAX_PORTAL_FRONTIER_ROOMS>;
/// Streamed room slot budget. A slot stores one runtime room payload:
/// the room `.psxw` plus the room-local render cache records carried by
/// the `.psxc` payload. Slots are sized to the largest payload in the cooked
/// WORLD.PAK, while the slot count is derived from a fixed byte budget so
/// smaller rooms can stay resident in larger numbers.
#[cfg(feature = "cd-stream-bench")]
const MIN_STREAMED_ROOM_SLOT_BYTES: usize = 2048;
#[cfg(feature = "cd-stream-bench")]
const MAX_STREAMED_ROOM_SLOT_BYTES: usize = 32 * 1024;
#[cfg(feature = "cd-stream-bench")]
const STREAMED_ROOM_RESIDENT_BUDGET_UNIT_BYTES: usize = MAX_STREAMED_ROOM_SLOT_BYTES;
#[cfg(feature = "cd-stream-bench")]
const STREAMED_ROOM_SLOT_BYTES: usize = clamp_streamed_room_slot_bytes(WORLD_PACK_MAX_CHUNK_BYTES);
#[cfg(feature = "cd-stream-bench")]
const STREAMED_ROOM_SLOT_WORDS: usize = STREAMED_ROOM_SLOT_BYTES / 4;
#[cfg(feature = "cd-stream-bench")]
const MAX_STREAMED_ROOM_SLOT_COUNT: usize = 256;
#[cfg(feature = "cd-stream-bench")]
const STREAMED_ROOM_SLOT_NONE: u16 = u16::MAX;
#[cfg(feature = "cd-stream-bench")]
const MAX_STREAMED_ROOM_INDEX_COUNT: usize = 256;
/// CD-backed room residency cache. The cooked manifest selects the byte
/// budget, and the runtime converts that budget into slots sized for this
/// particular chunk layout. This preserves the authored worst-case RAM cost
/// while allowing smaller chunks to keep more neighbors resident.
#[cfg(feature = "cd-stream-bench")]
const STREAMED_ROOM_SLOT_COUNT: usize =
    streamed_room_slot_count_for_budget_units(WORLD_RESIDENT_CHUNK_LIMIT);
#[cfg(feature = "cd-stream-bench")]
const MAX_RUNTIME_RESIDENT_CHUNKS: usize = STREAMED_ROOM_SLOT_COUNT;
#[cfg(feature = "cd-stream-bench")]
const MAX_COLLISION_ROOMS: usize = STREAMED_ROOM_SLOT_COUNT;
#[cfg(not(feature = "cd-stream-bench"))]
const MAX_COLLISION_ROOMS: usize = MAX_ACTIVE_ROOMS;

#[cfg(feature = "cd-stream-bench")]
const fn clamp_streamed_room_slot_count(raw: usize) -> usize {
    if raw < 1 {
        1
    } else if raw > MAX_STREAMED_ROOM_SLOT_COUNT {
        MAX_STREAMED_ROOM_SLOT_COUNT
    } else {
        raw
    }
}

#[cfg(feature = "cd-stream-bench")]
const fn streamed_room_slot_count_for_budget_units(raw_units: usize) -> usize {
    let units = if raw_units < 1 { 1 } else { raw_units };
    let budget_bytes = if units > usize::MAX / STREAMED_ROOM_RESIDENT_BUDGET_UNIT_BYTES {
        usize::MAX
    } else {
        units * STREAMED_ROOM_RESIDENT_BUDGET_UNIT_BYTES
    };
    clamp_streamed_room_slot_count(budget_bytes / STREAMED_ROOM_SLOT_BYTES)
}

#[cfg(feature = "cd-stream-bench")]
const fn clamp_streamed_room_slot_bytes(raw: usize) -> usize {
    let clamped = if raw < MIN_STREAMED_ROOM_SLOT_BYTES {
        MIN_STREAMED_ROOM_SLOT_BYTES
    } else if raw > MAX_STREAMED_ROOM_SLOT_BYTES {
        MAX_STREAMED_ROOM_SLOT_BYTES
    } else {
        raw
    };
    (clamped + 3) & !3
}
const INVALID_ROOM_INDEX: RoomIndex = RoomIndex(u16::MAX);

/// Per-frame projected-vertex scratch for the model renderer.
/// Sized to the largest part vertex count we expect; instances
/// over this cap drop their over-budget triangles graceful.
const MODEL_VERTEX_CAP: usize = 1024;
/// Predecoded face records shared by runtime model assets.
const MAX_RUNTIME_MODEL_FACES: usize = 4096;
/// Predecoded part records shared by runtime model assets.
const MAX_RUNTIME_MODEL_PARTS: usize = 128;
/// Predecoded vertex records shared by runtime model assets.
const MAX_RUNTIME_MODEL_DECODED_VERTICES: usize = 1024;
/// Projected edge threshold used to subdivide close model triangles.
const MODEL_TEXTURE_SPLIT_MAX_EDGE: u16 = 0;
/// Q8 fixed-point identity for per-instance visual model scale.
const MODEL_VISUAL_SCALE_ONE_Q8: u16 = 256;
/// Joint-transform scratch -- all biped rigs we currently cook
/// fit comfortably in 32.
const JOINT_CAP: usize = 32;
/// Cap on placed model instances rendered per frame.
const MAX_MODEL_INSTANCES: usize = 16;
/// Cap on static boxed prop collision blockers per frame.
const MAX_BOX_PROP_BLOCKERS: usize = 32;
/// Fixed authored box-prop state budget. Props beyond this still render
/// as static props, but cannot be toggled broken in this no-heap runtime.
const MAX_BOX_PROP_STATE: usize = 128;
const BOX_PROP_BROKEN_WORDS: usize = (MAX_BOX_PROP_STATE + 31) / 32;
/// Active baked break bursts retained after a prop is marked broken.
const MAX_BOX_PROP_BREAK_EVENTS: usize = 16;
const BOX_PROP_BREAK_FRAMES: u8 = 24;
const BOX_PROP_BREAK_MOTION_FRAMES: u8 = 20;
const BOX_PROP_BREAK_SHARD_COUNT: usize = 8;
/// Gravity applied to an unsupported, falling box (room units per vblank,
/// per vblank). Tuned so a stacked box drops over a handful of frames.
const BOX_PROP_FALL_GRAVITY: i32 = 28;
/// Per-vblank fall-speed cap so a tall drop cannot tunnel past its
/// landing in one step (the landing check snaps any overshoot anyway).
const BOX_PROP_FALL_MAX_VEL: i32 = 384;
/// Slack for "rests on the floor / on the box below" support tests, in
/// room units. Boxes are ~900+ units tall, so this only absorbs rounding
/// and small authored gaps.
const BOX_PROP_SUPPORT_TOLERANCE: i32 = 64;
const BOX_PROP_BREAK_ATTACK_REACH: i32 = 768;
const BOX_PROP_BREAK_ATTACK_WIDTH: i32 = 320;
const BOX_PROP_FACE_NORMAL_SHIFT: u32 = 10;
/// Cap on attached weapon/equipment visuals rendered per frame.
const MAX_EQUIPMENT_DRAWS: usize = 8;
/// Runtime model cache capacity. The current playtest package only
/// needs one player model, but this keeps a little headroom for
/// lightweight NPC experiments without introducing heap allocation.
const MAX_RUNTIME_MODELS: usize = 8;
/// Runtime animation cache capacity. Demo-scale character sets can
/// easily carry player + several enemy clip banks; keep this aligned
/// with the residency table rather than the old single-character cap.
const MAX_RUNTIME_MODEL_CLIPS: usize = 128;
const MODEL_PROFILE_ENABLED: bool = option_env!("PSXO_PROFILE_MODELS").is_some();
const MODEL_BOUNDS_CULLING_ENABLED: bool =
    option_env!("PSXO_BENCH_DISABLE_MODEL_BOUNDS_CULL").is_none();
const PROP_PARTICLE_GTE_PROJECT_ENABLED: bool =
    option_env!("PSXO_GTE_PROP_PARTICLE_PROJECT").is_some();
const BOX_PROP_GTE_PROJECT_ENABLED: bool = true;
const BOX_PROP_PROFILE_ENABLED: bool = option_env!("PSXO_PROFILE_BOX_PROPS").is_some();

static mut OT: OrderingTable<OT_DEPTH> = OrderingTable::new();
static mut PRIMITIVE_PACKETS: PrimitivePacketScratch<MAX_TEXTURED_TRIS> =
    PrimitivePacketScratch::ZERO;
static mut WORLD_COMMANDS: [WorldTriCommand; MAX_TEXTURED_TRIS] =
    [WorldTriCommand::EMPTY; MAX_TEXTURED_TRIS];
static mut CACHED_ROOM_PROJECTED_VERTICES: [ProjectedVertex; MAX_CACHED_ROOM_VERTICES] =
    [ProjectedVertex::new(0, 0, 0); MAX_CACHED_ROOM_VERTICES];
static mut CACHED_ROOM_PROJECTED_INDICES: [u16; MAX_CACHED_ROOM_VERTICES] =
    [0; MAX_CACHED_ROOM_VERTICES];
static mut CACHED_ROOM_PROJECTED_READY: [bool; MAX_CACHED_ROOM_VERTICES] =
    [false; MAX_CACHED_ROOM_VERTICES];
static mut CACHED_ROOM_PROJECTED_DEPTHS: [i32; MAX_CACHED_ROOM_VERTICES] =
    [0; MAX_CACHED_ROOM_VERTICES];
#[cfg(feature = "world-grid-visible")]
static mut CACHED_ROOM_ACCEPTED_CELL_INDICES: [u16; MAX_PRECOMPUTED_VISIBLE_CELLS] =
    [0; MAX_PRECOMPUTED_VISIBLE_CELLS];
#[cfg(feature = "world-grid-visible")]
static mut CACHED_ROOM_ACCEPTED_CELL_DEPTHS: [i32; MAX_PRECOMPUTED_VISIBLE_CELLS] =
    [0; MAX_PRECOMPUTED_VISIBLE_CELLS];
#[cfg(feature = "cd-stream-bench")]
static mut STREAMED_ROOM_WORDS: [[u32; STREAMED_ROOM_SLOT_WORDS]; STREAMED_ROOM_SLOT_COUNT] =
    [[0; STREAMED_ROOM_SLOT_WORDS]; STREAMED_ROOM_SLOT_COUNT];
#[cfg(feature = "cd-stream-bench")]
static mut ROOM_STREAM_SCHEDULER: RoomStreamScheduler<STREAMED_ROOM_SLOT_COUNT> =
    RoomStreamScheduler::new();
static mut MODEL_VERTICES: [ProjectedVertex; MODEL_VERTEX_CAP] =
    [ProjectedVertex::new(0, 0, 0); MODEL_VERTEX_CAP];
static mut JOINT_VIEW_TRANSFORMS: [JointViewTransform; JOINT_CAP] =
    [JointViewTransform::ZERO; JOINT_CAP];

/// Animation state machine for the player: idle with no movement,
/// walking for normal movement, running while Circle is held.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum PlayerAnim {
    Idle,
    Walk,
    Run,
    Roll,
    Backstep,
    LightAttack,
    HeavyAttack,
}

impl PlayerAnim {
    const fn action(self) -> CharacterAnimationAction {
        match self {
            Self::Idle => CharacterAnimationAction::Idle,
            Self::Walk => CharacterAnimationAction::Walk,
            Self::Run => CharacterAnimationAction::Run,
            Self::Roll => CharacterAnimationAction::Roll,
            Self::Backstep => CharacterAnimationAction::Backstep,
            Self::LightAttack => CharacterAnimationAction::LightAttack,
            Self::HeavyAttack => CharacterAnimationAction::HeavyAttack,
        }
    }

    const fn is_motor_fixed_action(self) -> bool {
        matches!(self, Self::Roll | Self::Backstep)
    }
}

const fn player_anim_is_attack(anim: PlayerAnim) -> bool {
    matches!(anim, PlayerAnim::LightAttack | PlayerAnim::HeavyAttack)
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct RuntimeCheckpoint {
    room: RoomIndex,
    position: RoomPoint,
    yaw: Angle,
    checkpoint_id: &'static str,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct RuntimeMessageOverlay {
    title: &'static str,
    body: &'static str,
}

#[derive(Copy, Clone, Debug, Default)]
struct EvadeRunIntent {
    sprint: bool,
    evade: bool,
}

/// Runtime view of the cooked LevelCharacterRecord -- the same
/// fields, decoded into runtime-friendly types. Resolved once
/// at init time so per-frame movement / animation / camera code
/// doesn't keep re-resolving the manifest.
#[derive(Copy, Clone, Debug)]
struct RuntimeCharacter {
    /// Index into `MODELS`.
    model: ModelIndex,
    action_clips: [OptionalModelClipIndex; CHARACTER_ANIMATION_ACTION_COUNT],
    action_flags: [u8; CHARACTER_ANIMATION_ACTION_COUNT],
    visual_offset: [i16; 3],
    visual_yaw: i16,
    visual_scale_q8: u16,
    weight_q8: u16,
    /// Coarse collision cylinder radius. Engine units.
    radius: i32,
    /// Coarse collision cylinder height. Engine units.
    height: i32,
    walk_speed: i32,
    run_speed: i32,
    /// Yaw rate translated from degrees/second to PSX angle
    /// units / 60 Hz frame at init time.
    yaw_step: Angle,
    stamina_max_q12: i32,
    sprint_min_q12: i32,
    sprint_drain_q12: i32,
    stamina_recover_q12: i32,
    roll_cost_q12: i32,
    roll_speed: i32,
    roll_active_frames: u8,
    roll_recovery_frames: u8,
    roll_invulnerable_frames: u8,
    backstep_cost_q12: i32,
    backstep_speed: i32,
    backstep_active_frames: u8,
    backstep_recovery_frames: u8,
    backstep_invulnerable_frames: u8,
}

impl RuntimeCharacter {
    /// Resolve the cooked record into the runtime's preferred
    /// units. Yaw is converted from degrees/second to per-frame
    /// quanta (`4096 quanta = full turn`, runtime targets 60 Hz)
    /// up-front so the per-frame update path is just a wrapping
    /// add.
    fn from_record(c: &LevelCharacterRecord) -> Self {
        // 4096 q12 / 360 deg = 11 q12 per deg, divided by
        // 60 Hz target ≈ 0.19 q12 per deg/frame. We approximate
        // as `(deg * 4096) / (360 * 60)` which is exact for the
        // 180 deg/s default (= 34 quanta/frame).
        let yaw_step_q12 = ((c.turn_speed_degrees_per_second as u32 * 4096) / (360 * 60)) as u16;
        Self {
            model: c.model,
            action_clips: c.action_clips,
            action_flags: c.action_flags,
            visual_offset: c.visual_offset,
            visual_yaw: c.visual_yaw,
            visual_scale_q8: c.visual_scale_q8,
            weight_q8: c.weight_q8,
            radius: c.radius as i32,
            height: c.height as i32,
            walk_speed: scaled_player_speed(c.walk_speed),
            run_speed: scaled_player_speed(c.run_speed),
            yaw_step: Angle::from_q12(yaw_step_q12),
            stamina_max_q12: c.stamina_max_q12,
            sprint_min_q12: c.sprint_min_q12,
            sprint_drain_q12: c.sprint_drain_q12,
            stamina_recover_q12: c.stamina_recover_q12,
            roll_cost_q12: c.roll_cost_q12,
            roll_speed: c.roll_speed,
            roll_active_frames: c.roll_active_frames,
            roll_recovery_frames: c.roll_recovery_frames,
            roll_invulnerable_frames: c.roll_invulnerable_frames,
            backstep_cost_q12: c.backstep_cost_q12,
            backstep_speed: c.backstep_speed,
            backstep_active_frames: c.backstep_active_frames,
            backstep_recovery_frames: c.backstep_recovery_frames,
            backstep_invulnerable_frames: c.backstep_invulnerable_frames,
        }
    }

    fn action_clip(&self, action: CharacterAnimationAction) -> OptionalModelClipIndex {
        self.action_clips
            .get(action.to_index())
            .copied()
            .unwrap_or(OptionalModelClipIndex::NONE)
    }

    fn action_flags(&self, action: CharacterAnimationAction) -> u8 {
        self.action_flags
            .get(action.to_index())
            .copied()
            .unwrap_or(0)
    }

    fn action_loops(&self, action: CharacterAnimationAction) -> bool {
        self.action_flags(action) & character_action_flags::LOOPING != 0
    }

    fn action_in_place_override(&self, action: CharacterAnimationAction) -> Option<bool> {
        let flags = self.action_flags(action);
        if flags & character_action_flags::IN_PLACE_OVERRIDE == 0 {
            None
        } else {
            Some(flags & character_action_flags::IN_PLACE != 0)
        }
    }

    /// Pick the clip index for an animation state, with
    /// cheap deterministic fallbacks for unassigned optional actions.
    fn clip_for(&self, anim: PlayerAnim) -> ModelClipIndex {
        let idle = self
            .action_clip(CharacterAnimationAction::Idle)
            .unwrap_or(ModelClipIndex::ZERO);
        let walk = self
            .action_clip(CharacterAnimationAction::Walk)
            .unwrap_or(idle);
        match anim.action() {
            CharacterAnimationAction::Idle => idle,
            CharacterAnimationAction::Walk => walk,
            CharacterAnimationAction::Run => self
                .action_clip(CharacterAnimationAction::Run)
                .unwrap_or(walk),
            CharacterAnimationAction::Roll => {
                self.action_clip(CharacterAnimationAction::Roll).unwrap_or(
                    self.action_clip(CharacterAnimationAction::Run)
                        .unwrap_or(walk),
                )
            }
            CharacterAnimationAction::Backstep => self
                .action_clip(CharacterAnimationAction::Backstep)
                .unwrap_or(walk),
            CharacterAnimationAction::LightAttack => self
                .action_clip(CharacterAnimationAction::LightAttack)
                .to_option()
                .or_else(|| {
                    self.action_clip(CharacterAnimationAction::ComboAttack)
                        .to_option()
                })
                .unwrap_or(idle),
            CharacterAnimationAction::HeavyAttack => self
                .action_clip(CharacterAnimationAction::HeavyAttack)
                .to_option()
                .or_else(|| {
                    self.action_clip(CharacterAnimationAction::LightAttack)
                        .to_option()
                })
                .unwrap_or(idle),
            CharacterAnimationAction::ComboAttack => self
                .action_clip(CharacterAnimationAction::ComboAttack)
                .to_option()
                .or_else(|| {
                    self.action_clip(CharacterAnimationAction::LightAttack)
                        .to_option()
                })
                .unwrap_or(idle),
            CharacterAnimationAction::Block => self
                .action_clip(CharacterAnimationAction::Block)
                .unwrap_or(idle),
            CharacterAnimationAction::HitReact => self
                .action_clip(CharacterAnimationAction::HitReact)
                .unwrap_or(idle),
            CharacterAnimationAction::Death => self
                .action_clip(CharacterAnimationAction::Death)
                .unwrap_or(idle),
            CharacterAnimationAction::Turn => idle,
        }
    }

    fn motor_config(&self) -> CharacterMotorConfig {
        let mut config = CharacterMotorConfig::character_with_body(
            self.radius,
            self.height,
            self.walk_speed,
            self.run_speed,
            self.yaw_step,
        );
        config.weight_q8 = self.weight_q8;
        config.stamina_max_q12 = self.stamina_max_q12;
        config.sprint_min_q12 = self.sprint_min_q12;
        config.sprint_drain_q12 = self.sprint_drain_q12;
        config.stamina_recover_q12 = self.stamina_recover_q12;
        config.roll_cost_q12 = self.roll_cost_q12;
        config.roll_speed = self.roll_speed;
        config.roll_active_frames = self.roll_active_frames;
        config.roll_recovery_frames = self.roll_recovery_frames;
        config.roll_invulnerable_frames = self.roll_invulnerable_frames;
        config.backstep_cost_q12 = self.backstep_cost_q12;
        config.backstep_speed = self.backstep_speed;
        config.backstep_active_frames = self.backstep_active_frames;
        config.backstep_recovery_frames = self.backstep_recovery_frames;
        config.backstep_invulnerable_frames = self.backstep_invulnerable_frames;
        config
    }
}

fn scaled_player_speed(speed: i32) -> i32 {
    let scaled = speed.saturating_mul(PLAYER_SPEED_SCALE_NUM) / PLAYER_SPEED_SCALE_DEN;
    if speed > 0 {
        scaled.max(1)
    } else {
        scaled
    }
}

fn square_i32_saturating(value: i32) -> i32 {
    let abs = value.saturating_abs();
    if abs > 46_340 {
        i32::MAX
    } else {
        abs * abs
    }
}

fn clamp_i16(value: i32) -> i16 {
    value.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

fn world_camera_from_position_focus(
    projection: WorldProjection,
    position: RoomPoint,
    focus: RoomPoint,
) -> WorldCamera {
    let dx = position.x.saturating_sub(focus.x);
    let dz = position.z.saturating_sub(focus.z);
    let radius =
        isqrt_i32(square_i32_saturating(dx).saturating_add(square_i32_saturating(dz))).max(1);
    let target_dy = focus.y.saturating_sub(position.y);
    let pitch_len =
        isqrt_i32(square_i32_saturating(radius).saturating_add(square_i32_saturating(target_dy)))
            .max(1);
    WorldCamera::from_basis(
        projection,
        position,
        Q12::from_ratio(dx, radius),
        Q12::from_ratio(dz, radius),
        Q12::from_ratio(target_dy, pitch_len),
        Q12::from_ratio(radius, pitch_len),
    )
}

fn yaw_q12_from_basis(sin_yaw: i32, cos_yaw: i32) -> u16 {
    if sin_yaw == 0 && cos_yaw == 0 {
        return 0;
    }
    let ax = abs_i32_saturating(sin_yaw);
    let az = abs_i32_saturating(cos_yaw);
    let base = if ax <= az {
        ax.saturating_mul(512) / az.max(1)
    } else {
        1024 - (az.saturating_mul(512) / ax.max(1))
    };
    let angle = if cos_yaw >= 0 {
        if sin_yaw >= 0 {
            base
        } else {
            4096 - base
        }
    } else if sin_yaw >= 0 {
        2048 - base
    } else {
        2048 + base
    };
    (angle & 0x0fff) as u16
}

fn abs_i32_saturating(value: i32) -> i32 {
    if value == i32::MIN {
        i32::MAX
    } else {
        value.abs()
    }
}

fn isqrt_i32(n: i32) -> i32 {
    if n <= 0 {
        return 0;
    }
    let mut bit = 1 << 30;
    let mut rest = n;
    let mut root = 0;
    while bit > rest {
        bit >>= 2;
    }
    while bit != 0 {
        if rest >= root + bit {
            rest -= root + bit;
            root = (root >> 1) + bit;
        } else {
            root >>= 1;
        }
        bit >>= 2;
    }
    root
}

struct Playtest {
    /// Active room. `None` until `init` runs and only `Some`
    /// when the manifest had at least one room and its bytes
    /// parsed.
    room: Option<RuntimeRoom<'static>>,
    /// Active collision room. Streamed builds use a compact
    /// collision-only payload here instead of a full `.psxw`.
    current_collision_room: Option<RuntimeCollisionRoom<'static>>,
    /// Ambient RGB for the room containing the player.
    current_ambient_rgb: [u8; 3],
    /// Cache-budgeted draw chunks, all expressed relative to
    /// `room_index`.
    active_rooms: [Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    /// Incremental active-room cache rebuild in progress. The old
    /// `active_rooms` remain drawable until the staged replacement is ready.
    active_room_job: ActiveRoomWindowJob,
    /// Portal traversal result for the current player/camera room.
    portal_visibility: RuntimePortalVisibility,
    /// Runtime room used as the root for the latest portal traversal.
    portal_visibility_root: RoomIndex,
    /// Absolute level-space render camera used by the latest portal traversal.
    portal_visibility_camera_global: RoomPoint,
    /// Global chunk bounds retained for portal diagnostics and streaming.
    portal_room_bounds: [PortalRoomBounds; MAX_PORTAL_ROOM_BOUNDS],
    /// Cached `portal_room_bounds` length. The bounds are a pure function of the
    /// static cooked geometry (ROOM_VISIBILITY / VISIBILITY_CELLS / ROOMS), so
    /// they are computed once and reused; recomputing them per portal-visibility
    /// refresh was ~74% of the portal-visibility cost.
    portal_room_bounds_count: Option<usize>,
    portal_visible_missing_resident: u16,
    portal_visible_missing_mask: RuntimeDebugMask,
    portal_visible_build_failed: u16,
    portal_visible_build_failed_mask: RuntimeDebugMask,
    portal_stream_priority_current: u16,
    portal_stream_priority_visible: u16,
    portal_stream_priority_frontier: u16,
    #[cfg(all(
        feature = "world-grid-visible",
        not(feature = "vis-full-active-chunks")
    ))]
    visible_cell_caches: [ActiveVisibleCellCache; MAX_ACTIVE_ROOMS],
    #[cfg(all(
        feature = "world-grid-visible",
        not(feature = "vis-full-active-chunks")
    ))]
    visible_cell_cache_cells: [GridVisibleCell; MAX_ACTIVE_VISIBLE_CELLS],
    #[cfg(all(
        feature = "world-grid-visible",
        not(feature = "vis-full-active-chunks")
    ))]
    visible_cell_cache_cursor: usize,
    active_room_candidates: u16,
    active_room_cache_skips: u16,
    active_room_anchor: RoomPoint,
    active_room_view_anchor: RoomPoint,
    active_room_view_sin_key: i16,
    active_room_view_cos_key: i16,
    active_room_view_pitch_sin_key: i16,
    active_room_view_pitch_cos_key: i16,
    /// Index in ROOMS the player is currently in. Used to scope
    /// model-instance + light queries.
    room_index: RoomIndex,
    /// The resident desired-set the last residency pass actually requested (the
    /// camera ring + visible). The boot gate waits for THIS to be resident, not
    /// the legacy player `stream_ring`, so loading completes against the set
    /// streaming actually loads.
    resident_desired: [RoomIndex; STREAMED_ROOM_SLOT_COUNT],
    resident_desired_count: usize,
    /// Active room's material table, ordered by `local_slot`.
    /// Indexed directly by the slot value the cooked `.psxw`
    /// stores per face.
    materials: [WorldRenderMaterial; MAX_ROOM_MATERIALS],
    /// `materials[..material_count]` is the in-use slice; rest
    /// is `None`.
    material_count: usize,
    /// Player locomotion state: position, yaw, stamina, and evade actions.
    motor: CharacterMotorState,
    /// Resolved Character driving the player -- `None` when no
    /// `PLAYER_CONTROLLER` was authored. Falls back to the
    /// pre-character debug controls in that case.
    character: Option<RuntimeCharacter>,
    /// Current animation state. Source of truth for which clip
    /// `draw_player` plays each frame.
    anim_state: PlayerAnim,
    /// Tick the current animation started at -- used to phase
    /// the loop relative to clip switches so transitions don't
    /// pop into the middle of the new clip.
    anim_start_tick: SimTick,
    /// Non-looping gameplay animation lock. While active,
    /// locomotion input is ignored and the current action clip
    /// plays from start to finish.
    anim_lock_until_tick: SimTick,
    /// Persistent runtime state for authored breakable box props.
    box_prop_broken: [u32; BOX_PROP_BROKEN_WORDS],
    /// Static derived box-prop data used by render, break tests, and collision.
    box_prop_runtime: [BoxPropRuntime; MAX_BOX_PROP_STATE],
    /// Dynamic fall state per box, parallel to `box_prop_broken`. A box
    /// starts falling when its support is removed and breaks on landing.
    box_prop_fall: [BoxPropFallState; MAX_BOX_PROP_STATE],
    /// Short-lived baked face-burst events for newly broken box props.
    box_prop_break_events: [BoxPropBreakEvent; MAX_BOX_PROP_BREAK_EVENTS],
    /// Circle is shared by tap-evade and hold-sprint. We delay
    /// either decision for a few simulation ticks: release before
    /// the threshold becomes evade; holding past it becomes sprint.
    evade_run_hold_ticks: u8,
    evade_run_hold_consumed: bool,
    /// `true` toggles a free-orbit camera around the spawn for
    /// debug inspection. Default = follow.
    free_orbit: bool,
    orbit_yaw: Angle,
    orbit_radius: i32,
    /// Runtime third-person camera rig. Updated at simulation cadence
    /// so control remains responsive when visuals are paced lower.
    camera: ThirdPersonCameraState,
    /// Last visual camera produced by the simulation update.
    render_camera: WorldCamera,
    /// Last movement result; stationary frames can use a broader cached
    /// visibility candidate set without rebuilding it for camera-only turns.
    player_moved_last_tick: bool,
    /// True when the latest input frame is manually rotating the camera.
    camera_turning_last_tick: bool,
    /// Index into `MODEL_INSTANCES` for the current lock-on target.
    /// Player-controlled characters are consumed by the player path,
    /// so remaining placed model instances are targetable actors for
    /// this first gameplay pass.
    lock_target: Option<usize>,
    lock_switch_stick_held: bool,
    /// Automatic camera-only target. Suppressed after strong
    /// manual camera input until the player leaves target range.
    soft_lock_target: Option<usize>,
    soft_lock_suppressed: bool,
    /// Nearest authored interactable currently in range.
    active_interactable: Option<usize>,
    /// Last synchronized in-memory checkpoint. Future death/respawn
    /// code restores from this; it is intentionally not persisted to
    /// memory card yet.
    checkpoint: Option<RuntimeCheckpoint>,
    /// Simple modal message overlay opened by an interactable.
    message_overlay: Option<RuntimeMessageOverlay>,
    /// Spawn position retained for orbit-mode targeting.
    spawn: RoomPoint,
    /// Runtime UI font atlases, compacted from the cooked manifest's used fonts.
    ui_fonts: [Option<FontAtlas>; MAX_RUNTIME_UI_FONTS],
    /// Parsed models/materials, built once at init.
    models: [Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    /// Predecoded model face records, shared by `models`.
    model_faces: [TexturedModelRenderFace; MAX_RUNTIME_MODEL_FACES],
    model_face_count: usize,
    /// Predecoded model part records, shared by `models`.
    model_parts: [ModelPart; MAX_RUNTIME_MODEL_PARTS],
    model_part_count: usize,
    /// Predecoded model vertex records, shared by `models`.
    model_vertices: [ModelVertex; MAX_RUNTIME_MODEL_DECODED_VERTICES],
    model_vertex_count: usize,
    /// Parsed animations, indexed like `MODEL_CLIPS`.
    clips: [Option<Animation<'static>>; MAX_RUNTIME_MODEL_CLIPS],
    /// VRAM-bound subtract-blended circular floor shadow.
    shadow_material: Option<TextureMaterial>,
    /// VRAM-bound 16x16 white circular sprite used by particle emitters.
    particle_material: Option<TextureMaterial>,
    /// Immediate-mode cylinder overlay for tuning actor blockers.
    show_collision_debug: bool,
    /// Cooperative background policy for room-window and VRAM upload work.
    streaming_jobs: RuntimeStreamingJobs,
    /// Host-visible render breadcrumbs emitted for a few frames after
    /// crossing into another room.
    post_cross_debug_frames: u8,
    /// Slow down verbose portal diagnostics so the host terminal cannot
    /// stall the playtest when a portal is rejected every camera tick.
    portal_debug_log_cooldown: u8,
}

impl Playtest {
    const fn new() -> Self {
        Self {
            room: None,
            current_collision_room: None,
            current_ambient_rgb: [0x80, 0x80, 0x80],
            active_rooms: [const { None }; MAX_ACTIVE_ROOMS],
            active_room_job: ActiveRoomWindowJob::EMPTY,
            portal_visibility: RuntimePortalVisibility::EMPTY,
            portal_visibility_root: RoomIndex::ZERO,
            portal_visibility_camera_global: RoomPoint::ZERO,
            portal_room_bounds: [PortalRoomBounds::EMPTY; MAX_PORTAL_ROOM_BOUNDS],
            portal_room_bounds_count: None,
            portal_visible_missing_resident: 0,
            portal_visible_missing_mask: RuntimeDebugMask::EMPTY,
            portal_visible_build_failed: 0,
            portal_visible_build_failed_mask: RuntimeDebugMask::EMPTY,
            portal_stream_priority_current: 0,
            portal_stream_priority_visible: 0,
            portal_stream_priority_frontier: 0,
            #[cfg(all(
                feature = "world-grid-visible",
                not(feature = "vis-full-active-chunks")
            ))]
            visible_cell_caches: [const { ActiveVisibleCellCache::EMPTY }; MAX_ACTIVE_ROOMS],
            #[cfg(all(
                feature = "world-grid-visible",
                not(feature = "vis-full-active-chunks")
            ))]
            visible_cell_cache_cells: [GridVisibleCell::EMPTY; MAX_ACTIVE_VISIBLE_CELLS],
            #[cfg(all(
                feature = "world-grid-visible",
                not(feature = "vis-full-active-chunks")
            ))]
            visible_cell_cache_cursor: 0,
            active_room_candidates: 0,
            active_room_cache_skips: 0,
            active_room_anchor: RoomPoint::ZERO,
            active_room_view_anchor: RoomPoint::ZERO,
            active_room_view_sin_key: 0,
            active_room_view_cos_key: 0,
            active_room_view_pitch_sin_key: 0,
            active_room_view_pitch_cos_key: 0,
            room_index: RoomIndex::ZERO,
            resident_desired: [INVALID_ROOM_INDEX; STREAMED_ROOM_SLOT_COUNT],
            resident_desired_count: 0,
            materials: [room_material_fallback(); MAX_ROOM_MATERIALS],
            material_count: 0,
            motor: CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO),
            character: None,
            anim_state: PlayerAnim::Idle,
            anim_start_tick: SimTick::ZERO,
            anim_lock_until_tick: SimTick::ZERO,
            box_prop_broken: [0; BOX_PROP_BROKEN_WORDS],
            box_prop_runtime: [BoxPropRuntime::EMPTY; MAX_BOX_PROP_STATE],
            box_prop_fall: [BoxPropFallState::EMPTY; MAX_BOX_PROP_STATE],
            box_prop_break_events: [BoxPropBreakEvent::EMPTY; MAX_BOX_PROP_BREAK_EVENTS],
            evade_run_hold_ticks: 0,
            evade_run_hold_consumed: false,
            free_orbit: false,
            orbit_yaw: CAMERA_START_YAW,
            orbit_radius: CAMERA_START_RADIUS,
            camera: ThirdPersonCameraState::new(CAMERA_START_YAW),
            render_camera: WorldCamera::from_basis(
                PROJECTION,
                WorldVertex::ZERO,
                Q12::ZERO,
                Q12::ONE,
                Q12::ZERO,
                Q12::ONE,
            ),
            player_moved_last_tick: false,
            camera_turning_last_tick: false,
            lock_target: None,
            lock_switch_stick_held: false,
            soft_lock_target: None,
            soft_lock_suppressed: false,
            active_interactable: None,
            checkpoint: None,
            message_overlay: None,
            spawn: RoomPoint::ZERO,
            ui_fonts: [const { None }; MAX_RUNTIME_UI_FONTS],
            models: [const { None }; MAX_RUNTIME_MODELS],
            model_faces: [TexturedModelRenderFace::ZERO; MAX_RUNTIME_MODEL_FACES],
            model_face_count: 0,
            model_parts: [ModelPart::ZERO; MAX_RUNTIME_MODEL_PARTS],
            model_part_count: 0,
            model_vertices: [ModelVertex::ZERO; MAX_RUNTIME_MODEL_DECODED_VERTICES],
            model_vertex_count: 0,
            clips: [const { None }; MAX_RUNTIME_MODEL_CLIPS],
            shadow_material: None,
            particle_material: None,
            show_collision_debug: false,
            streaming_jobs: RuntimeStreamingJobs::new(),
            post_cross_debug_frames: 0,
            portal_debug_log_cooldown: 0,
        }
    }

    fn step_streaming_jobs(&mut self, ctx: &mut Ctx) {
        let background_tick = self.streaming_jobs.background_tick(ctx);
        #[cfg(feature = "cd-stream-bench")]
        if background_tick {
            // Residency owner: the single per-frame declaration of which rooms
            // must be resident (pin + load), so the build paths no longer have
            // to request residency themselves.
            telemetry::stage_begin(telemetry::stage::SIM_RESIDENCY);
            self.update_room_residency();
            telemetry::stage_end(telemetry::stage::SIM_RESIDENCY);
        }
        #[cfg(feature = "cd-stream-bench")]
        let stream_progress = if background_tick {
            telemetry::stage_begin(telemetry::stage::SIM_PUMP);
            let progress = self.pump_room_stream(RUNTIME_SCHEDULE.stream_pump_sectors_per_tick);
            telemetry::stage_end(telemetry::stage::SIM_PUMP);
            progress
        } else {
            false
        };
        if background_tick {
            #[cfg(feature = "cd-stream-bench")]
            if stream_progress {
                if self.active_room_job.active {
                    self.active_room_job.update_streaming = true;
                } else {
                    self.begin_active_room_window_job(true);
                }
            }
            if self.streaming_jobs.step_vram_uploads() {
                self.refresh_active_room_materials();
            }
            self.step_active_room_window_job();
        }
    }

    fn initial_world_ready(&mut self) -> bool {
        #[cfg(not(feature = "cd-stream-bench"))]
        {
            true
        }
        #[cfg(feature = "cd-stream-bench")]
        {
            if !self.chunked_level() {
                return true;
            }
            let Some(record) = ROOMS.get(self.room_index.to_usize()) else {
                return true;
            };
            let textures_ready = self.initial_stream_ring_textures_ready();
            self.current_collision_room.is_some()
                && !self.active_room_job.active
                && self.portal_visible_rooms_are_active(record)
                && self.initial_stream_ring_resident()
                && textures_ready
                && self.streaming_jobs.vram_uploads_idle()
                && !streamed_room_stream_active()
        }
    }

    #[cfg(feature = "cd-stream-bench")]
    fn initial_stream_ring_resident(&self) -> bool {
        let count = self.resident_desired_count.min(STREAMED_ROOM_SLOT_COUNT);
        if count == 0 {
            return false;
        }
        let mut i = 0usize;
        while i < count {
            let room = self.resident_desired[i];
            if room == INVALID_ROOM_INDEX || !streamed_room_is_resident(room) {
                return false;
            }
            i += 1;
        }

        let Some(record) = ROOMS.get(self.room_index.to_usize()) else {
            return true;
        };
        let visible_limit = self.portal_visible_room_limit(record);
        let mut visible = 0usize;
        while visible < visible_limit {
            let room = self.portal_visibility.rooms[visible].room;
            if room != INVALID_ROOM_INDEX && !streamed_room_is_resident(room) {
                return false;
            }
            visible += 1;
        }
        true
    }

    #[cfg(feature = "cd-stream-bench")]
    fn initial_stream_ring_textures_ready(&mut self) -> bool {
        let mut ready = true;
        let count = self.resident_desired_count.min(STREAMED_ROOM_SLOT_COUNT);
        let mut i = 0usize;
        while i < count {
            let room = self.resident_desired[i];
            if room != INVALID_ROOM_INDEX {
                if let Some(record) = ROOMS.get(room.to_usize()) {
                    ready &= room_material_textures_ready(record);
                    ready &= room_backdrop_textures_ready(record);
                }
                ready &= room_prop_textures_ready(room);
            }
            i += 1;
        }

        let Some(record) = ROOMS.get(self.room_index.to_usize()) else {
            return ready;
        };
        let visible_limit = self.portal_visible_room_limit(record);
        let mut visible = 0usize;
        while visible < visible_limit {
            let room = self.portal_visibility.rooms[visible].room;
            if room != INVALID_ROOM_INDEX && !room_requested(room, &self.resident_desired, count) {
                if let Some(record) = ROOMS.get(room.to_usize()) {
                    ready &= room_material_textures_ready(record);
                    ready &= room_backdrop_textures_ready(record);
                }
                ready &= room_prop_textures_ready(room);
            }
            visible += 1;
        }

        ready
    }

    fn update_evade_run_button(&mut self, ctx: &Ctx, delta_vblanks: u16) -> EvadeRunIntent {
        if ctx.just_pressed(EVADE_RUN_BUTTON) {
            self.evade_run_hold_ticks = 0;
            self.evade_run_hold_consumed = false;
        }

        if ctx.is_held(EVADE_RUN_BUTTON) {
            self.evade_run_hold_ticks = self
                .evade_run_hold_ticks
                .saturating_add(delta_vblanks.min(u8::MAX as u16) as u8);
            if self.evade_run_hold_ticks >= EVADE_RUN_HOLD_VBLANKS {
                self.evade_run_hold_consumed = true;
                return EvadeRunIntent {
                    sprint: true,
                    evade: false,
                };
            }
            return EvadeRunIntent {
                sprint: false,
                evade: false,
            };
        }

        let evade = ctx.just_released(EVADE_RUN_BUTTON) && !self.evade_run_hold_consumed;
        self.evade_run_hold_ticks = 0;
        self.evade_run_hold_consumed = false;
        EvadeRunIntent {
            sprint: false,
            evade,
        }
    }
}

impl Scene for Playtest {
    /// Lend the uploaded HUD font to the flow driver so front-end UI
    /// scenes (the cooked Main Menu) draw their labels and buttons with
    /// the same glyphs the in-game HUD uses.
    fn ui_font(&self) -> Option<&FontAtlas> {
        self.ui_fonts[0].as_ref()
    }

    fn ui_font_at(&self, index: u8) -> Option<&FontAtlas> {
        self.ui_fonts
            .get(index as usize)
            .and_then(|font| font.as_ref())
    }

    fn ui_texture(&self, asset_id: AssetId) -> Option<UiTextureSlot> {
        let asset = find_asset_of_kind(ASSETS, asset_id, AssetKind::Texture)?;
        let slot = ensure_ui_texture_uploaded(asset.id, asset.bytes)?;
        Some(UiTextureSlot {
            clut_word: slot.clut_word,
            tpage_word: slot.tpage_word,
            texture_window: slot.texture_window,
            texture_width: slot.texture_width,
            texture_height: slot.texture_height,
        })
    }

    /// Every flow state shares one resource set: the UI font atlas is used by
    /// the menus and the gameplay HUD, so it is acquired once on the first
    /// state entered and never torn down (no per-transition re-upload).
    fn state_resource_key(&self, _state: SceneStateRef) -> u32 {
        UI_FONT_RESOURCE_KEY
    }

    /// Acquire the shared resource set. On first entry: reserve the static VRAM
    /// regions, then pack every UI font into one combined atlas and upload it
    /// in a single `GP0(A0h)` transfer. Uploading the fonts one-at-a-time
    /// desyncs the GPU command stream and freezes the world render, so the
    /// consolidated upload is the fix; routing it through the allocator keeps
    /// the font VRAM tracked.
    fn on_enter_state(&mut self, _state: SceneStateRef, _ctx: &mut Ctx) {
        acquire_shared_ui_fonts(&mut self.ui_fonts);
    }

    /// Release the shared resource set. Dormant in this project's flow (the
    /// font set never leaves the active set) but correct: free the VRAM and
    /// clear the atlases so a re-entry re-acquires cleanly.
    fn on_exit_state(&mut self, _state: SceneStateRef, _ctx: &mut Ctx) {
        release_shared_ui_fonts(&mut self.ui_fonts);
    }

    /// Apply front-end settings chosen before Play. Screen-position options
    /// shift the whole rendered scene through the display window.
    fn apply_options(&mut self, options: &[psx_level::LevelOptionDef], values: &[i32]) {
        for (option, value) in options.iter().zip(values) {
            if option.id == SCREEN_OFFSET_X_OPTION_ID {
                let offset_px = (*value).clamp(-128, 127) as i16;
                psx_gpu::set_screen_h_offset(offset_px, psx_gpu::Resolution::R320X240);
            } else if option.id == SCREEN_OFFSET_Y_OPTION_ID {
                let offset_px = (*value).clamp(-128, 127) as i16;
                psx_gpu::set_screen_v_offset(
                    offset_px,
                    psx_gpu::VideoMode::Ntsc,
                    psx_gpu::Resolution::R320X240,
                );
            }
        }
    }

    fn init(&mut self, _ctx: &mut Ctx) {
        self.shadow_material = upload_shadow_texture();
        self.particle_material = upload_particle_texture();

        // Empty manifest? Boot to a clear-coloured screen.
        if ROOMS.is_empty() {
            return;
        };

        // Player init: prefer PLAYER_CONTROLLER (cook output)
        // for spawn + character; fall back to the bare
        // PLAYER_SPAWN for placeholder manifests. The spawn room
        // may be a manual portal room rather than room zero.
        let (spawn, character) = match PLAYER_CONTROLLER {
            Some(pc) => {
                let character = CHARACTERS
                    .get(pc.character.to_usize())
                    .map(RuntimeCharacter::from_record);
                (pc.spawn, character)
            }
            None => (PLAYER_SPAWN, None),
        };
        if ROOMS.get(spawn.room.to_usize()).is_none() {
            return;
        };
        self.load_runtime_models();
        self.rebuild_box_prop_runtime();
        self.spawn = RoomPoint::new(spawn.x, spawn.y, spawn.z);
        self.character = character;
        self.motor
            .snap_to(self.spawn, Angle::from_q12(spawn.yaw as u16));
        self.room_index = spawn.room;
        self.anim_state = PlayerAnim::Idle;
        self.anim_start_tick = SimTick::ZERO;
        self.anim_lock_until_tick = SimTick::ZERO;
        self.active_interactable = None;
        self.checkpoint = None;
        self.message_overlay = None;
        self.box_prop_broken = [0; BOX_PROP_BROKEN_WORDS];
        self.box_prop_fall = [BoxPropFallState::EMPTY; MAX_BOX_PROP_STATE];
        self.box_prop_break_events = [BoxPropBreakEvent::EMPTY; MAX_BOX_PROP_BREAK_EVENTS];
        self.camera.snap_to_player_with_yaw(
            self.camera_target(None, false),
            self.camera_config(),
            CAMERA_START_YAW,
        );
        self.render_camera = world_camera_from_position_focus(
            PROJECTION,
            self.camera.position(),
            self.camera.focus(),
        );
        #[cfg(feature = "cd-stream-bench")]
        self.bootstrap_streamed_room_window();
        #[cfg(not(feature = "cd-stream-bench"))]
        self.load_active_room_window();
        #[cfg(feature = "cd-stream-benchmark")]
        cd_stream::run_benchmark();
    }

    fn loading_update(&mut self, ctx: &mut Ctx) -> bool {
        self.step_streaming_jobs(ctx);
        self.initial_world_ready()
    }

    fn update(&mut self, ctx: &mut Ctx) {
        self.portal_debug_log_cooldown = self.portal_debug_log_cooldown.saturating_sub(1);
        self.step_streaming_jobs(ctx);

        if ctx.just_pressed(button::R3) {
            self.lock_target = match self.lock_target {
                Some(_) => None,
                None => self.find_best_lock_target(LOCK_RANGE),
            };
            self.lock_switch_stick_held = false;
            self.soft_lock_target = None;
        }
        if ctx.just_pressed(COLLISION_DEBUG_BUTTON) {
            self.show_collision_debug = !self.show_collision_debug;
        }

        if self.message_overlay.is_some() {
            if ctx.just_pressed(INTERACT_BUTTON) || ctx.just_pressed(button::CIRCLE) {
                self.message_overlay = None;
            }
            self.camera_turning_last_tick = false;
            return;
        }

        if !ctx.pad.is_analog() {
            self.camera_turning_last_tick = false;
            return;
        }

        if ctx.just_pressed(button::SELECT) {
            self.free_orbit = !self.free_orbit;
        }
        let delta_vblanks = 1u16;
        self.advance_box_prop_break_events(delta_vblanks);
        self.advance_box_prop_falls(delta_vblanks);
        if CAMERA_SWEEP_ENABLED {
            self.update_camera_sweep(delta_vblanks);
            return;
        }
        if self.free_orbit {
            let (right_x, right_y) = ctx.pad.sticks.right_centered();
            self.camera_turning_last_tick = abs_i16(right_x) >= CAMERA_STICK_DEADZONE;
            self.orbit_yaw = self.orbit_yaw.add_signed_q12(scale_i16_by_vblanks(
                stick_to_yaw_delta(psx_engine::InputAxis::new(right_x.saturating_neg())),
                delta_vblanks,
            ));
            self.orbit_radius = (self.orbit_radius
                + scale_i32_by_vblanks(
                    stick_to_radius_delta(psx_engine::InputAxis::new(right_y)),
                    delta_vblanks,
                ))
            .clamp(CAMERA_RADIUS_MIN, CAMERA_RADIUS_MAX);
            let button_yaw_step =
                scale_i16_by_vblanks(CAMERA_YAW_STEP.as_q12() as i16, delta_vblanks);
            let button_radius_step = scale_i32_by_vblanks(CAMERA_RADIUS_STEP, delta_vblanks);
            if ctx.is_held(button::RIGHT) {
                self.orbit_yaw = self.orbit_yaw.add_signed_q12(button_yaw_step);
            }
            if ctx.is_held(button::LEFT) {
                self.orbit_yaw = self
                    .orbit_yaw
                    .add_signed_q12(button_yaw_step.saturating_neg());
            }
            if ctx.is_held(button::UP) {
                self.orbit_radius = (self.orbit_radius - button_radius_step).max(CAMERA_RADIUS_MIN);
            }
            if ctx.is_held(button::DOWN) {
                self.orbit_radius = (self.orbit_radius + button_radius_step).min(CAMERA_RADIUS_MAX);
            }
            self.player_moved_last_tick = false;
            self.active_interactable = None;
            telemetry::stage_begin(telemetry::stage::CAMERA);
            self.render_camera = self.free_orbit_camera();
            telemetry::stage_end(telemetry::stage::CAMERA);
            self.refresh_active_room_window_if_needed();
            #[cfg(all(
                feature = "world-grid-visible",
                not(feature = "vis-full-active-chunks")
            ))]
            self.prewarm_visible_cell_caches();
            return;
        }

        let now = ctx.sim_tick;
        let action_locked = self.anim_lock_until_tick > now;
        self.refresh_active_interactable();
        if !action_locked {
            if let Some(index) = self.active_interactable {
                if ctx.just_pressed(INTERACT_BUTTON) && self.activate_interactable(index) {
                    self.evade_run_hold_ticks = 0;
                    self.evade_run_hold_consumed = false;
                    self.camera_turning_last_tick = false;
                    return;
                }
            }
        }
        let circle = self.update_evade_run_button(ctx, delta_vblanks);
        let mut input = if action_locked {
            CharacterMotorInput::default()
        } else {
            motor_input(ctx, self.camera.yaw(), circle.sprint, circle.evade)
        };
        if !action_locked && self.motor.action().is_idle() {
            let started = if ctx.just_pressed(LIGHT_ATTACK_BUTTON) {
                self.start_player_anim_action(PlayerAnim::LightAttack, now, ctx.video_hz)
            } else if ctx.just_pressed(HEAVY_ATTACK_BUTTON) {
                self.start_player_anim_action(PlayerAnim::HeavyAttack, now, ctx.video_hz)
            } else {
                false
            };
            if started {
                input = CharacterMotorInput::default();
            }
        }
        let config = self.motor_config();
        if self.anim_lock_until_tick > now && player_anim_is_attack(self.anim_state) {
            self.break_box_props_for_attack(config);
        } else if let Some(trigger) =
            box_prop_movement_break_trigger(input, config, self.motor.stamina_q12())
        {
            self.break_box_props_for_movement(trigger, input, config, delta_vblanks);
        }
        telemetry::stage_begin(telemetry::stage::SIM_COLLISION);
        let mut collision_rooms = [const { CharacterCollisionRoom::EMPTY }; MAX_COLLISION_ROOMS];
        let collision_room_count = if self.chunked_level() {
            let catchup = delta_vblanks.min(4) as i32;
            let margin = config
                .radius
                .saturating_add(config.run_speed.saturating_mul(catchup));
            self.collect_collision_rooms(self.motor.position(), margin, &mut collision_rooms)
        } else {
            0
        };
        let single_collision_room = if collision_room_count == 1 {
            collision_rooms[0].room
        } else {
            None
        };
        let room_collision = match collision_room_count {
            0 => self
                .current_collision_room
                .as_ref()
                .map(|room| room.collision()),
            1 => single_collision_room.as_ref().map(|room| room.collision()),
            _ => None,
        };
        let mut blockers = [CharacterCollisionCylinder::EMPTY; MAX_MODEL_INSTANCES];
        let blocker_count = self.collect_collision_blockers(&mut blockers);
        let mut aabb_blockers = [CharacterCollisionAabb::EMPTY; MAX_BOX_PROP_BLOCKERS];
        let aabb_blocker_count = self.collect_box_prop_collision_blockers(&mut aabb_blockers);
        let collision = if collision_room_count <= 1 {
            CharacterCollision::new_with_aabbs(
                room_collision,
                &blockers[..blocker_count],
                &aabb_blockers[..aabb_blocker_count],
            )
        } else {
            CharacterCollision::rooms_with_aabbs(
                &collision_rooms[..collision_room_count],
                &blockers[..blocker_count],
                &aabb_blockers[..aabb_blocker_count],
            )
        };
        telemetry::stage_end(telemetry::stage::SIM_COLLISION);
        telemetry::stage_begin(telemetry::stage::SIM_SOLVE);
        let motor_frame =
            self.motor
                .update_vblanks_with_collision(collision, input, config, delta_vblanks);
        telemetry::stage_end(telemetry::stage::SIM_SOLVE);
        self.player_moved_last_tick = motor_frame.moved;
        telemetry::stage_begin(telemetry::stage::SIM_ROOM_TRACK);
        if !self.update_current_room_from_player() {
            self.refresh_active_room_window_if_needed();
        }
        telemetry::stage_end(telemetry::stage::SIM_ROOM_TRACK);

        let new_state = if self.anim_lock_until_tick > now {
            self.anim_state
        } else {
            player_anim_from_motor(motor_frame.anim)
        };
        if new_state != self.anim_state {
            self.anim_state = new_state;
            self.anim_start_tick = now;
            if new_state.is_motor_fixed_action() {
                if let Some(character) = self.character {
                    self.lock_player_anim_action(character, new_state, now, ctx.video_hz);
                }
            }
        }

        if self.lock_target.is_some() {
            if !self.lock_target_valid(LOCK_BREAK_RANGE) {
                self.lock_target = None;
                self.lock_switch_stick_held = false;
            } else {
                self.update_lock_target_switch(ctx);
            }
        }
        let (camera_right_x, _) = ctx.pad.sticks.right_centered();
        self.camera_turning_last_tick =
            self.lock_target.is_none() && abs_i16(camera_right_x) >= CAMERA_STICK_DEADZONE;
        if SOFT_LOCK_ENABLED {
            self.update_soft_lock(ctx);
        } else {
            self.soft_lock_target = None;
            self.soft_lock_suppressed = false;
        }

        telemetry::stage_begin(telemetry::stage::CAMERA);
        self.render_camera = self.update_follow_camera(ctx);
        telemetry::stage_end(telemetry::stage::CAMERA);
        self.refresh_active_room_window_if_needed();
        #[cfg(all(
            feature = "world-grid-visible",
            not(feature = "vis-full-active-chunks")
        ))]
        self.prewarm_visible_cell_caches();
    }

    fn render(&mut self, ctx: &mut Ctx) {
        if !ctx.pad.is_analog() {
            if let Some(font) = self.ui_fonts[0].as_ref() {
                draw_analog_required_prompt(font);
            }
            return;
        }

        let camera = self.render_camera;
        let post_cross_debug = POST_CROSS_RENDER_DEBUG_LOGS && self.post_cross_debug_frames != 0;
        let post_cross_detail = post_cross_debug
            && self.post_cross_debug_frames == RUNTIME_SCHEDULE.post_cross_render_debug_frames;
        let mut post_cross_logged_end = false;
        if post_cross_debug {
            debug_log_post_cross_render_start(
                self.room_index,
                camera,
                self.portal_visibility.visible_room_mask(),
                self.active_room_mask(),
                self.current_collision_room.is_some(),
            );
        }

        let mut ot = unsafe { OtFrame::begin(&mut OT) };
        let mut primitive_packets = unsafe { PrimitivePacketArena::new(&mut PRIMITIVE_PACKETS) };

        let room_record = ROOMS.get(self.room_index.to_usize());
        // Sky inserts into the OT background slot before the world pass borrows
        // the OT; world geometry (slots 0..=OT_DEPTH-2) then draws in front.
        if let Some(room_record) = room_record {
            telemetry::stage_begin(telemetry::stage::SKY);
            draw_sky_panorama(room_record.sky, camera, &mut primitive_packets, &mut ot);
            telemetry::stage_end(telemetry::stage::SKY);
        }

        let mut world = unsafe { begin_world_render_pass(&mut ot, &mut WORLD_COMMANDS) };

        if let Some(room_record) = room_record {
            telemetry::stage_begin(telemetry::stage::FAR_VISTA);
            draw_far_vista_ring(
                camera,
                room_record.far_vista,
                room_surface_options(room_record),
                &mut primitive_packets,
                &mut world,
            );
            telemetry::stage_end(telemetry::stage::FAR_VISTA);
        }

        if self.current_collision_room.is_some() {
            let mut total_instance_stats = ModelInstanceDrawStats::default();
            let mut room_active_chunks = 0u32;
            let mut room_cached_draws = 0u32;
            let mut room_uncached_draws = 0u32;
            let mut room_cache_cells = 0u32;
            let mut room_cache_vertices = 0u32;
            let mut room_cache_surfaces = 0u32;
            let mut room_cache_fallback_draws = 0u32;
            #[cfg(all(
                feature = "world-grid-visible",
                not(feature = "vis-full-active-chunks")
            ))]
            let mut room_visibility_fallback_draws = 0u32;
            #[cfg(not(all(
                feature = "world-grid-visible",
                not(feature = "vis-full-active-chunks")
            )))]
            let room_visibility_fallback_draws = 0u32;
            let mut room_active_chunk_mask = RuntimeDebugMask::EMPTY;
            let mut room_drawn_chunk_mask = RuntimeDebugMask::EMPTY;
            #[cfg(feature = "world-grid-visible")]
            let mut room_visible_cells = 0u32;
            #[cfg(all(
                feature = "world-grid-visible",
                not(feature = "vis-full-active-chunks")
            ))]
            let mut room_range_culled_cells = 0u32;
            #[cfg(all(feature = "world-grid-visible", feature = "vis-full-active-chunks"))]
            let room_range_culled_cells = 0u32;
            #[cfg(feature = "world-grid-visible")]
            let mut room_stats_total = GridVisibilityStats::default();

            let active_draw_order = active_room_draw_order(
                &self.active_rooms,
                camera,
                &self.portal_visibility,
                self.room_index,
                cached_room_draw_order_mode(),
            );
            for &active_slot in &active_draw_order {
                if active_slot == INVALID_ACTIVE_ROOM_SLOT {
                    continue;
                }
                let active_slot = active_slot as usize;
                let Some(active) = self.active_rooms[active_slot] else {
                    continue;
                };
                let draws_room = self.portal_visibility_draws_room(active.index);
                if post_cross_detail {
                    debug_log_post_cross_render_room(active_slot, active, draws_room);
                }
                if !draws_room {
                    continue;
                }
                room_active_chunks = room_active_chunks.saturating_add(1);
                let chunk_mask = room_index_debug_mask(active.index);
                room_active_chunk_mask |= chunk_mask;
                if active.surface_cache.ready {
                    room_cache_cells =
                        room_cache_cells.saturating_add(active.surface_cache.cell_count as u32);
                    room_cache_vertices = room_cache_vertices
                        .saturating_add(active.surface_cache.vertex_count as u32);
                    room_cache_surfaces = room_cache_surfaces
                        .saturating_add(active.surface_cache.surface_count as u32);
                }
                let materials = active.materials();
                let Some(room_record) = ROOMS.get(active.index.to_usize()) else {
                    continue;
                };
                let room_options = room_surface_options(room_record);
                let actor_options = room_options;
                let room_camera = camera_for_room(camera, active);
                let lighting = RuntimeRoomLighting {
                    room_index: active.index,
                    ambient: Rgb8::from_array(active.ambient_rgb),
                    camera: room_camera,
                    fog_enabled: room_record.flags & room_flags::FOG_ENABLED != 0,
                    fog_rgb: Rgb8::from_array(room_record.fog_rgb),
                    fog_near: room_record.fog_near,
                    fog_far: room_record.fog_far,
                };
                telemetry::stage_begin(telemetry::stage::ROOM);
                #[cfg(feature = "world-grid-visible")]
                {
                    #[cfg(feature = "vis-full-active-chunks")]
                    {
                        let stats = if active.surface_cache.ready {
                            room_cached_draws = room_cached_draws.saturating_add(1);
                            if let Some((
                                cached_cells,
                                cached_cell_vertices,
                                cached_vertices,
                                cached_surfaces,
                            )) = room_surface_cache_slices(active.index, active.surface_cache)
                            {
                                let vertex_count = cached_vertices.len();
                                let projected_indices =
                                    unsafe { &mut CACHED_ROOM_PROJECTED_INDICES[..vertex_count] };
                                let projected_vertices =
                                    unsafe { &mut CACHED_ROOM_PROJECTED_VERTICES[..vertex_count] };
                                let projected_ready =
                                    unsafe { &mut CACHED_ROOM_PROJECTED_READY[..vertex_count] };
                                let projected_depths =
                                    unsafe { &mut CACHED_ROOM_PROJECTED_DEPTHS[..vertex_count] };
                                let accepted_cell_indices =
                                    unsafe { &mut CACHED_ROOM_ACCEPTED_CELL_INDICES[..] };
                                let accepted_cell_depths =
                                    unsafe { &mut CACHED_ROOM_ACCEPTED_CELL_DEPTHS[..] };
                                draw_indexed_cached_room_vertex_lit_all_cells(
                                    cached_cells,
                                    cached_cell_vertices,
                                    cached_vertices,
                                    cached_surfaces,
                                    projected_indices,
                                    projected_vertices,
                                    projected_ready,
                                    projected_depths,
                                    accepted_cell_indices,
                                    accepted_cell_depths,
                                    materials,
                                    &lighting,
                                    &room_camera,
                                    room_options,
                                    cached_room_depth_mode(),
                                    cached_room_subdivision_mode(),
                                    ROOM_VISIBLE_CELL_SCREEN_MARGIN,
                                    &mut primitive_packets,
                                    &mut world,
                                )
                            } else {
                                room_uncached_draws = room_uncached_draws.saturating_add(1);
                                room_cache_fallback_draws =
                                    room_cache_fallback_draws.saturating_add(1);
                                if let Some(render_room) = active.render() {
                                    room_drawn_chunk_mask |= chunk_mask;
                                    draw_room_vertex_lit(
                                        render_room,
                                        materials,
                                        &lighting,
                                        &room_camera,
                                        room_options,
                                        &mut primitive_packets,
                                        &mut world,
                                    );
                                }
                                GridVisibilityStats::default()
                            }
                        } else {
                            room_uncached_draws = room_uncached_draws.saturating_add(1);
                            if active_surface_cache_failed(active.surface_cache) {
                                room_cache_fallback_draws =
                                    room_cache_fallback_draws.saturating_add(1);
                            }
                            if let Some(render_room) = active.render() {
                                room_drawn_chunk_mask |= chunk_mask;
                                draw_room_vertex_lit(
                                    render_room,
                                    materials,
                                    &lighting,
                                    &room_camera,
                                    room_options,
                                    &mut primitive_packets,
                                    &mut world,
                                );
                            }
                            GridVisibilityStats::default()
                        };
                        room_visible_cells =
                            room_visible_cells.saturating_add(stats.cells_drawn as u32);
                        if stats.cells_drawn > 0 || stats.surfaces_considered > 0 {
                            room_drawn_chunk_mask |= chunk_mask;
                        }
                        accumulate_grid_visibility_stats(&mut room_stats_total, stats);
                    }
                    #[cfg(not(feature = "vis-full-active-chunks"))]
                    {
                        let player = self.motor.position();
                        let global_visibility_anchor = player;
                        let visibility_anchor = RoomPoint::new(
                            global_visibility_anchor.x.saturating_sub(active.offset_x),
                            player.y,
                            global_visibility_anchor.z.saturating_sub(active.offset_z),
                        );
                        let visibility = GridVisibility::around(
                            visibility_anchor,
                            room_visibility_radius(room_record),
                        )
                        .with_screen_margin(ROOM_VISIBLE_CELL_SCREEN_MARGIN);
                        telemetry::stage_begin(telemetry::stage::ROOM_VISIBLE_LIST);
                        let visible_cells_result = self.cached_precomputed_visible_cells(
                            active_slot,
                            active.index,
                            active.width,
                            active.depth,
                            active.sector_size,
                            visibility_anchor,
                            active.offset_x,
                            active.offset_z,
                            global_visibility_anchor,
                            room_camera,
                            ROOM_VISIBLE_CELL_STATIONARY_CANDIDATES
                                && !self.player_moved_last_tick
                                && self.camera_turning_last_tick
                                && active.surface_cache.ready,
                        );
                        telemetry::stage_end(telemetry::stage::ROOM_VISIBLE_LIST);
                        let stats = if let Some((cells, range_culled)) = visible_cells_result {
                            room_range_culled_cells =
                                room_range_culled_cells.saturating_add(range_culled as u32);
                            room_visible_cells =
                                room_visible_cells.saturating_add(cells.len() as u32);
                            if active.surface_cache.ready {
                                room_cached_draws = room_cached_draws.saturating_add(1);
                                if let Some((
                                    cached_cells,
                                    cached_cell_vertices,
                                    cached_vertices,
                                    cached_surfaces,
                                )) =
                                    room_surface_cache_slices(active.index, active.surface_cache)
                                {
                                    let vertex_count = cached_vertices.len();
                                    let projected_indices = unsafe {
                                        &mut CACHED_ROOM_PROJECTED_INDICES[..vertex_count]
                                    };
                                    let projected_vertices = unsafe {
                                        &mut CACHED_ROOM_PROJECTED_VERTICES[..vertex_count]
                                    };
                                    let projected_ready =
                                        unsafe { &mut CACHED_ROOM_PROJECTED_READY[..vertex_count] };
                                    let projected_depths = unsafe {
                                        &mut CACHED_ROOM_PROJECTED_DEPTHS[..vertex_count]
                                    };
                                    let accepted_cell_indices =
                                        unsafe { &mut CACHED_ROOM_ACCEPTED_CELL_INDICES[..] };
                                    let accepted_cell_depths =
                                        unsafe { &mut CACHED_ROOM_ACCEPTED_CELL_DEPTHS[..] };
                                    draw_indexed_cached_room_vertex_lit_visible_cells(
                                        cached_cells,
                                        cached_cell_vertices,
                                        cached_vertices,
                                        cached_surfaces,
                                        projected_indices,
                                        projected_vertices,
                                        projected_ready,
                                        projected_depths,
                                        accepted_cell_indices,
                                        accepted_cell_depths,
                                        active.depth,
                                        active.sector_size,
                                        materials,
                                        &lighting,
                                        &room_camera,
                                        room_options,
                                        cached_room_depth_mode(),
                                        cached_room_subdivision_mode(),
                                        cells,
                                        visibility.screen_margin,
                                        &mut primitive_packets,
                                        &mut world,
                                    )
                                } else {
                                    room_uncached_draws = room_uncached_draws.saturating_add(1);
                                    if let Some(render_room) = active.render() {
                                        draw_room_vertex_lit_visible_cells(
                                            render_room,
                                            materials,
                                            &lighting,
                                            &room_camera,
                                            room_options,
                                            cells,
                                            visibility.screen_margin,
                                            &mut primitive_packets,
                                            &mut world,
                                        )
                                    } else {
                                        GridVisibilityStats::default()
                                    }
                                }
                            } else {
                                room_uncached_draws = room_uncached_draws.saturating_add(1);
                                if active_surface_cache_failed(active.surface_cache) {
                                    room_cache_fallback_draws =
                                        room_cache_fallback_draws.saturating_add(1);
                                }
                                if let Some(render_room) = active.render() {
                                    draw_room_vertex_lit_visible_cells(
                                        render_room,
                                        materials,
                                        &lighting,
                                        &room_camera,
                                        room_options,
                                        cells,
                                        visibility.screen_margin,
                                        &mut primitive_packets,
                                        &mut world,
                                    )
                                } else {
                                    GridVisibilityStats::default()
                                }
                            }
                        } else {
                            room_uncached_draws = room_uncached_draws.saturating_add(1);
                            room_visibility_fallback_draws =
                                room_visibility_fallback_draws.saturating_add(1);
                            if let Some(render_room) = active.render() {
                                draw_room_vertex_lit(
                                    render_room,
                                    materials,
                                    &lighting,
                                    &room_camera,
                                    room_options,
                                    &mut primitive_packets,
                                    &mut world,
                                );
                            }
                            GridVisibilityStats::default()
                        };
                        if stats.cells_drawn > 0 || stats.surfaces_considered > 0 {
                            room_drawn_chunk_mask |= chunk_mask;
                        }
                        accumulate_grid_visibility_stats(&mut room_stats_total, stats);
                    }
                }
                #[cfg(not(feature = "world-grid-visible"))]
                {
                    room_uncached_draws = room_uncached_draws.saturating_add(1);
                    if active_surface_cache_failed(active.surface_cache) {
                        room_cache_fallback_draws = room_cache_fallback_draws.saturating_add(1);
                    }
                    if let Some(render_room) = active.render() {
                        room_drawn_chunk_mask |= chunk_mask;
                        draw_room_vertex_lit(
                            render_room,
                            materials,
                            &lighting,
                            &room_camera,
                            room_options,
                            &mut primitive_packets,
                            &mut world,
                        );
                    }
                }
                telemetry::stage_end(telemetry::stage::ROOM);
                telemetry::stage_begin(telemetry::stage::ENTITY_MARKERS);
                draw_entity_markers(
                    ENTITIES,
                    active.index,
                    materials,
                    &room_camera,
                    room_options,
                    &mut primitive_packets,
                    &mut world,
                );
                telemetry::stage_end(telemetry::stage::ENTITY_MARKERS);
                telemetry::stage_begin(telemetry::stage::IMAGE_PROPS);
                box_prop_profile_begin(telemetry::stage::BOX_PROPS);
                draw_box_props(
                    BOX_PROPS,
                    &self.box_prop_broken,
                    &self.box_prop_runtime,
                    &self.box_prop_fall,
                    active.index,
                    &room_camera,
                    actor_options,
                    &lighting,
                    &mut primitive_packets,
                    &mut world,
                );
                box_prop_profile_end(telemetry::stage::BOX_PROPS);
                box_prop_profile_begin(telemetry::stage::BOX_PROP_DEBRIS);
                draw_box_prop_floor_debris(
                    BOX_PROPS,
                    &self.box_prop_broken,
                    &self.box_prop_runtime,
                    active.index,
                    &room_camera,
                    actor_options,
                    &lighting,
                    &mut primitive_packets,
                    &mut world,
                );
                box_prop_profile_end(telemetry::stage::BOX_PROP_DEBRIS);
                box_prop_profile_begin(telemetry::stage::BOX_PROP_SHARDS);
                draw_box_prop_break_events(
                    &self.box_prop_break_events,
                    BOX_PROPS,
                    &self.box_prop_runtime,
                    active.index,
                    &room_camera,
                    actor_options,
                    &lighting,
                    &mut primitive_packets,
                    &mut world,
                );
                box_prop_profile_end(telemetry::stage::BOX_PROP_SHARDS);
                box_prop_profile_begin(telemetry::stage::IMAGE_CARDS);
                draw_image_props(
                    IMAGE_PROPS,
                    active.index,
                    &room_camera,
                    actor_options,
                    &lighting,
                    &mut primitive_packets,
                    &mut world,
                );
                box_prop_profile_end(telemetry::stage::IMAGE_CARDS);
                telemetry::stage_end(telemetry::stage::IMAGE_PROPS);
                telemetry::stage_begin(telemetry::stage::MODEL_INSTANCES);
                let player = self.motor.position();
                let instance_depth_pass = player_actor_depth_for_room(
                    active,
                    self.character,
                    &self.models,
                    player,
                    &room_camera,
                )
                .map(ModelInstanceDepthPass::BehindPlayer)
                .unwrap_or(ModelInstanceDepthPass::All);
                if let Some(shadow_material) = self.shadow_material {
                    draw_model_instance_shadows(
                        active.index,
                        &room_camera,
                        actor_options,
                        shadow_material,
                        &self.models,
                        &mut primitive_packets,
                        &mut world,
                    );
                }
                let instance_stats = draw_model_instances(
                    active.index,
                    ctx.sim_tick,
                    ctx.video_hz,
                    &room_camera,
                    actor_options,
                    &lighting,
                    &self.models,
                    &self.model_faces[..self.model_face_count],
                    &self.model_parts[..self.model_part_count],
                    &self.model_vertices[..self.model_vertex_count],
                    &self.clips,
                    instance_depth_pass,
                    &mut primitive_packets,
                    &mut world,
                );
                telemetry::stage_end(telemetry::stage::MODEL_INSTANCES);
                accumulate_model_instance_draw_stats(&mut total_instance_stats, instance_stats);
            }

            // Player draws through the same compact model path as
            // placed model instances.
            if let Some(character) = self.character {
                let player = self.motor.position();
                let player_lighting = self.current_room_lighting(camera);
                let actor_options = current_room_surface_options(self.room_index);
                telemetry::stage_begin(telemetry::stage::PLAYER);
                if let Some(shadow_material) = self.shadow_material {
                    draw_actor_shadow(
                        player.x,
                        player.y,
                        player.z,
                        actor_shadow_radius(character.radius),
                        &camera,
                        actor_options,
                        shadow_material,
                        &mut primitive_packets,
                        &mut world,
                    );
                }
                let player_draw =
                    player_lighting.map_or(PlayerModelDrawStats::default(), |lighting| {
                        draw_player(
                            character,
                            &self.models,
                            &self.model_faces[..self.model_face_count],
                            &self.model_parts[..self.model_part_count],
                            &self.model_vertices[..self.model_vertex_count],
                            &self.clips,
                            player.x,
                            player.y,
                            player.z,
                            self.motor.yaw(),
                            self.anim_state.action(),
                            character.clip_for(self.anim_state),
                            self.anim_start_tick,
                            ctx.sim_tick,
                            ctx.video_hz,
                            &camera,
                            actor_options,
                            &lighting,
                            &mut primitive_packets,
                            &mut world,
                        )
                    });
                telemetry::stage_end(telemetry::stage::PLAYER);
                emit_model_counters(
                    player_draw.stats,
                    telemetry::counter::PLAYER_PROJECTED_VERTICES,
                    telemetry::counter::PLAYER_SUBMITTED_TRIS,
                    telemetry::counter::PLAYER_CULLED_TRIS,
                    telemetry::counter::PLAYER_DROPPED_TRIS,
                );
                telemetry::counter(
                    telemetry::counter::PLAYER_BOUNDS_TESTS,
                    player_draw.bounds_tests as u32,
                );
                telemetry::counter(
                    telemetry::counter::PLAYER_BOUNDS_CULLED,
                    player_draw.bounds_culled as u32,
                );
                telemetry::stage_begin(telemetry::stage::EQUIPMENT);
                let equipment_stats = if player_draw.bounds_culled != 0 {
                    EquipmentDrawStats::default()
                } else {
                    player_lighting.map_or(EquipmentDrawStats::default(), |lighting| {
                        draw_player_equipment(
                            self.room_index,
                            character,
                            &self.models,
                            &self.model_faces[..self.model_face_count],
                            &self.model_parts[..self.model_part_count],
                            &self.model_vertices[..self.model_vertex_count],
                            &self.clips,
                            player.x,
                            player.y,
                            player.z,
                            self.motor.yaw(),
                            self.anim_state.action(),
                            character.clip_for(self.anim_state),
                            self.anim_start_tick,
                            ctx.sim_tick,
                            ctx.video_hz,
                            &camera,
                            actor_options,
                            &lighting,
                            &mut primitive_packets,
                            &mut world,
                        )
                    })
                };
                telemetry::stage_end(telemetry::stage::EQUIPMENT);
                telemetry::counter(
                    telemetry::counter::EQUIPMENT_DRAWS,
                    equipment_stats.draws as u32,
                );
                telemetry::counter(
                    telemetry::counter::EQUIPMENT_ACTIVE_HITBOXES,
                    equipment_stats.active_hitboxes as u32,
                );
                telemetry::counter(
                    telemetry::counter::EQUIPMENT_TARGET_HITS,
                    equipment_stats.target_hits as u32,
                );
                emit_model_counters(
                    equipment_stats.stats,
                    telemetry::counter::EQUIPMENT_PROJECTED_VERTICES,
                    telemetry::counter::EQUIPMENT_SUBMITTED_TRIS,
                    telemetry::counter::EQUIPMENT_CULLED_TRIS,
                    telemetry::counter::EQUIPMENT_DROPPED_TRIS,
                );
            }

            if self.character.is_some() {
                let player = self.motor.position();
                for &active_slot in &active_draw_order {
                    if active_slot == INVALID_ACTIVE_ROOM_SLOT {
                        continue;
                    }
                    let Some(active) = self.active_rooms[active_slot as usize] else {
                        continue;
                    };
                    if !self.portal_visibility_draws_room(active.index) {
                        continue;
                    }
                    let room_camera = camera_for_room(camera, active);
                    let Some(player_depth) = player_actor_depth_for_room(
                        active,
                        self.character,
                        &self.models,
                        player,
                        &room_camera,
                    ) else {
                        continue;
                    };
                    let Some(room_record) = ROOMS.get(active.index.to_usize()) else {
                        continue;
                    };
                    let actor_options = room_surface_options(room_record);
                    let lighting = RuntimeRoomLighting {
                        room_index: active.index,
                        ambient: Rgb8::from_array(active.ambient_rgb),
                        camera: room_camera,
                        fog_enabled: room_record.flags & room_flags::FOG_ENABLED != 0,
                        fog_rgb: Rgb8::from_array(room_record.fog_rgb),
                        fog_near: room_record.fog_near,
                        fog_far: room_record.fog_far,
                    };
                    telemetry::stage_begin(telemetry::stage::MODEL_INSTANCES);
                    let instance_stats = draw_model_instances(
                        active.index,
                        ctx.sim_tick,
                        ctx.video_hz,
                        &room_camera,
                        actor_options,
                        &lighting,
                        &self.models,
                        &self.model_faces[..self.model_face_count],
                        &self.model_parts[..self.model_part_count],
                        &self.model_vertices[..self.model_vertex_count],
                        &self.clips,
                        ModelInstanceDepthPass::InFrontOfPlayer(player_depth),
                        &mut primitive_packets,
                        &mut world,
                    );
                    telemetry::stage_end(telemetry::stage::MODEL_INSTANCES);
                    accumulate_model_instance_draw_stats(&mut total_instance_stats, instance_stats);
                }
            }

            telemetry::counter(telemetry::counter::ROOM_ACTIVE_CHUNKS, room_active_chunks);
            emit_room_chunk_mask(
                telemetry::counter::ROOM_ACTIVE_CHUNK_MASK_LO,
                telemetry::counter::ROOM_ACTIVE_CHUNK_MASK_HI,
                room_active_chunk_mask,
            );
            emit_room_chunk_mask(
                telemetry::counter::ROOM_DRAWN_CHUNK_MASK_LO,
                telemetry::counter::ROOM_DRAWN_CHUNK_MASK_HI,
                room_drawn_chunk_mask,
            );
            let debug_view = self.active_room_selection_view();
            emit_player_map_debug(
                self.room_index,
                self.motor.position(),
                RoomPoint::new(camera.position.x, camera.position.y, camera.position.z),
                self.portal_visibility_camera_global,
                yaw_q12_from_basis(debug_view.sin_yaw, debug_view.cos_yaw),
                debug_view.sin_yaw,
                debug_view.cos_yaw,
                debug_view.sin_pitch,
                debug_view.cos_pitch,
            );
            self.emit_portal_visibility_counters();
            #[cfg(feature = "cd-stream-bench")]
            unsafe {
                telemetry::counter(
                    telemetry::counter::ROOM_STREAM_RESIDENT_SLOTS,
                    ROOM_STREAM_SCHEDULER.resident_slot_count() as u32,
                );
                emit_room_chunk_mask(
                    telemetry::counter::ROOM_STREAM_LOADING_MASK_LO,
                    telemetry::counter::ROOM_STREAM_LOADING_MASK_HI,
                    ROOM_STREAM_SCHEDULER.loading_room_mask(),
                );
                emit_room_chunk_mask(
                    telemetry::counter::ROOM_STREAM_RESIDENT_MASK_LO,
                    telemetry::counter::ROOM_STREAM_RESIDENT_MASK_HI,
                    ROOM_STREAM_SCHEDULER.resident_room_mask(),
                );
            }
            telemetry::counter(telemetry::counter::ROOM_CACHED_DRAWS, room_cached_draws);
            telemetry::counter(telemetry::counter::ROOM_UNCACHED_DRAWS, room_uncached_draws);
            telemetry::counter(telemetry::counter::ROOM_CACHE_CELLS, room_cache_cells);
            telemetry::counter(telemetry::counter::ROOM_CACHE_VERTICES, room_cache_vertices);
            telemetry::counter(telemetry::counter::ROOM_CACHE_SURFACES, room_cache_surfaces);
            telemetry::counter(
                telemetry::counter::ROOM_CACHE_FALLBACK_DRAWS,
                room_cache_fallback_draws,
            );
            telemetry::counter(
                telemetry::counter::ROOM_VISIBILITY_FALLBACK_DRAWS,
                room_visibility_fallback_draws,
            );
            telemetry::counter(
                telemetry::counter::ROOM_CHUNKS_CONSIDERED,
                self.active_room_candidates as u32,
            );
            telemetry::counter(
                telemetry::counter::ROOM_CHUNK_CACHE_SKIPS,
                self.active_room_cache_skips as u32,
            );
            #[cfg(feature = "world-grid-visible")]
            {
                telemetry::counter(telemetry::counter::ROOM_VISIBLE_CELLS, room_visible_cells);
                telemetry::counter(
                    telemetry::counter::ROOM_CELLS_RANGE_CULLED,
                    room_range_culled_cells,
                );
                telemetry::counter(
                    telemetry::counter::ROOM_CELLS_CONSIDERED,
                    room_stats_total.cells_considered as u32,
                );
                telemetry::counter(
                    telemetry::counter::ROOM_CELLS_DRAWN,
                    room_stats_total.cells_drawn as u32,
                );
                telemetry::counter(
                    telemetry::counter::ROOM_CELLS_CULLED,
                    room_stats_total.cells_frustum_culled as u32,
                );
                telemetry::counter(
                    telemetry::counter::ROOM_SURFACES_CONSIDERED,
                    room_stats_total.surfaces_considered as u32,
                );
                telemetry::counter(
                    telemetry::counter::ROOM_PROJECTED_VERTICES,
                    room_stats_total.projected_vertices as u32,
                );
            }
            telemetry::counter(
                telemetry::counter::MODEL_INSTANCE_DRAWS,
                total_instance_stats.draws as u32,
            );
            telemetry::counter(
                telemetry::counter::MODEL_INSTANCE_BOUNDS_TESTS,
                total_instance_stats.bounds_tests as u32,
            );
            telemetry::counter(
                telemetry::counter::MODEL_INSTANCE_BOUNDS_CULLED,
                total_instance_stats.bounds_culled as u32,
            );
            emit_model_counters(
                total_instance_stats.stats,
                telemetry::counter::MODEL_INSTANCE_PROJECTED_VERTICES,
                telemetry::counter::MODEL_INSTANCE_SUBMITTED_TRIS,
                telemetry::counter::MODEL_INSTANCE_CULLED_TRIS,
                telemetry::counter::MODEL_INSTANCE_DROPPED_TRIS,
            );
            if post_cross_debug {
                debug_log_post_cross_render_end(
                    self.room_index,
                    room_active_chunk_mask,
                    room_drawn_chunk_mask,
                    primitive_packets.len(),
                    primitive_packets.remaining(),
                    world.command_len(),
                );
                post_cross_logged_end = true;
            }
        }

        if post_cross_debug && !post_cross_logged_end {
            debug_log_post_cross_render_end(
                self.room_index,
                RuntimeDebugMask::EMPTY,
                RuntimeDebugMask::EMPTY,
                primitive_packets.len(),
                primitive_packets.remaining(),
                world.command_len(),
            );
        }
        if post_cross_debug {
            self.post_cross_debug_frames = self.post_cross_debug_frames.saturating_sub(1);
        }

        let world_command_len = world.command_len();
        telemetry::stage_begin(telemetry::stage::WORLD_FLUSH);
        world.flush();
        telemetry::stage_end(telemetry::stage::WORLD_FLUSH);
        let _ = self.draw_particle_emitters(camera, ctx.sim_tick, &mut ot, &mut primitive_packets);
        telemetry::counter(
            telemetry::counter::TRI_PRIMITIVES,
            primitive_packets.len() as u32,
        );
        telemetry::counter(
            telemetry::counter::TRI_PRIMITIVE_REMAINING,
            primitive_packets.remaining() as u32,
        );
        telemetry::counter(telemetry::counter::WORLD_COMMANDS, world_command_len as u32);
        telemetry::stage_begin(telemetry::stage::OT_SUBMIT);
        ot.submit();
        telemetry::stage_end(telemetry::stage::OT_SUBMIT);

        if let Some(room_record) = ROOMS.get(self.room_index.to_usize()) {
            draw_room_atmosphere_overlay(room_record, ctx.sim_tick);
        }

        if self.show_collision_debug {
            self.draw_collision_debug_overlay(camera);
        }

        if let Some(target) = self.lock_target_indicator_position() {
            draw_lock_target_indicator(target, camera, ctx.sim_tick);
        }

        if self.character.is_some() {
            // The shared UI_NODES pool now holds front-end menu scenes too, so
            // draw only the HUD scene's slice as the in-game overlay.
            let (hud_first, hud_count) = hud_scene_range();
            let font_table = [
                self.ui_fonts[0].as_ref(),
                self.ui_fonts[1].as_ref(),
                self.ui_fonts[2].as_ref(),
                self.ui_fonts[3].as_ref(),
            ];
            draw_player_hud(
                UI_NODES,
                hud_first,
                hud_count,
                &font_table,
                (ctx.sim_tick.as_u32() & 0xffff) as u16,
                self.motor.stamina_q12(),
                self.motor_config().stamina_max_q12,
            );
        }

        if let Some(font) = self.ui_fonts[0].as_ref() {
            if let Some(message) = self.message_overlay {
                draw_interactable_message(font, message.title, message.body);
            } else if let Some(index) = self.active_interactable {
                if let Some(interactable) = INTERACTABLES.get(index) {
                    draw_interaction_prompt(font, interactable.prompt);
                }
            }
        }
    }
}

/// Node-pool range `(first, count)` of the cooked "HUD" UI scene, so the
/// in-game overlay draws only the HUD and not the front-end menu scenes that
/// now share the same `UI_NODES` pool. Falls back to the whole pool when no
/// scene is named "HUD".
fn hud_scene_range() -> (usize, usize) {
    let mut i = 0usize;
    while i < UI_SCENES.len() {
        let scene = &UI_SCENES[i];
        if scene.name == "HUD" {
            return (scene.node_first as usize, scene.node_count as usize);
        }
        i += 1;
    }
    (0, UI_NODES.len())
}

fn interactable_is_active(interactable: &InteractableRecord) -> bool {
    (interactable.flags & psx_level::interactable_flags::ENABLED) != 0
}

fn interactable_message_text(interactable: &InteractableRecord) -> (&'static str, &'static str) {
    INTERACTABLE_MESSAGES
        .get(interactable.message as usize)
        .map(|message| (message.title, message.body))
        .unwrap_or(match interactable.kind {
            InteractableKind::Message => ("ECHO REMNANT", ""),
            InteractableKind::Checkpoint => ("SYNC RELAY", "Relay synchronized."),
        })
}

#[cfg(all(
    feature = "world-order-global",
    any(
        feature = "world-order-slot",
        feature = "world-order-linked",
        feature = "world-order-bucketed"
    )
))]
compile_error!("choose only one world-order-* feature");
#[cfg(all(
    feature = "world-order-slot",
    any(feature = "world-order-linked", feature = "world-order-bucketed")
))]
compile_error!("choose only one world-order-* feature");
#[cfg(all(feature = "world-order-linked", feature = "world-order-bucketed"))]
compile_error!("choose only one world-order-* feature");

fn begin_world_render_pass<'a, 'ot>(
    ot: &'a mut OtFrame<'ot, OT_DEPTH>,
    commands: &'a mut [WorldTriCommand],
) -> WorldRenderPass<'a, 'ot, OT_DEPTH> {
    #[cfg(feature = "world-order-global")]
    {
        return WorldRenderPass::new_deferred_sorted(ot, commands);
    }
    #[cfg(feature = "world-order-slot")]
    {
        return WorldRenderPass::new_deferred_slot_sorted(ot, commands);
    }
    #[cfg(feature = "world-order-linked")]
    {
        return WorldRenderPass::new(ot, commands);
    }
    #[cfg(feature = "world-order-bucketed")]
    {
        return WorldRenderPass::new_bucketed(ot, commands);
    }
    #[cfg(not(any(
        feature = "world-order-global",
        feature = "world-order-slot",
        feature = "world-order-linked",
        feature = "world-order-bucketed"
    )))]
    {
        WorldRenderPass::new_deferred_sorted(ot, commands)
    }
}

impl Playtest {
    fn start_player_anim_action(
        &mut self,
        anim: PlayerAnim,
        now: SimTick,
        video_hz: VideoHz,
    ) -> bool {
        let Some(character) = self.character else {
            return false;
        };
        if !self.lock_player_anim_action(character, anim, now, video_hz) {
            return false;
        }
        self.anim_state = anim;
        self.anim_start_tick = now;
        true
    }

    fn lock_player_anim_action(
        &mut self,
        character: RuntimeCharacter,
        anim: PlayerAnim,
        now: SimTick,
        video_hz: VideoHz,
    ) -> bool {
        if character.action_clip(anim.action()).is_none() {
            return false;
        }
        let clip = character.clip_for(anim);
        let duration = self
            .player_clip_duration_vblanks(character, clip, video_hz)
            .unwrap_or(24)
            .max(1);
        self.anim_lock_until_tick = now.saturating_add(duration);
        true
    }

    fn motor_config(&self) -> CharacterMotorConfig {
        let mut config = match self.character {
            Some(c) => c.motor_config(),
            None => CharacterMotorConfig::character(
                0,
                scaled_player_speed(FALLBACK_PLAYER_SPEED),
                scaled_player_speed(FALLBACK_PLAYER_SPEED),
                FALLBACK_PLAYER_YAW_STEP,
            ),
        };
        if let Some(room) = ROOMS.get(self.room_index.to_usize()) {
            config.gravity_per_tick = room.gravity_per_tick;
        }
        config
    }

    fn collect_collision_blockers(
        &self,
        out: &mut [CharacterCollisionCylinder; MAX_MODEL_INSTANCES],
    ) -> usize {
        let mut count = 0usize;
        for inst in MODEL_INSTANCES {
            if inst.room != self.room_index || count >= out.len() {
                continue;
            }
            let Some(model) = self.models.get(inst.model.to_usize()).copied().flatten() else {
                continue;
            };
            let height = (model.world_height as i32).max(1);
            let radius = i32::from(model.collision_radius).max(1);
            if radius <= 0 {
                continue;
            }
            out[count] = CharacterCollisionCylinder::new(
                RoomPoint::new(inst.x, inst.y, inst.z),
                radius,
                height,
            );
            count += 1;
        }
        count
    }

    fn collect_collision_rooms(
        &self,
        anchor: RoomPoint,
        margin: i32,
        out: &mut [CharacterCollisionRoom<'static>],
    ) -> usize {
        let mut count = 0usize;
        let mut collected_rooms = [INVALID_ROOM_INDEX; MAX_COLLISION_ROOMS];
        let current_authored = authored_room_for_chunk(self.room_index);
        for active in self.active_rooms.iter().flatten() {
            if count >= out.len() {
                break;
            }
            if current_authored.is_some()
                && authored_room_for_chunk(active.index) != current_authored
            {
                continue;
            }
            if !active_room_overlaps_collision_window(*active, anchor, margin) {
                continue;
            }
            out[count] = CharacterCollisionRoom::from_collision(
                active.collision_room,
                active.offset_x,
                active.offset_z,
            )
            .with_offset_y(active.offset_y);
            collected_rooms[count] = active.index;
            count += 1;
        }
        count = self.collect_current_portal_collision_rooms(
            current_authored,
            anchor,
            margin,
            out,
            &mut collected_rooms,
            count,
        );
        #[cfg(feature = "cd-stream-bench")]
        {
            count = self.collect_resident_streamed_collision_rooms(
                current_authored,
                anchor,
                margin,
                out,
                &mut collected_rooms,
                count,
            );
        }
        count
    }

    fn collect_current_portal_collision_rooms(
        &self,
        current_authored: Option<u32>,
        anchor: RoomPoint,
        margin: i32,
        out: &mut [CharacterCollisionRoom<'static>],
        collected_rooms: &mut [RoomIndex; MAX_COLLISION_ROOMS],
        mut count: usize,
    ) -> usize {
        let Some(current_record) = ROOMS.get(self.room_index.to_usize()) else {
            return count;
        };
        let portal_first = current_record.portal_first as usize;
        let portal_end = portal_first.saturating_add(current_record.portal_count as usize);
        let mut portal_index = portal_first;
        while portal_index < portal_end.min(ROOM_PORTALS.len()) && count < out.len() {
            let portal = ROOM_PORTALS[portal_index];
            portal_index += 1;
            if portal.source_room != self.room_index {
                continue;
            }
            let index = portal.destination_room;
            if collision_room_collected(collected_rooms, count, index) {
                continue;
            }
            if current_authored.is_some() && authored_room_for_chunk(index) != current_authored {
                continue;
            }
            let Some(chunk) = chunk_record_for_room(index) else {
                continue;
            };
            let Some(record) = ROOMS.get(index.to_usize()) else {
                continue;
            };
            if !chunk_overlaps_collision_window(*chunk, current_record, record, anchor, margin) {
                continue;
            }
            let Some(room) = parse_collision_room_for_index(index, record) else {
                continue;
            };
            out[count] = CharacterCollisionRoom::from_collision(
                room,
                room_origin_x(record).saturating_sub(room_origin_x(current_record)),
                room_origin_z(record).saturating_sub(room_origin_z(current_record)),
            )
            .with_offset_y(record.origin_y.saturating_sub(current_record.origin_y));
            collected_rooms[count] = index;
            count += 1;
        }
        count
    }

    #[cfg(feature = "cd-stream-bench")]
    fn collect_resident_streamed_collision_rooms(
        &self,
        current_authored: Option<u32>,
        anchor: RoomPoint,
        margin: i32,
        out: &mut [CharacterCollisionRoom<'static>],
        collected_rooms: &mut [RoomIndex; MAX_COLLISION_ROOMS],
        mut count: usize,
    ) -> usize {
        let Some(current_record) = ROOMS.get(self.room_index.to_usize()) else {
            return count;
        };
        for chunk in ROOM_CHUNKS {
            if count >= out.len() {
                break;
            }
            if collision_room_collected(collected_rooms, count, chunk.room) {
                continue;
            }
            if current_authored.is_some() && Some(chunk.authored_room) != current_authored {
                continue;
            }
            if !streamed_room_is_resident(chunk.room) {
                continue;
            }
            let Some(record) = ROOMS.get(chunk.room.to_usize()) else {
                continue;
            };
            if !chunk_overlaps_collision_window(*chunk, current_record, record, anchor, margin) {
                continue;
            }
            let Some(room) = parse_streamed_compact_collision_room(0, chunk.room) else {
                continue;
            };
            out[count] = CharacterCollisionRoom::from_collision(
                RuntimeCollisionRoom::Compact(room),
                room_origin_x(record).saturating_sub(room_origin_x(current_record)),
                room_origin_z(record).saturating_sub(room_origin_z(current_record)),
            )
            .with_offset_y(record.origin_y.saturating_sub(current_record.origin_y));
            collected_rooms[count] = chunk.room;
            count += 1;
        }
        count
    }

    fn draw_collision_debug_overlay(&self, camera: WorldCamera) {
        if let Some(character) = self.character {
            draw_collision_cylinder_debug(
                self.motor.position(),
                character.radius,
                character.height,
                camera,
                (0x40, 0xd8, 0xff),
            );
        }

        for active in self.active_rooms.iter().flatten().copied() {
            let room_camera = camera_for_room(camera, active);
            for inst in MODEL_INSTANCES {
                if inst.room != active.index {
                    continue;
                }
                let Some(model) = self.models.get(inst.model.to_usize()).copied().flatten() else {
                    continue;
                };
                draw_collision_cylinder_debug(
                    RoomPoint::new(inst.x, inst.y, inst.z),
                    i32::from(model.collision_radius),
                    i32::from(model.world_height),
                    room_camera,
                    (0xff, 0xd0, 0x40),
                );
            }
        }
    }

    fn draw_particle_emitters(
        &self,
        camera: WorldCamera,
        elapsed_tick: SimTick,
        ot: &mut OtFrame<'_, OT_DEPTH>,
        primitive_packets: &mut PrimitivePacketArena<'_>,
    ) -> usize {
        let Some(particle_material) = self.particle_material else {
            return 0;
        };
        let mut submitted = 0usize;
        for active in self.active_rooms.iter().flatten().copied() {
            if !self.portal_visibility_draws_room(active.index) {
                continue;
            }
            let room_camera = camera_for_room(camera, active);
            let depth_range = ROOMS
                .get(active.index.to_usize())
                .map(room_depth_range)
                .unwrap_or(WORLD_DEPTH_RANGE);
            let mut projector = None;
            for emitter in PARTICLE_EMITTERS {
                if emitter.room != active.index {
                    continue;
                }
                let projector = match projector {
                    Some(projector) => Some(projector),
                    None => {
                        if !PROP_PARTICLE_GTE_PROJECT_ENABLED {
                            None
                        } else {
                            let loaded = LoadedWorldCameraGte::load(room_camera);
                            projector = Some(loaded);
                            Some(loaded)
                        }
                    }
                };
                submitted += draw_particle_emitter(
                    *emitter,
                    room_camera,
                    projector,
                    depth_range,
                    particle_material,
                    elapsed_tick,
                    ot,
                    primitive_packets,
                );
            }
        }
        submitted
    }

    fn camera_config(&self) -> ThirdPersonCameraConfig {
        let camera = ROOMS
            .get(self.room_index.to_usize())
            .map(|room| room.camera)
            .unwrap_or(LevelCameraRecord::DEFAULT);
        let mut config = ThirdPersonCameraConfig::character(
            camera.distance,
            camera.height,
            camera.target_height,
        );
        config.height = config.height.max(256);
        config.min_floor_clearance = camera.min_floor_clearance;
        config
    }

    fn camera_target(
        &self,
        lock_target: Option<RoomPoint>,
        moving: bool,
    ) -> ThirdPersonCameraTarget {
        ThirdPersonCameraTarget {
            player: self.motor.position(),
            player_yaw: self.motor.yaw(),
            moving,
            lock_target,
        }
    }

    fn current_room_lighting(&self, camera: WorldCamera) -> Option<RuntimeRoomLighting> {
        self.current_collision_room?;
        let room_record = ROOMS.get(self.room_index.to_usize())?;
        Some(RuntimeRoomLighting {
            room_index: self.room_index,
            ambient: Rgb8::from_array(self.current_ambient_rgb),
            camera,
            fog_enabled: room_record.flags & room_flags::FOG_ENABLED != 0,
            fog_rgb: Rgb8::from_array(room_record.fog_rgb),
            fog_near: room_record.fog_near,
            fog_far: room_record.fog_far,
        })
    }

    fn free_orbit_camera(&self) -> WorldCamera {
        WorldCamera::orbit_yaw(
            PROJECTION,
            self.spawn,
            CAMERA_Y_OFFSET,
            self.orbit_radius,
            self.orbit_yaw,
        )
    }

    fn update_camera_sweep(&mut self, delta_vblanks: u16) {
        self.orbit_radius = CAMERA_SWEEP_RADIUS.clamp(CAMERA_RADIUS_MIN, CAMERA_RADIUS_MAX);
        self.orbit_yaw = self.orbit_yaw.add_signed_q12(scale_i16_by_vblanks(
            CAMERA_SWEEP_YAW_STEP_Q12,
            delta_vblanks,
        ));
        self.player_moved_last_tick = false;
        self.camera_turning_last_tick = true;
        telemetry::stage_begin(telemetry::stage::CAMERA);
        self.render_camera = self.free_orbit_camera();
        telemetry::stage_end(telemetry::stage::CAMERA);
        if CAMERA_SWEEP_FORCE_VISIBILITY {
            self.force_refresh_active_room_window_view();
        } else {
            self.refresh_active_room_window_if_needed();
        }
        #[cfg(all(
            feature = "world-grid-visible",
            not(feature = "vis-full-active-chunks")
        ))]
        self.prewarm_visible_cell_caches();
    }

    fn update_follow_camera(&mut self, ctx: &Ctx) -> WorldCamera {
        let input = if self.lock_target.is_some() {
            ThirdPersonCameraInput {
                yaw_delta_q12: 0,
                pitch_delta_q12: 0,
                recenter: ctx.is_held(button::L1),
            }
        } else {
            camera_input(ctx)
        };
        let lock_target = self
            .lock_target_position()
            .or_else(|| self.soft_lock_target_position());
        let target = self.camera_target(lock_target, self.anim_state != PlayerAnim::Idle);
        let config = self.camera_config();
        if CAMERA_COLLISION_ENABLED && self.chunked_level() {
            let mut collision_rooms =
                [const { CharacterCollisionRoom::EMPTY }; MAX_COLLISION_ROOMS];
            let margin = config
                .distance
                .saturating_add(config.collision_margin)
                .max(config.min_distance);
            let collision_room_count =
                self.collect_collision_rooms(target.player, margin, &mut collision_rooms);
            return self
                .camera
                .update_vblanks_with_collision_rooms(
                    PROJECTION,
                    &collision_rooms[..collision_room_count],
                    target,
                    input,
                    config,
                    1u16,
                )
                .camera;
        }
        let collision = if CAMERA_COLLISION_ENABLED {
            self.current_collision_room
                .as_ref()
                .map(|room| room.collision())
        } else {
            None
        };
        self.camera
            .update_vblanks(PROJECTION, collision, target, input, config, 1u16)
            .camera
    }

    fn lock_target_position(&self) -> Option<RoomPoint> {
        self.target_position(self.lock_target?)
    }

    fn soft_lock_target_position(&self) -> Option<RoomPoint> {
        self.target_position(self.soft_lock_target?)
    }

    fn target_position(&self, index: usize) -> Option<RoomPoint> {
        let target = MODEL_INSTANCES.get(index)?;
        if target.room != self.room_index {
            return None;
        }
        Some(RoomPoint::new(target.x, target.y, target.z))
    }

    fn refresh_active_interactable(&mut self) {
        self.active_interactable = self.find_best_interactable();
    }

    fn find_best_interactable(&self) -> Option<usize> {
        let player = self.motor.position();
        let mut best = None;
        let mut best_distance = i32::MAX;
        for (index, interactable) in INTERACTABLES.iter().enumerate() {
            if !interactable_is_active(interactable) || interactable.room != self.room_index {
                continue;
            }
            let target = RoomPoint::new(interactable.x, interactable.y, interactable.z);
            let distance = distance_xz_sq(player, target);
            let radius_sq = square_i32_saturating(interactable.radius as i32);
            if distance <= radius_sq && distance < best_distance {
                best = Some(index);
                best_distance = distance;
            }
        }
        best
    }

    fn activate_interactable(&mut self, index: usize) -> bool {
        let Some(interactable) = INTERACTABLES.get(index) else {
            return false;
        };
        if !interactable_is_active(interactable) {
            return false;
        }
        match interactable.kind {
            InteractableKind::Message => {
                self.open_interactable_message(interactable);
                true
            }
            InteractableKind::Checkpoint => {
                self.checkpoint = Some(RuntimeCheckpoint {
                    room: self.room_index,
                    position: self.motor.position(),
                    yaw: self.motor.yaw(),
                    checkpoint_id: interactable.checkpoint_id,
                });
                self.open_interactable_message(interactable);
                true
            }
        }
    }

    fn open_interactable_message(&mut self, interactable: &InteractableRecord) {
        let (title, body) = interactable_message_text(interactable);
        self.message_overlay = Some(RuntimeMessageOverlay { title, body });
    }

    fn lock_target_indicator_position(&self) -> Option<RoomPoint> {
        self.target_indicator_position(self.lock_target?)
    }

    fn target_indicator_position(&self, index: usize) -> Option<RoomPoint> {
        let target = MODEL_INSTANCES.get(index)?;
        if target.room != self.room_index {
            return None;
        }
        let height = MODELS
            .get(target.model.to_usize())
            .map(|model| model.world_height as i32)
            .unwrap_or(1024);
        Some(RoomPoint::new(
            target.x,
            target.y.saturating_add(height >> 1),
            target.z,
        ))
    }

    fn lock_target_valid(&self, range: i32) -> bool {
        self.lock_target
            .is_some_and(|index| self.target_index_valid(index, range))
    }

    fn target_index_valid(&self, index: usize, range: i32) -> bool {
        let Some(target) = self.target_position(index) else {
            return false;
        };
        distance_xz_sq(self.motor.position(), target) <= square_i32_saturating(range)
    }

    fn find_best_lock_target(&self, range: i32) -> Option<usize> {
        let player = self.motor.position();
        let view_yaw = self.camera.yaw().add(Angle::HALF);
        let sin_yaw = view_yaw.sin();
        let cos_yaw = view_yaw.cos();
        let range_sq = square_i32_saturating(range);
        let mut best: Option<(usize, i32)> = None;
        for (index, target) in MODEL_INSTANCES.iter().enumerate() {
            if target.room != self.room_index {
                continue;
            }
            let point = RoomPoint::new(target.x, target.y, target.z);
            let dx = point.x.saturating_sub(player.x);
            let dz = point.z.saturating_sub(player.z);
            let dist_sq = square_i32_saturating(dx).saturating_add(square_i32_saturating(dz));
            if dist_sq == 0 || dist_sq > range_sq {
                continue;
            }
            let dot = dx
                .saturating_mul(sin_yaw.raw())
                .saturating_add(dz.saturating_mul(cos_yaw.raw()));
            if dot <= 0 {
                continue;
            }
            let score = (dot >> 4).saturating_sub(dist_sq >> 12);
            match best {
                Some((_, best_score)) if best_score >= score => {}
                _ => best = Some((index, score)),
            }
        }
        best.map(|(index, _)| index)
    }

    fn update_soft_lock(&mut self, ctx: &Ctx) {
        if self.lock_target.is_some() {
            self.soft_lock_target = None;
            self.soft_lock_suppressed = false;
            return;
        }
        let (right_x, _) = ctx.pad.sticks.right_centered();
        if abs_i16(right_x) >= CAMERA_SOFT_LOCK_BREAK_STICK {
            self.soft_lock_target = None;
            self.soft_lock_suppressed = true;
            return;
        }
        if self.soft_lock_suppressed {
            if self.find_best_lock_target(SOFT_LOCK_BREAK_RANGE).is_none() {
                self.soft_lock_suppressed = false;
            }
            return;
        }
        match self.soft_lock_target {
            Some(index) if self.target_index_valid(index, SOFT_LOCK_BREAK_RANGE) => {}
            _ => self.soft_lock_target = self.find_best_lock_target(SOFT_LOCK_RANGE),
        }
    }

    fn update_lock_target_switch(&mut self, ctx: &Ctx) {
        let (right_x, _) = ctx.pad.sticks.right_centered();
        let magnitude = abs_i16(right_x);
        if magnitude <= LOCK_SWITCH_STICK_RELEASE {
            self.lock_switch_stick_held = false;
            return;
        }
        if magnitude < LOCK_SWITCH_STICK_THRESHOLD || self.lock_switch_stick_held {
            return;
        }

        self.switch_lock_target(if right_x > 0 { -1 } else { 1 });
        self.lock_switch_stick_held = true;
    }

    fn switch_lock_target(&mut self, direction: i32) {
        let Some(current_index) = self.lock_target else {
            return;
        };
        let Some(current) = MODEL_INSTANCES.get(current_index) else {
            self.lock_target = None;
            return;
        };
        let player = self.motor.position();
        let current_dx = current.x.saturating_sub(player.x);
        let current_dz = current.z.saturating_sub(player.z);
        if current_dx == 0 && current_dz == 0 {
            return;
        }
        let range_sq = square_i32_saturating(LOCK_RANGE);
        let mut best: Option<(usize, i32)> = None;
        for (index, target) in MODEL_INSTANCES.iter().enumerate() {
            if index == current_index || target.room != self.room_index {
                continue;
            }
            let dx = target.x.saturating_sub(player.x);
            let dz = target.z.saturating_sub(player.z);
            let dist_sq = square_i32_saturating(dx).saturating_add(square_i32_saturating(dz));
            if dist_sq == 0 || dist_sq > range_sq {
                continue;
            }
            let cross = current_dx
                .saturating_mul(dz)
                .saturating_sub(current_dz.saturating_mul(dx));
            if direction > 0 {
                if cross >= 0 {
                    continue;
                }
            } else if cross <= 0 {
                continue;
            }
            let dot = current_dx
                .saturating_mul(dx)
                .saturating_add(current_dz.saturating_mul(dz));
            let score = ratio_q8_i32(dot.max(0), dist_sq.max(1)).saturating_sub(dist_sq >> 14);
            match best {
                Some((_, best_score)) if best_score >= score => {}
                _ => best = Some((index, score)),
            }
        }
        if let Some((index, _)) = best {
            self.lock_target = Some(index);
        }
    }
}

fn ratio_q8_i32(numerator: i32, denominator: i32) -> i32 {
    if numerator <= 0 || denominator <= 0 {
        return 0;
    }
    let numerator = numerator as u32;
    let denominator = denominator as u32;
    let whole = numerator / denominator;
    let remainder = numerator % denominator;
    let scaled_whole = if whole > (i32::MAX as u32 / 256) {
        return i32::MAX;
    } else {
        whole * 256
    };
    let scaled_remainder = remainder.saturating_mul(256) / denominator;
    scaled_whole
        .saturating_add(scaled_remainder)
        .min(i32::MAX as u32) as i32
}

/// Walk `room.material_first..material_first + material_count`,
/// resolve each material's texture asset, and build a
/// TextureMaterial in `out` indexed by `local_slot`. Each
/// texture asset is uploaded at most once across the program
/// lifetime -- the residency manager + VRAM_SLOTS tracks who's
/// already up.
///
/// Returns the highest `local_slot + 1` so the caller knows the
/// in-use prefix length.
fn build_room_materials(
    room: &LevelRoomRecord,
    out: &mut [Option<WorldRenderMaterial>; MAX_ROOM_MATERIALS],
) -> usize {
    let first = room.material_first.to_usize();
    let count = room.material_count as usize;
    let slice: &[LevelMaterialRecord] = &MATERIALS[first..first + count];

    let mut max_slot: usize = 0;
    for material in slice {
        let slot = material.local_slot.to_usize();
        if slot >= MAX_ROOM_MATERIALS {
            continue;
        }
        if slot + 1 > max_slot {
            max_slot = slot + 1;
        }
        let Some(asset) = find_asset_of_kind(ASSETS, material.texture_asset, AssetKind::Texture)
        else {
            continue;
        };
        let Some(slot_record) = ensure_texture_uploaded(asset.id, asset.bytes) else {
            continue;
        };
        let texture = TextureMaterial::opaque(
            slot_record.clut_word,
            slot_record.tpage_word,
            rgb_tuple(material.tint_rgb),
        )
        .with_texture_window(slot_record.texture_window);
        let render_material = match material.sidedness() {
            LevelMaterialSidedness::Front => WorldRenderMaterial::front(texture),
            LevelMaterialSidedness::Back => WorldRenderMaterial::back(texture),
            LevelMaterialSidedness::Both => WorldRenderMaterial::both(texture),
        }
        .with_texture_size(
            vram_slot_texture_size_u8(slot_record.texture_width),
            vram_slot_texture_size_u8(slot_record.texture_height),
        );
        out[slot] = Some(render_material);
    }
    max_slot
}

#[derive(Copy, Clone)]
struct RuntimeRoomLighting {
    room_index: RoomIndex,
    ambient: Rgb8,
    camera: WorldCamera,
    fog_enabled: bool,
    fog_rgb: Rgb8,
    fog_near: i32,
    fog_far: i32,
}

impl RuntimeRoomLighting {
    fn shade_model_material(
        &self,
        point: WorldVertex,
        material: TextureMaterial,
    ) -> TextureMaterial {
        material.with_tint(self.shade_tint_at(point, material.tint()))
    }

    fn shade_tint_at(&self, point: RoomPoint, base: (u8, u8, u8)) -> (u8, u8, u8) {
        let tint = psx_engine::shade_material_tint_with_lights(
            MaterialTint::from_tuple(base),
            point.to_array(),
            self.ambient,
            self.point_lights(),
        )
        .to_tuple();
        if !self.fog_enabled || self.fog_far <= self.fog_near {
            return tint;
        }
        let depth = self.camera.view_vertex(point).z;
        self.apply_fog_at_depth(tint, depth)
    }

    fn shade_tint_at_depth(
        &self,
        point: RoomPoint,
        base: (u8, u8, u8),
        fog_weight: i32,
    ) -> (u8, u8, u8) {
        let tint = psx_engine::shade_material_tint_with_lights(
            MaterialTint::from_tuple(base),
            point.to_array(),
            self.ambient,
            self.point_lights(),
        )
        .to_tuple();
        self.apply_fog_weight(tint, fog_weight)
    }

    fn apply_fog_at_depth(&self, tint: (u8, u8, u8), depth: i32) -> (u8, u8, u8) {
        self.apply_fog_weight(tint, self.fog_weight_at_depth(depth))
    }

    fn apply_fog_weight(&self, tint: (u8, u8, u8), weight: i32) -> (u8, u8, u8) {
        apply_room_fog_weight(tint, self.fog_rgb, weight)
    }

    fn fog_weight_at_depth(&self, depth: i32) -> i32 {
        room_fog_weight(depth, self.fog_enabled, self.fog_near, self.fog_far)
    }

    fn point_lights(&self) -> impl Iterator<Item = PointLightSample> + '_ {
        LIGHTS
            .iter()
            .filter(move |light| light.room == self.room_index)
            .map(|light| {
                PointLightSample::from_rgb_intensity(
                    [light.x, light.y, light.z],
                    light.radius as i32,
                    Rgb8::from_array(light.color),
                    Q8::from_raw_u16(light.intensity_q8),
                )
            })
    }

    fn apply_vertex_fog(&self, rgb: (u8, u8, u8), vertex: WorldVertex) -> (u8, u8, u8) {
        if !self.fog_enabled || self.fog_far <= self.fog_near {
            return rgb;
        }
        let depth = self.camera.view_vertex(vertex).z;
        self.apply_fog_at_depth(rgb, depth)
    }

    fn apply_vertex_fog_weight(&self, rgb: (u8, u8, u8), weight: i32) -> (u8, u8, u8) {
        self.apply_fog_weight(rgb, weight)
    }
}

impl WorldSurfaceLighting for RuntimeRoomLighting {
    fn shade(
        &self,
        sample: WorldSurfaceSample,
        material: WorldRenderMaterial,
    ) -> WorldRenderMaterial {
        material.with_tint(self.shade_tint_at(sample.center, material.texture.tint()))
    }

    fn shade_vertex(
        &self,
        _sample: WorldSurfaceSample,
        vertex: RoomPoint,
        material: WorldRenderMaterial,
    ) -> (u8, u8, u8) {
        self.shade_tint_at(vertex, material.texture.tint())
    }

    fn shade_vertices(
        &self,
        sample: WorldSurfaceSample,
        vertices: [WorldVertex; 4],
        material: WorldRenderMaterial,
    ) -> [(u8, u8, u8); 4] {
        if let Some(vertex_rgb) = sample.baked_vertex_rgb {
            if !self.fog_enabled || self.fog_far <= self.fog_near {
                return vertex_rgb;
            }
            return [
                self.apply_vertex_fog(vertex_rgb[0], vertices[0]),
                self.apply_vertex_fog(vertex_rgb[1], vertices[1]),
                self.apply_vertex_fog(vertex_rgb[2], vertices[2]),
                self.apply_vertex_fog(vertex_rgb[3], vertices[3]),
            ];
        }
        [
            self.shade_vertex(sample, vertices[0], material),
            self.shade_vertex(sample, vertices[1], material),
            self.shade_vertex(sample, vertices[2], material),
            self.shade_vertex(sample, vertices[3], material),
        ]
    }

    fn shade_vertices_with_depths(
        &self,
        sample: WorldSurfaceSample,
        vertices: [WorldVertex; 4],
        depths: [i32; 4],
        material: WorldRenderMaterial,
    ) -> [(u8, u8, u8); 4] {
        if let Some(vertex_rgb) = sample.baked_vertex_rgb {
            if !self.fog_enabled || self.fog_far <= self.fog_near {
                return vertex_rgb;
            }
            return [
                self.apply_vertex_fog_weight(vertex_rgb[0], depths[0]),
                self.apply_vertex_fog_weight(vertex_rgb[1], depths[1]),
                self.apply_vertex_fog_weight(vertex_rgb[2], depths[2]),
                self.apply_vertex_fog_weight(vertex_rgb[3], depths[3]),
            ];
        }
        [
            self.shade_tint_at_depth(vertices[0], material.texture.tint(), depths[0]),
            self.shade_tint_at_depth(vertices[1], material.texture.tint(), depths[1]),
            self.shade_tint_at_depth(vertices[2], material.texture.tint(), depths[2]),
            self.shade_tint_at_depth(vertices[3], material.texture.tint(), depths[3]),
        ]
    }

    fn shade_cached_baked_vertices(
        &self,
        sample: WorldSurfaceSample,
        depths: Option<[i32; 4]>,
        _material: WorldRenderMaterial,
    ) -> Option<[(u8, u8, u8); 4]> {
        let vertex_rgb = sample.baked_vertex_rgb?;
        if !self.fog_enabled || self.fog_far <= self.fog_near {
            return Some(vertex_rgb);
        }
        let depths = depths?;
        Some([
            self.apply_vertex_fog_weight(vertex_rgb[0], depths[0]),
            self.apply_vertex_fog_weight(vertex_rgb[1], depths[1]),
            self.apply_vertex_fog_weight(vertex_rgb[2], depths[2]),
            self.apply_vertex_fog_weight(vertex_rgb[3], depths[3]),
        ])
    }

    fn uses_vertex_depths(&self) -> bool {
        self.fog_enabled && self.fog_far > self.fog_near
    }

    fn uses_direct_baked_vertex_rgb(&self) -> bool {
        !self.fog_enabled || self.fog_far <= self.fog_near
    }

    fn prepare_vertex_depth(&self, depth: i32) -> i32 {
        self.fog_weight_at_depth(depth)
    }

    fn needs_surface_sample_center(&self, sample_has_baked_rgb: bool) -> bool {
        !sample_has_baked_rgb
    }
}

#[inline(always)]
fn room_fog_weight(depth: i32, enabled: bool, fog_near: i32, fog_far: i32) -> i32 {
    if !enabled || fog_far <= fog_near || depth <= fog_near {
        return 0;
    }
    (((depth - fog_near).saturating_mul(256)) / (fog_far - fog_near)).clamp(0, 256)
}

#[inline(always)]
fn apply_room_fog_weight(tint: (u8, u8, u8), fog_rgb: Rgb8, weight: i32) -> (u8, u8, u8) {
    if weight <= 0 {
        return tint;
    }
    if weight >= 256 {
        return (fog_rgb.r, fog_rgb.g, fog_rgb.b);
    }
    let keep = 256 - weight;
    (
        blend_channel(tint.0, fog_rgb.r, keep, weight),
        blend_channel(tint.1, fog_rgb.g, keep, weight),
        blend_channel(tint.2, fog_rgb.b, keep, weight),
    )
}

#[inline(always)]
fn blend_channel(src: u8, fog: u8, keep: i32, weight: i32) -> u8 {
    (((src as i32) * keep + (fog as i32) * weight) >> 8) as u8
}

const fn rgb_tuple(rgb: [u8; 3]) -> (u8, u8, u8) {
    (rgb[0], rgb[1], rgb[2])
}

fn mul_q12_i32(value: i32, q12: i32) -> i32 {
    let whole = value >> Q12::FRACTIONAL_BITS;
    let fraction = value & (Q12::SCALE - 1);
    whole
        .saturating_mul(q12)
        .saturating_add(fraction.saturating_mul(q12) >> Q12::FRACTIONAL_BITS)
}

fn playtest_visual_pacing(video_mode: VideoMode) -> VisualPacing {
    match video_mode {
        VideoMode::Ntsc => VisualPacing::EveryNVBlanks(2),
        // PAL is 50Hz, so exact 30Hz pacing does not divide cleanly.
        // Use a deterministic 25Hz fallback instead of a jittery cadence.
        VideoMode::Pal => VisualPacing::EveryNVBlanks(2),
    }
}

#[no_mangle]
fn main() -> ! {
    let mut scene = Playtest::new();
    let video_mode = VideoMode::Ntsc;
    let config = Config {
        clear_color: (5, 7, 12),
        video_mode,
        visual_pacing: playtest_visual_pacing(video_mode),
        scheduler: SchedulerConfig::new()
            .with_max_fixed_ticks_before_visual(RUNTIME_SCHEDULE.max_fixed_ticks_before_visual),
        ..Config::default()
    };
    // Drive the cooked game flow (front-end UI scenes + gameplay) rather than
    // booting straight into gameplay, so an authored menu shows first and
    // `GAME_FLOW.entry` (set from the project's boot target) decides where
    // play begins.
    App::run_with_flow(
        config,
        &GAME_FLOW,
        UI_SCENES,
        UI_NODES,
        UI_PAINTS,
        OPTIONS,
        UI_SFX_SAMPLES,
        UI_SFX_CUES,
        &mut scene,
    );
}
